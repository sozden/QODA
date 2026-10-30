//! libiio üzerinden AD9361 tabanlı alıcılar (ADALM-Pluto, LibreSDR).
//!
//! libiio çalışma anında yüklenir: kurulu değilse program açılır, yalnızca bu
//! cihazlar kullanılamaz. libiio 0.x C API'si kullanılır (Windows'ta PothosSDR
//! ile gelen `libiio.dll`, Linux'ta `libiio.so.0`).

use std::ffi::{CString, c_char, c_void};

use libloading::Library;
use num_complex::Complex;

/// Bir iletimde okunan örnek sayısı (2.4 MS/s'de ~27 ms).
const BUFFER_SAMPLES: usize = 65_536;
/// AD9361 ADC'si 12 bit; örnekler ±2048 aralığında gelir.
const FULL_SCALE: f32 = 2048.0;

type Ptr = *mut c_void;

/// Kullanılan libiio işlevleri.
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
    // İşlev göstericileri geçerli kaldıkça kütüphane açık kalmalı.
    _lib: Library,
}

impl Api {
    fn load() -> Result<Self, String> {
        let names: &[&str] = if cfg!(windows) {
            &["libiio.dll", "iio.dll"]
        } else if cfg!(target_os = "macos") {
            &["libiio.dylib", "iio.framework/iio"]
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
                _lib: lib,
            })
        }
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("NUL içermeyen metin")
}

/// Açık bir AD9361 alıcısı.
pub struct IioRadio {
    api: Api,
    context: Ptr,
    /// ad9361-phy'nin alıcı ayar kanalı (örnekleme, bant, kazanç).
    rx_config: Ptr,
    /// ad9361-phy'nin RX_LO kanalı.
    rx_lo: Ptr,
    buffer: Ptr,
    pending: Vec<Complex<f32>>,
    position: usize,
}

// libiio nesneleri yalnızca bu yapıya ait ve tek iş parçacığında kullanılıyor.
unsafe impl Send for IioRadio {}

impl IioRadio {
    /// `uri` örn. "ip:192.168.1.47" veya "usb:".
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

    /// Örnekleme hızıyla birlikte analog süzgeci de ayarlar.
    pub fn set_sample_rate(&mut self, rate: f64) -> Result<(), String> {
        self.write(self.rx_config, "sampling_frequency", &format!("{}", rate.round() as i64))?;
        let bandwidth = (rate * 0.9).max(200_000.0);
        self.write(self.rx_config, "rf_bandwidth", &format!("{}", bandwidth.round() as i64))
    }

    /// `None` otomatik kazanç (yavaş AGC), aksi halde dB cinsinden sabit kazanç.
    pub fn set_gain(&mut self, gain: Option<f64>) -> Result<(), String> {
        match gain {
            Some(db) => {
                self.write(self.rx_config, "gain_control_mode", "manual")?;
                self.write(self.rx_config, "hardwaregain", &format!("{db:.0}"))
            }
            None => self.write(self.rx_config, "gain_control_mode", "slow_attack"),
        }
    }

    /// `out`u doldurur; gerekirse cihazdan yeni blok okur.
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
                // I ve Q kanalları sırayla, işaretli 16 bit.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Gerçek cihazla deneme: `QODA_IIO_URI=ip:192.168.2.1 cargo test --bin qoda iio -- --ignored`
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
        println!("1 s örnek {elapsed:.2} s'de okundu, RMS {rms:.4}, tepe {peak:.4}");
        assert!(rms > 0.001 && peak < 1.0);
    }
}
