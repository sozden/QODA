//! Streaming DVB-S2 receiver: IQ samples go in chunk by chunk, MPEG-TS comes out.
//!
//! Chain: DC blocking → frequency shift → (decimation if needed) → RRC matched filter → AGC → Gardner timing →
//! PL header sync → descrambling + carrier tracking + demap → LDPC → BCH →
//! BBFRAME → TS. Once synced, every header is verified at its expected position.

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;
use std::sync::mpsc::{self, Receiver as ChannelReceiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use num_complex::Complex;

use super::bbframe::{self, TsDepacketizer};
use super::fec::{Bch, FecParams};
use super::filters;
use super::ldpc::{Decoded, LdpcCode};
use super::modulation::Mapping;
use super::plframe::{self, CarrierState};
use super::plheader::{HEADER_LEN, HeaderHit, HeaderSearch, Modulation, Pls};
use super::timing::TimingRecovery;
use crate::tsinfo::{ServiceInfo, TsInfo};

const TIMING_BANDWIDTH: f64 = 0.005;
/// Mean tracking factor of the DC blocker (per sample). It learns local oscillator
/// leakage (the HackRF and RTL-SDR centre spike) in about 1/α samples; its notch
/// is narrow enough not to affect the signal itself.
const DC_ALPHA: f32 = 1e-5;
/// At least this many samples per symbol remain after decimation.
const MIN_SAMPLES_PER_SYMBOL: f64 = 2.4;
/// Bandwidth of the carrier tracking loop (relative to the symbol rate). A wider
/// loop slips on weak signals and on receivers with high phase noise such as the HackRF.
const CARRIER_BANDWIDTH: f32 = 0.002;
const LDPC_MAX_ITERATIONS: usize = 50;
/// Maximum number of threads for LDPC decoding.
const MAX_LDPC_WORKERS: usize = 8;
/// Number of symbols collected to search for headers while unlocked (more than one normal frame).
const SEARCH_WINDOW: usize = 40_000;
/// Symbol margin searched around the expected header while locked.
const VERIFY_SLACK: usize = 16;
/// The lock is dropped if this many headers in a row cannot be verified.
const MAX_MISSES: u32 = 3;
/// Minimum PLS confidence for accepting a header.
const MIN_PLS_MARGIN: f32 = 0.3;
/// Minimum SOF correlation for a header to count as in place while locked.
const MIN_LOCKED_SOF: f32 = 0.8;
/// Weight of the PLL frequency in the common estimate at the end of each frame.
const FREQ_TRACKING: f32 = 0.5;

pub struct ReceiverConfig {
    pub sample_rate: f64,
    /// Offset of the signal's centre from the centre of the received band (Hz).
    pub offset_hz: f64,
    pub symbol_rate: f64,
    pub rolloff: f64,
}

#[derive(Default, Clone)]
pub struct ReceiverStats {
    pub locked: bool,
    pub pls: Option<Pls>,
    pub frames_ok: u64,
    /// Frames on which LDPC did not converge.
    pub frames_failed: u64,
    pub bch_errors: u64,
    /// Frames in a modulation that is not supported yet.
    pub frames_unsupported: u64,
    /// SNR of the last frame body (dB).
    pub snr_db: f32,
    /// Remaining carrier offset relative to the tuned frequency (Hz).
    pub carrier_offset_hz: f64,
    pub ts_packets: u64,
    pub ts_crc_errors: u64,
    /// Iterations LDPC spent on the last frame.
    pub ldpc_iterations: usize,
    /// TS continuity counter discontinuities (packets missing from the broadcast).
    pub ts_cc_errors: u64,
    /// Service name, provider and streams read from the DVB tables.
    pub service: ServiceInfo,
}

struct FecState {
    pls: Pls,
    params: FecParams,
    ldpc: LdpcCode,
    bch: Bch,
}

/// A frame sent to the LDPC threads.
struct Job {
    seq: u64,
    fec: Arc<FecState>,
    llr: Vec<f32>,
}

struct Outcome {
    seq: u64,
    fec: Arc<FecState>,
    decoded: Decoded,
}

/// Threads decoding frames in parallel; results come back with a sequence number.
fn spawn_ldpc_workers() -> (Sender<Job>, ChannelReceiver<Outcome>) {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (out_tx, out_rx) = mpsc::channel();
    let job_rx = Arc::new(Mutex::new(job_rx));
    let workers = thread::available_parallelism()
        .map_or(2, |n| n.get().saturating_sub(2))
        .clamp(1, MAX_LDPC_WORKERS);
    for _ in 0..workers {
        let job_rx = Arc::clone(&job_rx);
        let out_tx = out_tx.clone();
        thread::spawn(move || {
            loop {
                let job = match job_rx.lock().unwrap().recv() {
                    Ok(job) => job,
                    Err(_) => return,
                };
                let decoded = job.fec.ldpc.decode(&job.llr, LDPC_MAX_ITERATIONS);
                let outcome = Outcome { seq: job.seq, fec: job.fec, decoded };
                if out_tx.send(outcome).is_err() {
                    return;
                }
            }
        });
    }
    (job_tx, out_rx)
}

pub struct Receiver {
    cfg: ReceiverConfig,
    /// Tracked value of the DC component at the input.
    dc: Complex<f32>,
    /// Frequency shift oscillator: rotates by `osc_step` every sample.
    osc: Complex<f32>,
    osc_step: Complex<f32>,
    /// Filter bringing the signal down to a rate near the symbol rate when the input rate is high.
    decimator: Option<filters::StreamingFir>,
    rrc: filters::StreamingFir,
    agc_power: Option<f32>,
    timing: TimingRecovery,
    symbols: Vec<Complex<f32>>,
    search: HeaderSearch,
    /// Expected position of the next header within `symbols` while locked.
    expected: Option<usize>,
    last_frame_len: usize,
    misses: u32,
    /// Common frequency offset (radians/symbol). Set from the header at first lock, then
    /// updated from the PLL frequency at the end of every frame.
    freq: Option<f32>,
    fec: Option<Arc<FecState>>,
    jobs: Sender<Job>,
    outcomes: ChannelReceiver<Outcome>,
    /// Number of frames sent and processed in order; those in between wait in `pending`.
    submitted: u64,
    emitted: u64,
    pending: BTreeMap<u64, Outcome>,
    scrambling: Vec<u8>,
    depacketizer: TsDepacketizer,
    last_cc: HashMap<u16, u8>,
    ts_info: TsInfo,
    pub stats: ReceiverStats,
}

impl Receiver {
    pub fn new(cfg: ReceiverConfig) -> Self {
        let decimation = ((cfg.sample_rate / (MIN_SAMPLES_PER_SYMBOL * cfg.symbol_rate)) as usize).max(1);
        let sps = cfg.sample_rate / decimation as f64 / cfg.symbol_rate;
        let decimator = (decimation > 1)
            .then(|| filters::StreamingFir::new(filters::decimation_taps(decimation), decimation));
        let rrc = filters::StreamingFir::new(filters::rrc_taps(sps, cfg.rolloff, 8), 1);
        let (jobs, outcomes) = spawn_ldpc_workers();
        Self {
            dc: Complex::default(),
            osc: Complex::new(1.0, 0.0),
            osc_step: Complex::from_polar(1.0, (-TAU * cfg.offset_hz / cfg.sample_rate) as f32),
            decimator,
            rrc,
            agc_power: None,
            timing: TimingRecovery::new(sps, TIMING_BANDWIDTH),
            symbols: Vec::new(),
            search: HeaderSearch::new(),
            expected: None,
            last_frame_len: 0,
            misses: 0,
            freq: None,
            fec: None,
            jobs,
            outcomes,
            submitted: 0,
            emitted: 0,
            pending: BTreeMap::new(),
            scrambling: Vec::new(),
            depacketizer: TsDepacketizer::default(),
            last_cc: Default::default(),
            ts_info: TsInfo::default(),
            stats: ReceiverStats::default(),
            cfg,
        }
    }

    /// Processes IQ samples; appends decoded TS packets to `ts`.
    pub fn process(&mut self, iq: &[Complex<f32>], ts: &mut Vec<u8>) {
        let filtered = self.front_end(iq);
        let mut symbols = Vec::with_capacity(filtered.len());
        self.timing.process(&filtered, &mut symbols);
        self.symbols.extend_from_slice(&symbols);
        self.frame_sync();
        self.collect(ts, false);
    }

    /// Waits for and processes all pending LDPC results (at the end of a recording).
    pub fn finish(&mut self, ts: &mut Vec<u8>) {
        self.collect(ts, true);
    }

    /// Collects LDPC results and processes them in frame order.
    fn collect(&mut self, ts: &mut Vec<u8>, wait: bool) {
        while self.emitted < self.submitted {
            let next = if wait { self.outcomes.recv().ok() } else { self.outcomes.try_recv().ok() };
            let got = next.is_some();
            if let Some(o) = next {
                self.pending.insert(o.seq, o);
            }
            while let Some(o) = self.pending.remove(&self.emitted) {
                self.emitted += 1;
                self.finish_frame(o, ts);
            }
            if !got {
                return;
            }
        }
    }

    /// DC blocking, frequency shift, decimation, RRC filter and AGC.
    fn front_end(&mut self, iq: &[Complex<f32>]) -> Vec<Complex<f32>> {
        let mixed: Vec<Complex<f32>> = iq
            .iter()
            .map(|s| {
                self.dc += (s - self.dc) * DC_ALPHA;
                let v = (s - self.dc) * self.osc;
                self.osc *= self.osc_step;
                v
            })
            .collect();
        // Keep the oscillator's amplitude from drifting with rounding errors.
        self.osc /= self.osc.norm();
        let narrowed = match &mut self.decimator {
            Some(d) => {
                let mut out = Vec::with_capacity(mixed.len());
                d.process(&mixed, &mut out);
                out
            }
            None => mixed,
        };
        let mut out = Vec::with_capacity(narrowed.len());
        self.rrc.process(&narrowed, &mut out);

        if !out.is_empty() {
            let p = out.iter().map(|z| z.norm_sqr()).sum::<f32>() / out.len() as f32;
            let avg = match self.agc_power {
                Some(a) => 0.9 * a + 0.1 * p,
                None => p,
            };
            self.agc_power = Some(avg);
            if avg > 0.0 {
                let k = 1.0 / avg.sqrt();
                out.iter_mut().for_each(|z| *z *= k);
            }
        }
        out
    }

    fn frame_sync(&mut self) {
        loop {
            let Some(expected) = self.expected else {
                if self.symbols.len() < SEARCH_WINDOW {
                    return;
                }
                let found = self
                    .search
                    .find_all(&self.symbols, 0.7)
                    .into_iter()
                    .find(|h| h.pls_margin >= 0.5 && h.pls.frame_len().is_some());
                match found {
                    Some(h) => {
                        self.expected = Some(h.position);
                        self.misses = 0;
                    }
                    None => {
                        let cut = self.symbols.len() - HEADER_LEN;
                        self.symbols.drain(..cut);
                        return;
                    }
                }
                continue;
            };

            if self.symbols.len() < expected + HEADER_LEN + VERIFY_SLACK {
                return;
            }
            let lo = expected.saturating_sub(VERIFY_SLACK);
            let window = &self.symbols[lo..expected + HEADER_LEN + VERIFY_SLACK];
            // While locked, if the SOF is in place the frame is accepted even with low PLS confidence:
            // as is if the same signalling came, with the previous signalling if it is ambiguous.
            let last = self.stats.pls;
            // If the frequency is known it is not re-estimated from the header: a single header's
            // estimate is much noisier at low SNR than the value the PLL tracks.
            let hit = self.search.best(window, self.freq).and_then(|mut h| {
                let confident = h.pls_margin >= MIN_PLS_MARGIN && h.pls.frame_len().is_some();
                if confident || Some(h.pls) == last {
                    Some(h)
                } else if h.sof_metric >= MIN_LOCKED_SOF {
                    h.pls = last?;
                    Some(h)
                } else {
                    None
                }
            });

            let Some(mut hit) = hit else {
                if std::env::var_os("QODA_DEBUG").is_some() {
                    let b = self.search.best(window, self.freq);
                    eprintln!(
                        "header not verified: frame {} best {:?}",
                        self.stats.frames_ok + self.stats.frames_failed,
                        b.map(|h| (h.position, h.sof_metric, h.pls_margin, h.pls))
                    );
                }
                self.misses += 1;
                if self.misses >= MAX_MISSES || self.last_frame_len == 0 {
                    self.expected = None;
                    self.stats.locked = false;
                } else {
                    // The header could not be read this time; assume the frame length has not changed.
                    self.advance(expected + self.last_frame_len);
                }
                continue;
            };
            hit.position += lo;
            let len = hit.pls.frame_len().unwrap();
            if self.symbols.len() < hit.position + len {
                return;
            }
            self.decode_frame(&hit);
            self.misses = 0;
            self.last_frame_len = len;
            self.stats.locked = true;
            self.stats.pls = Some(hit.pls);
            self.advance(hit.position + len);
        }
    }

    /// Waits for the next header at position `next` and drops the symbols before it.
    fn advance(&mut self, next: usize) {
        let cut = next.saturating_sub(VERIFY_SLACK).min(self.symbols.len());
        self.symbols.drain(..cut);
        self.expected = Some(next - cut);
    }

    /// Checks the continuity counter per PID in new TS packets.
    fn count_cc_errors(&mut self, packets: &[u8]) {
        for p in packets.chunks_exact(188) {
            let pid = (((p[1] & 0x1F) as u16) << 8) | p[2] as u16;
            if pid == 0x1FFF || p[3] & 0x10 == 0 {
                continue;
            }
            let cc = p[3] & 0x0F;
            if let Some(prev) = self.last_cc.insert(pid, cc) {
                if cc != (prev + 1) % 16 && cc != prev {
                    self.stats.ts_cc_errors += 1;
                }
            }
        }
    }

    /// Demaps the frame body and sends it to the threads for LDPC decoding.
    fn decode_frame(&mut self, hit: &HeaderHit) {
        let pls = hit.pls;
        if pls.modcod == 0 {
            return; // dummy frame
        }
        let Some((modulation, rate)) = pls.modulation_and_rate() else { return };

        let w = hit.freq_rad_per_symbol;
        let freq = *self.freq.get_or_insert(w);
        self.stats.carrier_offset_hz = freq as f64 * self.cfg.symbol_rate / TAU;

        // 32APSK is not supported yet.
        let mapping = match modulation {
            Modulation::Qpsk => None,
            m => match Mapping::new(m, rate) {
                Some(map) => Some(map),
                None => {
                    self.stats.frames_unsupported += 1;
                    return;
                }
            },
        };
        if self.fec.as_ref().is_none_or(|f| f.pls != pls) {
            let Some(params) = FecParams::new(rate, pls.short_frame) else { return };
            self.fec = Some(Arc::new(FecState { pls, ldpc: params.ldpc(), bch: params.bch(), params }));
        }

        let start = hit.position + HEADER_LEN;
        let body_len = pls.frame_len().unwrap() - HEADER_LEN;
        if self.scrambling.len() < body_len {
            self.scrambling = plframe::scrambling_sequence(0, body_len);
        }
        // The phase in the middle of the header is the most reliable point for the header's own
        // frequency estimate; from there advance to the start of the body with the common frequency.
        let mid = HEADER_LEN as f32 / 2.0;
        let carrier = CarrierState {
            phase: hit.phase + hit.freq_rad_per_symbol * mid + freq * mid,
            freq,
        };
        let body = &self.symbols[start..start + body_len];
        let scrambling = &self.scrambling[..body_len];
        let demapped = match &mapping {
            None => plframe::demap_qpsk(body, scrambling, pls.pilot_blocks(), carrier, CARRIER_BANDWIDTH),
            Some(map) => plframe::demap(body, scrambling, pls.pilot_blocks(), carrier, CARRIER_BANDWIDTH, map),
        };
        self.stats.snr_db = demapped.snr_db;
        // The PLL's frequency at the end of the body is much better than a single header's estimate.
        // It is used without waiting for the LDPC result; otherwise, while the first estimate is wrong,
        // the following headers are decoded with the same error.
        if let Some(f) = &mut self.freq {
            *f += FREQ_TRACKING * (demapped.final_freq - *f);
        }

        let job = Job {
            seq: self.submitted,
            fec: Arc::clone(self.fec.as_ref().unwrap()),
            llr: demapped.llr,
        };
        if self.jobs.send(job).is_ok() {
            self.submitted += 1;
        }
    }

    /// Finishes frames in order as their LDPC results arrive: BCH, BBFRAME, TS.
    fn finish_frame(&mut self, o: Outcome, ts: &mut Vec<u8>) {
        let Outcome { fec, decoded, .. } = o;
        self.stats.ldpc_iterations = decoded.iterations;
        if !decoded.converged {
            self.stats.frames_failed += 1;
            return;
        }
        self.stats.frames_ok += 1;
        if !fec.bch.is_codeword(&decoded.bits[..fec.params.k_ldpc]) {
            self.stats.bch_errors += 1;
        }
        let mut bytes: Vec<u8> = decoded.bits[..fec.params.k_bch]
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect();
        bbframe::bb_descramble(&mut bytes);
        let before = ts.len();
        self.depacketizer.push(&bytes, ts);
        self.count_cc_errors(&ts[before..]);
        self.ts_info.push(&ts[before..]);
        if self.ts_info.info() != &self.stats.service {
            self.stats.service = self.ts_info.info().clone();
        }
        self.stats.ts_packets = self.depacketizer.packets as u64;
        self.stats.ts_crc_errors = self.depacketizer.crc_errors as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvbs2::plframe::{is_pilot, scrambling_sequence};
    use crate::dvbs2::plheader::header_symbols;
    use crate::dvbs2::timing::tests::{Lcg, shape};

    const SPS: f64 = 2.5;
    const ROLLOFF: f64 = 0.2;

    /// Complete scrambled PLFRAMEs with pilots and LDPC-encoded random data (symbols).
    fn synthetic_frames(code: u8, frames: usize) -> Vec<Complex<f32>> {
        let pls = Pls::from_code(code);
        let (modulation, rate) = pls.modulation_and_rate().unwrap();
        let params = FecParams::new(rate, pls.short_frame).unwrap();
        let ldpc = params.ldpc();
        let map = Mapping::new(modulation, rate).unwrap();
        let bits = map.bits_per_symbol;
        let body_len = pls.frame_len().unwrap() - HEADER_LEN;
        let scrambling = scrambling_sequence(0, body_len);
        let pilot = Complex::new(std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2);
        let mut rng = Lcg(code as u64 + 1);
        let mut out = Vec::new();
        for _ in 0..frames {
            let info: Vec<u8> = (0..ldpc.k).map(|_| rng.next_bit()).collect();
            let word = ldpc.encode(&info);
            let n = word.len();
            let mut data = (0..n / bits).map(|i| {
                let index = (0..bits).fold(0, |acc, b| (acc << 1) | word[map.codeword_position(n, i, b)] as usize);
                map.points[index]
            });
            out.extend_from_slice(&header_symbols(code));
            for (pos, &k) in scrambling.iter().enumerate() {
                let z = if is_pilot(pos, pls.pilot_blocks()) { pilot } else { data.next().unwrap() };
                out.push(z * Complex::new(0.0, 1.0f32).powu(k as u32));
            }
            assert!(data.next().is_none());
        }
        out
    }

    /// Shapes the symbols with RRC and passes them through the receiver with `snr_db` noise.
    fn decode(symbols: &[Complex<f32>], snr_db: f64) -> ReceiverStats {
        let mut x = shape(symbols, SPS, ROLLOFF, 0.3);
        let sigma = (10f64.powf(-snr_db / 10.0) / SPS / 2.0).sqrt() as f32;
        let mut rng = Lcg(99);
        let mut uniform = || {
            let bits: u32 = (0..24).fold(0, |a, _| (a << 1) | rng.next_bit() as u32);
            (bits as f32 + 0.5) / (1u32 << 24) as f32
        };
        for z in &mut x {
            let (u1, u2) = (uniform(), uniform());
            let r = (-2.0 * u1.ln()).sqrt() * sigma;
            *z += Complex::from_polar(r, std::f32::consts::TAU * u2);
        }
        let mut rx = Receiver::new(ReceiverConfig { sample_rate: SPS * 1e6, offset_hz: 0.0, symbol_rate: 1e6, rolloff: ROLLOFF });
        let mut ts = Vec::new();
        for chunk in x.chunks(50_000) {
            rx.process(chunk, &mut ts);
        }
        rx.finish(&mut ts);
        println!(
            "{} frames decoded, {} failed, SNR {:.1} dB, read {:?}",
            rx.stats.frames_ok,
            rx.stats.frames_failed,
            rx.stats.snr_db,
            rx.stats.pls.map(|p| p.describe())
        );
        rx.stats.clone()
    }

    #[test]
    fn decodes_synthetic_8psk_with_pilots() {
        // 8PSK 2/3, normal frame, with pilots.
        let s = decode(&synthetic_frames((13 << 2) | 1, 4), 2.0);
        assert!(s.frames_ok >= 2 && s.frames_failed == 0, "{} ✓ {} ✗", s.frames_ok, s.frames_failed);
    }

    #[test]
    fn decodes_synthetic_8psk_rate_3_5() {
        // 8PSK 3/5: the bit interleaver columns are read in reverse order.
        let s = decode(&synthetic_frames(12 << 2, 4), 1.0);
        assert!(s.frames_ok >= 2 && s.frames_failed == 0, "{} ✓ {} ✗", s.frames_ok, s.frames_failed);
    }

    /// Writes the synthetic signal as SigMF (for inspection with qoda-analyze):
    /// `QODA_SYNTH_OUT=path QODA_SYNTH_CODE=17 QODA_SYNTH_SNR=-1 cargo test --lib write_synthetic -- --ignored`
    #[test]
    #[ignore]
    fn write_synthetic_sigmf() {
        let Some(out) = std::env::var_os("QODA_SYNTH_OUT") else { return };
        let code: u8 = std::env::var("QODA_SYNTH_CODE").ok().and_then(|v| v.parse().ok()).unwrap_or((4 << 2) | 1);
        let snr: f64 = std::env::var("QODA_SYNTH_SNR").ok().and_then(|v| v.parse().ok()).unwrap_or(-1.0);
        let mut x = shape(&synthetic_frames(code, 8), SPS, ROLLOFF, 0.3);
        let sigma = (10f64.powf(-snr / 10.0) / SPS / 2.0).sqrt() as f32;
        let mut rng = Lcg(99);
        let mut uniform = || {
            let bits: u32 = (0..24).fold(0, |a, _| (a << 1) | rng.next_bit() as u32);
            (bits as f32 + 0.5) / (1u32 << 24) as f32
        };
        for z in &mut x {
            let (u1, u2) = (uniform(), uniform());
            *z += Complex::from_polar((-2.0 * u1.ln()).sqrt() * sigma, std::f32::consts::TAU * u2);
        }
        let path = std::path::PathBuf::from(out);
        let bytes: Vec<u8> = x.iter().flat_map(|z| [z.re.to_le_bytes(), z.im.to_le_bytes()].concat()).collect();
        std::fs::write(path.with_extension("sigmf-data"), bytes).unwrap();
        std::fs::write(
            path.with_extension("sigmf-meta"),
            format!(r#"{{"global":{{"core:datatype":"cf32_le","core:sample_rate":{}}},"captures":[{{"core:frequency":0}}]}}"#, SPS * 1e6),
        )
        .unwrap();
    }

    #[test]
    fn decodes_synthetic_16apsk() {
        // 16APSK 3/4, short frame, with pilots.
        let s = decode(&synthetic_frames((19 << 2) | 3, 14), 5.5);
        assert!(s.frames_ok >= 4 && s.frames_failed == 0, "{} ✓ {} ✗", s.frames_ok, s.frames_failed);
    }
}
