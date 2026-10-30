//! QODA ana penceresi: sol panelde alıcı, frekans, yayın ve izleme kartları;
//! ortada gömülü video, spektrum ve şelale.

use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, FontId, Mesh, Pos2, Rect, RichText, Sense, Shape,
    Stroke, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};

use crate::decoder::{self, Decoder, PlayerIssue, VIDEO_HEIGHT, VIDEO_WIDTH};
use crate::i18n::{Lang, Strings, fill};
use crate::logo;
use crate::sdr::{self, Command, DeviceInfo, DeviceKind, Event, Frame, SdrError, Settings};
use crate::settings::UserSettings;
use qoda::detect::{self, Detection};
use qoda::dvbs2::plheader::Pls;
use qoda::dvbs2::receiver::ReceiverConfig;
use qoda::tsinfo::{ServiceInfo, StreamKind};
use qoda::tuning;

const FFT_SIZE: usize = 2048;
const WATERFALL_ROWS: usize = 300;
/// Spektrum çizgisinin üstel ortalama katsayısı.
const SMOOTHING: f32 = 0.35;
/// Kayıtların yazıldığı klasör (çalışma klasörüne göre).
const RECORDINGS_DIR: &str = "recordings";
const ROLLOFFS: [f32; 3] = [0.35, 0.25, 0.20];
/// Üst bardaki logo dokusunun piksel boyutu (yüksek DPI için çizim boyutunun katı).
const LOGO_PIXELS: usize = 128;
/// İzleme sürerken frekans değişince çözücünün yeniden başlaması için beklenen sessizlik.
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
/// Spektrumda fare tekerleğinin bir adımı (MHz); Shift ile 10 katı.
const WHEEL_STEP_MHZ: f64 = 0.010;
/// Yayın şeridi bu kadar spektrum karesinde bir yenilenir.
const SCAN_EVERY_FRAMES: u32 = 12;
/// QO-100 geniş bant transponderinin merkezi (10491–10499.5 MHz).
const WIDEBAND_CENTER_MHZ: f64 = 10495.25;
/// Geniş bant görünümü için gereken en düşük örnekleme hızı (MS/s).
const WIDEBAND_MIN_RATE: f64 = 10.0;

/// Renk paleti.
mod palette {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(11, 15, 22);
    pub const DEEP: Color32 = Color32::from_rgb(7, 10, 15);
    pub const CARD: Color32 = Color32::from_rgb(19, 25, 35);
    pub const BORDER: Color32 = Color32::from_rgb(34, 43, 57);
    pub const TEXT: Color32 = Color32::from_rgb(214, 222, 235);
    pub const MUTED: Color32 = Color32::from_rgb(128, 141, 160);
    pub const ACCENT: Color32 = Color32::from_rgb(56, 200, 240);
    pub const GOOD: Color32 = Color32::from_rgb(74, 222, 128);
    pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);
    pub const BAD: Color32 = Color32::from_rgb(248, 113, 113);
    pub const BAND: Color32 = Color32::from_rgb(251, 191, 36);
}
use palette::*;

/// Durum çubuğundaki ileti; seçili dilde gösterilir.
enum Status {
    NoDevice,
    DevicesFound(usize),
    Opened(String),
    Stopped,
    Saved(std::path::PathBuf),
    Error(SdrError),
    NoSignal(f64),
}

pub struct QodaApp {
    lang: Lang,
    devices: Vec<DeviceInfo>,
    device_index: usize,
    /// Eklenen ağ alıcılarının libiio URI'leri (kaydedilir).
    network_devices: Vec<String>,
    /// Ağ alıcısı ekleme kutusundaki metin.
    network_input: String,
    handle: Option<sdr::Handle>,
    status: Status,

    lo_mhz: f64,
    /// Alıcının ayarlandığı RF merkez frekansı.
    center_mhz: f64,
    /// Spektrumda seçili yayının RF frekansı.
    selected_mhz: f64,
    symbol_rate_ks: f32,
    rolloff: f32,
    sample_rate_ms: f64,
    auto_gain: bool,
    gain_db: f64,
    /// İzlerken sesi de çal.
    audio: bool,

    detection: Option<Detection>,
    /// Canlı DVB-S2 çözücü (izleme sürerken).
    decoder: Option<Decoder>,
    video_tex: Option<TextureHandle>,
    video_seq: u64,
    /// Süren kaydın yolu ve yazılan örnek sayısı.
    recording: Option<(std::path::PathBuf, u64)>,

    last_frame: Option<Frame>,
    smoothed: Vec<f32>,
    /// Görüntülenen dB aralığı (alt, üst); otomatik izlenir.
    range: (f32, f32),
    waterfall: ColorImage,
    waterfall_tex: Option<TextureHandle>,
    logo_tex: TextureHandle,
    /// İzlerken frekans değişti; bu andan RESTART_DELAY sonra çözücü yeniden başlar.
    pending_restart: Option<std::time::Instant>,
    /// Görünen bantta bulunan yayınlar: (RF MHz, sembol hızı kS/s, roll-off).
    signals: Vec<(f64, f32, f32)>,
    frames_since_scan: u32,
}

