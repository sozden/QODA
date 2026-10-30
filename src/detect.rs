//! Estimating signal parameters from the spectrum.
//!
//! DVB-S2 signals are shaped with a root raised cosine (RRC). The power spectrum
//! of such a signal has a raised cosine shape and falls to half of its peak
//! (-3 dB) at ±Rs/2. So the -3 dB bandwidth gives the symbol rate (Rs) directly,
//! independent of the roll-off. The total width down to the floor is
//! (1 + roll-off) × Rs.

/// Common symbol rates of QO-100 DATV signals (S/s).
const COMMON_SYMBOL_RATES: [f64; 9] = [
    33_000.0, 66_000.0, 125_000.0, 250_000.0, 333_000.0, 500_000.0, 1_000_000.0, 1_500_000.0, 2_000_000.0,
];
/// Relative difference allowed for rounding a measured rate to a common value.
const SNAP_TOLERANCE: f64 = 0.06;
/// Minimum level above the floor for something to count as a signal (dB).
const MIN_SNR_DB: f32 = 6.0;
/// The spectrum is smoothed over this width (Hz).
const SMOOTH_HZ: f64 = 10e3;
/// Regions wider than this are not signals (e.g. the transponder's noise hump).
const MAX_SYMBOL_RATE: f64 = 5e6;
/// Peaks narrower than this are not signals (e.g. the DC spike at the receiver's centre).
const MIN_SYMBOL_RATE: f64 = 20e3;
/// While searching for the floor, a valley is reached if the spectrum does not fall over this width (Hz).
const VALLEY_PATIENCE_HZ: f64 = 30e3;
/// Maximum number of signals searched for in a scan.
const MAX_SIGNALS: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// Offset of the signal's centre from the spectrum centre (Hz).
    pub center_offset_hz: f64,
    /// Symbol rate measured from the -3 dB width (S/s).
    pub measured_symbol_rate: f64,
    /// Common symbol rate, if close enough to the measured one.
    pub common_symbol_rate: Option<f64>,
    /// Rough roll-off estimate from the width down to the floor.
    pub rolloff: f32,
    /// Height of the flat top above the noise floor (dB).
    pub snr_db: f32,
    /// The signal touches the edge of the spectrum; the measurement may be incomplete.
    pub truncated: bool,
}

impl Detection {
    /// Returns the rounded common value if there is one, otherwise the measured value.
    pub fn symbol_rate(&self) -> f64 {
        self.common_symbol_rate.unwrap_or(self.measured_symbol_rate)
    }
}

/// Searches a centred dB spectrum for the signal containing bin `guess`.
///
/// The measurement is local: the signal's flat top and −3 dB edges are found, and
/// the floor is taken from the valleys where the spectrum stops falling when walking
/// outwards from the edges (the higher of the two sides). This way, in wideband, the
/// difference between the transponder's noise hump and out-of-band does not skew the measurement.
pub fn detect(db: &[f32], sample_rate: f64, guess: usize) -> Option<Detection> {
    let n = db.len();
    if n < 16 || guess >= n {
        return None;
    }
    let bin_hz = sample_rate / n as f64;
    let s = smooth(db, bin_hz);
    detect_smoothed(&s, bin_hz, guess).map(|(d, _)| d)
}

/// Finds every signal in the spectrum: starting from the strongest point, marks the
/// band of each signal found and keeps searching in the remaining region. Results
/// are sorted by frequency.
pub fn scan(db: &[f32], sample_rate: f64) -> Vec<Detection> {
    let n = db.len();
    if n < 16 {
        return Vec::new();
    }
    let bin_hz = sample_rate / n as f64;
    let s = smooth(db, bin_hz);
    let mut sorted = s.clone();
    sorted.sort_by(f32::total_cmp);
    // Points not even 6 dB above the overall floor are not candidates.
    let min_level = sorted[n / 10] + MIN_SNR_DB;
    let patience = valley_patience(bin_hz);

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| s[b].total_cmp(&s[a]));
    let mut covered = vec![false; n];
    let mut found = Vec::new();
    for i in order {
        if s[i] < min_level || found.len() >= MAX_SIGNALS {
            break;
        }
        if covered[i] {
            continue;
        }
        let span = match detect_smoothed(&s, bin_hz, i) {
            Some((d, span)) => {
                // Do not count a signal found again from its skirt a second time.
                let duplicate = found.iter().any(|f: &Detection| {
                    (f.center_offset_hz - d.center_offset_hz).abs() < f.measured_symbol_rate / 2.0
                });
                if !duplicate {
                    found.push(d);
                }
                span
            }
            None => (i.saturating_sub(patience), (i + patience).min(n - 1)),
        };
        covered[span.0..=span.1].fill(true);
    }
    found.sort_by(|a, b| a.center_offset_hz.total_cmp(&b.center_offset_hz));
    found
}

fn smooth(db: &[f32], bin_hz: f64) -> Vec<f32> {
    moving_average(db, ((SMOOTH_HZ / bin_hz).round() as usize).max(2))
}

fn valley_patience(bin_hz: f64) -> usize {
    ((VALLEY_PATIENCE_HZ / bin_hz).ceil() as usize).max(3)
}

