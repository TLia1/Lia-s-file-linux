//! Undo/redo history, and trashing (with a private trash).

use std::path::Path;

use gtk::{gio, prelude::*};
use lia_files::services::file_ops::{engine, *};
use lia_files::services::undo::{UndoRecord, UndoStack};

fn rename(name: &str) -> UndoRecord {
    let file = gio::File::for_path(format!("/tmp/{name}"));
    UndoRecord::new(
        Operation::Rename {
            file: file.clone(),
            name: format!("{name}-renamed"),
        },
        Operation::Move {
            pairs: vec![(file.clone(), file)],
        },
    )
}

fn forward_name(record: &UndoRecord) -> String {
    match &record.forward {
        Operation::Rename { file, .. } => file.basename().unwrap().to_string_lossy().into_owned(),
        _ => unreachable!(),
    }
}

#[test]
fn undo_redo_history() {
    let mut stack = UndoStack::default();
    assert!(!stack.can_undo() && !stack.can_redo());

    stack.record(rename("a"));
    stack.record(rename("b"));
    assert_eq!(stack.undo_label().as_deref(), Some("Rename"));

    // Undo b, then a.
    let b = stack.pop_undo().unwrap();
    assert_eq!(forward_name(&b), "b");
    stack.push_undone(b);
    let a = stack.pop_undo().unwrap();
    stack.push_undone(a);
    assert!(!stack.can_undo() && stack.can_redo());

    // Redo comes back in order: a, then b.
    let a = stack.pop_redo().unwrap();
    assert_eq!(forward_name(&a), "a");
    stack.push_redone(a);
    assert!(
        stack.can_redo(),
        "redoing keeps the rest of the redo history"
    );

    // A new action clears the redo history.
    stack.record(rename("c"));
    assert!(!stack.can_redo());
    assert_eq!(forward_name(&stack.pop_undo().unwrap()), "c");
    assert_eq!(forward_name(&stack.pop_undo().unwrap()), "a");
    assert!(stack.pop_undo().is_none());
}

#[test]
fn history_is_bounded() {
    let mut stack = UndoStack::default();
    for i in 0..150 {
        stack.record(rename(&i.to_string()));
    }
    let mut n = 0;
    while stack.pop_undo().is_some() {
        n += 1;
    }
    assert_eq!(n, 100);
}

#[test]
fn trash_records_a_restore() {
    // A private home and trash on the same file system as the files, so the
    // user's real trash is left alone. GLib reads these once, so this is
    // the only test in this binary that touches GIO's user directories.
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".local/share");
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY: set before any other thread reads the environment.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_DATA_HOME", &data);
    }

    let doomed = home.path().join("doomed.txt");
    std::fs::write(&doomed, "bye").unwrap();
    let outcome = engine::run(
        &Operation::Trash {
            files: vec![gio::File::for_path(&doomed)],
        },
        &Control::new(),
        &FixedAnswers {
            conflict: ConflictChoice::Skip,
            error: ErrorChoice::Skip,
        },
    );
    if !outcome.errors.is_empty() {
        // Trashing isn't available in this environment (e.g. some sandboxes).
        eprintln!("skipping: {:?}", outcome.errors);
        return;
    }
    assert_eq!(outcome.status, Status::Done);
    assert!(!doomed.exists());
    assert!(data.join("Trash/files/doomed.txt").exists());
    let undo = outcome.undo.expect("trashing is undoable");
    assert_eq!(undo.label(), "Move to Trash");
    match &undo.inverse {
        Operation::Restore { files } => {
            assert_eq!(files[0].path().as_deref(), Some(Path::new(&doomed)));
        }
        other => panic!("unexpected inverse {other:?}"),
    }

    // Undo: restored from the trash directory itself (works without GVfs).
    let outcome = engine::run(
        &undo.inverse,
        &Control::new(),
        &FixedAnswers {
            conflict: ConflictChoice::Skip,
            error: ErrorChoice::Skip,
        },
    );
    assert_eq!(outcome.errors, Vec::<String>::new());
    assert_eq!(std::fs::read_to_string(&doomed).unwrap(), "bye");
    assert!(!data.join("Trash/files/doomed.txt").exists());
    assert!(!data.join("Trash/info/doomed.txt.trashinfo").exists());
    // …and the restore can itself be undone (trashed again).
    assert!(matches!(
        outcome.undo.map(|u| u.inverse),
        Some(Operation::Trash { .. })
    ));
}

#[test]
fn history_survives_a_restart() {
    use std::os::unix::ffi::OsStrExt;
    let odd = gio::File::for_path(std::ffi::OsStr::from_bytes(b"/tmp/odd \xff\nname"));
    let dir = gio::File::for_path("/tmp/dir");
    let mut stack = UndoStack::default();
    stack.record(UndoRecord::new(
        Operation::copy_into(std::slice::from_ref(&odd), &dir),
        Operation::Trash {
            files: vec![dir.child("odd")],
        },
    ));
    stack.record(UndoRecord::new(
        Operation::CreateFile {
            parent: dir.clone(),
            name: "new.txt".into(),
            template: None,
        },
        Operation::EmptyTrash,
    ));
    stack.record(rename("c"));
    let undone = stack.pop_undo().unwrap();
    stack.push_undone(undone);

    let mut saved = UndoStack::parse(&stack.to_json());
    assert_eq!(saved.redo_label().as_deref(), Some("Rename"));
    assert_eq!(saved.undo_label().as_deref(), Some("New File"));
    saved.pop_undo();
    let copy = saved.pop_undo().unwrap();
    let Operation::Copy { pairs } = &copy.forward else {
        panic!("not a copy: {copy:?}");
    };
    // Any file name, as bytes, comes back unchanged.
    assert!(pairs[0].0.equal(&odd));
    assert!(pairs[0].1.has_parent(Some(&dir)));
    assert!(!saved.can_undo());

    // A damaged file gives an empty history rather than an error.
    assert!(!UndoStack::parse("{not json").can_undo());
}
