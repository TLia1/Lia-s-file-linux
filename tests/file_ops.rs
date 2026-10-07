//! The file operations engine on real temporary trees: copy, move, delete,
//! conflicts, errors, cancellation and the undo records it produces.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::{gio, prelude::*};
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

/// Sorted names in `dir`.
fn names(dir: impl AsRef<Path>) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn answers(conflict: ConflictChoice) -> FixedAnswers {
    FixedAnswers {
        conflict,
        error: ErrorChoice::Skip,
    }
}

fn run(op: &Operation, ui: &dyn Interaction) -> Outcome {
    engine::run(op, &Control::new(), ui)
}

/// Records the questions asked.
#[derive(Default)]
struct Recorder {
    conflicts: Mutex<Vec<Conflict>>,
    errors: Mutex<Vec<OpError>>,
    answer: Option<ConflictChoice>,
}

impl Interaction for Recorder {
    fn conflict(&self, conflict: &Conflict) -> ConflictResponse {
        self.conflicts.lock().unwrap().push(conflict.clone());
        ConflictResponse {
            choice: self.answer.unwrap_or(ConflictChoice::Skip),
            apply_to_all: false,
        }
    }

    fn error(&self, error: &OpError) -> ErrorResponse {
        self.errors.lock().unwrap().push(error.clone());
        ErrorResponse {
            choice: ErrorChoice::Skip,
            apply_to_all: false,
        }
    }
}

/// a.txt, docs/{b.txt, deep/c.txt}, link -> a.txt
fn sample_tree(root: &Path) {
    write(root.join("a.txt"), "alpha");
    write(root.join("docs/b.txt"), "bravo");
    write(root.join("docs/deep/c.txt"), "charlie");
    std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
}

#[test]
fn copies_files_and_folders_with_metadata() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    sample_tree(src.path());
    let old = filetime(src.path().join("docs/b.txt"), 1_000_000_000);
    std::fs::set_permissions(
        src.path().join("a.txt"),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();

    let sources = [
        file(src.path().join("a.txt")),
        file(src.path().join("docs")),
        file(src.path().join("link")),
    ];
    let control = Control::new();
    let outcome = engine::run(
        &Operation::copy_into(&sources, &file(dst.path())),
        &control,
        &answers(ConflictChoice::Skip),
    );

    assert_eq!(outcome.status, Status::Done);
    assert_eq!(outcome.succeeded, 3);
    assert_eq!(names(dst.path()), ["a.txt", "docs", "link"]);
    assert_eq!(read(dst.path().join("docs/deep/c.txt")), "charlie");
    // Symlinks are copied as links, not followed.
    assert_eq!(
        std::fs::read_link(dst.path().join("link")).unwrap(),
        PathBuf::from("a.txt")
    );
    // Permissions and modification times are preserved.
    let meta = std::fs::metadata(dst.path().join("a.txt")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
    let meta = std::fs::metadata(dst.path().join("docs/b.txt")).unwrap();
    assert_eq!(meta.mtime(), old);
    // Sources are untouched and no temporary file is left.
    assert_eq!(names(src.path()), ["a.txt", "docs", "link"]);
    assert!(!names(dst.path()).iter().any(|n| n.ends_with(".part")));

    let progress = control.progress();
    assert_eq!(progress.bytes_total, 5 + 5 + 7 + 5);
    assert_eq!(progress.bytes_done, progress.bytes_total);

    // Undo trashes the created top-level items.
    let undo = outcome.undo.expect("copy is undoable");
    match undo.inverse {
        Operation::Trash { files } => assert_eq!(files.len(), 3),
        other => panic!("unexpected inverse {other:?}"),
    }
}

/// Sets the mtime of `path` to `secs` and returns it.
fn filetime(path: PathBuf, secs: i64) -> i64 {
    let info = gio::FileInfo::new();
    info.set_attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED, secs as u64);
    file(&path)
        .set_attributes_from_info(&info, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .unwrap();
    secs
}

#[test]
fn pasting_into_the_same_folder_makes_copies() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path().join("note.txt"), "x");
    let source = file(dir.path().join("note.txt"));

    for _ in 0..2 {
        let outcome = run(
            &Operation::copy_into(std::slice::from_ref(&source), &file(dir.path())),
            &answers(ConflictChoice::Cancel),
        );
        assert_eq!(outcome.status, Status::Done);
    }
    assert_eq!(
        names(dir.path()),
        ["note (copy 2).txt", "note (copy).txt", "note.txt"]
    );

    let outcome = run(
        &Operation::Duplicate {
            files: vec![file(dir.path().join("note (copy).txt"))],
        },
        &answers(ConflictChoice::Cancel),
    );
    assert_eq!(
        outcome.created[0].basename().unwrap(),
        Path::new("note (copy 3).txt")
    );
}

