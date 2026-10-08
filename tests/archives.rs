//! Archives on real temporary trees: compress → extract round trips in
//! every format, where extracted files end up, unsafe entry paths,
//! conflicts, cancellation and undo records.

use std::path::{Path, PathBuf};

use gtk::{gio, prelude::*};
use lia_files::services::archives::{self, ArchiveFormat, libarchive};
use lia_files::services::file_ops::engine;
use lia_files::services::file_ops::*;

fn file(path: impl AsRef<Path>) -> gio::File {
    gio::File::for_path(path)
}

fn write(path: impl AsRef<Path>, contents: &str) {
    let path = path.as_ref();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Sorted names in `dir`, hidden ones included.
fn names(dir: impl AsRef<Path>) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

const ANSWERS: FixedAnswers = FixedAnswers {
    conflict: ConflictChoice::KeepBoth,
    error: ErrorChoice::Skip,
};

fn run(op: &Operation) -> Outcome {
    engine::run(op, &Control::new(), &ANSWERS)
}

fn compress(sources: &[PathBuf], target: &Path, format: ArchiveFormat) -> Outcome {
    run(&Operation::Compress {
        sources: sources.iter().map(file).collect(),
        target: file(target),
        format,
    })
}

fn extract(archive: &Path, destination: &Path) -> Outcome {
    run(&Operation::Extract {
        archives: vec![file(archive)],
        destination: file(destination),
    })
}

/// docs/{a.txt, sub/b.txt, link -> a.txt}
fn sample(root: &Path) -> PathBuf {
    let docs = root.join("docs");
    write(docs.join("a.txt"), "alpha");
    write(docs.join("sub/b.txt"), "bravo");
    std::os::unix::fs::symlink("a.txt", docs.join("link")).unwrap();
    docs
}

#[test]
fn round_trip_in_every_format() {
    for format in ArchiveFormat::ALL {
        let dir = tempfile::tempdir().unwrap();
        let docs = sample(dir.path());
        let archive = dir.path().join(format!("docs{}", format.extension()));

        let outcome = compress(std::slice::from_ref(&docs), &archive, format);
        assert_eq!(
            outcome.status,
            Status::Done,
            "{format:?}: {:?}",
            outcome.errors
        );
        assert!(outcome.created[0].equal(&file(&archive)));
        // No temporary file left.
        let mut expected = vec![archive.file_name().unwrap().to_str().unwrap(), "docs"];
        expected.sort();
        assert_eq!(names(dir.path()), expected);

        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let outcome = extract(&archive, &out);
        assert_eq!(
            outcome.status,
            Status::Done,
            "{format:?}: {:?}",
            outcome.errors
        );
        // One top-level folder: extracted as is.
        assert_eq!(names(&out), ["docs"], "{format:?}");
        assert_eq!(read(out.join("docs/a.txt")), "alpha");
        assert_eq!(read(out.join("docs/sub/b.txt")), "bravo");
        assert_eq!(
            std::fs::read_link(out.join("docs/link")).unwrap(),
            Path::new("a.txt"),
            "{format:?}"
        );
        assert!(outcome.created[0].equal(&file(out.join("docs"))));
    }
}

#[test]
fn several_items_go_into_a_folder_named_after_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path().join("one.txt"), "1");
    write(dir.path().join("two.txt"), "2");
    let archive = dir.path().join("pair.tar.gz");
    let sources = [dir.path().join("one.txt"), dir.path().join("two.txt")];
    compress(&sources, &archive, ArchiveFormat::TarGz);

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let outcome = extract(&archive, &out);
    assert_eq!(names(&out), ["pair"]);
    assert_eq!(names(out.join("pair")), ["one.txt", "two.txt"]);
    assert!(outcome.created[0].equal(&file(out.join("pair"))));
}

#[test]
fn single_compressed_file_is_decompressed() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    write(&source, "some notes");
    let gz = dir.path().join("notes.txt.gz");
    let status = std::process::Command::new("gzip")
        .arg("-k")
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    extract(&gz, &out);
    assert_eq!(names(&out), ["notes.txt"]);
    assert_eq!(read(out.join("notes.txt")), "some notes");
}

#[test]
fn existing_items_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let docs = sample(dir.path());
    let archive = dir.path().join("docs.zip");
    compress(&[docs], &archive, ArchiveFormat::Zip);

    // Extracting next to the original folder: "Keep both".
    let outcome = extract(&archive, dir.path());
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(names(dir.path()), ["docs", "docs (2)", "docs.zip"]);
    assert_eq!(read(dir.path().join("docs (2)/a.txt")), "alpha");

    // An archive created where one exists gets a numbered name.
    let outcome = compress(&[dir.path().join("docs")], &archive, ArchiveFormat::Zip);
    assert!(outcome.created[0].equal(&file(dir.path().join("docs (2).zip"))));
}

