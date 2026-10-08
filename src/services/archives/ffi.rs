//! The part of libarchive's C API used here (see `archive.h` and
//! `archive_entry.h`). Kept minimal; [`super::libarchive`] wraps it safely.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_long, c_void};

#[repr(C)]
pub struct archive {
    _private: [u8; 0],
}

#[repr(C)]
pub struct archive_entry {
    _private: [u8; 0],
}

pub type la_int64_t = i64;
pub type la_ssize_t = isize;
pub type mode_t = u32;
pub type time_t = i64;

pub const ARCHIVE_EOF: c_int = 1;
pub const ARCHIVE_OK: c_int = 0;
pub const ARCHIVE_WARN: c_int = -20;
pub const ARCHIVE_FAILED: c_int = -25;

pub const ARCHIVE_FORMAT_BASE_MASK: c_int = 0xff0000;
pub const ARCHIVE_FORMAT_RAW: c_int = 0x90000;

pub const ARCHIVE_EXTRACT_PERM: c_int = 0x0002;
pub const ARCHIVE_EXTRACT_TIME: c_int = 0x0004;
pub const ARCHIVE_EXTRACT_NO_OVERWRITE: c_int = 0x0008;
pub const ARCHIVE_EXTRACT_SECURE_SYMLINKS: c_int = 0x0100;
pub const ARCHIVE_EXTRACT_SECURE_NODOTDOT: c_int = 0x0200;

pub const AE_IFMT: mode_t = 0o170000;
pub const AE_IFREG: mode_t = 0o100000;
pub const AE_IFLNK: mode_t = 0o120000;
pub const AE_IFDIR: mode_t = 0o040000;

#[link(name = "archive")]
unsafe extern "C" {
    // Errors
    pub fn archive_error_string(a: *mut archive) -> *const c_char;
    pub fn archive_format(a: *mut archive) -> c_int;
    pub fn archive_filter_bytes(a: *mut archive, n: c_int) -> la_int64_t;

    // Reading
    pub fn archive_read_new() -> *mut archive;
    pub fn archive_read_support_filter_all(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_all(a: *mut archive) -> c_int;
    pub fn archive_read_support_format_raw(a: *mut archive) -> c_int;
    pub fn archive_read_open_filename(
        a: *mut archive,
        filename: *const c_char,
        block_size: usize,
    ) -> c_int;
    pub fn archive_read_next_header(a: *mut archive, entry: *mut *mut archive_entry) -> c_int;
    pub fn archive_read_data_block(
        a: *mut archive,
        buff: *mut *const c_void,
        size: *mut usize,
        offset: *mut la_int64_t,
    ) -> c_int;
    pub fn archive_read_free(a: *mut archive) -> c_int;

    // Writing to disk
    pub fn archive_write_disk_new() -> *mut archive;
    pub fn archive_write_disk_set_options(a: *mut archive, flags: c_int) -> c_int;
    pub fn archive_write_data_block(
        a: *mut archive,
        buff: *const c_void,
        size: usize,
        offset: la_int64_t,
    ) -> la_ssize_t;

    // Writing archives
    pub fn archive_write_new() -> *mut archive;
    pub fn archive_write_set_format_zip(a: *mut archive) -> c_int;
    pub fn archive_write_set_format_7zip(a: *mut archive) -> c_int;
    pub fn archive_write_set_format_pax_restricted(a: *mut archive) -> c_int;
    pub fn archive_write_add_filter_gzip(a: *mut archive) -> c_int;
    pub fn archive_write_add_filter_xz(a: *mut archive) -> c_int;
    pub fn archive_write_open_filename(a: *mut archive, filename: *const c_char) -> c_int;
    pub fn archive_write_header(a: *mut archive, entry: *mut archive_entry) -> c_int;
    pub fn archive_write_data(a: *mut archive, buff: *const c_void, size: usize) -> la_ssize_t;
    pub fn archive_write_finish_entry(a: *mut archive) -> c_int;
    pub fn archive_write_close(a: *mut archive) -> c_int;
    pub fn archive_write_free(a: *mut archive) -> c_int;

    // Entries
    pub fn archive_entry_new() -> *mut archive_entry;
    pub fn archive_entry_free(entry: *mut archive_entry);
    pub fn archive_entry_pathname(entry: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_hardlink(entry: *mut archive_entry) -> *const c_char;
    pub fn archive_entry_filetype(entry: *mut archive_entry) -> mode_t;
    pub fn archive_entry_size(entry: *mut archive_entry) -> la_int64_t;
    pub fn archive_entry_is_encrypted(entry: *mut archive_entry) -> c_int;
    pub fn archive_entry_copy_pathname(entry: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_copy_hardlink(entry: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_copy_symlink(entry: *mut archive_entry, name: *const c_char);
    pub fn archive_entry_set_filetype(entry: *mut archive_entry, filetype: mode_t);
    pub fn archive_entry_set_perm(entry: *mut archive_entry, perm: mode_t);
    pub fn archive_entry_set_size(entry: *mut archive_entry, size: la_int64_t);
    pub fn archive_entry_set_mtime(entry: *mut archive_entry, sec: time_t, nsec: c_long);
}
