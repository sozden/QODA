//! AD9361-based radios over libiio (ADALM-Pluto, LibreSDR): receiving and transmitting.
//!
//! libiio is loaded at run time: if it is not installed the program still starts
//! and only these devices are unavailable. The libiio 0.x C API is used (on Windows
//! the `libiio.dll` that comes with PothosSDR, on Linux `libiio.so.0`).

use std::ffi::{CString, c_char, c_void};

use libloading::Library;
use num_complex::Complex;

/// Number of samples read per transfer (~27 ms at 2.4 MS/s).
const BUFFER_SAMPLES: usize = 65_536;
/// The AD9361 ADC is 12-bit; samples arrive in the ±2048 range.
const FULL_SCALE: f32 = 2048.0;

type Ptr = *mut c_void;

/// libiio functions in use.
struct Api {
    create_context: unsafe extern "C" fn(*const c_char) -> Ptr,
    context_destroy: unsafe extern "C" fn(Ptr),
    find_device: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    find_channel: unsafe extern "C" fn(Ptr, *const c_char, bool) -> Ptr,
    attr_write: unsafe extern "C" fn(Ptr, *const c_char, *const c_char) -> isize,
    channel_enable: unsafe extern "C" fn(Ptr),
    create_buffer: unsafe extern "C" fn(Ptr, usize, bool) -> Ptr,
    buffer_refill: unsafe extern "C" fn(Ptr) -> isize,
    buffer_start: unsafe extern "C" fn(Ptr) -> *const c_void,
    buffer_end: unsafe extern "C" fn(Ptr) -> *const c_void,
    buffer_destroy: unsafe extern "C" fn(Ptr),
    buffer_push: unsafe extern "C" fn(Ptr) -> isize,
    debug_attr_write: unsafe extern "C" fn(Ptr, *const c_char, *const c_char) -> isize,
    attr_read: unsafe extern "C" fn(Ptr, *const c_char, *mut c_char, usize) -> isize,
    device_attr_read: unsafe extern "C" fn(Ptr, *const c_char, *mut c_char, usize) -> isize,
    // The library must stay open as long as the function pointers are valid.
    _lib: Library,
}

impl Api {
    fn load() -> Result<Self, String> {
        let names: &[&str] = if cfg!(windows) {
            &["libiio.dll", "iio.dll"]
        } else if cfg!(target_os = "macos") {
            // Homebrew's lib folders are not on the default dlopen search path.
            &[
                "libiio.dylib",
                "/opt/homebrew/lib/libiio.dylib",
                "/usr/local/lib/libiio.dylib",
                "/Library/Frameworks/iio.framework/iio",
            ]
        } else {
            &["libiio.so.0", "libiio.so"]
        };
        let lib = names
            .iter()
            .find_map(|&n| unsafe { Library::new(n) }.ok())
            .ok_or("libiio not found")?;
        unsafe {
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| format!("libiio: {e}"))?
                };
            }
            Ok(Self {
                create_context: sym!("iio_create_context_from_uri"),
                context_destroy: sym!("iio_context_destroy"),
                find_device: sym!("iio_context_find_device"),
                find_channel: sym!("iio_device_find_channel"),
                attr_write: sym!("iio_channel_attr_write"),
                channel_enable: sym!("iio_channel_enable"),
                create_buffer: sym!("iio_device_create_buffer"),
                buffer_refill: sym!("iio_buffer_refill"),
                buffer_start: sym!("iio_buffer_start"),
                buffer_end: sym!("iio_buffer_end"),
                buffer_destroy: sym!("iio_buffer_destroy"),
                buffer_push: sym!("iio_buffer_push"),
                debug_attr_write: sym!("iio_device_debug_attr_write"),
                attr_read: sym!("iio_channel_attr_read"),
                device_attr_read: sym!("iio_device_attr_read"),
                _lib: lib,
            })
        }
    }
}

/// Whether libiio can be loaded (for diagnostics).
pub fn library_status() -> Result<(), String> {
    Api::load().map(|_| ())
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("text without NUL")
}

/// An open AD9361 receiver.
pub struct IioRadio {
    api: Api,
    context: Ptr,
    /// The receive settings channel of ad9361-phy (sample rate, bandwidth, gain).
    rx_config: Ptr,
    /// The RX_LO channel of ad9361-phy.
    rx_lo: Ptr,
    buffer: Ptr,
    pending: Vec<Complex<f32>>,
    position: usize,
}

