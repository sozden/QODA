//! DVB-S2 LDPC kodu: eşlik matrisini standart tablodan kurma, kodlama ve
//! katmanlı normalize min-sum çözme.
//!
//! ETSI EN 302 307-1, bölüm 5.3.2: bilgi bitleri 360'lık gruplara ayrılır.
//! Tablonun r. satırı r. grubun ilk bitinin bağlı olduğu eşlik denetimlerini
//! verir; gruptaki m. bit için adresler `(x + m·q) mod (n - k)` olur. Eşlik
//! bitleri birikimlidir: `p_j = p_j ⊕ p_{j-1}`. Bu yüzden j. denetim, kendi
//! bilgi bitleri ile p_j ve p_{j-1}'i içerir (merdiven yapısı).

/// DVB-S2 kodlarında gruptaki bit sayısı.
pub const GROUP: usize = 360;
/// Min-sum mesajlarının ölçek katsayısı.
const NORMALIZATION: f32 = 0.8;

pub struct LdpcCode {
    pub n: usize,
    pub k: usize,
    /// CSR biçiminde denetim → değişken bağlantıları.
    check_start: Vec<u32>,
    edges: Vec<u32>,
}

pub struct Decoded {
    /// Kod sözcüğünün tamamı (bilgi + eşlik), sert kararlar.
    pub bits: Vec<u8>,
    pub iterations: usize,
    /// Tüm eşlik denetimleri sağlandı mı.
    pub converged: bool,
}

impl LdpcCode {
    /// `rows`: standart tablonun satırları (her biri bir bilgi grubunun adresleri).
    /// `group`: grup boyutu (DVB-S2'de 360; testlerde küçük değerler kullanılabilir).
    pub fn from_table(n: usize, k: usize, group: usize, rows: &[&[u32]]) -> Self {
        let m = n - k;
        assert_eq!(k % group, 0, "k grup boyutunun katı olmalı");
        assert_eq!(rows.len(), k / group, "tablo satır sayısı k / grup olmalı");
        let q = m / group;
        let mut per_check: Vec<Vec<u32>> = vec![Vec::new(); m];
        for (r, row) in rows.iter().enumerate() {
            for j in 0..group {
                let var = (r * group + j) as u32;
                for &x in row.iter() {
                    per_check[(x as usize + j * q) % m].push(var);
                }
            }
        }
        for (j, vars) in per_check.iter_mut().enumerate() {
            if j > 0 {
                vars.push((k + j - 1) as u32);
            }
            vars.push((k + j) as u32);
        }
        let mut check_start = Vec::with_capacity(m + 1);
        let mut edges = Vec::new();
        check_start.push(0);
        for vars in &per_check {
            edges.extend_from_slice(vars);
            check_start.push(edges.len() as u32);
        }
        Self { n, k, check_start, edges }
    }

    fn check(&self, j: usize) -> &[u32] {
        &self.edges[self.check_start[j] as usize..self.check_start[j + 1] as usize]
    }

    /// Bilgi bitlerinden sistematik kod sözcüğü üretir (testler için).
    pub fn encode(&self, info: &[u8]) -> Vec<u8> {
        assert_eq!(info.len(), self.k);
        let mut word = info.to_vec();
        word.resize(self.n, 0);
        let mut prev = 0u8;
        for j in 0..self.n - self.k {
            let acc = self
                .check(j)
                .iter()
                .filter(|&&v| (v as usize) < self.k)
                .fold(0u8, |a, &v| a ^ info[v as usize]);
            prev ^= acc;
            word[self.k + j] = prev;
        }
        word
    }

    /// Tüm eşlik denetimleri sağlanıyor mu.
    pub fn is_codeword(&self, bits: &[u8]) -> bool {
        (0..self.n - self.k).all(|j| self.check(j).iter().fold(0u8, |a, &v| a ^ bits[v as usize]) == 0)
    }

    /// `llr`: her bit için log-olabilirlik oranı (pozitif → 0).
    pub fn decode(&self, llr: &[f32], max_iterations: usize) -> Decoded {
        assert_eq!(llr.len(), self.n);
        let mut posterior = llr.to_vec();
        let mut messages = vec![0f32; self.edges.len()];
        let mut q = Vec::new();
        let mut bits = vec![0u8; self.n];

        for iteration in 1..=max_iterations {
            for j in 0..self.n - self.k {
                let range = self.check_start[j] as usize..self.check_start[j + 1] as usize;
                q.clear();
                let (mut min1, mut min2, mut min_at) = (f32::INFINITY, f32::INFINITY, 0);
                let mut sign_negative = false;
                for (i, e) in range.clone().enumerate() {
                    let v = self.edges[e] as usize;
                    let value = posterior[v] - messages[e];
                    q.push(value);
                    let mag = value.abs();
                    if mag < min1 {
                        min2 = min1;
                        min1 = mag;
                        min_at = i;
                    } else if mag < min2 {
                        min2 = mag;
                    }
                    sign_negative ^= value < 0.0;
                }
                for (i, e) in range.enumerate() {
                    let mag = NORMALIZATION * if i == min_at { min2 } else { min1 };
                    let negative = sign_negative ^ (q[i] < 0.0);
                    let msg = if negative { -mag } else { mag };
                    messages[e] = msg;
                    posterior[self.edges[e] as usize] = q[i] + msg;
                }
            }
            for (b, l) in bits.iter_mut().zip(&posterior) {
                *b = (*l < 0.0) as u8;
            }
            if self.is_codeword(&bits) {
                return Decoded { bits, iterations: iteration, converged: true };
            }
        }
        Decoded { bits, iterations: max_iterations, converged: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvbs2::timing::tests::Lcg;

    /// Küçük, DVB-S2 yapısında bir test kodu: n = 720, k = 360, grup 36.
    fn test_code() -> LdpcCode {
        let rows: Vec<Vec<u32>> = (0..10u32)
            .map(|r| vec![(r * 37 + 5) % 360, (r * 101 + 77) % 360, (r * 211 + 151) % 360])
            .collect();
        let refs: Vec<&[u32]> = rows.iter().map(|r| r.as_slice()).collect();
        LdpcCode::from_table(720, 360, 36, &refs)
    }

    #[test]
    fn encoded_words_satisfy_all_checks() {
        let code = test_code();
        let mut rng = Lcg(5);
        let info: Vec<u8> = (0..code.k).map(|_| rng.next_bit()).collect();
        assert!(code.is_codeword(&code.encode(&info)));
    }

    #[test]
    fn corrects_noisy_bpsk() {
        let code = test_code();
        let mut rng = Lcg(9);
        let info: Vec<u8> = (0..code.k).map(|_| rng.next_bit()).collect();
        let word = code.encode(&info);
        // Basit gürültü: her 25 bitten birini yanlış işaretle ve tüm LLR'leri zayıflat.
        let llr: Vec<f32> = word
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                let clean = if b == 0 { 4.0 } else { -4.0 };
                if i % 25 == 3 { -clean * 0.5 } else { clean * (0.6 + 0.4 * rng.next_bit() as f32) }
            })
            .collect();
        let hard_errors = llr.iter().zip(&word).filter(|(l, b)| (**l < 0.0) != (**b == 1)).count();
        assert!(hard_errors > 20);
        let out = code.decode(&llr, 50);
        assert!(out.converged, "{} iterasyonda yakınsamadı", out.iterations);
        assert_eq!(out.bits, word);
    }
}
