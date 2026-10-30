//! DVB-S2 physical layer frame header (PLHEADER).
//!
//! ETSI EN 302 307-1, clause 5.5.2: every PLFRAME starts with a 90-symbol header.
//! The header is π/2-BPSK modulated and has two parts:
//! - SOF (start of frame): fixed 26 bits, for finding the start of the frame.
//! - PLS code: 7-bit signalling (MODCOD 5 bits, TYPE 2 bits) expanded to 64 bits
//!   with a (64,7) code and scrambled with a fixed sequence.
//!
//! MODCOD gives the modulation and FEC rate, TYPE the frame size and whether
//! pilot symbols are present; so the receiver learns these from the header.

use std::f32::consts::FRAC_1_SQRT_2;

use num_complex::Complex;

pub const HEADER_LEN: usize = 90;
const SOF_LEN: usize = 26;
const SOF_BITS: u32 = 0x18D_2E82;
const PLS_SCRAMBLE: u64 = 0x719D_83C9_5342_2DFA;
/// Reed-Muller rows generating the first 32 bits of the (64,7) code; most significant bit sent first.
const PLS_ROWS: [u32; 6] = [0x5555_5555, 0x3333_3333, 0x0F0F_0F0F, 0x00FF_00FF, 0x0000_FFFF, 0xFFFF_FFFF];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modulation {
    Qpsk,
    Psk8,
    Apsk16,
    Apsk32,
}

impl Modulation {
    pub fn bits_per_symbol(self) -> usize {
        match self {
            Self::Qpsk => 2,
            Self::Psk8 => 3,
            Self::Apsk16 => 4,
            Self::Apsk32 => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Qpsk => "QPSK",
            Self::Psk8 => "8PSK",
            Self::Apsk16 => "16APSK",
            Self::Apsk32 => "32APSK",
        }
    }
}

/// Signalling decoded from the PLS code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pls {
    /// 0 = dummy frame, 1..=28 DVB-S2 MODCODs.
    pub modcod: u8,
    pub short_frame: bool,
    pub pilots: bool,
}

impl Pls {
    pub fn from_code(code: u8) -> Self {
        Self { modcod: code >> 2, short_frame: code & 2 != 0, pilots: code & 1 != 0 }
    }

    /// Modulation and FEC rate (numerator, denominator). `None` for dummy and reserved values.
    pub fn modulation_and_rate(&self) -> Option<(Modulation, (u8, u8))> {
        use Modulation::*;
        const TABLE: [(Modulation, (u8, u8)); 28] = [
            (Qpsk, (1, 4)), (Qpsk, (1, 3)), (Qpsk, (2, 5)), (Qpsk, (1, 2)), (Qpsk, (3, 5)),
            (Qpsk, (2, 3)), (Qpsk, (3, 4)), (Qpsk, (4, 5)), (Qpsk, (5, 6)), (Qpsk, (8, 9)),
            (Qpsk, (9, 10)), (Psk8, (3, 5)), (Psk8, (2, 3)), (Psk8, (3, 4)), (Psk8, (5, 6)),
            (Psk8, (8, 9)), (Psk8, (9, 10)), (Apsk16, (2, 3)), (Apsk16, (3, 4)), (Apsk16, (4, 5)),
            (Apsk16, (5, 6)), (Apsk16, (8, 9)), (Apsk16, (9, 10)), (Apsk32, (3, 4)), (Apsk32, (4, 5)),
            (Apsk32, (5, 6)), (Apsk32, (8, 9)), (Apsk32, (9, 10)),
        ];
        TABLE.get((self.modcod as usize).checked_sub(1)?).copied()
    }

    /// Number of pilot blocks (36 symbols each) in the frame.
    pub fn pilot_blocks(&self) -> usize {
        let Some((m, _)) = self.modulation_and_rate() else { return 0 };
        if !self.pilots {
            return 0;
        }
        let bits = if self.short_frame { 16_200 } else { 64_800 };
        (bits / m.bits_per_symbol() / 90 - 1) / 16
    }

    /// PLFRAME length including the header (symbols). `None` for reserved MODCODs.
    pub fn frame_len(&self) -> Option<usize> {
        if self.modcod == 0 {
            return Some(HEADER_LEN + 36 * 90);
        }
        let (m, _) = self.modulation_and_rate()?;
        let bits = if self.short_frame { 16_200 } else { 64_800 };
        let slots = bits / m.bits_per_symbol() / 90;
        // One 36-symbol pilot block every 16 slots (none after the last slot).
        let pilot_blocks = if self.pilots { (slots - 1) / 16 } else { 0 };
        Some(HEADER_LEN + slots * 90 + pilot_blocks * 36)
    }

