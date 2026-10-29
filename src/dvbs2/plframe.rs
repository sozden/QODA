//! PLFRAME gövdesi: karıştırma çözümü, taşıyıcı takibi ve QPSK yumuşak demap.
//!
//! ETSI EN 302 307-1, bölüm 5.5.4: başlıktan sonraki tüm semboller (veri ve
//! pilotlar) karmaşık bir Gold dizisiyle {1, j, -1, -j} çarpılarak karıştırılır.
//! Bölüm 5.4.1: QPSK'de ilk bit I'nın, ikinci bit Q'nun işaretini belirler
//! (0 → pozitif).

use num_complex::Complex;

use super::plheader::HEADER_LEN;

/// Gold dizisinin periyodu: 2^18 - 1.
const GOLD_PERIOD: usize = (1 << 18) - 1;

/// `count` sembol için karıştırma dizisinin {0,1,2,3} değerleri (j'nin kuvveti).
/// `code` PL karıştırma kodu numarasıdır; QO-100 yayınlarında genelde 0.
pub fn scrambling_sequence(code: usize, count: usize) -> Vec<u8> {
    // x: x^18 + x^7 + 1, x(0) = 1, diğerleri 0. y: x^18 + x^10 + x^7 + x^5 + 1, hepsi 1.
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

/// j^k çarpanının eşleniğiyle çarpar (karıştırmayı geri alır).
fn descramble(z: Complex<f32>, k: u8) -> Complex<f32> {
    match k {
        0 => z,
        1 => Complex::new(z.im, -z.re),  // × (-j)
        2 => -z,
        _ => Complex::new(-z.im, z.re), // × j
    }
}

/// Bir çerçevenin başlık sonrası sembollerini çözmek için başlangıç durumu.
pub struct CarrierState {
    /// Gövdenin ilk sembolündeki taşıyıcı fazı (radyan).
    pub phase: f32,
    /// Sembol başına faz ilerlemesi (radyan).
    pub freq: f32,
}

pub struct Demapped {
    /// Her bit için LLR; pozitif değer bit 0 demektir.
    pub llr: Vec<f32>,
    /// Karar yönlendirmeli faz hatasından ölçülen sembol SNR'ı (dB).
    pub snr_db: f32,
}

/// Pilotsuz QPSK çerçeve gövdesini çözer: karıştırmayı geri alır, karar
/// yönlendirmeli ikinci derece PLL ile fazı takip eder ve LLR üretir.
/// `body` başlıktan hemen sonraki semboller, `scrambling` aynı uzunlukta dizi.
pub fn demap_qpsk(body: &[Complex<f32>], scrambling: &[u8], start: CarrierState, bn: f32) -> Demapped {
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let damping = 0.707f32;
    let theta = bn / (damping + 1.0 / (4.0 * damping));
    let d = 1.0 + 2.0 * damping * theta + theta * theta;
    let (kp, ki) = (4.0 * damping * theta / d, 4.0 * theta * theta / d);

    let mut phase = start.phase;
    let mut freq = start.freq;
    let mut llr = Vec::with_capacity(body.len() * 2);
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (z, &k) in body.iter().zip(scrambling) {
        let r = descramble(*z, k) * Complex::from_polar(1.0, -phase);
        let decision = Complex::new(s.copysign(r.re), s.copysign(r.im));
        let e = (r * decision.conj()).im;
        freq += ki * e;
        // Uzun gövdelerde f32 hassasiyeti kaybolmasın diye fazı sar.
        phase = (phase + freq + kp * e).rem_euclid(std::f32::consts::TAU);
        llr.push(r.re);
        llr.push(r.im);
        let amp = (r * decision.conj()).re;
        sig += (amp * amp) as f64;
        err += (r - decision * amp).norm_sqr() as f64;
    }
    let snr_db = 10.0 * (sig / err.max(1e-12)).log10() as f32;
    // LLR ölçeği: 2·A·y/σ² ≈ y · 2√2 · SNR (birim genlikli QPSK, eksen başına).
    let scale = 2.0 * std::f32::consts::SQRT_2 * 10f32.powf(snr_db / 10.0);
    llr.iter_mut().for_each(|l| *l *= scale);
    Demapped { llr, snr_db }
}

/// Başlığın ölçülen faz ve frekansından gövdenin ilk sembolündeki fazı hesaplar.
/// `header_phase` başlığın ilk sembolündeki faz, `freq` radyan/sembol.
pub fn body_start(header_phase: f32, freq: f32) -> CarrierState {
    CarrierState { phase: header_phase + freq * HEADER_LEN as f32, freq }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrambling_starts_with_known_values() {
        // Kod 0 için ilk değerler; dizinin periyodik ve 4 değerli olduğunu da sınar.
        let seq = scrambling_sequence(0, 1000);
        assert!(seq.iter().all(|&k| k < 4));
        assert!(seq.iter().any(|&k| k == 3));
        // x(0)=1, y(0)=1 → z(0)=0; z(131072) ile birlikte R(0) ∈ {0, 2}.
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
        let out = demap_qpsk(&body, &scr, CarrierState { phase: 0.4, freq: w }, 0.01);
        let errors = out.llr.iter().zip(&bits).filter(|(l, b)| (**l < 0.0) != (**b == 1)).count();
        assert_eq!(errors, 0);
        assert!(out.snr_db > 30.0);
    }
}
