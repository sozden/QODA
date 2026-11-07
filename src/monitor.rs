//! Monitor output: sends the decoded MPEG-TS and the receiver's telemetry over UDP, so another
//! program can show them (VLC on the TS, or the DATV-Monitor sender, which forwards both to an
//! M5Stack Core2 screen).
//!
//! - TS: 7 packets (1316 bytes) per datagram to `host:ts_port`.
//! - Telemetry: to `host:telemetry_port`, four times a second; a datagram is `T` followed by
//!   `key=value` lines, or `C` followed by constellation points as int8 I,Q pairs (1.0 = 90).
//! - Spectrum: to the same port, about eight times a second whenever the receiver runs (also
//!   while not watching); see [`spectrum_datagram`] for the layout.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use crate::dvbs2::receiver::ReceiverStats;

const TS_PACKET: usize = 188;
const PACKETS_PER_DATAGRAM: usize = 7;
const TELEMETRY_PERIOD: Duration = Duration::from_millis(250);
/// int8 value of a unit-amplitude constellation point.
const CONSTELLATION_SCALE: f32 = 90.0;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_TS_PORT: u16 = 1234;
pub const DEFAULT_TELEMETRY_PORT: u16 = 5555;

/// Where to send and what the receiver is tuned to (the decoder does not know the RF frequency).
#[derive(Clone)]
pub struct MonitorConfig {
    pub host: String,
    pub ts_port: u16,
    pub telemetry_port: u16,
    pub frequency_mhz: f64,
    pub symbol_rate_ks: f64,
}

pub struct Monitor {
    socket: UdpSocket,
    ts_addr: String,
    telemetry_addr: String,
    cfg: MonitorConfig,
    pending: Vec<u8>,
    last_telemetry: Option<Instant>,
}

impl Monitor {
    pub fn new(cfg: MonitorConfig) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        // A missing listener must never hold up the decoder.
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            ts_addr: format!("{}:{}", cfg.host, cfg.ts_port),
            telemetry_addr: format!("{}:{}", cfg.host, cfg.telemetry_port),
            cfg,
            pending: Vec::new(),
            last_telemetry: None,
        })
    }

    /// Sends whole datagrams of TS packets; a remainder waits for the next call.
    /// Send errors (no listener, full buffer) are ignored: the output is best effort.
    pub fn send_ts(&mut self, ts: &[u8]) {
        self.pending.extend_from_slice(ts);
        let size = TS_PACKET * PACKETS_PER_DATAGRAM;
        let whole = self.pending.len() / size * size;
        for chunk in self.pending[..whole].chunks(size) {
            let _ = self.socket.send_to(chunk, &self.ts_addr);
        }
        self.pending.drain(..whole);
    }

    /// Sends telemetry and constellation if `TELEMETRY_PERIOD` has passed since the last time.
    pub fn maybe_send_telemetry(&mut self, stats: &ReceiverStats) {
        if self.last_telemetry.is_some_and(|t| t.elapsed() < TELEMETRY_PERIOD) {
            return;
        }
        self.last_telemetry = Some(Instant::now());
        let _ = self.socket.send_to(&telemetry_datagram(stats, &self.cfg), &self.telemetry_addr);
        if let Some(c) = constellation_datagram(stats) {
            let _ = self.socket.send_to(&c, &self.telemetry_addr);
        }
    }
}

