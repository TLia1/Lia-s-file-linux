//! Theme-native disk usage browser with a cancellable background scan.
use crate::{
    services::disk_analyzer::{self, Scan},
    util::format,
    widgets::folder_pane::FolderPane,
};
use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Browser {
    scan: Scan,
    current: Cell<usize>,
    limit: Cell<usize>,
    list: gtk::ListBox,
    path: gtk::Label,
    summary: gtk::Label,
    up: gtk::Button,
    more: gtk::Button,
}
impl Browser {
    fn render(&self) {
        self.list.remove_all();
        let current = self.current.get();
        let entry = &self.scan.entries[current];
        self.path
            .set_text(&format::one_line(&entry.path.to_string_lossy()));
        self.path
            .set_tooltip_text(Some(&entry.path.to_string_lossy()));
        self.up.set_sensitive(entry.parent.is_some());
        let children = self
            .scan
            .children
            .get(&current)
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.summary.set_text(&format!(
            "{} · {}",
            format::size(entry.bytes),
            format::item_count(children.len().min(u32::MAX as usize) as u32)
        ));
        self.more.set_visible(children.len() > self.limit.get());
        for index in children.iter().take(self.limit.get()) {
            let child = &self.scan.entries[*index];
            let name = child.path.file_name().unwrap_or_default().to_string_lossy();
            let row = adw::ActionRow::builder()
                .title(format::one_line(&name))
                .use_markup(false)
                .activatable(true)
                .build();
            row.set_widget_name(&index.to_string());
            row.add_prefix(&gtk::Image::from_icon_name(if child.directory {
                "folder-symbolic"
            } else {
                "text-x-generic-symbolic"
            }));
            let fraction = if entry.bytes == 0 {
                0.0
            } else {
                child.bytes as f64 / entry.bytes as f64
            };
            row.set_subtitle(&format!(
                "{} · {:.1}%",
                format::size(child.bytes),
                fraction * 100.0
            ));
            row.set_tooltip_text(Some(&format!(
                "{} — {} bytes",
                child.path.display(),
                child.bytes
            )));
            let bar = gtk::ProgressBar::builder()
                .fraction(fraction.clamp(0.0, 1.0))
                .width_request(110)
                .valign(gtk::Align::Center)
                .build();
            bar.update_property(&[gtk::accessible::Property::Label(&gettext("Used space"))]);
            row.add_suffix(&bar);
            if child.directory {
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            } else {
                let reveal = gtk::Label::new(Some(&gettext("Show in Folder")));
                reveal.add_css_class("dim-label");
                row.add_suffix(&reveal);
            }
            self.list.append(&row);
        }
        if children.is_empty() {
            self.list.append(&gtk::Label::new(Some(&gettext(
                "No readable entries in this folder",
            ))));
        }
    }
}

enum Update {
    Progress(usize),
    Done(std::io::Result<Scan>),
}

