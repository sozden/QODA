//! Extracting service information from the DVB tables in an MPEG-TS.
//!
//! ISO/IEC 13818-1 (PAT, PMT) and ETSI EN 300 468 (SDT, service descriptor):
//! the PAT gives the PMT PID from the program number, the PMT the types of the
//! video/audio streams, and the SDT the service name and provider.

use std::collections::HashMap;

const PID_PAT: u16 = 0x0000;
const PID_SDT: u16 = 0x0011;
const TABLE_PAT: u8 = 0x00;
const TABLE_PMT: u8 = 0x02;
/// SDT for this transport stream ("actual").
const TABLE_SDT: u8 = 0x42;
const SERVICE_DESCRIPTOR: u8 = 0x48;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceInfo {
    pub name: Option<String>,
    pub provider: Option<String>,
    /// Elementary streams in the PMT: (PID, stream type code).
    pub streams: Vec<(u16, u8)>,
}

impl ServiceInfo {
    /// Codec name and kind of the recognized streams, in PMT order.
    pub fn stream_list(&self) -> Vec<(&'static str, StreamKind)> {
        self.streams.iter().filter_map(|&(_, t)| stream_type(t)).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Data,
}

/// ISO/IEC 13818-1 and common DVB stream type codes: (codec name, kind).
pub fn stream_type(code: u8) -> Option<(&'static str, StreamKind)> {
    use StreamKind::*;
    Some(match code {
        0x01 => ("MPEG-1", Video),
        0x02 => ("MPEG-2", Video),
        0x03 => ("MP2", Audio),
        0x04 => ("MPEG-2", Audio),
        0x0F => ("AAC", Audio),
        0x11 => ("AAC-LATM", Audio),
        0x1B => ("H.264", Video),
        0x24 => ("H.265", Video),
        0x81 => ("AC-3", Audio),
        0x06 => ("PES", Data),
        _ => return None,
    })
}

/// Follows TS packets and rebuilds PSI/SI sections.
#[derive(Default)]
pub struct TsInfo {
    /// Partial section per PID.
    sections: HashMap<u16, Vec<u8>>,
    pmt_pid: Option<u16>,
    info: ServiceInfo,
}

impl TsInfo {
    pub fn info(&self) -> &ServiceInfo {
        &self.info
    }

    /// Processes 188-byte TS packets.
    pub fn push(&mut self, packets: &[u8]) {
        for p in packets.chunks_exact(188) {
            let pid = (((p[1] & 0x1F) as u16) << 8) | p[2] as u16;
            if pid != PID_PAT && pid != PID_SDT && Some(pid) != self.pmt_pid {
                continue;
            }
            let afc = (p[3] >> 4) & 3;
            if afc & 1 == 0 {
                continue;
            }
            let mut payload = &p[4..];
            if afc & 2 != 0 {
                let len = payload[0] as usize + 1;
                if len >= payload.len() {
                    continue;
                }
                payload = &payload[len..];
            }
            if p[1] & 0x40 != 0 {
                // A new section starts; the pointer field skips the continuation of the previous section.
                let pointer = payload[0] as usize;
                if let Some(prev) = self.sections.get_mut(&pid) {
                    prev.extend_from_slice(payload.get(1..1 + pointer).unwrap_or(&[]));
                }
                self.try_complete(pid);
                let start = 1 + pointer;
                if start < payload.len() {
                    self.sections.insert(pid, payload[start..].to_vec());
                }
            } else if let Some(buf) = self.sections.get_mut(&pid) {
                buf.extend_from_slice(payload);
            }
            self.try_complete(pid);
        }
    }

    fn try_complete(&mut self, pid: u16) {
        let Some(buf) = self.sections.get(&pid) else { return };
        if buf.len() < 3 {
            return;
        }
        let total = 3 + ((((buf[1] & 0x0F) as usize) << 8) | buf[2] as usize);
        if buf.len() < total {
            return;
        }
        let section = self.sections.remove(&pid).unwrap();
        let section = &section[..total];
        if crc32_mpeg(section) != 0 {
            return;
        }
        match section[0] {
            TABLE_PAT if pid == PID_PAT => self.parse_pat(section),
            TABLE_PMT if Some(pid) == self.pmt_pid => self.parse_pmt(section),
            TABLE_SDT if pid == PID_SDT => self.parse_sdt(section),
            _ => {}
        }
    }

    fn parse_pat(&mut self, s: &[u8]) {
        // After the 8-byte header, 4-byte (program, PID) pairs, CRC at the end.
        for entry in s[8..s.len() - 4].chunks_exact(4) {
            let program = u16::from_be_bytes([entry[0], entry[1]]);
            let pid = (((entry[2] & 0x1F) as u16) << 8) | entry[3] as u16;
            if program != 0 {
                self.pmt_pid = Some(pid);
                return;
            }
        }
    }

    fn parse_pmt(&mut self, s: &[u8]) {
        let end = s.len() - 4;
        let info_len = (((s[10] & 0x0F) as usize) << 8) | s[11] as usize;
        let mut i = 12 + info_len;
        let mut streams = Vec::new();
        while i + 5 <= end {
            let stream_type = s[i];
            let pid = (((s[i + 1] & 0x1F) as u16) << 8) | s[i + 2] as u16;
            let es_len = (((s[i + 3] & 0x0F) as usize) << 8) | s[i + 4] as usize;
            streams.push((pid, stream_type));
            i += 5 + es_len;
        }
        self.info.streams = streams;
    }

