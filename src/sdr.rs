//! SDR cihazı ile konuşan arka plan iş parçacığı.
//!
//! Arayüz komut gönderir (frekans, kazanç, örnekleme hızı); iş parçacığı
//! örnekleri okur, spektrumu hesaplar ve olay olarak geri yollar.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;

use num_complex::Complex;
use soapysdr::Direction::Rx;

use qoda::dsp::Spectrum;

use crate::iio::IioRadio;

/// Bir saniyede üretilecek yaklaşık spektrum karesi sayısı.
const FRAMES_PER_SECOND: f64 = 25.0;

/// Alıcının hangi arka uçla sürüldüğü.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// SoapySDR (RTL-SDR, HackRF, ...).
    Soapy,
    /// libiio ile AD9361 tabanlı alıcılar (ADALM-Pluto, LibreSDR).
    Iio,
}

impl DeviceKind {
    /// Arayüzde sunulan örnekleme hızları (MS/s). AD9361 2.08 MS/s altına
    /// ek süzgeç olmadan inemez; RTL-SDR 2.4 MS/s üstünde örnek kaçırır.
    /// 10–12 MS/s geniş bant transponderinin tamamını gösterir; gigabit Ethernet
    /// gerektirir (100 Mbit bağlantıda 2.4 MS/s kullanılmalı).
    pub fn sample_rates(self) -> &'static [f64] {
        match self {
            DeviceKind::Soapy => &[1.024, 1.4, 2.048, 2.4],
            DeviceKind::Iio => &[2.4, 3.0, 4.0, 6.0, 8.0, 10.0, 12.0],
        }
    }

    /// Ayarlanabilir kazanç aralığı (dB).
    pub fn gain_range(self) -> (f64, f64) {
        match self {
            DeviceKind::Soapy => (0.0, 50.0),
            DeviceKind::Iio => (0.0, 71.0),
        }
    }
}

/// Seçilebilir bir alıcı.
#[derive(Clone)]
pub struct DeviceInfo {
    pub label: String,
    /// SoapySDR için "anahtar=değer,..." argümanları, libiio için URI (ör. "ip:192.168.2.1").
    pub args: String,
    pub kind: DeviceKind,
}

impl DeviceInfo {
    /// Ağ üzerindeki bir Pluto/LibreSDR; `address` IP adresi ya da tam libiio URI'si.
    pub fn network(address: &str) -> Self {
        let uri = if address.contains(':') { address.to_string() } else { format!("ip:{address}") };
        Self { label: format!("Pluto/LibreSDR ({uri})"), args: uri, kind: DeviceKind::Iio }
    }
}

/// Ses kartları dışındaki SoapySDR cihazlarını listeler.
pub fn list_devices() -> Vec<DeviceInfo> {
    let Ok(found) = soapysdr::enumerate("") else {
        return Vec::new();
    };
    found
        .iter()
        .filter(|a| a.get("driver") != Some("audio"))
        .map(|a| {
            let args = a.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",");
            let label = a.get("label").unwrap_or(&args).to_string();
            DeviceInfo { label, args, kind: DeviceKind::Soapy }
        })
        .collect()
}

pub struct Settings {
    pub if_hz: f64,
    pub sample_rate: f64,
    /// dB cinsinden sabit kazanç; None ise otomatik kazanç.
    pub gain: Option<f64>,
    pub fft_size: usize,
}

pub enum Command {
    Tune(f64),
    Gain(Option<f64>),
    SampleRate(f64),
    /// Ham IQ örneklerini SigMF olarak kaydetmeye başla. Yol uzantısızdır;
    /// `.sigmf-data` ve `.sigmf-meta` dosyaları oluşturulur.
    StartRecording { path: PathBuf, rf_hz: f64 },
    StopRecording,
    /// Okunan her IQ bloğunun kopyasını bu kanala gönder (canlı çözücü için); None durdurur.
    /// Kanal doluyken atlanan bloklar sayaçta tutulur.
    IqTap(Option<(SyncSender<Vec<Complex<f32>>>, Arc<AtomicU64>)>),
}

pub struct Frame {
    pub if_hz: f64,
    pub sample_rate: f64,
    pub db: Vec<f32>,
}

