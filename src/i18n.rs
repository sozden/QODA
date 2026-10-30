//! Arayüz dilleri. Her dil, bütün arayüz metinlerini içeren bir [`Strings`]
//! tablosudur; yeni bir dil eklemek için `EN` tablosunun bir kopyası çevrilir,
//! [`Lang`]'e bir değer ve `strings`/`code`/`native_name` eşlemeleri eklenir.
//!
//! `{}` içeren metinler [`fill`] ile sırayla doldurulan şablonlardır.

use std::fmt::Display;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Tr,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Tr];

    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::En => &EN,
            Lang::Tr => &TR,
        }
    }

    /// Ayar dosyasında kullanılan kod.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Tr => "tr",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code() == code)
    }

    /// Dilin kendi dilindeki adı (seçicide gösterilir).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Tr => "Türkçe",
        }
    }
}

/// Şablondaki `{}` yerlerini sırayla `args` ile doldurur.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

pub struct Strings {
    pub language: &'static str,
    pub app_subtitle: &'static str,
    pub yatolye_tooltip: &'static str,

    // Üst bar
    pub broadcast_caps: &'static str,
    pub symbol_rate_caps: &'static str,
    pub service_caps: &'static str,
    pub watch: &'static str,
    pub stop_watching: &'static str,
    pub watch_tooltip: &'static str,
    pub start_receiver: &'static str,
    pub stop_receiver: &'static str,

    // Kart başlıkları
    pub card_receiver: &'static str,
    pub card_frequency: &'static str,
    pub card_broadcast: &'static str,
    pub card_watch: &'static str,
    pub card_recording: &'static str,

    // Alıcı ve frekans
    pub refresh: &'static str,
    pub network_hint: &'static str,
    pub network_tooltip: &'static str,
    pub add: &'static str,
    pub sample_rate: &'static str,
    pub gain: &'static str,
    pub auto_gain: &'static str,
    pub lnb_lo: &'static str,
    pub center_rf: &'static str,
    pub intermediate_frequency: &'static str,

    // Yayın
    pub frequency: &'static str,
    pub symbol_rate: &'static str,
    pub rolloff: &'static str,
    pub bandwidth: &'static str,
    pub auto_detect: &'static str,
    pub auto_detect_tooltip: &'static str,
    pub center: &'static str,
    pub center_tooltip: &'static str,
    pub broadcast_hint: &'static str,
    /// {} = ölçülen sembol hızı (kS/s)
    pub measured: &'static str,
    pub near_edge: &'static str,

    // İzleme
    pub audio: &'static str,
    pub not_watching: &'static str,
    pub locked: &'static str,
    pub searching: &'static str,
    pub metric_carrier: &'static str,
    pub metric_frames_ok: &'static str,
    pub metric_frames_failed: &'static str,
    pub metric_ts_packets: &'static str,
    /// {} = sayı
    pub warn_cc: &'static str,
    pub warn_unsupported: &'static str,
    pub warn_iq_dropped: &'static str,
    pub warn_ts_dropped: &'static str,
    pub ffmpeg_missing: &'static str,
    pub ffplay_missing: &'static str,
    /// {} = program adı, {} = hata
    pub player_start_failed: &'static str,
    pub player_closed: &'static str,

    // Kayıt
    pub record: &'static str,
    pub stop_recording: &'static str,
    pub record_tooltip: &'static str,

    // Video ve spektrum
    pub video_waiting: &'static str,
    pub video_searching: &'static str,
    pub live: &'static str,
    pub signals_caps: &'static str,
    pub signals_none: &'static str,
    pub signal_tooltip: &'static str,

    // Çerçeve ve akış tanımları
    pub frame_normal: &'static str,
    pub frame_short: &'static str,
    pub pilots_on: &'static str,
    pub pilots_off: &'static str,
    pub frame_dummy: &'static str,
    /// {} = MODCOD numarası
    pub modcod_reserved: &'static str,
    pub stream_video: &'static str,
    pub stream_audio: &'static str,
    pub stream_data: &'static str,

    // Durum çubuğu
    pub status_no_device: &'static str,
    /// {} = cihaz sayısı
    pub status_devices: &'static str,
    /// {} = cihaz adı
    pub status_opened: &'static str,
    pub status_stopped: &'static str,
    /// {} = dosya yolu
    pub status_saved: &'static str,
    /// {} = hata
    pub status_error: &'static str,
    pub status_recording_open: &'static str,
    pub status_recording_write: &'static str,
    /// {} = MHz
    pub status_no_signal: &'static str,
}

