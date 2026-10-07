//! Copy / cut / paste of files through the system clipboard, compatible
//! with other file managers:
//!
//! - `x-special/gnome-copied-files` ("copy" or "cut", then one URI per
//!   line) for Nautilus, Nemo, Thunar, Caja…
//! - `text/uri-list` + `application/x-kde-cutselection` for Dolphin and
//!   most other apps,
//! - `text/plain` paths for terminals and text fields,
//! - a `gdk::FileList` value for GTK apps (and the file transfer portal).
//!
//! The service also remembers which files are cut, so views can dim them
//! until the clipboard changes, and the files it last put on the clipboard:
//! if the compositor refuses or drops the selection and nobody else owns
//! it, copy/paste still works within the app.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;
use std::sync::OnceLock;

use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
pub const URI_LIST: &str = "text/uri-list";
pub const KDE_CUT_SELECTION: &str = "application/x-kde-cutselection";
const PLAIN_TEXT: &str = "text/plain;charset=utf-8";

/// `x-special/gnome-copied-files` contents.
pub fn encode_gnome(cut: bool, uris: &[String]) -> String {
    let mut text = String::from(if cut { "cut" } else { "copy" });
    for uri in uris {
        text.push('\n');
        text.push_str(uri);
    }
    text
}

/// `text/uri-list` contents (RFC 2483: CRLF line endings).
pub fn encode_uri_list(uris: &[String]) -> String {
    uris.iter().map(|uri| format!("{uri}\r\n")).collect()
}

/// Parses `x-special/gnome-copied-files`: (is cut, URIs).
pub fn decode_gnome(text: &str) -> Option<(bool, Vec<String>)> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let cut = match lines.next()? {
        "cut" => true,
        "copy" => false,
        _ => return None,
    };
    Some((cut, lines.map(String::from).collect()))
}

