//! Modulates an MPEG-TS file into DVB-S2: writes a SigMF recording, or transmits it on the
//! QO-100 wideband uplink through an ADALM-Pluto.
//!
//! Usage:
//!   qoda-tx <input.ts> --out <recording> [signal options]
//!   qoda-tx <input.ts> --pluto <uri> --freq <downlink MHz> [--atten dB] [--seconds S] [signal options]
//! Signal options: [--rs kS/s] [--modcod "QPSK 1/2"] [--short] [--no-pilots] [--rolloff R] [--sps N]
//!
//! A recording can be checked with `qoda-analyze <recording> --stream`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use num_complex::Complex;
use qoda::dvbs2::plheader::Pls;
use qoda::dvbs2::tx::{Modulator, NULL_PACKET, Shaper, TS_LEN, TxConfig};
use qoda::iio::IioTx;

/// Lowest sample rate of the AD9363 in the ADALM-Pluto without its extra FIR filter (S/s).
const MIN_SAMPLE_RATE: f64 = 2.1e6;
/// QO-100 wideband transponder: downlink = uplink + this (Hz).
const UPLINK_OFFSET_HZ: f64 = 8_089.5e6;
/// The wideband transponder's uplink band (Hz); the whole signal must stay inside it.
const UPLINK_BAND: (f64, f64) = (2_401.5e6, 2_409.5e6);
/// Default RMS level of the samples sent to the DAC (dB below full scale). The peaks of the
/// RRC-shaped signal reach about 1.7 times (4.6 dB above) the RMS; at -7 dBFS they stay below
/// full scale. -10.5 dBFS was too weak on the first air test.
const DEFAULT_LEVEL_DBFS: f64 = -7.0;
/// Allowed range of `--level`; above -4 dBFS the peaks clip.
const LEVEL_RANGE_DBFS: (f64, f64) = (-30.0, -4.0);
const DEFAULT_ATTENUATION_DB: f64 = 30.0;
const DEFAULT_SECONDS: f64 = 30.0;
const MAX_SECONDS: f64 = 600.0;

/// Set by Ctrl+C; the transmit loop stops and the transmitter is switched off.
static STOP: AtomicBool = AtomicBool::new(false);

struct Args {
    input: PathBuf,
    /// Make a test broadcast with this call sign as the service name instead of reading `input`.
    pattern: Option<String>,
    output: Option<PathBuf>,
    pluto: Option<String>,
    symbol_rate: f64,
    cfg: TxConfig,
    sps: Option<usize>,
    /// Downlink frequency of the signal's centre (Hz).
    freq: Option<f64>,
    /// Correction added to the transmit frequency for the Pluto's oscillator error (Hz).
    offset: f64,
    attenuation: f64,
    /// RMS level of the samples sent to the DAC (dBFS).
    level: f64,
    seconds: f64,
    /// Send the samples through the AD9361's digital loopback with the transmitter off:
    /// measures whether the link keeps up without radiating anything.
    loopback: bool,
}

const USAGE: &str = "usage:
  qoda-tx <input.ts | --pattern CALLSIGN> --out <recording> [signal options]
  qoda-tx <input.ts | --pattern CALLSIGN> --pluto <uri> --freq <downlink MHz> [--offset kHz] [--atten dB] [--level dBFS] [--seconds S] [signal options]