/// Detection on the smoothed spectrum, together with the bins the signal occupies down to the floor.
fn detect_smoothed(s: &[f32], bin_hz: f64, guess: usize) -> Option<(Detection, (usize, usize))> {
    let n = s.len();
    // Peak level: first the clicked point, then the upper quarter between the −3 dB edges.
    let mut plateau = s[guess];
    let (mut left, mut right) = (0.0, 0.0);
    for _ in 0..3 {
        (left, right) = edges(s, guess, plateau - 3.0);
        let (a, b) = (left as usize, (right.ceil() as usize).clamp(left as usize + 1, n));
        let mut core = s[a..b].to_vec();
        core.sort_by(f32::total_cmp);
        plateau = core[core.len() * 3 / 4];
    }
    let measured = (right - left) * bin_hz;
    if !(MIN_SYMBOL_RATE..=MAX_SYMBOL_RATE).contains(&measured) || right - left < 3.0 {
        return None;
    }

    let patience = valley_patience(bin_hz);
    let (floor_left, end_left, valley_left) = valley(s, (left as usize).min(n - 1), -1, patience);
    let (floor_right, end_right, valley_right) = valley(s, (right as usize).min(n - 1), 1, patience);
    let floor = floor_left.max(floor_right);
    let snr = plateau - floor;
    if snr < MIN_SNR_DB {
        return None;
    }
    let (fl, fr) = edges(s, guess, floor + 3.0);
    let rolloff = (((fr - fl) / (right - left)) as f32 - 1.0).clamp(0.0, 0.5);
    let common = COMMON_SYMBOL_RATES
        .iter()
        .copied()
        .find(|r| ((measured - r) / r).abs() <= SNAP_TOLERANCE);

    let detection = Detection {
        center_offset_hz: ((left + right) / 2.0 - n as f64 / 2.0) * bin_hz,
        measured_symbol_rate: measured,
        common_symbol_rate: common,
        rolloff,
        snr_db: snr,
        truncated: end_left || end_right || fl <= 0.5 || fr >= n as f64 - 1.5,
    };
    // The signal's area extends to the valleys so the scan does not restart from the skirts.
    Some((detection, (valley_left, valley_right)))
}

/// Walks from `start` in direction `dir` while the spectrum falls; stops when no new
/// minimum is found for `patience` bins. (minimum level, reached the end of the array, position of the minimum)
fn valley(s: &[f32], start: usize, dir: isize, patience: usize) -> (f32, bool, usize) {
    let mut i = start as isize;
    let (mut low, mut at) = (s[start], start);
    let mut since = 0;
    loop {
        i += dir;
        if i < 0 || i as usize >= s.len() {
            return (low, true, at);
        }
        let v = s[i as usize];
        if v < low - 0.3 {
            (low, at) = (v, i as usize);
            since = 0;
        } else {
            since += 1;
            if since >= patience {
                return (low, false, at);
            }
        }
    }
}

/// Left and right crossing points relative to the bin centres (fractional).
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
    // The distance between bin centres gives the width; add half a bin at each end.
    (left + 0.5, right + 0.5)
}

/// Walks from `start` in both directions while `inside` holds, returns the last bins where it held.
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

    /// Raised cosine power spectrum plus noise floor, in dB.
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
        let d = detect(&db, fs, n / 2 + 100).expect("signal should be found");
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
        let d = detect(&db, fs, guess).expect("signal should be found");
        assert_eq!(d.common_symbol_rate, Some(333e3), "{d:?}");
    }

    #[test]
    fn noise_only_finds_nothing() {
        let db = vec![-60.0; 1024];
        assert_eq!(detect(&db, 2.048e6, 512), None);
        assert!(scan(&db, 2.048e6).is_empty());
    }

    /// Wideband: out-of-band floor, an 8 MHz transponder hump and two signals inside it.
    #[test]
    fn scan_finds_signals_inside_transponder_hump() {
        let (n, fs) = (2048, 10e6);
        let a = rc_spectrum(n, fs, -3.25e6, 1.5e6, 0.2, 18.0);
        let b = rc_spectrum(n, fs, 1.0e6, 500e3, 0.2, 15.0);
        let db: Vec<f32> = (0..n)
            .map(|i| {
                let f = ((i as f64 + 0.5) / n as f64 - 0.5) * fs;
                let hump = if f.abs() < 4.0e6 { 7.0 } else { 0.0 };
                // Add linear powers: floor 1, hump and signals on top of it.
                let lin = 10f64.powf(hump / 10.0) + 10f64.powf(a[i] as f64 / 10.0) - 1.0 + 10f64.powf(b[i] as f64 / 10.0) - 1.0;
                (10.0 * lin.log10() - 60.0) as f32
            })
            .collect();
        let found = scan(&db, fs);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!((found[0].center_offset_hz + 3.25e6).abs() < 30e3, "{found:?}");
        assert_eq!(found[0].common_symbol_rate, Some(1.5e6), "{found:?}");
        assert!((found[1].center_offset_hz - 1.0e6).abs() < 30e3, "{found:?}");
        assert_eq!(found[1].common_symbol_rate, Some(500e3), "{found:?}");
    }
}