/// Parses `text/uri-list`, skipping comments.
pub fn decode_uri_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// The clipboard contents for `files`.
pub fn content_provider(files: &[gio::File], cut: bool) -> gdk::ContentProvider {
    let uris: Vec<String> = files.iter().map(|f| f.uri().into()).collect();
    let paths: Vec<String> = files.iter().map(|f| f.parse_name().into()).collect();
    let bytes = |text: String| glib::Bytes::from_owned(text.into_bytes());
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_bytes(GNOME_COPIED_FILES, &bytes(encode_gnome(cut, &uris))),
        gdk::ContentProvider::for_bytes(URI_LIST, &bytes(encode_uri_list(&uris))),
        gdk::ContentProvider::for_bytes(
            KDE_CUT_SELECTION,
            &bytes(if cut { "1" } else { "0" }.into()),
        ),
        gdk::ContentProvider::for_bytes(PLAIN_TEXT, &bytes(paths.join("\n"))),
        gdk::ContentProvider::for_value(&gdk::FileList::from_array(files).to_value()),
    ])
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ClipboardService {
        pub(super) clipboard: OnceCell<gdk::Clipboard>,
        /// URIs of the files we put on the clipboard as cut.
        pub(super) cut: RefCell<HashSet<String>>,
        /// What we last put on the clipboard: (cut, files).
        pub(super) last: RefCell<Option<(bool, Vec<gio::File>)>>,
        pub(super) has_files: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipboardService {
        const NAME: &'static str = "LiaClipboardService";
        type Type = super::ClipboardService;
    }

    impl ObjectImpl for ClipboardService {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            // The clipboard contents or the cut files changed.
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    pub struct ClipboardService(ObjectSubclass<imp::ClipboardService>);
}

thread_local! {
    static SERVICE: OnceCell<ClipboardService> = const { OnceCell::new() };
}

/// The clipboard service of the default display.
pub fn clipboard_service() -> ClipboardService {
    SERVICE.with(|cell| {
        cell.get_or_init(|| {
            let service: ClipboardService = glib::Object::new();
            service.setup();
            service
        })
        .clone()
    })
}

impl ClipboardService {
    fn setup(&self) {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let clipboard = display.clipboard();
        clipboard.connect_changed(clone!(
            #[weak(rename_to = service)]
            self,
            move |clipboard| {
                // Someone else owns the clipboard now: our files are gone
                // from it, and nothing is cut anymore.
                if !clipboard.is_local() && !clipboard.formats().mime_types().is_empty() {
                    let imp = service.imp();
                    imp.cut.borrow_mut().clear();
                    imp.last.replace(None);
                }
                service.update_has_files();
                service.emit_by_name::<()>("changed", &[]);
            }
        ));
        self.imp().clipboard.set(clipboard).unwrap();
        self.update_has_files();
    }

    fn clipboard(&self) -> Option<&gdk::Clipboard> {
        self.imp().clipboard.get()
    }

    /// Whether the system clipboard offers files.
    fn offers_files(&self) -> bool {
        self.clipboard().is_some_and(|clipboard| {
            let formats = clipboard.formats();
            formats.contain_mime_type(GNOME_COPIED_FILES)
                || formats.contain_mime_type(URI_LIST)
                || formats.contains_type(gdk::FileList::static_type())
        })
    }

    fn update_has_files(&self) {
        let has_files = self.offers_files() || self.imp().last.borrow().is_some();
        self.imp().has_files.set(has_files);
    }

    /// Whether pasting would paste files.
    pub fn has_files(&self) -> bool {
        self.imp().has_files.get()
    }

    /// Whether `file` was cut (and should look dimmed).
    pub fn is_cut(&self, file: &gio::File) -> bool {
        let cut = self.imp().cut.borrow();
        !cut.is_empty() && cut.contains(file.uri().as_str())
    }

    /// Puts `files` on the clipboard.
    pub fn set(&self, files: &[gio::File], cut: bool) {
        let Some(clipboard) = self.clipboard() else {
            return;
        };
        {
            let mut set = self.imp().cut.borrow_mut();
            set.clear();
            if cut {
                set.extend(files.iter().map(|f| f.uri().to_string()));
            }
        }
        self.imp().last.replace(Some((cut, files.to_vec())));
        if let Err(err) = clipboard.set_content(Some(&content_provider(files, cut))) {
            tracing::warn!("cannot set the clipboard: {err}");
        }
        // `changed` also fires, but the cut set must be visible right away.
        self.update_has_files();
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Empties the clipboard after cut files were pasted (they're gone from
    /// where the clipboard says they are).
    pub fn clear_cut(&self) {
        if self.imp().cut.borrow().is_empty() {
            return;
        }
        self.imp().cut.borrow_mut().clear();
        self.imp().last.replace(None);
        if let Some(clipboard) = self.clipboard() {
            let _ = clipboard.set_content(None::<&gdk::ContentProvider>);
        }
        self.update_has_files();
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Reads files from the clipboard: (is cut, files).
    pub async fn read(&self) -> Option<(bool, Vec<gio::File>)> {
        let clipboard = self.clipboard()?.clone();
        let formats = clipboard.formats();
        let to_files = |uris: Vec<String>| -> Vec<gio::File> {
            uris.iter().map(|u| gio::File::for_uri(u)).collect()
        };

        if formats.contain_mime_type(GNOME_COPIED_FILES)
            && let Some((cut, uris)) = read_text(&clipboard, GNOME_COPIED_FILES)
                .await
                .and_then(|text| decode_gnome(&text))
        {
            return Some((cut, to_files(uris)));
        }
        if formats.contain_mime_type(URI_LIST)
            && let Some(text) = read_text(&clipboard, URI_LIST).await
        {
            let cut = formats.contain_mime_type(KDE_CUT_SELECTION)
                && read_text(&clipboard, KDE_CUT_SELECTION)
                    .await
                    .is_some_and(|t| t.trim() == "1");
            return Some((cut, to_files(decode_uri_list(&text))));
        }
        if formats.contains_type(gdk::FileList::static_type()) {
            let value = clipboard
                .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
                .await
                .ok()?;
            let list = value.get::<gdk::FileList>().ok()?;
            return Some((false, list.files()));
        }
        self.imp().last.borrow().clone()
    }

    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "changed",
            false,
            glib::closure_local!(move |service: &Self| f(service)),
        )
    }
}

/// Reads one clipboard format as text.
async fn read_text(clipboard: &gdk::Clipboard, mime: &str) -> Option<String> {
    let (stream, _) = clipboard
        .read_future(&[mime], glib::Priority::DEFAULT)
        .await
        .ok()?;
    let mut data = Vec::new();
    loop {
        let bytes = stream
            .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
            .await
            .ok()?;
        if bytes.is_empty() {
            break;
        }
        data.extend_from_slice(&bytes);
    }
    let _ = stream.close_future(glib::Priority::DEFAULT).await;
    String::from_utf8(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_format_round_trips() {
        let uris = vec!["file:///a".to_owned(), "file:///b%20c".to_owned()];
        let text = encode_gnome(true, &uris);
        assert_eq!(text, "cut\nfile:///a\nfile:///b%20c");
        assert_eq!(decode_gnome(&text), Some((true, uris.clone())));
        // Some apps add a trailing newline or CRLF.
        assert_eq!(
            decode_gnome("copy\r\nfile:///a\r\nfile:///b%20c\n"),
            Some((false, uris))
        );
        assert_eq!(decode_gnome("hello"), None);
    }

    #[test]
    fn uri_lists() {
        let uris = vec!["file:///a".to_owned(), "sftp://host/b".to_owned()];
        assert_eq!(encode_uri_list(&uris), "file:///a\r\nsftp://host/b\r\n");
        assert_eq!(
            decode_uri_list("# comment\r\nfile:///a\r\n\r\nsftp://host/b"),
            uris
        );
    }
}