#[test]
fn entries_cannot_escape_the_destination() {
    assert_eq!(
        archives::sanitize_entry_path(b"a/b.txt"),
        Some("a/b.txt".into())
    );
    assert_eq!(
        archives::sanitize_entry_path(b"./a/./b"),
        Some("a/b".into())
    );
    assert_eq!(
        archives::sanitize_entry_path(b"/etc/passwd"),
        Some("etc/passwd".into())
    );
    assert_eq!(archives::sanitize_entry_path(b"../evil"), None);
    assert_eq!(archives::sanitize_entry_path(b"a/../../evil"), None);
    assert_eq!(archives::sanitize_entry_path(b""), None);
    assert_eq!(archives::sanitize_entry_path(b"./"), None);

    // A tar with "../escaped.txt" and a normal entry.
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    write(work.join("escaped.txt"), "outside");
    write(work.join("inner/ok.txt"), "inside");
    let archive = dir.path().join("evil.tar");
    let status = std::process::Command::new("bsdtar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(work.join("inner"))
        .args(["ok.txt", "../escaped.txt"])
        .status()
        .unwrap();
    assert!(status.success());

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let outcome = extract(&archive, &out);
    assert_eq!(outcome.status, Status::Done);
    // Only the safe entry; nothing written next to `out`.
    assert_eq!(names(&out), ["ok.txt"]);
    assert_eq!(names(dir.path()), ["evil.tar", "out", "work"]);
}

#[test]
fn broken_archives_are_reported_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("broken.zip");
    write(&archive, "this is not a zip file");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let outcome = extract(&archive, &out);
    assert_eq!(outcome.succeeded, 0);
    assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
    assert!(outcome.undo.is_none());
    // The hidden extraction folder is gone.
    assert!(names(&out).is_empty());
}

#[test]
fn cancelling_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let docs = sample(dir.path());
    let archive = dir.path().join("docs.7z");
    compress(
        std::slice::from_ref(&docs),
        &archive,
        ArchiveFormat::SevenZip,
    );
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let control = Control::new();
    control.cancel();
    let op = Operation::Extract {
        archives: vec![file(&archive)],
        destination: file(&out),
    };
    let outcome = engine::run(&op, &control, &ANSWERS);
    assert_eq!(outcome.status, Status::Cancelled);
    assert!(names(&out).is_empty());

    let control = Control::new();
    control.cancel();
    let op = Operation::Compress {
        sources: vec![file(&docs)],
        target: file(dir.path().join("new.zip")),
        format: ArchiveFormat::Zip,
    };
    let outcome = engine::run(&op, &control, &ANSWERS);
    assert_eq!(outcome.status, Status::Cancelled);
    assert_eq!(names(dir.path()), ["docs", "docs.7z", "out"]);
}

#[test]
fn undo_trashes_what_was_created() {
    let dir = tempfile::tempdir().unwrap();
    let docs = sample(dir.path());
    let archive = dir.path().join("docs.zip");
    let outcome = compress(&[docs], &archive, ArchiveFormat::Zip);
    let record = outcome.undo.expect("undoable");
    match &record.inverse {
        Operation::Trash { files } => assert!(files[0].equal(&file(&archive))),
        other => panic!("unexpected inverse {other:?}"),
    }

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let outcome = extract(&archive, &out);
    match &outcome.undo.expect("undoable").inverse {
        Operation::Trash { files } => assert!(files[0].equal(&file(out.join("docs")))),
        other => panic!("unexpected inverse {other:?}"),
    }
}

#[test]
fn reader_lists_entries() {
    let dir = tempfile::tempdir().unwrap();
    let docs = sample(dir.path());
    let archive = dir.path().join("docs.tar.xz");
    compress(&[docs], &archive, ArchiveFormat::TarXz);

    let mut reader = libarchive::Reader::open(&archive, false).unwrap();
    let mut entries = Vec::new();
    while let Some(entry) = reader.next_entry().unwrap() {
        // Tar stores folders with a trailing slash.
        let path = String::from_utf8(entry.path).unwrap();
        entries.push((path.trim_end_matches('/').to_owned(), entry.kind));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    use libarchive::EntryKind::*;
    assert_eq!(
        entries,
        [
            ("docs".to_owned(), Dir),
            ("docs/a.txt".to_owned(), File),
            ("docs/link".to_owned(), Symlink),
            ("docs/sub".to_owned(), Dir),
            ("docs/sub/b.txt".to_owned(), File),
        ]
    );
    assert!(reader.bytes_read() > 0);
}

#[test]
fn naming_rules() {
    assert_eq!(archives::archive_stem("photos.tar.gz"), "photos");
    assert_eq!(archives::archive_stem("notes.txt.gz"), "notes.txt");
    assert_eq!(archives::archive_stem("site.tgz"), "site");
    assert_eq!(archives::archive_stem("archive.zip"), "archive");
    assert_eq!(archives::archive_stem("noext"), "noext");
    assert_eq!(
        archives::suggested_name(&[("report.pdf".into(), false)]),
        "report"
    );
    assert_eq!(
        archives::suggested_name(&[("my.folder".into(), true)]),
        "my.folder"
    );
    assert!(archives::is_extractable("application/zip"));
    assert!(archives::is_extractable("application/gzip"));
    // Office documents are zip files, but not archives to the user.
    assert!(!archives::is_extractable(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    ));
    for format in ArchiveFormat::ALL {
        assert_eq!(ArchiveFormat::from_nick(format.nick()), format);
    }
}