pub static EN: Strings = Strings {
    language: "Language",
    app_subtitle: "QO-100 DATV Receiver",
    yatolye_tooltip: "yeniden atölye working group",

    broadcast_caps: "BROADCAST  ·  scroll or click a digit",
    symbol_rate_caps: "SYMBOL RATE",
    service_caps: "SERVICE",
    watch: "▶  Watch",
    stop_watching: "■  Stop watching",
    watch_tooltip: "Decode the selected broadcast as DVB-S2 and show the video",
    start_receiver: "Start receiver",
    stop_receiver: "Stop receiver",

    card_receiver: "RECEIVER",
    card_frequency: "FREQUENCY",
    card_broadcast: "BROADCAST",
    card_watch: "WATCH",
    card_recording: "RECORDING",

    refresh: "Refresh",
    network_hint: "Pluto/LibreSDR IP, e.g. 192.168.2.1",
    network_tooltip: "Add a Pluto or LibreSDR on the network by its IP address (or a libiio URI)",
    add: "Add",
    sample_rate: "Sample rate",
    gain: "Gain",
    auto_gain: "Auto",
    lnb_lo: "LNB LO",
    center_rf: "Center RF",
    intermediate_frequency: "IF",

    frequency: "Frequency",
    symbol_rate: "Symbol rate",
    rolloff: "Roll-off",
    bandwidth: "Bandwidth",
    auto_detect: "Auto detect",
    auto_detect_tooltip: "Detect the parameters of the broadcast at the selected frequency from the spectrum",
    center: "Center",
    center_tooltip: "Tune the receiver to the selected broadcast",
    broadcast_hint: "Click a broadcast in the spectrum; double-click centers it, the wheel moves the selection, dragging moves the receiver.",
    measured: "Measured {} kS/s",
    near_edge: " · at the edge of the band, center it",

    audio: "Audio",
    not_watching: "Not watching",
    locked: "Locked",
    searching: "Searching for sync",
    metric_carrier: "Carrier",
    metric_frames_ok: "Frames ✓",
    metric_frames_failed: "Frames ✗",
    metric_ts_packets: "TS packets",
    warn_cc: "{} continuity errors (packets missing in the stream)",
    warn_unsupported: "{} frames in an unsupported modulation",
    warn_iq_dropped: "{} IQ blocks dropped",
    warn_ts_dropped: "{} TS chunks dropped",
    ffmpeg_missing: "ffmpeg not found (install FFmpeg and add it to PATH)",
    ffplay_missing: "ffplay not found, audio is off",
    player_start_failed: "Could not start {}: {}",
    player_closed: "Player closed",

    record: "Record IQ",
    stop_recording: "■  Stop recording",
    record_tooltip: "Save raw IQ samples as SigMF in the recordings folder. Changing the frequency or sample rate ends the recording.",

    video_waiting: "Waiting for video…",
    video_searching: "Searching for DVB-S2 sync…",
    live: "LIVE",
    signals_caps: "SIGNALS",
    signals_none: "no signals in this band",
    signal_tooltip: "Select this signal",

    frame_normal: "normal frame",
    frame_short: "short frame",
    pilots_on: "pilots",
    pilots_off: "no pilots",
    frame_dummy: "dummy frame",
    modcod_reserved: "reserved MODCOD {}",
    stream_video: "video",
    stream_audio: "audio",
    stream_data: "private data",

    status_no_device: "No SDR device found",
    status_devices: "{} device(s) found",
    status_opened: "{} opened",
    status_stopped: "Stopped",
    status_saved: "Recording saved: {}",
    status_error: "Error: {}",
    status_recording_open: "Could not open the recording: {}",
    status_recording_write: "Could not write the recording: {}",
    status_no_signal: "No broadcast found near {} MHz",
};