    pub fn describe(&self) -> String {
        let size = if self.short_frame { "short" } else { "normal" };
        let pilots = if self.pilots { "pilots" } else { "no pilots" };
        match (self.modcod, self.modulation_and_rate()) {
            (0, _) => "dummy frame".into(),
            (_, Some((m, (n, d)))) => format!("{} {n}/{d}, {size} frame, {pilots}", m.name()),
            (mc, None) => format!("reserved MODCOD {mc}"),
        }
    }
}

/// Scrambled 64-bit code of a 7-bit PLS value (most significant bit first).
pub fn pls_codeword(code: u8) -> u64 {
    let six = code >> 1;
    let last = (code & 1) as u64;
    let mut c32 = 0u32;
    for (k, row) in PLS_ROWS.iter().enumerate() {
        if (six >> (5 - k)) & 1 == 1 {
            c32 ^= row;
        }
    }
    let mut c64 = 0u64;
    for i in (0..32).rev() {
        let bit = ((c32 >> i) & 1) as u64;
        c64 = (c64 << 2) | (bit << 1) | (bit ^ last);
    }
    c64 ^ PLS_SCRAMBLE
}

/// π/2-BPSK mapping for symbol `index` (0-based) of the header.
fn pi2_bpsk(index: usize, bit: u8) -> Complex<f32> {
    let sign = if bit == 0 { FRAC_1_SQRT_2 } else { -FRAC_1_SQRT_2 };
    // The standard counts from 1: odd indices (1+j), even indices (-1+j).
    if index % 2 == 0 { Complex::new(sign, sign) } else { Complex::new(-sign, sign) }
}

/// Ideal 90-symbol header for a given PLS value.
pub fn header_symbols(code: u8) -> [Complex<f32>; HEADER_LEN] {
    let cw = pls_codeword(code);
    std::array::from_fn(|i| {
        let bit = if i < SOF_LEN {
            (SOF_BITS >> (SOF_LEN - 1 - i)) & 1
        } else {
            ((cw >> (63 - (i - SOF_LEN))) & 1) as u32
        };
        pi2_bpsk(i, bit as u8)
    })
}

/// A frame header that was found.
#[derive(Debug, Clone)]
pub struct HeaderHit {
    /// Index of the header's first symbol.
    pub position: usize,
    pub pls: Pls,
    /// Normalized strength of the SOF correlation (0..1).
    pub sof_metric: f32,
    /// Confidence of the PLS decision: margin between the best and second-best candidate (normalized).
    pub pls_margin: f32,
    /// Frequency offset measured over the header (radians/symbol).
    pub freq_rad_per_symbol: f32,
    /// Carrier phase at the header's first symbol (radians).
    pub phase: f32,
    /// Per-symbol SNR measured from the known header symbols (dB).
    pub snr_db: f32,
}

/// Precomputed tables for header search (reference SOF and PLS codebook).
pub struct HeaderSearch {
    reference: [Complex<f32>; HEADER_LEN],
    ref_diff: Vec<Complex<f32>>,
    codebook: Vec<(u8, [f32; 64])>,
    /// The header's 89 consecutive symbol differences (conjugated) for each PLS code.
    header_diffs: Vec<[Complex<f32>; HEADER_LEN - 1]>,
}

impl Default for HeaderSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl HeaderSearch {
    pub fn new() -> Self {
        let reference = header_symbols(0);
        let ref_diff = (1..SOF_LEN).map(|i| reference[i] * reference[i - 1].conj()).collect();
        let codebook = (0u8..128)
            .map(|code| {
                let cw = pls_codeword(code);
                (code, std::array::from_fn(|i| if (cw >> (63 - i)) & 1 == 0 { 1.0 } else { -1.0 }))
            })
            .collect();
        let header_diffs = (0u8..128)
            .map(|code| {
                let h = header_symbols(code);
                std::array::from_fn(|i| (h[i + 1] * h[i].conj()).conj())
            })
            .collect();
        Self { reference, ref_diff, codebook, header_diffs }
    }

    /// Normalized correlation strength and raw correlation for an SOF starting at `p`.
    /// `diff[k] = s[k+1]·conj(s[k])`; the SOF's i-th difference is at `diff[p + i - 1]`.
    fn metric(&self, diff: &[Complex<f32>], p: usize) -> (f32, Complex<f32>) {
        let mut c = Complex::default();
        let mut norm = 0.0;
        for (i, r) in self.ref_diff.iter().enumerate() {
            let d = diff[p + i];
            c += d * r.conj();
            norm += d.norm();
        }
        (if norm > 0.0 { c.norm() / norm } else { 0.0 }, c)
    }

    fn differences(symbols: &[Complex<f32>]) -> Vec<Complex<f32>> {
        (1..symbols.len()).map(|k| symbols[k] * symbols[k - 1].conj()).collect()
    }

