//! QODA - QO-100 DATV alıcı.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod decoder;
mod i18n;
mod iio;
mod logo;
mod sdr;
mod settings;

use eframe::egui;

/// Pencere simgesinin piksel boyutu.
const ICON_SIZE: usize = 64;

/// Paketlenmiş sürümde SoapySDR modülleri programın yanındaki
/// `lib/SoapySDR/modules0.8` klasöründedir. SoapySDR modülleri kök klasöre göre
/// aradığı için kök, programın klasörü yapılır; böylece kurulu bir PothosSDR'ın
/// modülleriyle de çakışmaz. Klasör yoksa (geliştirme ortamı) dokunulmaz.
fn use_bundled_soapy_modules() {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
    if dir.join("lib").join("SoapySDR").join("modules0.8").is_dir() {
        // Henüz başka iş parçacığı yokken ayarlanır.
        unsafe { std::env::set_var("SOAPY_SDR_ROOT", &dir) };
    }
}

/// `qoda --diagnose`: kurulumu denetler ve sonucu standart çıktıya yazar
/// (ör. `qoda.exe --diagnose > tani.txt`).
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
