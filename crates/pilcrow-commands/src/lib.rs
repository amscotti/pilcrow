//! Typed actions, keymaps and menus for Pilcrow.
//!
//! The editor and the shell install handlers; this crate owns the names and
//! the one platform mapping so the two halves cannot fork namespaces.

pub mod app_actions;
pub mod editor_actions;
pub mod keymap;
pub mod menus;

pub use app_actions::*;
pub use editor_actions::*;
pub use keymap::{Platform, install_app_keybindings_for, install_editor_keybindings_for};
pub use menus::{MenuAppearance, MenuFocus, MenuMeasure, MenuOptions, menus};

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::MenuItem;

    #[test]
    fn every_menu_action_has_a_typed_handler_namespace() {
        // Compile-time proof that the menu model can name both shell and
        // editor actions without importing either implementation crate.
        let _ = MenuItem::action("Find", Find);
        let _ = MenuItem::action("Save", Save);
        let _ = MenuItem::action("Toggle Task", ToggleTask);
        let options = MenuOptions::default().with_appearance(MenuAppearance::Light);
        let built = menus(&options);
        assert_eq!(built.len(), 6);
    }
}
