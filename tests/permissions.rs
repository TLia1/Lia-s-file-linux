//! Permission bits as edited in the Properties dialog.

use lia_files::model::permissions::*;

#[test]
fn reads_and_toggles_bits() {
    let mode = 0o640;
    assert!(has(mode, Class::Owner, Access::Read));
    assert!(has(mode, Class::Owner, Access::Write));
    assert!(!has(mode, Class::Owner, Access::Execute));
    assert!(has(mode, Class::Group, Access::Read));
    assert!(!has(mode, Class::Others, Access::Read));

    assert_eq!(with(mode, Class::Others, Access::Read, true), 0o644);
    assert_eq!(with(mode, Class::Owner, Access::Write, false), 0o440);
    assert_eq!(with(mode, Class::Group, Access::Execute, true), 0o650);
}

#[test]
fn keeps_only_permission_bits() {
    // A regular file's unix::mode includes the file type (S_IFREG).
    let mode = 0o100_644;
    assert_eq!(with(mode, Class::Owner, Access::Execute, true), 0o744);
    assert_eq!(set_executable(mode, false), 0o644);
}

#[test]
fn executable_follows_read_bits() {
    assert!(!is_executable(0o644));
    assert_eq!(set_executable(0o644, true), 0o755);
    assert_eq!(set_executable(0o600, true), 0o700);
    // The owner can always execute once allowed.
    assert_eq!(set_executable(0o044, true), 0o155);
    assert!(is_executable(0o755));
    assert_eq!(set_executable(0o755, false), 0o644);
    // Special bits survive.
    assert_eq!(set_executable(0o4755, false), 0o4644);
}

#[test]
fn formats_like_ls_and_chmod() {
    assert_eq!(symbolic(0o755), "rwxr-xr-x");
    assert_eq!(symbolic(0o640), "rw-r-----");
    assert_eq!(symbolic(0o4755), "rwsr-xr-x");
    assert_eq!(symbolic(0o2745), "rwxr-Sr-x");
    assert_eq!(symbolic(0o1777), "rwxrwxrwt");
    assert_eq!(symbolic(0o1776), "rwxrwxrwT");
    assert_eq!(octal(0o100_644), "644");
    assert_eq!(octal(0o750), "750");
    assert_eq!(octal(0o1777), "1777");
}