signal options: [--rs kS/s] [--modcod \"QPSK 1/2\"] [--short] [--no-pilots] [--rolloff R] [--sps N]
--loopback with --pluto sends the samples at the real rate but only through the chip's digital loopback,
  with the transmitter off: checks that the link keeps up without radiating anything
--pattern makes a test broadcast (test card, 1 kHz tone, the call sign as service name) at the channel's exact bit rate";

fn parse_modcod(text: &str) -> Result<u8, String> {
    (1..=23)
        .find(|&m| {
            let p = Pls { modcod: m, short_frame: false, pilots: false };
            p.modulation_and_rate().is_some_and(|(md, (n, d))| format!("{} {n}/{d}", md.name()).eq_ignore_ascii_case(text.trim()))
        })
        .ok_or(format!("unknown MODCOD \"{text}\" (e.g. \"QPSK 1/2\", \"8PSK 2/3\")"))
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args {
        input: PathBuf::new(),
        pattern: None,
        output: None,
        pluto: None,
        symbol_rate: 333e3,
        cfg: TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 },
        sps: None,
        freq: None,
        offset: 0.0,
        attenuation: DEFAULT_ATTENUATION_DB,
        level: DEFAULT_LEVEL_DBFS,
        seconds: DEFAULT_SECONDS,
        loopback: false,
    };
    let num = |v: Option<String>, flag: &str| -> Result<f64, String> {
        v.ok_or(format!("missing value for {flag}"))?.parse().map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pattern" => args.pattern = Some(it.next().ok_or("missing value for --pattern")?),
            "--out" => args.output = Some(PathBuf::from(it.next().ok_or("missing value for --out")?)),
            "--pluto" => args.pluto = Some(it.next().ok_or("missing value for --pluto")?),
            "--rs" => args.symbol_rate = num(it.next(), &a)? * 1e3,
            "--modcod" => args.cfg.modcod = parse_modcod(&it.next().ok_or("missing value for --modcod")?)?,
            "--short" => args.cfg.short_frame = true,
            "--no-pilots" => args.cfg.pilots = false,
            "--rolloff" => args.cfg.rolloff = num(it.next(), &a)? as f32,
            "--sps" => args.sps = Some(num(it.next(), &a)? as usize),
            "--freq" => args.freq = Some(num(it.next(), &a)? * 1e6),
            "--offset" => args.offset = num(it.next(), &a)? * 1e3,
            "--atten" => args.attenuation = num(it.next(), &a)?,
            "--level" => args.level = num(it.next(), &a)?,
            "--seconds" => args.seconds = num(it.next(), &a)?,
            "--loopback" => args.loopback = true,
            _ if a.starts_with("--") => return Err(format!("unknown option: {a}\n{USAGE}")),
            _ if args.input.as_os_str().is_empty() => args.input = PathBuf::from(a),
            _ => return Err(USAGE.into()),
        }
    }
    if args.input.as_os_str().is_empty() == args.pattern.is_none() || (args.output.is_none() == args.pluto.is_none()) {
        return Err(format!("give an input file or --pattern, and either --out or --pluto\n{USAGE}"));
    }
    Ok(args)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let cfg = args.cfg;
    let capacity = cfg.ts_bitrate(args.symbol_rate).ok_or("invalid MODCOD and frame size")?;
    let input = match &args.pattern {
        Some(call) => make_pattern(call, capacity)?,
        None => args.input.clone(),
    };
    let ts = std::fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let start = ts.iter().position(|&b| b == 0x47).ok_or("no MPEG-TS sync byte in the input")?;
    let packets: Vec<u8> = ts[start..].chunks_exact(TS_LEN).filter(|p| p[0] == 0x47).flatten().copied().collect();

    // Integer samples per symbol, at least the Pluto's lowest rate.
    let sps = args.sps.unwrap_or_else(|| (MIN_SAMPLE_RATE / args.symbol_rate).ceil().max(2.0) as usize);
    let sample_rate = sps as f64 * args.symbol_rate;
    println!(
        "{} TS packets in, {}: TS capacity {:.1} kbit/s at {:.0} kS/s, {} samples/symbol = {:.3} MS/s",
        packets.len() / TS_LEN,
        cfg.pls().modulation_and_rate().map_or(String::new(), |(m, (n, d))| format!("{} {n}/{d}", m.name())),
        capacity / 1e3,
        args.symbol_rate / 1e3,
        sps,
        sample_rate / 1e6
    );
    let samples = modulate(&packets, cfg, sps)?;
    println!("Modulated {:.2} s of signal ({} samples).", samples.len() as f64 / sample_rate, samples.len());

    match (&args.output, &args.pluto) {
        (Some(out), _) => write_recording(out, &samples, sample_rate, args.freq.unwrap_or(10_491.5e6), args.symbol_rate),
        (None, Some(uri)) => transmit(uri, &samples, sample_rate, &args),
        (None, None) => unreachable!(),
    }
}

/// Length of the test broadcast; it repeats while transmitting.
const PATTERN_SECONDS: u32 = 20;
/// Channels below this TS rate (bit/s) get a test broadcast without audio.
const LOW_RATE: f64 = 100e3;

/// Makes a test broadcast with FFmpeg: test card, 1 kHz tone, `call` as the service name. The
/// TS is muxed at the channel's bit rate, so it plays at the right speed when sent in a loop.
fn make_pattern(call: &str, capacity: f64) -> Result<PathBuf, String> {
    let ffmpeg = qoda::ffmpeg::find_tool("ffmpeg").ok_or("FFmpeg not found; it is needed for --pattern")?;
    let mux = (capacity / 1000.0).floor() as u32;
    let path = std::env::temp_dir().join(format!("qoda-pattern-{mux}k.ts"));
    // FFmpeg exceeds the mux rate when the streams do not fit; the audio packets and tables
    // cost much more than their nominal rate at low rates. Measure and lower the video rate.
    let audio = if capacity < LOW_RATE { 0.0 } else { 40e3 };
    let mut video = ((capacity * 0.80 - audio) / 1000.0).max(20.0);
    for _ in 0..5 {
        run_ffmpeg_pattern(&ffmpeg, call, capacity, video as u32, mux, &path)?;
        let bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        let actual = bytes as f64 * 8.0 / PATTERN_SECONDS as f64 / 1000.0;
        if actual <= mux as f64 * 1.005 {
            let sound = if capacity < LOW_RATE { "no audio" } else { "MP2 32 kbit/s" };
            println!("Test broadcast \"{call}\": H.264 {video:.0} kbit/s + {sound}, muxed at {mux} kbit/s");
            return Ok(path);
        }
        if video <= 20.0 {
            break;
        }
        video = (video - (actual - mux as f64) * 1.2).max(20.0);
    }
    Err(format!("the test broadcast does not fit in {mux} kbit/s; use a higher symbol rate or code rate"))
}

