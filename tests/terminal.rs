//! Choosing a terminal and building its command line for "Open in Terminal".

use std::ffi::OsString;
use std::path::Path;

use lia_files::services::terminal::{TERMINALS, candidates, command};

fn named(name: &str) -> lia_files::services::terminal::Terminal {
    *TERMINALS.iter().find(|t| t.program == name).unwrap()
}

fn argv(name: &str, dir: &str) -> Vec<OsString> {
    command(&named(name), Path::new(dir))
}

#[test]
fn xdg_terminal_exec_comes_first() {
    for desktop in ["", "KDE", "GNOME", "ubuntu:GNOME"] {
        assert_eq!(candidates(desktop)[0].program, "xdg-terminal-exec");
    }
}

#[test]
fn prefers_the_desktop_terminal() {
    assert_eq!(candidates("KDE")[1].program, "konsole");
    let gnome: Vec<_> = candidates("ubuntu:GNOME")
        .iter()
        .skip(1)
        .take(3)
        .map(|t| t.program)
        .collect();
    assert_eq!(gnome, ["ptyxis", "kgx", "gnome-terminal"]);
    // Every known terminal is still tried.
    assert_eq!(candidates("KDE").len(), TERMINALS.len());
}

#[test]
fn passes_the_folder_the_way_each_terminal_expects() {
    let dir = "/home/lia/My Projects";
    assert_eq!(argv("xdg-terminal-exec", dir), ["xdg-terminal-exec"]);
    assert_eq!(
        argv("gnome-terminal", dir),
        [
            "gnome-terminal",
            "--working-directory=/home/lia/My Projects"
        ]
    );
    assert_eq!(
        argv("konsole", dir),
        ["konsole", "--workdir", "/home/lia/My Projects"]
    );
    assert_eq!(
        argv("kitty", dir),
        ["kitty", "--directory", "/home/lia/My Projects"]
    );
    assert_eq!(
        argv("wezterm", dir),
        ["wezterm", "start", "--cwd", "/home/lia/My Projects"]
    );
}
