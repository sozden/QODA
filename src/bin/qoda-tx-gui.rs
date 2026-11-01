//! QODA TX: a window for transmitting DVB-S2 on the QO-100 wideband uplink with an
//! ADALM-Pluto. The work is done by `qoda::transmit`, like the `qoda-tx` command line tool.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[path = "../logo.rs"]
mod logo;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke, vec2};
use qoda::dvbs2::tx::TxConfig;
use qoda::transmit::{self, AirSettings, Plan, Report, SYMBOL_RATES};
use qoda::ui::palette::*;
use qoda::ui::{apply_theme, card, dot, freq_tuner, metric};

const ROLLOFFS: [f32; 3] = [0.35, 0.25, 0.20];
/// MODCODs offered: QPSK and 8PSK.
const MODCODS: std::ops::RangeInclusive<u8> = 1..=17;

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    En,
    Tr,
}

#[derive(Clone, PartialEq)]
enum Source {
    /// Test card with this call sign as the service name.
    Pattern,
    /// An MPEG-TS file.
    File,
}

/// What the worker thread is doing, shared with the window.
#[derive(Default)]
struct Shared {
    /// Preparing the signal (FFmpeg, modulation).
    preparing: bool,
    /// The samples go out (or into the loopback).
    running: bool,
    elapsed: f64,
    achieved: f64,
    /// Lines for the log: (is a warning or error, text).
    log: Vec<(Color32, String)>,
}

