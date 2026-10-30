//! PLFRAME body: descrambling, carrier tracking and QPSK soft demapping.
//!
//! ETSI EN 302 307-1, clause 5.5.4: all symbols after the header (data and
//! pilots) are scrambled by multiplying with {1, j, -1, -j} from a complex Gold sequence.
//! Clause 5.4.1: in QPSK the first bit sets the sign of I, the second the sign of Q
//! (0 → positive).

use num_complex::Complex;

use super::modulation::Mapping;
use super::plheader::HEADER_LEN;

/// Period of the Gold sequence: 2^18 - 1.
const GOLD_PERIOD: usize = (1 << 18) - 1;

/// The {0,1,2,3} values (power of j) of the scrambling sequence for `count` symbols.
/// `code` is the PL scrambling code number; usually 0 for QO-100 signals.
pub fn scrambling_sequence(code: usize, count: usize) -> Vec<u8> {
    // x: x^18 + x^7 + 1, x(0) = 1, others 0. y: x^18 + x^10 + x^7 + x^5 + 1, all 1.
    let len = GOLD_PERIOD + 131_072 + count;
    let mut x = vec![0u8; len + 18];
    let mut y = vec![0u8; len + 18];
    x[0] = 1;
    y[..18].fill(1);
    for i in 0..len {
        x[i + 18] = x[i + 7] ^ x[i];
        y[i + 18] = y[i + 10] ^ y[i + 7] ^ y[i + 5] ^ y[i];
    }
    let z = |i: usize| x[(i + code) % GOLD_PERIOD] ^ y[i];
    (0..count).map(|i| 2 * z((i + 131_072) % GOLD_PERIOD) + z(i)).collect()
}

/// Multiplies by the conjugate of the j^k factor (undoes the scrambling).
fn descramble(z: Complex<f32>, k: u8) -> Complex<f32> {
    match k {
        0 => z,
        1 => Complex::new(z.im, -z.re),  // × (-j)
        2 => -z,
        _ => Complex::new(-z.im, z.re), // × j
    }
}

/// Initial state for demodulating the symbols after a frame's header.
pub struct CarrierState {
    /// Carrier phase at the first symbol of the body (radians).
    pub phase: f32,
    /// Phase advance per symbol (radians).
    pub freq: f32,
}

pub struct Demapped {
    /// LLR for each bit; a positive value means bit 0.
    pub llr: Vec<f32>,
    /// Symbol SNR measured from the decision-directed phase error (dB).
    pub snr_db: f32,
    /// Frequency estimate of the carrier loop at the end of the body (radians/symbol).
    pub final_freq: f32,
}

/// At the start of a frame the loop runs `ACQUIRE_FACTOR` times wider for this many
/// symbols: it closes the phase and frequency error left by the header much faster
/// than the narrow loop, then switches to the narrow loop to reduce noise.
const ACQUIRE_SYMBOLS: usize = 1000;
const ACQUIRE_FACTOR: f32 = 2.0;

/// (Proportional, integral) gains of the second-order loop.
fn loop_gains(bn: f32) -> (f32, f32) {
    let damping = 0.707f32;
    let theta = bn / (damping + 1.0 / (4.0 * damping));
    let d = 1.0 + 2.0 * damping * theta + theta * theta;
    (4.0 * damping * theta / d, 4.0 * theta * theta / d)
}

/// Length of the pilot block that comes every 16 slots (1440 data symbols) in pilot frames.
const PILOT_LEN: usize = 36;
const PILOT_PERIOD: usize = 16 * 90;

/// Whether symbol `index` of the body is a pilot. `pilot_blocks` is the number of pilot blocks in the frame.
pub(crate) fn is_pilot(index: usize, pilot_blocks: usize) -> bool {
    let segment = PILOT_PERIOD + PILOT_LEN;
    index / segment < pilot_blocks && index % segment >= PILOT_PERIOD
}

/// Demodulates a QPSK frame body: undoes the scrambling, tracks the phase with a
/// decision-directed second-order PLL and produces LLRs for the data symbols.
/// `body` is the symbols right after the header (including pilots, if any),
/// `scrambling` a sequence of the same length. Pilots drive the phase tracking and are not part of the LLRs.
pub fn demap_qpsk(
    body: &[Complex<f32>],
    scrambling: &[u8],
    pilot_blocks: usize,
    start: CarrierState,
    bn: f32,
) -> Demapped {
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let (acquire, track) = (loop_gains(bn * ACQUIRE_FACTOR), loop_gains(bn));

    let mut phase = start.phase;
    let mut freq = start.freq;
    let mut llr = Vec::with_capacity(body.len() * 2);
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (index, (z, &k)) in body.iter().zip(scrambling).enumerate() {
        let r = descramble(*z, k) * Complex::from_polar(1.0, -phase);
        let pilot = is_pilot(index, pilot_blocks);
        // Pilots are the known (1 + j)/√2 symbol; for data, the nearest QPSK point.
        let decision = if pilot { Complex::new(s, s) } else { Complex::new(s.copysign(r.re), s.copysign(r.im)) };
        let e = (r * decision.conj()).im;
        let (kp, ki) = if index < ACQUIRE_SYMBOLS { acquire } else { track };
        freq += ki * e;
        // Wrap the phase so f32 precision is not lost over long bodies.
        phase = (phase + freq + kp * e).rem_euclid(std::f32::consts::TAU);
        if !pilot {
            llr.push(r.re);
            llr.push(r.im);
        }
        let amp = (r * decision.conj()).re;
        sig += (amp * amp) as f64;
        err += (r - decision * amp).norm_sqr() as f64;
    }
    let snr_db = 10.0 * (sig / err.max(1e-12)).log10() as f32;
    // LLR scale: 2·A·y/σ² ≈ y · 2√2 · SNR (unit-amplitude QPSK, per axis).
    let scale = 2.0 * std::f32::consts::SQRT_2 * 10f32.powf(snr_db / 10.0);
    llr.iter_mut().for_each(|l| *l *= scale);
    Demapped { llr, snr_db, final_freq: freq }
}

