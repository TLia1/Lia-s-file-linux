//! Group-by rules (date, size and name groups) and the per-folder view
//! settings store.

use gtk::glib;
use lia_files::model::grouping::*;
use lia_files::model::view_settings::{FolderView, MAX_FOLDERS, ViewStore};

fn local(y: i32, m: i32, d: i32, h: i32) -> glib::DateTime {
    glib::DateTime::from_local(y, m, d, h, 0, 0.0).unwrap()
}

#[test]
fn date_groups() {
    // Thursday 8 October 2026, 15:00.
    let periods = Periods::at(&local(2026, 10, 8, 15));
    let group = |y, m, d| date_group(local(y, m, d, 12).to_unix(), &periods);
    assert_eq!(group(2026, 10, 8), 0, "today");
    assert_eq!(group(2026, 10, 7), 1, "yesterday");
    assert_eq!(group(2026, 10, 5), 2, "Monday: earlier this week");
    assert_eq!(group(2026, 9, 28), 3, "last week");
    assert_eq!(group(2026, 10, 2), 3, "last week, though this month");
    assert_eq!(group(2026, 9, 27), 5, "last month");
    assert_eq!(group(2026, 3, 1), 6, "earlier this year");
    assert_eq!(group(2025, 12, 31), 7, "a long time ago");
    assert_eq!(group(2027, 1, 1), 0, "future dates count as today");
    assert_eq!(date_group(0, &periods), 8, "unknown");

    // Late in the month: "earlier this month" exists.
    let periods = Periods::at(&local(2026, 10, 22, 9));
    assert_eq!(date_group(local(2026, 10, 3, 12).to_unix(), &periods), 4);
    // January: last month is in the previous year.
    let periods = Periods::at(&local(2027, 1, 20, 9));
    assert_eq!(date_group(local(2026, 12, 15, 12).to_unix(), &periods), 5);
}

#[test]
fn size_and_name_groups() {
    assert_eq!(size_group(4096, true), 0);
    assert_eq!(size_group(0, false), 1);
    assert_eq!(size_group(15_999, false), 2);
    assert_eq!(size_group(16_000, false), 3);
    assert_eq!(size_group(500_000_000, false), 5);
    assert_eq!(size_group(5_000_000_000, false), 7);

    assert_eq!(name_group("2024 taxes"), 0);
    assert_eq!(name_group("apple"), 1);
    assert_eq!(name_group("École"), 1);
    assert_eq!(name_group("Photos"), 2);
    assert_eq!(name_group(".zshrc"), 3);
    assert_eq!(name_group("_build"), 4);
    assert_eq!(name_group("日本"), 4);

    for group in GroupBy::ALL {
        assert_eq!(GroupBy::from_nick(group.nick()), group);
    }
}

fn view(sort: &str) -> FolderView {
    FolderView {
        view: "grid".into(),
        sort: sort.into(),
        descending: true,
        group: "modified".into(),
        used: 0,
    }
}

#[test]
fn store_round_trip() {
    let mut store = ViewStore::default();
    store.set("file:///home/lia/Pictures", view("modified"), 100);
    let json = store.to_json();
    let read = ViewStore::parse(&json);
    let folder = read.get("file:///home/lia/Pictures").unwrap();
    assert_eq!(folder.sort, "modified");
    assert_eq!(folder.used, 100);
    assert!(read.get("file:///elsewhere").is_none());

    // Older files without "group" still load.
    let old = r#"{"folders":{"file:///a":{"view":"details","sort":"name","descending":false}}}"#;
    assert_eq!(
        ViewStore::parse(old).get("file:///a").unwrap().group,
        "none"
    );
    // A damaged file is ignored.
    assert!(ViewStore::parse("{not json").is_empty());
}

#[test]
fn store_forgets_oldest_folders() {
    let mut store = ViewStore::default();
    for i in 0..MAX_FOLDERS + 10 {
        store.set(&format!("file:///f{i}"), view("name"), i as i64);
    }
    assert_eq!(store.len(), MAX_FOLDERS);
    assert!(store.get("file:///f0").is_none());
    assert!(
        store
            .get(&format!("file:///f{}", MAX_FOLDERS + 9))
            .is_some()
    );
    assert!(store.remove("file:///f20"));
    assert!(!store.remove("file:///f20"));
}
