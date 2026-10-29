//! Bir SigMF kaydında DVB-S2 çerçeve başlıklarını arar ve sinyalleşmeyi raporlar.
//!
//! Kullanım: qoda-analyze <kayıt> [--seconds S] [--rs kS/s] [--rolloff R]

use std::path::PathBuf;
use std::process::ExitCode;

use num_complex::Complex;
use qoda::detect;
use qoda::dsp::Spectrum;
use qoda::dvbs2::{filters, plheader, timing};

const SOF_THRESHOLD: f32 = 0.7;
/// Bu PLS güveninin altındaki başlıklar yanlış alarm sayılır.
const MIN_PLS_MARGIN: f32 = 0.2;
const TIMING_BANDWIDTH: f64 = 0.005;

struct Args {
    path: PathBuf,
    seconds: Option<f64>,
    symbol_rate: Option<f64>,
    rolloff: Option<f64>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args { path: PathBuf::new(), seconds: None, symbol_rate: None, rolloff: None };
    let num = |v: Option<String>, flag: &str| -> Result<f64, String> {
        v.ok_or(format!("{flag} için değer eksik"))?.parse().map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seconds" => args.seconds = Some(num(it.next(), &a)?),
            "--rs" => args.symbol_rate = Some(num(it.next(), &a)? * 1e3),
            "--rolloff" => args.rolloff = Some(num(it.next(), &a)?),
            _ if a.starts_with("--") => return Err(format!("bilinmeyen seçenek: {a}")),
            _ => args.path = PathBuf::from(a),
        }
    }
    if args.path.as_os_str().is_empty() {
        return Err("kullanım: qoda-analyze <kayıt> [--seconds S] [--rs kS/s] [--rolloff R]".into());
    }
    Ok(args)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hata: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let max = args.seconds.map(|s| s as usize); // aşağıda örnek sayısına çevrilir
    let rec = qoda::sigmf::read(&args.path, None).map_err(|e| format!("{}: {e}", args.path.display()))?;
    let fs = rec.sample_rate;
    let mut x = rec.samples;
    if let Some(s) = max.map(|_| args.seconds.unwrap()) {
        x.truncate((s * fs) as usize);
    }
    println!(
        "Kayıt: {} örnek, {:.3} MS/s, {:.2} s, RF {:.3} MHz",
        x.len(),
        fs / 1e6,
        x.len() as f64 / fs,
        rec.frequency / 1e6
    );

    // 1. Spektrumdan yayının merkezini ve sembol hızını bul.
    let mut spectrum = Spectrum::new(4096);
    let db = spectrum.compute(&x[..x.len().min(fs as usize)]);
    let found = detect::detect(&db, fs, db.len() / 2).ok_or("kaydın ortasında yayın bulunamadı")?;
    let rs = args.symbol_rate.unwrap_or(found.symbol_rate());
    let rolloff = args.rolloff.unwrap_or(nearest_rolloff(found.rolloff));
    println!(
        "Spektrum: merkez ofseti {:+.1} kHz, ölçülen {:.1} kS/s → {:.1} kS/s, roll-off {rolloff:.2}, SNR {:.1} dB",
        found.center_offset_hz / 1e3,
        found.measured_symbol_rate / 1e3,
        rs / 1e3,
        found.snr_db
    );

    // 2. Merkeze indir, RRC eşleştirilmiş filtre, zamanlama kurtarma.
    filters::mix_down(&mut x, found.center_offset_hz, fs);
    let sps = fs / rs;
    let mut y = filters::filter(&x, &filters::rrc_taps(sps, rolloff, 8));
    filters::normalize(&mut y);
    let t = timing::recover(&y, sps, TIMING_BANDWIDTH);
    let clock_ppm = (t.final_period / sps - 1.0) * 1e6;
    println!(
        "Zamanlama: {} sembol, {:.3} örnek/sembol, saat farkı {clock_ppm:+.0} ppm",
        t.symbols.len(),
        sps
    );

    // 3. Çerçeve başlıklarını ara. Bulunamazsa spektrum ters olabilir; eşleniği dene.
    let mut symbols = t.symbols;
    let mut hits = plheader::find_headers(&symbols, SOF_THRESHOLD);
    if hits.is_empty() {
        println!("Başlık bulunamadı, spektrum ters çevrilerek tekrar deneniyor...");
        symbols.iter_mut().for_each(|z| *z = z.conj());
        hits = plheader::find_headers(&symbols, SOF_THRESHOLD);
        if !hits.is_empty() {
            println!("Spektrum ters (IQ yer değiştirmiş).");
        }
    }
    if hits.is_empty() {
        return Err("DVB-S2 çerçeve başlığı bulunamadı".into());
    }

    // İkinci geçiş: güvenilir başlıkların ortanca frekans kaymasını tüm sembollerden çıkar.
    let mut ws: Vec<f32> = hits.iter().filter(|h| h.pls_margin > 0.5).map(|h| h.freq_rad_per_symbol).collect();
    ws.sort_by(f32::total_cmp);
    let residual_w = ws.get(ws.len() / 2).copied().unwrap_or(0.0);
    for (n, z) in symbols.iter_mut().enumerate() {
        let phase = (residual_w as f64 * n as f64).rem_euclid(2.0 * std::f64::consts::PI);
        *z *= Complex::from_polar(1.0, -phase as f32);
    }
    let residual_hz = residual_w as f64 * rs / (2.0 * std::f64::consts::PI);
    println!("Kalan frekans kayması {:+.2} kHz düzeltildi, başlıklar yeniden aranıyor.", residual_hz / 1e3);
    let all = plheader::find_headers(&symbols, SOF_THRESHOLD);
    let rejected = all.iter().filter(|h| h.pls_margin < MIN_PLS_MARGIN).count();
    hits = all.into_iter().filter(|h| h.pls_margin >= MIN_PLS_MARGIN).collect();
    if rejected > 0 {
        println!("PLS güveni düşük {rejected} aday yanlış alarm sayılıp atıldı.");
    }

    println!();
    println!("{:>9} {:>6} {:>6} {:>7} {:>9}  {:>9} {:>9}  sinyalleşme", "konum", "SOF", "PLS", "SNR dB", "Δf kHz", "aralık", "beklenen");
    let mut consistent = 0;
    for (i, h) in hits.iter().enumerate() {
        let gap = hits.get(i + 1).map(|n| n.position - h.position);
        let expected = h.pls.frame_len();
        if gap.is_some() && gap == expected {
            consistent += 1;
        }
        let df_khz = h.freq_rad_per_symbol as f64 * rs / (2.0 * std::f64::consts::PI) / 1e3;
        println!(
            "{:>9} {:>6.2} {:>6.2} {:>7.1} {:>+9.2}  {:>9} {:>9}  {}",
            h.position,
            h.sof_metric,
            h.pls_margin,
            h.snr_db,
            df_khz,
            gap.map_or("-".into(), |g| g.to_string()),
            expected.map_or("?".into(), |e| e.to_string()),
            h.pls.describe()
        );
    }

    // Özet: en sık görülen sinyalleşme ve çerçeve aralıklarının tutarlılığı.
    let mut counts: Vec<(plheader::Pls, usize)> = Vec::new();
    for h in &hits {
        match counts.iter_mut().find(|(p, _)| *p == h.pls) {
            Some((_, c)) => *c += 1,
            None => counts.push((h.pls, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let mean_df = residual_hz
        + hits.iter().map(|h| h.freq_rad_per_symbol as f64).sum::<f64>() / hits.len() as f64 * rs
            / (2.0 * std::f64::consts::PI);
    let mean_snr = hits.iter().map(|h| h.snr_db as f64).sum::<f64>() / hits.len() as f64;
    println!();
    println!("Özet: {} başlık bulundu, ardışık {} aralık beklenen çerçeve uzunluğuyla tutarlı.", hits.len(), consistent);
    for (pls, c) in &counts {
        println!("  {c:>4} × {}", pls.describe());
    }
    println!(
        "  Spektrum merkezine göre frekans kayması {:+.2} kHz, ortalama başlık SNR'ı {mean_snr:.1} dB",
        mean_df / 1e3
    );
    Ok(())
}

fn nearest_rolloff(r: f32) -> f64 {
    [0.35, 0.25, 0.20]
        .into_iter()
        .min_by(|a: &f64, b: &f64| (a - r as f64).abs().total_cmp(&(b - r as f64).abs()))
        .unwrap()
}
