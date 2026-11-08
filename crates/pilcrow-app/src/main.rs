#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    pilcrow_app::app_identity::prepare();
    pilcrow_app::run();
}
