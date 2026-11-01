//! Modulates an MPEG-TS file into a DVB-S2 SigMF recording (no transmitter involved).
//!
//! Usage: qoda-tx <input.ts> <output> [--rs kS/s] [--modcod "QPSK 1/2"] [--short] [--no-pilots]
//!        [--rolloff R] [--sps N] [--freq MHz]
//!
//! The recording can be checked with `qoda-analyze <output> --stream`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use qoda::dvbs2::plheader::Pls;
use qoda::dvbs2::tx::{Modulator, NULL_PACKET, Shaper, TS_LEN, TxConfig};

/// Lowest sample rate of the AD9363 in the ADALM-Pluto without its extra FIR filter (S/s).
const MIN_SAMPLE_RATE: f64 = 2.1e6;

struct Args {
    input: PathBuf,
    output: PathBuf,
    symbol_rate: f64,
    cfg: TxConfig,
    sps: Option<usize>,
    /// Downlink frequency written to the metadata (Hz).
    freq: f64,
}

fn parse_modcod(text: &str) -> Result<u8, String> {
    (1..=23)
        .find(|&m| {
            let p = Pls { modcod: m, short_frame: false, pilots: false };
            p.modulation_and_rate().is_some_and(|(md, (n, d))| format!("{} {n}/{d}", md.name()).eq_ignore_ascii_case(text.trim()))
        })
        .ok_or(format!("unknown MODCOD \"{text}\" (e.g. \"QPSK 1/2\", \"8PSK 2/3\")"))
}

fn parse_args() -> Result<Args, String> {
    let usage = "usage: qoda-tx <input.ts> <output> [--rs kS/s] [--modcod \"QPSK 1/2\"] [--short] [--no-pilots] [--rolloff R] [--sps N] [--freq MHz]";
    let mut it = std::env::args().skip(1);
    let mut files = Vec::new();
    let mut args = Args {
        input: PathBuf::new(),
        output: PathBuf::new(),
        symbol_rate: 333e3,
        cfg: TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 },
        sps: None,
        freq: 10_491.5e6,
    };
    let num = |v: Option<String>, flag: &str| -> Result<f64, String> {
        v.ok_or(format!("missing value for {flag}"))?.parse().map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rs" => args.symbol_rate = num(it.next(), &a)? * 1e3,
            "--modcod" => args.cfg.modcod = parse_modcod(&it.next().ok_or("missing value for --modcod")?)?,
            "--short" => args.cfg.short_frame = true,
            "--no-pilots" => args.cfg.pilots = false,
            "--rolloff" => args.cfg.rolloff = num(it.next(), &a)? as f32,
            "--sps" => args.sps = Some(num(it.next(), &a)? as usize),
            "--freq" => args.freq = num(it.next(), &a)? * 1e6,
            _ if a.starts_with("--") => return Err(format!("unknown option: {a}\n{usage}")),
            _ => files.push(PathBuf::from(a)),
        }
    }
    let [input, output] = <[PathBuf; 2]>::try_from(files).map_err(|_| usage.to_string())?;
    (args.input, args.output) = (input, output);
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
    let ts = std::fs::read(&args.input).map_err(|e| format!("{}: {e}", args.input.display()))?;
    let start = ts.iter().position(|&b| b == 0x47).ok_or("no MPEG-TS sync byte in the input")?;
    let packets: Vec<u8> = ts[start..].chunks_exact(TS_LEN).filter(|p| p[0] == 0x47).flatten().copied().collect();

    let cfg = args.cfg;
    let mut modulator = Modulator::new(cfg)?;
    // Integer samples per symbol, at least the Pluto's lowest rate.
    let sps = args.sps.unwrap_or_else(|| (MIN_SAMPLE_RATE / args.symbol_rate).ceil().max(2.0) as usize);
    let sample_rate = sps as f64 * args.symbol_rate;
    let capacity = cfg.ts_bitrate(args.symbol_rate).unwrap();
    println!(
        "{} TS packets in, {}: TS capacity {:.1} kbit/s at {:.0} kS/s, {} samples/symbol = {:.3} MS/s",
        packets.len() / TS_LEN,
        cfg.pls().modulation_and_rate().map_or(String::new(), |(m, (n, d))| format!("{} {n}/{d}", m.name())),
        capacity / 1e3,
        args.symbol_rate / 1e3,
        sps,
        sample_rate / 1e6
    );

    modulator.push_ts(&packets);
    // Fill the last frame with null packets.
    let dfl = cfg.data_field_bytes().unwrap();
    let short = (dfl - modulator.queued() % dfl) % dfl;
    for _ in 0..short.div_ceil(TS_LEN) {
        modulator.push_ts(&NULL_PACKET);
    }
    let mut shaper = Shaper::new(sps, cfg.rolloff);
    let data_path = args.output.with_extension("sigmf-data");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&data_path).map_err(|e| format!("{}: {e}", data_path.display()))?);
    let (mut frames, mut samples, mut peak) = (0usize, 0usize, 0f32);
    let mut buf = Vec::new();
    while let Some(frame) = modulator.next_frame() {
        buf.clear();
        shaper.process(&frame, &mut buf);
        for z in &buf {
            peak = peak.max(z.re.abs()).max(z.im.abs());
            out.write_all(&z.re.to_le_bytes()).and_then(|_| out.write_all(&z.im.to_le_bytes())).map_err(|e| e.to_string())?;
        }
        frames += 1;
        samples += buf.len();
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
    {{ "core:sample_start": 0, "core:frequency": {} }}
  ],
  "annotations": []
}}
"#,
        env!("CARGO_PKG_VERSION"),
        args.symbol_rate / 1e3,
        args.freq
    );
    std::fs::write(args.output.with_extension("sigmf-meta"), meta).map_err(|e| e.to_string())?;
    println!(
        "Wrote {frames} frames, {:.2} s of signal ({samples} samples, peak {peak:.2}) to {}",
        samples as f64 / sample_rate,
        data_path.display()
    );
    Ok(())
}
