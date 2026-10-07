//! `AddressBar`: a breadcrumb trail that turns into a text entry when you
//! click its empty area or press Ctrl+L. The entry completes folder names
//! with Tab.

use std::cell::RefCell;
use std::sync::OnceLock;

use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

mod crumbs_box;

use crumbs_box::CrumbsBox;

/// Upper bound on entries scanned for Tab completion.
const COMPLETION_SCAN_LIMIT: usize = 5000;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/address-bar.ui")]
    pub struct AddressBar {
        #[template_child]
        pub(super) stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) crumbs_frame: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) crumbs: TemplateChild<CrumbsBox>,
        #[template_child]
        pub(super) entry: TemplateChild<gtk::Entry>,
        pub(super) location: RefCell<Option<gio::File>>,
        /// Files and labels of the current segments, root first.
        pub(super) segments: RefCell<Vec<(gio::File, String)>>,
        pub(super) completion_task: RefCell<Option<glib::JoinHandle<()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AddressBar {
        const NAME: &'static str = "LiaAddressBar";
        type Type = super::AddressBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            CrumbsBox::ensure_type();
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lia-address-bar");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AddressBar {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("navigate")
                        .param_types([gio::File::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for AddressBar {}
}

glib::wrapper! {
    pub struct AddressBar(ObjectSubclass<imp::AddressBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AddressBar {
    fn setup(&self) {
        let imp = self.imp();

        // Clicking the empty part of the bar switches to the text entry.
        // Breadcrumb buttons claim their own clicks, so this only fires on
        // the background.
        let click = gtk::GestureClick::new();
        click.connect_released(clone!(
            #[weak(rename_to = bar)]
            self,
            move |_, _, _, _| bar.start_editing()
        ));
        imp.crumbs_frame.add_controller(click);

        // The overflow ("…") button lists the segments that don't fit.
        imp.crumbs.overflow_button().set_create_popup_func(clone!(
            #[weak(rename_to = bar)]
            self,
            move |button| bar.fill_overflow_popover(button)
        ));

        imp.entry.connect_activate(clone!(
            #[weak(rename_to = bar)]
            self,
            move |entry| {
                let file = bar.parse_input(&entry.text());
                bar.stop_editing();
                bar.emit_by_name::<()>("navigate", &[&file]);
            }
        ));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = bar)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| match key {
                gdk::Key::Escape => {
                    bar.stop_editing();
                    glib::Propagation::Stop
                }
                gdk::Key::Tab if modifiers.is_empty() => {
                    bar.complete();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        ));
        imp.entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.stop_editing()
        ));
        imp.entry.add_controller(focus);
    }

    // ── Public API ─────────────────────────────────────────────────────────

    pub fn set_location(&self, location: &gio::File) {
        self.imp().location.replace(Some(location.clone()));
        self.rebuild_crumbs(location);
        if self.is_editing() {
            self.imp().entry.set_text(&location.parse_name());
        }
    }

    pub fn is_editing(&self) -> bool {
        self.imp().stack.visible_child_name().as_deref() == Some("entry")
    }

    /// Switches to the text entry with the full path selected.
    pub fn start_editing(&self) {
        let imp = self.imp();
        let text = imp
            .location
            .borrow()
            .as_ref()
            .map(|l| l.parse_name().to_string())
            .unwrap_or_default();
        imp.entry.set_text(&text);
        imp.stack.set_visible_child_name("entry");
        imp.entry.grab_focus();
        imp.entry.select_region(0, -1);
    }

    pub fn stop_editing(&self) {
        let imp = self.imp();
        if let Some(task) = imp.completion_task.take() {
            task.abort();
        }
        imp.stack.set_visible_child_name("crumbs");
    }

    pub fn connect_navigate<F: Fn(&Self, &gio::File) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "navigate",
            false,
            glib::closure_local!(move |bar: &Self, file: &gio::File| f(bar, file)),
        )
    }

    // ── Breadcrumbs ────────────────────────────────────────────────────────

    fn rebuild_crumbs(&self, location: &gio::File) {
        let segments = segments(location);
        self.imp().segments.replace(
            segments
                .iter()
                .map(|s| (s.file.clone(), s.tooltip.clone()))
                .collect(),
        );
        let last = segments.len().saturating_sub(1);
        let widgets = segments
            .into_iter()
            .enumerate()
            .map(|(i, segment)| {
                // Each segment is "› [button]"; the first has no separator.
                let row = gtk::Box::builder().css_classes(["crumb-segment"]).build();
                if i > 0 {
                    row.append(
                        &gtk::Image::builder()
                            .icon_name("go-next-symbolic")
                            .pixel_size(12)
                            .css_classes(["crumb-separator"])
                            .build(),
                    );
                }
                row.append(&self.crumb_button(segment, i == last));
                row.upcast()
            })
            .collect();
        self.imp().crumbs.set_segments(widgets);
    }

    /// Lists the hidden leading segments, nearest parent first.
    fn fill_overflow_popover(&self, button: &gtk::MenuButton) {
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        let popover = gtk::Popover::builder()
            .child(&list)
            .has_arrow(false)
            .css_classes(["menu"])
            .build();
        let hidden = self.imp().crumbs.first_visible();
        let segments = self.imp().segments.borrow();
        for (file, label) in segments[..hidden.min(segments.len())].iter().rev() {
            let item = gtk::Button::builder()
                .child(
                    &gtk::Label::builder()
                        .label(label.as_str())
                        .xalign(0.0)
                        .build(),
                )
                .css_classes(["flat"])
                .build();
            let file = file.clone();
            item.connect_clicked(clone!(
                #[weak(rename_to = bar)]
                self,
                #[weak]
                popover,
                move |_| {
                    popover.popdown();
                    bar.emit_by_name::<()>("navigate", &[&file]);
                }
            ));
            list.append(&item);
        }
        button.set_popover(Some(&popover));
    }

    fn crumb_button(&self, segment: Segment, current: bool) -> gtk::Button {
        let content = gtk::Box::builder().spacing(6).build();
        if let Some(icon) = segment.icon {
            content.append(&gtk::Image::from_icon_name(icon));
        }
        if !segment.label.is_empty() {
            // Only the current segment may shrink (when nothing else fits).
            let label = gtk::Label::new(Some(&segment.label));
            if current {
                label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            }
            content.append(&label);
        }
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["flat", "crumb"])
            .tooltip_text(segment.file.parse_name())
            .build();
        if current {
            button.add_css_class("current");
        }
        button.update_property(&[gtk::accessible::Property::Label(
            if segment.label.is_empty() {
                segment.tooltip.as_str()
            } else {
                segment.label.as_str()
            },
        )]);
        let file = segment.file;
        button.connect_clicked(clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.emit_by_name::<()>("navigate", &[&file])
        ));
        button
    }

    // ── Text entry ─────────────────────────────────────────────────────────

    /// Turns typed text into a location: absolute paths, URIs, `~`, and paths
    /// relative to the current folder.
    fn parse_input(&self, text: &str) -> gio::File {
        let text = text.trim();
        let current = self.imp().location.borrow().clone();
        parse_location(text, current.as_ref())
    }

    /// Tab completion: completes the last path segment to the longest prefix
    /// shared by all matching sub-folders.
    fn complete(&self) {
        let imp = self.imp();
        let text = imp.entry.text().to_string();
        let (dir_part, prefix) = match text.rfind('/') {
            Some(i) => (text[..=i].to_string(), text[i + 1..].to_string()),
            None => (String::new(), text.clone()),
        };
        let dir = if dir_part.is_empty() {
            match imp.location.borrow().clone() {
                Some(location) => location,
                None => return,
            }
        } else {
            self.parse_input(&dir_part)
        };

        if let Some(task) = imp.completion_task.take() {
            task.abort();
        }
        let task = glib::spawn_future_local(clone!(
            #[weak(rename_to = bar)]
            self,
            async move {
                let Ok(names) = child_folders(&dir, &prefix).await else {
                    return;
                };
                let Some(completed) = complete_prefix(&prefix, &names) else {
                    bar.error_bell();
                    return;
                };
                let entry = &bar.imp().entry;
                // Ignore results if the user kept typing meanwhile.
                if entry.text() != text {
                    return;
                }
                let suffix = if names.len() == 1 { "/" } else { "" };
                entry.set_text(&format!("{dir_part}{completed}{suffix}"));
                entry.set_position(-1);
            }
        ));
        imp.completion_task.replace(Some(task));
    }
}

