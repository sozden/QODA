//! DVB-S2 LDPC code: building the parity-check matrix from the standard's table,
//! encoding and layered normalized min-sum decoding.
//!
//! ETSI EN 302 307-1, clause 5.3.2: information bits are split into groups of 360.
//! Row r of the table gives the parity checks the first bit of group r is connected
//! to; for bit m of the group the addresses are `(x + m·q) mod (n - k)`. Parity
//! bits are accumulated: `p_j = p_j ⊕ p_{j-1}`. So check j contains its own
//! information bits plus p_j and p_{j-1} (staircase structure).

/// Number of bits in a group in DVB-S2 codes.
pub const GROUP: usize = 360;
/// Scaling factor of min-sum messages.
const NORMALIZATION: f32 = 0.8;
/// Early stop: look back this many iterations; if the number of unsatisfied checks
/// has not dropped enough over that span, the frame is considered undecodable.
const STALL_WINDOW: usize = 6;
const STALL_IMPROVEMENT: f32 = 0.9;

pub struct LdpcCode {
    pub n: usize,
    pub k: usize,
    /// Check → variable connections in CSR form.
    check_start: Vec<u32>,
    edges: Vec<u32>,
}

pub struct Decoded {
    /// The whole codeword (information + parity), hard decisions.
    pub bits: Vec<u8>,
    pub iterations: usize,
    /// Whether all parity checks are satisfied.
    pub converged: bool,
}

impl LdpcCode {
    /// `rows`: rows of the standard's table (each the addresses of one information group).
    /// `group`: group size (360 in DVB-S2; tests may use small values).
    pub fn from_table(n: usize, k: usize, group: usize, rows: &[&[u16]]) -> Self {
        let m = n - k;
        assert_eq!(k % group, 0, "k must be a multiple of the group size");
        assert_eq!(rows.len(), k / group, "the table must have k / group rows");
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

    /// Produces a systematic codeword from information bits (for tests).
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

    /// Number of unsatisfied parity checks.
    fn unsatisfied(&self, bits: &[u8]) -> usize {
        (0..self.n - self.k)
            .filter(|&j| self.check(j).iter().fold(0u8, |a, &v| a ^ bits[v as usize]) != 0)
            .count()
    }

    /// Whether all parity checks are satisfied.
    pub fn is_codeword(&self, bits: &[u8]) -> bool {
        (0..self.n - self.k).all(|j| self.check(j).iter().fold(0u8, |a, &v| a ^ bits[v as usize]) == 0)
    }

    /// `llr`: log-likelihood ratio for each bit (positive → 0).
    pub fn decode(&self, llr: &[f32], max_iterations: usize) -> Decoded {
        assert_eq!(llr.len(), self.n);
        let mut posterior = llr.to_vec();
        let mut messages = vec![0f32; self.edges.len()];
        let mut q = Vec::new();
        let mut bits = vec![0u8; self.n];
        let mut history = Vec::with_capacity(max_iterations);

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
            let unsatisfied = self.unsatisfied(&bits);
            if unsatisfied == 0 {
                return Decoded { bits, iterations: iteration, converged: true };
            }
            // Decoding that makes no progress eats real time; give up when there is no hope.
            if iteration > STALL_WINDOW
                && unsatisfied as f32 > STALL_IMPROVEMENT * history[iteration - 1 - STALL_WINDOW] as f32
            {
                return Decoded { bits, iterations: iteration, converged: false };
            }
            history.push(unsatisfied);
        }
        Decoded { bits, iterations: max_iterations, converged: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvbs2::timing::tests::Lcg;

    /// A small test code with the DVB-S2 structure: n = 720, k = 360, group 36.
    fn test_code() -> LdpcCode {
        let rows: Vec<Vec<u16>> = (0..10u16)
            .map(|r| vec![(r * 37 + 5) % 360, (r * 101 + 77) % 360, (r * 211 + 151) % 360])
            .collect();
        let refs: Vec<&[u16]> = rows.iter().map(|r| r.as_slice()).collect();
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
        // Simple noise: flip the sign of one bit in 25 and weaken all LLRs.
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
        assert!(out.converged, "did not converge in {} iterations", out.iterations);
        assert_eq!(out.bits, word);
    }
}