fn run_ffmpeg_pattern(ffmpeg: &std::path::Path, call: &str, capacity: f64, video: u32, mux: u32, path: &std::path::Path) -> Result<(), String> {
    let (size, fps) = if capacity < 200e3 { ("320x180", 15) } else { ("640x360", 25) };
    let status = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate={fps}")])
        .args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000"])
        .args(["-t", &PATTERN_SECONDS.to_string()])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-pix_fmt", "yuv420p"])
        .args(["-b:v", &format!("{video}k"), "-maxrate", &format!("{video}k"), "-bufsize", &format!("{}k", video * 2)])
        .args(["-g", &(fps * 2).to_string()])
        // Below ~100 kbit/s the tone's 32 kbit/s would leave too little for the picture.
        .args(if capacity < LOW_RATE { vec!["-an"] } else { vec!["-c:a", "mp2", "-b:a", "32k"] })
        .args(["-f", "mpegts", "-muxrate", &format!("{mux}k")])
        // FFmpeg's default PCR every 20 ms takes a packet of its own at low rates (75 kbit/s);
        // DVB allows up to 100 ms. Tables every 0.5 s (PAT/PMT) and 1 s (SDT) instead of 0.1 s.
        .args(["-pcr_period", "80", "-pat_period", "0.5", "-sdt_period", "1"])
        .args(["-metadata", &format!("service_name={call}"), "-metadata", "service_provider=QODA"])
        .arg(path)
        .status()
        .map_err(|e| format!("could not start FFmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("FFmpeg failed ({status})"));
    }
    Ok(())
}

/// All packets → RRC-shaped samples; the last frame is filled with null packets.
fn modulate(packets: &[u8], cfg: TxConfig, sps: usize) -> Result<Vec<Complex<f32>>, String> {
    let mut modulator = Modulator::new(cfg)?;
    modulator.push_ts(packets);
    let dfl = cfg.data_field_bytes().unwrap();
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

fn write_recording(path: &std::path::Path, samples: &[Complex<f32>], sample_rate: f64, freq: f64, symbol_rate: f64) -> Result<(), String> {
    let data_path = path.with_extension("sigmf-data");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&data_path).map_err(|e| format!("{}: {e}", data_path.display()))?);
    for z in samples {
        out.write_all(&z.re.to_le_bytes()).and_then(|_| out.write_all(&z.im.to_le_bytes())).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;
    let meta = format!(
        r#"{{
  "global": {{
    "core:datatype": "cf32_le",
    "core:sample_rate": {sample_rate},
    "core:version": "1.0.0",
    "core:recorder": "qoda-tx {}",
    "core:description": "DVB-S2 modulated by qoda-tx, {:.0} kS/s"
  }},
  "captures": [
    {{ "core:sample_start": 0, "core:frequency": {freq} }}
  ],
  "annotations": []
}}
"#,
        env!("CARGO_PKG_VERSION"),
        symbol_rate / 1e3
    );
    std::fs::write(path.with_extension("sigmf-meta"), meta).map_err(|e| e.to_string())?;
    println!("Wrote {}", data_path.display());
    Ok(())
}

