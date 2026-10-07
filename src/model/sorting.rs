//! Sorters for file items.
//!
//! Only GTK's built-in `StringSorter` / `NumericSorter` are used, on
//! `FileItem` properties. Unlike a `CustomSorter`, they provide *sort keys*:
//! each item's key is computed once and the sort model compares raw keys,
//! which made loading 100k files several times faster than comparing through
//! Rust closures.
//!
//! Names use GTK's filename collation (the same as
//! `g_utf8_collate_key_for_filename`): locale-aware, case-insensitive and
//! natural, so "file2" sorts before "file10".

use gtk::prelude::*;

use super::FileItem;

fn property(name: &str) -> gtk::PropertyExpression {
    gtk::PropertyExpression::new(FileItem::static_type(), None::<&gtk::Expression>, name)
}

/// Natural, case-insensitive, locale-aware name order.
pub fn name_sorter() -> gtk::StringSorter {
    gtk::StringSorter::builder()
        .expression(property("display-name"))
        .collation(gtk::Collation::Filename)
        .ignore_case(true)
        .build()
}

/// `primary`, then by name for equal values.
fn then_by_name(primary: impl IsA<gtk::Sorter>) -> gtk::MultiSorter {
    let sorter = gtk::MultiSorter::new();
    sorter.append(primary);
    sorter.append(name_sorter());
    sorter
}

pub fn modified_sorter() -> gtk::MultiSorter {
    then_by_name(gtk::NumericSorter::new(Some(property("modified"))))
}

/// Folders have size 0 (see `FileItem`), so they stay grouped by name.
pub fn size_sorter() -> gtk::MultiSorter {
    then_by_name(gtk::NumericSorter::new(Some(property("size"))))
}

pub fn type_sorter() -> gtk::MultiSorter {
    then_by_name(
        gtk::StringSorter::builder()
            .expression(property("type-description"))
            .collation(gtk::Collation::Unicode)
            .ignore_case(true)
            .build(),
    )
}

/// Puts folders before files while enabled (see [`set_folders_first`]).
pub fn folders_first_sorter(enabled: bool) -> gtk::NumericSorter {
    let sorter = gtk::NumericSorter::new(None::<gtk::Expression>);
    sorter.set_sort_order(gtk::SortType::Descending);
    set_folders_first(&sorter, enabled);
    sorter
}

/// Without an expression a `NumericSorter` considers all items equal.
pub fn set_folders_first(sorter: &gtk::NumericSorter, enabled: bool) {
    sorter.set_expression(enabled.then(|| property("is-dir")));
}
