//! Gardner zamanlama hatası dedektörü ile sembol zamanlaması kurtarma.
//!
//! Eşleştirilmiş filtreden geçmiş örneklerden, kesirli enterpolasyonla her
//! sembolün tam ortasındaki değeri üretir. Gardner dedektörü sembol başına iki
//! noktaya (sembol anı ve iki sembol arasının ortası) ihtiyaç duyar; bunları
//! enterpolatör verdiği için sembol başına örnek sayısı tam sayı olmak zorunda
//! değildir (ör. 2.4 MS/s / 1.5 MS/s = 1.6).

use num_complex::Complex;

use super::filters::Interpolator;

pub struct TimingResult {
    pub symbols: Vec<Complex<f32>>,
    /// Döngünün sonundaki sembol periyodu tahmini (örnek cinsinden).
    pub final_period: f64,
}

/// İkinci dereceden döngü filtresi kazançları (oransal, integral).
/// `bn` sembol hızına göre normalize döngü bant genişliği.
fn loop_gains(bn: f64, damping: f64) -> (f64, f64) {
    let theta = bn / (damping + 1.0 / (4.0 * damping));
    let d = 1.0 + 2.0 * damping * theta + theta * theta;
    (4.0 * damping * theta / d, 4.0 * theta * theta / d)
}

/// `x`: birim güce normalize, eşleştirilmiş filtreden geçmiş örnekler.
/// `sps`: nominal sembol başına örnek. `bn`: döngü bant genişliği (ör. 0.005).
pub fn recover(x: &[Complex<f32>], sps: f64, bn: f64) -> TimingResult {
    let ip = Interpolator::new();
    let (kp, ki) = loop_gains(bn, 0.707);
    let mut t = Interpolator::TAPS as f64 + sps;
    let mut integ = 0.0;
    let mut prev: Option<Complex<f32>> = None;
    let mut symbols = Vec::with_capacity((x.len() as f64 / sps) as usize + 1);

    while let (Some(on), Some(mid)) = (ip.at(x, t), ip.at(x, t - (sps + integ) / 2.0)) {
        let mut step = sps + integ;
        if let Some(p) = prev {
            // Gardner: e > 0 ise örnekleme erken, bir sonraki anı ileri al.
            let e = ((p - on) * mid.conj()).re as f64;
            let e = e.clamp(-1.0, 1.0);
            integ += sps * ki * e;
            step += sps * kp * e;
        }
        prev = Some(on);
        symbols.push(on);
        t += step;
    }
    TimingResult { symbols, final_period: sps + integ }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::filters::{filter, normalize, rrc_pulse, rrc_taps};
    use super::*;

    /// Basit doğrusal eşlenik üreteci; testlerin tekrarlanabilir olması için.
    pub(crate) struct Lcg(pub u64);
    impl Lcg {
        pub(crate) fn next_bit(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 62) as u8 & 1
        }
    }

    /// Verilen sembolleri RRC darbesiyle, `sps` (kesirli) örnek hızında ve
    /// `delay` sembol gecikmeyle örnekler.
    pub(crate) fn shape(symbols: &[Complex<f32>], sps: f64, rolloff: f64, delay: f64) -> Vec<Complex<f32>> {
        let n = ((symbols.len() as f64 + 16.0) * sps) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / sps - 8.0 - delay;
                let k0 = (t - 8.0).ceil().max(0.0) as usize;
                let k1 = ((t + 8.0).floor() as i64).min(symbols.len() as i64 - 1);
                let mut acc = Complex::default();
                if k1 >= 0 {
                    for k in k0..=k1 as usize {
                        acc += symbols[k] * rrc_pulse(t - k as f64, rolloff) as f32;
                    }
                }
                acc
            })
            .collect()
    }

    #[test]
    fn recovers_qpsk_at_fractional_sps() {
        let mut rng = Lcg(7);
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let tx: Vec<Complex<f32>> = (0..6000)
            .map(|_| {
                let i = if rng.next_bit() == 1 { s } else { -s };
                let q = if rng.next_bit() == 1 { s } else { -s };
                Complex::new(i, q)
            })
            .collect();
        let sps = 1.6;
        let mut x = filter(&shape(&tx, sps, 0.2, 0.37), &rrc_taps(sps, 0.2, 8));
        normalize(&mut x);
        let r = recover(&x, sps, 0.005);
        // Döngü oturduktan sonraki sembollerde QPSK noktalarına uzaklık küçük olmalı.
        let tail = &r.symbols[r.symbols.len() / 2..r.symbols.len() - 20];
        let rms = (tail.iter().map(|z| z.norm_sqr()).sum::<f32>() / tail.len() as f32).sqrt();
        let evm = (tail
            .iter()
            .map(|z| {
                let z = z / rms;
                let ideal = Complex::new(s * z.re.signum(), s * z.im.signum());
                (z - ideal).norm_sqr()
            })
            .sum::<f32>()
            / tail.len() as f32)
            .sqrt();
        assert!(evm < 0.1, "EVM {evm}");
        assert!((r.final_period - sps).abs() < 0.01, "periyot {}", r.final_period);
    }
}