/// One breadcrumb segment.
struct Segment {
    file: gio::File,
    label: String,
    tooltip: String,
    icon: Option<&'static str>,
}

/// The chain of folders from the root (or Home) down to `location`.
fn segments(location: &gio::File) -> Vec<Segment> {
    let home = gio::File::for_path(glib::home_dir());
    let mut chain = Vec::new();
    let mut current = Some(location.clone());
    while let Some(file) = current {
        if file.equal(&home) {
            chain.push(Segment {
                label: gettext("Home"),
                tooltip: gettext("Home"),
                icon: Some("user-home-symbolic"),
                file,
            });
            break;
        }
        let parent = file.parent();
        let segment = if parent.is_some() {
            let label = file
                .basename()
                .map(|b| b.to_string_lossy().into_owned())
                .unwrap_or_default();
            Segment {
                tooltip: label.clone(),
                label,
                icon: None,
                file,
            }
        } else {
            root_segment(file)
        };
        chain.push(segment);
        current = parent;
    }
    chain.reverse();
    chain
}

fn root_segment(file: gio::File) -> Segment {
    let (label, icon) = match file.uri_scheme().as_deref() {
        Some("file") => (String::new(), "drive-harddisk-symbolic"),
        Some("trash") => (gettext("Trash"), "user-trash-symbolic"),
        Some("recent") => (gettext("Recent"), "document-open-recent-symbolic"),
        Some("network") => (gettext("Network"), "network-workgroup-symbolic"),
        _ => (
            file.parse_name().trim_end_matches('/').to_string(),
            "folder-remote-symbolic",
        ),
    };
    let tooltip = if label.is_empty() {
        gettext("Computer")
    } else {
        label.clone()
    };
    Segment {
        file,
        label,
        tooltip,
        icon: Some(icon),
    }
}

