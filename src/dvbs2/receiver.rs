//! Akış hâlinde DVB-S2 alıcı: IQ örnekleri parça parça girer, MPEG-TS çıkar.
//!
//! Zincir: frekans kaydırma → RRC eşleştirilmiş filtre → AGC → Gardner zamanlama →
//! PL başlık senkronu → karıştırma çözümü + taşıyıcı takibi + demap → LDPC → BCH →
//! BBFRAME → TS. Senkron bulunduktan sonra her başlık beklenen konumda doğrulanır.

use std::f64::consts::TAU;

use num_complex::Complex;

use super::bbframe::{self, TsDepacketizer};
use super::fec::{Bch, FecParams};
use super::filters;
use super::ldpc::LdpcCode;
use super::plframe::{self, CarrierState};
use super::plheader::{HEADER_LEN, HeaderHit, HeaderSearch, Modulation, Pls};
use super::timing::TimingRecovery;

const TIMING_BANDWIDTH: f64 = 0.005;
const CARRIER_BANDWIDTH: f32 = 0.002;
const LDPC_MAX_ITERATIONS: usize = 50;
/// Kilitsizken başlık aramak için biriktirilen sembol sayısı (bir normal çerçeveden fazla).
const SEARCH_WINDOW: usize = 40_000;
/// Kilitliyken beklenen başlığın etrafında aranan sembol payı.
const VERIFY_SLACK: usize = 16;
/// Üst üste bu kadar başlık doğrulanamazsa kilit bırakılır.
const MAX_MISSES: u32 = 3;
/// Başlık kabulü için en düşük PLS güveni.
const MIN_PLS_MARGIN: f32 = 0.3;
/// Kilitliyken başlığın yerinde sayılması için en düşük SOF ilişkisi.
const MIN_LOCKED_SOF: f32 = 0.8;
/// Başlıklardan ölçülen frekansın üstel ortalama katsayısı.
const FREQ_AVERAGING: f32 = 0.1;

pub struct ReceiverConfig {
    pub sample_rate: f64,
    /// Yayın merkezinin alınan bandın merkezine göre ofseti (Hz).
    pub offset_hz: f64,
    pub symbol_rate: f64,
    pub rolloff: f64,
}

#[derive(Default, Clone)]
pub struct ReceiverStats {
    pub locked: bool,
    pub pls: Option<Pls>,
    pub frames_ok: u64,
    /// LDPC'nin yakınsamadığı çerçeveler.
    pub frames_failed: u64,
    pub bch_errors: u64,
    /// Henüz desteklenmeyen modülasyondaki çerçeveler.
    pub frames_unsupported: u64,
    /// Son çerçeve gövdesinin SNR'ı (dB).
    pub snr_db: f32,
    /// Ayar frekansına göre kalan taşıyıcı kayması (Hz).
    pub carrier_offset_hz: f64,
    pub ts_packets: u64,
    pub ts_crc_errors: u64,
}

struct FecState {
    pls: Pls,
    params: FecParams,
    ldpc: LdpcCode,
    bch: Bch,
}

pub struct Receiver {
    cfg: ReceiverConfig,
    mix_phase: f64,
    mix_step: f64,
    taps: Vec<f32>,
    history: Vec<Complex<f32>>,
    agc_power: Option<f32>,
    timing: TimingRecovery,
    symbols: Vec<Complex<f32>>,
    search: HeaderSearch,
    /// Kilitliyken bir sonraki başlığın `symbols` içindeki beklenen konumu.
    expected: Option<usize>,
    last_frame_len: usize,
    misses: u32,
    /// Başlıklardan ölçülen ortak frekans kaymasının ortalaması (radyan/sembol).
    freq: Option<f32>,
    fec: Option<FecState>,
    scrambling: Vec<u8>,
    depacketizer: TsDepacketizer,
    pub stats: ReceiverStats,
}