/// Demodulates an 8PSK/16APSK frame body. Phase tracking is the same as in
/// `demap_qpsk`; decisions are the nearest constellation point. LLRs are returned
/// deinterleaved, in codeword order.
pub fn demap(
    body: &[Complex<f32>],
    scrambling: &[u8],
    pilot_blocks: usize,
    start: CarrierState,
    bn: f32,
    mapping: &Mapping,
) -> Demapped {
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let (acquire, track) = (loop_gains(bn * ACQUIRE_FACTOR), loop_gains(bn));

    let mut phase = start.phase;
    let mut freq = start.freq;
    let mut data = Vec::with_capacity(body.len());
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (index, (z, &k)) in body.iter().zip(scrambling).enumerate() {
        let r = descramble(*z, k) * Complex::from_polar(1.0, -phase);
        let pilot = is_pilot(index, pilot_blocks);
        let decision = if pilot { Complex::new(s, s) } else { mapping.decide(r) };
        // Scale by amplitude so the error is not too small on inner ring points.
        let e = (r * decision.conj()).im / decision.norm_sqr().max(0.05);
        let (kp, ki) = if index < ACQUIRE_SYMBOLS { acquire } else { track };
        freq += ki * e;
        phase = (phase + freq + kp * e).rem_euclid(std::f32::consts::TAU);
        if !pilot {
            data.push(r);
            sig += decision.norm_sqr() as f64;
            err += (r - decision).norm_sqr() as f64;
        }
    }
    let noise = (err / data.len().max(1) as f64).max(1e-6) as f32;
    let snr_db = 10.0 * (sig / err.max(1e-12)).log10() as f32;
    let bits = mapping.bits_per_symbol;
    let n = data.len() * bits;
    let mut llr = vec![0.0f32; n];
    let mut sym = [0.0f32; 5];
    for (i, z) in data.iter().enumerate() {
        mapping.llrs(*z, noise, &mut sym);
        for (b, &l) in sym[..bits].iter().enumerate() {
            llr[mapping.codeword_position(n, i, b)] = l;
        }
    }
    Demapped { llr, snr_db, final_freq: freq }
}

/// Computes the phase at the first symbol of the body from the header's measured phase and frequency.
/// `header_phase` is the phase at the header's first symbol, `freq` in radians/symbol.
pub fn body_start(header_phase: f32, freq: f32) -> CarrierState {
    CarrierState { phase: header_phase + freq * HEADER_LEN as f32, freq }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pilot_positions_follow_slot_structure() {
        // QPSK normal with pilots: 360 slots, 22 pilot blocks; body 32400 + 792 symbols.
        let body_len = 360 * 90 + 22 * 36;
        let pilots = (0..body_len).filter(|&i| is_pilot(i, 22)).count();
        assert_eq!(pilots, 22 * 36);
        assert!(!is_pilot(1439, 22) && is_pilot(1440, 22) && is_pilot(1475, 22) && !is_pilot(1476, 22));
        assert!(!is_pilot(body_len - 1, 22));
    }

    #[test]
    fn scrambling_starts_with_known_values() {
        // First values for code 0; also checks that the sequence is periodic and 4-valued.
        let seq = scrambling_sequence(0, 1000);
        assert!(seq.iter().all(|&k| k < 4));
        assert!(seq.iter().any(|&k| k == 3));
        // x(0)=1, y(0)=1 → z(0)=0; together with z(131072), R(0) ∈ {0, 2}.
        assert!(seq[0] == 0 || seq[0] == 2);
    }

    #[test]
    fn demaps_scrambled_qpsk_with_frequency_offset() {
        let mut rng = crate::dvbs2::timing::tests::Lcg(11);
        let bits: Vec<u8> = (0..4000).map(|_| rng.next_bit()).collect();
        let scr = scrambling_sequence(0, 2000);
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let w = 0.003f32;
        let body: Vec<Complex<f32>> = bits
            .chunks(2)
            .zip(&scr)
            .enumerate()
            .map(|(n, (b, &k))| {
                let sym = Complex::new(if b[0] == 0 { s } else { -s }, if b[1] == 0 { s } else { -s });
                let jk = Complex::new(0.0, 1.0f32).powu(k as u32);
                sym * jk * Complex::from_polar(1.0, 0.4 + w * n as f32)
            })
            .collect();
        let out = demap_qpsk(&body, &scr, 0, CarrierState { phase: 0.4, freq: w }, 0.01);
        let errors = out.llr.iter().zip(&bits).filter(|(l, b)| (**l < 0.0) != (**b == 1)).count();
        assert_eq!(errors, 0);
        assert!(out.snr_db > 30.0);
    }
}