/// Text form of the telemetry; keys are those of DATV-Monitor's protocol.
pub fn telemetry_datagram(s: &ReceiverStats, cfg: &MonitorConfig) -> Vec<u8> {
    let mut text = String::from("Tsrc=QODA\n");
    // 1 only while frames decode: locked on a signal too weak for its MODCOD shows as no lock.
    text += &format!("lock={}\n", u8::from(s.locked && s.decoding));
    text += &format!("freq={:.3}\nsr={}\n", cfg.frequency_mhz, cfg.symbol_rate_ks.round());
    if let Some((m, (n, d))) = s.pls.and_then(|p| p.modulation_and_rate()) {
        text += &format!("modcod={} {n}/{d}\n", m.name());
    }
    if s.frames_ok + s.frames_failed > 0 {
        text += &format!("snr={:.1}\n", s.snr_db);
    }
    text += &format!("ok={}\nbad={}\nldpc={}\n", s.frames_ok, s.frames_failed, s.ldpc_iterations);
    // Names come from the broadcast: keep them on one line.
    let clean = |v: &str| v.replace(['\n', '\r'], " ");
    if let Some(name) = &s.service.name {
        text += &format!("svc={}\n", clean(name));
    }
    if let Some(p) = &s.service.provider {
        text += &format!("prov={}\n", clean(p));
    }
    text.into_bytes()
}

/// Number of spectrum points sent: the width of the M5Stack Core2 screen.
pub const SPECTRUM_POINTS: usize = 320;
const SPECTRUM_PERIOD: Duration = Duration::from_millis(125);

/// What the receiver shows: spectrum, its scale and the selected signal.
pub struct SpectrumView<'a> {
    pub center_mhz: f64,
    pub span_mhz: f64,
    /// Power per FFT bin (dB), lowest frequency first.
    pub db: &'a [f32],
    /// Display range (dB): floor and top.
    pub range: (f32, f32),
    pub selected_mhz: f64,
    pub selected_bandwidth_mhz: f64,
    pub watching: bool,
}

/// `S`, then little-endian: centre MHz (f64), span MHz (f32), selected signal's offset from the
/// centre MHz (f32), its bandwidth MHz (f32), floor and top dB (i16 each), flags (u8, bit 0 =
/// watching), point count (u16), then one byte per point (0 = floor, 255 = top). Each point is the
/// highest bin it covers, so narrow signals stay visible.
pub fn spectrum_datagram(v: &SpectrumView) -> Vec<u8> {
    let n = SPECTRUM_POINTS;
    let (lo, hi) = v.range;
    let scale = 255.0 / (hi - lo).max(1.0);
    let mut out = Vec::with_capacity(28 + n);
    out.push(b'S');
    out.extend_from_slice(&v.center_mhz.to_le_bytes());
    out.extend_from_slice(&(v.span_mhz as f32).to_le_bytes());
    out.extend_from_slice(&((v.selected_mhz - v.center_mhz) as f32).to_le_bytes());
    out.extend_from_slice(&(v.selected_bandwidth_mhz as f32).to_le_bytes());
    out.extend_from_slice(&(lo.round() as i16).to_le_bytes());
    out.extend_from_slice(&(hi.round() as i16).to_le_bytes());
    out.push(u8::from(v.watching));
    out.extend_from_slice(&(n as u16).to_le_bytes());
    let bins = v.db.len();
    for i in 0..n {
        let a = i * bins / n;
        let b = ((i + 1) * bins / n).max(a + 1).min(bins);
        let peak = v.db[a.min(bins.saturating_sub(1))..b].iter().copied().fold(f32::MIN, f32::max);
        out.push(((peak - lo) * scale).round().clamp(0.0, 255.0) as u8);
    }
    out
}

/// Sends the spectrum from the UI thread at `SPECTRUM_PERIOD`; errors are ignored like the rest
/// of the monitor output.
pub struct SpectrumSender {
    socket: UdpSocket,
    addr: String,
    last: Option<Instant>,
}

impl SpectrumSender {
    pub fn new(host: &str, port: u16) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_nonblocking(true)?;
        Ok(Self { socket, addr: format!("{host}:{port}"), last: None })
    }

    pub fn maybe_send(&mut self, v: &SpectrumView) {
        if v.db.is_empty() || self.last.is_some_and(|t| t.elapsed() < SPECTRUM_PERIOD) {
            return;
        }
        self.last = Some(Instant::now());
        let _ = self.socket.send_to(&spectrum_datagram(v), &self.addr);
    }
}

