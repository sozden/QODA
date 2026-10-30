//! Alıcı ön ucu filtreleri: frekans kaydırma, RRC eşleştirilmiş filtre ve
//! kesirli gecikmeli enterpolasyon.

use std::f64::consts::PI;

use num_complex::Complex;

/// Örnekleri `-offset_hz` kadar kaydırır (yayın merkezini 0 Hz'e indirir).
pub fn mix_down(samples: &mut [Complex<f32>], offset_hz: f64, sample_rate: f64) {
    let step = -2.0 * PI * offset_hz / sample_rate;
    for (n, s) in samples.iter_mut().enumerate() {
        // Uzun kayıtlarda faz biriktirmek yerine her örnekte yeniden hesapla.
        let phase = (step * n as f64).rem_euclid(2.0 * PI);
        *s *= Complex::from_polar(1.0, phase as f32);
    }
}

/// Kök yükseltilmiş kosinüs darbesinin `t` (sembol cinsinden) anındaki değeri.
pub fn rrc_pulse(t: f64, rolloff: f64) -> f64 {
    let a = rolloff;
    if t.abs() < 1e-9 {
        1.0 - a + 4.0 * a / PI
    } else if (t.abs() - 1.0 / (4.0 * a)).abs() < 1e-9 {
        a / 2f64.sqrt() * ((1.0 + 2.0 / PI) * (PI / (4.0 * a)).sin() + (1.0 - 2.0 / PI) * (PI / (4.0 * a)).cos())
    } else {
        let num = (PI * t * (1.0 - a)).sin() + 4.0 * a * t * (PI * t * (1.0 + a)).cos();
        let den = PI * t * (1.0 - (4.0 * a * t).powi(2));
        num / den
    }
}

/// RRC filtresi katsayıları, birim enerjiye normalize.
/// `sps` sembol başına örnek (kesirli olabilir), `span` iki yandaki sembol sayısı.
pub fn rrc_taps(sps: f64, rolloff: f64, span: usize) -> Vec<f32> {
    let half = (span as f64 * sps).ceil() as i64;
    let taps: Vec<f64> = (-half..=half).map(|i| rrc_pulse(i as f64 / sps, rolloff)).collect();
    let energy = taps.iter().map(|x| x * x).sum::<f64>().sqrt();
    taps.iter().map(|x| (x / energy) as f32).collect()
}

/// Gerçek katsayılı FIR filtreyi uygular; çıkış girişle aynı uzunlukta ve
/// filtre gecikmesi kadar sola kaydırılmıştır (sıfır fazlı).
pub fn filter(samples: &[Complex<f32>], taps: &[f32]) -> Vec<Complex<f32>> {
    let half = taps.len() / 2;
    (0..samples.len())
        .map(|n| {
            let mut acc = Complex::default();
            for (k, &h) in taps.iter().enumerate() {
                let idx = n as i64 + half as i64 - k as i64;
                if idx >= 0 && (idx as usize) < samples.len() {
                    acc += samples[idx as usize] * h;
                }
            }
            acc
        })
        .collect()
}

/// Parça parça gelen örneklere gerçek katsayılı FIR uygulayıp her `decimation`.
/// çıkışı tutan akış süzgeci. Çıkış yalnızca tutulacak örnekler için
/// hesaplanır; seyreltme maliyeti katsayı sayısı × çıkış hızıdır.
pub struct StreamingFir {
    taps: Vec<f32>,
    decimation: usize,
    history: Vec<Complex<f32>>,
    /// Bir sonraki çıkış penceresinin `history` içindeki başlangıcı.
    phase: usize,
}

impl StreamingFir {
    pub fn new(taps: Vec<f32>, decimation: usize) -> Self {
        let history = vec![Complex::default(); taps.len() - 1];
        Self { taps, decimation: decimation.max(1), history, phase: 0 }
    }

    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<Complex<f32>>) {
        let mut buf = std::mem::take(&mut self.history);
        buf.extend_from_slice(input);
        let l = self.taps.len();
        let mut i = self.phase;
        while i + l <= buf.len() {
            out.push(dot(&buf[i..i + l], &self.taps));
            i += self.decimation;
        }
        let keep = buf.len() + 1 - l;
        self.phase = i - keep;
        self.history = buf[keep..].to_vec();
    }
}

/// Karmaşık örneklerle gerçek katsayıların iç çarpımı; dört ayrı toplayıcı
/// işlemcinin aynı anda birden çok çarpma yapabilmesini sağlar.
fn dot(x: &[Complex<f32>], h: &[f32]) -> Complex<f32> {
    let mut acc = [Complex::<f32>::default(); 4];
    let (xc, hc) = (x.chunks_exact(4), h.chunks_exact(4));
    let (xr, hr) = (xc.remainder(), hc.remainder());
    for (xs, hs) in xc.zip(hc) {
        for k in 0..4 {
            acc[k] += xs[k] * hs[k];
        }
    }
    let mut sum = acc[0] + acc[1] + acc[2] + acc[3];
    for (x, &h) in xr.iter().zip(hr) {
        sum += x * h;
    }
    sum
}

