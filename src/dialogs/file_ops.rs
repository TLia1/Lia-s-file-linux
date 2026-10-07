//! Questions asked during and before file operations: conflicts, errors,
//! and confirmations for what can't be undone.

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib};

use super::conflict::ConflictDialog;
use crate::services::file_ops::*;

/// Where a job's dialogs appear: its window, or the active one if that
/// window was closed.
fn parent_of(job: &FileOpJob) -> Option<gtk::Widget> {
    job.origin().filter(|w| w.root().is_some()).or_else(|| {
        gio::Application::default()
            .and_downcast::<gtk::Application>()
            .and_then(|app| app.active_window())
            .map(|w| w.upcast())
    })
}

/// Shows `dialog`, closing it if the job gets cancelled meanwhile.
async fn choose(job: &FileOpJob, dialog: &adw::AlertDialog) -> glib::GString {
    let weak = dialog.downgrade();
    let handler = job.connect_state_notify(move |job| {
        if job.state() == JobState::Cancelled
            && let Some(dialog) = weak.upgrade()
        {
            dialog.close();
        }
    });
    let response = dialog.clone().choose_future(parent_of(job).as_ref()).await;
    job.disconnect(handler);
    response
}

pub async fn ask_conflict(job: &FileOpJob, conflict: &Conflict) -> ConflictResponse {
    let operation = job.operation();
    let many = operation.item_count() > 1 || conflict.is_merge || conflict.source.is_dir;
    let dialog = ConflictDialog::new(conflict, many);
    let choice = match choose(job, dialog.upcast_ref()).await.as_str() {
        "replace" => ConflictChoice::Replace,
        "keep" => ConflictChoice::KeepBoth,
        "skip" => ConflictChoice::Skip,
        _ => ConflictChoice::Cancel,
    };
    ConflictResponse {
        choice,
        apply_to_all: dialog.apply_to_all(),
    }
}

pub async fn ask_error(job: &FileOpJob, error: &OpError) -> ErrorResponse {
    let name = &error.file_name;
    let many = job.operation().item_count() > 1
        || matches!(job.operation(), Operation::EmptyTrash)
        || job.bytes_total() > 0;
    let dialog = adw::AlertDialog::new(None, None);
    let mut apply_all = None;

    if error.kind == ErrorKind::TrashUnsupported {
        dialog.set_heading(Some(
            &gettext("“{}” Can't Be Moved to the Trash").replace("{}", name),
        ));
        dialog.set_body(&format!(
            "{}\n\n{}",
            error.message,
            gettext("Delete it permanently instead? It can't be recovered.")
        ));
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("skip", &gettext("_Skip")),
            ("delete", &gettext("_Delete Permanently")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("skip"));
        if many {
            let check = gtk::CheckButton::with_mnemonic(&gettext("_Do this for all items"));
            dialog.set_extra_child(Some(&check));
            apply_all = Some(check);
        }
    } else {
        dialog.set_heading(Some(&error_heading(job.operation().kind(), name)));
        dialog.set_body(&error.message);
        if error.fatal && !many {
            dialog.add_responses(&[("skip", &gettext("_OK"))]);
        } else {
            dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("skip", &gettext("_Skip"))]);
            if many {
                dialog.add_responses(&[("skip-all", &gettext("Skip _All"))]);
            }
            if !error.fatal {
                dialog.add_responses(&[("retry", &gettext("_Retry"))]);
                dialog.set_response_appearance("retry", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("retry"));
            }
        }
    }
    dialog.set_close_response(if error.fatal && !many {
        "skip"
    } else {
        "cancel"
    });

    let (choice, all) = match choose(job, &dialog).await.as_str() {
        "retry" => (ErrorChoice::Retry, false),
        "skip" => (ErrorChoice::Skip, false),
        "skip-all" => (ErrorChoice::Skip, true),
        "delete" => (ErrorChoice::DeletePermanently, false),
        _ => (ErrorChoice::Cancel, false),
    };
    ErrorResponse {
        choice,
        apply_to_all: all || apply_all.is_some_and(|c| c.is_active()),
    }
}

fn error_heading(kind: OpKind, name: &str) -> String {
    match kind {
        OpKind::Copy | OpKind::Duplicate => gettext("Couldn't Copy “{}”"),
        OpKind::Move => gettext("Couldn't Move “{}”"),
        OpKind::Link => gettext("Couldn't Create a Link to “{}”"),
        OpKind::Trash => gettext("Couldn't Move “{}” to the Trash"),
        OpKind::Delete | OpKind::EmptyTrash => gettext("Couldn't Delete “{}”"),
        OpKind::Restore => gettext("Couldn't Restore “{}”"),
        OpKind::Rename => gettext("Couldn't Rename “{}”"),
        OpKind::CreateFolder | OpKind::CreateFile => gettext("Couldn't Create “{}”"),
    }
    .replace("{}", name)
}

/// Asks before deleting permanently. `name` is used for a single item.
pub async fn confirm_delete(parent: &impl IsA<gtk::Widget>, count: usize, name: &str) -> bool {
    let heading = if count == 1 {
        gettext("Permanently Delete “{}”?").replace("{}", name)
    } else {
        ngettext(
            "Permanently Delete {} Item?",
            "Permanently Delete {} Items?",
            count as u32,
        )
        .replace("{}", &count.to_string())
    };
    let dialog = adw::AlertDialog::new(
        Some(&heading),
        Some(&gettext(
            "This can't be undone. Deleted items can't be recovered.",
        )),
    );
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("delete", &gettext("_Delete")),
    ]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose_future(Some(parent)).await == "delete"
}

pub async fn confirm_empty_trash(parent: &impl IsA<gtk::Widget>) -> bool {
    let dialog = adw::AlertDialog::new(
        Some(&gettext("Empty the Trash?")),
        Some(&gettext(
            "All items in the Trash will be permanently deleted. This can't be undone.",
        )),
    );
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("empty", &gettext("_Empty Trash")),
    ]);
    dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose_future(Some(parent)).await == "empty"
}