impl QodaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let logo_image = ColorImage::from_rgba_unmultiplied([LOGO_PIXELS, LOGO_PIXELS], &logo::rgba(LOGO_PIXELS));
        let logo_tex = cc.egui_ctx.load_texture("logo", logo_image, TextureOptions::LINEAR);
        let user = UserSettings::load();
        let devices = all_devices(&user.network_devices);
        let device_index = devices
            .iter()
            .position(|d| d.args.contains("driver=rtlsdr"))
            .unwrap_or(0);
        let status = if devices.is_empty() { Status::NoDevice } else { Status::DevicesFound(devices.len()) };
        Self {
            lang: user.lang,
            devices,
            device_index,
            network_devices: user.network_devices.clone(),
            network_input: String::new(),
            handle: None,
            status,
            lo_mhz: user.lnb_lo_mhz,
            center_mhz: 10491.5,
            selected_mhz: 10491.5,
            symbol_rate_ks: 1000.0,
            rolloff: 0.35,
            sample_rate_ms: 2.4,
            auto_gain: false,
            gain_db: 30.0,
            audio: true,
            detection: None,
            decoder: None,
            video_tex: None,
            video_seq: 0,
            recording: None,
            last_frame: None,
            smoothed: Vec::new(),
            range: (-100.0, -40.0),
            waterfall: ColorImage::new([FFT_SIZE, WATERFALL_ROWS], vec![Color32::BLACK; FFT_SIZE * WATERFALL_ROWS]),
            waterfall_tex: None,
            logo_tex,
            pending_restart: None,
            signals: Vec::new(),
            frames_since_scan: 0,
        }
    }

    fn t(&self) -> &'static Strings {
        self.lang.strings()
    }

    fn save_settings(&self) {
        UserSettings { lang: self.lang, lnb_lo_mhz: self.lo_mhz, network_devices: self.network_devices.clone() }.save();
    }

    fn status_text(&self) -> String {
        let t = self.t();
        match &self.status {
            Status::NoDevice => t.status_no_device.to_string(),
            Status::DevicesFound(n) => fill(t.status_devices, &[n]),
            Status::Opened(label) => fill(t.status_opened, &[label]),
            Status::Stopped => t.status_stopped.to_string(),
            Status::Saved(path) => fill(t.status_saved, &[&path.display()]),
            Status::Error(SdrError::Device(e)) => fill(t.status_error, &[e]),
            Status::Error(SdrError::RecordingOpen(e)) => fill(t.status_recording_open, &[e]),
            Status::Error(SdrError::RecordingWrite(e)) => fill(t.status_recording_write, &[e]),
            Status::NoSignal(mhz) => fill(t.status_no_signal, &[&format!("{mhz:.3}")]),
        }
    }

    fn device_kind(&self) -> DeviceKind {
        self.devices.get(self.device_index).map_or(DeviceKind::Soapy, |d| d.kind)
    }

    /// Seçili cihaz değişince örnekleme hızını ve kazancı onun sınırlarına uydurur.
    fn fit_to_device(&mut self) {
        let kind = self.device_kind();
        let rates = kind.sample_rates();
        if !rates.contains(&self.sample_rate_ms) {
            self.sample_rate_ms = if rates.contains(&2.4) { 2.4 } else { rates[0] };
        }
        let (lo, hi) = kind.gain_range();
        self.gain_db = self.gain_db.clamp(lo, hi);
    }

    /// Geniş bant transponderinin tamamını göster: cihazın en yüksek hızına geç
    /// ve transponderin ortasına ayarla.
    fn wideband_view(&mut self, ctx: &egui::Context) {
        let Some(&rate) = self.device_kind().sample_rates().last() else { return };
        if rate < WIDEBAND_MIN_RATE {
            return;
        }
        if self.sample_rate_ms != rate {
            self.sample_rate_ms = rate;
            self.send(Command::SampleRate(rate * 1e6));
        }
        self.retune(ctx, WIDEBAND_CENTER_MHZ);
    }

    fn add_network_device(&mut self) {
        let address = self.network_input.trim().to_string();
        if address.is_empty() {
            return;
        }
        let info = DeviceInfo::network(&address);
        if !self.network_devices.contains(&info.args) {
            self.network_devices.push(info.args.clone());
            self.save_settings();
        }
        self.devices = all_devices(&self.network_devices);
        self.device_index = self.devices.iter().position(|d| d.args == info.args).unwrap_or(0);
        self.network_input.clear();
        self.fit_to_device();
    }

    fn if_hz(&self) -> f64 {
        tuning::intermediate_frequency(self.center_mhz * 1e6, self.lo_mhz * 1e6)
    }

    fn gain(&self) -> Option<f64> {
        (!self.auto_gain).then_some(self.gain_db)
    }

    fn send(&self, cmd: Command) {
        if let Some(h) = &self.handle {
            let _ = h.commands.send(cmd);
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(dev) = self.devices.get(self.device_index) else {
            return;
        };
        let settings = Settings {
            if_hz: self.if_hz(),
            sample_rate: self.sample_rate_ms * 1e6,
            gain: self.gain(),
            fft_size: FFT_SIZE,
        };
        let ctx = ctx.clone();
        self.handle = Some(sdr::spawn(dev.clone(), settings, move || ctx.request_repaint()));
        self.status = Status::Opened(dev.label.clone());
    }

    fn stop(&mut self) {
        self.stop_watch();
        self.handle = None;
        self.recording = None;
        self.status = Status::Stopped;
    }

    fn retune(&mut self, ctx: &egui::Context, center_mhz: f64) {
        self.center_mhz = center_mhz;
        self.send(Command::Tune(self.if_hz()));
        let _ = ctx;
        self.schedule_restart();
    }

    /// İzleme sürüyorsa, ayarlar durulunca çözücüyü yeni ayarla başlatmak üzere işaretle.
    fn schedule_restart(&mut self) {
        if self.decoder.is_some() {
            self.pending_restart = Some(std::time::Instant::now());
        }
    }

    /// Seçili yayın değişti: görünür bandın dışına çıktıysa alıcıyı ortala.
    fn selection_changed(&mut self, ctx: &egui::Context) {
        let (center, span) = self.frame_span();
        if (self.selected_mhz - center).abs() > span * 0.42 {
            self.retune(ctx, self.selected_mhz);
        } else {
            self.schedule_restart();
        }
    }

    /// Yayın şeridinden ya da otomatik tespitten gelen yayını seç.
    fn select_signal(&mut self, ctx: &egui::Context, mhz: f64, symbol_rate_ks: f32, rolloff: f32) {
        self.selected_mhz = mhz;
        self.symbol_rate_ks = symbol_rate_ks;
        self.rolloff = rolloff;
        self.selection_changed(ctx);
    }

    /// Seçili yayını canlı çözmeye ve videoyu göstermeye başlar.
    fn start_watch(&mut self, ctx: &egui::Context) {
        self.stop_watch();
        if self.handle.is_none() {
            return;
        }
        let cfg = ReceiverConfig {
            sample_rate: self.sample_rate_ms * 1e6,
            offset_hz: (self.selected_mhz - self.center_mhz) * 1e6,
            symbol_rate: self.symbol_rate_ks as f64 * 1e3,
            rolloff: self.rolloff as f64,
        };
        let ctx = ctx.clone();
        let d = decoder::spawn(cfg, self.audio, move || ctx.request_repaint());
        self.send(Command::IqTap(Some((d.iq.clone(), d.dropped.clone()))));
        self.decoder = Some(d);
        self.video_seq = 0;
    }

    fn stop_watch(&mut self) {
        if self.decoder.take().is_some() {
            self.send(Command::IqTap(None));
        }
        self.video_tex = None;
    }

    fn poll_events(&mut self) {
        let Some(h) = &self.handle else { return };
        let mut error = None;
        let mut newest = None;
        while let Ok(ev) = h.events.try_recv() {
            match ev {
                Event::Frame(f) => newest = Some(f),
                Event::Recording { path, samples } => self.recording = Some((path, samples)),
                Event::RecordingStopped => {
                    if let Some((path, _)) = self.recording.take() {
                        self.status = Status::Saved(path.with_extension("sigmf-data"));
                    }
                }
                Event::Error(e) => error = Some(e),
            }
        }
        if let Some(e) = error {
            self.status = Status::Error(e);
            self.stop_watch();
            self.handle = None;
            self.recording = None;
        }
        if let Some(f) = newest {
            self.push_frame(f);
        }
    }

    fn poll_video(&mut self, ctx: &egui::Context) {
        let Some(d) = &self.decoder else { return };
        let guard = d.video.lock().unwrap();
        let Some(frame) = guard.as_ref() else { return };
        if frame.seq == self.video_seq {
            return;
        }
        self.video_seq = frame.seq;
        let image = ColorImage::from_rgba_unmultiplied([VIDEO_WIDTH, VIDEO_HEIGHT], &frame.rgba);
        drop(guard);
        match &mut self.video_tex {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => self.video_tex = Some(ctx.load_texture("video", image, TextureOptions::LINEAR)),
        }
    }

    fn push_frame(&mut self, frame: Frame) {
        if self.smoothed.len() != frame.db.len() {
            self.smoothed = frame.db.clone();
        } else {
            for (s, v) in self.smoothed.iter_mut().zip(&frame.db) {
                *s += SMOOTHING * (v - *s);
            }
        }

        // Görüntü aralığını gürültü tabanı ve tepe değerine göre yavaşça izle.
        let mut sorted = self.smoothed.clone();
        sorted.sort_by(f32::total_cmp);
        let floor = sorted[sorted.len() / 10] - 5.0;
        let top = sorted[sorted.len() - 1] + 5.0;
        let top = top.max(floor + 20.0);
        if self.last_frame.is_none() {
            self.range = (floor, top);
        } else {
            self.range.0 += 0.1 * (floor - self.range.0);
            self.range.1 += 0.1 * (top - self.range.1);
        }

        // Şelaleyi bir satır aşağı kaydırıp en üste yeni satırı yaz.
        let w = self.waterfall.size[0];
        self.waterfall.pixels.copy_within(0..w * (WATERFALL_ROWS - 1), w);
        let (lo, hi) = self.range;
        for (px, v) in self.waterfall.pixels[..w].iter_mut().zip(&frame.db) {
            *px = colormap((v - lo) / (hi - lo));
        }
        if let Some(tex) = &mut self.waterfall_tex {
            tex.set(self.waterfall.clone(), TextureOptions::LINEAR);
        }

        self.last_frame = Some(frame);
        self.frames_since_scan += 1;
        if self.frames_since_scan >= SCAN_EVERY_FRAMES {
            self.frames_since_scan = 0;
            self.signals = self.scan_signals();
        }
    }

    /// Görünen bantta bulunan yayınlar.
    fn scan_signals(&self) -> Vec<(f64, f32, f32)> {
        let (center, span) = self.frame_span();
        detect::scan(&self.smoothed, span * 1e6)
            .into_iter()
            .filter(|d| !d.truncated)
            .map(|d| (center + d.center_offset_hz / 1e6, (d.symbol_rate() / 1e3) as f32, nearest_rolloff(d.rolloff)))
            .collect()
    }

    /// Son karenin RF merkezi ve genişliği (MHz).
    fn frame_span(&self) -> (f64, f64) {
        self.last_frame.as_ref().map_or((self.center_mhz, self.sample_rate_ms), |f| {
            (f.if_hz / 1e6 + self.lo_mhz, f.sample_rate / 1e6)
        })
    }

    /// `mhz` civarındaki yayını spektrumdan bulup yayın ayarlarını doldurur.
    fn auto_detect(&mut self, mhz: f64) {
        let (center, span) = self.frame_span();
        let n = self.smoothed.len();
        let pos = (mhz - center) / span + 0.5;
        if n == 0 || !(0.0..1.0).contains(&pos) {
            return;
        }
        let guess = (pos * n as f64) as usize;
        self.detection = detect::detect(&self.smoothed, span * 1e6, guess);
        match &self.detection {
            Some(d) => {
                self.selected_mhz = center + d.center_offset_hz / 1e6;
                self.symbol_rate_ks = (d.symbol_rate() / 1e3) as f32;
                self.rolloff = nearest_rolloff(d.rolloff);
            }
            None => self.status = Status::NoSignal(mhz),
        }
    }

    fn toggle_recording(&mut self) {
        if self.recording.is_some() {
            self.send(Command::StopRecording);
            return;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // Adda nokta olmamalı: uzantılar `with_extension` ile eklenir.
        let name = format!(
            "qoda_{:.0}kHz_{:.0}ksps_{stamp}",
            self.center_mhz * 1e3,
            self.sample_rate_ms * 1e3
        );
        let path = std::path::Path::new(RECORDINGS_DIR).join(name);
        self.send(Command::StartRecording { path, rf_hz: self.center_mhz * 1e6 });
        // İlk olay gelene kadar düğme "kaydediyor" görünsün.
        self.recording = Some((std::path::PathBuf::new(), 0));
    }

    fn occupied_khz(&self) -> f32 {
        self.symbol_rate_ks * (1.0 + self.rolloff)
    }

    // ---- Üst bar ----

    fn header(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.logo_tex).fit_to_exact_size(vec2(44.0, 44.0)));
            ui.vertical(|ui| {
                ui.label(RichText::new("QODA").size(24.0).strong().color(ACCENT));
                ui.label(RichText::new(t.app_subtitle).size(12.0).color(MUTED));
            });
            ui.add_space(6.0);
            yatolye_badge(ui, t);
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(t.broadcast_caps).size(10.0).color(MUTED));
                if freq_tuner(ui, &mut self.selected_mhz, 22.0) {
                    self.selection_changed(ui.ctx());
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(t.symbol_rate_caps).size(10.0).color(MUTED));
                ui.label(RichText::new(format!("{:.0} kS/s", self.symbol_rate_ks)).size(18.0).monospace().color(TEXT));
            });
            if let Some((name, provider)) = self.service_title() {
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(t.service_caps).size(10.0).color(MUTED));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).size(18.0).strong().color(BAND));
                        if let Some(p) = provider {
                            ui.label(RichText::new(p).size(13.0).color(MUTED));
                        }
                    });
                });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let watching = self.decoder.is_some();
                let watch = if watching {
                    egui::Button::new(RichText::new(t.stop_watching).strong().color(Color32::WHITE)).fill(BAD.gamma_multiply(0.75))
                } else {
                    egui::Button::new(RichText::new(t.watch).strong().color(DEEP)).fill(ACCENT)
                };
                if ui
                    .add_enabled(self.handle.is_some(), watch.min_size(vec2(150.0, 34.0)).corner_radius(8))
                    .on_hover_text(t.watch_tooltip)
                    .clicked()
                {
                    if watching {
                        self.stop_watch();
                    } else {
                        self.start_watch(ui.ctx());
                    }
                }
                let running = self.handle.is_some();
                let power = if running {
                    egui::Button::new(RichText::new(t.stop_receiver).color(TEXT))
                } else {
                    egui::Button::new(RichText::new(t.start_receiver).strong().color(DEEP)).fill(GOOD)
                };
                if ui
                    .add_enabled(running || !self.devices.is_empty(), power.min_size(vec2(150.0, 34.0)).corner_radius(8))
                    .clicked()
                {
                    if running {
                        self.stop();
                    } else {
                        self.start(ui.ctx());
                    }
                }
                ui.add_space(8.0);
                let before = self.lang;
                egui::ComboBox::from_id_salt("language")
                    .width(96.0)
                    .selected_text(self.lang.native_name())
                    .show_ui(ui, |ui| {
                        for l in Lang::ALL {
                            ui.selectable_value(&mut self.lang, l, l.native_name());
                        }
                    })
                    .response
                    .on_hover_text(t.language);
                if self.lang != before {
                    self.save_settings();
                }
            });
        });
    }

    // ---- Sol panel ----

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let t = self.t();
            card(ui, t.card_receiver, |ui| self.receiver_card(ui));
            card(ui, t.card_frequency, |ui| self.frequency_card(ui));
            card(ui, t.card_broadcast, |ui| self.broadcast_card(ui));
            card(ui, t.card_watch, |ui| self.watch_card(ui));
            card(ui, t.card_recording, |ui| self.recording_card(ui));
        });
    }

    fn receiver_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let running = self.handle.is_some();
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                let current = self.devices.get(self.device_index).map_or("—", |d| d.label.as_str());
                let before = self.device_index;
                egui::ComboBox::from_id_salt("device")
                    .width(ui.available_width() - 70.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, d) in self.devices.iter().enumerate() {
                            ui.selectable_value(&mut self.device_index, i, &d.label);
                        }
                    });
                if ui.button(t.refresh).clicked() {
                    self.devices = all_devices(&self.network_devices);
                    self.device_index = self.device_index.min(self.devices.len().saturating_sub(1));
                }
                if self.device_index != before {
                    self.fit_to_device();
                }
            });
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut self.network_input)
                    .hint_text(t.network_hint)
                    .desired_width(ui.available_width() - 70.0);
                let response = ui.add(edit).on_hover_text(t.network_tooltip);
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button(t.add).clicked() || enter {
                    self.add_network_device();
                }
            });
        });
        egui::Grid::new("receiver").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new(t.sample_rate).color(MUTED));
            let before = self.sample_rate_ms;
            egui::ComboBox::from_id_salt("rate")
                .selected_text(format!("{} MS/s", self.sample_rate_ms))
                .show_ui(ui, |ui| {
                    for &r in self.device_kind().sample_rates() {
                        ui.selectable_value(&mut self.sample_rate_ms, r, format!("{r} MS/s"));
                    }
                });
            if self.sample_rate_ms != before {
                self.send(Command::SampleRate(self.sample_rate_ms * 1e6));
                if self.decoder.is_some() {
                    self.start_watch(ui.ctx());
                }
            }
            ui.end_row();

            ui.label(RichText::new(t.gain).color(MUTED));
            let mut changed = ui.checkbox(&mut self.auto_gain, t.auto_gain).changed();
            ui.end_row();
            ui.label("");
            let (gain_lo, gain_hi) = self.device_kind().gain_range();
            changed |= ui
                .add_enabled(!self.auto_gain, egui::Slider::new(&mut self.gain_db, gain_lo..=gain_hi).suffix(" dB"))
                .changed();
            if changed {
                self.send(Command::Gain(self.gain()));
            }
            ui.end_row();
        });
    }

    fn frequency_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let mut retune = false;
        let lo_before = self.lo_mhz;
        egui::Grid::new("frequency").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new(t.lnb_lo).color(MUTED));
            retune |= ui
                .add(egui::DragValue::new(&mut self.lo_mhz).speed(0.01).range(0.0..=30_000.0).suffix(" MHz"))
                .changed();
            ui.end_row();
            ui.label(RichText::new(t.center_rf).color(MUTED));
            retune |= freq_tuner(ui, &mut self.center_mhz, 16.0);
            ui.end_row();
            ui.label(RichText::new(t.intermediate_frequency).color(MUTED));
            ui.label(RichText::new(format!("{:.3} MHz", self.if_hz() / 1e6)).monospace().color(ACCENT));
            ui.end_row();
        });
        let wide_capable = self.device_kind().sample_rates().last().is_some_and(|&r| r >= WIDEBAND_MIN_RATE);
        let hover = if wide_capable { t.wideband_tooltip } else { t.wideband_unavailable };
        if ui
            .add_enabled(wide_capable, egui::Button::new(t.wideband).min_size(vec2(ui.available_width(), 26.0)))
            .on_hover_text(hover)
            .on_disabled_hover_text(hover)
            .clicked()
        {
            self.wideband_view(ui.ctx());
        }
        if retune {
            self.retune(ui.ctx(), self.center_mhz);
        }
        if self.lo_mhz != lo_before {
            self.save_settings();
        }
    }

    fn broadcast_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        egui::Grid::new("broadcast").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new(t.frequency).color(MUTED));
            if freq_tuner(ui, &mut self.selected_mhz, 16.0) {
                self.selection_changed(ui.ctx());
            }
            ui.end_row();
            ui.label(RichText::new(t.symbol_rate).color(MUTED));
            ui.add(egui::DragValue::new(&mut self.symbol_rate_ks).speed(1.0).range(25.0..=2400.0).suffix(" kS/s"));
            ui.end_row();
            ui.label(RichText::new(t.rolloff).color(MUTED));
            egui::ComboBox::from_id_salt("rolloff")
                .selected_text(format!("{:.2}", self.rolloff))
                .show_ui(ui, |ui| {
                    for r in ROLLOFFS {
                        ui.selectable_value(&mut self.rolloff, r, format!("{r:.2}"));
                    }
                });
            ui.end_row();
            ui.label(RichText::new(t.bandwidth).color(MUTED));
            ui.label(RichText::new(format!("{:.0} kHz", self.occupied_khz())).monospace());
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(t.auto_detect).on_hover_text(t.auto_detect_tooltip).clicked() {
                self.auto_detect(self.selected_mhz);
            }
            if ui.button(t.center).on_hover_text(t.center_tooltip).clicked() {
                self.retune(ui.ctx(), self.selected_mhz);
            }
        });
        ui.add_space(2.0);
        let hint = match &self.detection {
            None => t.broadcast_hint.to_string(),
            Some(d) => {
                let mut text = fill(t.measured, &[&format!("{:.1}", d.measured_symbol_rate / 1e3)]);
                if let Some(r) = d.common_symbol_rate {
                    text += &format!(" ≈ {:.0}", r / 1e3);
                }
                text += &format!(" · roll-off ~{:.2} · SNR {:.1} dB", d.rolloff, d.snr_db);
                if d.truncated {
                    text += t.near_edge;
                }
                text
            }
        };
        ui.label(RichText::new(hint).size(11.0).color(MUTED));
    }

    fn watch_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let before = self.audio;
        ui.checkbox(&mut self.audio, t.audio);
        if self.audio != before && self.decoder.is_some() {
            self.start_watch(ui.ctx());
        }
        let Some(d) = &self.decoder else {
            ui.label(RichText::new(t.not_watching).color(MUTED));
            return;
        };
        let status = d.status.lock().unwrap().clone();
        let dropped = d.dropped.load(std::sync::atomic::Ordering::Relaxed);
        let s = &status.stats;
        let (lock_text, lock_color) = if s.locked { (t.locked, GOOD) } else { (t.searching, WARN) };
        ui.horizontal(|ui| {
            dot(ui, lock_color);
            ui.label(RichText::new(lock_text).strong().color(lock_color));
        });
        if let Some(name) = &s.service.name {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).size(16.0).strong().color(BAND));
                if let Some(p) = &s.service.provider {
                    ui.label(RichText::new(p).color(MUTED));
                }
            });
        }
        if let Some(p) = s.pls {
            ui.label(RichText::new(describe_pls(&p, t)).color(TEXT));
        }
        let streams = describe_streams(&s.service, t);
        if !streams.is_empty() {
            ui.label(RichText::new(streams).size(11.0).color(MUTED));
        }
        ui.add_space(4.0);
        egui::Grid::new("metrics").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            metric(ui, "SNR", &format!("{:.1} dB", s.snr_db), snr_color(s.snr_db));
            metric(ui, t.metric_carrier, &format!("{:+.1} kHz", s.carrier_offset_hz / 1e3), TEXT);
            ui.end_row();
            metric(ui, t.metric_frames_ok, &s.frames_ok.to_string(), GOOD);
            metric(ui, t.metric_frames_failed, &s.frames_failed.to_string(), if s.frames_failed > 0 { BAD } else { TEXT });
            ui.end_row();
            metric(ui, "LDPC", &format!("{} it", s.ldpc_iterations), TEXT);
            metric(ui, t.metric_ts_packets, &s.ts_packets.to_string(), TEXT);
            ui.end_row();
        });
        let mut warnings = Vec::new();
        if s.ts_cc_errors > 0 {
            warnings.push(fill(t.warn_cc, &[&s.ts_cc_errors]));
        }
        if s.frames_unsupported > 0 {
            warnings.push(fill(t.warn_unsupported, &[&s.frames_unsupported]));
        }
        if dropped > 0 {
            warnings.push(fill(t.warn_iq_dropped, &[&dropped]));
        }
        if status.ts_dropped > 0 {
            warnings.push(fill(t.warn_ts_dropped, &[&status.ts_dropped]));
        }
        if let Some(issue) = &status.player {
            warnings.push(match issue {
                PlayerIssue::FfmpegMissing => t.ffmpeg_missing.to_string(),
                PlayerIssue::FfplayMissing => t.ffplay_missing.to_string(),
                PlayerIssue::StartFailed { program, error } => fill(t.player_start_failed, &[program, error]),
                PlayerIssue::Closed => t.player_closed.to_string(),
            });
        }
        for w in warnings {
            ui.label(RichText::new(w).size(11.0).color(WARN));
        }
    }

    fn recording_card(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let running = self.handle.is_some();
        let label = if self.recording.is_some() { t.stop_recording } else { t.record };
        if ui
            .add_enabled(running, egui::Button::new(label).min_size(vec2(ui.available_width(), 28.0)))
            .on_hover_text(t.record_tooltip)
            .clicked()
        {
            self.toggle_recording();
        }
        if let Some((path, samples)) = &self.recording {
            let seconds = *samples as f64 / (self.sample_rate_ms * 1e6);
            let mb = *samples as f64 * 8.0 / 1e6;
            ui.horizontal(|ui| {
                dot(ui, BAD);
                ui.label(RichText::new(format!("{seconds:.1} s · {mb:.0} MB")).color(BAD));
            });
            ui.label(RichText::new(path.display().to_string()).size(10.0).color(MUTED));
        }
    }

    // ---- Orta alan ----

    fn video(&self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.t();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(10), DEEP);
        painter.rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
        // 16:9 alanı dikdörtgene sığdır.
        let aspect = VIDEO_WIDTH as f32 / VIDEO_HEIGHT as f32;
        let inner = rect.shrink(6.0);
        let size = if inner.width() / inner.height() > aspect {
            vec2(inner.height() * aspect, inner.height())
        } else {
            vec2(inner.width(), inner.width() / aspect)
        };
        let video_rect = Rect::from_center_size(inner.center(), size);
        match &self.video_tex {
            Some(tex) => {
                painter.image(tex.id(), video_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                let locked = self
                    .decoder
                    .as_ref()
                    .is_some_and(|d| d.status.lock().unwrap().stats.locked);
                let text = if locked { t.video_waiting } else { t.video_searching };
                painter.text(video_rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(18.0), MUTED);
            }
        }
        if let Some(d) = &self.decoder {
            let s = d.status.lock().unwrap().stats.clone();
            if s.locked {
                let mut label = t.live.to_string();
                if let Some(name) = &s.service.name {
                    label += &format!("   {name}");
                }
                let galley = painter.layout_no_wrap(label, FontId::proportional(14.0), TEXT);
                let at = video_rect.min + vec2(34.0, 15.0);
                let badge = Rect::from_min_size(at - vec2(24.0, 5.0), galley.size() + vec2(32.0, 10.0));
                painter.rect_filled(badge, CornerRadius::same(6), Color32::from_black_alpha(170));
                painter.circle_filled(pos2(at.x - 12.0, at.y + galley.size().y / 2.0), 4.5, BAD);
                painter.galley(at, galley, TEXT);
            }
        }
    }

    /// Spektrum ve şelaleyi çizer; tıklama yayın seçer, çift tıklama alıcıyı ortalar.
    fn spectrum(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let tex = self
            .waterfall_tex
            .get_or_insert_with(|| ui.ctx().load_texture("waterfall", self.waterfall.clone(), TextureOptions::LINEAR))
            .clone();
        let response = ui.interact(rect, ui.id().with("spectrum"), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(10), DEEP);

        let full = rect.shrink(1.0);
        let spec_rect = Rect::from_min_size(full.min, Vec2::new(full.width(), full.height() * 0.48));
        let axis_h = 20.0;
        let wf_rect = Rect::from_min_max(pos2(full.left(), spec_rect.bottom() + axis_h), full.max);
        painter.image(tex.id(), wf_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

        // Çizilen spektrum, karenin ayarlandığı merkeze göre konumlanır.
        let (frame_center, span_mhz) = self.frame_span();
        let x_of = |mhz: f64| full.left() + ((mhz - frame_center) / span_mhz + 0.5) as f32 * full.width();
        let mhz_of = |x: f32| frame_center + ((x - full.left()) / full.width() - 0.5) as f64 * span_mhz;

        // dB ızgarası.
        let (lo, hi) = self.range;
        let y_of = |db: f32| spec_rect.bottom() - (db - lo) / (hi - lo) * spec_rect.height();
        let grid = Color32::from_rgb(26, 34, 46);
        let mut db = (lo / 10.0).ceil() * 10.0;
        while db < hi {
            let y = y_of(db);
            painter.line_segment([pos2(spec_rect.left(), y), pos2(spec_rect.right(), y)], Stroke::new(1.0, grid));
            painter.text(pos2(spec_rect.left() + 6.0, y - 1.0), Align2::LEFT_BOTTOM, format!("{db:.0} dB"), FontId::monospace(10.0), MUTED);
            db += 10.0;
        }

        // Frekans ekseni: işaretler arası en az ~90 piksel olacak şekilde aralık seç.
        let min_step = span_mhz * 90.0 / full.width().max(1.0) as f64;
        let step = [0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0].into_iter().find(|&s| s >= min_step).unwrap_or(10.0);
        let mut f = ((frame_center - span_mhz / 2.0) / step).ceil() * step;
        let axis = Rect::from_min_max(pos2(full.left(), spec_rect.bottom()), pos2(full.right(), wf_rect.top()));
        painter.rect_filled(axis, CornerRadius::ZERO, BG);
        while f <= frame_center + span_mhz / 2.0 {
            let x = x_of(f);
            painter.line_segment([pos2(x, spec_rect.top()), pos2(x, spec_rect.bottom())], Stroke::new(1.0, grid));
            painter.text(pos2(x, axis.center().y), Align2::CENTER_CENTER, format!("{f:.2}"), FontId::monospace(11.0), MUTED);
            f += step;
        }

        // Seçili yayının kapladığı bant.
        let half = self.occupied_khz() as f64 / 2000.0;
        let band = Rect::from_x_y_ranges(x_of(self.selected_mhz - half)..=x_of(self.selected_mhz + half), spec_rect.y_range());
        painter.rect_filled(band, CornerRadius::ZERO, BAND.gamma_multiply(0.10));
        for x in [band.left(), band.right()] {
            painter.line_segment([pos2(x, spec_rect.top()), pos2(x, spec_rect.bottom())], Stroke::new(1.0, BAND.gamma_multiply(0.5)));
        }
        let sx = x_of(self.selected_mhz);
        painter.line_segment([pos2(sx, spec_rect.top()), pos2(sx, wf_rect.bottom())], Stroke::new(1.0, BAND.gamma_multiply(0.8)));

        // Spektrum: altı renk geçişli dolgu, üstü çizgi.
        if !self.smoothed.is_empty() {
            let n = self.smoothed.len();
            let points: Vec<Pos2> = self
                .smoothed
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    let x = spec_rect.left() + (i as f32 + 0.5) / n as f32 * spec_rect.width();
                    pos2(x, y_of(v).clamp(spec_rect.top(), spec_rect.bottom()))
                })
                .collect();
            let mut mesh = Mesh::default();
            for p in &points {
                let t = 1.0 - (p.y - spec_rect.top()) / spec_rect.height();
                mesh.colored_vertex(*p, ACCENT.gamma_multiply(0.15 + 0.35 * t));
                mesh.colored_vertex(pos2(p.x, spec_rect.bottom()), ACCENT.gamma_multiply(0.02));
            }
            for i in 0..n as u32 - 1 {
                let a = 2 * i;
                mesh.add_triangle(a, a + 1, a + 2);
                mesh.add_triangle(a + 1, a + 3, a + 2);
            }
            painter.add(Shape::mesh(mesh));
            painter.add(Shape::line(points, Stroke::new(1.4, ACCENT)));
        }

        // Fare imleci altındaki frekans.
        if let Some(pos) = response.hover_pos() {
            painter.line_segment([pos2(pos.x, spec_rect.top()), pos2(pos.x, wf_rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(60)));
            let label = format!("{:.3} MHz", mhz_of(pos.x));
            let galley = painter.layout_no_wrap(label, FontId::monospace(12.0), TEXT);
            let at = pos2((pos.x + 8.0).min(spec_rect.right() - galley.size().x - 12.0), spec_rect.top() + 8.0);
            let bg = Rect::from_min_size(at - vec2(6.0, 3.0), galley.size() + vec2(12.0, 6.0));
            painter.rect_filled(bg, CornerRadius::same(4), Color32::from_black_alpha(180));
            painter.galley(at, galley, TEXT);
        }
        // Sürükleme alıcıyı kaydırır; seçili yayın RF'de yerinde kalır.
        if response.dragged() && response.drag_delta().x != 0.0 {
            let shift = -(response.drag_delta().x / full.width()) as f64 * span_mhz;
            let center = ((self.center_mhz + shift) * 1000.0).round() / 1000.0;
            self.retune(ui.ctx(), center);
        }
        // Tekerlek seçili yayını adım adım kaydırır.
        if response.hovered() {
            let (steps, shift) = wheel_steps(ui);
            if steps != 0 {
                let step = if shift { WHEEL_STEP_MHZ * 10.0 } else { WHEEL_STEP_MHZ };
                self.selected_mhz = ((self.selected_mhz + steps as f64 * step) * 1000.0).round() / 1000.0;
                self.selection_changed(ui.ctx());
            }
        }
        if let Some(pos) = response.interact_pointer_pos() {
            if response.double_clicked() {
                self.selected_mhz = mhz_of(pos.x);
                self.auto_detect(self.selected_mhz);
                self.retune(ui.ctx(), self.selected_mhz);
            } else if response.clicked() {
                self.selected_mhz = mhz_of(pos.x);
                self.auto_detect(self.selected_mhz);
            }
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        if self.decoder.is_some() {
            // Video üstte, en fazla alanın %62'si; spektrum altta.
            let video_h = (area.width() / (VIDEO_WIDTH as f32 / VIDEO_HEIGHT as f32)).min(area.height() * 0.62);
            let video_rect = Rect::from_min_size(area.min, vec2(area.width(), video_h));
            let rest = Rect::from_min_max(pos2(area.left(), video_rect.bottom() + 8.0), area.max);
            self.video(ui, video_rect);
            self.spectrum_with_signals(ui, rest);
        } else {
            self.spectrum_with_signals(ui, area);
        }
    }

    /// Üstte bulunan yayınların şeridi, altında spektrum.
    fn spectrum_with_signals(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.t();
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
        let spec = Rect::from_min_max(pos2(rect.left(), strip.bottom() + 6.0), rect.max);
        let mut picked = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(strip), |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(t.signals_caps).size(10.0).strong().color(MUTED));
                if self.signals.is_empty() {
                    ui.label(RichText::new(t.signals_none).size(11.0).color(MUTED));
                }
                for &(mhz, rs, rolloff) in &self.signals {
                    let selected = (mhz - self.selected_mhz).abs() < 0.02;
                    let text = RichText::new(format!("{mhz:.3}  ·  {rs:.0}k")).monospace().size(12.0);
                    let button = if selected {
                        egui::Button::new(text.color(DEEP).strong()).fill(BAND)
                    } else {
                        egui::Button::new(text.color(TEXT))
                    };
                    if ui.add(button.corner_radius(12)).on_hover_text(t.signal_tooltip).clicked() {
                        picked = Some((mhz, rs, rolloff));
                    }
                }
            });
        });
        if let Some((mhz, rs, rolloff)) = picked {
            self.select_signal(ui.ctx(), mhz, rs, rolloff);
        }
        self.spectrum(ui, spec);
    }

    /// Çözülen yayının servis adı ve sağlayıcısı (izleme sürerken ve okunduysa).
    fn service_title(&self) -> Option<(String, Option<String>)> {
        let d = self.decoder.as_ref()?;
        let service = d.status.lock().unwrap().stats.service.clone();
        Some((service.name?, service.provider))
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            dot(ui, if self.handle.is_some() { GOOD } else { MUTED });
            ui.label(RichText::new(self.status_text()).size(12.0).color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("GPL-3.0").size(11.0).color(MUTED));
                ui.label(RichText::new("yAtölye").size(13.0).strong().color(BAND));
                ui.label(RichText::new("QODA ·").size(13.0).color(MUTED));
            });
        });
    }
}