/// `decimation` kat seyreltme öncesi alçak geçiren süzgeç: kesim yeni Nyquist
/// frekansında, Blackman pencereli sinc, DC kazancı 1. Kesim ile yeni örnekleme
/// hızı arasına katlanan bileşenler yayın bandının dışında kalır ve sonraki RRC
/// süzgeci onları da bastırır.
pub fn decimation_taps(decimation: usize) -> Vec<f32> {
    let d = decimation.max(1) as f64;
    let len = 12 * decimation.max(1) + 1;
    let mid = (len / 2) as f64;
    let cutoff = 0.5 / d; // örnekleme hızına göre
    let taps: Vec<f64> = (0..len)
        .map(|i| {
            let n = i as f64 - mid;
            let sinc = if n == 0.0 { 2.0 * cutoff } else { (2.0 * PI * cutoff * n).sin() / (PI * n) };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (len - 1) as f64).cos()
                + 0.08 * (4.0 * PI * i as f64 / (len - 1) as f64).cos();
            sinc * w
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter().map(|t| (t / sum) as f32).collect()
}

/// Örnekleri birim RMS güce ölçekler.
pub fn normalize(samples: &mut [Complex<f32>]) {
    let power = samples.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / samples.len().max(1) as f64;
    if power > 0.0 {
        let k = (1.0 / power.sqrt()) as f32;
        samples.iter_mut().for_each(|s| *s *= k);
    }
}

/// Pencereli sinc ile kesirli konumda örnek üreten enterpolatör.
pub struct Interpolator {
    /// `phases` adet faz için `TAPS` katsayılı tablo.
    table: Vec<[f32; Self::TAPS]>,
}

impl Interpolator {
    pub const TAPS: usize = 16;
    const PHASES: usize = 256;

    pub fn new() -> Self {
        let half = (Self::TAPS / 2) as f64;
        let table = (0..Self::PHASES)
            .map(|p| {
                let frac = p as f64 / Self::PHASES as f64;
                let mut row = [0f32; Self::TAPS];
                let mut sum = 0.0;
                for (k, r) in row.iter_mut().enumerate() {
                    // Katsayı k, (taban - TAPS/2 + 1 + k) indisli örneği çarpar.
                    let x = k as f64 - (half - 1.0) - frac;
                    let sinc = if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) };
                    let w = 0.42 + 0.5 * (PI * x / half).cos() + 0.08 * (2.0 * PI * x / half).cos();
                    let v = sinc * if x.abs() < half { w } else { 0.0 };
                    *r = v as f32;
                    sum += v;
                }
                row.iter_mut().for_each(|r| *r /= sum as f32);
                row
            })
            .collect();
        Self { table }
    }

    /// `t` kesirli örnek konumundaki değer. Kenarlara yakın konumlarda `None`.
    pub fn at(&self, x: &[Complex<f32>], t: f64) -> Option<Complex<f32>> {
        let base = t.floor();
        let first = base as i64 - (Self::TAPS as i64 / 2 - 1);
        if first < 0 || first as usize + Self::TAPS > x.len() {
            return None;
        }
        let phase = (((t - base) * Self::PHASES as f64).round() as usize).min(Self::PHASES - 1);
        let row = &self.table[phase];
        let window = &x[first as usize..first as usize + Self::TAPS];
        Some(window.iter().zip(row).map(|(s, &h)| s * h).sum())
    }
}

impl Default for Interpolator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrc_is_symmetric_with_unit_energy() {
        let taps = rrc_taps(1.6, 0.2, 8);
        let n = taps.len();
        for i in 0..n / 2 {
            assert!((taps[i] - taps[n - 1 - i]).abs() < 1e-6);
        }
        let e: f32 = taps.iter().map(|x| x * x).sum();
        assert!((e - 1.0).abs() < 1e-4);
    }

    #[test]
    fn streaming_fir_matches_single_call() {
        let x: Vec<Complex<f32>> = (0..1000).map(|n| Complex::new((n as f32 * 0.37).sin(), (n as f32 * 0.11).cos())).collect();
        let taps = decimation_taps(3);
        let mut whole = Vec::new();
        StreamingFir::new(taps.clone(), 3).process(&x, &mut whole);
        let mut parts = Vec::new();
        let mut fir = StreamingFir::new(taps, 3);
        for chunk in x.chunks(77) {
            fir.process(chunk, &mut parts);
        }
        assert_eq!(whole.len(), parts.len());
        assert!(whole.iter().zip(&parts).all(|(a, b)| (a - b).norm() < 1e-5));
    }

    #[test]
    fn decimation_filter_passes_low_and_blocks_high_tones() {
        let d = 8;
        let tone = |f: f64| -> f32 {
            let x: Vec<Complex<f32>> = (0..20_000).map(|n| Complex::from_polar(1.0, (2.0 * PI * f * n as f64) as f32)).collect();
            let mut out = Vec::new();
            StreamingFir::new(decimation_taps(d), d).process(&x, &mut out);
            let tail = &out[out.len() / 2..];
            (tail.iter().map(|z| z.norm_sqr()).sum::<f32>() / tail.len() as f32).sqrt()
        };
        // Yeni Nyquist frekansı 1/16; yayın bandı bunun yarısına kadar, katlanan bölge 3/16'dan sonra.
        assert!(tone(0.02) > 0.95, "geçirme bandı");
        assert!(tone(0.25) < 0.01, "durdurma bandı");
    }

    #[test]
    fn interpolator_reproduces_slow_tone() {
        let w = 0.3;
        let x: Vec<Complex<f32>> = (0..200).map(|n| Complex::from_polar(1.0, (w * n as f64) as f32)).collect();
        let ip = Interpolator::new();
        let t = 100.37;
        let got = ip.at(&x, t).unwrap();
        let want = Complex::from_polar(1.0, (w * t) as f32);
        assert!((got - want).norm() < 0.01, "{got} vs {want}");
    }
}
