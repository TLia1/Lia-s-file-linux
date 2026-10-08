//! "Open in Terminal": starts the user's terminal in a folder.
//!
//! The default terminal is found through `xdg-terminal-exec` (the
//! freedesktop proposal) when it's installed. Otherwise the first known
//! terminal in `PATH` is used, preferring the desktop's own (Konsole on
//! KDE, Console on GNOME…). Each terminal is told the folder with its own
//! option, and also starts in it, for those that simply inherit it.

use std::ffi::OsString;
use std::path::Path;

use gtk::{gio, glib};

use crate::util::sandbox;

/// How a terminal is told which folder to start in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirArg {
    /// `--option=DIR`
    Joined(&'static str),
    /// `--option DIR`
    Separate(&'static str),
    /// `start --cwd DIR` (WezTerm)
    Subcommand(&'static str, &'static str),
    /// Only the working directory of the process.
    Inherit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
    pub program: &'static str,
    arg: DirArg,
    /// The `XDG_CURRENT_DESKTOP` names it belongs to; preferred there.
    desktops: &'static [&'static str],
}

const fn terminal(
    program: &'static str,
    arg: DirArg,
    desktops: &'static [&'static str],
) -> Terminal {
    Terminal {
        program,
        arg,
        desktops,
    }
}

/// Known terminals, in the order tried when no desktop prefers one.
pub const TERMINALS: [Terminal; 13] = [
    terminal("xdg-terminal-exec", DirArg::Inherit, &[]),
    terminal("ptyxis", DirArg::Joined("--working-directory"), &["GNOME"]),
    terminal("kgx", DirArg::Joined("--working-directory"), &["GNOME"]),
    terminal(
        "gnome-terminal",
        DirArg::Joined("--working-directory"),
        &["GNOME"],
    ),
    terminal("konsole", DirArg::Separate("--workdir"), &["KDE"]),
    terminal(
        "xfce4-terminal",
        DirArg::Joined("--working-directory"),
        &["XFCE"],
    ),
    terminal(
        "mate-terminal",
        DirArg::Joined("--working-directory"),
        &["MATE"],
    ),
    terminal("tilix", DirArg::Joined("--working-directory"), &[]),
    terminal("alacritty", DirArg::Separate("--working-directory"), &[]),
    terminal("kitty", DirArg::Separate("--directory"), &[]),
    terminal("wezterm", DirArg::Subcommand("start", "--cwd"), &[]),
    terminal("foot", DirArg::Joined("--working-directory"), &[]),
    terminal("xterm", DirArg::Inherit, &[]),
];

/// The terminals to try, best first: `xdg-terminal-exec`, then those of
/// the current desktop (`XDG_CURRENT_DESKTOP`, e.g. "KDE" or
/// "ubuntu:GNOME"), then the rest.
pub fn candidates(current_desktop: &str) -> Vec<Terminal> {
    let desktops: Vec<&str> = current_desktop.split(':').collect();
    let native = |t: &Terminal| t.desktops.iter().any(|d| desktops.contains(d));
    let (default, others) = TERMINALS.split_at(1);
    default
        .iter()
        .chain(others.iter().filter(|t| native(t)))
        .chain(others.iter().filter(|t| !native(t)))
        .copied()
        .collect()
}

/// The command line that opens `terminal` in `dir`.
pub fn command(terminal: &Terminal, dir: &Path) -> Vec<OsString> {
    let mut argv = vec![OsString::from(terminal.program)];
    match terminal.arg {
        DirArg::Joined(option) => {
            let mut arg = OsString::from(format!("{option}="));
            arg.push(dir);
            argv.push(arg);
        }
        DirArg::Separate(option) => {
            argv.push(option.into());
            argv.push(dir.into());
        }
        DirArg::Subcommand(subcommand, option) => {
            argv.push(subcommand.into());
            argv.push(option.into());
            argv.push(dir.into());
        }
        DirArg::Inherit => {}
    }
    argv
}

/// The first installed terminal.
pub fn find() -> Option<Terminal> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    candidates(&desktop)
        .into_iter()
        .find(|t| glib::find_program_in_path(t.program).is_some())
}

/// Opens a terminal in `dir`. Inside Flatpak, the host's default terminal
/// is started through `flatpak-spawn`.
pub fn open_in(dir: &Path) -> Result<(), glib::Error> {
    let argv: Vec<OsString> = if sandbox::is_flatpak() {
        let mut directory = OsString::from("--directory=");
        directory.push(dir);
        vec![
            "flatpak-spawn".into(),
            "--host".into(),
            directory,
            "xdg-terminal-exec".into(),
        ]
    } else {
        let terminal = find().ok_or_else(|| {
            glib::Error::new(
                gio::IOErrorEnum::NotFound,
                &gettextrs::gettext("No terminal application was found"),
            )
        })?;
        command(&terminal, dir)
    };
    tracing::debug!("opening a terminal: {argv:?}");

    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
    launcher.set_cwd(dir);
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(OsString::as_os_str).collect();
    let process = launcher.spawn(&argv)?;
    // Reap the process when it exits.
    glib::spawn_future_local(async move {
        let _ = process.wait_future().await;
    });
    Ok(())
}