impl Receiver {
    pub fn new(cfg: ReceiverConfig) -> Self {
        let sps = cfg.sample_rate / cfg.symbol_rate;
        let taps = filters::rrc_taps(sps, cfg.rolloff, 8);
        Self {
            mix_phase: 0.0,
            mix_step: -TAU * cfg.offset_hz / cfg.sample_rate,
            history: vec![Complex::default(); taps.len() - 1],
            taps,
            agc_power: None,
            timing: TimingRecovery::new(sps, TIMING_BANDWIDTH),
            symbols: Vec::new(),
            search: HeaderSearch::new(),
            expected: None,
            last_frame_len: 0,
            misses: 0,
            freq: None,
            fec: None,
            scrambling: Vec::new(),
            depacketizer: TsDepacketizer::default(),
            stats: ReceiverStats::default(),
            cfg,
        }
    }

    /// IQ örneklerini işler; çözülen TS paketlerini `ts`e ekler.
    pub fn process(&mut self, iq: &[Complex<f32>], ts: &mut Vec<u8>) {
        let filtered = self.front_end(iq);
        let mut symbols = Vec::with_capacity(filtered.len());
        self.timing.process(&filtered, &mut symbols);
        self.symbols.extend_from_slice(&symbols);
        self.frame_sync(ts);
    }

    /// Frekans kaydırma, RRC filtre ve AGC.
    fn front_end(&mut self, iq: &[Complex<f32>]) -> Vec<Complex<f32>> {
        let mut buf = std::mem::take(&mut self.history);
        buf.reserve(iq.len());
        for s in iq {
            buf.push(s * Complex::from_polar(1.0, self.mix_phase as f32));
            self.mix_phase = (self.mix_phase + self.mix_step).rem_euclid(TAU);
        }
        let l = self.taps.len();
        let mut out: Vec<Complex<f32>> = (0..buf.len() + 1 - l)
            .map(|i| buf[i..i + l].iter().zip(&self.taps).map(|(x, &h)| x * h).sum())
            .collect();
        self.history = buf[buf.len() + 1 - l..].to_vec();

        if !out.is_empty() {
            let p = out.iter().map(|z| z.norm_sqr()).sum::<f32>() / out.len() as f32;
            let avg = match self.agc_power {
                Some(a) => 0.9 * a + 0.1 * p,
                None => p,
            };
            self.agc_power = Some(avg);
            if avg > 0.0 {
                let k = 1.0 / avg.sqrt();
                out.iter_mut().for_each(|z| *z *= k);
            }
        }
        out
    }

    fn frame_sync(&mut self, ts: &mut Vec<u8>) {
        loop {
            let Some(expected) = self.expected else {
                if self.symbols.len() < SEARCH_WINDOW {
                    return;
                }
                let found = self
                    .search
                    .find_all(&self.symbols, 0.7)
                    .into_iter()
                    .find(|h| h.pls_margin >= 0.5 && h.pls.frame_len().is_some());
                match found {
                    Some(h) => {
                        self.expected = Some(h.position);
                        self.misses = 0;
                    }
                    None => {
                        let cut = self.symbols.len() - HEADER_LEN;
                        self.symbols.drain(..cut);
                        return;
                    }
                }
                continue;
            };

            if self.symbols.len() < expected + HEADER_LEN + VERIFY_SLACK {
                return;
            }
            let lo = expected.saturating_sub(VERIFY_SLACK);
            let window = &self.symbols[lo..expected + HEADER_LEN + VERIFY_SLACK];
            // Kilitliyken SOF yerindeyse PLS güveni düşük olsa da çerçeve kabul edilir:
            // aynı sinyalleşme geldiyse olduğu gibi, belirsizse önceki sinyalleşmeyle.
            let last = self.stats.pls;
            let hit = self.search.best(window).and_then(|mut h| {
                let confident = h.pls_margin >= MIN_PLS_MARGIN && h.pls.frame_len().is_some();
                if confident || Some(h.pls) == last {
                    Some(h)
                } else if h.sof_metric >= MIN_LOCKED_SOF {
                    h.pls = last?;
                    Some(h)
                } else {
                    None
                }
            });

            let Some(mut hit) = hit else {
                if std::env::var_os("QODA_DEBUG").is_some() {
                    let b = self.search.best(window);
                    eprintln!(
                        "başlık doğrulanamadı: çerçeve {} en iyi {:?}",
                        self.stats.frames_ok + self.stats.frames_failed,
                        b.map(|h| (h.position, h.sof_metric, h.pls_margin, h.pls))
                    );
                }
                self.misses += 1;
                if self.misses >= MAX_MISSES || self.last_frame_len == 0 {
                    self.expected = None;
                    self.stats.locked = false;
                } else {
                    // Başlık bu sefer okunamadı; çerçeve uzunluğu değişmemiş varsayılır.
                    self.advance(expected + self.last_frame_len);
                }
                continue;
            };
            hit.position += lo;
            let len = hit.pls.frame_len().unwrap();
            if self.symbols.len() < hit.position + len {
                return;
            }
            self.decode_frame(&hit, ts);
            self.misses = 0;
            self.last_frame_len = len;
            self.stats.locked = true;
            self.stats.pls = Some(hit.pls);
            self.advance(hit.position + len);
        }
    }

