//! Spektrum hesabı.

use std::f32::consts::PI;
use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Hann pencereli, blok ortalamalı güç spektrumu.
pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    acc: Vec<f32>,
}

impl Spectrum {
    pub fn new(size: usize) -> Self {
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / size as f32).cos())
            .collect();
        Self {
            fft: FftPlanner::new().plan_fft_forward(size),
            window,
            buf: vec![Complex::default(); size],
            acc: vec![0.0; size],
        }
    }

    pub fn size(&self) -> usize {
        self.window.len()
    }

    /// Örnekleri FFT boyutunda bloklara bölüp ortalama gücü dB olarak döndürür.
    /// Çıkış ortalanmıştır: ilk eleman -fs/2, son eleman +fs/2 civarıdır.
    pub fn compute(&mut self, samples: &[Complex<f32>]) -> Vec<f32> {
        let n = self.size();
        self.acc.fill(0.0);
        let mut blocks = 0;
        for chunk in samples.chunks_exact(n) {
            for ((b, s), w) in self.buf.iter_mut().zip(chunk).zip(&self.window) {
                *b = s * w;
            }
            self.fft.process(&mut self.buf);
            for (a, x) in self.acc.iter_mut().zip(&self.buf) {
                *a += x.norm_sqr();
            }
            blocks += 1;
        }
        let scale = 1.0 / (blocks.max(1) as f32 * n as f32 * n as f32);
        let mut db: Vec<f32> = self
            .acc
            .iter()
            .map(|p| 10.0 * (p * scale + 1e-20).log10())
            .collect();
        db.rotate_left(n / 2);
        db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_lands_in_expected_bin() {
        let n = 256;
        // fs/4 frekansında karmaşık ton: ortalanmış spektrumda 3n/4 kutusuna düşer.
        let samples: Vec<Complex<f32>> = (0..n * 4)
            .map(|i| Complex::from_polar(1.0, PI / 2.0 * i as f32))
            .collect();
        let db = Spectrum::new(n).compute(&samples);
        let peak = (0..n).max_by(|&a, &b| db[a].total_cmp(&db[b])).unwrap();
        assert_eq!(peak, 3 * n / 4);
    }
}
