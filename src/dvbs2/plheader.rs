//! DVB-S2 fiziksel katman çerçeve başlığı (PLHEADER).
//!
//! ETSI EN 302 307-1, bölüm 5.5.2: her PLFRAME 90 sembollük bir başlıkla başlar.
//! Başlık π/2-BPSK ile modüle edilir ve iki parçadan oluşur:
//! - SOF (start of frame): sabit 26 bit, çerçeve başını bulmak için.
//! - PLS kodu: 7 bitlik sinyalleşme (MODCOD 5 bit, TYPE 2 bit) bir (64,7)
//!   kodla 64 bite genişletilip sabit bir diziyle karıştırılır.
//!
//! MODCOD modülasyonu ve FEC oranını, TYPE çerçeve boyutunu ve pilot
//! sembollerinin varlığını verir; yani alıcı bu bilgileri başlıktan öğrenir.

use std::f32::consts::FRAC_1_SQRT_2;

use num_complex::Complex;

pub const HEADER_LEN: usize = 90;
const SOF_LEN: usize = 26;
const SOF_BITS: u32 = 0x18D_2E82;
const PLS_SCRAMBLE: u64 = 0x719D_83C9_5342_2DFA;
/// (64,7) kodun ilk 32 bitini üreten Reed-Muller satırları; en anlamlı bit önce gönderilir.
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

/// PLS kodundan çözülen sinyalleşme bilgisi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pls {
    /// 0 = dummy çerçeve, 1..=28 DVB-S2 MODCOD'ları.
    pub modcod: u8,
    pub short_frame: bool,
    pub pilots: bool,
}

impl Pls {
    pub fn from_code(code: u8) -> Self {
        Self { modcod: code >> 2, short_frame: code & 2 != 0, pilots: code & 1 != 0 }
    }

    /// Modülasyon ve FEC oranı (pay, payda). Dummy ve ayrılmış değerlerde `None`.
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

    /// Çerçevedeki pilot bloğu sayısı (36 sembollük).
    pub fn pilot_blocks(&self) -> usize {
        let Some((m, _)) = self.modulation_and_rate() else { return 0 };
        if !self.pilots {
            return 0;
        }
        let bits = if self.short_frame { 16_200 } else { 64_800 };
        (bits / m.bits_per_symbol() / 90 - 1) / 16
    }