// The libiio objects belong only to this struct and are used on a single thread.
unsafe impl Send for IioRadio {}

impl IioRadio {
    /// `uri` e.g. "ip:192.168.1.47" or "usb:".
    pub fn open(uri: &str) -> Result<Self, String> {
        let api = Api::load()?;
        unsafe {
            let context = (api.create_context)(cstr(uri).as_ptr());
            if context.is_null() {
                return Err(format!("could not connect to {uri}"));
            }
            let find = |dev: &str| (api.find_device)(context, cstr(dev).as_ptr());
            let (phy, rx) = (find("ad9361-phy"), find("cf-ad9361-lpc"));
            if phy.is_null() || rx.is_null() {
                (api.context_destroy)(context);
                return Err(format!("{uri} is not an AD9361 receiver"));
            }
            let channel = |dev, name: &str, output| (api.find_channel)(dev, cstr(name).as_ptr(), output);
            let rx_config = channel(phy, "voltage0", false);
            let rx_lo = channel(phy, "altvoltage0", true);
            let (i, q) = (channel(rx, "voltage0", false), channel(rx, "voltage1", false));
            if [rx_config, rx_lo, i, q].iter().any(|c| c.is_null()) {
                (api.context_destroy)(context);
                return Err(format!("{uri}: receiver channels not found"));
            }
            (api.channel_enable)(i);
            (api.channel_enable)(q);
            let buffer = (api.create_buffer)(rx, BUFFER_SAMPLES, false);
            if buffer.is_null() {
                (api.context_destroy)(context);
                return Err(format!("{uri}: could not create the sample buffer"));
            }
            Ok(Self { api, context, rx_config, rx_lo, buffer, pending: Vec::new(), position: 0 })
        }
    }

    fn write(&self, channel: Ptr, attr: &str, value: &str) -> Result<(), String> {
        let n = unsafe { (self.api.attr_write)(channel, cstr(attr).as_ptr(), cstr(value).as_ptr()) };
        if n < 0 { Err(format!("{attr}={value}: error {n}")) } else { Ok(()) }
    }

    pub fn set_frequency(&mut self, hz: f64) -> Result<(), String> {
        self.write(self.rx_lo, "frequency", &format!("{}", hz.round() as i64))
    }

    /// Sets the analog filter together with the sample rate. The filter is opened to
    /// the sample rate: when narrower, signals near the band edge look tilted on top
    /// and their measurements are skewed.
    pub fn set_sample_rate(&mut self, rate: f64) -> Result<(), String> {
        self.write(self.rx_config, "sampling_frequency", &format!("{}", rate.round() as i64))?;
        let bandwidth = rate.clamp(200_000.0, 40_000_000.0);
        self.write(self.rx_config, "rf_bandwidth", &format!("{}", bandwidth.round() as i64))
    }

    /// `None` for automatic gain (slow AGC), otherwise a fixed gain in dB.
    pub fn set_gain(&mut self, gain: Option<f64>) -> Result<(), String> {
        match gain {
            Some(db) => {
                self.write(self.rx_config, "gain_control_mode", "manual")?;
                self.write(self.rx_config, "hardwaregain", &format!("{db:.0}"))
            }
            None => self.write(self.rx_config, "gain_control_mode", "slow_attack"),
        }
    }

