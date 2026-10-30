//! Run explicitly on a disposable bus and display; never touches the user's portal.
use super::*;
use crate::widgets::folder_pane::FolderPane;
use std::time::{Duration, Instant};

fn find<T: IsA<gtk::Widget> + glib::object::IsClass>(widget: &gtk::Widget) -> Option<T> {
    if let Ok(found) = widget.clone().downcast::<T>() {
        return Some(found);
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(found) = find::<T>(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

async fn until(mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "UI did not settle"
        );
        glib::timeout_future(Duration::from_millis(20)).await;
    }
}

type Reply = Rc<RefCell<Option<Result<glib::Variant, glib::Error>>>>;
fn call(
    client: &gio::DBusConnection,
    destination: &str,
    handle: &str,
    method: &str,
    options: Options,
) -> Reply {
    let reply = Rc::new(RefCell::new(None));
    let result = reply.clone();
    let handle = glib::variant::ObjectPath::try_from(handle).unwrap();
    client.call(
        Some(destination),
        OBJECT_PATH,
        INTERFACE,
        method,
        Some(&(handle, "test.app", "", "Choose with Lia Files", options).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        15000,
        gio::Cancellable::NONE,
        move |value| {
            result.replace(Some(value));
        },
    );
    reply
}

fn result(reply: &Reply) -> (u32, Options) {
    reply.borrow_mut().take().unwrap().unwrap().get().unwrap()
}

#[test]
#[ignore = "requires a disposable D-Bus session and GTK display; see docs/file-chooser.md"]
fn portal_round_trip() {
    crate::register_resources();
    adw::init().unwrap();
    let context = glib::MainContext::default();
    context.block_on(async {
        let app = adw::Application::builder()
            .application_id("io.github.lia.Files.PortalTest")
            .resource_base_path(crate::config::RESOURCE_BASE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let connection = app.dbus_connection().unwrap();
        let registration = export(&app, &connection).unwrap();
        let client = gio::DBusConnection::for_address_sync(
            &std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(),
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .unwrap();
        let destination = connection.unique_name().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a # b.txt");
        let second = dir.path().join("c.txt");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&second, "c").unwrap();
        std::fs::write(dir.path().join("hidden.png"), "png").unwrap();
        let mut base = Options::new();
        use std::os::unix::ffi::OsStrExt;
        let mut path = dir.path().as_os_str().as_bytes().to_vec();
        path.push(0);
        base.insert("current_folder".into(), path.to_variant());

        // Multiple files, a glob filter and an application choice round-trip.
        let mut options = base.clone();
        options.insert("multiple".into(), true.to_variant());
        let filter: FileFilter = ("Text".into(), vec![(0, "*.txt".into())]);
        options.insert("filters".into(), vec![filter.clone()].to_variant());
        options.insert(
            "choices".into(),
            vec![(
                "encoding",
                "Encoding",
                vec![("utf8", "UTF-8"), ("latin1", "Latin 1")],
                "utf8",
            )]
            .to_variant(),
        );
        let reply = call(&client, &destination, "/test/open", "OpenFile", options);
        until(|| !app.windows().is_empty()).await;
        let window = app.windows()[0].clone();
        let pane = find::<FolderPane>(window.upcast_ref()).unwrap();
        until(|| !pane.loading()).await;
        // The unmatched PNG cannot enter the picker selection.
        pane.select_files(
            vec![gio::File::for_path(dir.path().join("hidden.png"))],
            false,
        );
        assert_eq!(pane.n_selected(), 0);
        pane.select_files(
            vec![gio::File::for_path(&first), gio::File::for_path(&second)],
            false,
        );
        until(|| pane.n_selected() == 2).await;
        let button = window
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert!(button.is_sensitive());
        if let Ok(path) = std::env::var("LIA_PICKER_SCREENSHOT") {
            glib::timeout_future(Duration::from_millis(300)).await;
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            let node = snapshot.to_node().expect("mapped picker snapshot");
            window
                .renderer()
                .unwrap()
                .render_texture(&node, None)
                .save_to_png(path)
                .unwrap();
        }
        button.emit_clicked();
        until(|| reply.borrow().is_some()).await;
        let (code, values) = result(&reply);
        assert_eq!(code, 0);
        assert_eq!(values["uris"].get::<Vec<String>>().unwrap().len(), 2);
        assert_eq!(
            values["current_filter"].get::<FileFilter>().unwrap(),
            filter
        );
        assert_eq!(
            values["choices"].get::<Vec<(String, String)>>().unwrap(),
            [("encoding".into(), "utf8".into())]
        );
        assert!(app.windows().is_empty());

        // Folder mode accepts the current folder with no explicit selection.
        let mut options = base.clone();
        options.insert("directory".into(), true.to_variant());
        let reply = call(&client, &destination, "/test/folder", "OpenFile", options);
        until(|| !app.windows().is_empty()).await;
        let window = app.windows()[0].clone();
        let pane = find::<FolderPane>(window.upcast_ref()).unwrap();
        until(|| !pane.loading()).await;
        assert_eq!(pane.n_selected(), 0);
        window
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        until(|| reply.borrow().is_some()).await;
        let (code, values) = result(&reply);
        assert_eq!(code, 0);
        assert_eq!(
            values["uris"].get::<Vec<String>>().unwrap(),
            [gio::File::for_path(dir.path()).uri()]
        );

        // Independent concurrent requests: Close cancels just its own window.
        let first_reply = call(
            &client,
            &destination,
            "/test/cancel1",
            "OpenFile",
            base.clone(),
        );
        let second_reply = call(
            &client,
            &destination,
            "/test/cancel2",
            "OpenFile",
            base.clone(),
        );
        until(|| app.windows().len() == 2).await;
        client
            .call_future(
                Some(&destination),
                "/test/cancel1",
                "org.freedesktop.impl.portal.Request",
                "Close",
                None,
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await
            .unwrap();
        until(|| first_reply.borrow().is_some()).await;
        assert_eq!(result(&first_reply).0, 1);
        assert_eq!(app.windows().len(), 1);
        assert!(second_reply.borrow().is_none());
        app.windows()[0].close();
        until(|| second_reply.borrow().is_some()).await;
        assert_eq!(result(&second_reply).0, 1);
        assert!(app.windows().is_empty());

        // A single-selection activation returns the file instead of launching it.
        let reply = call(
            &client,
            &destination,
            "/test/single",
            "OpenFile",
            base.clone(),
        );
        until(|| !app.windows().is_empty()).await;
        let window = app.windows()[0].clone();
        let pane = find::<FolderPane>(window.upcast_ref()).unwrap();
        until(|| !pane.loading()).await;
        pane.select_files(
            vec![gio::File::for_path(&first), gio::File::for_path(&second)],
            false,
        );
        assert_eq!(pane.n_selected(), 1);
        let item = pane.selected_items()[0].clone();
        pane.open_item(&item, false);
        until(|| reply.borrow().is_some()).await;
        assert_eq!(result(&reply).0, 0);

        // A different bus peer cannot cancel someone else's interaction.
        let reply = call(
            &client,
            &destination,
            "/test/owner",
            "OpenFile",
            base.clone(),
        );
        until(|| !app.windows().is_empty()).await;
        let error = connection
            .call_future(
                Some(&destination),
                "/test/owner",
                "org.freedesktop.impl.portal.Request",
                "Close",
                None,
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await
            .unwrap_err();
        assert!(error.matches(gio::DBusError::AccessDenied));
        assert_eq!(app.windows().len(), 1);
        app.windows()[0].close();
        until(|| reply.borrow().is_some()).await;
        assert_eq!(result(&reply).0, 1);
        assert!(
            client
                .call_future(
                    Some(&destination),
                    "/test/owner",
                    "org.freedesktop.impl.portal.Request",
                    "Close",
                    None,
                    None,
                    gio::DBusCallFlags::NONE,
                    5000
                )
                .await
                .is_err()
        );

        // SaveFile returns the typed name in the current folder, even though
        // it doesn't exist yet.
        let mut options = base.clone();
        options.insert("current_name".into(), "saved.txt".to_variant());
        options.insert("filters".into(), vec![filter.clone()].to_variant());
        let reply = call(&client, &destination, "/test/save", "SaveFile", options);
        until(|| !app.windows().is_empty()).await;
        let window = app.windows()[0].clone();
        let pane = find::<FolderPane>(window.upcast_ref()).unwrap();
        let name = find::<gtk::Entry>(window.upcast_ref()).unwrap();
        assert_eq!(name.text(), "saved.txt");
        until(|| !pane.loading()).await;
        let button = window
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert!(button.is_sensitive());
        button.emit_clicked();
        until(|| reply.borrow().is_some()).await;
        let (code, values) = result(&reply);
        assert_eq!(code, 0);
        assert_eq!(
            values["uris"].get::<Vec<String>>().unwrap(),
            [gio::File::for_path(dir.path().join("saved.txt")).uri()]
        );
        assert_eq!(
            values["current_filter"].get::<FileFilter>().unwrap(),
            filter
        );
        assert!(app.windows().is_empty());

        // Saving over an existing file asks first; activating it fills the name.
        let reply = call(
            &client,
            &destination,
            "/test/replace",
            "SaveFile",
            base.clone(),
        );
        until(|| !app.windows().is_empty()).await;
        let window = app.windows()[0].clone();
        let pane = find::<FolderPane>(window.upcast_ref()).unwrap();
        until(|| !pane.loading()).await;
        pane.select_files(vec![gio::File::for_path(&second)], false);
        until(|| pane.n_selected() == 1).await;
        let item = pane.selected_items()[0].clone();
        pane.open_item(&item, false);
        let mut dialog = None;
        until(|| {
            dialog = find::<adw::AlertDialog>(window.upcast_ref());
            dialog.is_some()
        })
        .await;
        assert!(reply.borrow().is_none());
        dialog
            .unwrap()
            .emit_by_name::<()>("response", &[&"replace"]);
        until(|| reply.borrow().is_some()).await;
        let (code, values) = result(&reply);
        assert_eq!(code, 0);
        assert_eq!(
            values["uris"].get::<Vec<String>>().unwrap(),
            [gio::File::for_path(&second).uri()]
        );

        // SaveFiles picks a folder and keeps existing files by renaming.
        let mut options = base.clone();
        options.insert(
            "files".into(),
            vec![b"c.txt\0".to_vec(), b"new.txt\0".to_vec()].to_variant(),
        );
        let reply = call(&client, &destination, "/test/saves", "SaveFiles", options);
        until(|| !app.windows().is_empty()).await;
        let window = app.windows()[0].clone();
        let pane = find::<FolderPane>(window.upcast_ref()).unwrap();
        until(|| !pane.loading()).await;
        window
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        until(|| reply.borrow().is_some()).await;
        let (code, values) = result(&reply);
        assert_eq!(code, 0);
        assert_eq!(
            values["uris"].get::<Vec<String>>().unwrap(),
            [
                gio::File::for_path(dir.path().join("c (2).txt")).uri(),
                gio::File::for_path(dir.path().join("new.txt")).uri(),
            ]
        );

        // Wrong option types fail cleanly without opening a window.
        let mut options = base;
        options.insert("multiple".into(), "wrong".to_variant());
        let reply = call(&client, &destination, "/test/invalid", "OpenFile", options);
        until(|| reply.borrow().is_some()).await;
        assert_eq!(result(&reply).0, 2);
        assert!(app.windows().is_empty());
        // A vanished caller leaves no orphan picker window.
        let _reply = call(
            &client,
            &destination,
            "/test/disconnect",
            "OpenFile",
            Options::new(),
        );
        until(|| !app.windows().is_empty()).await;
        client.close_future().await.unwrap();
        until(|| app.windows().is_empty()).await;
        connection.unregister_object(registration).unwrap();
    });
}