    fn parse_sdt(&mut self, s: &[u8]) {
        let end = s.len() - 4;
        let mut i = 11;
        while i + 5 <= end {
            let loop_len = (((s[i + 3] & 0x0F) as usize) << 8) | s[i + 4] as usize;
            let mut d = i + 5;
            let d_end = (d + loop_len).min(end);
            while d + 2 <= d_end {
                let (tag, len) = (s[d], s[d + 1] as usize);
                let body = &s[(d + 2).min(d_end)..(d + 2 + len).min(d_end)];
                if tag == SERVICE_DESCRIPTOR && body.len() >= 2 {
                    let provider_len = body[1] as usize;
                    let provider = body.get(2..2 + provider_len);
                    let name = body
                        .get(2 + provider_len)
                        .and_then(|&n| body.get(3 + provider_len..3 + provider_len + n as usize));
                    self.info.provider = provider.map(dvb_text).filter(|t| !t.is_empty());
                    self.info.name = name.map(dvb_text).filter(|t| !t.is_empty());
                    return;
                }
                d += 2 + len;
            }
            i += 5 + loop_len;
        }
    }
}

/// EN 300 468 Annex A text: if the first byte is below 0x20 it selects the character set.
/// 0x15 means UTF-8; others are read as Latin-based sets.
fn dvb_text(bytes: &[u8]) -> String {
    let (utf8, body) = match bytes.first() {
        Some(0x15) => (true, &bytes[1..]),
        Some(0x10) => (false, bytes.get(3..).unwrap_or(&[])),
        Some(&b) if b < 0x20 => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    let text = if utf8 {
        String::from_utf8_lossy(body).into_owned()
    } else {
        // 0x80–0x9F are control codes (emphasis etc.); skipped.
        body.iter().filter(|&&b| !(0x80..=0x9F).contains(&b)).map(|&b| b as char).collect()
    };
    text.trim().to_string()
}

/// MPEG-2 CRC-32 (polynomial 0x04C11DB7, initial value 0xFFFFFFFF). Computed over
/// the whole section including the CRC, the result is 0.
fn crc32_mpeg(data: &[u8]) -> u32 {
    data.iter().fold(0xFFFF_FFFFu32, |mut crc, &b| {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
        crc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds length and CRC to a section.
    fn section(table: u8, body: &[u8]) -> Vec<u8> {
        let len = body.len() + 4;
        let mut s = vec![table, 0xB0 | (len >> 8) as u8, len as u8];
        s.extend_from_slice(body);
        let crc = crc32_mpeg(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    /// Places a section in a single TS packet.
    fn packet(pid: u16, sec: &[u8]) -> Vec<u8> {
        let mut p = vec![0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10, 0x00];
        p.extend_from_slice(sec);
        p.resize(188, 0xFF);
        p
    }

    #[test]
    fn reads_service_name_and_streams() {
        // PAT: program 1 → PMT PID 0x100.
        let pat = section(TABLE_PAT, &[0xAA, 0xAA, 0xC1, 0, 0, 0x00, 0x01, 0xE1, 0x00]);
        // PMT: PCR PID 0x103, H.264 0x101 and MPEG-1 audio 0x102.
        let pmt = section(
            TABLE_PMT,
            &[0, 1, 0xC1, 0, 0, 0xE1, 0x03, 0xF0, 0, 0x1B, 0xE1, 0x01, 0xF0, 0, 0x03, 0xE1, 0x02, 0xF0, 0],
        );
        // SDT: service 1, provider "QARS", name "A71A".
        let desc = [SERVICE_DESCRIPTOR, 11, 0x01, 4, b'Q', b'A', b'R', b'S', 4, b'A', b'7', b'1', b'A'];
        let mut sdt_body = vec![0xAA, 0xAA, 0xC1, 0, 0, 0xFF, 0x01, 0xFF];
        sdt_body.extend_from_slice(&[0x00, 0x01, 0xFC, 0x80, desc.len() as u8]);
        sdt_body.extend_from_slice(&desc);
        let sdt = section(TABLE_SDT, &sdt_body);

        let mut ts = Vec::new();
        ts.extend(packet(PID_PAT, &pat));
        ts.extend(packet(0x100, &pmt));
        ts.extend(packet(PID_SDT, &sdt));
        let mut info = TsInfo::default();
        info.push(&ts);
        let i = info.info();
        assert_eq!(i.name.as_deref(), Some("A71A"));
        assert_eq!(i.provider.as_deref(), Some("QARS"));
        assert_eq!(i.streams, vec![(0x101, 0x1B), (0x102, 0x03)]);
        assert_eq!(i.stream_list(), vec![("H.264", StreamKind::Video), ("MP2", StreamKind::Audio)]);
    }

    #[test]
    fn decodes_dvb_text_encodings() {
        assert_eq!(dvb_text(b"\x15DC3ZB \xC3\xA7"), "DC3ZB ç");
        assert_eq!(dvb_text(b"\x05CT3FM"), "CT3FM");
        assert_eq!(dvb_text(b"plain"), "plain");
    }
}
