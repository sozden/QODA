//! SDR cihazı ile konuşan arka plan iş parçacığı.
//!
//! Arayüz komut gönderir (frekans, kazanç, örnekleme hızı); iş parçacığı
//! örnekleri okur, spektrumu hesaplar ve olay olarak geri yollar.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use num_complex::Complex;
use soapysdr::Direction::Rx;

use crate::dsp::Spectrum;

/// Bir saniyede üretilecek yaklaşık spektrum karesi sayısı.
const FRAMES_PER_SECOND: f64 = 25.0;

/// SoapySDR'ın bulduğu bir alıcı.
#[derive(Clone)]
pub struct DeviceInfo {
    pub label: String,
    /// `Device::new` için "anahtar=değer,..." biçiminde argümanlar.
    pub args: String,
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
            DeviceInfo { label, args }
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
}

pub struct Frame {
    pub if_hz: f64,
    pub sample_rate: f64,
    pub db: Vec<f32>,
}

pub enum Event {
    Frame(Frame),
    Error(String),
}

/// Çalışan iş parçacığına bağlantı. Düşürüldüğünde iş parçacığı durur.
pub struct Handle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

/// İş parçacığını başlatır. `notify` her yeni olayda çağrılır (ör. arayüzü yeniden çizdirmek için).
pub fn spawn(device_args: String, settings: Settings, notify: impl Fn() + Send + 'static) -> Handle {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(e) = run(&device_args, settings, &cmd_rx, &ev_tx, &notify) {
            let _ = ev_tx.send(Event::Error(e.to_string()));
            notify();
        }
    });
    Handle { commands: cmd_tx, events: ev_rx }
}

fn apply_gain(dev: &soapysdr::Device, gain: Option<f64>) -> Result<(), soapysdr::Error> {
    match gain {
        Some(db) => {
            dev.set_gain_mode(Rx, 0, false)?;
            dev.set_gain(Rx, 0, db)
        }
        None => dev.set_gain_mode(Rx, 0, true),
    }
}

fn run(
    device_args: &str,
    mut s: Settings,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
    notify: &dyn Fn(),
) -> Result<(), soapysdr::Error> {
    let dev = soapysdr::Device::new(device_args)?;
    dev.set_sample_rate(Rx, 0, s.sample_rate)?;
    dev.set_frequency(Rx, 0, s.if_hz, ())?;
    apply_gain(&dev, s.gain)?;

    let mut stream = dev.rx_stream::<Complex<f32>>(&[0])?;
    stream.activate(None)?;
    let mut spectrum = Spectrum::new(s.fft_size);

    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Tune(hz)) => {
                    s.if_hz = hz;
                    dev.set_frequency(Rx, 0, hz, ())?;
                }
                Ok(Command::Gain(g)) => {
                    s.gain = g;
                    apply_gain(&dev, g)?;
                }
                Ok(Command::SampleRate(rate)) => {
                    s.sample_rate = rate;
                    stream.deactivate(None)?;
                    dev.set_sample_rate(Rx, 0, rate)?;
                    stream.activate(None)?;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stream.deactivate(None)?;
                    return Ok(());
                }
            }
        }

        let blocks = (s.sample_rate / (s.fft_size as f64 * FRAMES_PER_SECOND)).max(1.0) as usize;
        let mut samples = vec![Complex::default(); blocks * s.fft_size];
        let mut filled = 0;
        while filled < samples.len() {
            match stream.read(&mut [&mut samples[filled..]], 1_000_000) {
                Ok(n) => filled += n,
                Err(e) if e.code == soapysdr::ErrorCode::Overflow => {}
                Err(e) => return Err(e),
            }
        }

        let frame = Frame {
            if_hz: s.if_hz,
            sample_rate: s.sample_rate,
            db: spectrum.compute(&samples),
        };
        if events.send(Event::Frame(frame)).is_err() {
            stream.deactivate(None)?;
            return Ok(());
        }
        notify();
    }
}
