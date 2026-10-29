//! QODA ana penceresi: cihaz ve frekans kontrolleri, spektrum ve şelale.

use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Shape, Stroke, TextureHandle,
    TextureOptions, Vec2, pos2,
};

use crate::detect::{self, Detection};
use crate::sdr::{self, Command, DeviceInfo, Event, Frame, Settings};
use crate::tuning;

const FFT_SIZE: usize = 2048;
const WATERFALL_ROWS: usize = 300;
/// Spektrum çizgisinin üstel ortalama katsayısı.
const SMOOTHING: f32 = 0.35;
/// RTL-SDR için güvenli örnekleme hızları (MS/s).
const SAMPLE_RATES: [f64; 4] = [1.024, 1.4, 2.048, 2.4];
/// Kayıtların yazıldığı klasör (çalışma klasörüne göre).
const RECORDINGS_DIR: &str = "recordings";
const ROLLOFFS: [f32; 3] = [0.35, 0.25, 0.20];

pub struct QodaApp {
    devices: Vec<DeviceInfo>,
    device_index: usize,
    handle: Option<sdr::Handle>,
    status: String,

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

    detection: Option<Detection>,
    /// Süren kaydın yolu ve yazılan örnek sayısı.
    recording: Option<(std::path::PathBuf, u64)>,

    last_frame: Option<Frame>,
    smoothed: Vec<f32>,
    /// Görüntülenen dB aralığı (alt, üst); otomatik izlenir.
    range: (f32, f32),
    waterfall: ColorImage,
    waterfall_tex: Option<TextureHandle>,
}