    /// Searches a symbol sequence for SOFs and decodes the PLS code of each header above the threshold.
    ///
    /// The SOF is searched for over the phase difference between consecutive symbols
    /// (differentially), so the carrier phase and small frequency offsets do not affect the search.
    pub fn find_all(&self, symbols: &[Complex<f32>], threshold: f32) -> Vec<HeaderHit> {
        let diff = Self::differences(symbols);
        let mut hits = Vec::new();
        let mut p = 0;
        while p + HEADER_LEN < symbols.len() {
            let (m, _) = self.metric(&diff, p);
            if m < threshold {
                p += 1;
                continue;
            }
            // Take the highest point in the region above the threshold.
            let mut best = (p, m);
            for q in p + 1..(p + SOF_LEN).min(symbols.len() - HEADER_LEN) {
                let (mq, _) = self.metric(&diff, q);
                if mq > best.1 {
                    best = (q, mq);
                }
            }
            hits.push(self.decode_at(symbols, &diff, best.0));
            p = best.0 + HEADER_LEN;
        }
        hits
    }

    /// Decodes the header at the strongest SOF position in the window (for verification while locked).
    /// If `freq` is given, the frequency is not estimated from the header; this value is used.
    pub fn best(&self, symbols: &[Complex<f32>], freq: Option<f32>) -> Option<HeaderHit> {
        if symbols.len() <= HEADER_LEN {
            return None;
        }
        let diff = Self::differences(symbols);
        let pos = (0..symbols.len() - HEADER_LEN).max_by(|&a, &b| {
            self.metric(&diff, a).0.total_cmp(&self.metric(&diff, b).0)
        })?;
        Some(self.decode(symbols, &diff, pos, freq))
    }

    fn decode_at(&self, symbols: &[Complex<f32>], diff: &[Complex<f32>], pos: usize) -> HeaderHit {
        self.decode(symbols, diff, pos, None)
    }

    fn decode(&self, symbols: &[Complex<f32>], diff: &[Complex<f32>], pos: usize, freq: Option<f32>) -> HeaderHit {
        let (sof_metric, _) = self.metric(diff, pos);
        let header = &symbols[pos..pos + HEADER_LEN];

        let w = match freq {
            Some(w) => w,
            None => {
                // Coarse frequency: correlation with the 89 symbol differences of each PLS
                // candidate; the strongest candidate wins. This measure cannot tell a code from
                // its complement, so the fine estimate uses pairs inside the SOF and PLS regions.
                let hdiff = &diff[pos..pos + HEADER_LEN - 1];
                let (code, acc) = self
                    .header_diffs
                    .iter()
                    .enumerate()
                    .map(|(c, r)| (c as u8, hdiff.iter().zip(r).map(|(d, r)| d * r).sum::<Complex<f32>>()))
                    .max_by(|a, b| a.1.norm_sqr().total_cmp(&b.1.norm_sqr()))
                    .unwrap();
                refine_frequency(header, &header_symbols(code), acc.arg(), true)
            }
        };
        let (code, pls_margin) = decode_pls(header, w, &self.reference[..SOF_LEN], &self.codebook);
        let ideal = header_symbols(code);
        // Once the code is known, refine the frequency once more with all 90 symbols.
        let w = if freq.is_some() { w } else { refine_frequency(header, &ideal, w, false) };

        // SNR from the 90 known symbols: amplitude and error power.
        let aligned = align(header, w, &ideal);
        let amp = aligned.iter().zip(&ideal).map(|(z, h)| (z * h.conj()).re).sum::<f32>() / HEADER_LEN as f32;
        let noise = aligned.iter().zip(&ideal).map(|(z, h)| (z - h * amp).norm_sqr()).sum::<f32>() / HEADER_LEN as f32;
        let snr_db = 10.0 * (amp * amp / noise.max(1e-12)).log10();

        HeaderHit {
            position: pos,
            pls: Pls::from_code(code),
            sof_metric,
            pls_margin,
            freq_rad_per_symbol: w,
            phase: carrier_phase(header, w, &ideal),
            snr_db,
        }
    }
}

/// Removes the modulation with `ideal` and refines the frequency with growing lags
/// (10, 40 symbols): the estimate gets finer as the lag grows, and each step's
/// ambiguity limit stays wider than the previous step's error. If `split` is true,
/// pairs crossing the SOF and PLS regions are not used (when the code's complement is not yet resolved).
fn refine_frequency(header: &[Complex<f32>], ideal: &[Complex<f32>; HEADER_LEN], w0: f32, split: bool) -> f32 {
    let mut w = w0;
    for lag in [10usize, 40] {
        let y: Vec<Complex<f32>> = header
            .iter()
            .zip(ideal)
            .enumerate()
            .map(|(i, (z, h))| z * h.conj() * Complex::from_polar(1.0, -w * i as f32))
            .collect();
        let acc: Complex<f32> = (lag..HEADER_LEN)
            .filter(|&i| !split || (i < SOF_LEN) == (i - lag < SOF_LEN))
            .map(|i| y[i] * y[i - lag].conj())
            .sum();
        if acc.norm_sqr() > 0.0 {
            w += acc.arg() / lag as f32;
        }
    }
    w
}

