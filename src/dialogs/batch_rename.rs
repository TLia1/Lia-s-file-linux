//! The batch rename dialog: renames the selected items with a pattern
//! (`{name}_{n:03}`), a find & replace, or a case change, with a live
//! preview of the new names and of the problems (duplicates, names taken,
//! invalid characters). Applying it runs one `BatchRename` operation,
//! undone in one step.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{glib, glib::clone};

use crate::model::FileItem;
use crate::services::batch_rename::{self, CaseChange, Problem, Rule, Source};
use crate::services::file_ops::Operation;
use crate::widgets::folder_pane::FolderPane;

/// Rows shown in the preview; problems further down are still counted.
const PREVIEW_ROWS: usize = 200;
const DEFAULT_PATTERN: &str = "{name}_{n:03}";

/// One preview row: the old name and what it becomes.
struct PreviewRow {
    new_label: gtk::Label,
    icon: gtk::Image,
    row: gtk::ListBoxRow,
}

struct Widgets {
    mode: adw::ToggleGroup,
    pattern: adw::EntryRow,
    start: adw::SpinRow,
    find: adw::EntryRow,
    replace: adw::EntryRow,
    match_case: adw::SwitchRow,
    case: adw::ComboRow,
    preview_group: adw::PreferencesGroup,
    rows: Vec<PreviewRow>,
    rename_button: gtk::Button,
}

impl Widgets {
    fn rule(&self) -> Rule {
        match self.mode.active_name().as_deref() {
            Some("replace") => Rule::Replace {
                find: self.find.text().into(),
                replace: self.replace.text().into(),
                match_case: self.match_case.is_active(),
            },
            Some("case") => Rule::Case(
                CaseChange::ALL
                    .get(self.case.selected() as usize)
                    .copied()
                    .unwrap_or_default(),
            ),
            _ => Rule::Pattern {
                pattern: self.pattern.text().into(),
                start: self.start.value().max(0.0) as u32,
            },
        }
    }
}