    /// Fills `out`; reads a new block from the device if needed.
    pub fn read(&mut self, out: &mut [Complex<f32>]) -> Result<usize, String> {
        if self.position >= self.pending.len() {
            let n = unsafe { (self.api.buffer_refill)(self.buffer) };
            if n < 0 {
                return Err(format!("sample read failed: error {n}"));
            }
            self.pending.clear();
            self.position = 0;
            unsafe {
                let start = (self.api.buffer_start)(self.buffer) as *const i16;
                let end = (self.api.buffer_end)(self.buffer) as *const i16;
                let raw = std::slice::from_raw_parts(start, end.offset_from(start) as usize);
                // I and Q channels interleaved, signed 16-bit.
                self.pending.extend(
                    raw.chunks_exact(2)
                        .map(|iq| Complex::new(iq[0] as f32 / FULL_SCALE, iq[1] as f32 / FULL_SCALE)),
                );
            }
        }
        let n = out.len().min(self.pending.len() - self.position);
        out[..n].copy_from_slice(&self.pending[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}

impl Drop for IioRadio {
    fn drop(&mut self) {
        unsafe {
            (self.api.buffer_destroy)(self.buffer);
            (self.api.context_destroy)(self.context);
        }
    }
}

/// Samples written per transfer when transmitting.
const TX_BUFFER_SAMPLES: usize = 65_536;
/// Highest TX attenuation of the AD9361 (dB).
pub const MAX_TX_ATTENUATION: f64 = 89.75;

/// An open AD9361 transmitter. The output is switched off (TX LO powered down) when dropped.
pub struct IioTx {
    api: Api,
    context: Ptr,
    phy: Ptr,
    /// The transmit settings channel of ad9361-phy (sample rate, bandwidth, attenuation).
    tx_config: Ptr,
    /// The TX_LO channel of ad9361-phy.
    tx_lo: Ptr,
    buffer: Ptr,
}

unsafe impl Send for IioTx {}

impl IioTx {
    /// Opens the transmitter with the output off; `set_*` then `power_on` before `write`.
    pub fn open(uri: &str) -> Result<Self, String> {
        let api = Api::load()?;
        unsafe {
            let context = (api.create_context)(cstr(uri).as_ptr());
            if context.is_null() {
                return Err(format!("could not connect to {uri}"));
            }
            let find = |dev: &str| (api.find_device)(context, cstr(dev).as_ptr());
            let (phy, tx) = (find("ad9361-phy"), find("cf-ad9361-dds-core-lpc"));
            if phy.is_null() || tx.is_null() {
                (api.context_destroy)(context);
                return Err(format!("{uri} is not an AD9361 transmitter"));
            }
            let channel = |dev, name: &str, output| (api.find_channel)(dev, cstr(name).as_ptr(), output);
            let tx_config = channel(phy, "voltage0", true);
            let tx_lo = channel(phy, "altvoltage1", true);
            let (i, q) = (channel(tx, "voltage0", true), channel(tx, "voltage1", true));
            if [tx_config, tx_lo, i, q].iter().any(|c| c.is_null()) {
                (api.context_destroy)(context);
                return Err(format!("{uri}: transmitter channels not found"));
            }
            let mut radio = Self { api, context, phy, tx_config, tx_lo, buffer: std::ptr::null_mut() };
            // Off and fully attenuated until the caller has set everything up.
            radio.write_attr(tx_lo, "powerdown", "1")?;
            radio.set_attenuation(MAX_TX_ATTENUATION)?;
            (radio.api.channel_enable)(i);
            (radio.api.channel_enable)(q);
            radio.buffer = (radio.api.create_buffer)(tx, TX_BUFFER_SAMPLES, false);
            if radio.buffer.is_null() {
                return Err(format!("{uri}: could not create the transmit buffer"));
            }
            Ok(radio)
        }
    }

    fn write_attr(&self, channel: Ptr, attr: &str, value: &str) -> Result<(), String> {
        let n = unsafe { (self.api.attr_write)(channel, cstr(attr).as_ptr(), cstr(value).as_ptr()) };
        if n < 0 { Err(format!("{attr}={value}: error {n}")) } else { Ok(()) }
    }

    pub fn set_frequency(&mut self, hz: f64) -> Result<(), String> {
        self.write_attr(self.tx_lo, "frequency", &format!("{}", hz.round() as i64))
    }

    /// Sets the sample rate and opens the analog filter to it.
    pub fn set_sample_rate(&mut self, rate: f64) -> Result<(), String> {
        self.write_attr(self.tx_config, "sampling_frequency", &format!("{}", rate.round() as i64))?;
        let bandwidth = rate.clamp(200_000.0, 40_000_000.0);
        self.write_attr(self.tx_config, "rf_bandwidth", &format!("{}", bandwidth.round() as i64))
    }

    /// Output attenuation in dB, 0 (full power) to `MAX_TX_ATTENUATION`, in 0.25 dB steps.
    pub fn set_attenuation(&mut self, db: f64) -> Result<(), String> {
        let db = (db.clamp(0.0, MAX_TX_ATTENUATION) * 4.0).round() / 4.0;
        self.write_attr(self.tx_config, "hardwaregain", &format!("{:.2}", -db))
    }

    /// Switches the transmitter's local oscillator on (the output stays silent until samples arrive).
    pub fn power_on(&mut self) -> Result<(), String> {
        self.write_attr(self.tx_lo, "powerdown", "0")
    }

    /// AD9361 digital loopback: 1 sends the transmit samples straight to the receive path inside
    /// the chip, nothing reaches the antenna port; 0 is normal operation. For testing.
    pub fn set_loopback(&mut self, mode: u8) -> Result<(), String> {
        let n = unsafe { (self.api.debug_attr_write)(self.phy, cstr("loopback").as_ptr(), cstr(&mode.to_string()).as_ptr()) };
        if n < 0 { Err(format!("loopback={mode}: error {n}")) } else { Ok(()) }
    }

    /// The transmit path settings the device reports (rates through the interpolation chain,
    /// FIR filter, analog bandwidth, attenuation), one "name: value" per line, for diagnosis.
    pub fn describe(&self) -> String {
        let read = |f: &dyn Fn(*mut c_char, usize) -> isize| {
            let mut buf = vec![0u8; 512];
            let n = f(buf.as_mut_ptr() as *mut c_char, buf.len());
            if n <= 0 {
                return "?".to_string();
            }
            let end = buf.iter().position(|&b| b == 0).unwrap_or(n as usize);
            String::from_utf8_lossy(&buf[..end]).trim().to_string()
        };
        let mut out = String::new();
        for attr in ["tx_path_rates", "trx_rate_governor", "ensm_mode"] {
            let v = read(&|p, l| unsafe { (self.api.device_attr_read)(self.phy, cstr(attr).as_ptr(), p, l) });
            out += &format!("{attr}: {v}\n");
        }
        for attr in ["sampling_frequency", "rf_bandwidth", "filter_fir_en", "hardwaregain", "rf_port_select"] {
            let v = read(&|p, l| unsafe { (self.api.attr_read)(self.tx_config, cstr(attr).as_ptr(), p, l) });
            out += &format!("{attr}: {v}\n");
        }
        let v = read(&|p, l| unsafe { (self.api.attr_read)(self.tx_lo, cstr("frequency").as_ptr(), p, l) });
        out += &format!("tx_lo: {v}\n");
        out
    }

    /// Number of samples one `write` call takes.
    pub fn buffer_samples(&self) -> usize {
        TX_BUFFER_SAMPLES
    }

    /// Sends `buffer_samples()` samples (shorter input is padded with silence); blocks until the
    /// device has room, which paces the caller to the sample rate. Samples should stay within ±1.
    pub fn write(&mut self, samples: &[Complex<f32>]) -> Result<(), String> {
        unsafe {
            let start = (self.api.buffer_start)(self.buffer) as *mut i16;
            let end = (self.api.buffer_end)(self.buffer) as *const i16;
            let raw = std::slice::from_raw_parts_mut(start, end.offset_from(start) as usize);
            // I and Q interleaved, signed 16-bit; the 12-bit DAC takes the upper bits.
            for (iq, z) in raw.chunks_exact_mut(2).zip(samples.iter().chain(std::iter::repeat(&Complex::default()))) {
                iq[0] = (z.re.clamp(-1.0, 1.0) * 32_767.0) as i16;
                iq[1] = (z.im.clamp(-1.0, 1.0) * 32_767.0) as i16;
            }
            let n = (self.api.buffer_push)(self.buffer);
            if n < 0 { Err(format!("sample write failed: error {n}")) } else { Ok(()) }
        }
    }
}

impl Drop for IioTx {
    fn drop(&mut self) {
        let _ = self.set_attenuation(MAX_TX_ATTENUATION);
        let _ = self.write_attr(self.tx_lo, "powerdown", "1");
        unsafe {
            if !self.buffer.is_null() {
                (self.api.buffer_destroy)(self.buffer);
            }
            (self.api.context_destroy)(self.context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test with a real device: `QODA_IIO_URI=ip:192.168.2.1 cargo test --lib iio -- --ignored`
    #[test]
    #[ignore]
    fn reads_samples_from_device() {
        let Some(uri) = std::env::var_os("QODA_IIO_URI") else { return };
        let mut radio = IioRadio::open(uri.to_str().unwrap()).unwrap();
        radio.set_sample_rate(2.4e6).unwrap();
        radio.set_frequency(741.5e6).unwrap();
        radio.set_gain(Some(38.0)).unwrap();
        let mut buf = vec![Complex::default(); 2_400_000];
        let mut filled = 0;
        let started = std::time::Instant::now();
        while filled < buf.len() {
            filled += radio.read(&mut buf[filled..]).unwrap();
        }
        let elapsed = started.elapsed().as_secs_f64();
        let rms = (buf.iter().map(|z| z.norm_sqr()).sum::<f32>() / buf.len() as f32).sqrt();
        let peak = buf.iter().map(|z| z.re.abs().max(z.im.abs())).fold(0.0, f32::max);
        println!("1 s of samples read in {elapsed:.2} s, RMS {rms:.4}, peak {peak:.4}");
        assert!(rms > 0.001 && peak < 1.0);
    }

    /// Sends a DVB-S2 test signal through the AD9361's digital loopback (transmitter LO off,
    /// full attenuation: nothing is radiated) and records what the receiver gets as SigMF:
    /// `QODA_IIO_LOOPBACK=ip:192.168.1.47 QODA_LOOPBACK_OUT=path cargo test --lib loopback -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn digital_loopback() {
        use crate::dvbs2::tx::{Modulator, Shaper, TS_LEN, TxConfig};
        let (Some(uri), Some(out)) = (std::env::var("QODA_IIO_LOOPBACK").ok(), std::env::var_os("QODA_LOOPBACK_OUT")) else { return };
        let (rs, sps) = (333e3, 7usize);
        let rate = rs * sps as f64;
        let cfg = TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 };
        let mut m = Modulator::new(cfg).unwrap();
        let packets: Vec<u8> = (0..4000u32)
            .flat_map(|i| {
                let mut p = vec![0x47, 0x01, 0x00, 0x10 | (i % 16) as u8];
                p.extend((4..TS_LEN).map(|j| (i as usize * 7 + j) as u8));
                p
            })
            .collect();
        m.push_ts(&packets);
        let mut shaper = Shaper::new(sps, cfg.rolloff);
        let mut signal = Vec::new();
        while let Some(f) = m.next_frame() {
            shaper.process(&f, &mut signal);
        }
        let rms = (signal.iter().map(|z| z.norm_sqr()).sum::<f32>() / signal.len() as f32).sqrt();
        signal.iter_mut().for_each(|z| *z *= 0.3 / rms);

        let mut tx = IioTx::open(&uri).unwrap();
        tx.set_sample_rate(rate).unwrap();
        tx.set_loopback(1).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sender = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let chunk = tx.buffer_samples();
                let mut pos = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let end = (pos + chunk).min(signal.len());
                    tx.write(&signal[pos..end]).unwrap();
                    pos = if end == signal.len() { 0 } else { end };
                }
                tx.set_loopback(0).unwrap();
            })
        };
        let mut rx = IioRadio::open(&uri).unwrap();
        rx.set_sample_rate(rate).unwrap();
        rx.set_gain(Some(0.0)).unwrap();
        let mut buf = vec![Complex::default(); (rate * 4.0) as usize];
        let mut filled = 0;
        while filled < buf.len() {
            filled += rx.read(&mut buf[filled..]).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        sender.join().unwrap();
        let rms = (buf.iter().map(|z| z.norm_sqr()).sum::<f32>() / buf.len() as f32).sqrt();
        let peak = buf.iter().map(|z| z.re.abs().max(z.im.abs())).fold(0.0, f32::max);
        println!("received RMS {rms:.4}, peak {peak:.4}");
        let path = std::path::PathBuf::from(out);
        let bytes: Vec<u8> = buf.iter().flat_map(|z| [z.re.to_le_bytes(), z.im.to_le_bytes()].concat()).collect();
        std::fs::write(path.with_extension("sigmf-data"), bytes).unwrap();
        std::fs::write(
            path.with_extension("sigmf-meta"),
            format!(r#"{{"global":{{"core:datatype":"cf32_le","core:sample_rate":{rate}}},"captures":[{{"core:frequency":0}}]}}"#),
        )
        .unwrap();
    }
}
