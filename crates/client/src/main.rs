//! Corddis arayüzü. Windows'ta konsol penceresi açmaz.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = corddis_client::gui::run() {
        corddis_client::platform::error_box("Corddis başlatılamadı", &format!("{e}"));
    }
}
