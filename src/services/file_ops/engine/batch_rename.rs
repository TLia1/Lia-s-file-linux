//! Renaming several files at once, as a step of the [`Runner`].
//!
//! New names may be taken by other files of the batch (swapping "a" and
//! "b", shifting numbers): files whose new name is free are renamed first,
//! and when only blocked ones are left, one blocking file is moved aside
//! under a temporary name. Nothing outside the batch is ever overwritten.

use gettextrs::gettext;
use gtk::{gio, prelude::*};

use super::{Abort, NOFOLLOW, Runner, display_name, unique_sibling};
use crate::services::file_ops::naming::UniqueStyle;

struct Pending {
    original: gio::File,
    original_name: String,
    /// Where the file is now (moved aside, maybe).
    current: gio::File,
    name: String,
}

impl Pending {
    fn target(&self) -> Option<gio::File> {
        Some(self.current.parent()?.child(&self.name))
    }
}

impl Runner<'_> {
    pub(super) fn run_batch_rename(
        &mut self,
        renames: &[(gio::File, String)],
    ) -> Result<(), Abort> {
        let mut pending: Vec<Pending> = renames
            .iter()
            .filter(|(file, name)| {
                file.basename()
                    .is_none_or(|current| current.to_string_lossy() != name.as_str())
            })
            .map(|(file, name)| Pending {
                original: file.clone(),
                original_name: display_name(file),
                current: file.clone(),
                name: name.clone(),
            })
            .collect();
        self.control
            .update(|p| p.files_total = pending.len() as u64);

        let result = self.rename_all(&mut pending);
        // Whatever couldn't be finished goes back to its name.
        for item in &pending {
            restore(item);
        }
        result
    }

    fn rename_all(&mut self, pending: &mut Vec<Pending>) -> Result<(), Abort> {
        while !pending.is_empty() {
            self.check()?;
            // Rename everything whose new name is free.
            let mut progressed = false;
            let mut i = 0;
            while i < pending.len() {
                let Some(target) = pending[i].target() else {
                    pending.remove(i);
                    continue;
                };
                if self.exists(&target) {
                    i += 1;
                    continue;
                }
                self.check()?;
                let item = pending.remove(i);
                self.rename_one(item)?;
                progressed = true;
            }
            if progressed || pending.is_empty() {
                continue;
            }

            // Every new name is taken. Move aside a file of the batch that
            // blocks another one (or that only differs by case, on file
            // systems that ignore it).
            let blocker = pending.iter().find_map(|item| {
                let target = item.target()?;
                if same_file(&target, &item.current) {
                    return pending.iter().position(|p| p.current.equal(&item.current));
                }
                pending
                    .iter()
                    .position(|other| other.current.equal(&target))
            });
            if let Some(index) = blocker {
                if !self.move_aside(&mut pending[index])? {
                    // Skipped: the files it blocks will fail below.
                    pending.remove(index);
                    self.control.update(|p| p.files_done += 1);
                }
                continue;
            }

            // The rest collide with files outside the batch; they stay in
            // `pending` to get their names back.
            let blocked: Vec<(gio::File, String)> = pending
                .iter()
                .map(|item| (item.original.clone(), item.name.clone()))
                .collect();
            for (original, name) in blocked {
                let message = gettext("An item named “{}” already exists").replace("{}", &name);
                self.fail(&original, &message)?;
                self.control.update(|p| p.files_done += 1);
            }
            break;
        }
        Ok(())
    }

    fn rename_one(&mut self, item: Pending) -> Result<(), Abort> {
        self.set_current(&item.original_name);
        let cancellable = self.control.cancellable().clone();
        let renamed = self.attempt(&item.current, || {
            item.current
                .set_display_name(&item.name, Some(&cancellable))
        })?;
        match renamed {
            Some(renamed) => {
                self.created.push(renamed.clone());
                self.renamed
                    .push((item.original, item.original_name, renamed, item.name));
                self.succeeded += 1;
            }
            // Skipped (maybe while moved aside).
            None => restore(&item),
        }
        self.control.update(|p| p.files_done += 1);
        Ok(())
    }

    /// Renames `item` to a free temporary name. Returns false if skipped.
    fn move_aside(&mut self, item: &mut Pending) -> Result<bool, Abort> {
        let name = item
            .current
            .basename()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(parent) = item.current.parent() else {
            return Ok(false);
        };
        let temp = unique_sibling(
            &parent.child(format!(".{name}.renaming")),
            false,
            UniqueStyle::Numbered,
        );
        let temp_name = temp
            .basename()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cancellable = self.control.cancellable().clone();
        let current = item.current.clone();
        match self.attempt(&current, || {
            current.set_display_name(&temp_name, Some(&cancellable))
        })? {
            Some(moved) => {
                item.current = moved;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// Gives a file moved aside its name back; a numbered variant if another
/// file of the batch took it meanwhile, so it never stays hidden.
fn restore(item: &Pending) {
    if item.current.equal(&item.original) {
        return;
    }
    let Some(parent) = item.current.parent() else {
        return;
    };
    let free = unique_sibling(
        &parent.child(&item.original_name),
        false,
        UniqueStyle::Numbered,
    );
    let name = free
        .basename()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| item.original_name.clone());
    if let Err(err) = item.current.set_display_name(&name, gio::Cancellable::NONE) {
        tracing::warn!("cannot restore {}: {err}", item.original.uri());
    }
}

/// Whether two locations are the same file (e.g. "a.txt" and "A.TXT" on
/// a file system that ignores case).
fn same_file(a: &gio::File, b: &gio::File) -> bool {
    let id = |file: &gio::File| {
        file.query_info(
            gio::FILE_ATTRIBUTE_ID_FILE,
            NOFOLLOW,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|info| info.attribute_string(gio::FILE_ATTRIBUTE_ID_FILE))
    };
    match (id(a), id(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
