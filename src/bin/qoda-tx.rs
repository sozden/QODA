//! Modulates an MPEG-TS file into DVB-S2: writes a SigMF recording, or transmits it on the
//! QO-100 wideband uplink through an ADALM-Pluto. `qoda-tx-gui` does the same in a window.
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

use num_complex::Complex;
use qoda::dvbs2::tx::{TS_LEN, TxConfig};
use qoda::transmit::{self, AirSettings, Plan, Report};

/// Set by Ctrl+C; the transmit loop stops and the transmitter is switched off.
static STOP: AtomicBool = AtomicBool::new(false);

struct Args {
    input: PathBuf,
    /// Make a test broadcast with this call sign as the service name instead of reading `input`.
    pattern: Option<String>,
    output: Option<PathBuf>,
    pluto: Option<String>,
    plan: Plan,
    /// `--freq` was given.
    freq_given: bool,
    air: AirSettings,
    /// With `--out`: write this many passes of the endless stream as sent on air.
    loops: u32,
    /// Transmit a TS file even if its mux rate does not match the channel.
    ignore_rate: bool,
}

const USAGE: &str = "usage:
  qoda-tx <input.ts | --pattern CALLSIGN> --out <recording> [signal options]
  qoda-tx <input.ts | --pattern CALLSIGN> --pluto <uri> --freq <downlink MHz> [--offset kHz] [--atten dB] [--level dBFS] [--seconds S] [signal options]