struct Job {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    loopback: bool,
    sample_rate: f64,
    seconds: f64,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct TxApp {
    lang: Lang,
    uri: String,
    source: Source,
    call: String,
    file: String,
    downlink_mhz: f64,
    symbol_rate: f64,
    modcod: u8,
    short_frame: bool,
    pilots: bool,
    rolloff: f32,
    offset_khz: f64,
    level: f64,
    attenuation: f64,
    seconds: f64,
    job: Option<Job>,
    /// Log of finished jobs, kept when a new one starts.
    log: Vec<(Color32, String)>,
}

impl TxApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let mut app = Self {
            lang: Lang::En,
            uri: "ip:192.168.2.1".into(),
            source: Source::Pattern,
            call: String::new(),
            file: String::new(),
            downlink_mhz: 10_496.75,
            symbol_rate: 125e3,
            modcod: 4,
            short_frame: false,
            pilots: true,
            rolloff: 0.20,
            offset_khz: 0.0,
            level: transmit::DEFAULT_LEVEL_DBFS,
            attenuation: transmit::DEFAULT_ATTENUATION_DB,
            seconds: transmit::DEFAULT_SECONDS,
            job: None,
            log: Vec::new(),
        };
        app.load();
        app
    }

    /// English or Turkish text.
    fn tr(&self, en: &'static str, tr: &'static str) -> &'static str {
        if self.lang == Lang::Tr { tr } else { en }
    }

    fn plan(&self) -> Plan {
        Plan {
            symbol_rate: self.symbol_rate,
            cfg: TxConfig { modcod: self.modcod, short_frame: self.short_frame, pilots: self.pilots, rolloff: self.rolloff },
            downlink_hz: self.downlink_mhz * 1e6,
            offset_hz: self.offset_khz * 1e3,
            shift_hz: None,
            sps: None,
        }
    }

    fn air(&self, loopback: bool) -> AirSettings {
        AirSettings { level_dbfs: self.level, attenuation_db: self.attenuation, seconds: self.seconds, loopback }
    }

    fn busy(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.thread.as_ref().is_some_and(|t| !t.is_finished()))
    }

    /// What stands in the way of starting, if anything.
    fn problem(&self) -> Option<String> {
        let plan = self.plan();
        if plan.capacity().is_none() {
            return Some(self.tr("This code rate has no short frame.", "Bu kod oranının kısa çerçevesi yok.").into());
        }
        if let Err(e) = plan.check_band() {
            return Some(e);
        }
        if let Err(e) = self.air(false).check() {
            return Some(e);
        }
        match self.source {
            Source::Pattern if self.call.trim().is_empty() => {
                Some(self.tr("Enter your call sign for the test broadcast.", "Test yayını için çağrı işaretinizi girin.").into())
            }
            Source::File if !std::path::Path::new(self.file.trim()).is_file() => {
                Some(self.tr("Choose an MPEG-TS file.", "Bir MPEG-TS dosyası seçin.").into())
            }
            _ => None,
        }
    }

    /// Prepares the signal and transmits it (or runs the loopback test) on a worker thread.
    fn start(&mut self, ctx: &egui::Context, loopback: bool) {
        self.finish_job();
        self.save_settings();
        let plan = self.plan();
        let air = self.air(loopback);
        let uri = self.uri.trim().to_string();
        let source = match self.source {
            Source::Pattern => Err(self.call.trim().to_uppercase()),
            Source::File => Ok(PathBuf::from(self.file.trim())),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared { preparing: true, ..Default::default() }));
        let (stop_t, shared_t, ctx_t) = (stop.clone(), shared.clone(), ctx.clone());
        let thread = std::thread::spawn(move || {
            let log = |color: Color32, text: String| {
                shared_t.lock().unwrap().log.push((color, text));
                ctx_t.request_repaint();
            };
            let result = (|| -> Result<(), String> {
                let capacity = plan.capacity().ok_or("invalid MODCOD")?;
                let input = match source {
                    Err(call) => {
                        let (path, description) = transmit::make_pattern(&call, capacity)?;
                        log(MUTED, description);
                        path
                    }
                    Ok(path) => path,
                };
                let packets = transmit::read_ts(&input)?;
                let sps = plan.samples_per_symbol(true);
                let rate = plan.sample_rate(true);
                let samples = transmit::modulate(&packets, plan.cfg, sps)?;
                log(MUTED, format!("{:.1} s of signal at {:.3} MS/s, repeated while on air", samples.len() as f64 / rate, rate / 1e6));
                let prepared = transmit::prepare_for_air(&samples, air.level_dbfs, plan.shift(), rate);
                if stop_t.load(Ordering::Relaxed) {
                    return Ok(());
                }
                shared_t.lock().unwrap().preparing = false;
                let summary = transmit::transmit(&uri, &prepared, &plan, &air, &stop_t, &mut |r| match r {
                    Report::Info(text) => log(MUTED, text),
                    Report::Warning(text) => log(WARN, text),
                    Report::Started => {
                        shared_t.lock().unwrap().running = true;
                        ctx_t.request_repaint();
                    }
                    Report::Progress { elapsed, achieved } => {
                        let mut s = shared_t.lock().unwrap();
                        (s.elapsed, s.achieved) = (elapsed, achieved);
                        ctx_t.request_repaint();
                    }
                })?;
                log(GOOD, format!("Transmitter off after {:.1} s; sent {:.3} MS/s on average", summary.elapsed, summary.achieved / 1e6));
                Ok(())
            })();
            if let Err(e) = result {
                log(BAD, e);
            }
            let mut s = shared_t.lock().unwrap();
            (s.preparing, s.running) = (false, false);
            ctx_t.request_repaint();
        });
        self.job = Some(Job { stop, shared, loopback, sample_rate: plan.sample_rate(true), seconds: air.seconds, thread: Some(thread) });
    }

    /// Moves a finished job's log into the window's log.
    fn finish_job(&mut self) {
        if let Some(mut job) = self.job.take() {
            job.stop.store(true, Ordering::Relaxed);
            if let Some(t) = job.thread.take() {
                let _ = t.join();
            }
            self.log.append(&mut job.shared.lock().unwrap().log);
        }
    }

    fn stop(&self) {
        if let Some(job) = &self.job {
            job.stop.store(true, Ordering::Relaxed);
        }
    }

    // ---- Settings ----

    fn settings_path() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            PathBuf::from(std::env::var_os("APPDATA")?).join("QODA")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?
                .join("qoda")
        };
        Some(base.join("tx-settings.txt"))
    }

    fn load(&mut self) {
        let Some(text) = Self::settings_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let num = value.trim().parse::<f64>().ok();
            match key.trim() {
                "lang" => self.lang = if value.trim() == "tr" { Lang::Tr } else { Lang::En },
                "uri" => self.uri = value.trim().into(),
                "call" => self.call = value.trim().into(),
                "file" => self.file = value.trim().into(),
                "source" => self.source = if value.trim() == "file" { Source::File } else { Source::Pattern },
                "downlink_mhz" => self.downlink_mhz = num.unwrap_or(self.downlink_mhz),
                "symbol_rate" => self.symbol_rate = num.unwrap_or(self.symbol_rate),
                "modcod" => self.modcod = num.map_or(self.modcod, |v| v as u8),
                "short_frame" => self.short_frame = value.trim() == "1",
                "pilots" => self.pilots = value.trim() != "0",
                "rolloff" => self.rolloff = num.map_or(self.rolloff, |v| v as f32),
                "offset_khz" => self.offset_khz = num.unwrap_or(self.offset_khz),
                "level" => self.level = num.unwrap_or(self.level),
                "attenuation" => self.attenuation = num.unwrap_or(self.attenuation),
                "seconds" => self.seconds = num.unwrap_or(self.seconds),
                _ => {}
            }
        }
    }

    fn save_settings(&self) {
        let Some(path) = Self::settings_path() else { return };
        let text = format!(
            "lang={}\nuri={}\nsource={}\ncall={}\nfile={}\ndownlink_mhz={}\nsymbol_rate={}\nmodcod={}\nshort_frame={}\npilots={}\nrolloff={}\noffset_khz={}\nlevel={}\nattenuation={}\nseconds={}\n",
            if self.lang == Lang::Tr { "tr" } else { "en" },
            self.uri,
            if self.source == Source::File { "file" } else { "pattern" },
            self.call,
            self.file,
            self.downlink_mhz,
            self.symbol_rate,
            self.modcod,
            self.short_frame as u8,
            self.pilots as u8,
            self.rolloff,
            self.offset_khz,
            self.level,
            self.attenuation,
            self.seconds
        );
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }

    // ---- Panels ----

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("QODA TX").size(24.0).strong().color(ACCENT));
            ui.label(RichText::new(self.tr("DVB-S2 transmitter for the QO-100 wideband transponder", "QO-100 geniş bant transponderi için DVB-S2 verici")).size(12.0).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let before = self.lang;
                egui::ComboBox::from_id_salt("lang")
                    .width(96.0)
                    .selected_text(if self.lang == Lang::Tr { "Türkçe" } else { "English" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.lang, Lang::En, "English");
                        ui.selectable_value(&mut self.lang, Lang::Tr, "Türkçe");
                    });
                if self.lang != before {
                    self.save_settings();
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        let lang = self.lang;
        let tr = move |en: &'static str, tr: &'static str| if lang == Lang::Tr { tr } else { en };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                card(ui, tr("ADALM-PLUTO", "ADALM-PLUTO"), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.uri).hint_text("ip:192.168.2.1").desired_width(f32::INFINITY))
                        .on_hover_text(tr("Network address (ip:…) or usb:", "Ağ adresi (ip:…) ya da usb:"));
                });
                card(ui, tr("SOURCE", "KAYNAK"), |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.source, Source::Pattern, tr("Test broadcast", "Test yayını"));
                        ui.radio_value(&mut self.source, Source::File, tr("TS file", "TS dosyası"));
                    });
                    match self.source {
                        Source::Pattern => {
                            ui.add(egui::TextEdit::singleline(&mut self.call).hint_text(tr("Call sign", "Çağrı işareti")).desired_width(f32::INFINITY));
                            ui.label(RichText::new(tr("Test card, 1 kHz tone, the call sign as service name, at the channel's bit rate", "Test kartı, 1 kHz ton, servis adı çağrı işareti; kanalın bit hızında")).size(11.0).color(MUTED));
                        }
                        Source::File => {
                            ui.add(egui::TextEdit::singleline(&mut self.file).hint_text(tr("Path of a .ts file (or drop it here)", ".ts dosyasının yolu (ya da buraya bırakın)")).desired_width(f32::INFINITY));
                            ui.label(RichText::new(tr("Mux it at or below the TS capacity shown on the right", "Sağda görünen TS kapasitesinde ya da altında paketlenmiş olmalı")).size(11.0).color(MUTED));
                        }
                    }
                });
                card(ui, tr("SIGNAL", "SİNYAL"), |ui| {
                    egui::Grid::new("signal").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new(tr("Symbol rate", "Sembol hızı")).color(MUTED));
                        egui::ComboBox::from_id_salt("rs").selected_text(format!("{:.0} kS/s", self.symbol_rate / 1e3)).show_ui(ui, |ui| {
                            for r in SYMBOL_RATES {
                                ui.selectable_value(&mut self.symbol_rate, r, format!("{:.0} kS/s", r / 1e3));
                            }
                        });
                        ui.end_row();
                        ui.label(RichText::new("MODCOD").color(MUTED));
                        egui::ComboBox::from_id_salt("modcod").selected_text(transmit::modcod_name(self.modcod)).show_ui(ui, |ui| {
                            for m in MODCODS {
                                ui.selectable_value(&mut self.modcod, m, transmit::modcod_name(m));
                            }
                        });
                        ui.end_row();
                        ui.label(RichText::new(tr("Frame", "Çerçeve")).color(MUTED));
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.short_frame, tr("short", "kısa"));
                            ui.checkbox(&mut self.pilots, tr("pilots", "pilot"));
                        });
                        ui.end_row();
                        ui.label(RichText::new("Roll-off").color(MUTED));
                        egui::ComboBox::from_id_salt("rolloff").selected_text(format!("{:.2}", self.rolloff)).show_ui(ui, |ui| {
                            for r in ROLLOFFS {
                                ui.selectable_value(&mut self.rolloff, r, format!("{r:.2}"));
                            }
                        });
                        ui.end_row();
                    });
                });
                card(ui, tr("POWER AND TIME", "GÜÇ VE SÜRE"), |ui| {
                    egui::Grid::new("power").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new(tr("Level", "Seviye")).color(MUTED));
                        ui.add(egui::Slider::new(&mut self.level, transmit::LEVEL_RANGE_DBFS.0..=transmit::LEVEL_RANGE_DBFS.1).step_by(0.5).suffix(" dBFS"))
                            .on_hover_text(tr("RMS level of the samples; -4 dBFS is the most before the peaks clip", "Örneklerin RMS seviyesi; tepeler kırpılmadan en fazla -4 dBFS"));
                        ui.end_row();
                        ui.label(RichText::new(tr("Attenuation", "Zayıflatma")).color(MUTED));
                        ui.add(egui::Slider::new(&mut self.attenuation, 0.0..=40.0).step_by(0.25).suffix(" dB"))
                            .on_hover_text(tr("Pluto output attenuation: start high and lower it watching your downlink", "Pluto çıkış zayıflatması: yüksekten başlayıp downlink'i izleyerek düşürün"));
                        ui.end_row();
                        ui.label(RichText::new(tr("Time limit", "Süre sınırı")).color(MUTED));
                        ui.add(egui::Slider::new(&mut self.seconds, 10.0..=transmit::MAX_SECONDS).step_by(10.0).suffix(" s"));
                        ui.end_row();
                        ui.label(RichText::new(tr("Correction", "Düzeltme")).color(MUTED));
                        ui.add(egui::DragValue::new(&mut self.offset_khz).speed(0.1).range(-100.0..=100.0).suffix(" kHz"))
                            .on_hover_text(tr("Added to the transmit frequency for the Pluto's oscillator error", "Pluto'nun osilatör hatası için verici frekansına eklenir"));
                        ui.end_row();
                    });
                });
            });
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let plan = self.plan();
        let busy = self.busy();
        let (preparing, running, elapsed, achieved) = self.job.as_ref().map_or((false, false, 0.0, 0.0), |j| {
            let s = j.shared.lock().unwrap();
            (s.preparing, s.running, s.elapsed, s.achieved)
        });
        let loopback = self.job.as_ref().is_some_and(|j| j.loopback);
        let on_air = busy && running && !loopback;

        // Frequency and what follows from it.
        egui::Frame::new().fill(CARD).stroke(Stroke::new(1.0, BORDER)).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(self.tr("DOWNLINK FREQUENCY (signal centre)", "DOWNLINK FREKANSI (sinyal merkezi)")).size(11.0).color(MUTED));
            ui.add_enabled_ui(!busy, |ui| {
                freq_tuner(ui, &mut self.downlink_mhz, 34.0);
            });
            ui.add_space(6.0);
            egui::Grid::new("derived").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
                metric(ui, self.tr("Uplink", "Uplink"), &format!("{:.4} MHz", plan.uplink_hz() / 1e6), ACCENT);
                metric(ui, self.tr("Pluto LO", "Pluto osilatörü"), &format!("{:.4} MHz", plan.lo_hz() / 1e6), TEXT);
                metric(ui, self.tr("Bandwidth", "Bant genişliği"), &format!("{:.0} kHz", plan.half_width() * 2.0 / 1e3), TEXT);
                metric(ui, self.tr("TS capacity", "TS kapasitesi"), &plan.capacity().map_or("—".into(), |c| format!("{:.1} kbit/s", c / 1e3)), TEXT);
                ui.end_row();
            });
        });
        ui.add_space(10.0);

        // Start / stop and status.
        let problem = self.problem();
        ui.horizontal(|ui| {
            if busy {
                let label = if preparing { self.tr("■  CANCEL", "■  İPTAL") } else { self.tr("■  STOP", "■  DURDUR") };
                if ui.add(egui::Button::new(RichText::new(label).size(22.0).strong().color(Color32::WHITE)).fill(BAD).min_size(vec2(260.0, 64.0)).corner_radius(10)).clicked() {
                    self.stop();
                }
            } else {
                let button = egui::Button::new(RichText::new(self.tr("●  TRANSMIT", "●  YAYINLA")).size(22.0).strong().color(Color32::WHITE))
                    .fill(BAD.gamma_multiply(0.85))
                    .min_size(vec2(260.0, 64.0))
                    .corner_radius(10);
                let response = ui.add_enabled(problem.is_none(), button);
                let response = match &problem {
                    Some(p) => response.on_disabled_hover_text(p.as_str()),
                    None => response.on_hover_text(self.tr("Transmits on the QO-100 uplink until the time limit or STOP", "Süre sınırına ya da DURDUR'a kadar QO-100 uplink'inde yayın yapar")),
                };
                if response.clicked() {
                    self.start(ui.ctx(), false);
                }
                ui.add_space(8.0);
                let test = egui::Button::new(RichText::new(self.tr("Link test", "Bağlantı testi")).size(15.0)).min_size(vec2(150.0, 64.0)).corner_radius(10);
                if ui
                    .add_enabled(problem.is_none(), test)
                    .on_hover_text(self.tr(
                        "Sends the signal at the real rate through the Pluto's digital loopback with the transmitter off: nothing is radiated",
                        "Sinyali gerçek hızda, verici kapalıyken Pluto'nun dijital geri döngüsünden geçirir: hiçbir şey yayılmaz",
                    ))
                    .clicked()
                {
                    self.start(ui.ctx(), true);
                }
            }
        });
        ui.add_space(8.0);
        if let Some(p) = problem.filter(|_| !busy) {
            ui.label(RichText::new(p).color(WARN));
        }
        if busy {
            let (text, color) = if preparing {
                (self.tr("Preparing the signal…", "Sinyal hazırlanıyor…").to_string(), WARN)
            } else if on_air {
                (self.tr("ON AIR", "YAYINDA").to_string(), BAD)
            } else if running {
                (self.tr("LINK TEST, nothing is transmitted", "BAĞLANTI TESTİ, yayın yok").to_string(), ACCENT)
            } else {
                (self.tr("Connecting to the Pluto…", "Pluto'ya bağlanılıyor…").to_string(), WARN)
            };
            egui::Frame::new().fill(color.gamma_multiply(0.18)).stroke(Stroke::new(1.5, color)).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    dot(ui, color);
                    ui.label(RichText::new(text).size(18.0).strong().color(color));
                    if running {
                        let job = self.job.as_ref().unwrap();
                        ui.add_space(16.0);
                        ui.label(RichText::new(format!("{elapsed:.0} / {:.0} s", job.seconds)).size(16.0).monospace().color(TEXT));
                        ui.add_space(16.0);
                        let ok = achieved >= job.sample_rate * 0.98 || elapsed < 3.0;
                        ui.label(RichText::new(format!("{:.3} / {:.3} MS/s", achieved / 1e6, job.sample_rate / 1e6)).size(16.0).monospace().color(if ok { GOOD } else { BAD }));
                    }
                });
                if running {
                    let job = self.job.as_ref().unwrap();
                    ui.add(egui::ProgressBar::new((elapsed / job.seconds).clamp(0.0, 1.0) as f32).desired_height(6.0));
                }
            });
        }
        ui.add_space(10.0);

        // Log.
        ui.label(RichText::new(self.tr("LOG", "GÜNLÜK")).size(11.0).strong().color(ACCENT));
        egui::Frame::new().fill(DEEP).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                let job_log: Vec<(Color32, String)> = self.job.as_ref().map(|j| j.shared.lock().unwrap().log.clone()).unwrap_or_default();
                for (color, line) in self.log.iter().chain(job_log.iter()) {
                    ui.label(RichText::new(line).monospace().size(12.0).color(*color));
                }
            });
        });
    }
}

impl eframe::App for TxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // A dropped .ts file becomes the source.
        if let Some(path) = ui.ctx().input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
            self.file = path.display().to_string();
            self.source = Source::File;
        }
        let bar = egui::Frame::new().fill(CARD).inner_margin(egui::Margin::symmetric(14, 8));
        egui::Panel::top("header").frame(bar).show(ui, |ui| self.header(ui));
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(320.0)
            .size_range(280.0..=460.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(10)))
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(14)))
            .show(ui, |ui| self.central(ui));
        if self.busy() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn on_exit(&mut self) {
        // Closing the window stops a transmission; the transmitter is switched off on the way out.
        self.finish_job();
        self.save_settings();
    }
}

fn main() -> eframe::Result {
    let size = 64;
    let icon = egui::IconData { rgba: logo::rgba(size), width: size as u32, height: size as u32 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("QODA TX - QO-100 DATV transmitter").with_inner_size([1180.0, 760.0]).with_icon(icon),
        ..Default::default()
    };
    eframe::run_native("QODA TX", options, Box::new(|cc| Ok(Box::new(TxApp::new(cc)))))
}
