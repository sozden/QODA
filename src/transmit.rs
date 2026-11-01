//! Transmitting DVB-S2 on the QO-100 wideband uplink with an ADALM-Pluto: making a test
//! broadcast, modulating, preparing the samples for the DAC and the transmit loop. Shared by
//! the `qoda-tx` command line tool and the `qoda-tx-gui` window.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use num_complex::Complex;

use crate::dvbs2::plheader::Pls;
use crate::dvbs2::tx::{Modulator, NULL_PACKET, Shaper, TS_LEN, TxConfig};
use crate::iio::IioTx;

/// QO-100 wideband transponder: downlink = uplink + this (Hz).
pub const UPLINK_OFFSET_HZ: f64 = 8_089.5e6;
/// The wideband transponder's uplink band (Hz); the whole signal must stay inside it.
pub const UPLINK_BAND: (f64, f64) = (2_401.5e6, 2_409.5e6);
/// Lowest sample rate of the AD9363 in the ADALM-Pluto without its extra FIR filter (S/s).
pub const MIN_SAMPLE_RATE: f64 = 2.1e6;
/// The sample rate is at least this many times the shifted signal's outer edge, which keeps it
/// inside the flat part of the AD9361's analog filter (set to the sample rate).
const SHIFT_RATE_FACTOR: f64 = 2.5;
/// Default RMS level of the samples sent to the DAC (dB below full scale). The peaks of the
/// RRC-shaped signal reach about 1.7 times (4.6 dB above) the RMS; at -7 dBFS they stay below
/// full scale. -10.5 dBFS was too weak on the first air test.
pub const DEFAULT_LEVEL_DBFS: f64 = -7.0;
/// Allowed range of the level; above -4 dBFS the peaks clip.
pub const LEVEL_RANGE_DBFS: (f64, f64) = (-30.0, -4.0);
pub const DEFAULT_ATTENUATION_DB: f64 = 30.0;
pub const DEFAULT_SECONDS: f64 = 30.0;
pub const MAX_SECONDS: f64 = 600.0;
/// Symbol rates offered in the window (S/s): the common QO-100 DATV rates.
pub const SYMBOL_RATES: [f64; 8] = [33e3, 66e3, 125e3, 250e3, 333e3, 500e3, 1000e3, 1500e3];
/// Length of the test broadcast; it repeats while transmitting.
const PATTERN_SECONDS: u32 = 20;
/// Channels below this TS rate (bit/s) get a test broadcast without audio.
const LOW_RATE: f64 = 100e3;

/// The signal and where it goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub symbol_rate: f64,
    pub cfg: TxConfig,
    /// Downlink frequency of the signal's centre (Hz).
    pub downlink_hz: f64,
    /// Correction added to the transmit frequency for the Pluto's oscillator error (Hz).
    pub offset_hz: f64,
    /// Distance of the signal above the transmitter's LO (Hz); `None` picks it from the width.
    pub shift_hz: Option<f64>,
    /// Samples per symbol; `None` picks it from the rate and the shift.
    pub sps: Option<usize>,
}

impl Plan {
    /// Half of the occupied bandwidth (Hz).
    pub fn half_width(&self) -> f64 {
        self.symbol_rate * (1.0 + self.cfg.rolloff as f64) / 2.0
    }

    pub fn uplink_hz(&self) -> f64 {
        self.downlink_hz - UPLINK_OFFSET_HZ
    }

    /// How far the signal is moved from the transmitter's LO (Hz): the Pluto leaks some of its
    /// LO, which otherwise sits as a carrier in the middle of the signal and costs SNR there. 1.5
    /// times the signal's half width plus 50 kHz puts it clear of the signal's edge.
    pub fn shift(&self) -> f64 {
        self.shift_hz.unwrap_or(1.5 * self.half_width() + 50e3)
    }

    /// The transmitter's LO frequency (Hz), the oscillator correction included.
    pub fn lo_hz(&self) -> f64 {
        self.uplink_hz() - self.shift() + self.offset_hz
    }

    /// Integer samples per symbol, at least the Pluto's lowest rate, with room for the shift.
    pub fn samples_per_symbol(&self, transmitting: bool) -> usize {
        let shift = if transmitting { self.shift().abs() } else { 0.0 };
        let min_rate = MIN_SAMPLE_RATE.max(SHIFT_RATE_FACTOR * (shift + self.half_width()));
        self.sps.unwrap_or_else(|| (min_rate / self.symbol_rate).ceil().max(2.0) as usize)
    }

