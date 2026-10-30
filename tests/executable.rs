use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use gtk::{gio, prelude::*};
use lia_files::services::executable::{ATTRIBUTES, Executable};

fn inspect(path: &Path) -> Option<Executable> {
    let file = gio::File::for_path(path);
    let info = file
        .query_info(
            ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .unwrap();
    Executable::from_info(&file, &info)
}

fn script(path: &Path, contents: &str, mode: u32) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn executable_scripts_run_literally_in_their_own_folder() {
    let dir = tempfile::tempdir().unwrap();
    // If interpreted as shell source this name would create "injected".
    let path = dir.path().join("a b;touch injected;#.sh");
    script(
        &path,
        "#!/bin/sh\nprintf '%s' \"$PWD\" > working-directory\n",
        0o755,
    );
    let executable = inspect(&path).expect("shell script should be runnable");
    assert!(executable.is_script);
    assert!(executable.can_execute);
    executable
        .launch(false)
        .unwrap()
        .wait_check(gio::Cancellable::NONE)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("working-directory")).unwrap(),
        dir.path().to_str().unwrap()
    );
    assert!(!dir.path().join("injected").exists());
}

#[test]
fn shebang_interpreter_is_preserved_and_plain_shell_scripts_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    script(
        &path,
        "#!/bin/bash\nitems=(one two)\nprintf '%s' \"${items[1]}\" > output\n",
        0o755,
    );
    inspect(&path)
        .unwrap()
        .launch(false)
        .unwrap()
        .wait_check(gio::Cancellable::NONE)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("output")).unwrap(),
        "two"
    );
    script(&path, "printf plain > output\n", 0o755);
    inspect(&path)
        .unwrap()
        .launch(false)
        .unwrap()
        .wait_check(gio::Cancellable::NONE)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("output")).unwrap(),
        "plain"
    );
}

#[test]
fn execution_permission_is_required_and_is_not_changed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    script(&path, "#!/bin/sh\ntouch should-not-exist\n", 0o644);
    let executable = inspect(&path).unwrap();
    assert!(!executable.can_execute);
    assert!(
        executable
            .launch(false)
            .unwrap_err()
            .matches(gio::IOErrorEnum::PermissionDenied)
    );
    assert!(!dir.path().join("should-not-exist").exists());
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn permissions_are_checked_again_after_the_run_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    script(&path, "touch should-not-exist\n", 0o755);
    let executable = inspect(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        executable
            .launch(false)
            .unwrap_err()
            .matches(gio::IOErrorEnum::PermissionDenied)
    );
    assert!(!dir.path().join("should-not-exist").exists());
}

#[test]
fn binaries_and_symlinks_launch_without_a_mime_handler() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native program");
    std::fs::copy("/bin/true", &path).unwrap();
    let link = dir.path().join("linked program");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    for path in [&path, &link] {
        let executable = inspect(path).expect("ELF program should be runnable");
        assert!(!executable.is_script);
        executable
            .launch(false)
            .unwrap()
            .wait_check(gio::Cancellable::NONE)
            .unwrap();
    }
}

#[test]
fn documents_directories_and_remote_files_are_not_executed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    script(&path, "ordinary document\n", 0o755);
    assert!(inspect(&path).is_none());
    assert!(inspect(dir.path()).is_none());
    let info = gio::FileInfo::new();
    info.set_file_type(gio::FileType::Regular);
    info.set_content_type("text/x-shellscript");
    info.set_attribute_boolean(gio::FILE_ATTRIBUTE_ACCESS_CAN_EXECUTE, true);
    for uri in ["sftp://example.org/script.sh", "trash:///script.sh"] {
        assert!(Executable::from_info(&gio::File::for_uri(uri), &info).is_none());
    }
}