impl eframe::App for QodaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        let ctx = ui.ctx().clone();
        self.poll_video(&ctx);
        if let Some(t) = self.pending_restart {
            if t.elapsed() >= RESTART_DELAY {
                self.pending_restart = None;
                self.start_watch(&ctx);
            } else {
                ctx.request_repaint_after(RESTART_DELAY);
            }
        }

        let bar = egui::Frame::new().fill(CARD).inner_margin(egui::Margin::symmetric(14, 8));
        egui::Panel::top("header").frame(bar).show(ui, |ui| self.header(ui));
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(DEEP).inner_margin(egui::Margin::symmetric(12, 4)))
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(300.0)
            .size_range(260.0..=440.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(10)))
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(10)))
            .show(ui, |ui| self.central(ui));
    }
}

/// Başlıklı, kenarlıklı bir kart.
fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(11.0).strong().color(ACCENT));
            ui.add_space(6.0);
            add(ui);
        });
    ui.add_space(10.0);
}

/// Rakam rakam ayarlanabilen frekans göstergesi (MHz, 3 ondalık). Fare
/// tekerleği üzerindeki rakamı o basamak kadar değiştirir; rakamın üst yarısına
/// tıklamak artırır, alt yarısına tıklamak azaltır.
fn freq_tuner(ui: &mut egui::Ui, mhz: &mut f64, size: f32) -> bool {
    let text = group_mhz(*mhz);
    let int_digits = text.split('.').next().unwrap_or("").chars().filter(char::is_ascii_digit).count();
    let font = FontId::monospace(size);
    let mut delta = 0.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (mut int_index, mut frac_index, mut after_dot) = (0, 0, false);
        for c in text.chars() {
            let step = if !c.is_ascii_digit() {
                after_dot |= c == '.';
                None
            } else if after_dot {
                frac_index += 1;
                Some(10f64.powi(-frac_index))
            } else {
                int_index += 1;
                Some(10f64.powi((int_digits - int_index) as i32))
            };
            let plain = ui.painter().layout_no_wrap(c.to_string(), font.clone(), TEXT);
            let sense = if step.is_some() { Sense::click() } else { Sense::hover() };
            let (rect, response) = ui.allocate_exact_size(plain.size(), sense);
            let mut galley = plain;
            if let (Some(step), true) = (step, response.hovered()) {
                ui.painter().rect_filled(rect.expand(1.0), CornerRadius::same(3), ACCENT.gamma_multiply(0.22));
                galley = ui.painter().layout_no_wrap(c.to_string(), font.clone(), ACCENT);
                let (steps, _) = wheel_steps(ui);
                delta += steps as f64 * step;
                if let Some(pos) = response.clicked().then(|| response.interact_pointer_pos()).flatten() {
                    delta += if pos.y < rect.center().y { step } else { -step };
                }
            }
            ui.painter().galley(rect.min, galley, TEXT);
        }
        ui.label(RichText::new(" MHz").size(size * 0.55).color(MUTED));
    });
    if delta == 0.0 {
        return false;
    }
    *mhz = ((*mhz + delta) * 1000.0).round() / 1000.0;
    true
}

