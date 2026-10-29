//! QODA - QO-100 DATV alıcı.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod decoder;
mod logo;
mod sdr;

use eframe::egui;

/// Pencere simgesinin piksel boyutu.
const ICON_SIZE: usize = 64;

fn main() -> eframe::Result {
    let icon = egui::IconData { rgba: logo::rgba(ICON_SIZE), width: ICON_SIZE as u32, height: ICON_SIZE as u32 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("QODA - QO-100 DATV")
            .with_inner_size([1360.0, 860.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native("QODA", options, Box::new(|cc| Ok(Box::new(app::QodaApp::new(cc)))))
}
