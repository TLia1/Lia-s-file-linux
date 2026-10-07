//! Human-friendly formatting of sizes, dates and counts.

use gettextrs::{gettext, ngettext};
use gtk::glib;

/// "1.2 MB", "640 kB", "12 bytes" (SI units, like the rest of GNOME).
pub fn size(bytes: u64) -> String {
    glib::format_size(bytes).into()
}

/// "1 item", "5 items".
pub fn item_count(n: u32) -> String {
    ngettext("{} item", "{} items", n).replace("{}", &n.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    Older,
}

/// Classifies `date` relative to `now` by calendar day (in local time).
pub fn day_of(date: &glib::DateTime, now: &glib::DateTime) -> Day {
    let ymd = |d: &glib::DateTime| (d.year(), d.month(), d.day_of_month());
    if ymd(date) == ymd(now) {
        return Day::Today;
    }
    match now.add_days(-1) {
        Ok(yesterday) if ymd(date) == ymd(&yesterday) => Day::Yesterday,
        _ => Day::Older,
    }
}

/// "Today 14:02", "Yesterday 09:13" or the locale's date, e.g. "03/10/2026".
pub fn date_relative_to(date: &glib::DateTime, now: &glib::DateTime) -> String {
    let date = date.to_local().unwrap_or_else(|_| date.clone());
    let fmt = |f: &str| date.format(f).map(String::from).unwrap_or_default();
    match day_of(&date, now) {
        // Translators: "Today 14:02"
        Day::Today => gettext("Today {}").replace("{}", &fmt("%H:%M")),
        // Translators: "Yesterday 14:02"
        Day::Yesterday => gettext("Yesterday {}").replace("{}", &fmt("%H:%M")),
        Day::Older => fmt("%x"),
    }
}

/// Formats a UNIX timestamp for the "Date modified" column.
pub fn date(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let (Ok(date), Ok(now)) = (
        glib::DateTime::from_unix_local(unix),
        glib::DateTime::now_local(),
    ) else {
        return String::new();
    };
    date_relative_to(&date, &now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, mo: i32, d: i32, h: i32, mi: i32) -> glib::DateTime {
        glib::DateTime::from_local(y, mo, d, h, mi, 0.0).unwrap()
    }

    #[test]
    fn classifies_days() {
        let now = local(2026, 10, 7, 15, 0);
        assert_eq!(day_of(&local(2026, 10, 7, 0, 1), &now), Day::Today);
        assert_eq!(day_of(&local(2026, 10, 6, 23, 59), &now), Day::Yesterday);
        assert_eq!(day_of(&local(2026, 10, 6, 0, 0), &now), Day::Yesterday);
        assert_eq!(day_of(&local(2026, 10, 5, 23, 59), &now), Day::Older);
        assert_eq!(day_of(&local(2025, 10, 7, 15, 0), &now), Day::Older);
    }

    #[test]
    fn yesterday_across_month_boundary() {
        let now = local(2026, 3, 1, 8, 0);
        assert_eq!(day_of(&local(2026, 2, 28, 22, 0), &now), Day::Yesterday);
    }

    #[test]
    fn relative_dates_include_time() {
        let now = local(2026, 10, 7, 15, 0);
        assert_eq!(
            date_relative_to(&local(2026, 10, 7, 14, 2), &now),
            "Today 14:02"
        );
        assert_eq!(
            date_relative_to(&local(2026, 10, 6, 9, 13), &now),
            "Yesterday 09:13"
        );
    }

    #[test]
    fn formats_sizes_and_counts() {
        assert_eq!(size(1_200_000), "1.2\u{a0}MB");
        assert_eq!(item_count(1), "1 item");
        assert_eq!(item_count(5), "5 items");
    }
}
