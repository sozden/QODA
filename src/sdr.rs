//! Background thread talking to the SDR device.
//!
//! The UI sends commands (frequency, gain, sample rate); the thread reads
//! samples, computes the spectrum and sends it back as events.

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

/// Approximate number of spectrum frames produced per second.
const FRAMES_PER_SECOND: f64 = 25.0;

/// Receiver kind; sets the back end and its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// Generic receiver over SoapySDR (RTL-SDR and others).
    Soapy,
    /// HackRF over SoapySDR: it has a strong DC spike at its centre and no
    /// automatic gain.
    HackRf,
    /// AD9361-based receivers over libiio (ADALM-Pluto, LibreSDR).
    Iio,
}

impl DeviceKind {
    /// Sample rates offered in the UI (MS/s). The AD9361 cannot go below 2.08 MS/s
    /// without extra filtering; the RTL-SDR drops samples above 2.4 MS/s.
    /// 10–12 MS/s shows the whole wideband transponder; it needs gigabit Ethernet
    /// (use 2.4 MS/s on a 100 Mbit link).
    pub fn sample_rates(self) -> &'static [f64] {
        match self {
            DeviceKind::Soapy => &[1.024, 1.4, 2.048, 2.4],
            DeviceKind::HackRf => &[4.0, 8.0, 10.0, 12.0, 16.0, 20.0],
            DeviceKind::Iio => &[2.4, 3.0, 4.0, 6.0, 8.0, 10.0, 12.0],
        }
    }

    /// Sample rate used when the device is selected (MS/s). On the HackRF the window is
    /// kept wider so the signal can stay away from the DC spike.
    pub fn default_rate(self) -> f64 {
        match self {
            DeviceKind::HackRf => 8.0,
            DeviceKind::Soapy | DeviceKind::Iio => 2.4,
        }
    }

    /// Adjustable gain range (dB). On the HackRF, LNA (0–40) + VGA (0–62).
    pub fn gain_range(self) -> (f64, f64) {
        match self {
            DeviceKind::Soapy => (0.0, 50.0),
            DeviceKind::HackRf => (0.0, 102.0),
            DeviceKind::Iio => (0.0, 71.0),
        }
    }

    /// Gain used when the device kind changes (dB).
    pub fn default_gain(self) -> f64 {
        match self {
            DeviceKind::Soapy => 30.0,
            DeviceKind::HackRf => 44.0,
            DeviceKind::Iio => 38.0,
        }
    }

    pub fn supports_auto_gain(self) -> bool {
        self != DeviceKind::HackRf
    }

    /// How far beside a signal the receiver is tuned when centring on it (MHz).
    /// On receivers with a strong DC spike at the centre, it moves outside the signal band.
    pub fn center_offset_mhz(self, occupied_mhz: f64) -> f64 {
        match self {
            DeviceKind::HackRf => occupied_mhz / 2.0 + 0.2,
            DeviceKind::Soapy | DeviceKind::Iio => 0.0,
        }
    }
}

/// A selectable receiver.
#[derive(Clone)]
pub struct DeviceInfo {
    pub label: String,
    /// "key=value,..." arguments for SoapySDR, a URI for libiio (e.g. "ip:192.168.2.1").
    pub args: String,
    pub kind: DeviceKind,
}

impl DeviceInfo {
    /// A Pluto/LibreSDR on the network; `address` is an IP address or a full libiio URI.
    pub fn network(address: &str) -> Self {
        let uri = if address.contains(':') { address.to_string() } else { format!("ip:{address}") };
        Self { label: format!("Pluto/LibreSDR ({uri})"), args: uri, kind: DeviceKind::Iio }
    }
}

/// SoapySDR's search folders and the modules it found (for diagnostics). For each
/// module, the loader's result: the drivers it registered and any error messages.
pub fn soapy_modules() -> (Vec<String>, Vec<(String, String)>) {
    use std::ffi::{CStr, CString};
    unsafe {
        let take = |list: *mut *mut std::os::raw::c_char, len: usize| -> Vec<String> {
            let v = (0..len).map(|i| CStr::from_ptr(*list.add(i)).to_string_lossy().into_owned()).collect();
            let mut list = list;
            soapysdr_sys::SoapySDRStrings_clear(&mut list, len);
            v
        };
        let mut len = 0;
        let paths = take(soapysdr_sys::SoapySDR_listSearchPaths(&mut len), len);
        soapysdr_sys::SoapySDR_loadModules();
        let mut len = 0;
        let modules = take(soapysdr_sys::SoapySDR_listModules(&mut len), len)
            .into_iter()
            .map(|path| {
                let c = CString::new(path.clone()).unwrap_or_default();
                let mut result = soapysdr_sys::SoapySDR_getLoaderResult(c.as_ptr());
                let errors: Vec<String> = (0..result.size)
                    .map(|i| {
                        // Key is the driver name, value the error message (empty if fine).
                        let key = CStr::from_ptr(*result.keys.add(i)).to_string_lossy();
                        let val = CStr::from_ptr(*result.vals.add(i)).to_string_lossy();
                        if val.is_empty() { format!("{key} ok") } else { format!("{key}: {val}") }
                    })
                    .collect();
                soapysdr_sys::SoapySDRKwargs_clear(&mut result);
                (path, errors.join("; "))
            })
            .collect();
        (paths, modules)
    }
}

