//! QODA - QO-100 DATV alıcı.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod sdr;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("QODA - QO-100 DATV")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native("QODA", options, Box::new(|cc| Ok(Box::new(app::QodaApp::new(cc)))))
}
