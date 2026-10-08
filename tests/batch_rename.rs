//! Batch rename: the rules (pattern, find & replace, case), the checks,
//! and the engine applying them — including swapped names.

use std::collections::HashSet;
use std::path::Path;

use gtk::gio;
use lia_files::services::batch_rename::*;
use lia_files::services::file_ops::engine;
use lia_files::services::file_ops::*;

fn sources(names: &[&str]) -> Vec<Source> {
    names
        .iter()
        .map(|name| Source {
            name: (*name).to_owned(),
            is_dir: false,
            date: "2026-10-08".into(),
            folder: "f".into(),
        })
        .collect()
}

fn rename(rule: Rule, names: &[&str]) -> Vec<String> {
    new_names(&rule, &sources(names))
}

#[test]
fn patterns() {
    let rule = |pattern: &str, start| Rule::Pattern {
        pattern: pattern.into(),
        start,
    };
    assert_eq!(
        rename(rule("{name}_{n:03}", 1), &["a.jpg", "b.jpg", "notes"]),
        ["a_001.jpg", "b_002.jpg", "notes_003"]
    );
    assert_eq!(
        rename(rule("Holiday {n}", 9), &["x.png", "y.png"]),
        ["Holiday 9.png", "Holiday 10.png"]
    );
    assert_eq!(
        rename(rule("{date} {name}", 1), &["scan.pdf"]),
        ["2026-10-08 scan.pdf"]
    );
    // Unknown or unfinished tokens stay as typed.
    assert_eq!(
        rename(rule("{what} {n:x} {name", 1), &["a.txt"]),
        ["{what} {n:x} {name.txt"]
    );
    // Compound extensions are kept whole.
    assert_eq!(
        rename(rule("backup-{n}", 1), &["site.tar.gz"]),
        ["backup-1.tar.gz"]
    );
}

#[test]
fn find_and_replace() {
    let rule = |find: &str, replace: &str, match_case| Rule::Replace {
        find: find.into(),
        replace: replace.into(),
        match_case,
    };
    assert_eq!(
        rename(rule("IMG", "Photo", true), &["IMG_01.jpg", "img_02.jpg"]),
        ["Photo_01.jpg", "img_02.jpg"]
    );
    assert_eq!(
        rename(rule("img", "Photo", false), &["IMG_01.jpg", "img_img.jpg"]),
        ["Photo_01.jpg", "Photo_Photo.jpg"]
    );
    // Only the name: the extension isn't touched.
    assert_eq!(rename(rule("jpg", "png", true), &["jpg.jpg"]), ["png.jpg"]);
    // Non-ASCII, case-insensitive.
    assert_eq!(
        rename(rule("été", "summer", false), &["Photos ÉTÉ.png"]),
        ["Photos summer.png"]
    );
    assert_eq!(rename(rule("", "x", false), &["same.txt"]), ["same.txt"]);
}

#[test]
fn case_changes() {
    let names = ["my HOLIDAY_photos-rome.JPG"];
    assert_eq!(
        rename(Rule::Case(CaseChange::Lower), &names),
        ["my holiday_photos-rome.JPG"]
    );
    assert_eq!(
        rename(Rule::Case(CaseChange::Upper), &names),
        ["MY HOLIDAY_PHOTOS-ROME.JPG"]
    );
    assert_eq!(
        rename(Rule::Case(CaseChange::Title), &names),
        ["My Holiday_Photos-Rome.JPG"]
    );
    assert_eq!(
        rename(Rule::Case(CaseChange::Sentence), &names),
        ["My holiday_photos-rome.JPG"]
    );
}

