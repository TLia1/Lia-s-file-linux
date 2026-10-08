//! `CrumbsBox`: lays out breadcrumb segments in a row. When they don't fit,
//! the leading segments are hidden and an overflow ("…") button is shown in
//! their place; the current (last) segment is always visible and shrinks
//! last. Right-to-left languages get the mirrored layout.

use std::cell::{Cell, OnceCell, RefCell};

use gettextrs::gettext;
use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CrumbsBox {
        pub(super) overflow: OnceCell<gtk::MenuButton>,
        pub(super) segments: RefCell<Vec<gtk::Widget>>,
        pub(super) first_visible: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CrumbsBox {
        const NAME: &'static str = "LiaCrumbsBox";
        type Type = super::CrumbsBox;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("breadcrumbs");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }
    }

    impl ObjectImpl for CrumbsBox {
        fn constructed(&self) {
            self.parent_constructed();
            let overflow = gtk::MenuButton::builder()
                .icon_name("view-more-horizontal-symbolic")
                .css_classes(["flat", "crumb"])
                .tooltip_text(gettext("Show Parent Folders"))
                .build();
            overflow.set_parent(&*self.obj());
            self.overflow.set(overflow).unwrap();
        }

        fn dispose(&self) {
            for segment in self.segments.take() {
                segment.unparent();
            }
            if let Some(overflow) = self.overflow.get() {
                overflow.unparent();
            }
        }
    }

    impl WidgetImpl for CrumbsBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let segments = self.segments.borrow();
            let overflow = self.overflow.get().unwrap();
            if orientation == gtk::Orientation::Horizontal {
                let natural: i32 = segments.iter().map(|s| s.measure(orientation, -1).1).sum();
                let minimum = match segments.last() {
                    Some(last) if segments.len() > 1 => {
                        overflow.measure(orientation, -1).1 + last.measure(orientation, -1).0
                    }
                    Some(last) => last.measure(orientation, -1).0,
                    None => 0,
                };
                (minimum, natural.max(minimum), -1, -1)
            } else {
                let (mut minimum, mut natural, _, _) = overflow.measure(orientation, -1);
                for segment in segments.iter() {
                    let (min, nat, _, _) = segment.measure(orientation, -1);
                    minimum = minimum.max(min);
                    natural = natural.max(nat);
                }
                (minimum, natural, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let segments = self.segments.borrow();
            let overflow = self.overflow.get().unwrap();
            let naturals: Vec<i32> = segments
                .iter()
                .map(|s| s.measure(gtk::Orientation::Horizontal, -1).1)
                .collect();
            let overflow_width = overflow.measure(gtk::Orientation::Horizontal, -1).1;

            // Show as many trailing segments as fit; the last always shows.
            let mut first = 0;
            if naturals.iter().sum::<i32>() > width && !segments.is_empty() {
                first = segments.len() - 1;
                let mut used = overflow_width + naturals[first];
                while first > 0 && used + naturals[first - 1] <= width {
                    first -= 1;
                    used += naturals[first];
                }
            }
            self.first_visible.set(first);

            // Positions are computed left to right, then mirrored for
            // right-to-left languages (the root on the right).
            let rtl = self.obj().direction() == gtk::TextDirection::Rtl;
            let place = |widget: &gtk::Widget, x: i32, w: i32| {
                let x = if rtl { width - x - w } else { x };
                widget.size_allocate(&gtk::Allocation::new(x, 0, w, height), -1);
            };
            let mut x = 0;
            overflow.set_child_visible(first > 0);
            if first > 0 {
                place(overflow.upcast_ref(), 0, overflow_width);
                x = overflow_width;
            }
            let last = segments.len().saturating_sub(1);
            for (i, segment) in segments.iter().enumerate() {
                if i < first {
                    segment.set_child_visible(false);
                    continue;
                }
                segment.set_child_visible(true);
                let mut w = naturals[i];
                if i == last {
                    let minimum = segment.measure(gtk::Orientation::Horizontal, -1).0;
                    w = w.min(width - x).max(minimum);
                }
                place(segment, x, w);
                x += w;
            }
        }
    }
}

glib::wrapper! {
    pub struct CrumbsBox(ObjectSubclass<imp::CrumbsBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CrumbsBox {
    /// Replaces the segments (in order, root first).
    pub fn set_segments(&self, segments: Vec<gtk::Widget>) {
        let imp = self.imp();
        for old in imp.segments.take() {
            old.unparent();
        }
        for segment in &segments {
            segment.insert_before(self, None::<&gtk::Widget>);
        }
        imp.segments.replace(segments);
        imp.first_visible.set(0);
        self.queue_resize();
    }

    /// Index of the first segment shown; segments before it are in the
    /// overflow menu.
    pub fn first_visible(&self) -> usize {
        self.imp().first_visible.get()
    }

    pub fn overflow_button(&self) -> &gtk::MenuButton {
        self.imp().overflow.get().unwrap()
    }
}
