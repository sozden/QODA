//! DVB-S2 FEC parametreleri: MODCOD'a göre LDPC kodu ve BCH denetimi.
//!
//! ETSI EN 302 307-1, tablo 5a/5b (parametreler) ve 6a/6b (BCH polinomları).

use super::ldpc::{GROUP, LdpcCode};
use super::ldpc_tables as t;

/// Bir FEC çerçevesinin kodlama parametreleri.
pub struct FecParams {
    pub n_ldpc: usize,
    pub k_ldpc: usize,
    /// BBFRAME uzunluğu (bit).
    pub k_bch: usize,
    /// BCH düzeltme kapasitesi.
    pub t: usize,
    table: &'static [&'static [u16]],
}

impl FecParams {
    /// Kod oranı (pay, payda) ve çerçeve boyutuna göre parametreler.
    pub fn new(rate: (u8, u8), short: bool) -> Option<Self> {
        #[rustfmt::skip]
        let (k_bch, k_ldpc, bch_t, table): (usize, usize, usize, &'static [&'static [u16]]) = match (short, rate) {
            (false, (1, 4)) => (16_008, 16_200, 12, &t::NORMAL_1_4),
            (false, (1, 3)) => (21_408, 21_600, 12, &t::NORMAL_1_3),
            (false, (2, 5)) => (25_728, 25_920, 12, &t::NORMAL_2_5),
            (false, (1, 2)) => (32_208, 32_400, 12, &t::NORMAL_1_2),
            (false, (3, 5)) => (38_688, 38_880, 12, &t::NORMAL_3_5),
            (false, (2, 3)) => (43_040, 43_200, 10, &t::NORMAL_2_3),
            (false, (3, 4)) => (48_408, 48_600, 12, &t::NORMAL_3_4),
            (false, (4, 5)) => (51_648, 51_840, 12, &t::NORMAL_4_5),
            (false, (5, 6)) => (53_840, 54_000, 10, &t::NORMAL_5_6),
            (false, (8, 9)) => (57_472, 57_600, 8, &t::NORMAL_8_9),
            (false, (9, 10)) => (58_192, 58_320, 8, &t::NORMAL_9_10),
            (true, (1, 4)) => (3_072, 3_240, 12, &t::SHORT_1_4),
            (true, (1, 3)) => (5_232, 5_400, 12, &t::SHORT_1_3),
            (true, (2, 5)) => (6_312, 6_480, 12, &t::SHORT_2_5),
            (true, (1, 2)) => (7_032, 7_200, 12, &t::SHORT_1_2),
            (true, (3, 5)) => (9_552, 9_720, 12, &t::SHORT_3_5),
            (true, (2, 3)) => (10_632, 10_800, 12, &t::SHORT_2_3),
            (true, (3, 4)) => (11_712, 11_880, 12, &t::SHORT_3_4),
            (true, (4, 5)) => (12_432, 12_600, 12, &t::SHORT_4_5),
            (true, (5, 6)) => (13_152, 13_320, 12, &t::SHORT_5_6),
            (true, (8, 9)) => (14_232, 14_400, 12, &t::SHORT_8_9),
            _ => return None,
        };
        let n_ldpc = if short { 16_200 } else { 64_800 };
        Some(Self { n_ldpc, k_ldpc, k_bch, t: bch_t, table })
    }

    pub fn ldpc(&self) -> LdpcCode {
        LdpcCode::from_table(self.n_ldpc, self.k_ldpc, GROUP, self.table)
    }

    pub fn bch(&self) -> Bch {
        let polys: &[&[u8]] = if self.n_ldpc == 16_200 { &t::BCH_SHORT } else { &t::BCH_NORMAL };
        Bch::new(&polys[..self.t])
    }
}

/// BCH kodu: üreteç polinomu g(x) = g1(x)·…·gt(x) ile sözcük denetimi.
pub struct Bch {
    /// g(x) katsayıları, en yüksek dereceli terim hariç; bit i = x^i.
    generator: Vec<u64>,
    degree: usize,
}

impl Bch {
    fn new(polys: &[&[u8]]) -> Self {
        // GF(2) üzerinde polinom çarpımı; katsayılar bit dizisi olarak.
        let mut g = vec![1u8];
        for p in polys {
            let deg = *p.iter().max().unwrap() as usize;
            let mut out = vec![0u8; g.len() + deg];
            for (i, &gi) in g.iter().enumerate() {
                if gi == 1 {
                    for &e in p.iter() {
                        out[i + e as usize] ^= 1;
                    }
                }
            }
            g = out;
        }
        let degree = g.len() - 1;
        let mut generator = vec![0u64; degree.div_ceil(64)];
        for (i, &c) in g[..degree].iter().enumerate() {
            generator[i / 64] |= (c as u64) << (i % 64);
        }
        Self { generator, degree }
    }