signal options: [--rs kS/s] [--modcod \"QPSK 1/2\"] [--short] [--no-pilots] [--rolloff R] [--sps N]
--shift kHz: distance of the signal above the transmitter's LO, whose leakage would otherwise sit in the
  middle of the signal (default 1.5 x half the bandwidth + 50 kHz; 0 turns it off)
--loopback with --pluto sends the samples at the real rate but only through the chip's digital loopback,
  with the transmitter off: checks that the link keeps up without radiating anything
--loops N with --out writes N passes of the endless stream exactly as sent on air (shifted from the centre)
a TS file must be muxed at the channel's TS capacity (within 3%); --ignore-rate transmits it anyway
--pattern makes a test broadcast (test card, 1 kHz tone, the call sign as service name) at the channel's exact bit rate";

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args {
        input: PathBuf::new(),
        pattern: None,
        output: None,
        pluto: None,
        plan: Plan {
            symbol_rate: 333e3,
            cfg: TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 },
            downlink_hz: 10_491.5e6,
            offset_hz: 0.0,
            shift_hz: None,
            sps: None,
        },
        freq_given: false,
        air: AirSettings {
            level_dbfs: transmit::DEFAULT_LEVEL_DBFS,
            attenuation_db: transmit::DEFAULT_ATTENUATION_DB,
            seconds: transmit::DEFAULT_SECONDS,
            loopback: false,
        },
        loops: 0,
        ignore_rate: false,
    };
    let num = |v: Option<String>, flag: &str| -> Result<f64, String> {
        v.ok_or(format!("missing value for {flag}"))?.parse().map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pattern" => args.pattern = Some(it.next().ok_or("missing value for --pattern")?),
            "--out" => args.output = Some(PathBuf::from(it.next().ok_or("missing value for --out")?)),
            "--pluto" => args.pluto = Some(it.next().ok_or("missing value for --pluto")?),
            "--rs" => args.plan.symbol_rate = num(it.next(), &a)? * 1e3,
            "--modcod" => args.plan.cfg.modcod = transmit::parse_modcod(&it.next().ok_or("missing value for --modcod")?)?,
            "--short" => args.plan.cfg.short_frame = true,
            "--no-pilots" => args.plan.cfg.pilots = false,
            "--rolloff" => args.plan.cfg.rolloff = num(it.next(), &a)? as f32,
            "--sps" => args.plan.sps = Some(num(it.next(), &a)? as usize),
            "--freq" => {
                args.plan.downlink_hz = num(it.next(), &a)? * 1e6;
                args.freq_given = true;
            }
            "--offset" => args.plan.offset_hz = num(it.next(), &a)? * 1e3,
            "--shift" => args.plan.shift_hz = Some(num(it.next(), &a)? * 1e3),
            "--atten" => args.air.attenuation_db = num(it.next(), &a)?,
            "--level" => args.air.level_dbfs = num(it.next(), &a)?,
            "--seconds" => args.air.seconds = num(it.next(), &a)?,
            "--loopback" => args.air.loopback = true,
            "--loops" => args.loops = num(it.next(), &a)? as u32,
            "--ignore-rate" => args.ignore_rate = true,
            _ if a.starts_with("--") => return Err(format!("unknown option: {a}\n{USAGE}")),
            _ if args.input.as_os_str().is_empty() => args.input = PathBuf::from(a),
            _ => return Err(USAGE.into()),
        }
    }
    if args.input.as_os_str().is_empty() == args.pattern.is_none() || (args.output.is_none() == args.pluto.is_none()) {
        return Err(format!("give an input file or --pattern, and either --out or --pluto\n{USAGE}"));
    }
    if args.pluto.is_some() && !args.freq_given {
        return Err("--freq (downlink MHz of the signal's centre) is needed to transmit".into());
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
    let plan = args.plan;
    let capacity = plan.capacity().ok_or("invalid MODCOD and frame size")?;
    let input = match &args.pattern {
        Some(call) => {
            let (path, description) = transmit::make_pattern(call, &transmit::pattern_caption(&plan), capacity)?;
            println!("{description}");
            path
        }
        None => args.input.clone(),
    };
    let packets = transmit::read_ts(&input)?;
    let transmitting = args.pluto.is_some();
    // A file of one's own must be muxed at the channel's rate; the test broadcast is.
    if args.pattern.is_none() {
        match transmit::check_ts_rate(&packets, capacity) {
            Ok(rate) => println!("TS file muxed at {:.1} kbit/s", rate / 1e3),
            Err(e) if transmitting && !args.ignore_rate => return Err(format!("{e} (--ignore-rate sends it anyway)")),
            Err(e) => println!("WARNING: {e}"),
        }
    }
    let sps = plan.samples_per_symbol(transmitting);
    let sample_rate = plan.sample_rate(transmitting);
    println!(
        "{} TS packets in ({:.1} s at the channel rate), {}: TS capacity {:.1} kbit/s at {:.0} kS/s, {} samples/symbol = {:.3} MS/s",
        packets.len() / TS_LEN,
        (packets.len() * 8) as f64 / capacity,
        transmit::modcod_name(plan.cfg.modcod),
        capacity / 1e3,
        plan.symbol_rate / 1e3,
        sps,
        sample_rate / 1e6
    );

    match (&args.output, &args.pluto) {
        (Some(out), _) if args.loops > 0 => {
            // The endless stream as sent on air, `loops` passes of it; the signal sits `shift`
            // above the recording's centre frequency, at the downlink frequency.
            let air_rate = plan.sample_rate(true);
            let n = (args.loops as f64 * (packets.len() * 8) as f64 / capacity * air_rate) as usize;
            let mut source = transmit::AirSource::new(packets, &plan, args.air.level_dbfs)?;
            let mut samples = Vec::new();
            source.fill(&mut samples, n);
            write_recording(out, &samples, air_rate, plan.downlink_hz - plan.shift(), plan.symbol_rate)
        }
        (Some(out), _) => {
            let samples = transmit::modulate(&packets, plan.cfg, sps)?;
            write_recording(out, &samples, sample_rate, plan.downlink_hz, plan.symbol_rate)
        }
        (None, Some(uri)) => {
            plan.check_band()?;
            args.air.check()?;
            install_stop_handler();
            println!();
            // The file plays as an endless stream, modulated as it is sent.
            let summary = transmit::transmit(uri, packets, &plan, &args.air, &STOP, &mut |r| match r {
                Report::Info(line) => println!("    {line}"),
                Report::Warning(text) => println!("\nWARNING: {text}"),
                Report::Started if args.air.loopback => println!("    Ctrl+C stops"),
                Report::Started => println!("*** TRANSMITTING ***  {uri}; Ctrl+C stops"),
                Report::Progress { elapsed, achieved } => {
                    let state = if args.air.loopback { "running" } else { "on air" };
                    print!("\r    {state} {elapsed:.0} s, sending {:.3} of {:.3} MS/s ", achieved / 1e6, sample_rate / 1e6);
                    let _ = std::io::stdout().flush();
                }
            })?;
            println!("\rTransmitter off after {:.1} s; sent {:.3} MS/s on average.            ", summary.elapsed, summary.achieved / 1e6);
            Ok(())
        }
        (None, None) => unreachable!(),
    }
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