/// Parses user input into a `gio::File`.
pub fn parse_location(text: &str, current: Option<&gio::File>) -> gio::File {
    if text == "~" {
        return gio::File::for_path(glib::home_dir());
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return gio::File::for_path(glib::home_dir().join(rest));
    }
    let is_absolute = text.starts_with('/') || text.contains("://") || text.ends_with(":///");
    match current {
        Some(current) if !is_absolute && !text.is_empty() => current.resolve_relative_path(text),
        _ if text.is_empty() => current
            .cloned()
            .unwrap_or_else(|| gio::File::for_path(glib::home_dir())),
        _ => gio::File::for_parse_name(text),
    }
}

/// Names of sub-folders of `dir` starting with `prefix` (case-insensitive).
async fn child_folders(dir: &gio::File, prefix: &str) -> Result<Vec<String>, glib::Error> {
    let enumerator = dir
        .enumerate_children_future(
            "standard::name,standard::type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await?;
    let prefix = prefix.to_lowercase();
    let mut names = Vec::new();
    let mut scanned = 0;
    loop {
        let infos = enumerator
            .next_files_future(500, glib::Priority::DEFAULT)
            .await?;
        if infos.is_empty() || scanned > COMPLETION_SCAN_LIMIT {
            break;
        }
        scanned += infos.len();
        names.extend(
            infos
                .into_iter()
                .filter(|info| info.file_type() == gio::FileType::Directory)
                .map(|info| info.name().to_string_lossy().into_owned())
                .filter(|name| name.to_lowercase().starts_with(&prefix)),
        );
    }
    Ok(names)
}

/// The longest completion of `prefix` shared by all `names`, or `None` if
/// nothing matches.
pub fn complete_prefix(prefix: &str, names: &[String]) -> Option<String> {
    let first = names.first()?;
    let mut common: Vec<char> = first.chars().collect();
    for name in &names[1..] {
        let len = common
            .iter()
            .zip(name.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        common.truncate(len);
    }
    let common: String = common.into_iter().collect();
    // Keep what was typed if case differences shortened the common part.
    Some(if common.chars().count() < prefix.chars().count() {
        prefix.to_string()
    } else {
        common
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn completes_to_common_prefix() {
        assert_eq!(
            complete_prefix("Do", &names(&["Documents"])).as_deref(),
            Some("Documents")
        );
        assert_eq!(
            complete_prefix("D", &names(&["Documents", "Downloads"])).as_deref(),
            Some("Do")
        );
        assert_eq!(complete_prefix("x", &names(&[])), None);
    }

    #[test]
    fn keeps_typed_text_on_case_mismatch() {
        assert_eq!(
            complete_prefix("doc", &names(&["Documents", "docs"])).as_deref(),
            Some("doc")
        );
    }

    #[test]
    fn parses_locations() {
        let home = glib::home_dir();
        assert_eq!(parse_location("~", None).path().unwrap(), home);
        assert_eq!(
            parse_location("~/Music", None).path().unwrap(),
            home.join("Music")
        );
        assert_eq!(
            parse_location("/usr/share", None).path().unwrap(),
            std::path::PathBuf::from("/usr/share")
        );
        let current = gio::File::for_path("/usr");
        assert_eq!(
            parse_location("share", Some(&current)).path().unwrap(),
            std::path::PathBuf::from("/usr/share")
        );
        assert_eq!(
            parse_location("file:///usr/share", None).path().unwrap(),
            std::path::PathBuf::from("/usr/share")
        );
    }
}