#[test]
fn resolves_file_conflicts() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(src.path().join("a.txt"), "new");
    let source = [file(src.path().join("a.txt"))];
    let copy = Operation::copy_into(&source, &file(dst.path()));

    // Skip: the existing file is kept; the question describes both sides.
    write(dst.path().join("a.txt"), "old!");
    let recorder = Recorder::default();
    let outcome = run(&copy, &recorder);
    assert_eq!(read(dst.path().join("a.txt")), "old!");
    assert_eq!(outcome.skipped, 1);
    assert!(outcome.undo.is_none());
    let conflicts = recorder.conflicts.lock().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].source.size, 3);
    assert_eq!(conflicts[0].target.size, 4);
    assert!(conflicts[0].can_replace && !conflicts[0].is_merge);

    // Keep both: the new file gets a numbered name.
    run(&copy, &answers(ConflictChoice::KeepBoth));
    assert_eq!(names(dst.path()), ["a (2).txt", "a.txt"]);
    assert_eq!(read(dst.path().join("a (2).txt")), "new");

    // Replace.
    run(&copy, &answers(ConflictChoice::Replace));
    assert_eq!(read(dst.path().join("a.txt")), "new");
    assert_eq!(names(dst.path()), ["a (2).txt", "a.txt"]);
}

#[test]
fn merges_folders_and_never_replaces_a_folder_with_a_file() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(src.path().join("photos/1.jpg"), "one");
    write(src.path().join("photos/2.jpg"), "two!");
    write(dst.path().join("photos/2.jpg"), "old");
    write(dst.path().join("photos/3.jpg"), "three");

    // The top-level folder conflict is asked as a merge; inner conflicts
    // are asked file by file.
    let recorder = Recorder {
        answer: Some(ConflictChoice::Replace),
        ..Default::default()
    };
    let outcome = run(
        &Operation::copy_into(&[file(src.path().join("photos"))], &file(dst.path())),
        &recorder,
    );
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(
        names(dst.path().join("photos")),
        ["1.jpg", "2.jpg", "3.jpg"]
    );
    assert_eq!(read(dst.path().join("photos/2.jpg")), "two!");
    let conflicts = recorder.conflicts.lock().unwrap();
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts[0].is_merge);
    assert!(!conflicts[1].is_merge);
    // Undo of a merge only removes what was added, not the whole folder.
    match outcome.undo.unwrap().inverse {
        Operation::Trash { files } => {
            let mut names: Vec<_> = files.iter().map(|f| f.basename().unwrap()).collect();
            names.sort();
            assert_eq!(names, [Path::new("1.jpg"), Path::new("2.jpg")]);
        }
        other => panic!("unexpected inverse {other:?}"),
    }

    // A file can't replace a folder: "Replace" falls back to keeping both.
    write(src.path().join("x"), "file");
    std::fs::create_dir(dst.path().join("x")).unwrap();
    let recorder = Recorder {
        answer: Some(ConflictChoice::Replace),
        ..Default::default()
    };
    run(
        &Operation::copy_into(&[file(src.path().join("x"))], &file(dst.path())),
        &recorder,
    );
    assert!(!recorder.conflicts.lock().unwrap()[0].can_replace);
    assert!(dst.path().join("x").is_dir());
    assert_eq!(read(dst.path().join("x (2)")), "file");
}