pub fn present(parent: &impl IsA<gtk::Widget>, root: PathBuf, pane: &FolderPane) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Disk Usage"))
        .content_width(700)
        .content_height(620)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for set in [
        gtk::Widget::set_margin_start,
        gtk::Widget::set_margin_end,
        gtk::Widget::set_margin_top,
        gtk::Widget::set_margin_bottom,
    ] {
        set(body.upcast_ref(), 18);
    }
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let up = gtk::Button::from_icon_name("go-up-symbolic");
    up.set_tooltip_text(Some(&gettext("Parent folder")));
    up.set_sensitive(false);
    let path = gtk::Label::builder()
        .label(root.to_string_lossy())
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .hexpand(true)
        .xalign(0.0)
        .selectable(true)
        .build();
    let open = gtk::Button::with_label(&gettext("Open Folder"));
    open.set_sensitive(false);
    controls.append(&up);
    controls.append(&path);
    controls.append(&open);
    body.append(&controls);
    let summary = gtk::Label::builder()
        .label(gettext("Scanning…"))
        .xalign(0.0)
        .css_classes(["title-2"])
        .build();
    body.append(&summary);
    let notice = gtk::Label::builder().label(gettext("Space on disk · Hidden files included · Hard links counted once · Other filesystems skipped")).wrap(true).xalign(0.0).css_classes(["dim-label"]).build();
    body.append(&notice);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Disk usage results",
    ))]);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    body.append(&scroll);
    let more = gtk::Button::with_label(&gettext("Show more"));
    more.set_visible(false);
    body.append(&more);
    let stop = gtk::Button::with_label(&gettext("Cancel Scan"));
    body.append(&stop);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    let cancel = Arc::new(AtomicBool::new(false));
    let state: Rc<RefCell<Option<Browser>>> = Rc::new(RefCell::new(None));
    let token = cancel.clone();
    stop.connect_clicked(move |button| {
        token.store(true, Ordering::Relaxed);
        button.set_sensitive(false);
    });
    let token = cancel.clone();
    let closed_state = state.clone();
    dialog.connect_closed(move |_| {
        token.store(true, Ordering::Relaxed);
        closed_state.borrow_mut().take();
    });
    let s = state.clone();
    up.connect_clicked(move |_| {
        if let Some(b) = s.borrow().as_ref()
            && let Some(parent) = b.scan.entries[b.current.get()].parent
        {
            b.current.set(parent);
            b.limit.set(100);
            b.render();
        }
    });
    let s = state.clone();
    more.connect_clicked(move |_| {
        if let Some(b) = s.borrow().as_ref() {
            b.limit.set(b.limit.get() + 100);
            b.render();
        }
    });
    let s = state.clone();
    let weak_pane_for_row = pane.downgrade();
    let weak_dialog_for_row = dialog.downgrade();
    list.connect_row_activated(move |_, row| {
        let reveal = {
            let state = s.borrow();
            let Some(b) = state.as_ref() else {
                return;
            };
            let Ok(index) = row.widget_name().parse::<usize>() else {
                return;
            };
            if b.scan.entries[index].directory {
                b.current.set(index);
                b.limit.set(100);
                b.render();
                None
            } else {
                Some(b.scan.entries[index].path.clone())
            }
        };
        if let Some(path) = reveal
            && let Some(parent) = path.parent()
            && let Some(pane) = weak_pane_for_row.upgrade()
        {
            pane.navigate_to(&gio::File::for_path(parent));
            pane.select_files(vec![gio::File::for_path(&path)], false);
            if let Some(dialog) = weak_dialog_for_row.upgrade() {
                dialog.close();
            }
        }
    });
    let s = state.clone();
    let weak_pane = pane.downgrade();
    let weak_dialog = dialog.downgrade();
    open.connect_clicked(move |_| {
        let path = s
            .borrow()
            .as_ref()
            .map(|b| b.scan.entries[b.current.get()].path.clone());
        if let (Some(path), Some(pane)) = (path, weak_pane.upgrade()) {
            pane.navigate_to(&gio::File::for_path(path));
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.close();
            }
        }
    });
    let (sender, receiver) = async_channel::bounded(2);
    let worker_cancel = cancel.clone();
    let spawned = std::thread::Builder::new()
        .name("lia-disk-analyzer".into())
        .spawn(move || {
            let mut last = Instant::now();
            let result = disk_analyzer::scan(root, &worker_cancel, |count| {
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    let _ = sender.try_send(Update::Progress(count));
                }
            });
            let _ = sender.send_blocking(Update::Done(result));
        });
    if let Err(error) = spawned {
        summary.set_text(&error.to_string());
        stop.set_visible(false);
    }
    let weak_dialog = dialog.downgrade();
    glib::spawn_future_local(async move {
        while let Ok(update) = receiver.recv().await {
            if weak_dialog.upgrade().is_none() {
                break;
            }
            match update {
                Update::Progress(count) => summary
                    .set_text(&gettext("Scanning… {} items").replace("{}", &count.to_string())),
                Update::Done(result) => {
                    stop.set_visible(false);
                    match result {
                        Ok(scan) if !cancel.load(Ordering::Relaxed) => {
                            notice.set_text(&gettext("Allocated space · {unreadable} unreadable entries · {skipped} excluded mounts/system folders. Links are not followed; hard links count once. Directory metadata is included.").replace("{unreadable}", &scan.unreadable.to_string()).replace("{skipped}", &scan.skipped.to_string()));
                            let browser = Browser {
                                scan,
                                current: Cell::new(0),
                                limit: Cell::new(100),
                                list: list.clone(),
                                path: path.clone(),
                                summary: summary.clone(),
                                up: up.clone(),
                                more: more.clone(),
                            };
                            browser.render();
                            state.replace(Some(browser));
                            open.set_sensitive(true);
                        }
                        Ok(_) => summary.set_text(&gettext("Scan cancelled")),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                            summary.set_text(&gettext("Scan cancelled"))
                        }
                        Err(error) => summary.set_text(
                            &gettext("Could not scan: {}").replace("{}", &error.to_string()),
                        ),
                    }
                    break;
                }
            }
        }
    });
    dialog.present(Some(parent));
}