/// Sends the samples in a loop until `args.seconds` pass or Ctrl+C is pressed.
fn transmit(uri: &str, samples: &[Complex<f32>], sample_rate: f64, args: &Args) -> Result<(), String> {
    let downlink = args.freq.ok_or("--freq (downlink MHz of the signal's centre) is needed to transmit")?;
    let uplink = downlink - UPLINK_OFFSET_HZ;
    let half = args.symbol_rate * (1.0 + args.cfg.rolloff as f64) / 2.0;
    if uplink - half < UPLINK_BAND.0 || uplink + half > UPLINK_BAND.1 {
        return Err(format!(
            "the signal ({:.4}–{:.4} MHz uplink) must lie inside the wideband transponder's uplink, {:.1}–{:.1} MHz \
             (downlink {:.2}–{:.2} MHz)",
            (uplink - half) / 1e6,
            (uplink + half) / 1e6,
            UPLINK_BAND.0 / 1e6,
            UPLINK_BAND.1 / 1e6,
            (UPLINK_BAND.0 + UPLINK_OFFSET_HZ) / 1e6,
            (UPLINK_BAND.1 + UPLINK_OFFSET_HZ) / 1e6
        ));
    }
    if !(0.0..=MAX_SECONDS).contains(&args.seconds) {
        return Err(format!("--seconds must be between 0 and {MAX_SECONDS}"));
    }
    if !(LEVEL_RANGE_DBFS.0..=LEVEL_RANGE_DBFS.1).contains(&args.level) {
        return Err(format!("--level must be between {} and {} dBFS", LEVEL_RANGE_DBFS.0, LEVEL_RANGE_DBFS.1));
    }
    let tx_hz = uplink + args.offset;

    // Scale to the DAC: a fixed RMS level, so the PA drive is set by --level and --atten.
    let rms = (samples.iter().map(|z| z.norm_sqr() as f64).sum::<f64>() / samples.len() as f64).sqrt() as f32;
    let scale = 10f32.powf(args.level as f32 / 20.0) / rms;
    let scaled: Vec<Complex<f32>> = samples.iter().map(|z| z * scale).collect();

    let mut radio = IioTx::open(uri)?;
    radio.set_sample_rate(sample_rate)?;
    radio.set_frequency(tx_hz)?;
    install_stop_handler();
    println!();
    if args.loopback {
        // The LO stays powered down and the attenuation at maximum, as `open` left them.
        radio.set_loopback(1)?;
        println!("LOOPBACK TEST, nothing is transmitted  {uri}");
        println!("    {:.3} MS/s through the digital loopback for up to {:.0} s; Ctrl+C stops", sample_rate / 1e6, args.seconds);
    } else {
        radio.set_loopback(0)?;
        println!("*** TRANSMITTING ***  {uri}");
        println!(
            "    downlink {:.4} MHz, uplink {:.4} MHz (TX LO {:.4} MHz with {:+.1} kHz correction)",
            downlink / 1e6,
            uplink / 1e6,
            tx_hz / 1e6,
            args.offset / 1e3
        );
        println!("    level {:.1} dBFS, attenuation {:.2} dB, up to {:.0} s; Ctrl+C stops", args.level, args.attenuation, args.seconds);
        radio.set_attenuation(args.attenuation)?;
        radio.power_on()?;
    }

    let chunk = radio.buffer_samples();
    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut position = 0;
    let mut sent = 0usize;
    let mut starved = false;
    let mut block = Vec::with_capacity(chunk);
    while !STOP.load(Ordering::Relaxed) && started.elapsed().as_secs_f64() < args.seconds {
        // Loop over the signal, wrapping around at its end.
        block.clear();
        while block.len() < chunk {
            let take = (chunk - block.len()).min(scaled.len() - position);
            block.extend_from_slice(&scaled[position..position + take]);
            position = (position + take) % scaled.len();
        }
        radio.write(&block)?;
        sent += chunk;
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            // The device takes samples at the sample rate; if they arrive slower, its buffer
            // runs dry and only the carrier leaking from the LO goes out.
            let elapsed = started.elapsed().as_secs_f64();
            let achieved = sent as f64 / elapsed;
            starved |= elapsed > 3.0 && achieved < sample_rate * 0.98;
            let state = if args.loopback { "running" } else { "on air" };
            print!("\r    {state} {elapsed:.0} s, sending {:.3} of {:.3} MS/s ", achieved / 1e6, sample_rate / 1e6);
            let _ = std::io::stdout().flush();
        }
    }
    if args.loopback {
        radio.set_loopback(0)?;
    }
    drop(radio);
    let elapsed = started.elapsed().as_secs_f64();
    println!("\rTransmitter off after {elapsed:.1} s; sent {:.3} MS/s on average.            ", sent as f64 / elapsed / 1e6);
    if starved {
        println!(
            "WARNING: the samples did not reach the Pluto as fast as it sends them ({:.3} MS/s, about {:.0} Mbit/s);\n\
             its buffer ran dry, so mostly an unmodulated carrier went out. A faster link (gigabit Ethernet or USB)\n\
             or a lower symbol rate helps.",
            sample_rate / 1e6,
            sample_rate * 32.0 / 1e6
        );
    }
    Ok(())
}

/// Ctrl+C sets `STOP` instead of killing the process, so the transmitter is switched off.
fn install_stop_handler() {
    #[cfg(windows)]
    {
        unsafe extern "system" fn handler(_event: u32) -> i32 {
            STOP.store(true, Ordering::Relaxed);
            1
        }
        unsafe extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    }
    #[cfg(unix)]
    {
        extern "C" fn handler(_signal: i32) {
            STOP.store(true, Ordering::Relaxed);
        }
        unsafe extern "C" {
            fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
        }
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        unsafe {
            signal(SIGINT, handler);
            signal(SIGTERM, handler);
        }
    }
}
