//! `DirectoryModel` must load a folder and follow external changes
//! (create, delete, rename) through its FileMonitor.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use gtk::{gio, glib, prelude::*};
use lia_files::model::{DirectoryModel, FileItem};

fn names(model: &DirectoryModel) -> BTreeSet<String> {
    (0..model.n_items())
        .filter_map(|i| model.item(i).and_downcast::<FileItem>())
        .map(|item| item.name().to_string_lossy().into_owned())
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// Iterates `context` until `condition` holds, failing after 5 seconds.
fn wait_until(context: &glib::MainContext, what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        if !context.iteration(false) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn loads_and_follows_changes() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path();
            std::fs::write(path.join("a.txt"), "a").unwrap();
            std::fs::write(path.join("b.txt"), "b").unwrap();
            std::fs::create_dir(path.join("sub")).unwrap();

            let model = DirectoryModel::new();
            model.load(&gio::File::for_path(path));
            assert!(model.loading());
            wait_until(&context, "initial load", || !model.loading());
            assert!(model.error().is_none());
            assert_eq!(names(&model), set(&["a.txt", "b.txt", "sub"]));

            let sub = model
                .find(&gio::File::for_path(path.join("sub")))
                .expect("sub is indexed");
            assert!(sub.is_dir());

            std::fs::write(path.join("c.txt"), "c").unwrap();
            wait_until(&context, "create", || names(&model).contains("c.txt"));

            std::fs::remove_file(path.join("a.txt")).unwrap();
            wait_until(&context, "delete", || !names(&model).contains("a.txt"));

            std::fs::rename(path.join("b.txt"), path.join("d.txt")).unwrap();
            wait_until(&context, "rename", || {
                let n = names(&model);
                n.contains("d.txt") && !n.contains("b.txt")
            });
            assert_eq!(names(&model), set(&["c.txt", "d.txt", "sub"]));

            std::fs::write(path.join("c.txt"), "longer content").unwrap();
            wait_until(&context, "change", || {
                model
                    .find(&gio::File::for_path(path.join("c.txt")))
                    .is_some_and(|item| item.size() == 14)
            });
        })
        .unwrap();
}

#[test]
fn reports_errors() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let dir = tempfile::tempdir().unwrap();
            let model = DirectoryModel::new();
            model.load(&gio::File::for_path(dir.path().join("missing")));
            wait_until(&context, "load failure", || !model.loading());
            let err = model.error().expect("an error is reported");
            assert!(err.matches(gio::IOErrorEnum::NotFound));
            assert_eq!(model.n_items(), 0);
        })
        .unwrap();
}

#[test]
fn streams_large_folders() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let dir = tempfile::tempdir().unwrap();
            for i in 0..2_000 {
                std::fs::write(dir.path().join(format!("file{i}")), "").unwrap();
            }
            let model = DirectoryModel::new();
            let batches = std::rc::Rc::new(std::cell::Cell::new(0));
            let counter = batches.clone();
            model.connect_items_changed(move |_, _, _, added| {
                if added > 0 {
                    counter.set(counter.get() + 1);
                }
            });
            model.load(&gio::File::for_path(dir.path()));
            wait_until(&context, "load", || !model.loading());
            assert_eq!(model.n_items(), 2_000);
            assert!(batches.get() > 1, "items should arrive in several batches");
        })
        .unwrap();
}