/// Lists SoapySDR devices other than sound cards.
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
            let kind = if a.get("driver") == Some("hackrf") { DeviceKind::HackRf } else { DeviceKind::Soapy };
            DeviceInfo { label, args, kind }
        })
        .collect()
}

pub struct Settings {
    pub if_hz: f64,
    pub sample_rate: f64,
    /// Fixed gain in dB; None for automatic gain.
    pub gain: Option<f64>,
    pub fft_size: usize,
}

pub enum Command {
    Tune(f64),
    Gain(Option<f64>),
    SampleRate(f64),
    /// Start recording raw IQ samples as SigMF. The path has no extension;
    /// `.sigmf-data` and `.sigmf-meta` files are created.
    StartRecording { path: PathBuf, rf_hz: f64 },
    StopRecording,
    /// Send a copy of every IQ block read to this channel (for the live decoder); None stops it.
    /// Blocks dropped while the channel is full are counted.
    IqTap(Option<(SyncSender<Vec<Complex<f32>>>, Arc<AtomicU64>)>),
}

pub struct Frame {
    pub if_hz: f64,
    pub sample_rate: f64,
    pub db: Vec<f32>,
}

pub enum Event {
    Frame(Frame),
    /// Recording in progress; the number of samples written so far.
    Recording { path: PathBuf, samples: u64 },
    RecordingStopped,
    Error(SdrError),
}

/// Errors of the SDR thread; the UI turns them into text in its own language.
#[derive(Debug, Clone)]
pub enum SdrError {
    /// Device, SoapySDR or libiio error (with the library's own message).
    Device(String),
    RecordingOpen(String),
    RecordingWrite(String),
}

/// An open SigMF recording.
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

/// Handle to the running thread. Dropping it stops the thread.
pub struct Handle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

/// Starts the thread. `notify` is called on every new event (e.g. to repaint the UI).
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

/// Receiver for using both back ends the same way.
enum Radio {
    Soapy { device: soapysdr::Device, stream: soapysdr::RxStream<Complex<f32>>, hackrf: bool },
    Iio(IioRadio),
}

