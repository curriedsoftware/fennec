//! Ogg Opus streams (RFC 7845), the format of Telegram voice notes.
//!
//! [`OggOpus`] wraps Opus packets, encoded elsewhere, into Ogg pages. Each
//! page is returned as soon as it is complete, so a recording can be kept as
//! a list of finished pages.

use alloc::vec::Vec;

/// Ogg counts Opus positions at 48 kHz, whatever the input sample rate.
const GRANULE_RATE: u32 = 48_000;
/// Packets per page: one second of 20 ms packets keeps the per-page overhead
/// small without holding much back.
const PACKETS_PER_PAGE: usize = 50;
const MAX_SEGMENTS: usize = 255;

const FLAG_BOS: u8 = 0x02;
const FLAG_EOS: u8 = 0x04;

pub struct OggOpus {
    serial: u32,
    sequence: u32,
    pre_skip: u16,
    sample_rate: u32,
    /// Input samples covered by the packets written so far.
    samples: u64,
    /// The page being filled: lacing values and packet data.
    segments: Vec<u8>,
    data: Vec<u8>,
    packets: usize,
}

impl OggOpus {
    /// A mono stream of `sample_rate` audio whose decoder must skip
    /// `pre_skip` samples (at 48 kHz) at the start. `serial` tells streams
    /// apart; any value works for a file with one stream.
    pub fn new(serial: u32, sample_rate: u32, pre_skip: u16) -> Self {
        Self {
            serial,
            sequence: 0,
            pre_skip,
            sample_rate,
            samples: 0,
            segments: Vec::new(),
            data: Vec::new(),
            packets: 0,
        }
    }

    /// The two header pages (`OpusHead` and `OpusTags`) that start the file.
    pub fn headers(&mut self, vendor: &str) -> Vec<u8> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(1); // channels
        head.extend_from_slice(&self.pre_skip.to_le_bytes());
        head.extend_from_slice(&self.sample_rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // mapping family: mono/stereo

        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments

        let mut out = Vec::new();
        self.page(&mut out, FLAG_BOS, 0, &lacing(head.len()), &head);
        self.page(&mut out, 0, 0, &lacing(tags.len()), &tags);
        out
    }

    /// Adds one packet holding `samples` input samples. Returns a finished
    /// page when this packet completed one.
    pub fn push(&mut self, packet: &[u8], samples: usize) -> Option<Vec<u8>> {
        let lacing = lacing(packet.len());
        let page = if self.segments.len() + lacing.len() > MAX_SEGMENTS {
            Some(self.flush(0))
        } else {
            None
        };
        self.segments.extend_from_slice(&lacing);
        self.data.extend_from_slice(packet);
        self.packets += 1;
        self.samples += samples as u64;
        if page.is_some() {
            return page;
        }
        (self.packets >= PACKETS_PER_PAGE).then(|| self.flush(0))
    }

    /// The last page, which marks the end of the stream. `total_samples`
    /// (input samples actually recorded) lets players drop the padding of the
    /// last packet.
    pub fn finish(mut self, total_samples: usize) -> Vec<u8> {
        self.samples = self.samples.min(total_samples as u64);
        self.flush(FLAG_EOS)
    }

    fn flush(&mut self, flags: u8) -> Vec<u8> {
        let granule = self.pre_skip as u64
            + self.samples * GRANULE_RATE as u64 / self.sample_rate as u64;
        let segments = core::mem::take(&mut self.segments);
        let data = core::mem::take(&mut self.data);
        self.packets = 0;
        let mut out = Vec::with_capacity(27 + segments.len() + data.len());
        self.page(&mut out, flags, granule, &segments, &data);
        out
    }

    fn page(&mut self, out: &mut Vec<u8>, flags: u8, granule: u64, segments: &[u8], data: &[u8]) {
        let start = out.len();
        out.extend_from_slice(b"OggS");
        out.push(0); // version
        out.push(flags);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // CRC, filled in below
        out.push(segments.len() as u8);
        out.extend_from_slice(segments);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
    }
}

/// Lacing values for one packet of `len` bytes.
fn lacing(len: usize) -> Vec<u8> {
    let mut out = alloc::vec![255; len / 255];
    out.push((len % 255) as u8);
    out
}

/// The CRC Ogg uses: polynomial 0x04c11db7, no reflection, no final XOR.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits a stream into (flags, granule, sequence, segments, data).
    fn pages(mut bytes: &[u8]) -> Vec<(u8, u64, u32, Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            assert_eq!(&bytes[..4], b"OggS");
            let count = bytes[26] as usize;
            let segments = bytes[27..27 + count].to_vec();
            let len = 27 + count + segments.iter().map(|&s| s as usize).sum::<usize>();
            let mut page = bytes[..len].to_vec();
            let crc = u32::from_le_bytes(page[22..26].try_into().unwrap());
            page[22..26].fill(0);
            assert_eq!(crc, crc32(&page), "bad CRC");
            out.push((
                bytes[5],
                u64::from_le_bytes(bytes[6..14].try_into().unwrap()),
                u32::from_le_bytes(bytes[18..22].try_into().unwrap()),
                segments,
                bytes[27 + count..len].to_vec(),
            ));
            bytes = &bytes[len..];
        }
        out
    }

    #[test]
    fn crc_matches_reference() {
        // CRC-32 with Ogg's parameters, check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0x89a1_897f);
    }

    #[test]
    fn writes_a_valid_stream() {
        let mut ogg = OggOpus::new(7, 16_000, 312);
        let mut stream = ogg.headers("fennec");
        let packet = [0xaa; 300]; // needs two lacing values
        for _ in 0..120 {
            if let Some(page) = ogg.push(&packet, 320) {
                stream.extend_from_slice(&page);
            }
        }
        stream.extend_from_slice(&ogg.finish(120 * 320 - 100));

        let pages = pages(&stream);
        let (flags, granule, sequence, _, head) = &pages[0];
        assert_eq!((*flags, *granule, *sequence), (FLAG_BOS, 0, 0));
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(u16::from_le_bytes([head[10], head[11]]), 312);
        assert_eq!(&pages[1].4[..8], b"OpusTags");

        let audio = &pages[2..];
        assert_eq!(audio.len(), 3); // 50 + 50 + 20 packets
        for (i, page) in audio.iter().enumerate() {
            assert_eq!(page.2, i as u32 + 2, "page sequence");
        }
        assert_eq!(audio[0].1, 312 + 50 * 960);
        assert_eq!(audio[0].3.len(), 100);
        let (flags, granule, ..) = audio.last().unwrap();
        assert_eq!(*flags, FLAG_EOS);
        assert_eq!(*granule, 312 + (120 * 320 - 100) as u64 * 3);
        let data: usize = audio.iter().map(|p| p.4.len()).sum();
        assert_eq!(data, 120 * 300);
    }

    #[test]
    fn splits_pages_at_255_segments() {
        let mut ogg = OggOpus::new(1, 16_000, 0);
        // 2000-byte packets take 8 lacing values: 31 fit in one page, the
        // 32nd starts the next.
        let pages: Vec<_> = (0..32).filter_map(|_| ogg.push(&[0; 2000], 320)).collect();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0][26], 31 * 8);
        assert_eq!(pages[0].len(), 27 + 31 * 8 + 31 * 2000);
    }
}
