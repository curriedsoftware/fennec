//! The last few seconds of microphone audio, so that a recording can begin
//! before what started it. Say the wake word; if the device does not react,
//! press the button, and the recording starts with the take it missed. And
//! when the wake word fires, the recording can start with what fired it,
//! which for a false wake-up is the sound that fooled the model.
//!
//! While listening, the audio is kept as IMA ADPCM blocks (about 8 KB a
//! second), the oldest dropped as new ones arrive. Once a button recording
//! starts, the buffer becomes a queue: live audio goes in at the back while
//! the recording takes it from the front. The Opus encoder runs faster than
//! real time, so the queue empties after a few seconds, giving its memory
//! back long before the upload needs it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use fennec_core::SAMPLE_RATE;
use fennec_core::adpcm::{self, BLOCK_ALIGN, Encoder, SAMPLES_PER_BLOCK};
use fennec_core::endpoint;

type Frame = [i16; endpoint::FRAME];

/// Bytes of heap the pre-roll holds. Recordings count it as free: it is
/// given back before the upload.
static HELD: AtomicUsize = AtomicUsize::new(0);

pub fn held() -> usize {
    HELD.load(Ordering::Relaxed)
}

pub struct Preroll {
    encoder: Encoder,
    blocks: VecDeque<Vec<u8>>,
    /// Blocks kept while listening; 0 disables the pre-roll.
    capacity: usize,
    /// Whether a recording is taking the audio, so that none is dropped.
    queue: bool,
    /// The last, partial block and how many samples it holds, after
    /// [`Preroll::finish`].
    tail: Option<([u8; BLOCK_ALIGN], usize)>,
    /// Decoded audio not handed out yet, from `at` on.
    pending: Vec<i16>,
    at: usize,
}

impl Preroll {
    pub fn new(ms: u32) -> Self {
        Self {
            encoder: Encoder::new(),
            blocks: VecDeque::new(),
            capacity: blocks_for(ms),
            queue: false,
            tail: None,
            pending: Vec::new(),
            at: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.capacity > 0
    }

    /// Adds a frame of microphone audio.
    pub fn push(&mut self, frame: &Frame) {
        if !self.enabled() {
            return;
        }
        for &sample in frame {
            if let Some(block) = self.encoder.push(sample) {
                let block = *block;
                self.store(&block);
            }
        }
    }

    fn store(&mut self, block: &[u8; BLOCK_ALIGN]) {
        let mut stored = if !self.queue && self.blocks.len() >= self.capacity {
            // Reuse the oldest block's memory.
            let mut oldest = self.blocks.pop_front().unwrap();
            oldest.clear();
            oldest
        } else {
            let mut new = Vec::new();
            if new.try_reserve_exact(BLOCK_ALIGN).is_err() {
                log::warn!("out of memory for the pre-roll; dropping 64 ms of audio");
                return;
            }
            HELD.fetch_add(BLOCK_ALIGN, Ordering::Relaxed);
            new
        };
        stored.extend_from_slice(block);
        self.blocks.push_back(stored);
    }

    /// Stops dropping old audio: a recording takes it from here on,
    /// starting `ms` before now, or as far back as there is.
    pub fn start_queue(&mut self, ms: u32) {
        let keep = blocks_for(ms);
        while self.blocks.len() > keep {
            self.blocks.pop_front();
            HELD.fetch_sub(BLOCK_ALIGN, Ordering::Relaxed);
        }
        self.queue = true;
    }

    /// Seconds of audio waiting to be taken.
    pub fn seconds(&self) -> f32 {
        let samples = self.blocks.len() * SAMPLES_PER_BLOCK
            + self.tail.map_or(self.encoder.total_samples() % SAMPLES_PER_BLOCK, |(_, len)| {
                len
            })
            + self.pending.len()
            - self.at;
        samples as f32 / SAMPLE_RATE as f32
    }

    /// The oldest frame not taken yet; [`Preroll::advance`] takes it.
    pub fn peek(&mut self) -> Option<Frame> {
        while self.pending.len() - self.at < endpoint::FRAME {
            self.pending.drain(..self.at);
            self.at = 0;
            if let Some(block) = self.blocks.pop_front() {
                HELD.fetch_sub(BLOCK_ALIGN, Ordering::Relaxed);
                self.pending
                    .extend(adpcm::decode(&block, SAMPLES_PER_BLOCK));
            } else {
                let (block, len) = self.tail.take()?;
                self.pending.extend(adpcm::decode(&block, len));
                // Pad the last frame with silence.
                let frames = self.pending.len().div_ceil(endpoint::FRAME);
                self.pending.resize(frames * endpoint::FRAME, 0);
                if self.pending.is_empty() {
                    return None;
                }
            }
        }
        Some(self.pending[self.at..self.at + endpoint::FRAME].try_into().unwrap())
    }

    pub fn advance(&mut self) {
        self.at += endpoint::FRAME;
    }

    /// Queues the audio not yet in a complete block, so that
    /// [`Preroll::peek`] hands out everything that was pushed.
    pub fn finish(&mut self) {
        let len = self.encoder.total_samples() % SAMPLES_PER_BLOCK;
        if let Some(block) = self.encoder.finish() {
            self.tail = Some((*block, len));
        }
    }

    /// Drops all audio and goes back to keeping the last few seconds.
    pub fn clear(&mut self) {
        HELD.fetch_sub(self.blocks.len() * BLOCK_ALIGN, Ordering::Relaxed);
        self.blocks = VecDeque::new();
        self.pending = Vec::new();
        self.at = 0;
        self.encoder = Encoder::new();
        self.tail = None;
        self.queue = false;
    }
}

/// ADPCM blocks holding at least `ms` of audio.
fn blocks_for(ms: u32) -> usize {
    let samples = ms as usize * SAMPLE_RATE as usize / 1000;
    samples.div_ceil(SAMPLES_PER_BLOCK)
}
