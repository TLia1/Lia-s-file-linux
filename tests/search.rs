//! Recursive search (`services::search`) and the search filter chips.

use std::collections::BTreeSet;

use gtk::{gio, glib, prelude::*};
use lia_files::model::search_filters::{DateRange, Filters, Kind, SizeRange};
use lia_files::services::search::{self, Event, Query, Search};
use lia_files::widgets::folder_pane::relative_folder;

/// Runs a search to the end; returns the matches as paths relative to
/// `root`.
fn search(root: &std::path::Path, text: &str, show_hidden: bool) -> BTreeSet<String> {
    let context = glib::MainContext::new();
    let query = Query {
        text: text.to_lowercase(),
        show_hidden,
    };
    let search = Search::start(&gio::File::for_path(root), query);
    context.block_on(async {
        let mut found = BTreeSet::new();
        while let Some(event) = search.next().await {
            let Event::Found(matches) = event else {
                panic!("unexpected limit");
            };
            for m in matches {
                let item = m.into_item();
                let path = item.file().path().unwrap();
                found.insert(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
        found
    })
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn finds_matches_in_subfolders() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("a/b/c")).unwrap();
    std::fs::create_dir_all(root.join("Reports")).unwrap();
    std::fs::write(root.join("report.txt"), "1").unwrap();
    std::fs::write(root.join("a/b/c/Annual REPORT.pdf"), "2").unwrap();
    std::fs::write(root.join("a/notes.txt"), "3").unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();
    std::fs::write(root.join(".hidden/report-old.txt"), "4").unwrap();
    std::fs::write(root.join(".report"), "5").unwrap();
    // A link back to the root must not make the walk loop.
    std::os::unix::fs::symlink(root, root.join("a/loop")).unwrap();

    assert_eq!(
        search(root, "Report", false),
        set(&["Reports", "report.txt", "a/b/c/Annual REPORT.pdf"])
    );
    assert_eq!(
        search(root, "report", true),
        set(&[
            "Reports",
            "report.txt",
            "a/b/c/Annual REPORT.pdf",
            ".hidden/report-old.txt",
            ".report",
        ])
    );
    assert_eq!(search(root, "nothing-matches", true), set(&[]));
}

#[test]
fn results_are_complete_items() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/photo.png"), "not really").unwrap();
    let context = glib::MainContext::new();
    let query = Query {
        text: "photo".into(),
        show_hidden: false,
    };
    let search = Search::start(&gio::File::for_path(dir.path()), query);
    let item = context.block_on(async {
        let Some(Event::Found(mut matches)) = search.next().await else {
            panic!("no results");
        };
        matches.remove(0).into_item()
    });
    assert_eq!(item.display_name(), "photo.png");
    assert_eq!(item.size(), 10);
    assert!(!item.is_dir());
    assert!(item.icon().is_some());
    assert!(item.content_type().is_some());
}

#[test]
fn dropping_a_search_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..50 {
        std::fs::create_dir_all(dir.path().join(format!("d{i}/x/y"))).unwrap();
        std::fs::write(dir.path().join(format!("d{i}/x/y/match{i}")), "").unwrap();
    }
    let query = Query {
        text: "match".into(),
        show_hidden: false,
    };
    // Must not hang or panic when the receiver goes away mid-walk.
    drop(Search::start(&gio::File::for_path(dir.path()), query));
}

#[test]
fn name_matching_ignores_case() {
    assert!(search::name_matches("Holiday Photos", "photo"));
    assert!(search::name_matches("ÉTÉ.jpg", "été"));
    assert!(!search::name_matches("notes.txt", "photo"));
    assert!(search::name_matches("anything", ""));
}

#[test]
fn kind_filter() {
    assert!(Kind::Any.matches("inode/directory", true));
    assert!(Kind::Folder.matches("inode/directory", true));
    assert!(!Kind::Folder.matches("text/plain", false));
    assert!(Kind::Image.matches("image/png", false));
    assert!(!Kind::Image.matches("inode/directory", true));
    assert!(Kind::Video.matches("video/mp4", false));
    assert!(Kind::Audio.matches("audio/flac", false));
    assert!(Kind::Archive.matches("application/zip", false));
    assert!(Kind::Archive.matches("application/x-compressed-tar", false));
    assert!(!Kind::Archive.matches("application/pdf", false));
    assert!(Kind::Document.matches("application/pdf", false));
    assert!(Kind::Document.matches("text/markdown", false));
    assert!(Kind::Document.matches(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        false
    ));
    assert!(Kind::Document.matches("application/vnd.oasis.opendocument.text", false));
    assert!(!Kind::Document.matches("image/png", false));
    for kind in Kind::ALL {
        assert_eq!(Kind::from_nick(kind.nick()), kind);
    }
}

#[test]
fn date_filter() {
    let now = 1_000_000_000;
    let today = now - 3600;
    assert_eq!(DateRange::Any.earliest(now, today), None);
    assert_eq!(DateRange::Today.earliest(now, today), Some(today));
    assert_eq!(DateRange::Week.earliest(now, today), Some(now - 7 * 86400));
    let filters = Filters {
        date: DateRange::Today,
        ..Default::default()
    };
    let earliest = filters.date.earliest(now, today);
    assert!(filters.matches("text/plain", false, 1, today + 1, earliest));
    assert!(!filters.matches("text/plain", false, 1, today - 1, earliest));
    for date in DateRange::ALL {
        assert_eq!(DateRange::from_nick(date.nick()), date);
    }
}

#[test]
fn size_filter() {
    assert!(SizeRange::Any.matches(0, true));
    assert!(SizeRange::Empty.matches(0, false));
    assert!(!SizeRange::Empty.matches(0, true));
    assert!(SizeRange::Tiny.matches(15_999, false));
    assert!(SizeRange::Small.matches(16_000, false));
    assert!(SizeRange::Medium.matches(1_000_000, false));
    assert!(SizeRange::Large.matches(999_999_999, false));
    assert!(SizeRange::Huge.matches(5_000_000_000, false));
    assert!(!SizeRange::Huge.matches(5_000_000, false));
    for size in SizeRange::ALL {
        assert_eq!(SizeRange::from_nick(size.nick()), size);
    }
    assert!(!Filters::default().is_active());
    assert!(
        Filters {
            size: SizeRange::Small,
            ..Default::default()
        }
        .is_active()
    );
}

#[test]
fn result_folders() {
    let root = gio::File::for_path("/data/Docs");
    let file = |path: &str| gio::File::for_path(format!("/data/Docs/{path}"));
    assert_eq!(relative_folder(&root, &file("a.txt")), "Docs");
    assert_eq!(relative_folder(&root, &file("x/y/a.txt")), "Docs/x/y");
    assert_eq!(
        relative_folder(&root, &gio::File::for_path("/elsewhere/a.txt")),
        "/elsewhere"
    );
}
