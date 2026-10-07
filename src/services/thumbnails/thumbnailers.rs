//! The system's thumbnailers: programs described by `.thumbnailer` files
//! in `$XDG_DATA_DIRS/thumbnailers` (videos via ffmpegthumbnailer, images
//! via glycin, documents via Papers/Evince, fonts…).
//!
//! ```ini
//! [Thumbnailer Entry]
//! TryExec=ffmpegthumbnailer
//! Exec=ffmpegthumbnailer -i %i -o %o -s %s -f
//! MimeType=video/mp4;video/webm;
//! ```

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use gtk::{gio, glib};

const GROUP: &str = "Thumbnailer Entry";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnailer {
    pub exec: String,
    pub mime_types: Vec<String>,
}

impl Thumbnailer {
    /// Parses a `.thumbnailer` file. Entries whose `TryExec` program isn't
    /// installed are skipped when `check_installed` is set.
    pub fn parse(data: &str, check_installed: bool) -> Option<Self> {
        let keyfile = glib::KeyFile::new();
        keyfile
            .load_from_data(data, glib::KeyFileFlags::NONE)
            .ok()?;
        let exec = keyfile.string(GROUP, "Exec").ok()?.to_string();
        if check_installed {
            let program = match keyfile.string(GROUP, "TryExec") {
                Ok(try_exec) => try_exec.to_string(),
                Err(_) => glib::shell_parse_argv(&exec)
                    .ok()?
                    .first()?
                    .to_string_lossy()
                    .into_owned(),
            };
            glib::find_program_in_path(&program)?;
        }
        let mime_types = keyfile
            .string_list(GROUP, "MimeType")
            .ok()?
            .iter()
            .map(|mime| mime.to_string())
            .filter(|mime| !mime.is_empty())
            .collect::<Vec<_>>();
        (!exec.is_empty() && !mime_types.is_empty()).then_some(Self { exec, mime_types })
    }

    /// The command line for one file: `%i` input path, `%u` input URI,
    /// `%o` output path, `%s` size, `%%` a percent sign.
    pub fn command(
        &self,
        input: &Path,
        uri: &str,
        output: &Path,
        size: i32,
    ) -> Option<Vec<OsString>> {
        let args = glib::shell_parse_argv(&self.exec).ok()?;
        let size = size.to_string();
        let expanded = args
            .iter()
            .map(|arg| {
                let arg = arg.to_str()?;
                let mut out = OsString::new();
                let mut chars = arg.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        out.push(c.to_string());
                        continue;
                    }
                    match chars.next() {
                        Some('i') => out.push(input),
                        Some('u') => out.push(uri),
                        Some('o') => out.push(output),
                        Some('s') => out.push(&size),
                        Some('%') => out.push("%"),
                        // Unknown field codes are dropped.
                        _ => {}
                    }
                }
                Some(out)
            })
            .collect::<Option<Vec<_>>>()?;
        (!expanded.is_empty()).then_some(expanded)
    }
}

/// All installed thumbnailers, by MIME type.
#[derive(Debug, Default)]
pub struct Registry {
    thumbnailers: Vec<Thumbnailer>,
    by_mime: HashMap<String, usize>,
}

impl Registry {
    /// Thumbnailers from the user's data directory, then the system's.
    pub fn system() -> Self {
        let dirs: Vec<PathBuf> = std::iter::once(glib::user_data_dir())
            .chain(glib::system_data_dirs())
            .map(|dir| dir.join("thumbnailers"))
            .collect();
        Self::from_dirs(&dirs, true)
    }

    /// Reads the `.thumbnailer` files in `dirs`. For a MIME type handled by
    /// several, the first directory wins (so users can override).
    pub fn from_dirs(dirs: &[PathBuf], check_installed: bool) -> Self {
        let mut registry = Self::default();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|ext| ext == "thumbnailer"))
                .collect();
            paths.sort();
            for path in paths {
                let Some(thumbnailer) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|data| Thumbnailer::parse(&data, check_installed))
                else {
                    continue;
                };
                registry.add(thumbnailer);
            }
        }
        registry
    }

    pub fn add(&mut self, thumbnailer: Thumbnailer) {
        let index = self.thumbnailers.len();
        for mime in &thumbnailer.mime_types {
            self.by_mime.entry(mime.clone()).or_insert(index);
        }
        self.thumbnailers.push(thumbnailer);
    }

    /// The thumbnailer for `mime_type`, or for a type it's a subclass of
    /// (e.g. a thumbnailer for `image/x-dds` also handles its aliases).
    pub fn find(&self, mime_type: &str) -> Option<&Thumbnailer> {
        if let Some(&index) = self.by_mime.get(mime_type) {
            return Some(&self.thumbnailers[index]);
        }
        self.by_mime
            .iter()
            .filter(|(handled, _)| gio::content_type_is_a(mime_type, handled))
            .map(|(_, &index)| index)
            .min()
            .map(|index| &self.thumbnailers[index])
    }

    pub fn is_empty(&self) -> bool {
        self.thumbnailers.is_empty()
    }
}
