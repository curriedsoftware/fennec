//! Streaming wake word detection.

use alloc::vec;
use alloc::vec::Vec;

use crate::frontend::{Frontend, N_MELS};
use crate::model::{Model, Scratch};

#[derive(Debug, Clone, Copy)]
pub struct DetectorConfig {
    /// Probability above which a window counts as a hit.
    pub threshold: f32,
    /// Frames (10 ms each) between two evaluations of the model.
    pub stride: usize,
    /// Consecutive hits needed to trigger.
    pub consecutive: usize,
    /// Frames to ignore after a detection, so one utterance triggers once.
    pub refractory: usize,
}

impl DetectorConfig {
    /// Default settings for `model`, using the threshold stored in it.
    pub fn for_model(model: &Model<'_>) -> Self {
        Self {
            threshold: model.header.threshold,
            stride: 10,
            consecutive: 2,
            refractory: 150,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// The lowest probability among the hits that triggered.
    pub probability: f32,
}

pub struct Detector<'a> {
    model: Model<'a>,
    pub config: DetectorConfig,
    frontend: Frontend,
    /// The most recent frames as the model wants them, `[N_MELS][frames]`,
    /// oldest first.
    window: Vec<f32>,
    filled: usize,
    since_eval: usize,
    hits: usize,
    hit_probability: f32,
    cooldown: usize,
    scratch: Scratch,
    last_probability: f32,
    evaluated: bool,
}

impl<'a> Detector<'a> {
    pub fn new(model: Model<'a>, config: DetectorConfig) -> Self {
        let frames = model.header.frames;
        Self {
            model,
            config,
            frontend: Frontend::new(),
            window: vec![0.0; frames * N_MELS],
            filled: 0,
            since_eval: 0,
            hits: 0,
            hit_probability: 1.0,
            cooldown: 0,
            scratch: Scratch::default(),
            last_probability: 0.0,
            evaluated: false,
        }
    }

    pub fn model(&self) -> &Model<'a> {
        &self.model
    }

    /// Probability computed by the most recent evaluation.
    pub fn last_probability(&self) -> f32 {
        self.last_probability
    }

    /// Returns the probability of the evaluation that happened since the
    /// last call, if any. Used to record score traces.
    pub fn take_evaluation(&mut self) -> Option<f32> {
        core::mem::take(&mut self.evaluated).then_some(self.last_probability)
    }

    /// Forgets all buffered audio, e.g. after the microphone was paused.
    pub fn reset(&mut self) {
        self.frontend.reset();
        self.filled = 0;
        self.since_eval = 0;
        self.hits = 0;
        self.cooldown = 0;
        self.last_probability = 0.0;
    }

    /// Feeds audio. Returns a detection if the wake word was heard in it;
    /// audio after the detection in the same call is still consumed.
    pub fn push(&mut self, samples: &[i16]) -> Option<Detection> {
        let mut detection = None;
        let mut frame = [0.0; N_MELS];
        for &sample in samples {
            if self.frontend.push(sample, &mut frame)
                && let Some(d) = self.push_frame(&frame)
            {
                detection.get_or_insert(d);
            }
        }
        detection
    }

    /// Feeds one feature frame directly.
    pub fn push_frame(&mut self, frame: &[f32; N_MELS]) -> Option<Detection> {
        let frames = self.model.header.frames;
        for (row, &value) in self.window.chunks_exact_mut(frames).zip(frame) {
            row.copy_within(1.., 0);
            row[frames - 1] = value;
        }
        self.filled = (self.filled + 1).min(frames);
        self.cooldown = self.cooldown.saturating_sub(1);
        self.since_eval += 1;
        if self.filled < frames || self.since_eval < self.config.stride {
            return None;
        }
        self.since_eval = 0;

        let p = self.model.probability(&self.window, &mut self.scratch);
        self.last_probability = p;
        self.evaluated = true;

        if p < self.config.threshold {
            self.hits = 0;
            self.hit_probability = 1.0;
            return None;
        }
        self.hits += 1;
        self.hit_probability = self.hit_probability.min(p);
        if self.hits < self.config.consecutive || self.cooldown > 0 {
            return None;
        }
        let detection = Detection {
            probability: self.hit_probability,
        };
        self.hits = 0;
        self.hit_probability = 1.0;
        self.cooldown = self.config.refractory;
        Some(detection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConvShape, Header, NAME_LEN, aligned_copy, encode};

    /// A model that fires on loud windows: one conv channel averaging all
    /// mel bands, with a dense layer on the max over time.
    fn loudness_model() -> Vec<u32> {
        let header = Header {
            frames: 20,
            threshold: 0.5,
            input_scale: 1.0,
            name: [0; NAME_LEN],
            convs: vec![ConvShape {
                in_channels: N_MELS,
                out_channels: 1,
                kernel: 1,
                stride: 1,
            }],
        };
        let mut weights = vec![1.0 / N_MELS as f32; N_MELS];
        weights.push(0.0); // conv bias
        weights.extend([0.0, 1.0, -2.0]); // dense: mean, max, bias
        aligned_copy(&encode(&header, &weights).unwrap())
    }

    #[test]
    fn fires_once_on_a_burst() {
        let words = loudness_model();
        let model = Model::from_bytes(bytemuck::cast_slice(&words)).unwrap();
        let mut config = DetectorConfig::for_model(&model);
        config.stride = 2;
        let mut detector = Detector::new(model, config);

        // Quiet first: the window mean is subtracted, so a constant level
        // never fires.
        assert_eq!(detector.push(&vec![10; 16_000]), None);
        let burst: Vec<i16> = (0..4000)
            .map(|n| (20_000.0 * libm::sinf(n as f32 * 2.0 * core::f32::consts::PI / 16.0)) as i16)
            .collect();
        let mut detections = 0;
        for chunk in burst.chunks(160).chain(vec![10i16; 8000].chunks(160)) {
            detections += detector.push(chunk).is_some() as usize;
        }
        assert_eq!(detections, 1);
    }
}
