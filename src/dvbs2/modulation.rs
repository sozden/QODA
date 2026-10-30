//! Takımyıldızlar, bit karıştırıcı ve yumuşak demap (QPSK, 8PSK, 16APSK).
//!
//! ETSI EN 302 307-1, bölüm 5.3.3 (bit karıştırıcı) ve 5.4 (eşleme). Nokta
//! dizinleri sembolün bitlerinden oluşur, ilk bit en anlamlı bittir.

use std::f32::consts::{FRAC_1_SQRT_2, PI};

use num_complex::Complex;

use super::plheader::Modulation;

/// Bir modülasyonun noktaları ve bit karıştırıcı düzeni.
pub struct Mapping {
    /// `points[i]`: bitleri `i` (MSB önce) olan sembol; ortalama enerji 1.
    pub points: Vec<Complex<f32>>,
    pub bits_per_symbol: usize,
    /// 8PSK 3/5'te karıştırıcı sütunları ters sırada okunur.
    reversed_columns: bool,
}

impl Mapping {
    /// Desteklenmeyen modülasyonda (32APSK) `None`.
    pub fn new(modulation: Modulation, rate: (u8, u8)) -> Option<Self> {
        let points = match modulation {
            Modulation::Qpsk => {
                let s = FRAC_1_SQRT_2;
                // İlk bit I'nın, ikinci bit Q'nun işaretini belirler (0 → pozitif).
                vec![Complex::new(s, s), Complex::new(s, -s), Complex::new(-s, s), Complex::new(-s, -s)]
            }
            Modulation::Psk8 => {
                // Gray kodlu, mutlak eşleme (şekil 10).
                const ANGLES: [f32; 8] = [0.25, 0.0, 1.0, 1.25, 0.5, 1.75, 0.75, 1.5];
                ANGLES.iter().map(|a| Complex::from_polar(1.0, a * PI)).collect()
            }
            Modulation::Apsk16 => {
                // 12 noktalı dış halka (R2) ve 4 noktalı iç halka (R1), şekil 11 ve tablo 9.
                let gamma: f32 = match rate {
                    (2, 3) => 3.15,
                    (3, 4) => 2.85,
                    (4, 5) => 2.75,
                    (5, 6) => 2.70,
                    (8, 9) => 2.60,
                    _ => 2.57,
                };
                let r1 = 2.0 / (1.0 + 3.0 * gamma * gamma).sqrt();
                let r2 = gamma * r1;
                const OUTER: [f32; 12] = [
                    3.0, -3.0, 9.0, -9.0, 1.0, -1.0, 11.0, -11.0, 5.0, -5.0, 7.0, -7.0,
                ];
                const INNER: [f32; 4] = [1.0, -1.0, 3.0, -3.0];
                OUTER
                    .iter()
                    .map(|k| Complex::from_polar(r2, k * PI / 12.0))
                    .chain(INNER.iter().map(|k| Complex::from_polar(r1, k * PI / 4.0)))
                    .collect()
            }
            Modulation::Apsk32 => return None,
        };
        let bits_per_symbol = modulation.bits_per_symbol();
        let reversed_columns = modulation == Modulation::Psk8 && rate == (3, 5);
        Some(Self { points, bits_per_symbol, reversed_columns })
    }

    /// `index`. sembolün `bit`. bitinin (0 = MSB) kod sözcüğündeki konumu.
    /// Karıştırıcı kod sözcüğünü sütun sütun yazar, satır satır okur; QPSK'da
    /// karıştırma yoktur.
    pub fn codeword_position(&self, n: usize, index: usize, bit: usize) -> usize {
        let columns = self.bits_per_symbol;
        if columns == 2 {
            return index * 2 + bit;
        }
        let rows = n / columns;
        let column = if self.reversed_columns { columns - 1 - bit } else { bit };
        column * rows + index
    }

    /// En yakın nokta.
    pub fn decide(&self, z: Complex<f32>) -> Complex<f32> {
        *self
            .points
            .iter()
            .min_by(|a, b| (z - *a).norm_sqr().total_cmp(&(z - *b).norm_sqr()))
            .unwrap()
    }

    /// Bir sembolün bitleri için max-log LLR'ler (pozitif → 0), `noise` karmaşık gürültü gücü.
    pub fn llrs(&self, z: Complex<f32>, noise: f32, out: &mut [f32]) {
        let dist: Vec<f32> = self.points.iter().map(|p| (z - p).norm_sqr()).collect();
        for (bit, llr) in out.iter_mut().enumerate().take(self.bits_per_symbol) {
            let mask = 1 << (self.bits_per_symbol - 1 - bit);
            let (mut d0, mut d1) = (f32::INFINITY, f32::INFINITY);
            for (i, &d) in dist.iter().enumerate() {
                if i & mask == 0 {
                    d0 = d0.min(d);
                } else {
                    d1 = d1.min(d);
                }
            }
            *llr = (d1 - d0) / noise;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constellations_have_unit_energy() {
        for (m, rate) in [(Modulation::Qpsk, (3, 4)), (Modulation::Psk8, (2, 3)), (Modulation::Apsk16, (3, 4))] {
            let map = Mapping::new(m, rate).unwrap();
            let e = map.points.iter().map(|p| p.norm_sqr()).sum::<f32>() / map.points.len() as f32;
            assert!((e - 1.0).abs() < 1e-4, "{m:?}: {e}");
        }
    }

    #[test]
    fn psk8_is_gray_coded() {
        // Çember üzerinde komşu noktaların bitleri yalnızca bir bitte farklı olmalı.
        let map = Mapping::new(Modulation::Psk8, (2, 3)).unwrap();
        let mut by_angle: Vec<(f32, usize)> =
            map.points.iter().enumerate().map(|(i, p)| (p.arg().rem_euclid(2.0 * PI), i)).collect();
        by_angle.sort_by(|a, b| a.0.total_cmp(&b.0));
        for k in 0..8 {
            let (a, b) = (by_angle[k].1, by_angle[(k + 1) % 8].1);
            assert_eq!((a ^ b).count_ones(), 1, "{a:03b} ve {b:03b}");
        }
    }

    #[test]
    fn interleaver_positions_are_a_permutation() {
        for (m, rate) in [(Modulation::Psk8, (2, 3)), (Modulation::Psk8, (3, 5)), (Modulation::Apsk16, (3, 4))] {
            let map = Mapping::new(m, rate).unwrap();
            let n = 16_200;
            let mut seen = vec![false; n];
            for i in 0..n / map.bits_per_symbol {
                for b in 0..map.bits_per_symbol {
                    let p = map.codeword_position(n, i, b);
                    assert!(!seen[p]);
                    seen[p] = true;
                }
            }
            assert!(seen.iter().all(|&s| s));
        }
    }

    #[test]
    fn llrs_decide_noiseless_points_correctly() {
        let map = Mapping::new(Modulation::Apsk16, (3, 4)).unwrap();
        let mut llr = [0.0; 4];
        for (i, &p) in map.points.iter().enumerate() {
            map.llrs(p, 0.1, &mut llr);
            let bits: usize = llr.iter().fold(0, |acc, &l| (acc << 1) | (l < 0.0) as usize);
            assert_eq!(bits, i);
        }
    }
}
