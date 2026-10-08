//! Group by: which group a file falls in when the details view is grouped
//! by type, date modified, size or name, and the group headers' labels.
//!
//! Groups are sections of the sort model (GTK ≥ 4.12): a *section sorter*
//! puts the items of a group together (in group order), and the column
//! sorter orders the items inside each group. Like the column sorters, the
//! section sorter is key-based (see `sorting`): each item's group is
//! computed once per sort.

use gettextrs::gettext;
use gtk::{glib, prelude::*};

use super::FileItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBy {
    #[default]
    None,
    Type,
    Modified,
    Size,
    Name,
}

impl GroupBy {
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Name,
        Self::Modified,
        Self::Type,
        Self::Size,
    ];

    pub fn nick(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Type => "type",
            Self::Modified => "modified",
            Self::Size => "size",
            Self::Name => "name",
        }
    }

    pub fn from_nick(nick: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|g| g.nick() == nick)
            .unwrap_or_default()
    }
}

// ── Date modified ──────────────────────────────────────────────────────────

/// Start times (UNIX) of the periods dates are grouped in.
#[derive(Debug, Clone, Copy)]
pub struct Periods {
    pub today: i64,
    pub yesterday: i64,
    /// Monday of this week.
    pub week: i64,
    pub last_week: i64,
    pub month: i64,
    pub last_month: i64,
    pub year: i64,
}

impl Periods {
    /// The periods around `now`, in local time.
    pub fn at(now: &glib::DateTime) -> Self {
        let midnight = |date: &glib::DateTime| {
            glib::DateTime::from_local(date.year(), date.month(), date.day_of_month(), 0, 0, 0.0)
                .map(|d| d.to_unix())
                .unwrap_or(0)
        };
        let today =
            glib::DateTime::from_local(now.year(), now.month(), now.day_of_month(), 0, 0, 0.0)
                .unwrap_or_else(|_| now.clone());
        let days_ago = |days: i32| today.add_days(-days).map(|d| midnight(&d)).unwrap_or(0);
        let week_day = today.day_of_week() - 1;
        let first_of = |year: i32, month: i32| {
            glib::DateTime::from_local(year, month, 1, 0, 0, 0.0)
                .map(|d| d.to_unix())
                .unwrap_or(0)
        };
        let (last_month_year, last_month) = if today.month() == 1 {
            (today.year() - 1, 12)
        } else {
            (today.year(), today.month() - 1)
        };
        Self {
            today: today.to_unix(),
            yesterday: days_ago(1),
            week: days_ago(week_day),
            last_week: days_ago(week_day + 7),
            month: first_of(today.year(), today.month()),
            last_month: first_of(last_month_year, last_month),
            year: first_of(today.year(), 1),
        }
    }

    pub fn now() -> Self {
        Self::at(
            &glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().unwrap()),
        )
    }
}

/// The date group of a modification time: 0 = Today … 7 = A long time
/// ago, 8 = unknown.
pub fn date_group(modified: i64, periods: &Periods) -> u32 {
    if modified <= 0 {
        return 8;
    }
    [
        periods.today,
        periods.yesterday,
        periods.week,
        periods.last_week,
        periods.month,
        periods.last_month,
        periods.year,
    ]
    .iter()
    .position(|&start| modified >= start)
    .map_or(7, |i| i as u32)
}

fn date_label(group: u32) -> String {
    match group {
        0 => gettext("Today"),
        1 => gettext("Yesterday"),
        2 => gettext("Earlier this week"),
        3 => gettext("Last week"),
        4 => gettext("Earlier this month"),
        5 => gettext("Last month"),
        6 => gettext("Earlier this year"),
        7 => gettext("A long time ago"),
        _ => gettext("Unknown date"),
    }
}

// ── Size ───────────────────────────────────────────────────────────────────

const KB: u64 = 1000;
const MB: u64 = 1000 * KB;
const GB: u64 = 1000 * MB;

