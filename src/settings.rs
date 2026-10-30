//! Kalıcı kullanıcı ayarları: basit `anahtar=değer` satırlarından oluşan bir dosya.
//!
//! Windows'ta `%APPDATA%\QODA\settings.txt`, diğer sistemlerde
//! `$XDG_CONFIG_HOME/qoda/settings.txt` ya da `~/.config/qoda/settings.txt`.

use std::path::PathBuf;

use crate::i18n::Lang;

pub struct UserSettings {
    pub lang: Lang,
    pub lnb_lo_mhz: f64,
    /// Eklenen ağ alıcılarının libiio URI'leri.
    pub network_devices: Vec<String>,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self { lang: Lang::En, lnb_lo_mhz: 9750.0, network_devices: Vec::new() }
    }
}

fn path() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("QODA").join("settings.txt"));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("qoda").join("settings.txt"))
}

impl UserSettings {
    /// Dosya yoksa veya okunamazsa varsayılanlar kullanılır.
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return s;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "lang" => s.lang = Lang::from_code(value.trim()).unwrap_or(s.lang),
                "lnb_lo_mhz" => s.lnb_lo_mhz = value.trim().parse().unwrap_or(s.lnb_lo_mhz),
                "network_device" => s.network_devices.push(value.trim().to_string()),
                _ => {}
            }
        }
        s
    }

    /// Kaydetme hatası arayüzü etkilememeli; sessizce yok sayılır.
    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text = format!("lang={}\nlnb_lo_mhz={}\n", self.lang.code(), self.lnb_lo_mhz);
        for uri in &self.network_devices {
            text += &format!("network_device={uri}\n");
        }
        let _ = std::fs::write(p, text);
    }
}