/// Searches a symbol sequence for all headers; see [`HeaderSearch::find_all`].
pub fn find_headers(symbols: &[Complex<f32>], threshold: f32) -> Vec<HeaderHit> {
    HeaderSearch::new().find_all(symbols, threshold)
}

/// Carrier phase at the header's first symbol, measured against the first
/// `known.len()` known symbols after removing the frequency offset `w`.
fn carrier_phase(header: &[Complex<f32>], w: f32, known: &[Complex<f32>]) -> f32 {
    let acc: Complex<f32> = header
        .iter()
        .zip(known)
        .enumerate()
        .map(|(i, (z, h))| z * Complex::from_polar(1.0, -w * i as f32) * h.conj())
        .sum();
    acc.arg()
}

/// Removes the frequency offset and carrier phase, aligning the header with the ideal symbols.
fn align(header: &[Complex<f32>], w: f32, known: &[Complex<f32>]) -> Vec<Complex<f32>> {
    let phase = carrier_phase(header, w, known);
    header
        .iter()
        .enumerate()
        .map(|(i, z)| z * Complex::from_polar(1.0, -w * i as f32 - phase))
        .collect()
}

/// Picks the PLS code as the most correlated candidate; returns (code, confidence margin).
fn decode_pls(header: &[Complex<f32>], w: f32, known: &[Complex<f32>], codebook: &[(u8, [f32; 64])]) -> (u8, f32) {
    let aligned = align(header, w, known);
    // Undo the π/2 rotation; the real part is a soft bit proportional to (1 - 2b).
    let soft: Vec<f32> = (0..64)
        .map(|i| {
            let k = SOF_LEN + i;
            (aligned[k] * pi2_bpsk(k, 0).conj()).re
        })
        .collect();
    let mut scores: Vec<(f32, u8)> = codebook
        .iter()
        .map(|(code, cw)| (cw.iter().zip(&soft).map(|(c, s)| c * s).sum::<f32>(), *code))
        .collect();
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let total: f32 = soft.iter().map(|s| s.abs()).sum();
    let margin = if total > 0.0 { (scores[0].0 - scores[1].0) / total } else { 0.0 };
    (scores[0].1, margin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_lengths_match_standard() {
        // QPSK normal frame: 360 slots; 32490 symbols without pilots, 33282 with 22 pilot blocks.
        let qpsk_half = |pilots| Pls { modcod: 4, short_frame: false, pilots };
        assert_eq!(qpsk_half(false).frame_len(), Some(32_490));
        assert_eq!(qpsk_half(true).frame_len(), Some(33_282));
        // 8PSK short frame: 60 slots, 3 pilot blocks.
        assert_eq!(Pls { modcod: 13, short_frame: true, pilots: true }.frame_len(), Some(90 + 60 * 90 + 3 * 36));
        assert_eq!(Pls { modcod: 0, short_frame: false, pilots: false }.frame_len(), Some(3330));
    }

    #[test]
    fn pls_codewords_are_far_apart() {
        // (64,7) bi-orthogonal code: at least 32 bits differ between different codes.
        for a in 0u8..128 {
            for b in a + 1..128 {
                let d = (pls_codeword(a) ^ pls_codeword(b)).count_ones();
                assert!(d >= 32, "{d} between {a} and {b}");
            }
        }
    }

    #[test]
    fn finds_header_with_phase_and_frequency_offset() {
        let code = (7 << 2) | 1; // QPSK 3/4, normal, with pilots
        let header = header_symbols(code);
        let mut rng = crate::dvbs2::timing::tests::Lcg(3);
        let s = FRAC_1_SQRT_2;
        let mut symbols: Vec<Complex<f32>> = (0..500)
            .map(|_| Complex::new(if rng.next_bit() == 1 { s } else { -s }, if rng.next_bit() == 1 { s } else { -s }))
            .collect();
        symbols[200..290].copy_from_slice(&header);
        let w = 0.01f32;
        for (n, z) in symbols.iter_mut().enumerate() {
            *z *= Complex::from_polar(1.0, 1.1 + w * n as f32);
        }
        let hits = find_headers(&symbols, 0.7);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].position, 200);
        assert_eq!(hits[0].pls, Pls::from_code(code));
        assert!((hits[0].freq_rad_per_symbol - w).abs() < 1e-3);
    }
}