/// Bu karedeki fare tekerleği adımları (yukarı pozitif) ve Shift basılı mı.
/// Kullanılan kaydırma, çevredeki kaydırma alanlarına geçmesin diye tüketilir.
fn wheel_steps(ui: &egui::Ui) -> (i32, bool) {
    let (steps, shift) = ui.input(|i| {
        let steps: i32 = i
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, .. } => {
                    let d = delta.x + delta.y;
                    (d != 0.0).then_some(d.signum() as i32)
                }
                _ => None,
            })
            .sum();
        (steps, i.modifiers.shift)
    });
    if steps != 0 {
        ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
    }
    (steps, shift)
}

/// Küçük renkli durum noktası.
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn nearest_rolloff(r: f32) -> f32 {
    ROLLOFFS
        .iter()
        .copied()
        .min_by(|a, b| (a - r).abs().total_cmp(&(b - r).abs()))
        .unwrap_or(0.35)
}

/// SoapySDR cihazları ile kaydedilmiş ağ alıcıları.
fn all_devices(network: &[String]) -> Vec<DeviceInfo> {
    let mut devices = sdr::list_devices();
    devices.extend(network.iter().map(|uri| DeviceInfo::network(uri)));
    devices
}

/// Çerçeve sinyalleşmesinin seçili dildeki tanımı, ör. "QPSK 4/5, normal frame, no pilots".
fn describe_pls(p: &Pls, t: &Strings) -> String {
    let size = if p.short_frame { t.frame_short } else { t.frame_normal };
    let pilots = if p.pilots { t.pilots_on } else { t.pilots_off };
    match (p.modcod, p.modulation_and_rate()) {
        (0, _) => t.frame_dummy.to_string(),
        (_, Some((m, (n, d)))) => format!("{} {n}/{d}, {size}, {pilots}", m.name()),
        (mc, None) => fill(t.modcod_reserved, &[&mc]),
    }
}

