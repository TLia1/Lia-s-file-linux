//! Local programs are executed, rather than opened through a MIME handler.

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;

use gtk::{gio, glib, prelude::*};

use crate::services::terminal;
use crate::util::sandbox;

pub const ATTRIBUTES: &str = "standard::type,standard::content-type,access::can-execute";

#[derive(Debug)]
pub struct Executable {
    path: PathBuf,
    is_shell_script: bool,
    pub is_script: bool,
    pub can_execute: bool,
}

impl Executable {
    /// Only local regular programs and scripts qualify. An executable bit on
    /// a document, or a remote backend's permissions, must not run a file.
    pub fn from_info(file: &gio::File, info: &gio::FileInfo) -> Option<Self> {
        let path = file.path().filter(|_| file.is_native())?;
        if info.file_type() != gio::FileType::Regular {
            return None;
        }
        let content_type = info.content_type()?;
        let is_script = gio::content_type_is_a(&content_type, "text/plain")
            && gio::content_type_is_a(&content_type, "application/x-executable");
        let is_program = gio::content_type_is_a(&content_type, "application/x-executable")
            || gio::content_type_is_a(&content_type, "application/x-pie-executable");
        if !is_program {
            return None;
        }
        Some(Self {
            path,
            is_shell_script: gio::content_type_is_a(&content_type, "text/x-shellscript"),
            is_script,
            can_execute: info.boolean(gio::FILE_ATTRIBUTE_ACCESS_CAN_EXECUTE),
        })
    }

    /// Pass the path as one argument, never as shell source. A shell script
    /// without a shebang uses /bin/sh; other scripts keep their interpreter.
    pub fn command(&self) -> Result<Vec<OsString>, glib::Error> {
        let info = gio::File::for_path(&self.path).query_info(
            gio::FILE_ATTRIBUTE_ACCESS_CAN_EXECUTE,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )?;
        if !info.boolean(gio::FILE_ATTRIBUTE_ACCESS_CAN_EXECUTE) {
            return Err(glib::Error::new(
                gio::IOErrorEnum::PermissionDenied,
                "The file is not executable",
            ));
        }
        let mut argv = Vec::new();
        if self.is_shell_script {
            let mut header = [0; 2];
            let mut file = std::fs::File::open(&self.path)
                .map_err(|error| glib::Error::new(gio::IOErrorEnum::Failed, &error.to_string()))?;
            let length = file
                .read(&mut header)
                .map_err(|error| glib::Error::new(gio::IOErrorEnum::Failed, &error.to_string()))?;
            if length != 2 || header != *b"#!" {
                argv.push("/bin/sh".into());
            }
        }
        argv.push(self.path.as_os_str().to_owned());
        Ok(argv)
    }

    pub fn launch(&self, in_terminal: bool) -> Result<gio::Subprocess, glib::Error> {
        let dir = self.path.parent().expect("a local file has a parent");
        let command = self.command()?;
        let argv = if sandbox::is_flatpak() {
            let mut directory = OsString::from("--directory=");
            directory.push(dir);
            let mut argv = vec!["flatpak-spawn".into(), "--host".into(), directory];
            if in_terminal {
                argv.extend(["xdg-terminal-exec".into(), "--".into()]);
            }
            argv.extend(command);
            argv
        } else if in_terminal {
            let terminal = terminal::find().ok_or_else(|| {
                glib::Error::new(
                    gio::IOErrorEnum::NotFound,
                    &gettextrs::gettext("No terminal application was found"),
                )
            })?;
            terminal::command_with_program(&terminal, dir, &command)
        } else {
            command
        };
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
        launcher.set_cwd(dir);
        let argv: Vec<_> = argv.iter().map(OsString::as_os_str).collect();
        launcher.spawn(&argv)
    }
}
