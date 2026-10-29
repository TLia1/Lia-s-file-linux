//! User-installed desktop actions. GIO expands Exec field codes and passes
//! file arguments directly; the file manager never builds a shell command.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gtk::{gio, glib, prelude::*};

#[derive(Debug, Clone)]
pub struct CustomAction {
    pub name: String,
    pub icon: Option<String>,
    pub exec: String,
    pub mime_types: Vec<String>,
    pub terminal: bool,
    uris: bool,
    multiple: bool,
}

impl CustomAction {
    pub fn matches(&self, files: &[(gio::File, String)]) -> bool {
        !files.is_empty()
            && files.iter().all(|(file, content_type)| {
                (self.uris || file.path().is_some())
                    && self
                        .mime_types
                        .iter()
                        .any(|mime| gio::content_type_is_a(content_type, mime))
            })
            && (files.len() == 1 || self.multiple)
    }

    pub fn launch(
        &self,
        files: &[gio::File],
        context: Option<&gio::AppLaunchContext>,
    ) -> Result<(), glib::Error> {
        let mut flags = gio::AppInfoCreateFlags::NONE;
        if self.uris {
            flags |= gio::AppInfoCreateFlags::SUPPORTS_URIS;
        }
        let command = if crate::util::sandbox::is_flatpak() {
            format!(
                "flatpak-spawn --host {}{}",
                if self.terminal {
                    "xdg-terminal-exec "
                } else {
                    ""
                },
                self.exec
            )
        } else {
            if self.terminal {
                flags |= gio::AppInfoCreateFlags::NEEDS_TERMINAL;
            }
            self.exec.clone()
        };
        gio::AppInfo::create_from_commandline(&command, Some(&self.name), flags)?
            .launch(files, context)
    }
}

/// XDG priority applies by filename; a user's Hidden=true masks the system copy.
pub fn discover_in(directories: &[PathBuf]) -> Vec<CustomAction> {
    let mut seen = HashSet::new();
    let mut actions = Vec::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|ext| ext != "desktop") || !path.is_file() {
                continue;
            }
            if !seen.insert(path.file_name().unwrap().to_owned()) {
                continue;
            }
            if let Some(action) = read_action(&path) {
                actions.push(action);
            }
        }
    }
    actions.sort_by(|a, b| a.name.cmp(&b.name));
    actions
}

pub fn discover() -> Vec<CustomAction> {
    let mut dirs = vec![glib::user_data_dir().join("lia-files/actions")];
    dirs.extend(
        glib::system_data_dirs()
            .iter()
            .map(|dir| dir.join("lia-files/actions")),
    );
    discover_in(&dirs)
}

fn read_action(path: &Path) -> Option<CustomAction> {
    let key = glib::KeyFile::new();
    key.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    let group = "Desktop Entry";
    if key.string(group, "Type").ok()?.as_str() != "Application"
        || key.boolean(group, "Hidden").unwrap_or(false)
        || key.boolean(group, "NoDisplay").unwrap_or(false)
    {
        return None;
    }
    if let Ok(command) = key.string(group, "TryExec")
        && !crate::util::sandbox::is_flatpak()
        && glib::find_program_in_path(command.as_str()).is_none()
    {
        return None;
    }
    let exec = key.string(group, "Exec").ok()?.to_string();
    // Require explicit file arguments. Reject unsupported field codes rather
    // than letting a malformed desktop file launch against the wrong target.
    let mut chars = exec.chars();
    let mut file_code = None;
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next()? {
                code @ ('f' | 'F' | 'u' | 'U') => {
                    if file_code.replace(code).is_some() {
                        return None;
                    }
                }
                '%' => (),
                _ => return None,
            }
        }
    }
    let file_code = file_code?;
    let mime_types = key
        .string(group, "MimeType")
        .ok()?
        .split(';')
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .collect();
    Some(CustomAction {
        name: key.locale_string(group, "Name", None).ok()?.to_string(),
        icon: key.string(group, "Icon").ok().map(String::from),
        exec,
        mime_types,
        terminal: key.boolean(group, "Terminal").unwrap_or(false),
        uris: matches!(file_code, 'u' | 'U'),
        multiple: matches!(file_code, 'F' | 'U'),
    })
}
