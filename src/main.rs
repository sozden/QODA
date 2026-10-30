//! QODA - QO-100 DATV receiver.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod decoder;
mod i18n;
mod iio;
mod logo;
mod sdr;
mod settings;

use eframe::egui;

/// Pixel size of the window icon.
const ICON_SIZE: usize = 64;

/// In the packaged build the SoapySDR modules are in the `lib/SoapySDR/modules0.8`
/// folder next to the program. SoapySDR searches for modules relative to its root,
/// so the root is set to the program's folder; this also avoids clashing with the
/// modules of an installed PothosSDR. Left alone if the folder is missing (development).
fn use_bundled_soapy_modules() {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
    if dir.join("lib").join("SoapySDR").join("modules0.8").is_dir() {
        // Set while there are no other threads yet.
        unsafe { std::env::set_var("SOAPY_SDR_ROOT", &dir) };
    }
}

/// Under WSL, the WSLg Wayland compositor drops the connection when the window is resized,
/// which closes the program. Its X11 server (XWayland) does not, so Wayland is skipped there.
fn prefer_x11_under_wsl() {
    if cfg!(target_os = "linux") && std::env::var_os("WSL_DISTRO_NAME").is_some() && std::env::var_os("DISPLAY").is_some() {
        // Set while there are no other threads yet.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }
}

/// `qoda --diagnose`: checks the installation and writes the result to standard output
/// (e.g. `qoda.exe --diagnose > diag.txt`).
fn diagnose() {
    println!("QODA {}", env!("CARGO_PKG_VERSION"));
    println!("SOAPY_SDR_ROOT = {:?}", std::env::var_os("SOAPY_SDR_ROOT"));
    let (paths, modules) = sdr::soapy_modules();
    println!("SoapySDR search paths: {paths:?}");
    for (path, result) in modules {
        println!("  module {path}: {result}");
    }
    let devices = sdr::list_devices();
    println!("SoapySDR devices: {}", devices.len());
    for d in devices {
        println!("  {:?} | {} | {}", d.kind, d.label, d.args);
    }
    println!("libiio: {:?}", iio::library_status());
    for tool in ["ffmpeg", "ffplay"] {
        println!("{tool}: {:?}", decoder::find_ffmpeg_tool(tool));
    }
}

fn main() -> eframe::Result {
    use_bundled_soapy_modules();
    prefer_x11_under_wsl();
    if std::env::args().any(|a| a == "--diagnose") {
        diagnose();
        return Ok(());
    }
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
