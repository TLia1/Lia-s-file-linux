//! Crash recovery: temporary items logged by a job are removed and
//! moved-aside files renamed back when the job's process is gone.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::gio;
use lia_files::services::file_ops::{engine, *};
use lia_files::services::recovery::{Recovered, RecoveryLog, recover};

/// A pid no process can have (above the kernel's maximum).
const DEAD_PID: u32 = 999_999_999;

fn logs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

/// Makes the logs in `dir` look like they were left by a crashed process.
fn orphan_logs(dir: &Path) {
    for (i, log) in logs(dir).into_iter().enumerate() {
        std::fs::rename(&log, dir.join(format!("{DEAD_PID}-{i}.log"))).unwrap();
    }
}

/// Writes records as a job would, then "crashes" (the log isn't closed).
fn crashed_log(dir: &Path, record: impl FnOnce(&RecoveryLog)) {
    let log = RecoveryLog::new(dir);
    record(&log);
    std::mem::forget(log);
    orphan_logs(dir);
}

#[test]
fn removes_temporary_items_of_a_crashed_job() {
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let part = work.path().join(".photo.jpg.part");
    let scratch = work.path().join(".archive.part");
    std::fs::write(&part, "half").unwrap();
    std::fs::create_dir_all(scratch.join("inner")).unwrap();
    std::fs::write(scratch.join("inner/file"), "x").unwrap();
    // A visible file must never be deleted, even if a log names it.
    let visible = work.path().join("photo.jpg");
    std::fs::write(&visible, "keep").unwrap();

    crashed_log(state.path(), |log| {
        log.temporary(&part);
        log.temporary(&scratch);
        log.temporary(&visible);
        log.temporary(&work.path().join(".already-gone.part"));
    });

    let recovered = recover(state.path());
    assert_eq!(
        recovered,
        Recovered {
            removed: 2,
            restored: 0
        }
    );
    assert!(!part.exists() && !scratch.exists());
    assert_eq!(std::fs::read_to_string(&visible).unwrap(), "keep");
    assert!(logs(state.path()).is_empty(), "the log is removed");
}

#[test]
fn renames_moved_aside_files_back() {
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    std::fs::write(dir.join(".a.txt.renaming"), "a").unwrap();
    std::fs::write(dir.join(".b.txt.renaming"), "b").unwrap();
    // "b.txt" was taken meanwhile: the file comes back as "b (2).txt".
    std::fs::write(dir.join("b.txt"), "other").unwrap();

    crashed_log(state.path(), |log| {
        log.moved_aside(&dir.join(".a.txt.renaming"), &dir.join("a.txt"));
        log.moved_aside(&dir.join(".b.txt.renaming"), &dir.join("b.txt"));
    });

    assert_eq!(recover(state.path()).restored, 2);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "a");
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "other");
    assert_eq!(std::fs::read_to_string(dir.join("b (2).txt")).unwrap(), "b");
}

#[test]
fn names_with_newlines_and_invalid_utf8_survive_the_log() {
    use std::os::unix::ffi::OsStrExt;
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let odd = std::ffi::OsStr::from_bytes(b".line\nbreak \xff.part");
    let part = work.path().join(odd);
    std::fs::write(&part, "x").unwrap();
    let aside = work.path().join(".été.renaming");
    let original = work.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::write(&aside, "y").unwrap();

    crashed_log(state.path(), |log| {
        log.temporary(&part);
        log.moved_aside(&aside, &original);
    });

    assert_eq!(
        recover(state.path()),
        Recovered {
            removed: 1,
            restored: 1
        }
    );
    assert!(!part.exists());
    assert_eq!(std::fs::read_to_string(&original).unwrap(), "y");
}

#[test]
fn ignores_logs_of_running_processes_and_truncated_records() {
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let part = work.path().join(".mine.part");
    std::fs::write(&part, "x").unwrap();

    // This process is alive: its log is left alone.
    let log = RecoveryLog::new(state.path());
    log.temporary(&part);
    assert_eq!(recover(state.path()), Recovered::default());
    assert!(part.exists());
    assert_eq!(logs(state.path()).len(), 1);
    // Closing the log (the job ended) removes it.
    log.close();
    assert!(logs(state.path()).is_empty());

    // A record cut short by the crash is ignored, the ones before it kept.
    let mut bytes = b"T".to_vec();
    bytes.extend_from_slice(part.as_os_str().as_encoded_bytes());
    bytes.extend_from_slice(b"\0A/some/where\0");
    std::fs::write(state.path().join(format!("{DEAD_PID}-1.log")), bytes).unwrap();
    assert_eq!(recover(state.path()).removed, 1);
    assert!(!part.exists());
}

#[test]
fn a_finished_job_leaves_no_log() {
    let state = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), "alpha").unwrap();

    let control = Control::new();
    control.set_recovery_log(RecoveryLog::new(state.path()));
    let outcome = engine::run(
        &Operation::copy_into(
            &[gio::File::for_path(src.path().join("a.txt"))],
            &gio::File::for_path(dst.path()),
        ),
        &control,
        &FixedAnswers {
            conflict: ConflictChoice::Skip,
            error: ErrorChoice::Skip,
        },
    );
    assert_eq!(outcome.status, Status::Done);
    assert!(logs(state.path()).is_empty());
}

/// A copy interrupted mid-file: what the log holds at that moment is
/// enough to find and remove the partial file.
#[test]
fn a_copy_cut_short_can_be_cleaned_up() {
    let state = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("big.bin"), vec![7u8; 128 << 20]).unwrap();

    // The job is paused as soon as its partial file appears; the copy may
    // rarely finish first, then it's tried again.
    for _ in 0..5 {
        let dst = tempfile::tempdir().unwrap();
        let part = dst.path().join(".big.bin.part");
        let control = Arc::new(Control::new());
        control.set_recovery_log(RecoveryLog::new(state.path()));
        let worker = {
            let control = control.clone();
            let op = Operation::copy_into(
                &[gio::File::for_path(src.path().join("big.bin"))],
                &gio::File::for_path(dst.path()),
            );
            std::thread::spawn(move || {
                let answers = FixedAnswers {
                    conflict: ConflictChoice::Skip,
                    error: ErrorChoice::Cancel,
                };
                engine::run(&op, &control, &answers).status
            })
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !part.exists() && !worker.is_finished() {
            assert!(Instant::now() < deadline, "the copy never started");
        }
        control.set_paused(true);
        if worker.is_finished() || dst.path().join("big.bin").exists() {
            control.cancel();
            worker.join().unwrap();
            continue;
        }

        // Simulate the crash: the log as it is now, owned by a dead process.
        let live = logs(state.path());
        assert_eq!(live.len(), 1);
        std::fs::copy(&live[0], state.path().join(format!("{DEAD_PID}-1.log"))).unwrap();
        assert_eq!(recover(state.path()).removed, 1);
        assert!(!part.exists());
        assert!(live[0].exists(), "the running job's own log is kept");

        control.cancel();
        assert_eq!(worker.join().unwrap(), Status::Cancelled);
        assert!(logs(state.path()).is_empty());
        assert!(!dst.path().join("big.bin").exists());
        return;
    }
    panic!("the copy was always too fast to interrupt");
}