/// Akış türlerinin seçili dildeki listesi, ör. "H.264 video, MP2 audio".
fn describe_streams(service: &ServiceInfo, t: &Strings) -> String {
    service
        .stream_list()
        .iter()
        .map(|(codec, kind)| {
            let kind = match kind {
                StreamKind::Video => t.stream_video,
                StreamKind::Audio => t.stream_audio,
                StreamKind::Data => t.stream_data,
            };
            format!("{codec} {kind}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "yeniden atölye" çalışma grubunun belirgin rozeti.
fn yatolye_badge(ui: &mut egui::Ui, t: &Strings) {
    egui::Frame::new()
        .fill(BAND.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, BAND.gamma_multiply(0.7)))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(12, 4))
        .show(ui, |ui| {
            ui.label(RichText::new("yAtölye").size(17.0).strong().color(BAND))
                .on_hover_text(t.yatolye_tooltip);
        });
}

/// Küçük etiketli ölçüm kutucuğu.
fn metric(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    egui::Frame::new()
        .fill(DEEP)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_min_width(108.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(10.0).color(MUTED));
                ui.label(RichText::new(value).size(14.0).monospace().color(color));
            });
        });
}

fn snr_color(snr_db: f32) -> Color32 {
    if snr_db >= 8.0 {
        GOOD
    } else if snr_db >= 5.0 {
        WARN
    } else {
        BAD
    }
}