    pub fn sample_rate(&self, transmitting: bool) -> f64 {
        self.samples_per_symbol(transmitting) as f64 * self.symbol_rate
    }

    /// MPEG-TS bit rate the channel carries (bit/s).
    pub fn capacity(&self) -> Option<f64> {
        self.cfg.ts_bitrate(self.symbol_rate)
    }

    /// Whether the signal, and the LO whose leakage goes out too, lie in the uplink band.
    pub fn check_band(&self) -> Result<(), String> {
        let (up, half) = (self.uplink_hz(), self.half_width());
        if up - half < UPLINK_BAND.0 || up + half > UPLINK_BAND.1 {
            return Err(format!(
                "the signal ({:.4}–{:.4} MHz uplink) must lie inside the wideband transponder's uplink, {:.1}–{:.1} MHz \
                 (downlink {:.2}–{:.2} MHz)",
                (up - half) / 1e6,
                (up + half) / 1e6,
                UPLINK_BAND.0 / 1e6,
                UPLINK_BAND.1 / 1e6,
                (UPLINK_BAND.0 + UPLINK_OFFSET_HZ) / 1e6,
                (UPLINK_BAND.1 + UPLINK_OFFSET_HZ) / 1e6
            ));
        }
        let lo = up - self.shift();
        if !(UPLINK_BAND.0..=UPLINK_BAND.1).contains(&lo) {
            return Err(format!(
                "the transmitter LO ({:.4} MHz, {:.0} kHz below the signal) would fall outside the uplink band; \
                 choose another frequency or a smaller shift",
                lo / 1e6,
                self.shift() / 1e3
            ));
        }
        let rate = self.sample_rate(true);
        if self.shift().abs() + half > rate / 2.0 * 0.8 {
            return Err(format!("a shift of {:.0} kHz does not fit in {:.3} MS/s", self.shift() / 1e3, rate / 1e6));
        }
        Ok(())
    }
}

/// "QPSK 1/2" and the like.
pub fn modcod_name(modcod: u8) -> String {
    Pls { modcod, short_frame: false, pilots: false }
        .modulation_and_rate()
        .map_or(format!("MODCOD {modcod}"), |(m, (n, d))| format!("{} {n}/{d}", m.name()))
}

/// The MODCOD number for a name like "QPSK 1/2" (QPSK, 8PSK and 16APSK).
pub fn parse_modcod(text: &str) -> Result<u8, String> {
    (1..=23)
        .find(|&m| modcod_name(m).eq_ignore_ascii_case(text.trim()))
        .ok_or(format!("unknown MODCOD \"{text}\" (e.g. \"QPSK 1/2\", \"8PSK 2/3\")"))
}

/// The text of the test card for a plan: frequency, symbol rate and MODCOD.
pub fn pattern_caption(plan: &Plan) -> String {
    format!(
        "{:.3} MHz  ·  {:.0} kS/s  ·  {}",
        plan.downlink_hz / 1e6,
        plan.symbol_rate / 1e3,
        modcod_name(plan.cfg.modcod)
    )
}

/// Makes a test broadcast with FFmpeg: colour bars with the call sign, "QODA TX · QO-100 DATV"
/// and `caption` below them, a frame counter that shows the picture is live, a 1 kHz tone and
/// `call` as the service name. The picture is still, so even 125 kS/s carries it sharp. The TS is
/// muxed at the channel's bit rate, so it plays at the right speed when sent in a loop.
/// Returns the file and a one-line description.
pub fn make_pattern(call: &str, caption: &str, capacity: f64) -> Result<(PathBuf, String), String> {
    // Only the characters of a call sign go into the FFmpeg filter text.
    let call: String = call.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '/' || *c == '-').collect::<String>().to_uppercase();
    let caption: String = caption.chars().filter(|c| !matches!(c, '\'' | '\\' | ':' | '%' | ',' | ';')).collect();
    let ffmpeg = crate::ffmpeg::find_tool("ffmpeg").ok_or("FFmpeg not found; it is needed for the test broadcast")?;
    let mux = (capacity / 1000.0).floor() as u32;
    let path = std::env::temp_dir().join(format!("qoda-pattern-{mux}k.ts"));
    // FFmpeg exceeds the mux rate when the streams do not fit; the audio packets and tables
    // cost much more than their nominal rate at low rates. Measure and lower the video rate.
    let audio = if capacity < LOW_RATE { 0.0 } else { 40e3 };
    let mut video = ((capacity * 0.80 - audio) / 1000.0).max(20.0);
    for _ in 0..5 {
        run_ffmpeg_pattern(&ffmpeg, &call, &caption, capacity, video as u32, mux, &path)?;
        let bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        // A file that fits is padded to the mux rate, but FFmpeg runs slightly past the
        // duration, so it measures up to ~1% above the mux rate; a real overflow is far more.
        let actual = bytes as f64 * 8.0 / PATTERN_SECONDS as f64 / 1000.0;
        if actual <= capacity / 1000.0 * 1.02 {
            let sound = if capacity < LOW_RATE { "no audio" } else { "MP2 32 kbit/s" };
            return Ok((path, format!("Test broadcast \"{call}\": H.264 {video:.0} kbit/s + {sound}, muxed at {mux} kbit/s")));
        }
        if video <= 20.0 {
            break;
        }
        video = (video - (actual - mux as f64) * 1.2).max(20.0);
    }
    Err(format!("the test broadcast does not fit in {mux} kbit/s; use a higher symbol rate or code rate"))
}

