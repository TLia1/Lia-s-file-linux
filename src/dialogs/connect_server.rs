//! The Connect to Server dialog: a server address (`smb://`, `sftp://`,
//! `dav://`…), examples, and the recently used servers. Connecting mounts
//! the location through GVfs, asking for a password if needed.

use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib, glib::clone};

use crate::services::{network, volumes};

/// Asks for a server and mounts it; `on_connected` gets its location.
pub fn present(parent: &impl IsA<gtk::Widget>, on_connected: impl Fn(gio::File) + 'static) {
    let address = adw::EntryRow::builder()
        .title(gettext("Server Address"))
        .input_purpose(gtk::InputPurpose::Url)
        .build();
    let error = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .margin_top(6)
        .css_classes(["error", "caption"])
        .build();
    let address_group = adw::PreferencesGroup::builder()
        .description(gettext(
            "For example smb://nas/share, sftp://user@host, dav://host/path or ftp://host",
        ))
        .build();
    address_group.add(&address);
    address_group.add(&error);

    let recent_group = adw::PreferencesGroup::builder()
        .title(gettext("Recent Servers"))
        .build();

    let page = adw::PreferencesPage::new();
    page.add(&address_group);
    page.add(&recent_group);

    let connect_button = gtk::Button::builder()
        .label(gettext("C_onnect"))
        .use_underline(true)
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    let spinner = adw::Spinner::builder().visible(false).build();
    let cancel_button = gtk::Button::with_mnemonic(&gettext("_Cancel"));
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel_button);
    header.pack_end(&connect_button);
    header.pack_end(&spinner);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .title(gettext("Connect to Server"))
        .content_width(460)
        .content_height(480)
        .child(&toolbar)
        .build();

    let on_connected = Rc::new(on_connected);
    let connect = Rc::new(clone!(
        #[weak]
        dialog,
        #[weak]
        address,
        #[weak]
        error,
        #[weak]
        connect_button,
        #[weak]
        spinner,
        move || {
            let uri = match network::parse_address(&address.text()) {
                Ok(uri) => uri,
                Err(err) => {
                    error.set_label(&err.message());
                    error.set_visible(true);
                    return;
                }
            };
            error.set_visible(false);
            connect_button.set_sensitive(false);
            address.set_sensitive(false);
            spinner.set_visible(true);
            let location = gio::File::for_uri(&uri);
            let on_connected = on_connected.clone();
            glib::spawn_future_local(async move {
                let result = volumes::mount_location(&location, Some(dialog.upcast_ref())).await;
                spinner.set_visible(false);
                connect_button.set_sensitive(true);
                address.set_sensitive(true);
                match result {
                    Ok(()) => {
                        network::remember_server(&uri);
                        dialog.close();
                        on_connected(location);
                    }
                    Err(err) if volumes::is_handled(&err) => {}
                    Err(err) => {
                        tracing::warn!("cannot connect to {uri}: {err}");
                        let message = if err.matches(gio::IOErrorEnum::NotSupported) {
                            gettext(
                                "This kind of location isn't available. Network locations \
                                 need GVfs and its backends (e.g. the “gvfs-smb” package).",
                            )
                        } else {
                            err.message().to_owned()
                        };
                        error.set_label(&message);
                        error.set_visible(true);
                        address.grab_focus();
                    }
                }
            });
        }
    ));

    address.connect_changed(clone!(
        #[weak]
        connect_button,
        #[weak]
        error,
        move |entry| {
            connect_button.set_sensitive(!entry.text().trim().is_empty());
            error.set_visible(false);
        }
    ));
    address.connect_entry_activated(clone!(
        #[strong]
        connect,
        move |_| connect()
    ));
    connect_button.connect_clicked(clone!(
        #[strong]
        connect,
        move |_| connect()
    ));
    cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    // Recent servers: activate to connect again.
    let servers = network::recent_servers();
    recent_group.set_visible(!servers.is_empty());
    for uri in servers {
        let row = adw::ActionRow::builder()
            .title(&uri)
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("folder-remote-symbolic"));
        let forget = gtk::Button::builder()
            .icon_name("edit-delete-symbolic")
            .tooltip_text(gettext("Remove From Recent Servers"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        forget.connect_clicked(clone!(
            #[weak]
            row,
            #[weak]
            recent_group,
            #[strong]
            uri,
            move |_| {
                network::forget_server(&uri);
                recent_group.remove(&row);
                recent_group.set_visible(!network::recent_servers().is_empty());
            }
        ));
        row.add_suffix(&forget);
        row.connect_activated(clone!(
            #[weak]
            address,
            #[strong]
            connect,
            #[strong]
            uri,
            move |_| {
                address.set_text(&uri);
                connect();
            }
        ));
        recent_group.add(&row);
    }

    dialog.set_focus(Some(&address));
    dialog.present(Some(parent));
}