#[test]
fn checks() {
    let sources = sources(&["a.txt", "b.txt", "c.txt"]);
    let existing: HashSet<(String, String)> = [("f".to_owned(), "taken.txt".to_owned())].into();
    let names: Vec<String> = ["b.txt", "x.txt", "taken.txt"].map(String::from).to_vec();
    // "b.txt" is freed by the batch: renaming a → b is fine.
    assert_eq!(
        check(&sources, &names, &existing, false),
        [None, None, Some(Problem::Exists)]
    );

    let names: Vec<String> = ["same.txt", "same.txt", "c.txt"].map(String::from).to_vec();
    assert_eq!(
        check(&sources, &names, &existing, false),
        [Some(Problem::Duplicate), Some(Problem::Duplicate), None]
    );

    // A renamed file can't take the name of one that stays.
    let names: Vec<String> = ["c.txt", "b.txt", "c.txt"].map(String::from).to_vec();
    assert_eq!(
        check(&sources, &names, &existing, false)[0],
        Some(Problem::Duplicate)
    );

    let names: Vec<String> = ["a/b.txt", "b.txt", "what?.txt"].map(String::from).to_vec();
    let problems = check(&sources, &names, &existing, true);
    assert!(matches!(problems[0], Some(Problem::Invalid(_))));
    assert!(matches!(problems[2], Some(Problem::Invalid(_))));
}

// ── Engine ─────────────────────────────────────────────────────────────────

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn batch(dir: &Path, renames: &[(&str, &str)]) -> Outcome {
    let op = Operation::BatchRename {
        renames: renames
            .iter()
            .map(|(from, to)| (gio::File::for_path(dir.join(from)), (*to).to_owned()))
            .collect(),
    };
    engine::run(
        &op,
        &Control::new(),
        &FixedAnswers {
            conflict: ConflictChoice::Skip,
            error: ErrorChoice::Skip,
        },
    )
}

#[test]
fn renames_and_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(&d.join("a.txt"), "A");
    write(&d.join("b.txt"), "B");

    let outcome = batch(d, &[("a.txt", "one.txt"), ("b.txt", "two.txt")]);
    assert_eq!(outcome.succeeded, 2);
    assert_eq!(names_in(d), ["one.txt", "two.txt"]);
    assert_eq!(read(&d.join("one.txt")), "A");

    // The inverse gives the old names back.
    let record = outcome.undo.expect("undoable");
    let undone = engine::run(
        &record.inverse,
        &Control::new(),
        &FixedAnswers {
            conflict: ConflictChoice::Skip,
            error: ErrorChoice::Skip,
        },
    );
    assert_eq!(undone.succeeded, 2);
    assert_eq!(names_in(d), ["a.txt", "b.txt"]);
    assert_eq!(read(&d.join("a.txt")), "A");
}

#[test]
fn swaps_and_shifts() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(&d.join("a"), "A");
    write(&d.join("b"), "B");
    write(&d.join("c"), "C");

    // a → b → c → a: a cycle.
    let outcome = batch(d, &[("a", "b"), ("b", "c"), ("c", "a")]);
    assert_eq!(outcome.succeeded, 3, "{:?}", outcome.errors);
    assert_eq!(names_in(d), ["a", "b", "c"]);
    assert_eq!(read(&d.join("b")), "A");
    assert_eq!(read(&d.join("c")), "B");
    assert_eq!(read(&d.join("a")), "C");

    // Shifting numbers up: 1 → 2, 2 → 3.
    write(&d.join("1"), "one");
    write(&d.join("2"), "two");
    let outcome = batch(d, &[("1", "2"), ("2", "3")]);
    assert_eq!(outcome.succeeded, 2);
    assert_eq!(read(&d.join("2")), "one");
    assert_eq!(read(&d.join("3")), "two");
}

#[test]
fn never_overwrites_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(&d.join("a"), "A");
    write(&d.join("b"), "B");
    write(&d.join("taken"), "keep me");

    // b's new name is taken by a file outside the batch, and a needs b's
    // name: neither can be renamed, and nothing is lost or left hidden.
    let outcome = batch(d, &[("a", "b"), ("b", "taken")]);
    assert_eq!(read(&d.join("taken")), "keep me");
    assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
    let names = names_in(d);
    assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
    assert_eq!(names.len(), 3);
    assert_eq!(read(&d.join("b")), "A");
}