/// A font for the test card's text: (regular, bold). `None` makes a card without text.
fn pattern_fonts() -> Option<(String, String)> {
    let candidates: &[(&str, &str)] = &[
        ("C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/arialbd.ttf"),
        ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
        ("/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"),
        ("/System/Library/Fonts/Supplemental/Arial.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf"),
    ];
    candidates
        .iter()
        .find(|(r, b)| Path::new(r).is_file() && Path::new(b).is_file())
        // FFmpeg filter syntax: the colon of a drive letter is escaped.
        .map(|(r, b)| (r.replace(':', "\\:"), b.replace(':', "\\:")))
}

/// The test card filter: colour bars on top, a dark band with the text below.
fn pattern_filter(size: &str, fps: u32, call: &str, caption: &str) -> String {
    let mut f = format!("smptebars=size={size}:rate={fps},drawbox=x=0:y=ih*0.62:w=iw:h=ih*0.40:color=0x0B0F16:t=fill");
    if let Some((regular, bold)) = pattern_fonts() {
        f += &format!(
            ",drawtext=fontfile='{bold}':text='{call}':fontsize=h/7:fontcolor=white:x=(w-text_w)/2:y=h*0.645\
             ,drawtext=fontfile='{regular}':text='QODA TX  ·  QO-100 DATV':fontsize=h/18:fontcolor=0x38C8F0:x=(w-text_w)/2:y=h*0.81\
             ,drawtext=fontfile='{regular}':text='{caption}':fontsize=h/22:fontcolor=0xD6DEEB:x=(w-text_w)/2:y=h*0.89\
             ,drawtext=fontfile='{regular}':text='%{{n}}':fontsize=h/22:fontcolor=white:box=1:boxcolor=black@0.6:x=w-text_w-8:y=8"
        );
    }
    f
}

fn run_ffmpeg_pattern(ffmpeg: &Path, call: &str, caption: &str, capacity: f64, video: u32, mux: u32, path: &Path) -> Result<(), String> {
    // The card is still, so it takes few bits; the size grows with the channel.
    let (size, fps) = match capacity {
        c if c < LOW_RATE => ("320x180", 10),
        c if c < 200e3 => ("480x270", 15),
        c if c < 600e3 => ("640x360", 25),
        _ => ("960x540", 25),
    };
    // A key frame every 5 s at low rates (a still picture needs few), every 2 s otherwise.
    let gop = if capacity < 200e3 { fps * 5 } else { fps * 2 };
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &pattern_filter(size, fps, call, caption)])
        .args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000"])
        .args(["-t", &PATTERN_SECONDS.to_string()])
        .args(["-c:v", "libx264", "-preset", "medium", "-tune", "stillimage", "-pix_fmt", "yuv420p"])
        .args(["-b:v", &format!("{video}k"), "-maxrate", &format!("{video}k"), "-bufsize", &format!("{}k", video * 2)])
        .args(["-g", &gop.to_string()])
        // Below ~100 kbit/s the tone's 32 kbit/s would leave too little for the picture.
        .args(if capacity < LOW_RATE { vec!["-an"] } else { vec!["-c:a", "mp2", "-b:a", "32k"] })
        .args(["-f", "mpegts", "-muxrate", &format!("{mux}k")])
        // FFmpeg's default PCR every 20 ms takes a packet of its own at low rates (75 kbit/s);
        // DVB allows up to 100 ms. Tables every 0.5 s (PAT/PMT) and 1 s (SDT) instead of 0.1 s.
        .args(["-pcr_period", "80", "-pat_period", "0.5", "-sdt_period", "1"])
        .args(["-metadata", &format!("service_name={call}"), "-metadata", "service_provider=QODA"])
        .arg(path);
    hide_window(&mut cmd);
    let status = cmd.status().map_err(|e| format!("could not start FFmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("FFmpeg failed ({status})"));
    }
    Ok(())
}

