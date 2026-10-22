//! IMA ADPCM, as stored in WAV files (format tag `0x0011`).
//!
//! The Atom Echo has no PSRAM, so a recording is compressed 4:1 while it is
//! made: 12 s of 16 kHz audio take 96 KB instead of 384 KB. ffmpeg, sox and
//! most players read these files directly.

use alloc::vec::Vec;

/// Bytes per ADPCM block (mono).
pub const BLOCK_ALIGN: usize = 512;
/// Samples per block: one in the header plus two per remaining byte.
pub const SAMPLES_PER_BLOCK: usize = 1 + (BLOCK_ALIGN - 4) * 2;
/// Size of the header written by [`wav_header`].
pub const WAV_HEADER_LEN: usize = 60;

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

const INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

#[derive(Clone, Copy, Default)]
struct State {
    predictor: i32,
    index: i32,
}

impl State {
    fn encode(&mut self, sample: i16) -> u8 {
        let step = STEPS[self.index as usize];
        let mut diff = sample as i32 - self.predictor;
        let mut nibble = 0u8;
        if diff < 0 {
            nibble = 8;
            diff = -diff;
        }
        let mut delta = step >> 3;
        let mut step = step;
        for bit in [4u8, 2, 1] {
            if diff >= step {
                nibble |= bit;
                diff -= step;
                delta += step;
            }
            step >>= 1;
        }
        self.apply(nibble, delta);
        nibble
    }

    fn decode(&mut self, nibble: u8) -> i16 {
        let step = STEPS[self.index as usize];
        let mut delta = step >> 3;
        if nibble & 4 != 0 {
            delta += step;
        }
        if nibble & 2 != 0 {
            delta += step >> 1;
        }
        if nibble & 1 != 0 {
            delta += step >> 2;
        }
        self.apply(nibble, delta);
        self.predictor as i16
    }

    fn apply(&mut self, nibble: u8, delta: i32) {
        if nibble & 8 != 0 {
            self.predictor -= delta;
        } else {
            self.predictor += delta;
        }
        self.predictor = self.predictor.clamp(i16::MIN as i32, i16::MAX as i32);
        self.index = (self.index + INDEX_ADJUST[(nibble & 7) as usize]).clamp(0, 88);
    }
}

/// Encodes samples into fixed-size ADPCM blocks.
pub struct Encoder {
    state: State,
    block: [u8; BLOCK_ALIGN],
    /// Samples written into `block`.
    in_block: usize,
    total: usize,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            state: State::default(),
            block: [0; BLOCK_ALIGN],
            in_block: 0,
            total: 0,
        }
    }

    /// Samples encoded so far, not counting padding.
    pub fn total_samples(&self) -> usize {
        self.total
    }

    /// Encodes one sample, returning a block once it is complete.
    pub fn push(&mut self, sample: i16) -> Option<&[u8; BLOCK_ALIGN]> {
        self.total += 1;
        self.push_raw(sample)
    }

    fn push_raw(&mut self, sample: i16) -> Option<&[u8; BLOCK_ALIGN]> {
        if self.in_block == 0 {
            // Each block restarts from a literal sample.
            self.state.predictor = sample as i32;
            self.block[..2].copy_from_slice(&sample.to_le_bytes());
            self.block[2] = self.state.index as u8;
            self.block[3] = 0;
        } else {
            let nibble = self.state.encode(sample);
            let pos = self.in_block - 1;
            let byte = &mut self.block[4 + pos / 2];
            if pos.is_multiple_of(2) {
                *byte = nibble;
            } else {
                *byte |= nibble << 4;
            }
        }
        self.in_block += 1;
        if self.in_block == SAMPLES_PER_BLOCK {
            self.in_block = 0;
            Some(&self.block)
        } else {
            None
        }
    }

    /// Pads the current block with silence and returns it, if it holds any
    /// samples.
    pub fn finish(&mut self) -> Option<&[u8; BLOCK_ALIGN]> {
        if self.in_block == 0 {
            return None;
        }
        while self.in_block != 0 {
            self.push_raw(0);
        }
        Some(&self.block)
    }
}