    /// Bir sonraki başlığı `next` konumunda bekle ve önceki sembolleri at.
    fn advance(&mut self, next: usize) {
        let cut = next.saturating_sub(VERIFY_SLACK).min(self.symbols.len());
        self.symbols.drain(..cut);
        self.expected = Some(next - cut);
    }

    fn decode_frame(&mut self, hit: &HeaderHit, ts: &mut Vec<u8>) {
        let pls = hit.pls;
        if pls.modcod == 0 {
            return; // dummy çerçeve
        }
        let Some((modulation, rate)) = pls.modulation_and_rate() else { return };

        // Tek başlığın frekans tahmini gürültülüdür; ortak kayma yavaşça ortalanır.
        let w = hit.freq_rad_per_symbol;
        if hit.pls_margin > 0.5 {
            self.freq = Some(self.freq.map_or(w, |f| f + FREQ_AVERAGING * (w - f)));
        }
        let freq = self.freq.unwrap_or(w);
        self.stats.carrier_offset_hz = freq as f64 * self.cfg.symbol_rate / TAU;

        if modulation != Modulation::Qpsk {
            self.stats.frames_unsupported += 1;
            return;
        }
        if self.fec.as_ref().is_none_or(|f| f.pls != pls) {
            let Some(params) = FecParams::new(rate, pls.short_frame) else { return };
            self.fec = Some(FecState { pls, ldpc: params.ldpc(), bch: params.bch(), params });
        }

        let start = hit.position + HEADER_LEN;
        let body_len = pls.frame_len().unwrap() - HEADER_LEN;
        if self.scrambling.len() < body_len {
            self.scrambling = plframe::scrambling_sequence(0, body_len);
        }
        // Başlığın ortasındaki faz, başlığın kendi frekans tahminine göre en güvenilir noktadır;
        // oradan gövde başına ortak frekansla ilerlenir.
        let mid = HEADER_LEN as f32 / 2.0;
        let carrier = CarrierState {
            phase: hit.phase + hit.freq_rad_per_symbol * mid + freq * mid,
            freq,
        };
        let body = &self.symbols[start..start + body_len];
        let demapped =
            plframe::demap_qpsk(body, &self.scrambling[..body_len], pls.pilot_blocks(), carrier, CARRIER_BANDWIDTH);
        self.stats.snr_db = demapped.snr_db;

        let fec = self.fec.as_ref().unwrap();
        let decoded = fec.ldpc.decode(&demapped.llr, LDPC_MAX_ITERATIONS);
        if !decoded.converged {
            if std::env::var_os("QODA_DEBUG").is_some() {
                eprintln!(
                    "LDPC başarısız: çerçeve {} marj {:.2} w {:.4} ort {:.4} snr {:.1}",
                    self.stats.frames_ok + self.stats.frames_failed,
                    hit.pls_margin,
                    w,
                    freq,
                    demapped.snr_db
                );
            }
            self.stats.frames_failed += 1;
            return;
        }
        self.stats.frames_ok += 1;
        if !fec.bch.is_codeword(&decoded.bits[..fec.params.k_ldpc]) {
            self.stats.bch_errors += 1;
        }
        let mut bytes: Vec<u8> = decoded.bits[..fec.params.k_bch]
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect();
        bbframe::bb_descramble(&mut bytes);
        self.depacketizer.push(&bytes, ts);
        self.stats.ts_packets = self.depacketizer.packets as u64;
        self.stats.ts_crc_errors = self.depacketizer.crc_errors as u64;
    }
}
