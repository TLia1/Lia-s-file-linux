//! Folder sizes for the Properties dialog, measured on real temporary trees.

use std::os::unix::fs::symlink;
use std::path::Path;

use gtk::{gio, glib, prelude::*};
use lia_files::services::disk_usage::{Measurement, Totals, measure};

fn write(path: impl AsRef<Path>, bytes: usize) {
    let path = path.as_ref();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

fn file(path: impl AsRef<Path>) -> gio::File {
    gio::File::for_path(path)
}

fn run(files: &[gio::File]) -> Totals {
    measure(files, &gio::Cancellable::new(), |_| {})
}

#[test]
fn counts_a_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root.join("a.txt"), 100);
    write(root.join("sub/b.txt"), 2_000);
    write(root.join("sub/deeper/c.txt"), 30);
    std::fs::create_dir(root.join("empty")).unwrap();

    let totals = run(&[file(root)]);
    assert_eq!(
        totals,
        Totals {
            size: 2_130,
            files: 3,
            folders: 3,
            unreadable: 0,
        }
    );
}

#[test]
fn counts_selected_files_and_folders_together() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root.join("one.bin"), 10);
    write(root.join("folder/two.bin"), 20);

    let totals = run(&[file(root.join("one.bin")), file(root.join("folder"))]);
    assert_eq!((totals.size, totals.files, totals.folders), (30, 2, 0));
}

#[test]
fn links_are_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root.join("big/data.bin"), 5_000);
    std::fs::create_dir(root.join("view")).unwrap();
    symlink(root.join("big"), root.join("view/link")).unwrap();

    let totals = run(&[file(root.join("view"))]);
    assert_eq!(totals.files, 1, "the link counts as one file");
    assert_eq!(totals.folders, 0);
    assert!(totals.size < 5_000, "the link's target isn't counted");
}

#[test]
fn reports_progress_and_missing_items() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for i in 0..5 {
        write(root.join(format!("d{i}/f")), 1);
    }
    let mut calls = 0;
    let totals = measure(
        &[file(root), file(root.join("missing"))],
        &gio::Cancellable::new(),
        |_| calls += 1,
    );
    assert_eq!(calls, 6, "once per folder read");
    assert_eq!((totals.files, totals.folders, totals.unreadable), (5, 5, 1));
}

#[test]
fn stops_when_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path().join("a/b/c"), 1);
    let cancellable = gio::Cancellable::new();
    cancellable.cancel();
    let totals = measure(&[file(dir.path())], &cancellable, |_| {});
    assert_eq!(totals.files, 0);
}

#[test]
fn streams_final_totals() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path().join("x/y.bin"), 42);
    let measurement = Measurement::start(vec![file(dir.path())]);
    let context = glib::MainContext::default();
    let last = context.block_on(async {
        let mut last = None;
        while let Some(totals) = measurement.next().await {
            last = Some(totals);
        }
        last
    });
    assert_eq!(last.map(|t| (t.size, t.files, t.folders)), Some((42, 1, 1)));
}
