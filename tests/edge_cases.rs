//! Awkward file systems and file names: unreadable folders, broken
//! symbolic links, names that aren't UTF-8 or contain newlines, 255-byte
//! names, read-only destinations, a destination that disappears mid-copy
//! (a USB stick pulled out) and FAT drives.
//!
//! The FAT test needs a mounted FAT drive, given by `LIA_TEST_FAT_DIR`
//! (e.g. a loop image mounted with `udisksctl loop-setup` + `mount`).
//! The disk-full case is in `disk_full.rs`: it changes process limits.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::{gio, glib, prelude::*};
use lia_files::model::{DirectoryModel, FileItem};
use lia_files::services::file_ops::{engine, *};
use lia_files::util::format;

fn file(path: impl AsRef<Path>) -> gio::File {
    gio::File::for_path(path)
}

fn bytes(name: &[u8]) -> &OsStr {
    OsStr::from_bytes(name)
}

/// Sorted names in `dir`, as bytes.
fn names(dir: impl AsRef<Path>) -> Vec<OsString> {
    let mut names: Vec<OsString> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    names
}

/// Every file under `dir` whose name ends in ".part".
fn leftovers(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.as_os_str().as_bytes().ends_with(b".part") {
            found.push(path.clone());
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            found.extend(leftovers(&path));
        }
    }
    found
}

/// Records the errors and skips them.
#[derive(Default)]
struct Recorder {
    errors: Mutex<Vec<OpError>>,
}

