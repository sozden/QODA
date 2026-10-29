//! QODA - QO-100 DATV alıcı.
//!
//! Şimdilik: SDR cihazını açar, LNB LO ve yayın frekansından IF'yi hesaplar,
//! bir süre örnek okur ve kabaca bir spektrum yazdırır.

mod tuning;

use std::error::Error;

use num_complex::Complex;
use rustfft::FftPlanner;
use soapysdr::Direction::Rx;

const FFT_SIZE: usize = 1024;

struct Args {
    device: String,
    lo_hz: f64,
    rf_hz: f64,
    sample_rate: f64,
    seconds: f64,
    /// dB cinsinden sabit kazanç; None ise otomatik kazanç.
    gain: Option<f64>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        device: "driver=rtlsdr".into(),
        lo_hz: 9_750.0e6,
        rf_hz: 10_491.0e6,
        sample_rate: 2.048e6,
        seconds: 1.0,
        gain: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} için değer eksik"));
        let mhz = |v: String| v.parse::<f64>().map(|x| x * 1e6).map_err(|e| e.to_string());
        match flag.as_str() {
            "--device" => args.device = value()?,
            "--lo" => args.lo_hz = mhz(value()?)?,
            "--rf" => args.rf_hz = mhz(value()?)?,
            "--rate" => args.sample_rate = mhz(value()?)?,
            "--seconds" => args.seconds = value()?.parse().map_err(|e: std::num::ParseFloatError| e.to_string())?,
            "--gain" => args.gain = Some(value()?.parse().map_err(|e: std::num::ParseFloatError| e.to_string())?),
            _ => return Err(format!("bilinmeyen seçenek: {flag}")),
        }
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let if_hz = tuning::intermediate_frequency(args.rf_hz, args.lo_hz);

    println!("Cihazlar:");
    for dev in soapysdr::enumerate("")? {
        println!("  {dev}");
    }

    let dev = soapysdr::Device::new(args.device.as_str())?;
    dev.set_sample_rate(Rx, 0, args.sample_rate)?;
    dev.set_frequency(Rx, 0, if_hz, ())?;
    match args.gain {
        Some(db) => {
            dev.set_gain_mode(Rx, 0, false)?;
            dev.set_gain(Rx, 0, db)?;
        }
        None => dev.set_gain_mode(Rx, 0, true)?,
    }
    println!(
        "RF {:.3} MHz, LO {:.3} MHz -> IF {:.3} MHz, {:.3} MS/s",
        args.rf_hz / 1e6,
        args.lo_hz / 1e6,
        if_hz / 1e6,
        args.sample_rate / 1e6
    );

    let mut stream = dev.rx_stream::<Complex<f32>>(&[0])?;
    stream.activate(None)?;

    let total = (args.sample_rate * args.seconds) as usize;
    let mut samples = vec![Complex::new(0.0f32, 0.0); total];
    let mut filled = 0;
    while filled < total {
        filled += stream.read(&mut [&mut samples[filled..]], 1_000_000)?;
    }
    stream.deactivate(None)?;

    print_spectrum(&samples, args.sample_rate);
    Ok(())
}

/// Ortalama güç spektrumunu 32 kutuya bölerek yazdırır.
fn print_spectrum(samples: &[Complex<f32>], sample_rate: f64) {
    let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
    let mut psd = vec![0.0f32; FFT_SIZE];
    let mut blocks = 0;
    for chunk in samples.chunks_exact(FFT_SIZE) {
        let mut buf = chunk.to_vec();
        fft.process(&mut buf);
        for (p, x) in psd.iter_mut().zip(&buf) {
            *p += x.norm_sqr();
        }
        blocks += 1;
    }
    psd.rotate_left(FFT_SIZE / 2);
    let db: Vec<f32> = psd.iter().map(|p| 10.0 * (p / blocks as f32 + 1e-12).log10()).collect();
    let floor = db.iter().cloned().fold(f32::INFINITY, f32::min);

    let step = FFT_SIZE / 32;
    println!("ofset_kHz  seviye_dB");
    for (i, seg) in db.chunks(step).enumerate() {
        let center = (i * step + step / 2) as f64 - (FFT_SIZE / 2) as f64;
        let khz = center * sample_rate / FFT_SIZE as f64 / 1e3;
        let lvl = seg.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        println!("{khz:9.0}  {lvl:7.1}  {}", "#".repeat(((lvl - floor) / 2.0) as usize));
    }
}
