//! Persistent user settings: a file of simple `key=value` lines.
//!
//! `%APPDATA%\QODA\settings.txt` on Windows, on other systems
//! `$XDG_CONFIG_HOME/qoda/settings.txt` or `~/.config/qoda/settings.txt`.

use std::path::PathBuf;

use crate::i18n::Lang;

pub struct UserSettings {
    pub lang: Lang,
    pub lnb_lo_mhz: f64,
    /// LO to return to after it was set to zero.
    pub saved_lo_mhz: f64,
    /// Audio volume, 1.0 = unchanged.
    pub volume: f32,
    pub auto_level: bool,
    /// libiio URIs of added network receivers.
    pub network_devices: Vec<String>,
    /// Send the TS and telemetry over UDP while watching (see `monitor.rs`).
    pub monitor: bool,
    pub monitor_host: String,
    pub monitor_ts_port: u16,
    pub monitor_telemetry_port: u16,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            lang: Lang::En,
            lnb_lo_mhz: 9750.0,
            saved_lo_mhz: 9750.0,
            volume: 1.0,
            auto_level: false,
            network_devices: Vec::new(),
            monitor: false,
            monitor_host: qoda::monitor::DEFAULT_HOST.to_string(),
            monitor_ts_port: qoda::monitor::DEFAULT_TS_PORT,
            monitor_telemetry_port: qoda::monitor::DEFAULT_TELEMETRY_PORT,
        }
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
    /// Defaults are used if the file is missing or unreadable.
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
                "saved_lo_mhz" => s.saved_lo_mhz = value.trim().parse().unwrap_or(s.saved_lo_mhz),
                "volume" => s.volume = value.trim().parse().unwrap_or(s.volume),
                "auto_level" => s.auto_level = value.trim() == "1",
                "network_device" => s.network_devices.push(value.trim().to_string()),
                "monitor" => s.monitor = value.trim() == "1",
                "monitor_host" if !value.trim().is_empty() => s.monitor_host = value.trim().to_string(),
                "monitor_ts_port" => s.monitor_ts_port = value.trim().parse().unwrap_or(s.monitor_ts_port),
                "monitor_telemetry_port" => {
                    s.monitor_telemetry_port = value.trim().parse().unwrap_or(s.monitor_telemetry_port)
                }
                _ => {}
            }
        }
        s
    }

    /// A save error must not affect the UI; it is silently ignored.
    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text = format!(
            "lang={}\nlnb_lo_mhz={}\nsaved_lo_mhz={}\nvolume={}\nauto_level={}\n",
            self.lang.code(),
            self.lnb_lo_mhz,
            self.saved_lo_mhz,
            self.volume,
            u8::from(self.auto_level)
        );
        text += &format!(
            "monitor={}\nmonitor_host={}\nmonitor_ts_port={}\nmonitor_telemetry_port={}\n",
            u8::from(self.monitor),
            self.monitor_host,
            self.monitor_ts_port,
            self.monitor_telemetry_port
        );
        for uri in &self.network_devices {
            text += &format!("network_device={uri}\n");
        }
        let _ = std::fs::write(p, text);
    }
}