pub enum Event {
    Frame(Frame),
    /// Kayıt sürüyor; şimdiye kadar yazılan örnek sayısı.
    Recording { path: PathBuf, samples: u64 },
    RecordingStopped,
    Error(SdrError),
}

/// SDR iş parçacığının hataları; metne arayüz kendi dilinde çevirir.
#[derive(Debug, Clone)]
pub enum SdrError {
    /// Cihaz, SoapySDR ya da libiio hatası (kütüphanenin kendi iletisiyle).
    Device(String),
    RecordingOpen(String),
    RecordingWrite(String),
}

/// Açık bir SigMF kaydı.
struct Recording {
    path: PathBuf,
    data: BufWriter<File>,
    samples: u64,
}

impl Recording {
    fn start(path: PathBuf, sample_rate: f64, rf_hz: f64, if_hz: f64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let meta = format!(
            r#"{{
  "global": {{
    "core:datatype": "cf32_le",
    "core:sample_rate": {sample_rate},
    "core:version": "1.0.0",
    "core:recorder": "QODA {version}",
    "core:description": "QO-100 DATV IQ recording, LNB output IF {if_mhz:.6} MHz"
  }},
  "captures": [
    {{ "core:sample_start": 0, "core:frequency": {rf_hz} }}
  ],
  "annotations": []
}}
"#,
            version = env!("CARGO_PKG_VERSION"),
            if_mhz = if_hz / 1e6,
        );
        std::fs::write(path.with_extension("sigmf-meta"), meta)?;
        let data = BufWriter::new(File::create(path.with_extension("sigmf-data"))?);
        Ok(Self { path, data, samples: 0 })
    }

    fn write(&mut self, samples: &[Complex<f32>]) -> std::io::Result<()> {
        for s in samples {
            self.data.write_all(&s.re.to_le_bytes())?;
            self.data.write_all(&s.im.to_le_bytes())?;
        }
        self.samples += samples.len() as u64;
        Ok(())
    }
}

/// Çalışan iş parçacığına bağlantı. Düşürüldüğünde iş parçacığı durur.
pub struct Handle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

/// İş parçacığını başlatır. `notify` her yeni olayda çağrılır (ör. arayüzü yeniden çizdirmek için).
pub fn spawn(device: DeviceInfo, settings: Settings, notify: impl Fn() + Send + 'static) -> Handle {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(e) = run(&device, settings, &cmd_rx, &ev_tx, &notify) {
            let _ = ev_tx.send(Event::Error(SdrError::Device(e)));
            notify();
        }
    });
    Handle { commands: cmd_tx, events: ev_rx }
}

/// İki arka ucu aynı biçimde kullanmak için alıcı.
enum Radio {
    Soapy { device: soapysdr::Device, stream: soapysdr::RxStream<Complex<f32>> },
    Iio(IioRadio),
}

impl Radio {
    fn open(info: &DeviceInfo, s: &Settings) -> Result<Self, String> {
        let mut radio = match info.kind {
            DeviceKind::Soapy => {
                let device = soapysdr::Device::new(info.args.as_str()).map_err(|e| e.to_string())?;
                let stream = device.rx_stream::<Complex<f32>>(&[0]).map_err(|e| e.to_string())?;
                Radio::Soapy { device, stream }
            }
            DeviceKind::Iio => Radio::Iio(IioRadio::open(&info.args)?),
        };
        radio.set_sample_rate(s.sample_rate)?;
        radio.set_frequency(s.if_hz)?;
        radio.set_gain(s.gain)?;
        if let Radio::Soapy { stream, .. } = &mut radio {
            stream.activate(None).map_err(|e| e.to_string())?;
        }
        Ok(radio)
    }

    fn set_frequency(&mut self, hz: f64) -> Result<(), String> {
        match self {
            Radio::Soapy { device, .. } => device.set_frequency(Rx, 0, hz, ()).map_err(|e| e.to_string()),
            Radio::Iio(r) => r.set_frequency(hz),
        }
    }