impl Radio {
    fn open(info: &DeviceInfo, s: &Settings) -> Result<Self, String> {
        let mut radio = match info.kind {
            DeviceKind::Soapy | DeviceKind::HackRf => {
                let device = soapysdr::Device::new(info.args.as_str()).map_err(|e| e.to_string())?;
                let stream = device.rx_stream::<Complex<f32>>(&[0]).map_err(|e| e.to_string())?;
                Radio::Soapy { device, stream, hackrf: info.kind == DeviceKind::HackRf }
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

    /// Changed without stopping the stream: some rtlsdr.dll versions crash during deactivate.
    fn set_sample_rate(&mut self, rate: f64) -> Result<(), String> {
        match self {
            Radio::Soapy { device, .. } => device.set_sample_rate(Rx, 0, rate).map_err(|e| e.to_string()),
            Radio::Iio(r) => r.set_sample_rate(rate),
        }
    }

    fn set_gain(&mut self, gain: Option<f64>) -> Result<(), String> {
        match self {
            Radio::Soapy { device, hackrf: true, .. } => {
                // A single gain value is split between the LNA (8 dB steps) and the VGA (2 dB steps).
                // The RF amplifier stays off: it could overdrive the LNB output.
                let db = gain.unwrap_or(44.0).clamp(0.0, 102.0);
                // The LNA gets priority (better for noise): 44 dB → LNA 24 + VGA 20.
                let lna = ((db * 0.6 / 8.0).floor() * 8.0).min(40.0);
                let vga = (((db - lna) / 2.0).round() * 2.0).clamp(0.0, 62.0);
                [("AMP", 0.0), ("LNA", lna), ("VGA", vga)]
                    .into_iter()
                    .try_for_each(|(name, value)| device.set_gain_element(Rx, 0, name, value))
                    .map_err(|e| e.to_string())
            }
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

    /// Number of samples read; 0 on transient conditions such as overflow.
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

/// Closes the device and opens it again with all settings. Used when the device rejects
/// a setting while streaming: over USB/IP (WSL) the RTL-SDR stalls every control transfer
/// once streaming has started, but opening it with the new settings still works.
fn reopen(radio: Radio, device: &DeviceInfo, s: &Settings) -> Result<Radio, String> {
    drop(radio);
    Radio::open(device, s)
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
                    if radio.set_frequency(hz).is_err() {
                        radio = reopen(radio, device, &s)?;
                    }
                    // Recording metadata holds a single frequency; close the recording when the frequency changes.
                    stop_recording(&mut recording);
                }
                Ok(Command::Gain(g)) => {
                    s.gain = g;
                    if radio.set_gain(g).is_err() {
                        radio = reopen(radio, device, &s)?;
                    }
                }
                Ok(Command::SampleRate(rate)) => {
                    s.sample_rate = rate;
                    stop_recording(&mut recording);
                    if radio.set_sample_rate(rate).is_err() {
                        radio = reopen(radio, device, &s)?;
                    }
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
            // If the decoder cannot keep up the block is dropped; SDR reading never waits.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// SigMF recording from a real SoapySDR device (for development):
    /// `QODA_CAPTURE="driver=hackrf" QODA_CAPTURE_OUT=path QODA_RATE=10e6 QODA_IF=745.25e6 QODA_RF=10495.25e6
    ///  QODA_GAINS="LNA=24,VGA=20,AMP=0" cargo test --bin qoda capture -- --ignored`
    #[test]
    #[ignore]
    fn capture_from_soapy_device() {
        let (Some(args), Some(out)) = (std::env::var_os("QODA_CAPTURE"), std::env::var_os("QODA_CAPTURE_OUT")) else {
            return;
        };
        let env = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (rate, if_hz, rf_hz) = (env("QODA_RATE", 10e6), env("QODA_IF", 745.25e6), env("QODA_RF", 10495.25e6));
        let device = soapysdr::Device::new(args.to_str().unwrap()).unwrap();
        device.set_sample_rate(Rx, 0, rate).unwrap();
        device.set_frequency(Rx, 0, if_hz, ()).unwrap();
        for item in std::env::var("QODA_GAINS").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
            let (name, value) = item.split_once('=').unwrap();
            device.set_gain_element(Rx, 0, name, value.parse().unwrap()).unwrap();
        }
        let mut stream = device.rx_stream::<Complex<f32>>(&[0]).unwrap();
        stream.activate(None).unwrap();
        let mut buf = vec![Complex::default(); (rate * 3.0) as usize];
        let mut filled = 0;
        while filled < buf.len() {
            filled += stream.read(&mut [&mut buf[filled..]], 1_000_000).unwrap_or(0);
        }
        let rms = (buf.iter().map(|z| z.norm_sqr()).sum::<f32>() / buf.len() as f32).sqrt();
        let peak = buf.iter().map(|z| z.re.abs().max(z.im.abs())).fold(0.0, f32::max);
        println!("RMS {rms:.4}, tepe {peak:.4}");
        let path = std::path::PathBuf::from(out);
        let mut rec = Recording::start(path, rate, rf_hz, if_hz).unwrap();
        rec.write(&buf).unwrap();
        rec.data.flush().unwrap();
    }
}

#[cfg(test)]
mod listing_tests {
    /// Prints the kinds of connected devices: `cargo test --bin qoda listing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn print_device_kinds() {
        for d in super::list_devices() {
            println!("{:?} | {} | {}", d.kind, d.label, d.args);
        }
    }
}

#[cfg(test)]
mod radio_tests {
    use super::*;

    /// Recording along the path the UI uses (Radio + device-kind gain):
    /// `QODA_RADIO_OUT=path cargo test --bin qoda radio_path -- --ignored`
    #[test]
    #[ignore]
    fn radio_path_capture() {
        let Some(out) = std::env::var_os("QODA_RADIO_OUT") else { return };
        let device = list_devices().into_iter().next().expect("no device");
        let kind = device.kind;
        let rate = kind.default_rate() * 1e6;
        // Put the signal beside the centre (the UI's centring offset).
        let rf = 10_491.53e6 + kind.center_offset_mhz(1.8) * 1e6;
        let s = Settings { if_hz: rf - 9_750e6, sample_rate: rate, gain: Some(kind.default_gain()), fft_size: 2048 };
        let mut radio = Radio::open(&device, &s).unwrap();
        let mut buf = vec![Complex::default(); (rate * 3.0) as usize];
        let mut filled = 0;
        while filled < buf.len() {
            filled += radio.read(&mut buf[filled..]).unwrap();
        }
        let peak = buf.iter().map(|z| z.re.abs().max(z.im.abs())).fold(0.0, f32::max);
        println!("{kind:?} {} MS/s, gain {} dB, peak {peak:.3}", rate / 1e6, kind.default_gain());
        let mut rec = Recording::start(std::path::PathBuf::from(out), rate, rf, s.if_hz).unwrap();
        rec.write(&buf).unwrap();
        rec.data.flush().unwrap();
    }
}