pub static TR: Strings = Strings {
    language: "Dil",
    app_subtitle: "QO-100 DATV Alıcı",
    yatolye_tooltip: "yeniden atölye çalışma grubu",

    broadcast_caps: "YAYIN  ·  rakam üzerinde tekerlek / tıklama",
    symbol_rate_caps: "SEMBOL HIZI",
    service_caps: "SERVİS",
    watch: "▶  İzle",
    stop_watching: "■  İzlemeyi durdur",
    watch_tooltip: "Seçili yayını DVB-S2 olarak çöz ve videoyu göster",
    start_receiver: "Alıcıyı başlat",
    stop_receiver: "Alıcıyı durdur",

    card_receiver: "ALICI",
    card_frequency: "FREKANS",
    card_broadcast: "YAYIN",
    card_watch: "İZLEME",
    card_recording: "KAYIT",

    refresh: "Yenile",
    network_hint: "Pluto/LibreSDR IP, ör. 192.168.2.1",
    network_tooltip: "Ağdaki Pluto veya LibreSDR'ı IP adresiyle (ya da libiio URI'siyle) ekle",
    add: "Ekle",
    sample_rate: "Örnekleme",
    gain: "Kazanç",
    auto_gain: "Otomatik",
    lnb_lo: "LNB LO",
    center_rf: "Merkez RF",
    intermediate_frequency: "Ara frekans",

    frequency: "Frekans",
    symbol_rate: "Sembol hızı",
    rolloff: "Roll-off",
    bandwidth: "Bant genişliği",
    auto_detect: "Otomatik bul",
    auto_detect_tooltip: "Seçili frekanstaki yayının parametrelerini spektrumdan bul",
    center: "Ortala",
    center_tooltip: "Alıcıyı seçili yayına ayarla",
    broadcast_hint: "Spektrumda yayına tıklayın; çift tıklama ortalar, tekerlek kaydırır, sürükleme alıcıyı kaydırır.",
    measured: "Ölçülen {} kS/s",
    near_edge: " · kenara taşıyor, ortalayın",

    audio: "Ses",
    not_watching: "İzleme kapalı",
    locked: "Kilitli",
    searching: "Senkron aranıyor",
    metric_carrier: "Taşıyıcı",
    metric_frames_ok: "Çerçeve ✓",
    metric_frames_failed: "Çerçeve ✗",
    metric_ts_packets: "TS paket",
    warn_cc: "{} CC kopukluğu (yayında eksik paket)",
    warn_unsupported: "{} çerçeve desteklenmeyen modülasyonda",
    warn_iq_dropped: "{} IQ bloğu atlandı",
    warn_ts_dropped: "{} TS parçası atlandı",
    ffmpeg_missing: "ffmpeg bulunamadı (FFmpeg kurulu ve PATH'te olmalı)",
    ffplay_missing: "ffplay bulunamadı, ses kapalı",
    player_start_failed: "{} başlatılamadı: {}",
    player_closed: "Oynatıcı kapandı",

    record: "IQ kaydet",
    stop_recording: "■  Kaydı bitir",
    record_tooltip: "Ham IQ örneklerini SigMF olarak recordings klasörüne kaydet. Frekans veya örnekleme hızı değişince kayıt kapanır.",

    video_waiting: "Video bekleniyor…",
    video_searching: "DVB-S2 senkronu aranıyor…",
    live: "CANLI",
    signals_caps: "YAYINLAR",
    signals_none: "bu bantta yayın görünmüyor",
    signal_tooltip: "Bu yayını seç",

    frame_normal: "normal çerçeve",
    frame_short: "kısa çerçeve",
    pilots_on: "pilotlu",
    pilots_off: "pilotsuz",
    frame_dummy: "dummy çerçeve",
    modcod_reserved: "ayrılmış MODCOD {}",
    stream_video: "video",
    stream_audio: "ses",
    stream_data: "özel veri",

    status_no_device: "SDR cihazı bulunamadı",
    status_devices: "{} cihaz bulundu",
    status_opened: "{} açıldı",
    status_stopped: "Durduruldu",
    status_saved: "Kayıt kaydedildi: {}",
    status_error: "Hata: {}",
    status_recording_open: "Kayıt açılamadı: {}",
    status_recording_write: "Kayıt yazılamadı: {}",
    status_no_signal: "{} MHz civarında yayın bulunamadı",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_placeholders_in_order() {
        assert_eq!(fill("{} of {}", &[&1, &"two"]), "1 of two");
        assert_eq!(fill("none", &[]), "none");
    }

    #[test]
    fn codes_round_trip() {
        for l in Lang::ALL {
            assert_eq!(Lang::from_code(l.code()), Some(l));
        }
    }
}
