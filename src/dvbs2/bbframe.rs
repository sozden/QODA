//! BBFRAME: baseband descrambling, BBHEADER and MPEG-TS extraction.
//!
//! ETSI EN 302 307-1, clauses 5.1–5.2: a BBFRAME consists of an 80-bit BBHEADER
//! followed by a data field carrying user packets, and is scrambled with the
//! 1 + x^14 + x^15 sequence. In TS streams (normal mode) each packet's 0x47 sync
//! byte is removed and replaced by the CRC-8 of the previous packet.

/// XORs a byte array with the baseband scrambling sequence (scrambling and descrambling are the same).
pub fn bb_descramble(bytes: &mut [u8]) {
    // Register cells 1..15, initial state 100101010000000.
    let mut reg: u16 = 0b100_1010_1000_0000;
    for byte in bytes.iter_mut() {
        let mut mask = 0u8;
        for bit in (0..8).rev() {
            let out = ((reg >> 1) ^ reg) as u8 & 1; // cell 14 ⊕ cell 15
            reg = (reg >> 1) | ((out as u16) << 14);
            mask |= out << bit;
        }
        *byte ^= mask;
    }
}

/// DVB-S2 CRC-8: x^8 + x^7 + x^6 + x^4 + x^2 + 1, most significant bit first, initial value 0.
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut crc, &b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0xD5 } else { crc << 1 };
        }
        crc
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BbHeader {
    pub matype: u16,
    /// User packet length (bits); 1504 for TS.
    pub upl: u16,
    /// Data field length (bits).
    pub dfl: u16,
    pub sync: u8,
    /// Distance from the start of the data field to the first user packet (bits).
    pub syncd: u16,
}

impl BbHeader {
    pub const LEN: usize = 10;

    /// Parses the header if its CRC is correct.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let b = bytes.get(..Self::LEN)?;
        if crc8(&b[..9]) != b[9] {
            return None;
        }
        let word = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        Some(Self { matype: word(0), upl: word(2), dfl: word(4), sync: b[6], syncd: word(7) })
    }

    /// The stream is MPEG-TS if MATYPE's TS/GS field is 11.
    pub fn is_transport_stream(&self) -> bool {
        self.matype >> 14 == 0b11
    }
}

/// Rebuilds MPEG-TS packets from consecutive BBFRAMEs.
#[derive(Default)]
pub struct TsDepacketizer {
    /// Partial packet left over from the previous frame.
    partial: Vec<u8>,
    pub packets: usize,
    pub crc_errors: usize,
}

const TS_LEN: usize = 188;

impl TsDepacketizer {
    /// Processes a descrambled BBFRAME (as bytes) and appends whole packets to `out`.
    /// Returns `None` if the header is invalid or the stream is not TS.
    pub fn push(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Option<BbHeader> {
        let header = BbHeader::parse(frame)?;
        if !header.is_transport_stream() || header.upl != (TS_LEN * 8) as u16 {
            return None;
        }
        let dfl = header.dfl as usize / 8;
        let data = frame.get(BbHeader::LEN..BbHeader::LEN + dfl)?;
        let syncd = header.syncd as usize / 8;
        if header.syncd == 0xFFFF || syncd > data.len() {
            // No packet starts in this frame; all of it continues the previous packet.
            self.partial.extend_from_slice(data);
            return Some(header);
        }
        // Before SYNCD: the end of the packet left over from the previous frame.
        if !self.partial.is_empty() {
            self.partial.extend_from_slice(&data[..syncd]);
            if self.partial.len() == TS_LEN {
                let p = std::mem::take(&mut self.partial);
                self.emit(&p, &data[syncd..], out);
            }
            self.partial.clear();
        }
        let mut pos = syncd;
        while pos + TS_LEN <= data.len() {
            let next = &data[pos + TS_LEN..];
            self.emit(&data[pos..pos + TS_LEN], next, out);
            pos += TS_LEN;
        }
        self.partial.extend_from_slice(&data[pos..]);
        Some(header)
    }

    /// `packet[0]` is the CRC slot; `rest` is the bytes after the packet (its first byte is this packet's CRC).
    fn emit(&mut self, packet: &[u8], rest: &[u8], out: &mut Vec<u8>) {
        // This packet's CRC is in the first byte of the next packet; check it if present.
        if let Some(&crc) = rest.first() {
            if crc8(&packet[1..]) != crc {
                self.crc_errors += 1;
            }
        }
        out.push(0x47);
        out.extend_from_slice(&packet[1..]);
        self.packets += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descramble_is_its_own_inverse() {
        let original: Vec<u8> = (0..200u8).collect();
        let mut data = original.clone();
        bb_descramble(&mut data);
        assert_ne!(data, original);
        bb_descramble(&mut data);
        assert_eq!(data, original);
    }

    #[test]
    fn parses_header_with_valid_crc() {
        let mut h = vec![0xF0, 0x00, 0x05, 0xE0, 0xC9, 0x00, 0x47, 0x00, 0x08];
        h.push(crc8(&h));
        let header = BbHeader::parse(&h).unwrap();
        assert!(header.is_transport_stream());
        assert_eq!(header.upl, 1504);
        assert_eq!(header.syncd, 8);
        h[9] ^= 1;
        assert_eq!(BbHeader::parse(&h), None);
    }

    #[test]
    fn rebuilds_packets_across_frames() {
        // A two-packet stream: each packet's sync byte is replaced by the previous packet's CRC.
        let packets: Vec<Vec<u8>> = (0..3u8).map(|i| {
            let mut p = vec![0x47];
            p.extend((0..187).map(|j| (j as u8).wrapping_mul(i + 1)));
            p
        }).collect();
        let mut stream = Vec::new();
        let mut prev_crc = 0u8;
        for p in &packets {
            stream.push(prev_crc);
            stream.extend_from_slice(&p[1..]);
            prev_crc = crc8(&p[1..]);
        }
        stream.push(prev_crc);
        // Split the stream in two in the middle of a packet.
        let frame = |data: &[u8], syncd_bits: u16| {
            let mut h = vec![0xF0, 0x00, 0x05, 0xE0];
            h.extend_from_slice(&((data.len() * 8) as u16).to_be_bytes());
            h.push(0x47);
            h.extend_from_slice(&syncd_bits.to_be_bytes());
            h.push(crc8(&h));
            h.extend_from_slice(data);
            h
        };
        let cut = 300;
        let mut d = TsDepacketizer::default();
        let mut out = Vec::new();
        d.push(&frame(&stream[..cut], 0), &mut out).unwrap();
        // In the second frame the first packet starts at byte 2·188.
        d.push(&frame(&stream[cut..], ((2 * 188 - cut) * 8) as u16), &mut out).unwrap();
        assert_eq!(d.packets, 3);
        assert_eq!(d.crc_errors, 0);
        assert_eq!(out, packets.concat());
    }
}
