//! `org.freedesktop.FileManager1` argument handling.

use gtk::gio;
use gtk::gio::prelude::*;
use lia_files::services::file_manager1::{group_by_parent, parse_uris};

fn uris(files: &[gio::File]) -> Vec<String> {
    files.iter().map(|f| f.uri().into()).collect()
}

#[test]
fn accepts_uris_and_paths_and_drops_duplicates() {
    let files = parse_uris(&[
        "file:///tmp/a.txt".into(),
        "/tmp/a.txt".into(),
        "".into(),
        "  ".into(),
        "file:///tmp/b%20c.txt".into(),
    ]);
    assert_eq!(uris(&files), ["file:///tmp/a.txt", "file:///tmp/b%20c.txt"]);
}

#[test]
fn groups_items_by_folder_in_order() {
    let files = parse_uris(&[
        "file:///home/u/Downloads/x.zip".into(),
        "file:///home/u/Pictures/p.png".into(),
        "file:///home/u/Downloads/y.pdf".into(),
    ]);
    let groups = group_by_parent(&files);
    let summary: Vec<(String, Vec<String>)> = groups
        .iter()
        .map(|(folder, items)| (folder.uri().into(), uris(items)))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "file:///home/u/Downloads".to_string(),
                vec![
                    "file:///home/u/Downloads/x.zip".to_string(),
                    "file:///home/u/Downloads/y.pdf".to_string()
                ]
            ),
            (
                "file:///home/u/Pictures".to_string(),
                vec!["file:///home/u/Pictures/p.png".to_string()]
            ),
        ]
    );
}

#[test]
fn root_is_shown_as_a_folder() {
    let groups = group_by_parent(&parse_uris(&["file:///".into()]));
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0.uri(), "file:///");
    assert!(groups[0].1.is_empty());
}
