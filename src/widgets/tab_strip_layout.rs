//! `TabStripLayout`: lays out a tab bar and the new-tab button after it like
//! a browser's tab strip. The tab bar is as wide as its tabs, up to the
//! space left by the button, and the button follows the last tab.
//!
//! A plain box can't do it: the tab bar is built to scroll, so it only
//! reports a token natural width whatever its tabs need.

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TabStripLayout;

    #[glib::object_subclass]
    impl ObjectSubclass for TabStripLayout {
        const NAME: &'static str = "LiaTabStripLayout";
        type Type = super::TabStripLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for TabStripLayout {}

    impl LayoutManagerImpl for TabStripLayout {
        fn measure(
            &self,
            strip: &gtk::Widget,
            orientation: gtk::Orientation,
            _for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let (mut min, mut nat) = (0, 0);
            let mut child = strip.first_child();
            while let Some(widget) = child {
                let (child_min, mut child_nat, ..) = widget.measure(orientation, -1);
                if orientation == gtk::Orientation::Horizontal {
                    if widget.is::<adw::TabBar>() {
                        child_nat = tabs_width(&widget);
                    }
                    min += child_min;
                    nat += child_nat;
                } else {
                    min = min.max(child_min);
                    nat = nat.max(child_nat);
                }
                child = widget.next_sibling();
            }
            (min, nat, -1, -1)
        }

        fn allocate(&self, strip: &gtk::Widget, width: i32, height: i32, baseline: i32) {
            let Some(tab_bar) = strip.first_child() else {
                return;
            };
            let button = tab_bar.next_sibling();
            let button_width = button.as_ref().map_or(0, |button| {
                button.measure(gtk::Orientation::Horizontal, -1).1
            });
            let tab_bar_min = tab_bar.measure(gtk::Orientation::Horizontal, -1).0;
            let tabs = tabs_width(&tab_bar)
                .min(width - button_width)
                .max(tab_bar_min);
            tab_bar.size_allocate(&gtk::Allocation::new(0, 0, tabs, height), baseline);

            if let Some(button) = button {
                let button_height = button.measure(gtk::Orientation::Vertical, button_width).1;
                button.size_allocate(
                    &gtk::Allocation::new(
                        tabs,
                        (height - button_height) / 2,
                        button_width,
                        button_height,
                    ),
                    -1,
                );
            }
        }
    }
}

glib::wrapper! {
    pub struct TabStripLayout(ObjectSubclass<imp::TabStripLayout>)
        @extends gtk::LayoutManager;
}

impl Default for TabStripLayout {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// The width `tab_bar` needs to show all its tabs without scrolling.
fn tabs_width(tab_bar: &gtk::Widget) -> i32 {
    // Its own natural width covers the padding around the tab boxes.
    let mut width = tab_bar.measure(gtk::Orientation::Horizontal, -1).1;
    let mut stack = vec![tab_bar.clone()];
    while let Some(widget) = stack.pop() {
        let mut child = widget.first_child();
        while let Some(c) = child {
            if c.css_name() == "tabbox" {
                let nat = c.measure(gtk::Orientation::Horizontal, -1).1;
                width += nat;
            } else {
                stack.push(c.clone());
            }
            child = c.next_sibling();
        }
    }
    width
}
