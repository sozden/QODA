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

/// Parça parça gelen örneklerle çalışan zamanlama kurtarıcı.
pub struct TimingRecovery {
    ip: Interpolator,
    sps: f64,
    kp: f64,
    ki: f64,
    /// İşlenmeyi bekleyen örnekler; `t` bu tampona göre kesirli konumdur.
    buf: Vec<Complex<f32>>,
    t: f64,
    integ: f64,
    prev: Option<Complex<f32>>,
}

impl TimingRecovery {
    /// `sps`: nominal sembol başına örnek. `bn`: döngü bant genişliği (ör. 0.005).
    pub fn new(sps: f64, bn: f64) -> Self {
        let (kp, ki) = loop_gains(bn, 0.707);
        Self {
            ip: Interpolator::new(),
            sps,
            kp,
            ki,
            buf: Vec::new(),
            t: Interpolator::TAPS as f64 + sps,
            integ: 0.0,
            prev: None,
        }
    }

    /// Şu anki sembol periyodu tahmini (örnek cinsinden).
    pub fn period(&self) -> f64 {
        self.sps + self.integ
    }

    /// `x`: birim güce yakın, eşleştirilmiş filtreden geçmiş örnekler.
    /// Üretilen semboller `out`a eklenir.
    pub fn process(&mut self, x: &[Complex<f32>], out: &mut Vec<Complex<f32>>) {
        self.buf.extend_from_slice(x);
        let buf = &self.buf;
        while let (Some(on), Some(mid)) = (self.ip.at(buf, self.t), self.ip.at(buf, self.t - self.period() / 2.0)) {
            let mut step = self.period();
            if let Some(p) = self.prev {
                // Gardner: e > 0 ise örnekleme erken, bir sonraki anı ileri al.
                let e = ((p - on) * mid.conj()).re as f64;
                let e = e.clamp(-1.0, 1.0);
                self.integ += self.sps * self.ki * e;
                step += self.sps * self.kp * e;
            }
            self.prev = Some(on);
            out.push(on);
            self.t += step;
        }
        // Enterpolatörün ihtiyaç duyacağı kadar geçmişi tutup gerisini at.
        let keep_from = (self.t - self.sps - Interpolator::TAPS as f64).floor().max(0.0) as usize;
        let keep_from = keep_from.min(self.buf.len());
        self.buf.drain(..keep_from);
        self.t -= keep_from as f64;
    }
}

/// `x`: birim güce normalize, eşleştirilmiş filtreden geçmiş örnekler.
/// `sps`: nominal sembol başına örnek. `bn`: döngü bant genişliği (ör. 0.005).
pub fn recover(x: &[Complex<f32>], sps: f64, bn: f64) -> TimingResult {
    let mut tr = TimingRecovery::new(sps, bn);
    let mut symbols = Vec::with_capacity((x.len() as f64 / sps) as usize + 1);
    tr.process(x, &mut symbols);
    TimingResult { symbols, final_period: tr.period() }
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

        // Parça parça işleme aynı sembolleri üretmeli.
        let mut tr = TimingRecovery::new(sps, 0.005);
        let mut chunked = Vec::new();
        for part in x.chunks(777) {
            tr.process(part, &mut chunked);
        }
        assert_eq!(chunked.len(), r.symbols.len());
        assert!(chunked.iter().zip(&r.symbols).all(|(a, b)| (a - b).norm() < 1e-5));
    }
}