#[test]
fn moves_and_undoes_moves() {
    let root = tempfile::tempdir().unwrap();
    sample_tree(&root.path().join("src"));
    std::fs::create_dir(root.path().join("dst")).unwrap();
    let sources = [
        file(root.path().join("src/a.txt")),
        file(root.path().join("src/docs")),
    ];

    let outcome = run(
        &Operation::move_into(&sources, &file(root.path().join("dst"))),
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(names(root.path().join("src")), ["link"]);
    assert_eq!(names(root.path().join("dst")), ["a.txt", "docs"]);
    assert_eq!(read(root.path().join("dst/docs/deep/c.txt")), "charlie");

    let undo = outcome.undo.unwrap();
    let outcome = run(&undo.inverse, &answers(ConflictChoice::Skip));
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(names(root.path().join("src")), ["a.txt", "docs", "link"]);
    assert!(names(root.path().join("dst")).is_empty());

    // Redo.
    run(&undo.forward, &answers(ConflictChoice::Skip));
    assert_eq!(names(root.path().join("dst")), ["a.txt", "docs"]);
}

#[test]
fn moves_across_file_systems() {
    // The temp dir (often tmpfs) and the target dir (on disk) are usually
    // different devices, which exercises copy + delete.
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    sample_tree(src.path());
    let devices_differ = std::fs::metadata(src.path()).unwrap().dev()
        != std::fs::metadata(dst.path()).unwrap().dev();

    let outcome = run(
        &Operation::move_into(&[file(src.path().join("docs"))], &file(dst.path())),
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(names(src.path()), ["a.txt", "link"]);
    assert_eq!(read(dst.path().join("docs/deep/c.txt")), "charlie");
    if devices_differ {
        eprintln!("cross-device move exercised");
    }

    let undo = outcome.undo.unwrap();
    run(&undo.inverse, &answers(ConflictChoice::Skip));
    assert_eq!(names(src.path()), ["a.txt", "docs", "link"]);
    assert_eq!(read(src.path().join("docs/deep/c.txt")), "charlie");
}

#[test]
fn refuses_to_move_a_folder_into_itself() {
    let root = tempfile::tempdir().unwrap();
    write(root.path().join("docs/a.txt"), "a");
    let recorder = Recorder::default();
    let outcome = run(
        &Operation::move_into(
            &[file(root.path().join("docs"))],
            &file(root.path().join("docs")),
        ),
        &recorder,
    );
    assert_eq!(outcome.succeeded, 0);
    assert_eq!(outcome.skipped, 1);
    let errors = recorder.errors.lock().unwrap();
    assert!(errors[0].fatal);
    assert_eq!(names(root.path().join("docs")), ["a.txt"]);

    // Moving into the folder it's already in does nothing.
    let outcome = run(
        &Operation::move_into(
            &[file(root.path().join("docs/a.txt"))],
            &file(root.path().join("docs")),
        ),
        &recorder,
    );
    assert_eq!(outcome.status, Status::Done);
    assert!(outcome.undo.is_none());
}

#[test]
fn deletes_recursively() {
    let root = tempfile::tempdir().unwrap();
    sample_tree(root.path());
    let control = Control::new();
    let outcome = engine::run(
        &Operation::Delete {
            files: vec![
                file(root.path().join("docs")),
                file(root.path().join("link")),
            ],
        },
        &control,
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(outcome.succeeded, 2);
    assert!(outcome.undo.is_none(), "permanent deletion can't be undone");
    assert_eq!(names(root.path()), ["a.txt"]);
    let progress = control.progress();
    assert_eq!(progress.files_done, 5);
    assert_eq!(progress.files_total, 5);
}

#[test]
fn reports_errors_and_skips() {
    if nix_is_root() {
        return;
    }
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(src.path().join("ok.txt"), "ok");
    write(src.path().join("secret.txt"), "no");
    std::fs::set_permissions(
        src.path().join("secret.txt"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let recorder = Recorder::default();
    let outcome = run(
        &Operation::copy_into(
            &[
                file(src.path().join("secret.txt")),
                file(src.path().join("ok.txt")),
            ],
            &file(dst.path()),
        ),
        &recorder,
    );
    assert_eq!(outcome.status, Status::Done);
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(outcome.skipped, 1);
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(recorder.errors.lock().unwrap()[0].file_name, "secret.txt");
    // The failed copy left nothing behind.
    assert_eq!(names(dst.path()), ["ok.txt"]);
}

fn nix_is_root() -> bool {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
}

#[test]
fn cancelling_leaves_no_partial_files() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(src.path().join("big.bin"), &"x".repeat(8 << 20));

    let control = Arc::new(Control::new());
    control.set_paused(true);
    let worker = {
        let control = control.clone();
        let op = Operation::copy_into(&[file(src.path().join("big.bin"))], &file(dst.path()));
        std::thread::spawn(move || {
            engine::run(&op, &control, &answers(ConflictChoice::Skip)).status
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(control.is_paused());
    control.cancel();
    assert_eq!(worker.join().unwrap(), Status::Cancelled);
    assert!(names(dst.path()).is_empty());
}

#[test]
fn pause_and_resume() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(src.path().join("a.bin"), &"x".repeat(1 << 20));

    let control = Arc::new(Control::new());
    control.set_paused(true);
    let worker = {
        let control = control.clone();
        let op = Operation::copy_into(&[file(src.path().join("a.bin"))], &file(dst.path()));
        std::thread::spawn(move || {
            engine::run(&op, &control, &answers(ConflictChoice::Skip)).status
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(!dst.path().join("a.bin").exists());
    control.set_paused(false);
    assert_eq!(worker.join().unwrap(), Status::Done);
    assert_eq!(
        std::fs::metadata(dst.path().join("a.bin")).unwrap().len(),
        1 << 20
    );
}

#[test]
fn renames_creates_and_links() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path().join("old.txt"), "x");

    let outcome = run(
        &Operation::Rename {
            file: file(dir.path().join("old.txt")),
            name: "new.txt".into(),
        },
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(names(dir.path()), ["new.txt"]);
    run(
        &outcome.undo.unwrap().inverse,
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(names(dir.path()), ["old.txt"]);

    // New folders get numbered names.
    for _ in 0..2 {
        run(
            &Operation::CreateFolder {
                parent: file(dir.path()),
                name: "New Folder".into(),
            },
            &answers(ConflictChoice::Skip),
        );
    }
    assert_eq!(
        names(dir.path()),
        ["New Folder", "New Folder (2)", "old.txt"]
    );

    // Files from a template copy its contents.
    let outcome = run(
        &Operation::CreateFile {
            parent: file(dir.path()),
            name: "old.txt".into(),
            template: Some(file(dir.path().join("old.txt"))),
        },
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(
        outcome.created[0].basename().unwrap(),
        Path::new("old (2).txt")
    );
    assert_eq!(read(dir.path().join("old (2).txt")), "x");
    match &outcome.undo.unwrap().forward {
        Operation::CreateFile { name, .. } => assert_eq!(name, "old (2).txt"),
        other => panic!("unexpected forward {other:?}"),
    }

    let links = tempfile::tempdir().unwrap();
    run(
        &Operation::link_into(&[file(dir.path().join("old.txt"))], &file(links.path())),
        &answers(ConflictChoice::Skip),
    );
    assert_eq!(
        std::fs::read_link(links.path().join("old.txt")).unwrap(),
        dir.path().join("old.txt")
    );
}
