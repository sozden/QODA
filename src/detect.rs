//! Spektrumdan yayın parametrelerini tahmin etme.
//!
//! DVB-S2 yayınları kök yükseltilmiş kosinüs (RRC) ile şekillendirilir. Böyle bir
//! işaretin güç spektrumu yükseltilmiş kosinüs biçimindedir ve ±Rs/2'de tepe
//! değerinin yarısına (-3 dB) düşer. Bu yüzden -3 dB bant genişliği roll-off'tan
//! bağımsız olarak doğrudan sembol hızını (Rs) verir. Tabana kadar olan toplam
//! genişlik ise (1 + roll-off) × Rs'dir.

/// QO-100 DATV yayınlarında yaygın sembol hızları (S/s).
const COMMON_SYMBOL_RATES: [f64; 9] = [
    33_000.0, 66_000.0, 125_000.0, 250_000.0, 333_000.0, 500_000.0, 1_000_000.0, 1_500_000.0, 2_000_000.0,
];
/// Ölçülen hızın yaygın bir değere yuvarlanması için izin verilen bağıl fark.
const SNAP_TOLERANCE: f64 = 0.06;
/// Yayın sayılması için tabanın üzerinde gereken en az seviye (dB).
const MIN_SNR_DB: f32 = 6.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// Yayın merkezinin spektrum merkezine göre ofseti (Hz).
    pub center_offset_hz: f64,
    /// -3 dB genişliğinden ölçülen sembol hızı (S/s).
    pub measured_symbol_rate: f64,
    /// Ölçülen hıza yeterince yakınsa yaygın sembol hızı.
    pub common_symbol_rate: Option<f64>,
    /// Tabana kadar olan genişlikten kaba roll-off tahmini.
    pub rolloff: f32,
    /// Düz tepe seviyesinin gürültü tabanı üzerindeki farkı (dB).
    pub snr_db: f32,
    /// Yayın spektrumun kenarına değiyor; ölçüm eksik olabilir.
    pub truncated: bool,
}

impl Detection {
    /// Yaygın değere yuvarlanmışsa onu, değilse ölçülen değeri döndürür.
    pub fn symbol_rate(&self) -> f64 {
        self.common_symbol_rate.unwrap_or(self.measured_symbol_rate)
    }
}

/// Ortalanmış dB spektrumunda `guess` kutusunu içeren yayını arar.
pub fn detect(db: &[f32], sample_rate: f64, guess: usize) -> Option<Detection> {
    let n = db.len();
    if n < 16 || guess >= n {
        return None;
    }
    let s = moving_average(db, (n / 256).max(2));
    let bin_hz = sample_rate / n as f64;

    let mut sorted = s.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[n / 10];
    if s[guess] < floor + MIN_SNR_DB {
        return None;
    }

    // Tabanın belirgin üzerindeki bölgeyi bul, düz tepe seviyesini ortancadan al.
    let threshold = floor + MIN_SNR_DB;
    let (l, r) = walk(&s, guess, |v| v > threshold);
    let mut core = s[l..=r].to_vec();
    core.sort_by(f32::total_cmp);
    let plateau = core[core.len() * 3 / 4];

    let half = plateau - 3.0;
    let (left, right) = edges(&s, guess, half);
    let measured = (right - left) * bin_hz;
    let near_floor = floor + 3.0;
    let (fl, fr) = edges(&s, guess, near_floor);
    let rolloff = (((fr - fl) / (right - left)) as f32 - 1.0).clamp(0.0, 0.5);

    let common = COMMON_SYMBOL_RATES
        .iter()
        .copied()
        .find(|r| ((measured - r) / r).abs() <= SNAP_TOLERANCE);

    Some(Detection {
        center_offset_hz: ((left + right) / 2.0 - n as f64 / 2.0) * bin_hz,
        measured_symbol_rate: measured,
        common_symbol_rate: common,
        rolloff,
        snr_db: plateau - floor,
        truncated: fl <= 0.5 || fr >= n as f64 - 1.5,
    })
}

/// Kutuların merkez konumuna göre (kesirli) sol ve sağ kesişim noktaları.
fn edges(s: &[f32], guess: usize, level: f32) -> (f64, f64) {
    let (l, r) = walk(s, guess, |v| v > level);
    let cross = |inside: usize, outside: Option<usize>| match outside {
        Some(o) => {
            let (a, b) = (s[inside], s[o]);
            let t = ((a - level) / (a - b)).clamp(0.0, 1.0) as f64;
            inside as f64 + t * (o as f64 - inside as f64)
        }
        None => inside as f64,
    };
    let left = cross(l, l.checked_sub(1));
    let right = cross(r, (r + 1 < s.len()).then_some(r + 1));
    // Kutu merkezleri arasındaki mesafe genişliği verir; uçlara yarım kutu ekle.
    (left + 0.5, right + 0.5)
}

/// `start`tan iki yöne `inside` sağlandıkça yürür, son sağlanan kutuları döndürür.
fn walk(s: &[f32], start: usize, inside: impl Fn(f32) -> bool) -> (usize, usize) {
    let mut l = start;
    while l > 0 && inside(s[l - 1]) {
        l -= 1;
    }
    let mut r = start;
    while r + 1 < s.len() && inside(s[r + 1]) {
        r += 1;
    }
    (l, r)
}

fn moving_average(v: &[f32], half_width: usize) -> Vec<f32> {
    (0..v.len())
        .map(|i| {
            let a = i.saturating_sub(half_width);
            let b = (i + half_width + 1).min(v.len());
            v[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yükseltilmiş kosinüs güç spektrumu + gürültü tabanı, dB olarak.
    fn rc_spectrum(n: usize, fs: f64, center: f64, rs: f64, alpha: f64, snr_db: f64) -> Vec<f32> {
        let peak = 10f64.powf(snr_db / 10.0);
        (0..n)
            .map(|i| {
                let f = ((i as f64 + 0.5) / n as f64 - 0.5) * fs - center;
                let a = f.abs();
                let lo = (1.0 - alpha) * rs / 2.0;
                let hi = (1.0 + alpha) * rs / 2.0;
                let h = if a <= lo {
                    1.0
                } else if a <= hi {
                    0.5 * (1.0 + (std::f64::consts::PI / (alpha * rs) * (a - lo)).cos())
                } else {
                    0.0
                };
                (10.0 * (1.0 + peak * h).log10()) as f32
            })
            .collect()
    }

    #[test]
    fn measures_symbol_rate_and_center() {
        let (n, fs) = (2048, 2.048e6);
        let db = rc_spectrum(n, fs, 100e3, 1.0e6, 0.35, 20.0);
        let d = detect(&db, fs, n / 2 + 100).expect("yayın bulunmalı");
        assert!((d.measured_symbol_rate - 1.0e6).abs() < 20e3, "{d:?}");
        assert!((d.center_offset_hz - 100e3).abs() < 5e3, "{d:?}");
        assert_eq!(d.common_symbol_rate, Some(1.0e6));
        assert!(!d.truncated);
    }

    #[test]
    fn narrow_signal_snaps_to_common_rate() {
        let (n, fs) = (2048, 2.048e6);
        let db = rc_spectrum(n, fs, -400e3, 333e3, 0.25, 15.0);
        let guess = n / 2 - 400;
        let d = detect(&db, fs, guess).expect("yayın bulunmalı");
        assert_eq!(d.common_symbol_rate, Some(333e3), "{d:?}");
    }

    #[test]
    fn noise_only_finds_nothing() {
        let db = vec![-60.0; 1024];
        assert_eq!(detect(&db, 2.048e6, 512), None);
    }
}