impl Interaction for Recorder {
    fn conflict(&self, _: &Conflict) -> ConflictResponse {
        ConflictResponse {
            choice: ConflictChoice::KeepBoth,
            apply_to_all: true,
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

impl Recorder {
    fn error_count(&self) -> usize {
        self.errors.lock().unwrap().len()
    }
}

fn run(op: &Operation, ui: &dyn Interaction) -> Outcome {
    engine::run(op, &Control::new(), ui)
}

/// Permissions don't stop root: permission tests are pointless then.
fn permissions_apply(locked: &Path) -> bool {
    std::fs::read_dir(locked).is_err()
}

#[test]
fn unreadable_folder_is_reported_and_the_rest_copied() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let tree = src.path().join("tree");
    std::fs::create_dir_all(tree.join("locked")).unwrap();
    std::fs::write(tree.join("locked/secret.txt"), "s").unwrap();
    std::fs::write(tree.join("ok.txt"), "ok").unwrap();
    std::fs::write(tree.join("unreadable.txt"), "u").unwrap();
    std::fs::set_permissions(tree.join("locked"), PermissionsExt::from_mode(0o000)).unwrap();
    std::fs::set_permissions(
        tree.join("unreadable.txt"),
        PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let unlock = || {
        let _ = std::fs::set_permissions(tree.join("locked"), PermissionsExt::from_mode(0o755));
    };
    if !permissions_apply(&tree.join("locked")) {
        unlock();
        return;
    }

    let ui = Recorder::default();
    let outcome = run(
        &Operation::copy_into(&[file(&tree)], &file(dst.path())),
        &ui,
    );
    unlock();

    assert_eq!(outcome.status, Status::Done);
    assert!(ui.error_count() >= 2, "both unreadable items are reported");
    assert_eq!(
        std::fs::read_to_string(dst.path().join("tree/ok.txt")).unwrap(),
        "ok"
    );
    assert!(!dst.path().join("tree/unreadable.txt").exists());
    assert!(!dst.path().join("tree/locked/secret.txt").exists());
    assert!(leftovers(dst.path()).is_empty());
    // An incomplete copy isn't reported as a full success.
    assert_eq!(outcome.succeeded, 0);
}

#[test]
fn moving_a_partly_unreadable_folder_keeps_what_wasnt_copied() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let tree = src.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("ok.txt"), "ok").unwrap();
    std::fs::write(tree.join("unreadable.txt"), "u").unwrap();
    std::fs::set_permissions(
        tree.join("unreadable.txt"),
        PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    if std::fs::read(tree.join("unreadable.txt")).is_ok() {
        return;
    }

    let ui = Recorder::default();
    let outcome = run(
        &Operation::move_into(&[file(&tree)], &file(dst.path())),
        &ui,
    );
    assert_eq!(outcome.status, Status::Done);
    // Same device: the folder is renamed as a whole, nothing is lost.
    let moved = dst.path().join("tree");
    assert!(moved.join("ok.txt").exists() && moved.join("unreadable.txt").exists());
    let _ = std::fs::set_permissions(
        moved.join("unreadable.txt"),
        PermissionsExt::from_mode(0o644),
    );
}

#[test]
fn broken_symlinks_are_copied_and_moved_as_links() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("nowhere/missing", src.path().join("broken")).unwrap();
    std::os::unix::fs::symlink("broken", src.path().join("chain")).unwrap();
    let ui = Recorder::default();

    let outcome = run(
        &Operation::copy_into(
            &[
                file(src.path().join("broken")),
                file(src.path().join("chain")),
            ],
            &file(dst.path()),
        ),
        &ui,
    );
    assert_eq!((outcome.status, outcome.succeeded), (Status::Done, 2));
    assert_eq!(ui.error_count(), 0);
    assert_eq!(
        std::fs::read_link(dst.path().join("broken")).unwrap(),
        PathBuf::from("nowhere/missing")
    );

    let moved = tempfile::tempdir().unwrap();
    let outcome = run(
        &Operation::move_into(&[file(dst.path().join("broken"))], &file(moved.path())),
        &ui,
    );
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(
        std::fs::read_link(moved.path().join("broken")).unwrap(),
        PathBuf::from("nowhere/missing")
    );
    assert!(std::fs::symlink_metadata(dst.path().join("broken")).is_err());

    // Duplicating a broken link copies the link.
    let outcome = run(
        &Operation::Duplicate {
            files: vec![file(moved.path().join("broken"))],
        },
        &ui,
    );
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(names(moved.path()), ["broken", "broken (copy)"]);
}

#[test]
fn names_that_arent_utf8_or_contain_newlines_are_kept_byte_for_byte() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let odd: [&[u8]; 4] = [
        b"latin1 caf\xe9.txt",
        b"two\nlines.txt",
        b"\xff\xfe",
        b"tab\there -- \"quotes\" 'and' \\back*slash?",
    ];
    for name in odd {
        std::fs::write(src.path().join(bytes(name)), name).unwrap();
    }
    let dir = src.path().join(bytes(b"folder \xe9\n"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(bytes(b"inner\xff")), "inner").unwrap();

    let sources: Vec<gio::File> = std::fs::read_dir(src.path())
        .unwrap()
        .map(|e| file(e.unwrap().path()))
        .collect();
    let ui = Recorder::default();
    let outcome = run(&Operation::copy_into(&sources, &file(dst.path())), &ui);
    assert_eq!(ui.error_count(), 0, "{:?}", ui.errors.lock().unwrap());
    assert_eq!(outcome.succeeded, 5);
    assert_eq!(names(dst.path()), names(src.path()));
    for name in odd {
        assert_eq!(std::fs::read(dst.path().join(bytes(name))).unwrap(), name);
    }
    assert!(
        dst.path()
            .join(bytes(b"folder \xe9\n"))
            .join(bytes(b"inner\xff"))
            .exists()
    );

    // Copying again: "keep both" numbers the copies, keeping the bytes.
    let outcome = run(
        &Operation::copy_into(&[file(src.path().join(bytes(odd[0])))], &file(dst.path())),
        &ui,
    );
    assert_eq!(outcome.succeeded, 1);
    let created = outcome.created[0].basename().unwrap();
    assert_ne!(created.as_os_str(), bytes(odd[0]));
    assert!(dst.path().join(&created).exists());

    // Moving and undoing the move.
    let moved = tempfile::tempdir().unwrap();
    let outcome = run(
        &Operation::move_into(&[file(dst.path().join(bytes(odd[1])))], &file(moved.path())),
        &ui,
    );
    assert!(moved.path().join(bytes(odd[1])).exists());
    let undo = outcome.undo.expect("a move can be undone");
    run(&undo.inverse, &ui);
    assert!(dst.path().join(bytes(odd[1])).exists());
    assert!(!moved.path().join(bytes(odd[1])).exists());
    assert_eq!(ui.error_count(), 0);
}

#[test]
fn names_of_255_bytes() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    // The longest name allowed: no room for ".name.part".
    let long = format!("{}x.txt", "é".repeat(125)); // 250 + 5 bytes
    assert_eq!(long.len(), 255);
    std::fs::write(src.path().join(&long), "long").unwrap();
    let ui = Recorder::default();