/// Keeps FFmpeg from opening a console window when started from the window program.
fn hide_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// The whole 188-byte TS packets of a file.
pub fn read_ts(path: &Path) -> Result<Vec<u8>, String> {
    let ts = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let start = ts.iter().position(|&b| b == 0x47).ok_or("no MPEG-TS sync byte in the input")?;
    Ok(ts[start..].chunks_exact(TS_LEN).filter(|p| p[0] == 0x47).flatten().copied().collect())
}

/// All packets → RRC-shaped samples; the last frame is filled with null packets.
pub fn modulate(packets: &[u8], cfg: TxConfig, sps: usize) -> Result<Vec<Complex<f32>>, String> {
    let mut modulator = Modulator::new(cfg)?;
    modulator.push_ts(packets);
    let dfl = cfg.data_field_bytes().ok_or("invalid MODCOD")?;
    let short = (dfl - modulator.queued() % dfl) % dfl;
    for _ in 0..short.div_ceil(TS_LEN) {
        modulator.push_ts(&NULL_PACKET);
    }
    let mut shaper = Shaper::new(sps, cfg.rolloff);
    let mut samples = Vec::new();
    while let Some(frame) = modulator.next_frame() {
        shaper.process(&frame, &mut samples);
    }
    Ok(samples)
}

/// Scales to the DAC at a fixed RMS level, so the PA drive is set by the level and the
/// attenuation, and moves the signal `shift` up from the LO. Where the loop wraps around the
/// phase jumps once per pass, which the receiver's carrier tracking rides through.
pub fn prepare_for_air(samples: &[Complex<f32>], level_dbfs: f64, shift_hz: f64, sample_rate: f64) -> Vec<Complex<f32>> {
    let rms = (samples.iter().map(|z| z.norm_sqr() as f64).sum::<f64>() / samples.len().max(1) as f64).sqrt() as f32;
    let scale = 10f32.powf(level_dbfs as f32 / 20.0) / rms.max(1e-12);
    let step = std::f64::consts::TAU * shift_hz / sample_rate;
    samples
        .iter()
        .enumerate()
        .map(|(n, z)| z * scale * Complex::from_polar(1.0, (step * n as f64).rem_euclid(std::f64::consts::TAU) as f32))
        .collect()
}

/// How a transmission runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AirSettings {
    pub level_dbfs: f64,
    pub attenuation_db: f64,
    pub seconds: f64,
    /// Through the AD9361's digital loopback with the transmitter off: nothing is radiated.
    pub loopback: bool,
}

impl AirSettings {
    pub fn check(&self) -> Result<(), String> {
        if !(0.0..=MAX_SECONDS).contains(&self.seconds) {
            return Err(format!("the time on air must be between 0 and {MAX_SECONDS} s"));
        }
        if !(LEVEL_RANGE_DBFS.0..=LEVEL_RANGE_DBFS.1).contains(&self.level_dbfs) {
            return Err(format!("the level must be between {} and {} dBFS", LEVEL_RANGE_DBFS.0, LEVEL_RANGE_DBFS.1));
        }
        Ok(())
    }
}

/// What the transmit loop reports as it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    /// A line about the setup (device settings, frequencies).
    Info(String),
    Warning(String),
    /// The samples go out from now on (or into the loopback).
    Started,
    /// Seconds since the start and the sample rate the link achieves.
    Progress { elapsed: f64, achieved: f64 },
}

/// The end of a transmission.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub elapsed: f64,
    pub achieved: f64,
    /// The samples did not reach the device as fast as it sends them.
    pub starved: bool,
}

