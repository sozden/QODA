//! DVB-S2 transmitter: MPEG-TS packets in, PLFRAME symbols and RRC-shaped IQ samples out.
//!
//! The mirror image of the receiver, from ETSI EN 302 307-1: mode adaptation (clause 5.1:
//! each TS packet's sync byte replaced by the CRC-8 of the previous packet, BBHEADER),
//! baseband scrambling (5.2), BCH and LDPC encoding (5.3), bit interleaving and mapping (5.4),
//! PL framing with pilots and PL scrambling (5.5) and RRC shaping (5.6).

use std::collections::VecDeque;

use num_complex::Complex;

use super::bbframe::{BbHeader, bb_descramble, crc8};
use super::fec::{Bch, FecParams};
use super::filters::rrc_pulse;
use super::ldpc::LdpcCode;
use super::modulation::Mapping;
use super::plframe::{is_pilot, scrambling_sequence};
use super::plheader::{HEADER_LEN, Modulation, Pls, header_symbols};

pub const TS_LEN: usize = 188;
/// A null packet (PID 0x1FFF), for filling frames when there is not enough data.
pub const NULL_PACKET: [u8; TS_LEN] = {
    let mut p = [0xFFu8; TS_LEN];
    p[0] = 0x47;
    p[1] = 0x1F;
    p[2] = 0xFF;
    p[3] = 0x10;
    p
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TxConfig {
    /// 1..=11 QPSK, 12..=17 8PSK, 18..=23 16APSK (EN 302 307-1 table 12).
    pub modcod: u8,
    pub short_frame: bool,
    pub pilots: bool,
    /// 0.20, 0.25 or 0.35.
    pub rolloff: f32,
}

impl TxConfig {
    pub fn pls(&self) -> Pls {
        Pls { modcod: self.modcod, short_frame: self.short_frame, pilots: self.pilots }
    }

    /// PLFRAME length in symbols.
    pub fn frame_len(&self) -> Option<usize> {
        self.pls().frame_len()
    }

    /// Data field of a BBFRAME (bytes): the BBFRAME minus its 10-byte header.
    pub fn data_field_bytes(&self) -> Option<usize> {
        let (_, rate) = self.pls().modulation_and_rate()?;
        Some(FecParams::new(rate, self.short_frame)?.k_bch / 8 - BbHeader::LEN)
    }

    /// MPEG-TS bit rate the channel carries at `symbol_rate` (bit/s).
    pub fn ts_bitrate(&self, symbol_rate: f64) -> Option<f64> {
        Some(symbol_rate * (self.data_field_bytes()? * 8) as f64 / self.frame_len()? as f64)
    }
}

/// Turns MPEG-TS packets into PLFRAMEs (complex symbols at one sample per symbol).
pub struct Modulator {
    cfg: TxConfig,
    params: FecParams,
    ldpc: LdpcCode,
    bch: Bch,
    mapping: Mapping,
    header: [Complex<f32>; HEADER_LEN],
    scrambling: Vec<u8>,
    /// Data waiting for a frame: packets with their sync byte replaced by the previous packet's CRC.
    stream: VecDeque<u8>,
    /// Bytes of the packet at the front of `stream` already sent in earlier frames.
    sent_of_packet: usize,
    last_crc: u8,
}

impl Modulator {
    pub fn new(cfg: TxConfig) -> Result<Self, String> {
        let pls = cfg.pls();
        let (modulation, rate) = pls.modulation_and_rate().ok_or(format!("MODCOD {} is not valid", cfg.modcod))?;
        if modulation == Modulation::Apsk32 {
            return Err("32APSK is not supported".into());
        }
        let params = FecParams::new(rate, cfg.short_frame).ok_or("this code rate has no short frame")?;
        let mapping = Mapping::new(modulation, rate).ok_or("unsupported modulation")?;
        let code = (cfg.modcod << 2) | ((cfg.short_frame as u8) << 1) | cfg.pilots as u8;
        let body_len = pls.frame_len().unwrap() - HEADER_LEN;
        Ok(Self {
            ldpc: params.ldpc(),
            bch: params.bch(),
            params,
            mapping,
            header: header_symbols(code),
            scrambling: scrambling_sequence(0, body_len),
            stream: VecDeque::new(),
            sent_of_packet: 0,
            last_crc: 0,
            cfg,
        })
    }

    pub fn config(&self) -> &TxConfig {
        &self.cfg
    }

    /// Queues whole 188-byte TS packets (each starting with 0x47).
    pub fn push_ts(&mut self, packets: &[u8]) {
        for p in packets.chunks_exact(TS_LEN) {
            self.stream.push_back(self.last_crc);
            self.stream.extend(&p[1..]);
            self.last_crc = crc8(&p[1..]);
        }
    }

    /// Bytes queued and not yet framed.
    pub fn queued(&self) -> usize {
        self.stream.len()
    }

    /// The next PLFRAME if a whole data field is queued.
    pub fn next_frame(&mut self) -> Option<Vec<Complex<f32>>> {
        let dfl = self.params.k_bch / 8 - BbHeader::LEN;
        if self.stream.len() < dfl {
            return None;
        }
        // SYNCD: bits from the start of the data field to the first packet that starts in it.
        let to_next = (TS_LEN - self.sent_of_packet) % TS_LEN;
        let syncd = if to_next >= dfl { 0xFFFF } else { (to_next * 8) as u16 };
        let mut frame = Vec::with_capacity(self.params.k_bch / 8);
        frame.extend(self.bbheader(dfl, syncd));
        frame.extend(self.stream.drain(..dfl));
        self.sent_of_packet = (self.sent_of_packet + dfl) % TS_LEN;
        Some(self.encode(frame))
    }

    fn bbheader(&self, dfl: usize, syncd: u16) -> [u8; BbHeader::LEN] {
        // MATYPE-1: TS, single input stream, CCM, no ISSY, no null packet deletion, roll-off.
        let ro = match self.cfg.rolloff {
            r if r > 0.3 => 0b00,
            r if r > 0.22 => 0b01,
            _ => 0b10,
        };
        let upl = ((TS_LEN * 8) as u16).to_be_bytes();
        let dfl = ((dfl * 8) as u16).to_be_bytes();
        let syncd = syncd.to_be_bytes();
        let mut h = [0b1111_0000 | ro, 0, upl[0], upl[1], dfl[0], dfl[1], 0x47, syncd[0], syncd[1], 0];
        h[9] = crc8(&h[..9]);
        h
    }

    /// BBFRAME bytes → scrambled, BCH and LDPC encoded, mapped PLFRAME.
    fn encode(&self, mut bbframe: Vec<u8>) -> Vec<Complex<f32>> {
        bb_descramble(&mut bbframe);
        let mut bits: Vec<u8> = bbframe.iter().flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1)).collect();
        bits.extend(self.bch.parity(&bits));
        let word = self.ldpc.encode(&bits);
        let n = word.len();
        let m = self.mapping.bits_per_symbol;
        let mut data = (0..n / m).map(|i| {
            let index = (0..m).fold(0, |acc, b| (acc << 1) | word[self.mapping.codeword_position(n, i, b)] as usize);
            self.mapping.points[index]
        });
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let pilot = Complex::new(s, s);
        let pilot_blocks = self.cfg.pls().pilot_blocks();
        let mut out = Vec::with_capacity(HEADER_LEN + self.scrambling.len());
        out.extend_from_slice(&self.header);
        for (pos, &k) in self.scrambling.iter().enumerate() {
            let z = if is_pilot(pos, pilot_blocks) { pilot } else { data.next().unwrap() };
            // PL scrambling: multiply by j^k.
            out.push(match k {
                0 => z,
                1 => Complex::new(-z.im, z.re),
                2 => -z,
                _ => Complex::new(z.im, -z.re),
            });
        }
        out
    }
}

