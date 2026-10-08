//! Server addresses typed in Connect to Server, and the recent servers list.

use lia_files::model::places::{THIS_PC_URI, is_this_pc};
use lia_files::services::network::{AddressError, MAX_RECENT_SERVERS, add_recent, parse_address};

use gtk::{gio, prelude::*};

#[test]
fn accepts_server_uris() {
    assert_eq!(
        parse_address(" smb://nas/share "),
        Ok("smb://nas/share".into())
    );
    assert_eq!(
        parse_address("SFTP://me@host:2222/srv"),
        Ok("sftp://me@host:2222/srv".into())
    );
    assert_eq!(parse_address("dav://host/dav"), Ok("dav://host/dav".into()));
}

#[test]
fn converts_windows_share_paths() {
    assert_eq!(
        parse_address(r"\\nas\photos\2026"),
        Ok("smb://nas/photos/2026".into())
    );
    assert_eq!(parse_address(r"\\"), Err(AddressError::MissingHost));
}

#[test]
fn rejects_incomplete_addresses() {
    assert_eq!(parse_address("   "), Err(AddressError::Empty));
    assert_eq!(parse_address("nas/share"), Err(AddressError::MissingScheme));
    assert_eq!(
        parse_address("http://example.com"),
        Err(AddressError::UnknownScheme("http".into()))
    );
    assert_eq!(parse_address("smb://"), Err(AddressError::MissingHost));
    assert_eq!(
        parse_address("smb:///share"),
        Err(AddressError::MissingHost)
    );
}

#[test]
fn recent_servers_are_most_recent_first_without_duplicates() {
    let list = vec!["smb://a".to_owned(), "sftp://b/".to_owned()];
    assert_eq!(add_recent(&list, "sftp://b"), ["sftp://b", "smb://a"]);
    assert_eq!(
        add_recent(&list, "ftp://c"),
        ["ftp://c", "smb://a", "sftp://b/"]
    );

    let long: Vec<String> = (0..20).map(|i| format!("smb://host{i}")).collect();
    let updated = add_recent(&long, "smb://new");
    assert_eq!(updated.len(), MAX_RECENT_SERVERS);
    assert_eq!(updated[0], "smb://new");
}

#[test]
fn this_pc_is_a_location_of_its_own() {
    let file = gio::File::for_uri(THIS_PC_URI);
    assert!(is_this_pc(&file));
    assert!(!is_this_pc(&gio::File::for_path("/")));
    assert_eq!(file.uri(), THIS_PC_URI);
}
