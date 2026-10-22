//! A recording compressed with IMA ADPCM, kept in RAM and uploaded as a WAV
//! file. Used without the `opus` feature; `voice.rs` has the voice note one.

use fennec_core::adpcm::{self, BLOCK_ALIGN, Encoder};
use fennec_core::SAMPLE_RATE;

/// Sent as a document: Telegram shows WAV files as attachments.
#[cfg(feature = "telegram")]
pub const UPLOAD: crate::telegram::Upload = crate::telegram::Upload {
    method: "sendDocument",
    field: "document",
    content_type: "audio/wav",
    extension: "wav",
};

/// ADPCM blocks per allocation (8 KB, about 0.5 s of audio).
const BLOCKS_PER_CHUNK: usize = 16;
const CHUNK_LEN: usize = BLOCKS_PER_CHUNK * BLOCK_ALIGN;

/// Heap that must stay free for Wi-Fi and the TLS connection used to upload
/// the recording.
const HEAP_RESERVE: usize = 64 * 1024;

pub struct Recording {
    encoder: Encoder,
    chunks: Vec<Vec<u8>>,
    blocks: usize,
    full: bool,
}

impl Recording {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            encoder: Encoder::new(),
            chunks: Vec::new(),
            blocks: 0,
            full: false,
        })
    }

    /// Appends samples. Returns `false` once memory runs out; the recording
    /// stays valid but drops further audio.
    pub fn push(&mut self, samples: &[i16]) -> bool {
        for &sample in samples {
            if self.full {
                return false;
            }
            if let Some(block) = self.encoder.push(sample) {
                let block = *block;
                self.store(&block);
            }
        }
        !self.full
    }

    /// The same as [`Recording::push`]: encoding never waits.
    pub fn try_push(&mut self, samples: &[i16]) -> Option<bool> {
        Some(self.push(samples))
    }

    /// Flushes the last, partial block.
    pub fn finish(&mut self) {
        if self.full {
            return;
        }
        if let Some(block) = self.encoder.finish() {
            let block = *block;
            self.store(&block);
        }
    }

    fn store(&mut self, block: &[u8; BLOCK_ALIGN]) {
        if self.chunks.last().is_none_or(|c| c.len() == CHUNK_LEN) {
            let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() } as usize;
            let mut chunk = Vec::new();
            // The pre-roll's memory comes back before the upload.
            let free = free + crate::preroll::held();
            if free < HEAP_RESERVE + CHUNK_LEN || chunk.try_reserve_exact(CHUNK_LEN).is_err() {
                log::warn!("out of memory after {} blocks", self.blocks);
                self.full = true;
                return;
            }
            self.chunks.push(chunk);
        }
        self.chunks.last_mut().unwrap().extend_from_slice(block);
        self.blocks += 1;
    }

    /// Samples recorded, excluding padding.
    pub fn samples(&self) -> usize {
        self.encoder
            .total_samples()
            .min(self.blocks * fennec_core::adpcm::SAMPLES_PER_BLOCK)
    }

    pub fn bytes(&self) -> usize {
        self.blocks * BLOCK_ALIGN
    }

    /// The WAV header that precedes [`Recording::chunks`].
    pub fn header(&self) -> Vec<u8> {
        adpcm::wav_header(SAMPLE_RATE, self.samples(), self.blocks).to_vec()
    }

    pub fn seconds(&self) -> f32 {
        self.samples() as f32 / fennec_core::SAMPLE_RATE as f32
    }

    pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
        self.chunks.iter().map(Vec::as_slice)
    }
}