    let outcome = run(
        &Operation::copy_into(&[file(src.path().join(&long))], &file(dst.path())),
        &ui,
    );
    assert_eq!(outcome.succeeded, 1);
    assert_eq!(
        std::fs::read_to_string(dst.path().join(&long)).unwrap(),
        "long"
    );
    assert!(leftovers(dst.path()).is_empty());

    // "name (copy).txt" doesn't fit: the name is shortened to make room.
    let outcome = run(
        &Operation::Duplicate {
            files: vec![file(dst.path().join(&long))],
        },
        &ui,
    );
    assert_eq!((outcome.status, outcome.succeeded), (Status::Done, 1));
    assert_eq!(ui.error_count(), 0);
    let copy = outcome.created[0].basename().unwrap();
    let copy = copy.to_str().unwrap();
    assert!(copy.len() <= 255 && copy.ends_with(" (copy).txt"), "{copy}");
    assert_eq!(
        std::fs::read_to_string(dst.path().join(copy)).unwrap(),
        "long"
    );
}

#[test]
fn read_only_destination() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), "a").unwrap();
    std::fs::set_permissions(dst.path(), PermissionsExt::from_mode(0o555)).unwrap();
    let restore = || {
        std::fs::set_permissions(dst.path(), PermissionsExt::from_mode(0o755)).unwrap();
    };
    if std::fs::write(dst.path().join("probe"), "").is_ok() {
        restore();
        return;
    }

    let ui = Recorder::default();
    for op in [
        Operation::copy_into(&[file(src.path().join("a.txt"))], &file(dst.path())),
        Operation::move_into(&[file(src.path().join("a.txt"))], &file(dst.path())),
        Operation::CreateFolder {
            parent: file(dst.path()),
            name: "new".into(),
        },
    ] {
        let outcome = run(&op, &ui);
        assert_eq!(outcome.status, Status::Done);
        assert_eq!(outcome.succeeded, 0);
    }
    restore();
    assert_eq!(ui.error_count(), 3);
    assert!(names(dst.path()).is_empty());
    // The failed move left the source alone.
    assert_eq!(
        std::fs::read_to_string(src.path().join("a.txt")).unwrap(),
        "a"
    );
}

