use gtk::{gio, prelude::*};
use lia_files::services::custom_actions::discover_in;
use std::os::unix::fs::PermissionsExt;

fn desktop(name: &str, mime: &str, exec: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName={name}\nMimeType={mime};\nExec={exec}\n")
}

#[test]
fn user_actions_override_system_and_match_the_entire_selection() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user");
    let system = dir.path().join("system");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(
        system.join("wallpaper.desktop"),
        desktop("System", "image/png", "example %F"),
    )
    .unwrap();
    std::fs::write(
        user.join("wallpaper.desktop"),
        desktop("User", "image/png", "example %F"),
    )
    .unwrap();
    std::fs::write(
        system.join("hidden.desktop"),
        desktop("Hidden", "image/png", "example %F"),
    )
    .unwrap();
    std::fs::write(
        user.join("hidden.desktop"),
        "[Desktop Entry]\nType=Application\nHidden=true\n",
    )
    .unwrap();
    std::fs::write(
        user.join("bad.desktop"),
        desktop("Bad", "image/png", "example %k"),
    )
    .unwrap();
    let actions = discover_in(&[user.clone(), system]);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].name, "User");
    let png = (gio::File::for_path("/tmp/test.png"), "image/png".to_owned());
    let text = (
        gio::File::for_path("/tmp/test.txt"),
        "text/plain".to_owned(),
    );
    assert!(actions[0].matches(std::slice::from_ref(&png)));
    assert!(!actions[0].matches(&[png.clone(), text]));
    assert!(!actions[0].matches(&[(
        gio::File::for_uri("sftp://host/test.png"),
        "image/png".into()
    )]));
    std::fs::write(
        user.join("remote.desktop"),
        desktop("Remote", "image/png", "example %U"),
    )
    .unwrap();
    let remote = discover_in(&[user])
        .into_iter()
        .find(|a| a.name == "Remote")
        .unwrap();
    assert!(remote.matches(&[(
        gio::File::for_uri("sftp://host/test.png"),
        "image/png".into()
    )]));
}

#[test]
fn exec_passes_filenames_as_arguments_without_shell_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("record");
    let output = dir.path().join("arguments");
    std::fs::write(
        &script,
        "#!/bin/sh\nout=$1\nshift\nprintf '%s\\n' \"$@\" > \"$out\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.path().join("record.desktop"),
        desktop(
            "Record",
            "text/plain",
            &format!("\"{}\" \"{}\" %F", script.display(), output.display()),
        ),
    )
    .unwrap();
    let files: Vec<_> = ["a b.txt", "quote'$(echo unsafe);.txt"]
        .iter()
        .map(|name| gio::File::for_path(dir.path().join(name)))
        .collect();
    let action = discover_in(&[dir.path().to_owned()]).remove(0);
    action.launch(&files, None).unwrap();
    for _ in 0..100 {
        if let Ok(actual) = std::fs::read_to_string(&output) {
            let expected = files
                .iter()
                .map(|f| format!("{}\n", f.path().unwrap().display()))
                .collect::<String>();
            if actual == expected {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("action did not receive the exact file arguments");
}

#[test]
fn escaped_field_codes_do_not_change_the_selection_contract() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("single.desktop"),
        desktop("Single", "image/png", "example %%U %%F %f"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("duplicate.desktop"),
        desktop("Invalid", "image/png", "example %f %U"),
    )
    .unwrap();
    let actions = discover_in(&[dir.path().to_owned()]);
    assert_eq!(actions.len(), 1);
    let file = (gio::File::for_path("/tmp/a.png"), "image/png".into());
    assert!(actions[0].matches(std::slice::from_ref(&file)));
    assert!(!actions[0].matches(&[file.clone(), file]));
    assert!(!actions[0].matches(&[(gio::File::for_uri("sftp://host/a.png"), "image/png".into())]));
}
