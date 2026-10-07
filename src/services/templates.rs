//! "New ▸" entries from the user's Templates folder (`~/Templates`): one
//! menu item per file, labelled without the extension, with sub-folders as
//! sub-menus. The menu is shared by all windows and kept up to date.

use std::cell::OnceCell;

use gtk::{gio, glib, glib::clone, prelude::*};

use crate::services::file_ops::naming;

/// Deeper template folders are ignored.
const MAX_DEPTH: u32 = 3;

thread_local! {
    static MENU: OnceCell<(gio::Menu, Option<gio::FileMonitor>)> = const { OnceCell::new() };
}

/// The templates folder, unless it's missing or the home folder itself
/// (which XDG uses to mean "disabled").
pub fn templates_dir() -> Option<gio::File> {
    let dir = glib::user_special_dir(glib::UserDirectory::Templates)?;
    (dir != glib::home_dir()).then(|| gio::File::for_path(dir))
}

/// The templates section of the "New" menus. Items activate
/// `win.new-from-template` with the template's URI.
pub fn templates_menu() -> gio::Menu {
    MENU.with(|cell| {
        cell.get_or_init(|| {
            let menu = gio::Menu::new();
            let Some(dir) = templates_dir() else {
                return (menu, None);
            };
            let monitor = dir
                .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
                .ok();
            if let Some(monitor) = &monitor {
                monitor.connect_changed(clone!(
                    #[weak]
                    menu,
                    #[strong]
                    dir,
                    move |_, _, _, _| reload(&menu, &dir)
                ));
            }
            reload(&menu, &dir);
            (menu, monitor)
        })
        .0
        .clone()
    })
}

fn reload(menu: &gio::Menu, dir: &gio::File) {
    let menu = menu.clone();
    let dir = dir.clone();
    glib::spawn_future_local(async move {
        let fresh = build(&dir, 0).await;
        menu.remove_all();
        for i in 0..fresh.n_items() {
            menu.append_item(&gio::MenuItem::from_model(&fresh, i));
        }
    });
}

async fn build(dir: &gio::File, depth: u32) -> gio::Menu {
    let menu = gio::Menu::new();
    let Ok(enumerator) = dir
        .enumerate_children_future(
            "standard::name,standard::display-name,standard::type,standard::is-hidden,\
             standard::is-backup",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::LOW,
        )
        .await
    else {
        return menu;
    };
    let mut entries = Vec::new();
    while let Ok(infos) = enumerator.next_files_future(100, glib::Priority::LOW).await {
        if infos.is_empty() {
            break;
        }
        entries.extend(
            infos
                .into_iter()
                .filter(|i| !i.is_hidden() && !i.is_backup()),
        );
    }
    entries.sort_by_key(|info| info.display_name().to_lowercase());

    for info in entries {
        let file = dir.child(info.name());
        let name = info.display_name();
        if info.file_type() == gio::FileType::Directory {
            if depth + 1 < MAX_DEPTH {
                let submenu = Box::pin(build(&file, depth + 1)).await;
                if submenu.n_items() > 0 {
                    menu.append_submenu(Some(&name), &submenu);
                }
            }
            continue;
        }
        let label = naming::split_extension(&name, false).0.replace('_', "__");
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_action_and_target_value(
            Some("win.new-from-template"),
            Some(&file.uri().to_variant()),
        );
        menu.append_item(&item);
    }
    menu
}
