//! Bir SigMF kaydında DVB-S2 çerçeve başlıklarını arar ve sinyalleşmeyi raporlar.
//!
//! Kullanım: qoda-analyze <kayıt> [--seconds S] [--rs kS/s] [--rolloff R]

use std::path::PathBuf;
use std::process::ExitCode;

use num_complex::Complex;
use qoda::detect;
use qoda::dsp::Spectrum;
use qoda::dvbs2::bbframe::{self, TsDepacketizer};
use qoda::dvbs2::fec::FecParams;
use qoda::dvbs2::{filters, plframe, plheader, timing};

const SOF_THRESHOLD: f32 = 0.7;
/// Bu PLS güveninin altındaki başlıklar yanlış alarm sayılır.
const MIN_PLS_MARGIN: f32 = 0.2;
const TIMING_BANDWIDTH: f64 = 0.005;
/// Çerçeve gövdesindeki taşıyıcı takip döngüsünün bant genişliği.
const CARRIER_BANDWIDTH: f32 = 0.002;
const LDPC_MAX_ITERATIONS: usize = 50;

struct Args {
    path: PathBuf,
    seconds: Option<f64>,
    symbol_rate: Option<f64>,
    rolloff: Option<f64>,
    /// Kaydı akış alıcısından parça parça geçir (canlı çözümü sınamak için).
    stream: bool,
    /// Sınama için eklenen gürültünün, kaydın ortalama gücüne oranı (dB).
    noise_db: Option<f64>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args { path: PathBuf::new(), seconds: None, symbol_rate: None, rolloff: None, stream: false, noise_db: None };
    let num = |v: Option<String>, flag: &str| -> Result<f64, String> {
        v.ok_or(format!("{flag} için değer eksik"))?.parse().map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seconds" => args.seconds = Some(num(it.next(), &a)?),
            "--rs" => args.symbol_rate = Some(num(it.next(), &a)? * 1e3),
            "--rolloff" => args.rolloff = Some(num(it.next(), &a)?),
            "--stream" => args.stream = true,
            "--noise" => args.noise_db = Some(num(it.next(), &a)?),
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
    if let Some(db) = args.noise_db {
        add_noise(&mut x, db);
        println!("Sınama: kayda ortalama gücün {db:+.1} dB'i kadar beyaz gürültü eklendi.");
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

    if args.stream {
        return run_stream(&x, fs, found.center_offset_hz, rs, rolloff, &args.path.with_extension("stream.ts"));
    }

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

    let llrs = demap_frames(&symbols, &hits);
    decode_frames(&llrs, &args.path.with_extension("ts"))
}

/// QPSK çerçevelerinin gövdelerini çözüp gövde boyunca SNR'ı raporlar.
fn demap_frames(symbols: &[Complex<f32>], hits: &[plheader::HeaderHit]) -> Vec<(plheader::Pls, Vec<f32>)> {
    let mut scrambling: Vec<u8> = Vec::new();
    let mut snrs = Vec::new();
    let mut llrs = Vec::new();
    // Başlık başına frekans tahmini gürültülü; ortak kayma için ortancayı kullan.
    let mut ws: Vec<f32> = hits.iter().map(|h| h.freq_rad_per_symbol).collect();
    ws.sort_by(f32::total_cmp);
    let common_w = ws[ws.len() / 2];
    for h in hits {
        let qpsk = matches!(h.pls.modulation_and_rate(), Some((plheader::Modulation::Qpsk, _)));
        let Some(len) = h.pls.frame_len() else { continue };
        let body_len = len - plheader::HEADER_LEN;
        let start = h.position + plheader::HEADER_LEN;
        if !qpsk || start + body_len > symbols.len() {
            continue;
        }
        if scrambling.len() < body_len {
            scrambling = plframe::scrambling_sequence(0, body_len);
        }
        let carrier = plframe::body_start(h.phase, common_w);
        let body = &symbols[start..start + body_len];
        // Gövdeyi dörde bölüp her parçadaki SNR'a bak: takip kaçarsa sonlara doğru düşer.
        let out = plframe::demap_qpsk(body, &scrambling[..body_len], h.pls.pilot_blocks(), carrier, CARRIER_BANDWIDTH);
        let quarter = body_len / 4;
        let parts: Vec<f32> = (0..4)
            .map(|i| {
                let seg = &out.llr[2 * i * quarter..2 * (i + 1) * quarter];
                let mean_abs = seg.iter().map(|l| l.abs()).sum::<f32>() / seg.len() as f32;
                let var = seg.iter().map(|l| (l.abs() - mean_abs).powi(2)).sum::<f32>() / seg.len() as f32;
                10.0 * (mean_abs * mean_abs / var.max(1e-12) / 2.0).log10()
            })
            .collect();
        snrs.push((h.position, out.snr_db, parts));
        llrs.push((h.pls, out.llr));
    }
    if snrs.is_empty() {
        println!("Gövdesi çözülebilecek QPSK çerçevesi yok.");
        return llrs;
    }
    println!();
    println!("Gövde çözümü (karıştırma çözümü + taşıyıcı takibi + QPSK demap):");
    println!("{:>9} {:>8}   çeyreklerde SNR dB", "konum", "SNR dB");
    for (pos, snr, parts) in snrs.iter().take(8) {
        println!("{pos:>9} {snr:>8.1}   {:.1} {:.1} {:.1} {:.1}", parts[0], parts[1], parts[2], parts[3]);
    }
    let mean = snrs.iter().map(|s| s.1).sum::<f32>() / snrs.len() as f32;
    let worst = snrs.iter().map(|s| s.1).fold(f32::INFINITY, f32::min);
    println!("  {} çerçeve, ortalama gövde SNR'ı {mean:.1} dB, en kötü {worst:.1} dB", snrs.len());
    llrs
}

/// LDPC ve BCH ile hata düzeltme, BBFRAME'den MPEG-TS çıkarma; TS'yi `ts_path`e yazar.
fn decode_frames(frames: &[(plheader::Pls, Vec<f32>)], ts_path: &std::path::Path) -> Result<(), String> {
    let Some((pls, _)) = frames.first() else { return Ok(()) };
    let (_, rate) = pls.modulation_and_rate().ok_or("bilinmeyen MODCOD")?;
    let params = FecParams::new(rate, pls.short_frame).ok_or("bu FEC oranı desteklenmiyor")?;
    let code = params.ldpc();
    let bch = params.bch();

    let mut depacketizer = TsDepacketizer::default();
    let mut ts = Vec::new();
    let (mut converged, mut bch_ok, mut header_ok, mut iterations) = (0, 0, 0, 0);
    let mut first_header = None;
    for (frame_pls, llr) in frames {
        if frame_pls != pls {
            continue;
        }
        let decoded = code.decode(llr, LDPC_MAX_ITERATIONS);
        iterations += decoded.iterations;
        if !decoded.converged {
            continue;
        }
        converged += 1;
        if bch.is_codeword(&decoded.bits[..params.k_ldpc]) {
            bch_ok += 1;
        }
        let mut bytes: Vec<u8> = decoded.bits[..params.k_bch]
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect();
        bbframe::bb_descramble(&mut bytes);
        if let Some(h) = depacketizer.push(&bytes, &mut ts) {
            header_ok += 1;
            first_header.get_or_insert(h);
        }
    }
    let n = frames.len();
    println!();
    println!("Hata düzeltme ({}):", pls.describe());
    println!("  LDPC: {converged}/{n} çerçeve yakınsadı, ortalama {:.1} iterasyon", iterations as f64 / n as f64);
    println!("  BCH: {bch_ok}/{converged} çerçeve geçerli");
    println!("  BBHEADER: {header_ok}/{converged} çerçevede CRC doğru");
    if let Some(h) = &first_header {
        println!(
            "  MATYPE 0x{:04X}, UPL {} bit, DFL {} bit, SYNC 0x{:02X}",
            h.matype, h.upl, h.dfl, h.sync
        );
    }
    println!(
        "  MPEG-TS: {} paket, {} paket CRC hatası",
        depacketizer.packets, depacketizer.crc_errors
    );
    if ts.is_empty() {
        return Ok(());
    }
    std::fs::write(ts_path, &ts).map_err(|e| format!("{}: {e}", ts_path.display()))?;
    println!("  TS dosyası: {}", ts_path.display());
    let mut info = qoda::tsinfo::TsInfo::default();
    info.push(&ts);
    print_service(info.info());

    // PID başına paket sayısı ve süreklilik sayacı (CC) kopuklukları.
    let mut pids: Vec<(u16, usize, usize)> = Vec::new();
    let mut last_cc: std::collections::HashMap<u16, u8> = Default::default();
    for p in ts.chunks_exact(188) {
        let pid = (((p[1] & 0x1F) as u16) << 8) | p[2] as u16;
        let has_payload = p[3] & 0x10 != 0;
        let cc = p[3] & 0x0F;
        let mut gap = 0;
        if pid != 0x1FFF && has_payload {
            if let Some(&prev) = last_cc.get(&pid) {
                // Aynı CC'li tekrar paketine izin verilir.
                if cc != (prev + 1) % 16 && cc != prev {
                    gap = 1;
                }
            }
            last_cc.insert(pid, cc);
        }
        match pids.iter_mut().find(|(q, _, _)| *q == pid) {
            Some((_, c, g)) => {
                *c += 1;
                *g += gap;
            }
            None => pids.push((pid, 1, gap)),
        }
    }
    pids.sort();
    let list: Vec<String> = pids
        .iter()
        .map(|(p, c, g)| if *g > 0 { format!("0x{p:04X}×{c} ({g} CC kopukluğu)") } else { format!("0x{p:04X}×{c}") })
        .collect();
    println!("  PID'ler: {}", list.join(", "));
    let gaps: usize = pids.iter().map(|(_, _, g)| g).sum();
    if gaps > 0 {
        println!(
            "  Not: BBFRAME'lerin tamamı BCH/CRC denetiminden geçtiği için CC kopuklukları
        yayının kendisindeki eksik paketleri gösterir."
        );
    }
    Ok(())
}

/// Kaydı canlı alıcıdaki gibi küçük parçalarla akış alıcısına verir.
fn run_stream(x: &[Complex<f32>], fs: f64, offset_hz: f64, rs: f64, rolloff: f64, ts_path: &std::path::Path) -> Result<(), String> {
    use qoda::dvbs2::receiver::{Receiver, ReceiverConfig};
    let mut rx = Receiver::new(ReceiverConfig { sample_rate: fs, offset_hz, symbol_rate: rs, rolloff });
    let mut ts = Vec::new();
    let started = std::time::Instant::now();
    for chunk in x.chunks(16_384) {
        rx.process(chunk, &mut ts);
    }
    rx.finish(&mut ts);
    let elapsed = started.elapsed().as_secs_f64();
    let duration = x.len() as f64 / fs;
    println!(
        "Akış hızı: {duration:.1} s kayıt {elapsed:.2} s'de işlendi ({:.1}× gerçek zaman)",
        duration / elapsed
    );
    let s = &rx.stats;
    println!(
        "Akış alıcısı: kilit={}, {} çerçeve çözüldü, {} başarısız, {} BCH hatası, {} desteklenmeyen",
        s.locked, s.frames_ok, s.frames_failed, s.bch_errors, s.frames_unsupported
    );
    println!(
        "  SNR {:.1} dB, taşıyıcı kayması {:+.2} kHz, {} TS paketi, {} CRC hatası",
        s.snr_db,
        s.carrier_offset_hz / 1e3,
        s.ts_packets,
        s.ts_crc_errors
    );
    if let Some(p) = s.pls {
        println!("  {}", p.describe());
    }
    print_service(&s.service);
    std::fs::write(ts_path, &ts).map_err(|e| format!("{}: {e}", ts_path.display()))?;
    println!("  TS dosyası: {}", ts_path.display());
    Ok(())
}

/// Örneklere, ortalama güçlerine göre `db` seviyesinde karmaşık Gauss gürültüsü ekler.
fn add_noise(x: &mut [Complex<f32>], db: f64) {
    let power = x.iter().map(|z| z.norm_sqr() as f64).sum::<f64>() / x.len() as f64;
    let sigma = (power * 10f64.powf(db / 10.0) / 2.0).sqrt();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut uniform = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    for z in x.iter_mut() {
        // Box-Muller
        let (u1, u2) = (uniform(), uniform());
        let r = (-2.0 * u1.ln()).sqrt() * sigma;
        let th = std::f64::consts::TAU * u2;
        *z += Complex::new((r * th.cos()) as f32, (r * th.sin()) as f32);
    }
}

fn print_service(s: &qoda::tsinfo::ServiceInfo) {
    if s.name.is_none() && s.streams.is_empty() {
        return;
    }
    println!(
        "  Servis: {} · sağlayıcı: {} · {}",
        s.name.as_deref().unwrap_or("?"),
        s.provider.as_deref().unwrap_or("?"),
        s.describe_streams()
    );
}

fn nearest_rolloff(r: f32) -> f64 {
    [0.35, 0.25, 0.20]
        .into_iter()
        .min_by(|a: &f64, b: &f64| (a - r as f64).abs().total_cmp(&(b - r as f64).abs()))
        .unwrap()
}
