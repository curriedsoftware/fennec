//! A recording encoded as an Ogg Opus voice note while it is made, kept in
//! RAM. Only built with the `opus` feature; see `recording.rs` for the WAV
//! one with the same interface.
//!
//! Encoding takes about 9 ms per 20 ms of audio, so it runs on its own thread
//! on the second core, which the rest of the firmware leaves idle. The main
//! loop hands it 10 ms frames over a channel and keeps reading the
//! microphone. The thread and its stack only exist while recording.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use anyhow::Context;
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;

use fennec_core::endpoint;
use fennec_core::ogg::OggOpus;
use fennec_core::SAMPLE_RATE;

use crate::opus::{self, Encoder};

/// What Telegram needs to know to show it as a voice note.
pub const UPLOAD: crate::telegram::Upload = crate::telegram::Upload {
    method: "sendVoice",
    field: "voice",
    content_type: "audio/ogg",
    extension: "ogg",
};

/// Speech at 24 kbit/s is close to transparent; 12 s take about 36 KB.
const BITRATE: i32 = 24_000;
/// 2 of 10: about 9 ms per 20 ms frame on the ESP32 (5 takes about 13 ms).
const COMPLEXITY: i32 = 2;
/// Heap that must stay free for Wi-Fi and the TLS connection used to upload
/// the recording.
const HEAP_RESERVE: usize = 64 * 1024;
/// The encoder needs about 24 KB of stack (libopus keeps its scratch buffers
/// there).
const STACK: usize = 32 * 1024;
/// Frames the encoder may fall behind by: 0.5 s.
const BACKLOG: usize = 50;
/// Heap the encoder thread holds while recording (its stack, plus about
/// 15 KB of encoder state) and gives back before the upload, when the
/// reserve is needed.
const ENCODER_HEAP: usize = STACK + 16 * 1024;

type Frame = [i16; endpoint::FRAME];

pub struct Recording {
    frames: Option<SyncSender<Frame>>,
    encoder: Option<JoinHandle<anyhow::Result<Vec<Vec<u8>>>>>,
    full: Arc<AtomicBool>,
    pages: Vec<Vec<u8>>,
    samples: usize,
}

impl Recording {
    pub fn new() -> anyhow::Result<Self> {
        let (frames, rx) = sync_channel(BACKLOG);
        let full = Arc::new(AtomicBool::new(false));
        ThreadSpawnConfiguration {
            name: Some(c"opus"),
            pin_to_core: Some(Core::Core1),
            ..Default::default()
        }
        .set()?;
        let spawned = std::thread::Builder::new().stack_size(STACK).spawn({
            let full = full.clone();
            move || encode(rx, &full)
        });
        ThreadSpawnConfiguration::default().set()?;
        Ok(Self {
            frames: Some(frames),
            encoder: Some(spawned.context("cannot start the Opus encoder")?),
            full,
            pages: Vec::new(),
            samples: 0,
        })
    }

    /// Appends one 10 ms frame. Returns `false` once memory runs out; the
    /// recording stays valid but drops further audio.
    pub fn push(&mut self, frame: &Frame) -> bool {
        if self.full.load(Ordering::Relaxed) {
            return false;
        }
        let Some(frames) = &self.frames else {
            return false;
        };
        if frames.send(*frame).is_err() {
            // The encoder stopped; `finish` reports why.
            return false;
        }
        self.samples += frame.len();
        true
    }

    /// Like [`Recording::push`], but returns `None` instead of waiting when
    /// the encoder is behind.
    pub fn try_push(&mut self, frame: &Frame) -> Option<bool> {
        if self.full.load(Ordering::Relaxed) {
            return Some(false);
        }
        match self.frames.as_ref()?.try_send(*frame) {
            Ok(()) => {
                self.samples += frame.len();
                Some(true)
            }
            Err(TrySendError::Full(_)) => None,
            Err(TrySendError::Disconnected(_)) => Some(false),
        }
    }

    /// Waits for the encoder to write the last page.
    pub fn finish(&mut self) {
        self.frames = None;
        if let Some(encoder) = self.encoder.take() {
            match encoder.join() {
                Ok(Ok(pages)) => self.pages = pages,
                Ok(Err(e)) => log::error!("encoding the voice note failed: {e:#}"),
                Err(_) => log::error!("the Opus encoder panicked"),
            }
        }
    }

    pub fn seconds(&self) -> f32 {
        self.samples as f32 / SAMPLE_RATE as f32
    }

    pub fn bytes(&self) -> usize {
        self.pages.iter().map(Vec::len).sum()
    }

    /// The file's contents, after [`Recording::finish`].
    pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
        self.pages.iter().map(Vec::as_slice)
    }

    /// Nothing precedes the pages: the Ogg headers are the first of them.
    pub fn header(&self) -> Vec<u8> {
        Vec::new()
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        // A recording that is thrown away still stops its thread.
        self.finish();
    }
}

/// The encoder thread: 10 ms frames in, Ogg pages out.
fn encode(frames: Receiver<Frame>, full: &AtomicBool) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut encoder = Encoder::new(BITRATE, COMPLEXITY)?;
    let serial = unsafe { esp_idf_svc::sys::esp_random() };
    let mut ogg = OggOpus::new(serial, SAMPLE_RATE, encoder.pre_skip()?);
    let mut pages = vec![ogg.headers(concat!("fennec ", env!("CARGO_PKG_VERSION")))];
    let mut pcm = [0i16; opus::FRAME];
    let mut filled = 0;
    let mut samples = 0;
    let mut packet = [0u8; opus::MAX_PACKET];
    let store = |page: Vec<u8>, pages: &mut Vec<Vec<u8>>| {
        let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() } as usize;
        // The encoder's and the pre-roll's memory come back before the
        // upload.
        if free + ENCODER_HEAP + crate::preroll::held() < HEAP_RESERVE + page.len() {
            full.store(true, Ordering::Relaxed);
        }
        pages.push(page);
    };
    // The channel closes when the recording ends.
    for frame in frames {
        samples += frame.len();
        for chunk in frame.chunks(opus::FRAME - filled) {
            pcm[filled..filled + chunk.len()].copy_from_slice(chunk);
            filled += chunk.len();
            if filled == opus::FRAME {
                filled = 0;
                let len = encoder.encode(&pcm, &mut packet)?;
                if let Some(page) = ogg.push(&packet[..len], opus::FRAME) {
                    store(page, &mut pages);
                }
            }
        }
    }
    if filled > 0 {
        // Pad the last frame with silence; the end position trims it.
        pcm[filled..].fill(0);
        let len = encoder.encode(&pcm, &mut packet)?;
        if let Some(page) = ogg.push(&packet[..len], opus::FRAME) {
            pages.push(page);
        }
    }
    pages.push(ogg.finish(samples));
    Ok(pages)
}
