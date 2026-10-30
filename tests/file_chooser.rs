use gtk::{gio, prelude::*};
use lia_files::services::file_chooser::{
    Options, folder_from_options, save_files_uris, save_uri, selection_uris,
};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[test]
fn folder_bytes_preserve_non_utf8_and_reject_relative_or_embedded_nul() {
    let path = std::ffi::OsString::from_vec(b"/tmp/lia-\xff".to_vec());
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    let mut options = Options::new();
    options.insert("current_folder".into(), bytes.to_variant());
    assert_eq!(
        folder_from_options(&options)
            .unwrap()
            .path()
            .unwrap()
            .as_os_str(),
        path
    );
    for invalid in [b"relative\0".to_vec(), b"/tmp/a\0b\0".to_vec(), vec![0]] {
        options.insert("current_folder".into(), invalid.to_variant());
        assert!(folder_from_options(&options).is_none());
    }
}

#[test]
fn validates_kind_count_existence_and_uri_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a # b.txt");
    std::fs::write(&path, "data").unwrap();
    let file = gio::File::for_path(&path);
    let folder = gio::File::for_path(dir.path());
    assert!(selection_uris(&[], false, false).is_err());
    assert!(selection_uris(std::slice::from_ref(&folder), false, false).is_err());
    assert!(selection_uris(std::slice::from_ref(&file), true, false).is_err());
    assert_eq!(
        selection_uris(std::slice::from_ref(&folder), true, false).unwrap(),
        [folder.uri()]
    );
    let uris = selection_uris(std::slice::from_ref(&file), false, false).unwrap();
    assert!(uris[0].ends_with("a%20%23%20b.txt"));
    assert!(selection_uris(&[file.clone(), file.clone()], false, false).is_err());
    assert_eq!(
        selection_uris(&[file.clone(), file.clone()], false, true)
            .unwrap()
            .len(),
        2
    );
    assert!(selection_uris(&[gio::File::for_uri("smb://server/share/a")], false, false).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(selection_uris(&[file], false, false).is_err());
}

#[test]
fn symlink_selection_follows_target_kind() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("folder-link");
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();
    let file = gio::File::for_path(link);
    assert!(selection_uris(std::slice::from_ref(&file), true, false).is_ok());
    assert!(selection_uris(&[file], false, false).is_err());
}

#[test]
fn save_names_a_new_file_in_a_local_folder() {
    let dir = tempfile::tempdir().unwrap();
    let folder = gio::File::for_path(dir.path());
    let uri = save_uri(&folder, "a # b.txt", false).unwrap();
    assert!(uri.ends_with("a%20%23%20b.txt"));
    assert!(save_uri(&folder, "", false).is_err());
    assert!(save_uri(&folder, "a/b", false).is_err());
    assert!(save_uri(&folder, "a:b", true).is_err());
    assert!(save_uri(&gio::File::for_uri("smb://server/share"), "a", false).is_err());
    assert!(save_uri(&folder.child("missing"), "a", false).is_err());
}

#[test]
fn save_files_keeps_existing_files_and_names_empty_ones() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let folder = gio::File::for_path(dir.path());
    let uri = |name: &str| gio::File::for_path(dir.path().join(name)).uri().to_string();
    let uris = save_files_uris(
        &folder,
        &[
            b"a.txt\0".to_vec(),
            b"b.txt".to_vec(),
            b"b.txt".to_vec(),
            b"/etc/../c.txt".to_vec(),
            Vec::new(),
        ],
    )
    .unwrap();
    assert_eq!(
        uris,
        [
            uri("a (2).txt"),
            uri("b.txt"),
            uri("b (2).txt"),
            uri("c.txt"),
            uri("Untitled"),
        ]
    );
}