/// WAV header for `blocks` ADPCM blocks holding `samples` samples.
pub fn wav_header(sample_rate: u32, samples: usize, blocks: usize) -> [u8; WAV_HEADER_LEN] {
    let data_len = (blocks * BLOCK_ALIGN) as u32;
    let byte_rate = (sample_rate as u64 * BLOCK_ALIGN as u64 / SAMPLES_PER_BLOCK as u64) as u32;
    let mut h = [0u8; WAV_HEADER_LEN];
    let mut pos = 0;
    let mut put = |bytes: &[u8]| {
        h[pos..pos + bytes.len()].copy_from_slice(bytes);
        pos += bytes.len();
    };
    put(b"RIFF");
    put(&(WAV_HEADER_LEN as u32 - 8 + data_len).to_le_bytes());
    put(b"WAVEfmt ");
    put(&20u32.to_le_bytes());
    put(&0x0011u16.to_le_bytes()); // IMA ADPCM
    put(&1u16.to_le_bytes()); // mono
    put(&sample_rate.to_le_bytes());
    put(&byte_rate.to_le_bytes());
    put(&(BLOCK_ALIGN as u16).to_le_bytes());
    put(&4u16.to_le_bytes()); // bits per sample
    put(&2u16.to_le_bytes()); // extra format bytes
    put(&(SAMPLES_PER_BLOCK as u16).to_le_bytes());
    put(b"fact");
    put(&4u32.to_le_bytes());
    put(&(samples as u32).to_le_bytes());
    put(b"data");
    put(&data_len.to_le_bytes());
    h
}

/// Decodes ADPCM blocks back to PCM.
pub fn decode(blocks: &[u8], samples: usize) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples);
    for block in blocks.as_chunks::<BLOCK_ALIGN>().0 {
        let first = i16::from_le_bytes([block[0], block[1]]);
        let mut state = State {
            predictor: first as i32,
            index: (block[2] as i32).clamp(0, 88),
        };
        out.push(first);
        for &byte in &block[4..] {
            out.push(state.decode(byte & 0x0f));
            out.push(state.decode(byte >> 4));
        }
    }
    out.truncate(samples);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_all(samples: &[i16]) -> Vec<u8> {
        let mut encoder = Encoder::new();
        let mut out = Vec::new();
        for &s in samples {
            if let Some(block) = encoder.push(s) {
                out.extend_from_slice(block);
            }
        }
        if let Some(block) = encoder.finish() {
            out.extend_from_slice(block);
        }
        out
    }

    #[test]
    fn roundtrip_preserves_speech_band_audio() {
        let samples: Vec<i16> = (0..5000)
            .map(|n| {
                let t = n as f32 / 16_000.0;
                let x = 0.4 * libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t)
                    + 0.2 * libm::sinf(2.0 * core::f32::consts::PI * 1800.0 * t);
                (x * 32767.0) as i16
            })
            .collect();
        let blocks = encode_all(&samples);
        assert_eq!(
            blocks.len(),
            samples.len().div_ceil(SAMPLES_PER_BLOCK) * BLOCK_ALIGN
        );
        let decoded = decode(&blocks, samples.len());
        assert_eq!(decoded.len(), samples.len());

        let signal: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        let noise: f64 = samples
            .iter()
            .zip(&decoded)
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum();
        let snr = 10.0 * libm::log10(signal / noise);
        assert!(snr > 20.0, "SNR {snr} dB");
    }

    #[test]
    fn header_sizes_are_consistent() {
        let h = wav_header(16_000, 2000, 2);
        assert_eq!(&h[..4], b"RIFF");
        let riff = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff + 8, WAV_HEADER_LEN + 2 * BLOCK_ALIGN);
        assert_eq!(&h[52..56], b"data");
    }
}
