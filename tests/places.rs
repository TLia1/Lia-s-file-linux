//! The GTK bookmarks file format used for pinned folders.

use lia_files::model::places::{
    Bookmark, add_bookmarks, parse_bookmarks, remove_bookmark, serialize_bookmarks,
};

#[test]
fn parses_uris_and_labels() {
    let parsed = parse_bookmarks(
        "file:///home/lia/Projects\n\
         file:///home/lia/My%20Music Tunes and more\n\
         \n\
         sftp://server/srv   \n",
    );
    assert_eq!(
        parsed,
        vec![
            Bookmark::new("file:///home/lia/Projects"),
            Bookmark {
                uri: "file:///home/lia/My%20Music".into(),
                label: Some("Tunes and more".into()),
            },
            Bookmark::new("sftp://server/srv"),
        ]
    );
}

#[test]
fn round_trips() {
    let text = "file:///a\nfile:///b Label B\n";
    assert_eq!(serialize_bookmarks(&parse_bookmarks(text)), text);
}

#[test]
fn adds_without_duplicates() {
    let mut bookmarks = parse_bookmarks("file:///a\n");
    let changed = add_bookmarks(&mut bookmarks, &["file:///a".into(), "file:///b".into()]);
    assert!(changed);
    assert_eq!(serialize_bookmarks(&bookmarks), "file:///a\nfile:///b\n");
    assert!(!add_bookmarks(&mut bookmarks, &["file:///b".into()]));
}

#[test]
fn removes_by_uri() {
    let mut bookmarks = parse_bookmarks("file:///a\nfile:///b Bee\nfile:///c\n");
    assert!(remove_bookmark(&mut bookmarks, "file:///b"));
    assert_eq!(serialize_bookmarks(&bookmarks), "file:///a\nfile:///c\n");
    assert!(!remove_bookmark(&mut bookmarks, "file:///b"));
}
