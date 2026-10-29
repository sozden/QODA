//! BBFRAME: temel bant karıştırmasını çözme, BBHEADER ve MPEG-TS çıkarma.
//!
//! ETSI EN 302 307-1, bölüm 5.1–5.2: BBFRAME, 80 bitlik BBHEADER ve ardından
//! kullanıcı paketlerini taşıyan veri alanından oluşur ve 1 + x^14 + x^15
//! dizisiyle karıştırılır. TS akışlarında (normal mod) her paketin 0x47 senkron
//! baytı çıkarılır ve yerine bir önceki paketin CRC-8'i konur.

/// Bayt dizisini temel bant karıştırma dizisiyle XOR'lar (karıştırma ve çözme aynıdır).
pub fn bb_descramble(bytes: &mut [u8]) {
    // Kayıt hücreleri 1..15, başlangıç 100101010000000.
    let mut reg: u16 = 0b100_1010_1000_0000;
    for byte in bytes.iter_mut() {
        let mut mask = 0u8;
        for bit in (0..8).rev() {
            let out = ((reg >> 1) ^ reg) as u8 & 1; // hücre 14 ⊕ hücre 15
            reg = (reg >> 1) | ((out as u16) << 14);
            mask |= out << bit;
        }
        *byte ^= mask;
    }
}

/// DVB-S2 CRC-8: x^8 + x^7 + x^6 + x^4 + x^2 + 1, en anlamlı bit önce, başlangıç 0.
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
    /// Kullanıcı paketi uzunluğu (bit); TS için 1504.
    pub upl: u16,
    /// Veri alanı uzunluğu (bit).
    pub dfl: u16,
    pub sync: u8,
    /// Veri alanı başından ilk kullanıcı paketine uzaklık (bit).
    pub syncd: u16,
}

impl BbHeader {
    pub const LEN: usize = 10;

    /// CRC doğruysa başlığı ayrıştırır.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let b = bytes.get(..Self::LEN)?;
        if crc8(&b[..9]) != b[9] {
            return None;
        }
        let word = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        Some(Self { matype: word(0), upl: word(2), dfl: word(4), sync: b[6], syncd: word(7) })
    }

    /// MATYPE'ın TS/GS alanı 11 ise akış MPEG-TS'dir.
    pub fn is_transport_stream(&self) -> bool {
        self.matype >> 14 == 0b11
    }
}

/// Ardışık BBFRAME'lerden MPEG-TS paketlerini yeniden kurar.
#[derive(Default)]
pub struct TsDepacketizer {
    /// Önceki çerçeveden kalan yarım paket.
    partial: Vec<u8>,
    pub packets: usize,
    pub crc_errors: usize,
}

const TS_LEN: usize = 188;

impl TsDepacketizer {
    /// Karıştırması çözülmüş bir BBFRAME'i (bayt olarak) işler, tam paketleri `out`a ekler.
    /// Başlık geçersizse veya akış TS değilse `None` döner.
    pub fn push(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Option<BbHeader> {
        let header = BbHeader::parse(frame)?;
        if !header.is_transport_stream() || header.upl != (TS_LEN * 8) as u16 {
            return None;
        }
        let dfl = header.dfl as usize / 8;
        let data = frame.get(BbHeader::LEN..BbHeader::LEN + dfl)?;
        let syncd = header.syncd as usize / 8;
        if header.syncd == 0xFFFF || syncd > data.len() {
            // Bu çerçevede paket başlangıcı yok; hepsi önceki paketin devamı.
            self.partial.extend_from_slice(data);
            return Some(header);
        }
        // SYNCD öncesi: önceki çerçeveden kalan paketin sonu.
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

    /// `packet[0]` CRC yeri; `rest` paketten sonraki baytlar (ilk baytı bu paketin CRC'si).
    fn emit(&mut self, packet: &[u8], rest: &[u8], out: &mut Vec<u8>) {
        // Bu paketin CRC'si bir sonraki paketin ilk baytında; varsa denetle.
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
        // İki paketlik bir akış: her paketin yerine önceki paketin CRC'si konur.
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
        // Akışı paket ortasından ikiye böl.
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
        // İkinci çerçevede ilk paket başlangıcı 2·188 baytta.
        d.push(&frame(&stream[cut..], ((2 * 188 - cut) * 8) as u16), &mut out).unwrap();
        assert_eq!(d.packets, 3);
        assert_eq!(d.crc_errors, 0);
        assert_eq!(out, packets.concat());
    }
}