/// Sends `samples` (already prepared for the DAC) in a loop until `settings.seconds` pass or
/// `stop` is set. The transmitter is off before and after.
pub fn transmit(
    uri: &str,
    samples: &[Complex<f32>],
    plan: &Plan,
    settings: &AirSettings,
    stop: &AtomicBool,
    report: &mut dyn FnMut(Report),
) -> Result<Summary, String> {
    plan.check_band()?;
    settings.check()?;
    if samples.is_empty() {
        return Err("nothing to transmit".into());
    }
    let sample_rate = plan.sample_rate(true);
    let mut radio = IioTx::open(uri)?;
    // If the device refuses at its current rate, try again from a rate that needs no FIR.
    if radio.disable_fir().is_err() {
        radio.set_sample_rate(4e6)?;
        if let Err(e) = radio.disable_fir() {
            report(Report::Warning(format!("{e}; the signal may come out tilted")));
        }
    }
    radio.set_sample_rate(sample_rate)?;
    radio.set_frequency(plan.lo_hz())?;
    // What the device made of the settings, for diagnosing the transmit path.
    for line in radio.describe().lines() {
        report(Report::Info(format!("pluto {line}")));
    }
    if settings.loopback {
        // The LO stays powered down and the attenuation at maximum, as `open` left them.
        radio.set_loopback(1)?;
        report(Report::Info(format!("LOOPBACK TEST, nothing is transmitted: {:.3} MS/s through the digital loopback", sample_rate / 1e6)));
    } else {
        radio.set_loopback(0)?;
        report(Report::Info(format!(
            "downlink {:.4} MHz, uplink {:.4} MHz (TX LO {:.4} MHz: {:.0} kHz below the signal, {:+.1} kHz correction)",
            plan.downlink_hz / 1e6,
            plan.uplink_hz() / 1e6,
            plan.lo_hz() / 1e6,
            plan.shift() / 1e3,
            plan.offset_hz / 1e3
        )));
        report(Report::Info(format!("level {:.1} dBFS, attenuation {:.2} dB, up to {:.0} s", settings.level_dbfs, settings.attenuation_db, settings.seconds)));
        radio.set_attenuation(settings.attenuation_db)?;
        radio.power_on()?;
    }
    report(Report::Started);

    let chunk = radio.buffer_samples();
    let started = Instant::now();
    let mut last_report = Instant::now();
    let (mut position, mut sent, mut starved) = (0usize, 0usize, false);
    let mut block = Vec::with_capacity(chunk);
    let result = loop {
        if stop.load(Ordering::Relaxed) || started.elapsed().as_secs_f64() >= settings.seconds {
            break Ok(());
        }
        // Loop over the signal, wrapping around at its end.
        block.clear();
        while block.len() < chunk {
            let take = (chunk - block.len()).min(samples.len() - position);
            block.extend_from_slice(&samples[position..position + take]);
            position = (position + take) % samples.len();
        }
        if let Err(e) = radio.write(&block) {
            break Err(e);
        }
        sent += chunk;
        if last_report.elapsed() >= Duration::from_millis(500) {
            last_report = Instant::now();
            // The device takes samples at the sample rate; if they arrive slower, its buffer
            // runs dry and only the carrier leaking from the LO goes out.
            let elapsed = started.elapsed().as_secs_f64();
            let achieved = sent as f64 / elapsed;
            starved |= elapsed > 3.0 && achieved < sample_rate * 0.98;
            report(Report::Progress { elapsed, achieved });
        }
    };
    if settings.loopback {
        let _ = radio.set_loopback(0);
    }
    // Dropping the radio switches the transmitter off.
    drop(radio);
    result?;
    let elapsed = started.elapsed().as_secs_f64();
    if starved {
        report(Report::Warning(format!(
            "the samples did not reach the Pluto as fast as it sends them ({:.3} MS/s, about {:.0} Mbit/s); its buffer \
             ran dry, so mostly an unmodulated carrier went out. A faster link or a lower symbol rate helps",
            sample_rate / 1e6,
            sample_rate * 32.0 / 1e6
        )));
    }
    Ok(Summary { elapsed, achieved: sent as f64 / elapsed.max(1e-9), starved })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(downlink_mhz: f64) -> Plan {
        Plan {
            symbol_rate: 125e3,
            cfg: TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 },
            downlink_hz: downlink_mhz * 1e6,
            offset_hz: 0.0,
            shift_hz: None,
            sps: None,
        }
    }

    #[test]
    fn plan_frequencies_and_band() {
        let p = plan(10496.777);
        assert!((p.uplink_hz() - 2407.277e6).abs() < 1.0);
        assert!((p.shift() - 162.5e3).abs() < 1.0);
        assert!((p.lo_hz() - 2407.1145e6).abs() < 1.0);
        assert_eq!(p.samples_per_symbol(true), 17);
        assert!(p.check_band().is_ok());
        // Signal past the top of the band; LO below the bottom.
        assert!(plan(10499.0).check_band().is_err());
        assert!(plan(10491.1).check_band().is_err());
    }

    #[test]
    fn modcod_names_round_trip() {
        for m in 1..=23 {
            assert_eq!(parse_modcod(&modcod_name(m)), Ok(m));
        }
        assert!(parse_modcod("QPSK 7/8").is_err());
    }
}
