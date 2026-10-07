//! Drag and drop rules shared by the file view, the sidebar and the tabs.
//!
//! Like Explorer: Shift moves, Ctrl copies, Ctrl+Shift links; without a
//! modifier, files move within a device and are copied across devices.
//! Hovering a folder for a moment while dragging opens it.

use std::os::unix::fs::MetadataExt;
use std::time::Duration;

use gtk::{gdk, gio, prelude::*};

use crate::services::file_ops::Operation;
use crate::services::file_ops::engine::TRASH_URI;

/// Hover time before a folder opens under a drag ("spring-loaded").
pub const SPRING_DELAY: Duration = Duration::from_millis(800);

/// The actions file drops support.
pub const ACTIONS: gdk::DragAction = gdk::DragAction::COPY
    .union(gdk::DragAction::MOVE)
    .union(gdk::DragAction::LINK);

pub fn is_trash(file: &gio::File) -> bool {
    file.uri_scheme().as_deref() == Some("trash")
}

/// Whether `a` and `b` are on the same device, which makes "move" the
/// default. Only cheap checks: a `stat` for local files, the URI authority
/// for others.
pub fn same_device(a: &gio::File, b: &gio::File) -> bool {
    if is_trash(a) {
        // Dragging out of the trash restores.
        return true;
    }
    match (a.path(), b.path()) {
        (Some(a), Some(b)) if a.is_absolute() && b.is_absolute() => {
            match (std::fs::symlink_metadata(&a), std::fs::metadata(&b)) {
                (Ok(a), Ok(b)) => a.dev() == b.dev(),
                _ => false,
            }
        }
        _ => authority(a) == authority(b),
    }
}

/// "smb://host" for "smb://host/share/file".
fn authority(file: &gio::File) -> String {
    let uri = file.uri();
    let rest = uri.split_once("://").map_or("", |(_, rest)| rest);
    let host = rest.split('/').next().unwrap_or_default();
    format!("{}://{host}", file.uri_scheme().unwrap_or_default())
}

/// The action for dropping `files` on the folder `dest` with `modifiers`
/// held. `same_device` is evaluated only when needed. Empty means the drop
/// isn't allowed (a folder onto itself or into its own subfolder).
pub fn action_for(
    files: &[gio::File],
    dest: &gio::File,
    modifiers: gdk::ModifierType,
    same_device: impl FnOnce() -> bool,
) -> gdk::DragAction {
    if files
        .iter()
        .any(|file| file.equal(dest) || dest.has_prefix(file))
    {
        return gdk::DragAction::empty();
    }
    if is_trash(dest) {
        return gdk::DragAction::MOVE;
    }
    let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    match (ctrl, shift) {
        (true, true) => gdk::DragAction::LINK,
        (true, false) => gdk::DragAction::COPY,
        (false, true) => gdk::DragAction::MOVE,
        (false, false) if same_device() => gdk::DragAction::MOVE,
        (false, false) => gdk::DragAction::COPY,
    }
}

/// The keyboard modifiers currently held (for drop targets that don't get
/// key state from their events).
pub fn current_modifiers(widget: &impl IsA<gtk::Widget>) -> gdk::ModifierType {
    widget
        .display()
        .default_seat()
        .and_then(|seat| seat.keyboard())
        .map(|keyboard| keyboard.modifier_state())
        .unwrap_or_else(gdk::ModifierType::empty)
}

/// The operation a drop performs, or `None` if there's nothing to do
/// (e.g. moving files into the folder they're already in).
pub fn operation_for(
    files: &[gio::File],
    dest: &gio::File,
    action: gdk::DragAction,
) -> Option<Operation> {
    if is_trash(dest) {
        let files: Vec<gio::File> = files.iter().filter(|f| !is_trash(f)).cloned().collect();
        return (!files.is_empty()).then_some(Operation::Trash { files });
    }
    let files: Vec<gio::File> = if action == gdk::DragAction::MOVE {
        files
            .iter()
            .filter(|f| !f.parent().is_some_and(|p| p.equal(dest)))
            .cloned()
            .collect()
    } else {
        files.to_vec()
    };
    if files.is_empty() {
        return None;
    }
    Some(if action == gdk::DragAction::MOVE {
        Operation::move_into(&files, dest)
    } else if action == gdk::DragAction::LINK {
        Operation::link_into(&files, dest)
    } else {
        Operation::copy_into(&files, dest)
    })
}

/// The trash location, as a file.
pub fn trash() -> gio::File {
    gio::File::for_uri(TRASH_URI)
}

/// Highlights the row (or grid cell) containing `widget` as a drop target.
pub fn highlight_row(widget: &gtk::Widget, on: bool) {
    let mut current = widget.parent();
    for _ in 0..3 {
        let Some(ancestor) = current else {
            break;
        };
        if matches!(ancestor.css_name().as_str(), "row" | "child") {
            if on {
                ancestor.add_css_class("drop-target");
            } else {
                ancestor.remove_css_class("drop-target");
            }
            return;
        }
        current = ancestor.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str) -> gio::File {
        gio::File::for_path(path)
    }

    #[test]
    fn chooses_actions_like_explorer() {
        let files = [f("/home/a/x.txt")];
        let dest = f("/home/a/docs");
        let none = gdk::ModifierType::empty();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(
            action_for(&files, &dest, none, || true),
            gdk::DragAction::MOVE
        );
        assert_eq!(
            action_for(&files, &dest, none, || false),
            gdk::DragAction::COPY
        );
        assert_eq!(
            action_for(&files, &dest, ctrl, || true),
            gdk::DragAction::COPY
        );
        assert_eq!(
            action_for(&files, &dest, shift, || false),
            gdk::DragAction::MOVE
        );
        assert_eq!(
            action_for(&files, &dest, ctrl | shift, || true),
            gdk::DragAction::LINK
        );
        // Into itself or its subfolder: refused.
        let folder = [f("/home/a/docs")];
        assert!(action_for(&folder, &dest, none, || true).is_empty());
        assert!(action_for(&folder, &f("/home/a/docs/sub"), none, || true).is_empty());
    }

    #[test]
    fn builds_operations() {
        let files = [f("/home/a/x.txt")];
        // Moving into the same folder does nothing.
        assert!(operation_for(&files, &f("/home/a"), gdk::DragAction::MOVE).is_none());
        assert!(matches!(
            operation_for(&files, &f("/home/a"), gdk::DragAction::COPY),
            Some(Operation::Copy { .. })
        ));
        assert!(matches!(
            operation_for(&files, &trash(), gdk::DragAction::COPY),
            Some(Operation::Trash { .. })
        ));
        assert_eq!(
            authority(&gio::File::for_uri("smb://host/share/a")),
            "smb://host"
        );
    }
}