/// Copies `big` into `dst` and runs `interrupt` while the copy is paused
/// mid-file, then lets it go on. Returns `None` if the copy was too fast
/// to catch.
fn interrupt_copy(
    big: &Path,
    dst: &Path,
    interrupt: impl FnOnce(),
) -> Option<(Outcome, Arc<Recorder>)> {
    let part = dst.join(".big.bin.part");
    let control = Arc::new(Control::new());
    let ui = Arc::new(Recorder::default());
    let worker = {
        let (control, ui) = (control.clone(), ui.clone());
        let op = Operation::copy_into(&[file(big)], &file(dst));
        std::thread::spawn(move || engine::run(&op, &control, &*ui))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !part.exists() && !worker.is_finished() {
        assert!(Instant::now() < deadline, "the copy never started");
    }
    control.set_paused(true);
    if worker.is_finished() || dst.join("big.bin").exists() {
        control.cancel();
        worker.join().unwrap();
        return None;
    }
    interrupt();
    control.set_paused(false);
    Some((worker.join().unwrap(), ui))
}

#[test]
fn destination_disappearing_mid_copy() {
    // Like a USB stick pulled out: the destination folder is gone when the
    // copy resumes.
    let src = tempfile::tempdir().unwrap();
    let big = src.path().join("big.bin");
    std::fs::write(&big, vec![1u8; 128 << 20]).unwrap();
    for _ in 0..5 {
        let root = tempfile::tempdir().unwrap();
        let dst = root.path().join("stick");
        std::fs::create_dir(&dst).unwrap();
        let interrupted = interrupt_copy(&big, &dst, || std::fs::remove_dir_all(&dst).unwrap());
        let Some((outcome, ui)) = interrupted else {
            continue;
        };
        assert_eq!(outcome.status, Status::Done);
        assert_eq!(outcome.succeeded, 0);
        assert_eq!(ui.error_count(), 1);
        assert!(big.exists(), "the source is untouched");
        assert!(!dst.exists());
        return;
    }
    panic!("the copy was always too fast to interrupt");
}

#[test]
fn copying_to_a_fat_drive_adapts_names() {
    let Some(fat) = std::env::var_os("LIA_TEST_FAT_DIR") else {
        eprintln!("skipped: set LIA_TEST_FAT_DIR to a folder on a mounted FAT drive");
        return;
    };
    let dst = tempfile::tempdir_in(fat).unwrap();
    let src = tempfile::tempdir().unwrap();
    let names_in: [&[u8]; 5] = [b"a:b?.txt", b"bad\xff", b"trail.", b"CON.txt", b"fine.txt"];
    for name in names_in {
        std::fs::write(src.path().join(bytes(name)), name).unwrap();
    }
    // "trail" exists too: FAT would have merged it with "trail.".
    std::fs::write(src.path().join("trail"), "no dot").unwrap();
    let dir = src.path().join("why?");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("x*y"), "inner").unwrap();

    let sources: Vec<gio::File> = std::fs::read_dir(src.path())
        .unwrap()
        .map(|e| file(e.unwrap().path()))
        .collect();
    let ui = Recorder::default();
    let outcome = run(&Operation::copy_into(&sources, &file(dst.path())), &ui);
    assert_eq!(ui.error_count(), 0, "{:?}", ui.errors.lock().unwrap());
    assert_eq!(outcome.succeeded, 7);
    let read = |name: &str| std::fs::read(dst.path().join(name)).unwrap();
    assert_eq!(read("a_b_.txt"), b"a:b?.txt");
    assert_eq!(read("bad\u{FFFD}"), b"bad\xff");
    assert_eq!(read("trail_"), b"trail.");
    assert_eq!(read("trail"), b"no dot");
    assert_eq!(read("CON_.txt"), b"CON.txt");
    assert_eq!(read("fine.txt"), b"fine.txt");
    assert_eq!(read("why_/x_y"), b"inner");
    assert!(leftovers(dst.path()).is_empty());
}

/// Iterates `context` until `condition` holds, failing after 5 seconds.
fn wait_until(context: &glib::MainContext, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        if !context.iteration(false) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn folders_with_awkward_entries_list_completely() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path();
            std::fs::write(path.join(bytes(b"caf\xe9")), "").unwrap();
            std::fs::write(path.join("two\nlines"), "").unwrap();
            std::fs::write(path.join("x".repeat(255)), "").unwrap();
            std::os::unix::fs::symlink("missing", path.join("broken")).unwrap();
            std::os::unix::fs::symlink("loop", path.join("loop")).unwrap();
            std::fs::create_dir(path.join("locked")).unwrap();
            std::fs::set_permissions(path.join("locked"), PermissionsExt::from_mode(0o000))
                .unwrap();

            let model = DirectoryModel::new();
            model.load(&file(path));
            wait_until(&context, || !model.loading());
            std::fs::set_permissions(path.join("locked"), PermissionsExt::from_mode(0o755))
                .unwrap();
            assert!(model.error().is_none());
            assert_eq!(model.n_items(), 6);
            for i in 0..model.n_items() {
                let item = model.item(i).and_downcast::<FileItem>().unwrap();
                let shown = format::one_line(&item.display_name()).into_owned();
                assert!(!shown.is_empty());
                assert!(!shown.chars().any(char::is_control), "shown on one line");
            }

            // An unreadable folder is an error state, not a crash or an
            // empty folder.
            std::fs::set_permissions(path.join("locked"), PermissionsExt::from_mode(0o000))
                .unwrap();
            if permissions_apply(&path.join("locked")) {
                model.load(&file(path.join("locked")));
                wait_until(&context, || !model.loading());
                assert!(model.error().is_some());
            }
            std::fs::set_permissions(path.join("locked"), PermissionsExt::from_mode(0o755))
                .unwrap();
        })
        .unwrap();
}