    /// Başlık dahil PLFRAME uzunluğu (sembol). Ayrılmış MODCOD'larda `None`.
    pub fn frame_len(&self) -> Option<usize> {
        if self.modcod == 0 {
            return Some(HEADER_LEN + 36 * 90);
        }
        let (m, _) = self.modulation_and_rate()?;
        let bits = if self.short_frame { 16_200 } else { 64_800 };
        let slots = bits / m.bits_per_symbol() / 90;
        // 16 yuvada bir 36 sembollük pilot bloğu (son yuvadan sonra yok).
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

/// 7 bitlik PLS değerinin karıştırılmış 64 bitlik kodu (en anlamlı bit önce).
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

/// Başlığın `index`. sembolü (0 tabanlı) için π/2-BPSK eşlemesi.
fn pi2_bpsk(index: usize, bit: u8) -> Complex<f32> {
    let sign = if bit == 0 { FRAC_1_SQRT_2 } else { -FRAC_1_SQRT_2 };
    // Standart 1 tabanlı sayar: tek indisler (1+j), çift indisler (-1+j).
    if index % 2 == 0 { Complex::new(sign, sign) } else { Complex::new(-sign, sign) }
}

/// Belirli bir PLS değeri için 90 sembollük ideal başlık.
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

/// Bulunan bir çerçeve başlığı.
#[derive(Debug, Clone)]
pub struct HeaderHit {
    /// Başlığın ilk sembolünün indeksi.
    pub position: usize,
    pub pls: Pls,
    /// SOF ilişkisinin normalize gücü (0..1).
    pub sof_metric: f32,
    /// PLS kararının güveni: en iyi ile ikinci en iyi aday arasındaki fark (normalize).
    pub pls_margin: f32,
    /// Başlık boyunca ölçülen frekans kayması (radyan/sembol).
    pub freq_rad_per_symbol: f32,
    /// Başlığın ilk sembolündeki taşıyıcı fazı (radyan).
    pub phase: f32,
    /// Bilinen başlık sembollerinden ölçülen sembol başına SNR (dB).
    pub snr_db: f32,
}

/// Başlık arama için önceden hesaplanan tablolar (referans SOF ve PLS kod kitabı).
pub struct HeaderSearch {
    reference: [Complex<f32>; HEADER_LEN],
    ref_diff: Vec<Complex<f32>>,
    codebook: Vec<(u8, [f32; 64])>,
    /// Her PLS kodu için başlığın 89 ardışık sembol farkı (eşlenikli).
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

    /// `p` konumunda başlayan SOF için normalize ilişki gücü ve ham ilişki.
    /// `diff[k] = s[k+1]·conj(s[k])`; SOF'un i. farkı `diff[p + i - 1]`'dedir.
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

    /// Sembol dizisinde SOF arayıp eşiği aşan her başlığın PLS kodunu çözer.
    ///
    /// SOF, ardışık semboller arasındaki faz farkı (diferansiyel) üzerinden aranır;
    /// bu sayede taşıyıcı fazı ve küçük frekans kaymaları aramayı etkilemez.
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
            // Eşiği aşan bölgedeki en yüksek noktayı al.
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

    /// Penceredeki en güçlü SOF konumundaki başlığı çözer (kilitliyken doğrulama için).
    /// `freq` verilirse başlıktan frekans tahmin edilmez, bu değer kullanılır.
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
                // Kaba frekans: her PLS adayı için 89 sembol farkıyla ilişki; en güçlü
                // aday seçilir. Bu ölçüt bir kodu tümleyeninden ayıramaz, o yüzden
                // ince tahmin SOF ve PLS bölgelerinin kendi içindeki çiftlerle yapılır.
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
        // Kod belli olunca 90 sembolün tamamıyla frekansı bir kez daha incelt.
        let w = if freq.is_some() { w } else { refine_frequency(header, &ideal, w, false) };

        // Bilinen 90 sembolle SNR: genlik ve hata gücü.
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

/// Modülasyonu `ideal` ile kaldırıp frekansı artan gecikmelerle (10, 40 sembol)
/// inceltir: gecikme büyüdükçe tahmin hassaslaşır, her adımın belirsizlik sınırı
/// bir öncekinin hatasından geniş kalır. `split` true ise SOF ve PLS bölgelerini
/// aşan çiftler kullanılmaz (kodun tümleyeni henüz ayırt edilmemişse).
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

/// Sembol dizisindeki tüm başlıkları arar; bkz. [`HeaderSearch::find_all`].
pub fn find_headers(symbols: &[Complex<f32>], threshold: f32) -> Vec<HeaderHit> {
    HeaderSearch::new().find_all(symbols, threshold)
}

/// Başlığın ilk sembolündeki taşıyıcı fazı; frekans kayması `w` çıkarıldıktan
/// sonra bilinen ilk `known.len()` sembole göre ölçülür.
fn carrier_phase(header: &[Complex<f32>], w: f32, known: &[Complex<f32>]) -> f32 {
    let acc: Complex<f32> = header
        .iter()
        .zip(known)
        .enumerate()
        .map(|(i, (z, h))| z * Complex::from_polar(1.0, -w * i as f32) * h.conj())
        .sum();
    acc.arg()
}

/// Frekans kaymasını ve taşıyıcı fazını çıkarıp başlığı ideal sembollerle hizalar.
fn align(header: &[Complex<f32>], w: f32, known: &[Complex<f32>]) -> Vec<Complex<f32>> {
    let phase = carrier_phase(header, w, known);
    header
        .iter()
        .enumerate()
        .map(|(i, z)| z * Complex::from_polar(1.0, -w * i as f32 - phase))
        .collect()
}

/// PLS kodunu en yüksek ilişkili aday olarak seçer; (kod, güven payı) döndürür.
fn decode_pls(header: &[Complex<f32>], w: f32, known: &[Complex<f32>], codebook: &[(u8, [f32; 64])]) -> (u8, f32) {
    let aligned = align(header, w, known);
    // π/2 dönüşünü geri al; gerçel kısım (1 - 2b) ile orantılı yumuşak bittir.
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
        // QPSK normal çerçeve: 360 yuva; pilotsuz 32490, pilotlu 22 blokla 33282 sembol.
        let qpsk_half = |pilots| Pls { modcod: 4, short_frame: false, pilots };
        assert_eq!(qpsk_half(false).frame_len(), Some(32_490));
        assert_eq!(qpsk_half(true).frame_len(), Some(33_282));
        // 8PSK kısa çerçeve: 60 yuva, 3 pilot bloğu.
        assert_eq!(Pls { modcod: 13, short_frame: true, pilots: true }.frame_len(), Some(90 + 60 * 90 + 3 * 36));
        assert_eq!(Pls { modcod: 0, short_frame: false, pilots: false }.frame_len(), Some(3330));
    }

    #[test]
    fn pls_codewords_are_far_apart() {
        // (64,7) bi-ortogonal kod: farklı kodlar arasında en az 32 bit fark.
        for a in 0u8..128 {
            for b in a + 1..128 {
                let d = (pls_codeword(a) ^ pls_codeword(b)).count_ones();
                assert!(d >= 32, "{a} ve {b} arası {d}");
            }
        }
    }

    #[test]
    fn finds_header_with_phase_and_frequency_offset() {
        let code = (7 << 2) | 1; // QPSK 3/4, normal, pilotlu
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
