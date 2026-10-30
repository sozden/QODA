//! Symbol timing recovery with a Gardner timing error detector.
//!
//! From matched-filtered samples, produces the value at the exact centre of each
//! symbol by fractional interpolation. The Gardner detector needs two points per
//! symbol (the symbol instant and the midpoint between two symbols); since the
//! interpolator provides them, the number of samples per symbol need not be an
//! integer (e.g. 2.4 MS/s / 1.5 MS/s = 1.6).

use num_complex::Complex;

use super::filters::Interpolator;

pub struct TimingResult {
    pub symbols: Vec<Complex<f32>>,
    /// Symbol period estimate at the end of the loop (in samples).
    pub final_period: f64,
}

/// Second-order loop filter gains (proportional, integral).
/// `bn` is the loop bandwidth normalized to the symbol rate.
fn loop_gains(bn: f64, damping: f64) -> (f64, f64) {
    let theta = bn / (damping + 1.0 / (4.0 * damping));
    let d = 1.0 + 2.0 * damping * theta + theta * theta;
    (4.0 * damping * theta / d, 4.0 * theta * theta / d)
}

/// Timing recovery working on samples that arrive in chunks.
pub struct TimingRecovery {
    ip: Interpolator,
    sps: f64,
    kp: f64,
    ki: f64,
    /// Samples waiting to be processed; `t` is a fractional position in this buffer.
    buf: Vec<Complex<f32>>,
    t: f64,
    integ: f64,
    prev: Option<Complex<f32>>,
}

impl TimingRecovery {
    /// `sps`: nominal samples per symbol. `bn`: loop bandwidth (e.g. 0.005).
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

    /// Current symbol period estimate (in samples).
    pub fn period(&self) -> f64 {
        self.sps + self.integ
    }

    /// `x`: matched-filtered samples near unit power.
    /// Produced symbols are appended to `out`.
    pub fn process(&mut self, x: &[Complex<f32>], out: &mut Vec<Complex<f32>>) {
        self.buf.extend_from_slice(x);
        let buf = &self.buf;
        while let (Some(on), Some(mid)) = (self.ip.at(buf, self.t), self.ip.at(buf, self.t - self.period() / 2.0)) {
            let mut step = self.period();
            if let Some(p) = self.prev {
                // Gardner: if e > 0 sampling is early; move the next instant forward.
                let e = ((p - on) * mid.conj()).re as f64;
                let e = e.clamp(-1.0, 1.0);
                self.integ += self.sps * self.ki * e;
                step += self.sps * self.kp * e;
            }
            self.prev = Some(on);
            out.push(on);
            self.t += step;
        }
        // Keep as much history as the interpolator needs and drop the rest.
        let keep_from = (self.t - self.sps - Interpolator::TAPS as f64).floor().max(0.0) as usize;
        let keep_from = keep_from.min(self.buf.len());
        self.buf.drain(..keep_from);
        self.t -= keep_from as f64;
    }
}

/// `x`: matched-filtered samples normalized to unit power.
/// `sps`: nominal samples per symbol. `bn`: loop bandwidth (e.g. 0.005).
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

    /// Simple linear congruential generator, so tests are repeatable.
    pub(crate) struct Lcg(pub u64);
    impl Lcg {
        pub(crate) fn next_bit(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 62) as u8 & 1
        }
    }

    /// Samples the given symbols with an RRC pulse at a (fractional) rate of `sps`
    /// samples per symbol with a delay of `delay` symbols.
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
        // Once the loop settles, the distance to the QPSK points must be small.
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

        // Processing in chunks must produce the same symbols.
        let mut tr = TimingRecovery::new(sps, 0.005);
        let mut chunked = Vec::new();
        for part in x.chunks(777) {
            tr.process(part, &mut chunked);
        }
        assert_eq!(chunked.len(), r.symbols.len());
        assert!(chunked.iter().zip(&r.symbols).all(|(a, b)| (a - b).norm() < 1e-5));
    }
}