pub fn constellation_datagram(s: &ReceiverStats) -> Option<Vec<u8>> {
    if s.constellation.is_empty() {
        return None;
    }
    let to_i8 = |v: f32| (v * CONSTELLATION_SCALE).round().clamp(-127.0, 127.0) as i8 as u8;
    let mut out = Vec::with_capacity(1 + 2 * s.constellation.len());
    out.push(b'C');
    for p in &s.constellation {
        out.push(to_i8(p.re));
        out.push(to_i8(p.im));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_complex::Complex;
    use crate::dvbs2::plheader::Pls;

    fn cfg() -> MonitorConfig {
        MonitorConfig {
            host: DEFAULT_HOST.into(),
            ts_port: 0,
            telemetry_port: 0,
            frequency_mhz: 10491.5,
            symbol_rate_ks: 1500.0,
        }
    }

    #[test]
    fn telemetry_has_the_monitor_keys() {
        let mut s = ReceiverStats { locked: true, decoding: true, frames_ok: 5, snr_db: 12.34, ..Default::default() };
        s.pls = Some(Pls::from_code(8 << 2)); // QPSK 4/5, normal, no pilots
        s.service.name = Some("A71A\nX".into());
        let text = String::from_utf8(telemetry_datagram(&s, &cfg())).unwrap();
        assert!(text.starts_with('T'));
        for line in ["lock=1", "freq=10491.500", "sr=1500", "modcod=QPSK 4/5", "snr=12.3", "ok=5", "svc=A71A X"] {
            assert!(text.lines().any(|l| l.trim_start_matches('T') == line), "{line} missing in {text}");
        }
    }

    #[test]
    fn constellation_is_scaled_and_clamped() {
        let s = ReceiverStats { constellation: vec![Complex::new(1.0, -0.5), Complex::new(3.0, 0.0)], ..Default::default() };
        let c = constellation_datagram(&s).unwrap();
        assert_eq!(c, vec![b'C', 90, (-45i8) as u8, 127, 0]);
    }

    #[test]
    fn spectrum_keeps_peaks_and_scale() {
        // 640 bins at the floor with one narrow peak at the top of the range.
        let mut db = vec![-80.0f32; 640];
        db[321] = -40.0;
        let v = SpectrumView {
            center_mhz: 10495.25,
            span_mhz: 10.0,
            db: &db,
            range: (-80.0, -40.0),
            selected_mhz: 10496.25,
            selected_bandwidth_mhz: 0.4,
            watching: true,
        };
        let d = spectrum_datagram(&v);
        assert_eq!(d.len(), 28 + SPECTRUM_POINTS);
        assert_eq!(d[0], b'S');
        assert_eq!(f64::from_le_bytes(d[1..9].try_into().unwrap()), 10495.25);
        assert_eq!(f32::from_le_bytes(d[13..17].try_into().unwrap()), 1.0);
        assert_eq!(i16::from_le_bytes(d[21..23].try_into().unwrap()), -80);
        assert_eq!(d[25], 1);
        assert_eq!(u16::from_le_bytes(d[26..28].try_into().unwrap()), 320);
        let points = &d[28..];
        assert_eq!(points[160], 255, "the narrow peak survives the reduction");
        assert_eq!(points.iter().filter(|&&p| p == 0).count(), SPECTRUM_POINTS - 1);
    }

    #[test]
    fn ts_goes_out_in_whole_datagrams() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut m = Monitor::new(MonitorConfig { ts_port: port, ..cfg() }).unwrap();
        m.send_ts(&vec![0x47; TS_PACKET * 10]);
        let mut buf = [0u8; 2048];
        let n = listener.recv(&mut buf).unwrap();
        assert_eq!(n, 1316);
        assert_eq!(m.pending.len(), TS_PACKET * 3);
    }
}