/// The size group: 0 = folders, 1 = empty, 2 = tiny … 7 = gigantic.
pub fn size_group(size: u64, is_dir: bool) -> u32 {
    if is_dir {
        return 0;
    }
    match size {
        0 => 1,
        s if s < 16 * KB => 2,
        s if s < MB => 3,
        s if s < 128 * MB => 4,
        s if s < GB => 5,
        s if s < 4 * GB => 6,
        _ => 7,
    }
}

fn size_label(group: u32) -> String {
    match group {
        0 => gettext("Folders"),
        1 => gettext("Empty"),
        2 => gettext("Tiny (up to 16 kB)"),
        3 => gettext("Small (16 kB – 1 MB)"),
        4 => gettext("Medium (1 – 128 MB)"),
        5 => gettext("Large (128 MB – 1 GB)"),
        6 => gettext("Huge (1 – 4 GB)"),
        _ => gettext("Gigantic (over 4 GB)"),
    }
}

// ── Name ───────────────────────────────────────────────────────────────────

/// The name group: 0 = digits, 1 = A–H, 2 = I–P, 3 = Q–Z, 4 = other.
/// Accents are ignored ("École" is in A–H).
pub fn name_group(name: &str) -> u32 {
    let base = glib::normalize(name.trim_start_matches('.'), glib::NormalizeMode::Default);
    let Some(first) = base.chars().next() else {
        return 4;
    };
    match first.to_ascii_uppercase() {
        '0'..='9' => 0,
        'A'..='H' => 1,
        'I'..='P' => 2,
        'Q'..='Z' => 3,
        _ => 4,
    }
}

fn name_label(group: u32) -> String {
    match group {
        0 => gettext("0 – 9"),
        1 => gettext("A – H"),
        2 => gettext("I – P"),
        3 => gettext("Q – Z"),
        _ => gettext("Other"),
    }
}

// ── GTK side ───────────────────────────────────────────────────────────────

fn item_expression<T: glib::value::ValueType + Into<glib::Value>>(
    f: impl Fn(&FileItem) -> T + 'static,
) -> gtk::ClosureExpression {
    gtk::ClosureExpression::new::<T>(
        &[] as &[gtk::Expression],
        glib::closure_local!(move |item: FileItem| f(&item)),
    )
}

/// The section sorter putting items in groups, or `None` without grouping.
/// Headers must use the same `periods` (see [`group_label`]).
pub fn section_sorter(group: GroupBy, periods: Periods) -> Option<gtk::Sorter> {
    let numeric = |f: fn(&FileItem, &Periods) -> u32| {
        gtk::NumericSorter::new(Some(item_expression(move |item| f(item, &periods)))).upcast()
    };
    Some(match group {
        GroupBy::None => return None,
        GroupBy::Modified => numeric(|item, periods| date_group(item.modified(), periods)),
        GroupBy::Size => numeric(|item, _| size_group(item.size(), item.is_dir())),
        GroupBy::Name => numeric(|item, _| name_group(&item.display_name())),
        GroupBy::Type => {
            // Folders first, then by type.
            let sorter = gtk::MultiSorter::new();
            sorter.append(super::sorting::folders_first_sorter(true));
            sorter.append(
                gtk::StringSorter::builder()
                    .expression(item_expression(|item| item.type_description()))
                    .collation(gtk::Collation::Unicode)
                    .ignore_case(true)
                    .build(),
            );
            sorter.upcast()
        }
    })
}

/// The header of the group `item` is in.
pub fn group_label(group: GroupBy, item: &FileItem, periods: &Periods) -> String {
    match group {
        GroupBy::None => String::new(),
        GroupBy::Type if item.is_dir() => gettext("Folders"),
        GroupBy::Type => {
            let description = item.type_description();
            if description.is_empty() {
                gettext("Unknown type")
            } else {
                description
            }
        }
        GroupBy::Modified => date_label(date_group(item.modified(), periods)),
        GroupBy::Size => size_label(size_group(item.size(), item.is_dir())),
        GroupBy::Name => name_label(name_group(&item.display_name())),
    }
}