    /// Akış durdurulmadan değiştirilir: bazı rtlsdr.dll sürümleri deactivate sırasında çöküyor.
    fn set_sample_rate(&mut self, rate: f64) -> Result<(), String> {
        match self {
            Radio::Soapy { device, .. } => device.set_sample_rate(Rx, 0, rate).map_err(|e| e.to_string()),
            Radio::Iio(r) => r.set_sample_rate(rate),
        }
    }

    fn set_gain(&mut self, gain: Option<f64>) -> Result<(), String> {
        match self {
            Radio::Soapy { device, .. } => {
                let result = match gain {
                    Some(db) => device.set_gain_mode(Rx, 0, false).and_then(|_| device.set_gain(Rx, 0, db)),
                    None => device.set_gain_mode(Rx, 0, true),
                };
                result.map_err(|e| e.to_string())
            }
            Radio::Iio(r) => r.set_gain(gain),
        }
    }

    /// Okunan örnek sayısı; taşma gibi geçici durumlarda 0.
    fn read(&mut self, out: &mut [Complex<f32>]) -> Result<usize, String> {
        match self {
            Radio::Soapy { stream, .. } => match stream.read(&mut [out], 1_000_000) {
                Ok(n) => Ok(n),
                Err(e) if e.code == soapysdr::ErrorCode::Overflow => Ok(0),
                Err(e) => Err(e.to_string()),
            },
            Radio::Iio(r) => r.read(out),
        }
    }
}

fn run(
    device: &DeviceInfo,
    mut s: Settings,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
    notify: &dyn Fn(),
) -> Result<(), String> {
    let mut radio = Radio::open(device, &s)?;
    let mut spectrum = Spectrum::new(s.fft_size);
    let mut recording: Option<Recording> = None;
    let mut tap: Option<(SyncSender<Vec<Complex<f32>>>, Arc<AtomicU64>)> = None;
    let stop_recording = |rec: &mut Option<Recording>| {
        if let Some(mut r) = rec.take() {
            let _ = r.data.flush();
            let _ = events.send(Event::RecordingStopped);
        }
    };

    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Tune(hz)) => {
                    s.if_hz = hz;
                    radio.set_frequency(hz)?;
                    // Kayıt meta verisi tek frekans içerir; frekans değişince kaydı kapat.
                    stop_recording(&mut recording);
                }
                Ok(Command::Gain(g)) => {
                    s.gain = g;
                    radio.set_gain(g)?;
                }
                Ok(Command::SampleRate(rate)) => {
                    s.sample_rate = rate;
                    stop_recording(&mut recording);
                    radio.set_sample_rate(rate)?;
                }
                Ok(Command::StartRecording { path, rf_hz }) => {
                    stop_recording(&mut recording);
                    match Recording::start(path, s.sample_rate, rf_hz, s.if_hz) {
                        Ok(r) => recording = Some(r),
                        Err(e) => {
                            let _ = events.send(Event::Error(SdrError::RecordingOpen(e.to_string())));
                        }
                    }
                }
                Ok(Command::StopRecording) => stop_recording(&mut recording),
                Ok(Command::IqTap(t)) => tap = t,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        let blocks = (s.sample_rate / (s.fft_size as f64 * FRAMES_PER_SECOND)).max(1.0) as usize;
        let mut samples = vec![Complex::default(); blocks * s.fft_size];
        let mut filled = 0;
        while filled < samples.len() {
            filled += radio.read(&mut samples[filled..])?;
        }

        if let Some((t, dropped)) = &tap {
            // Çözücü yetişemezse blok atlanır; SDR okuması hiç bekletilmez.
            match t.try_send(samples.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => tap = None,
            }
        }
        if let Some(r) = &mut recording {
            match r.write(&samples) {
                Ok(()) => {
                    let _ = events.send(Event::Recording { path: r.path.clone(), samples: r.samples });
                }
                Err(e) => {
                    recording = None;
                    let _ = events.send(Event::Error(SdrError::RecordingWrite(e.to_string())));
                }
            }
        }

        let frame = Frame {
            if_hz: s.if_hz,
            sample_rate: s.sample_rate,
            db: spectrum.compute(&samples),
        };
        if events.send(Event::Frame(frame)).is_err() {
            return Ok(());
        }
        notify();
    }
}