/// RRC pulse shaping by an integer number of samples per symbol (streaming).
pub struct Shaper {
    sps: usize,
    /// Polyphase taps: `phases[p][k]` weights the symbol `k` steps back for output phase `p`.
    phases: Vec<Vec<f32>>,
    history: VecDeque<Complex<f32>>,
}

/// Symbols on each side of the pulse centre.
const SHAPER_SPAN: usize = 10;

impl Shaper {
    /// Output power is about 1 for unit-energy constellations.
    pub fn new(sps: usize, rolloff: f32) -> Self {
        let len = 2 * SHAPER_SPAN + 1;
        let phases: Vec<Vec<f32>> = (0..sps)
            .map(|p| {
                (0..len)
                    .map(|k| rrc_pulse(k as f64 - SHAPER_SPAN as f64 + p as f64 / sps as f64, rolloff as f64) as f32)
                    .collect()
            })
            .collect();
        Self { sps, phases, history: VecDeque::from(vec![Complex::default(); len]) }
    }

    pub fn samples_per_symbol(&self) -> usize {
        self.sps
    }

    /// Appends `sps` samples per input symbol to `out`. The output is delayed by `SHAPER_SPAN` symbols.
    pub fn process(&mut self, symbols: &[Complex<f32>], out: &mut Vec<Complex<f32>>) {
        out.reserve(symbols.len() * self.sps);
        for &s in symbols {
            self.history.pop_front();
            self.history.push_back(s);
            for taps in &self.phases {
                // history[k] is the symbol (len-1-k) steps back; taps[j] weights the one j steps back.
                let acc = self.history.iter().rev().zip(taps).fold(Complex::default(), |a, (z, &t)| a + z * t);
                out.push(acc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvbs2::receiver::{Receiver, ReceiverConfig};

    fn test_packets(count: usize) -> Vec<u8> {
        (0..count)
            .flat_map(|i| {
                let mut p = vec![0x47, 0x01, 0x00, 0x10 | (i % 16) as u8];
                p.extend((4..TS_LEN).map(|j| (i * 7 + j) as u8));
                p
            })
            .collect()
    }

    #[test]
    fn bbheader_and_capacity() {
        let cfg = TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 };
        // QPSK 1/2 normal: K_bch 32208 bits → 4016 data bytes in 33282 symbols.
        assert_eq!(cfg.data_field_bytes(), Some(4016));
        let rate = cfg.ts_bitrate(333e3).unwrap();
        assert!((rate - 333e3 * 32128.0 / 33282.0).abs() < 1.0, "{rate}");
        let m = Modulator::new(cfg).unwrap();
        let h = BbHeader::parse(&m.bbheader(4016, 8)).unwrap();
        assert!(h.is_transport_stream());
        assert_eq!((h.upl, h.dfl, h.sync, h.syncd), (1504, 32128, 0x47, 8));
    }

    /// Loopback: packets → modulator → shaper → QODA's receiver → the same packets.
    #[test]
    fn receiver_decodes_modulator_output() {
        for cfg in [
            TxConfig { modcod: 4, short_frame: false, pilots: true, rolloff: 0.2 },
            TxConfig { modcod: 13, short_frame: true, pilots: false, rolloff: 0.35 },
        ] {
            let mut m = Modulator::new(cfg).unwrap();
            // Enough frames for the receiver's 40000-symbol search window plus a few to decode.
            let frames = 6.max(60_000 / cfg.frame_len().unwrap());
            let packets = test_packets(m.config().data_field_bytes().unwrap() * frames / TS_LEN);
            m.push_ts(&packets);
            let mut symbols = Vec::new();
            while let Some(f) = m.next_frame() {
                symbols.extend(f);
            }
            let sps = 4;
            let mut shaper = Shaper::new(sps, cfg.rolloff);
            let mut x = Vec::new();
            shaper.process(&symbols, &mut x);
            let mut rx = Receiver::new(ReceiverConfig { sample_rate: sps as f64 * 1e6, offset_hz: 0.0, symbol_rate: 1e6, rolloff: cfg.rolloff as f64 });
            let mut ts = Vec::new();
            for chunk in x.chunks(50_000) {
                rx.process(chunk, &mut ts);
            }
            rx.finish(&mut ts);
            assert_eq!(rx.stats.frames_failed, 0, "{cfg:?}");
            assert!(rx.stats.frames_ok >= 3, "{cfg:?}: {} frames", rx.stats.frames_ok);
            assert_eq!(rx.stats.ts_crc_errors, 0, "{cfg:?}");
            // The receiver skips the frames it locks on with; what it outputs must be a run of the input.
            assert!(ts.len() >= 3 * TS_LEN, "{cfg:?}");
            let first = ts[..TS_LEN].to_vec();
            let at = packets.chunks(TS_LEN).position(|p| p == first.as_slice()).expect("first packet not in the input");
            assert_eq!(&packets[at * TS_LEN..at * TS_LEN + ts.len()], ts.as_slice(), "{cfg:?}");
        }
    }
}
