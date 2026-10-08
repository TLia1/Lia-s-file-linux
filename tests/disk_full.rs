//! A copy that runs out of space must fail cleanly: an error is reported,
//! no partial file is left, and a file it was replacing is untouched.
//!
//! "Out of space" is simulated with a file size limit (`RLIMIT_FSIZE`):
//! writes past it fail with EFBIG, as they would with ENOSPC. The limit
//! applies to the whole process, hence this separate test binary with a
//! single test.

use std::path::Path;
use std::sync::Mutex;

use gtk::gio;
use lia_files::services::file_ops::{engine, *};

const LIMIT: u64 = 1 << 20;

#[derive(Default)]
struct Recorder {
    errors: Mutex<Vec<OpError>>,
}

impl Interaction for Recorder {
    fn conflict(&self, _: &Conflict) -> ConflictResponse {
        ConflictResponse {
            choice: ConflictChoice::Replace,
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

fn limit_file_size(bytes: u64) {
    // SAFETY: plain libc calls; ignoring SIGXFSZ makes oversized writes
    // fail with EFBIG instead of killing the process.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        let limit = libc::rlimit {
            rlim_cur: bytes,
            rlim_max: libc::RLIM_INFINITY,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
    }
}

fn file(path: impl AsRef<Path>) -> gio::File {
    gio::File::for_path(path)
}

#[test]
fn running_out_of_space_mid_copy() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("small.txt"), "fits").unwrap();
    std::fs::write(src.path().join("big.bin"), vec![0u8; 4 * LIMIT as usize]).unwrap();
    std::fs::write(dst.path().join("big.bin"), "the old version").unwrap();

    limit_file_size(LIMIT);
    let ui = Recorder::default();
    let outcome = engine::run(
        &Operation::copy_into(
            &[
                file(src.path().join("big.bin")),
                file(src.path().join("small.txt")),
            ],
            &file(dst.path()),
        ),
        &Control::new(),
        &ui,
    );
    limit_file_size(libc::RLIM_INFINITY);

    assert_eq!(outcome.status, Status::Done);
    assert_eq!(outcome.succeeded, 1, "the small file still fits");
    assert_eq!(ui.errors.lock().unwrap().len(), 1);
    let mut names: Vec<_> = std::fs::read_dir(dst.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["big.bin", "small.txt"], "no partial file is left");
    assert_eq!(
        std::fs::read_to_string(dst.path().join("big.bin")).unwrap(),
        "the old version"
    );
}
