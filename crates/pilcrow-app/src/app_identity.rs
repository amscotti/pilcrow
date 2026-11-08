//! What the operating system calls this application, and what it looks like.
//!
//! The executable is `pilcrow`, but the product name is `Pilcrow`. macOS uses
//! the process name for the application menu and native About panel, and it
//! will fall back to a generic icon for an unbundled development build. Set
//! both once at startup from the icon that ships with the repository.

#[cfg(target_os = "macos")]
mod platform {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSImage, NSMenu, NSMenuItem};
    use objc2_foundation::{NSData, NSProcessInfo, NSString};

    const ICON_PNG: &[u8] = include_bytes!("../../../macos/icon-1024.png");

    /// Tag for the standard AppKit items we add after GPUI installs its menu,
    /// so a menu rebuild can remove the previous copies before inserting again.
    const STANDARD_MENU_TAG: isize = 0x5049_4c43; // "PILC"

    /// Called before GPUI creates the application menu. `NSProcessInfo` does
    /// not need the application object yet.
    pub fn prepare() {
        NSProcessInfo::processInfo().setProcessName(&NSString::from_str("Pilcrow"));
    }

    pub fn apply() {
        prepare();
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let data =
            unsafe { NSData::dataWithBytes_length(ICON_PNG.as_ptr().cast(), ICON_PNG.len()) };
        if let Some(image) = NSImage::initWithData(mtm.alloc(), &data) {
            let app = NSApplication::sharedApplication(mtm);
            unsafe {
                app.setApplicationIconImage(Some(&image));
            }
        }
    }

    /// macOS titles the first menu from the process, which can be cached
    /// before the GPUI menu bar is installed. Set the visible app-menu item
    /// explicitly after every menu rebuild.
    pub fn set_app_menu_title(title: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(menu) = app.mainMenu() else {
            return;
        };
        let Some(item) = menu.itemAtIndex(0) else {
            return;
        };
        item.setTitle(&NSString::from_str(title));
        let Some(submenu) = item.submenu() else {
            return;
        };
        install_standard_items(&submenu, mtm);
    }

    /// GPUI's menu model has no Hide/Hide Others/Show All actions, but the
    /// macOS app menu is expected to carry them. Insert the standard responder
    /// actions after GPUI builds its menu; the tag makes the next rebuild
    /// replace rather than duplicate them.
    fn install_standard_items(menu: &NSMenu, mtm: MainThreadMarker) {
        let mut index = menu.numberOfItems();
        while index > 0 {
            index -= 1;
            if menu
                .itemAtIndex(index)
                .is_some_and(|item| item.tag() == STANDARD_MENU_TAG)
            {
                menu.removeItemAtIndex(index);
            }
        }

        let count = menu.numberOfItems();
        let mut insert_at = count.saturating_sub(1);
        let has_leading_separator = insert_at > 0
            && menu
                .itemAtIndex(insert_at - 1)
                .is_some_and(|item| item.isSeparatorItem());
        if !has_leading_separator {
            let separator = NSMenuItem::separatorItem(mtm);
            separator.setTag(STANDARD_MENU_TAG);
            menu.insertItem_atIndex(&separator, insert_at);
            insert_at += 1;
        }

        let hide = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Hide Pilcrow"),
                Some(objc2::sel!(hide:)),
                &NSString::from_str("h"),
            )
        };
        hide.setTag(STANDARD_MENU_TAG);
        hide.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        menu.insertItem_atIndex(&hide, insert_at);
        insert_at += 1;

        let hide_others = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Hide Others"),
                Some(objc2::sel!(hideOtherApplications:)),
                &NSString::from_str("h"),
            )
        };
        hide_others.setTag(STANDARD_MENU_TAG);
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        menu.insertItem_atIndex(&hide_others, insert_at);
        insert_at += 1;

        let show_all = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str("Show All"),
                Some(objc2::sel!(unhideAllApplications:)),
                &NSString::from_str(""),
            )
        };
        show_all.setTag(STANDARD_MENU_TAG);
        show_all.setKeyEquivalentModifierMask(NSEventModifierFlags(0));
        menu.insertItem_atIndex(&show_all, insert_at);
        insert_at += 1;

        let separator = NSMenuItem::separatorItem(mtm);
        separator.setTag(STANDARD_MENU_TAG);
        menu.insertItem_atIndex(&separator, insert_at);
    }

    pub fn show_about() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanel(None);
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn prepare() {}
    pub fn apply() {}
    pub fn set_app_menu_title(_title: &str) {}
    pub fn show_about() {}
}

pub fn prepare() {
    platform::prepare();
}

pub fn apply() {
    platform::apply();
}

pub fn set_app_menu_title(title: &str) {
    platform::set_app_menu_title(title);
}

pub fn show_about() {
    platform::show_about();
}
