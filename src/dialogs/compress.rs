//! Archive dialogs: Compress… (archive name and format) and the folder
//! chooser of Extract To….

use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib, glib::clone};

use crate::services::archives::{self, ArchiveFormat};
use crate::services::file_ops::{Operation, naming};
use crate::util::settings;
use crate::widgets::folder_pane::FolderPane;

/// Asks for the name and format of an archive of `files`, created in the
/// pane's folder. `names` are the files' (display name, is folder).
pub fn present(pane: &FolderPane, files: Vec<gio::File>, names: &[(String, bool)], portable: bool) {
    let Some(folder) = pane.location() else {
        return;
    };
    let name = adw::EntryRow::builder()
        .title(gettext("Archive Name"))
        .text(archives::suggested_name(names))
        .build();
    let labels: Vec<String> = ArchiveFormat::ALL.iter().map(|f| f.label()).collect();
    let format = adw::ComboRow::builder()
        .title(gettext("Format"))
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .build();
    let saved = ArchiveFormat::from_nick(&settings().string("compress-format"));
    let index = ArchiveFormat::ALL
        .iter()
        .position(|f| *f == saved)
        .unwrap_or(0);
    format.set_selected(index as u32);
    let error = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["error", "caption"])
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.append(&name);
    list.append(&format);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    content.append(&list);
    content.append(&error);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Compress"))
        .extra_child(&content)
        .default_response("compress")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("compress", &gettext("C_ompress")),
    ]);
    dialog.set_response_appearance("compress", adw::ResponseAppearance::Suggested);

    let selected_format = |format: &adw::ComboRow| {
        ArchiveFormat::ALL
            .get(format.selected() as usize)
            .copied()
            .unwrap_or_default()
    };
    let validate = Rc::new(clone!(
        #[weak]
        dialog,
        #[weak]
        name,
        #[weak]
        format,
        #[weak]
        error,
        move || {
            let file_name = format!(
                "{}{}",
                name.text().trim(),
                selected_format(&format).extension()
            );
            let problem = if name.text().trim().is_empty() {
                Some(gettext("Enter a name for the archive"))
            } else {
                naming::validate_name(&file_name, portable)
                    .err()
                    .map(|err| err.message())
            };
            error.set_label(problem.as_deref().unwrap_or_default());
            error.set_visible(problem.is_some());
            dialog.set_response_enabled("compress", problem.is_none());
        }
    ));
    name.connect_changed(clone!(
        #[strong]
        validate,
        move |_| validate()
    ));
    format.connect_selected_notify(clone!(
        #[strong]
        validate,
        move |_| validate()
    ));
    name.connect_entry_activated(clone!(
        #[weak]
        dialog,
        move |_| {
            if dialog.is_response_enabled("compress") {
                dialog.emit_by_name::<()>("response", &[&"compress"]);
                dialog.close();
            }
        }
    ));
    validate();

    dialog.connect_response(
        Some("compress"),
        clone!(
            #[weak]
            pane,
            #[weak]
            name,
            #[weak]
            format,
            move |_, _| {
                let chosen = selected_format(&format);
                let _ = settings().set_string("compress-format", chosen.nick());
                let target = folder.child(format!("{}{}", name.text().trim(), chosen.extension()));
                pane.run_operation(
                    Operation::Compress {
                        sources: files.clone(),
                        target,
                        format: chosen,
                    },
                    false,
                );
            }
        ),
    );
    dialog.set_focus(Some(&name));
    dialog.present(Some(pane));
    // Select the name for typing over it.
    glib::idle_add_local_once(clone!(
        #[weak]
        name,
        move || {
            name.grab_focus();
            name.select_region(0, -1);
        }
    ));
}

/// Asks for a folder, then extracts `archives` into it.
pub fn extract_to(pane: &FolderPane, archives: Vec<gio::File>) {
    let chooser = gtk::FileDialog::builder()
        .title(gettext("Extract To"))
        .accept_label(gettext("_Extract Here"))
        .modal(true)
        .build();
    if let Some(location) = pane.location().filter(|l| l.path().is_some()) {
        chooser.set_initial_folder(Some(&location));
    }
    let window = pane.root().and_downcast::<gtk::Window>();
    glib::spawn_future_local(clone!(
        #[weak]
        pane,
        async move {
            match chooser.select_folder_future(window.as_ref()).await {
                Ok(destination) => {
                    pane.run_operation(
                        Operation::Extract {
                            archives,
                            destination,
                        },
                        false,
                    );
                }
                Err(err) if err.matches(gtk::DialogError::Dismissed) => {}
                Err(err) => tracing::warn!("cannot choose a folder: {err}"),
            }
        }
    ));
}