    pub fn parity_bits(&self) -> usize {
        self.degree
    }

    /// Sözcük (ilk bit en yüksek dereceli katsayı) g(x)'e tam bölünüyor mu.
    pub fn is_codeword(&self, bits: &[u8]) -> bool {
        // Bit seri LFSR bölme: kalan register'da tutulur.
        let words = self.generator.len();
        let top = self.degree - 1;
        let mut reg = vec![0u64; words];
        for &b in bits {
            let feedback = ((reg[top / 64] >> (top % 64)) & 1) as u8 ^ b;
            for w in (0..words).rev() {
                let carry = if w > 0 { reg[w - 1] >> 63 } else { 0 };
                reg[w] = (reg[w] << 1) | carry;
            }
            if self.degree % 64 != 0 {
                reg[words - 1] &= (1u64 << (self.degree % 64)) - 1;
            }
            if feedback == 1 {
                for (r, g) in reg.iter_mut().zip(&self.generator) {
                    *r ^= g;
                }
            }
        }
        reg.iter().all(|&w| w == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_degrees_match_parity_lengths() {
        // BCH eşlik uzunluğu = N_bch - K_bch = 16·t (normal), 14·t (kısa).
        for (rate, short) in [((4, 5), false), ((2, 3), false), ((8, 9), false), ((1, 2), true)] {
            let p = FecParams::new(rate, short).unwrap();
            assert_eq!(p.bch().parity_bits(), p.k_ldpc - p.k_bch, "{rate:?} kısa={short}");
        }
    }

    #[test]
    fn ldpc_encoding_produces_valid_words_for_real_tables() {
        let p = FecParams::new((4, 5), false).unwrap();
        let code = p.ldpc();
        let mut rng = crate::dvbs2::timing::tests::Lcg(1);
        let info: Vec<u8> = (0..code.k).map(|_| rng.next_bit()).collect();
        assert!(code.is_codeword(&code.encode(&info)));
    }

    #[test]
    fn bch_accepts_multiples_of_generator() {
        // g(x)'in kendisi (en yüksek terim dahil) bir kod sözcüğüdür; tek bit hatası değildir.
        let bch = FecParams::new((4, 5), false).unwrap().bch();
        let n = 500;
        let mut word = vec![0u8; n];
        word[n - 1 - bch.degree] = 1;
        for i in 0..bch.degree {
            if (bch.generator[i / 64] >> (i % 64)) & 1 == 1 {
                word[n - 1 - i] = 1;
            }
        }
        assert!(bch.is_codeword(&word));
        word[100] ^= 1;
        assert!(!bch.is_codeword(&word));
    }
}

#[cfg(test)]
mod threshold_probe {
    use super::*;
    use crate::dvbs2::timing::tests::Lcg;

    /// İdeal BPSK LLR'leriyle LDPC eşiği (geliştirme için):
    /// `cargo test --lib ldpc_threshold -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ldpc_threshold() {
        for (rate, ebn0s) in [((1, 2), [1.0, 1.5, 2.0]), ((3, 5), [2.0, 2.5, 3.0]), ((2, 3), [2.5, 3.0, 3.5])] {
            let p = FecParams::new(rate, false).unwrap();
            let code = p.ldpc();
            let r = rate.0 as f64 / rate.1 as f64;
            for ebn0 in ebn0s {
                let mut rng = Lcg(7);
                let mut uniform = || {
                    let bits: u32 = (0..24).fold(0, |a, _| (a << 1) | rng.next_bit() as u32);
                    (bits as f64 + 0.5) / (1u32 << 24) as f64
                };
                let sigma = (1.0 / (2.0 * r * 10f64.powf(ebn0 / 10.0))).sqrt();
                let mut ok = 0;
                for _ in 0..4 {
                    let info: Vec<u8> = (0..code.k).map(|_| (uniform() < 0.5) as u8).collect();
                    let word = code.encode(&info);
                    let llr: Vec<f32> = word
                        .iter()
                        .map(|&b| {
                            let g = (-2.0 * uniform().ln()).sqrt() * (std::f64::consts::TAU * uniform()).cos();
                            let y = (1.0 - 2.0 * b as f64) + sigma * g;
                            (2.0 * y / (sigma * sigma)) as f32
                        })
                        .collect();
                    ok += code.decode(&llr, 50).converged as usize;
                }
                println!("oran {rate:?} Eb/N0 {ebn0} dB: {ok}/4");
            }
        }
    }
}