impl QodaApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let devices = sdr::list_devices();
        let device_index = devices
            .iter()
            .position(|d| d.args.contains("driver=rtlsdr"))
            .unwrap_or(0);
        let status = if devices.is_empty() {
            "SDR cihazı bulunamadı".into()
        } else {
            format!("{} cihaz bulundu", devices.len())
        };
        Self {
            devices,
            device_index,
            handle: None,
            status,
            lo_mhz: 9750.0,
            center_mhz: 10491.5,
            selected_mhz: 10491.5,
            symbol_rate_ks: 1000.0,
            rolloff: 0.35,
            sample_rate_ms: 2.048,
            auto_gain: false,
            gain_db: 30.0,
            detection: None,
            recording: None,
            last_frame: None,
            smoothed: Vec::new(),
            range: (-100.0, -40.0),
            waterfall: ColorImage::new([FFT_SIZE, WATERFALL_ROWS], vec![Color32::BLACK; FFT_SIZE * WATERFALL_ROWS]),
            waterfall_tex: None,
        }
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
        self.handle = Some(sdr::spawn(dev.args.clone(), settings, move || ctx.request_repaint()));
        self.status = format!("{} açıldı", dev.label);
    }

    fn stop(&mut self) {
        self.handle = None;
        self.recording = None;
        self.status = "Durduruldu".into();
    }

    fn retune(&mut self, center_mhz: f64) {
        self.center_mhz = center_mhz;
        self.send(Command::Tune(self.if_hz()));
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
                        self.status = format!("Kayıt kaydedildi: {}", path.with_extension("sigmf-data").display());
                    }
                }
                Event::Error(e) => error = Some(e),
            }
        }
        if let Some(e) = error {
            self.status = format!("Hata: {e}");
            self.handle = None;
            self.recording = None;
        }
        if let Some(f) = newest {
            self.push_frame(f);
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
                self.rolloff = ROLLOFFS
                    .iter()
                    .copied()
                    .min_by(|a, b| (a - d.rolloff).abs().total_cmp(&(b - d.rolloff).abs()))
                    .unwrap_or(0.35);
            }
            None => self.status = format!("{mhz:.3} MHz civarında yayın bulunamadı"),
        }
    }

    fn detection_text(&self) -> String {
        let Some(d) = &self.detection else {
            return "Yayın seçmek için spektruma tıklayın".into();
        };
        let mut text = format!(
            "Bulunan: {:.1} kS/s ölçüldü",
            d.measured_symbol_rate / 1e3
        );
        if let Some(r) = d.common_symbol_rate {
            text += &format!(" (≈ {:.0} kS/s)", r / 1e3);
        }
        text += &format!(", roll-off ~{:.2}, SNR {:.1} dB", d.rolloff, d.snr_db);
        if d.truncated {
            text += " · yayın spektrum kenarına taşıyor, alıcıyı ortalayın";
        }
        text
    }

    fn toggle_recording(&mut self) {
        if self.recording.is_some() {
            self.send(Command::StopRecording);
            return;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let name = format!(
            "qoda_{:.3}MHz_{:.0}ksps_{stamp}",
            self.center_mhz,
            self.sample_rate_ms * 1e3
        );
        let path = std::path::Path::new(RECORDINGS_DIR).join(name);
        self.send(Command::StartRecording { path, rf_hz: self.center_mhz * 1e6 });
        // İlk olay gelene kadar düğme "kaydediyor" görünsün.
        self.recording = Some((std::path::PathBuf::new(), 0));
    }

    fn recording_text(&self) -> Option<String> {
        let (path, samples) = self.recording.as_ref()?;
        let seconds = *samples as f64 / (self.sample_rate_ms * 1e6);
        let mb = *samples as f64 * 8.0 / 1e6;
        Some(format!("⏺ Kaydediliyor {seconds:.1} s, {mb:.0} MB · {}", path.display()))
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let running = self.handle.is_some();
            ui.add_enabled_ui(!running, |ui| {
                let current = self.devices.get(self.device_index).map_or("—", |d| d.label.as_str());
                egui::ComboBox::from_label("Cihaz")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, d) in self.devices.iter().enumerate() {
                            ui.selectable_value(&mut self.device_index, i, &d.label);
                        }
                    });
                if ui.button("Yenile").clicked() {
                    self.devices = sdr::list_devices();
                    self.device_index = self.device_index.min(self.devices.len().saturating_sub(1));
                }
            });
            if running {
                if ui.button("⏹ Durdur").clicked() {
                    self.stop();
                }
            } else if ui.add_enabled(!self.devices.is_empty(), egui::Button::new("▶ Başlat")).clicked() {
                self.start(ui.ctx());
            }

            if running {
                let label = if self.recording.is_some() { "⏹ Kaydı bitir" } else { "⏺ Kaydet" };
                if ui
                    .button(label)
                    .on_hover_text("Ham IQ örneklerini SigMF olarak recordings klasörüne kaydet. Frekans veya örnekleme hızı değişince kayıt kapanır.")
                    .clicked()
                {
                    self.toggle_recording();
                }
            }

            ui.separator();
            ui.label("Örnekleme");
            let before = self.sample_rate_ms;
            egui::ComboBox::from_id_salt("rate")
                .selected_text(format!("{} MS/s", self.sample_rate_ms))
                .show_ui(ui, |ui| {
                    for r in SAMPLE_RATES {
                        ui.selectable_value(&mut self.sample_rate_ms, r, format!("{r} MS/s"));
                    }
                });
            if self.sample_rate_ms != before {
                self.send(Command::SampleRate(self.sample_rate_ms * 1e6));
            }

            ui.separator();
            let mut gain_changed = ui.checkbox(&mut self.auto_gain, "Otomatik kazanç").changed();
            gain_changed |= ui
                .add_enabled(!self.auto_gain, egui::Slider::new(&mut self.gain_db, 0.0..=50.0).suffix(" dB"))
                .changed();
            if gain_changed {
                self.send(Command::Gain(self.gain()));
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("LNB LO");
            let lo = ui.add(egui::DragValue::new(&mut self.lo_mhz).speed(0.01).range(0.0..=30_000.0).suffix(" MHz"));
            ui.label("Merkez RF");
            let center = ui.add(
                egui::DragValue::new(&mut self.center_mhz).speed(0.01).range(0.0..=30_000.0).fixed_decimals(3).suffix(" MHz"),
            );
            if lo.changed() || center.changed() {
                self.retune(self.center_mhz);
            }
            ui.label(format!("IF {:.3} MHz", self.if_hz() / 1e6));

            ui.separator();
            ui.label("Yayın");
            ui.add(
                egui::DragValue::new(&mut self.selected_mhz).speed(0.001).range(0.0..=30_000.0).fixed_decimals(3).suffix(" MHz"),
            );
            if ui.button("Ortala").on_hover_text("Alıcıyı seçili yayına ayarla").clicked() {
                self.retune(self.selected_mhz);
            }
            ui.label("Sembol hızı");
            ui.add(egui::DragValue::new(&mut self.symbol_rate_ks).speed(1.0).range(25.0..=2400.0).suffix(" kS/s"));
            egui::ComboBox::from_label("Roll-off")
                .selected_text(format!("{:.2}", self.rolloff))
                .show_ui(ui, |ui| {
                    for r in ROLLOFFS {
                        ui.selectable_value(&mut self.rolloff, r, format!("{r:.2}"));
                    }
                });
            ui.label(format!("Bant genişliği {:.0} kHz", self.occupied_khz()));
            if ui.button("Otomatik bul").on_hover_text("Seçili frekanstaki yayının parametrelerini spektrumdan bul").clicked() {
                self.auto_detect(self.selected_mhz);
            }
        });
        ui.label(self.detection_text());
    }

    fn occupied_khz(&self) -> f32 {
        self.symbol_rate_ks * (1.0 + self.rolloff)
    }

    /// Spektrum ve şelaleyi çizer; tıklama yayın seçer, çift tıklama alıcıyı ortalar.
    fn display(&mut self, ui: &mut egui::Ui) {
        let tex = self
            .waterfall_tex
            .get_or_insert_with(|| ui.ctx().load_texture("waterfall", self.waterfall.clone(), TextureOptions::LINEAR))
            .clone();

        let size = ui.available_size();
        let (response, painter) = ui.allocate_painter(size, Sense::click());
        let full = response.rect;
        let spec_rect = Rect::from_min_size(full.min, Vec2::new(full.width(), full.height() * 0.45));
        let axis_h = 18.0;
        let wf_rect = Rect::from_min_max(pos2(full.left(), spec_rect.bottom() + axis_h), full.max);

        painter.rect_filled(spec_rect, 0.0, Color32::from_rgb(12, 16, 24));
        painter.image(tex.id(), wf_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

        // Çizilen spektrum, karenin ayarlandığı merkeze göre konumlanır.
        let (frame_center, span_mhz) = self.frame_span();
        let x_of = |mhz: f64| full.left() + ((mhz - frame_center) / span_mhz + 0.5) as f32 * full.width();
        let mhz_of = |x: f32| frame_center + ((x - full.left()) / full.width() - 0.5) as f64 * span_mhz;

        // dB ızgarası.
        let (lo, hi) = self.range;
        let y_of = |db: f32| spec_rect.bottom() - (db - lo) / (hi - lo) * spec_rect.height();
        let grid = Color32::from_gray(50);
        let mut db = (lo / 10.0).ceil() * 10.0;
        while db < hi {
            let y = y_of(db);
            painter.line_segment([pos2(spec_rect.left(), y), pos2(spec_rect.right(), y)], Stroke::new(1.0, grid));
            painter.text(pos2(spec_rect.left() + 4.0, y), Align2::LEFT_BOTTOM, format!("{db:.0} dB"), FontId::monospace(10.0), Color32::GRAY);
            db += 10.0;
        }

        // Frekans ekseni: 250 kHz aralıklı işaretler.
        let step = 0.25;
        let mut f = ((frame_center - span_mhz / 2.0) / step).ceil() * step;
        while f <= frame_center + span_mhz / 2.0 {
            let x = x_of(f);
            painter.line_segment([pos2(x, spec_rect.top()), pos2(x, spec_rect.bottom())], Stroke::new(1.0, grid));
            painter.text(pos2(x, spec_rect.bottom() + 2.0), Align2::CENTER_TOP, format!("{f:.2}"), FontId::monospace(11.0), Color32::LIGHT_GRAY);
            f += step;
        }

        // Seçili yayının kapladığı bant.
        let half = self.occupied_khz() as f64 / 2000.0;
        let band = Rect::from_x_y_ranges(x_of(self.selected_mhz - half)..=x_of(self.selected_mhz + half), spec_rect.y_range());
        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(80, 160, 255, 40));
        let sx = x_of(self.selected_mhz);
        painter.line_segment([pos2(sx, spec_rect.top()), pos2(sx, wf_rect.bottom())], Stroke::new(1.0, Color32::from_rgb(80, 160, 255)));

        // Spektrum çizgisi.
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
            painter.add(Shape::line(points, Stroke::new(1.5, Color32::from_rgb(255, 210, 80))));
        }

        // Fare imleci altındaki frekans.
        if let Some(pos) = response.hover_pos() {
            painter.line_segment([pos2(pos.x, spec_rect.top()), pos2(pos.x, spec_rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(80)));
            painter.text(
                pos2(pos.x + 6.0, spec_rect.top() + 4.0),
                Align2::LEFT_TOP,
                format!("{:.3} MHz", mhz_of(pos.x)),
                FontId::monospace(12.0),
                Color32::WHITE,
            );
        }
        if let Some(pos) = response.interact_pointer_pos() {
            if response.double_clicked() {
                self.selected_mhz = mhz_of(pos.x);
                self.auto_detect(self.selected_mhz);
                self.retune(self.selected_mhz);
            } else if response.clicked() {
                self.selected_mhz = mhz_of(pos.x);
                self.auto_detect(self.selected_mhz);
            }
        }
    }
}

impl eframe::App for QodaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events();
        egui::Panel::top("controls").show(ui, |ui| {
            ui.add_space(4.0);
            self.controls(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                match self.recording_text() {
                    Some(t) => ui.colored_label(Color32::from_rgb(255, 90, 90), t),
                    None => ui.label(&self.status),
                };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("QODA · yAtölye");
                });
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.display(ui));
    }
}

/// 0..1 aralığındaki değeri koyu maviden sarıya/kırmızıya giden renge çevirir.
fn colormap(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.0, [0.0, 0.0, 0.05]),
        (0.3, [0.0, 0.1, 0.5]),
        (0.55, [0.0, 0.7, 0.8]),
        (0.8, [1.0, 0.85, 0.1]),
        (1.0, [1.0, 0.2, 0.1]),
    ];
    let i = STOPS.iter().rposition(|(p, _)| *p <= t).unwrap_or(0).min(STOPS.len() - 2);
    let (p0, c0) = STOPS[i];
    let (p1, c1) = STOPS[i + 1];
    let k = (t - p0) / (p1 - p0);
    let ch = |j: usize| ((c0[j] + k * (c1[j] - c0[j])) * 255.0) as u8;
    Color32::from_rgb(ch(0), ch(1), ch(2))
}