/// Opens the dialog for `items`. `existing` lists the other names in their
/// folders (see `batch_rename::check`); `portable`: the folder needs
/// Windows-compatible names.
pub fn present(
    pane: &FolderPane,
    items: Vec<FileItem>,
    existing: HashSet<(String, String)>,
    portable: bool,
) {
    let sources: Vec<Source> = items
        .iter()
        .map(|item| Source {
            name: item.display_name().into(),
            is_dir: item.is_dir(),
            date: glib::DateTime::from_unix_local(item.modified())
                .and_then(|d| d.format("%Y-%m-%d"))
                .map(String::from)
                .unwrap_or_default(),
            folder: item
                .file()
                .parent()
                .map(|p| p.uri().to_string())
                .unwrap_or_default(),
        })
        .collect();

    // ── Rules ──────────────────────────────────────────────────────────────
    let mode = adw::ToggleGroup::builder()
        .homogeneous(true)
        .css_classes(["round"])
        .build();
    for (name, label) in [
        ("pattern", gettext("Pattern")),
        ("replace", gettext("Find & Replace")),
        ("case", gettext("Change Case")),
    ] {
        mode.add(adw::Toggle::builder().name(name).label(label).build());
    }
    mode.set_active_name(Some("pattern"));

    let pattern = adw::EntryRow::builder()
        .title(gettext("New Name"))
        .text(DEFAULT_PATTERN)
        .build();
    let start = adw::SpinRow::with_range(0.0, 1_000_000.0, 1.0);
    start.set_title(&gettext("Start Numbering At"));
    start.set_value(1.0);
    let pattern_group = adw::PreferencesGroup::builder()
        .description(gettext(
            "{name} is the current name, {n} a number ({n:03} gives 001, 002…), \
             {date} the date modified. The extension is kept.",
        ))
        .build();
    pattern_group.add(&pattern);
    pattern_group.add(&start);

    let find = adw::EntryRow::builder().title(gettext("Find")).build();
    let replace = adw::EntryRow::builder()
        .title(gettext("Replace With"))
        .build();
    let match_case = adw::SwitchRow::builder()
        .title(gettext("Match Case"))
        .build();
    let replace_group = adw::PreferencesGroup::builder()
        .description(gettext(
            "Replaces every occurrence in the names. The extension is kept.",
        ))
        .build();
    replace_group.add(&find);
    replace_group.add(&replace);
    replace_group.add(&match_case);

    let labels: Vec<String> = CaseChange::ALL.iter().map(|c| c.label()).collect();
    let case = adw::ComboRow::builder()
        .title(gettext("Change To"))
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .build();
    let case_group = adw::PreferencesGroup::builder()
        .description(gettext("The extension is kept."))
        .build();
    case_group.add(&case);

    let rules = gtk::Stack::builder()
        .vhomogeneous(false)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    rules.add_named(&pattern_group, Some("pattern"));
    rules.add_named(&replace_group, Some("replace"));
    rules.add_named(&case_group, Some("case"));

    let mode_group = adw::PreferencesGroup::new();
    let mode_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    mode_box.append(&mode);
    mode_box.append(&rules);
    mode_group.add(&mode_box);

    // ── Preview ────────────────────────────────────────────────────────────
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "rename-preview"])
        .build();
    let mut rows = Vec::new();
    for source in sources.iter().take(PREVIEW_ROWS) {
        let old_label = gtk::Label::builder()
            .label(&source.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .width_chars(8)
            .css_classes(["secondary"])
            .build();
        let arrow = gtk::Image::from_icon_name("go-next-symbolic");
        arrow.add_css_class("dimmed");
        let new_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .width_chars(8)
            .build();
        let icon = gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .visible(false)
            .css_classes(["error"])
            .build();
        // Two equal columns, so that the arrows line up.
        let new_side = gtk::Box::builder().spacing(10).build();
        new_side.append(&arrow);
        new_side.append(&new_label);
        new_side.append(&icon);
        let content = gtk::Box::builder()
            .spacing(10)
            .homogeneous(true)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&old_label);
        content.append(&new_side);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(false)
            .build();
        list.append(&row);
        rows.push(PreviewRow {
            new_label,
            icon,
            row,
        });
    }
    if sources.len() > PREVIEW_ROWS {
        let more = sources.len() - PREVIEW_ROWS;
        let label = gtk::Label::builder()
            .label(
                ngettext("And {} more item", "And {} more items", more as u32)
                    .replace("{}", &more.to_string()),
            )
            .margin_top(8)
            .margin_bottom(8)
            .css_classes(["dimmed"])
            .build();
        list.append(
            &gtk::ListBoxRow::builder()
                .child(&label)
                .activatable(false)
                .build(),
        );
    }
    let preview_group = adw::PreferencesGroup::builder()
        .title(gettext("Preview"))
        .build();
    preview_group.add(&list);

    let page = adw::PreferencesPage::new();
    page.add(&mode_group);
    page.add(&preview_group);

    // ── Dialog ─────────────────────────────────────────────────────────────
    let cancel_button = gtk::Button::with_mnemonic(&gettext("_Cancel"));
    let rename_button = gtk::Button::builder()
        .label(gettext("_Rename"))
        .use_underline(true)
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel_button);
    header.pack_end(&rename_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let count = sources.len() as u32;
    let dialog = adw::Dialog::builder()
        .title(
            ngettext("Rename {} Item", "Rename {} Items", count).replace("{}", &count.to_string()),
        )
        .content_width(560)
        .content_height(640)
        .child(&toolbar)
        .build();

    let widgets = Rc::new(Widgets {
        mode,
        pattern,
        start,
        find,
        replace,
        match_case,
        case,
        preview_group,
        rows,
        rename_button,
    });
    let sources = Rc::new(sources);
    // The names computed last, applied by Rename.
    let names: Rc<RefCell<Vec<String>>> = Rc::default();

    let update = Rc::new(clone!(
        #[strong]
        widgets,
        #[strong]
        sources,
        #[strong]
        names,
        #[weak]
        rules,
        move || {
            if let Some(name) = widgets.mode.active_name() {
                rules.set_visible_child_name(&name);
            }
            let new_names = batch_rename::new_names(&widgets.rule(), &sources);
            let problems = batch_rename::check(&sources, &new_names, &existing, portable);
            for (((row, source), name), problem) in widgets
                .rows
                .iter()
                .zip(sources.iter())
                .zip(&new_names)
                .zip(&problems)
            {
                update_row(row, name, name == &source.name, problem.as_ref());
            }
            let changed = sources
                .iter()
                .zip(&new_names)
                .filter(|(source, name)| source.name != **name)
                .count();
            let n_problems = problems.iter().flatten().count();
            let summary = if n_problems > 0 {
                ngettext(
                    "{} name can't be used",
                    "{} names can't be used",
                    n_problems as u32,
                )
                .replace("{}", &n_problems.to_string())
            } else if changed == 0 {
                gettext("No names change")
            } else {
                ngettext("{} name changes", "{} names change", changed as u32)
                    .replace("{}", &changed.to_string())
            };
            widgets.preview_group.set_description(Some(&summary));
            widgets
                .rename_button
                .set_sensitive(changed > 0 && n_problems == 0);
            names.replace(new_names);
        }
    ));

    widgets.mode.connect_active_name_notify(clone!(
        #[strong]
        update,
        move |_| update()
    ));
    for entry in [&widgets.pattern, &widgets.find, &widgets.replace] {
        entry.connect_changed(clone!(
            #[strong]
            update,
            move |_| update()
        ));
    }
    widgets.start.connect_value_notify(clone!(
        #[strong]
        update,
        move |_| update()
    ));
    widgets.match_case.connect_active_notify(clone!(
        #[strong]
        update,
        move |_| update()
    ));
    widgets.case.connect_selected_notify(clone!(
        #[strong]
        update,
        move |_| update()
    ));
    update();

    let apply = Rc::new(clone!(
        #[weak]
        dialog,
        #[weak]
        pane,
        #[strong]
        widgets,
        #[strong]
        names,
        move || {
            if !widgets.rename_button.is_sensitive() {
                return;
            }
            let renames: Vec<_> = items
                .iter()
                .zip(names.borrow().iter())
                .filter(|(item, name)| item.display_name().as_str() != name.as_str())
                .map(|(item, name)| (item.file().clone(), name.clone()))
                .collect();
            dialog.close();
            pane.run_operation(Operation::BatchRename { renames }, false);
        }
    ));
    widgets.rename_button.connect_clicked(clone!(
        #[strong]
        apply,
        move |_| apply()
    ));
    for entry in [&widgets.pattern, &widgets.find, &widgets.replace] {
        entry.connect_entry_activated(clone!(
            #[strong]
            apply,
            move |_| apply()
        ));
    }
    cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    dialog.set_focus(Some(&widgets.pattern));
    dialog.present(Some(pane));
}

fn update_row(row: &PreviewRow, name: &str, unchanged: bool, problem: Option<&Problem>) {
    row.new_label.set_label(name);
    if unchanged {
        row.new_label.add_css_class("dimmed");
    } else {
        row.new_label.remove_css_class("dimmed");
    }
    row.icon.set_visible(problem.is_some());
    let message = problem.map(Problem::message);
    row.row.set_tooltip_text(message.as_deref());
    if problem.is_some() {
        row.new_label.add_css_class("error");
    } else {
        row.new_label.remove_css_class("error");
    }
}