/// 10491.527 → "10 491.527" (binlik ayıraçla okunaklı frekans).
fn group_mhz(mhz: f64) -> String {
    let text = format!("{mhz:.3}");
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(c);
    }
    format!("{grouped}.{frac}")
}

/// Koyu, lacivert tonlu tema.
fn apply_theme(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = DEEP;
        v.faint_bg_color = CARD;
        v.window_corner_radius = CornerRadius::same(10);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        let radius = CornerRadius::same(6);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.noninteractive.corner_radius = radius;
        for (w, fill) in [
            (&mut v.widgets.inactive, Color32::from_rgb(28, 36, 49)),
            (&mut v.widgets.hovered, Color32::from_rgb(38, 50, 67)),
            (&mut v.widgets.active, Color32::from_rgb(48, 64, 86)),
            (&mut v.widgets.open, Color32::from_rgb(38, 50, 67)),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.corner_radius = radius;
        }
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals = v;
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.interact_size.y = 24.0;
    });
}

/// 0..1 aralığındaki değeri koyu maviden sarıya/kırmızıya giden renge çevirir.
fn colormap(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.02, 0.03, 0.06]),
        (0.25, [0.05, 0.12, 0.35]),
        (0.5, [0.05, 0.55, 0.75]),
        (0.7, [0.45, 0.85, 0.65]),
        (0.85, [1.0, 0.85, 0.2]),
        (1.0, [1.0, 0.3, 0.15]),
    ];
    let i = STOPS.iter().rposition(|(p, _)| *p <= t).unwrap_or(0).min(STOPS.len() - 2);
    let (p0, c0) = STOPS[i];
    let (p1, c1) = STOPS[i + 1];
    let k = (t - p0) / (p1 - p0);
    let ch = |j: usize| ((c0[j] + k * (c1[j] - c0[j])) * 255.0) as u8;
    Color32::from_rgb(ch(0), ch(1), ch(2))
}
