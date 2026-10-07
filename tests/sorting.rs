//! The sorters used by the views: natural, case-insensitive names, folders
//! first, and the per-column sorters. GTK sorters need GTK to be initialized,
//! so this test is skipped when no display is available.

use gtk::{gio, glib, prelude::*};
use lia_files::model::{FileItem, directory_model::ATTRIBUTES, sorting};

fn item(dir: &std::path::Path, name: &str) -> FileItem {
    let file = gio::File::for_path(dir.join(name));
    let info = file
        .query_info(
            ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .unwrap();
    FileItem::new(file, info)
}

fn sorted_names(items: &[FileItem], sorter: &impl IsA<gtk::Sorter>) -> Vec<String> {
    let store = gio::ListStore::new::<FileItem>();
    store.extend_from_slice(items);
    let model = gtk::SortListModel::new(Some(store), Some(sorter.clone()));
    (0..model.n_items())
        .map(|i| {
            model
                .item(i)
                .and_downcast::<FileItem>()
                .unwrap()
                .display_name()
                .to_string()
        })
        .collect()
}

#[test]
fn sorters() {
    if gtk::init().is_err() {
        eprintln!("no display available, skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    for (name, size) in [
        ("file10.txt", 30),
        ("file2.txt", 10),
        ("File1.txt", 20),
        ("banana", 5),
        ("Apple", 1),
    ] {
        std::fs::write(path.join(name), vec![b'x'; size]).unwrap();
    }
    std::fs::create_dir(path.join("zeta")).unwrap();
    std::fs::create_dir(path.join("Alpha")).unwrap();
    // Distinct modification times: file2 oldest, banana newest.
    for (i, name) in ["file2.txt", "Apple", "file10.txt", "File1.txt", "banana"]
        .iter()
        .enumerate()
    {
        let time = glib::DateTime::from_unix_utc(1_700_000_000 + i as i64 * 60).unwrap();
        let info = gio::FileInfo::new();
        info.set_modification_date_time(&time);
        gio::File::for_path(path.join(name))
            .set_attributes_from_info(&info, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            .unwrap();
    }

    let items: Vec<FileItem> = [
        "file10.txt",
        "zeta",
        "file2.txt",
        "File1.txt",
        "banana",
        "Alpha",
        "Apple",
    ]
    .iter()
    .map(|name| item(path, name))
    .collect();

    // Natural and case-insensitive.
    assert_eq!(
        sorted_names(&items, &sorting::name_sorter()),
        [
            "Alpha",
            "Apple",
            "banana",
            "File1.txt",
            "file2.txt",
            "file10.txt",
            "zeta"
        ]
    );

    // Folders first, then by name; and switching it off.
    let folders_first = sorting::folders_first_sorter(true);
    let combined = gtk::MultiSorter::new();
    combined.append(folders_first.clone());
    combined.append(sorting::name_sorter());
    assert_eq!(
        sorted_names(&items, &combined),
        [
            "Alpha",
            "zeta",
            "Apple",
            "banana",
            "File1.txt",
            "file2.txt",
            "file10.txt"
        ]
    );
    sorting::set_folders_first(&folders_first, false);
    assert_eq!(
        sorted_names(&items, &combined),
        [
            "Alpha",
            "Apple",
            "banana",
            "File1.txt",
            "file2.txt",
            "file10.txt",
            "zeta"
        ]
    );

    // Size: folders count as 0 and keep name order among themselves.
    assert_eq!(
        sorted_names(&items, &sorting::size_sorter()),
        [
            "Alpha",
            "zeta",
            "Apple",
            "banana",
            "file2.txt",
            "File1.txt",
            "file10.txt"
        ]
    );

    // Modification time, oldest first (folders were created just now).
    let by_date = sorted_names(&items, &sorting::modified_sorter());
    assert_eq!(
        by_date[..5],
        ["file2.txt", "Apple", "file10.txt", "File1.txt", "banana"]
    );
}
