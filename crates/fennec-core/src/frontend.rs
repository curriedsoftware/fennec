//! Streaming log-mel front end.
//!
//! Audio is cut into 25 ms Hann windows every 10 ms, transformed with a
//! 512-point FFT and folded into [`N_MELS`] mel bands. Each 10 ms hop produces
//! one frame of `N_MELS` natural-log energies.

use alloc::vec;
use alloc::vec::Vec;

use crate::SAMPLE_RATE;

/// Samples per analysis window (25 ms).
pub const WINDOW: usize = 400;
/// Samples between frames (10 ms).
pub const HOP: usize = 160;
/// FFT size.
pub const N_FFT: usize = 512;
/// Number of mel bands per frame.
pub const N_MELS: usize = 40;
/// Lowest frequency covered by the mel filters.
pub const F_MIN: f32 = 60.0;
/// Highest frequency covered by the mel filters.
pub const F_MAX: f32 = 7600.0;

const N_BINS: usize = N_FFT / 2 + 1;
const LOG_FLOOR: f32 = 1e-6;

/// Number of samples needed to produce `frames` frames.
pub const fn samples_for_frames(frames: usize) -> usize {
    WINDOW + (frames - 1) * HOP
}

struct MelFilter {
    first_bin: usize,
    weights: Vec<f32>,
}

/// Converts a stream of samples into log-mel frames.
pub struct Frontend {
    window: Vec<f32>,
    filters: Vec<MelFilter>,
    /// Ring buffer with the last `WINDOW` samples; `head` is the oldest.
    history: Vec<f32>,
    head: usize,
    /// Samples received since the last frame was emitted.
    pending: usize,
    /// Samples received in total, saturating at `WINDOW`.
    filled: usize,
    fft: [f32; N_FFT],
}

impl Default for Frontend {
    fn default() -> Self {
        Self::new()
    }
}

impl Frontend {
    pub fn new() -> Self {
        let window = (0..WINDOW)
            .map(|n| 0.5 - 0.5 * libm::cosf(2.0 * core::f32::consts::PI * n as f32 / WINDOW as f32))
            .collect();
        Self {
            window,
            filters: mel_filters(),
            history: vec![0.0; WINDOW],
            head: 0,
            pending: 0,
            filled: 0,
            fft: [0.0; N_FFT],
        }
    }

    /// Forgets all buffered audio.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.head = 0;
        self.pending = 0;
        self.filled = 0;
    }

    /// Feeds one sample. Returns `true` when a new frame is ready, in which
    /// case it has been written to `frame`.
    pub fn push(&mut self, sample: i16, frame: &mut [f32; N_MELS]) -> bool {
        self.history[self.head] = sample as f32 / 32768.0;
        self.head = (self.head + 1) % WINDOW;
        self.filled = (self.filled + 1).min(WINDOW);
        self.pending += 1;
        if self.filled < WINDOW || self.pending < HOP {
            return false;
        }
        self.pending = 0;
        self.compute(frame);
        true
    }

    /// Feeds a block of samples, calling `on_frame` for every frame produced.
    pub fn push_slice(&mut self, samples: &[i16], mut on_frame: impl FnMut(&[f32; N_MELS])) {
        let mut frame = [0.0; N_MELS];
        for &sample in samples {
            if self.push(sample, &mut frame) {
                on_frame(&frame);
            }
        }
    }

    fn compute(&mut self, frame: &mut [f32; N_MELS]) {
        let mean = self.history.iter().sum::<f32>() / WINDOW as f32;
        let (newest, oldest) = self.history.split_at(self.head);
        let ordered = oldest.iter().chain(newest);
        for ((dst, &x), &w) in self.fft.iter_mut().zip(ordered).zip(&self.window) {
            *dst = (x - mean) * w;
        }
        self.fft[WINDOW..].fill(0.0);

        let spectrum = microfft::real::rfft_512(&mut self.fft);
        let mut power = [0.0f32; N_BINS];
        power[0] = spectrum[0].re * spectrum[0].re;
        power[N_BINS - 1] = spectrum[0].im * spectrum[0].im;
        for (p, c) in power[1..N_BINS - 1].iter_mut().zip(&spectrum[1..]) {
            *p = c.re * c.re + c.im * c.im;
        }

        for (out, filter) in frame.iter_mut().zip(&self.filters) {
            let energy: f32 = filter
                .weights
                .iter()
                .zip(&power[filter.first_bin..])
                .map(|(w, p)| w * p)
                .sum();
            *out = libm::logf(energy + LOG_FLOOR);
        }
    }
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * libm::log10f(1.0 + hz / 700.0)
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (libm::powf(10.0, mel / 2595.0) - 1.0)
}

/// Triangular HTK-style mel filters, stored sparsely.
fn mel_filters() -> Vec<MelFilter> {
    let mel_min = hz_to_mel(F_MIN);
    let mel_max = hz_to_mel(F_MAX);
    let edges: Vec<f32> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(mel_min + (mel_max - mel_min) * i as f32 / (N_MELS + 1) as f32))
        .collect();
    let bin_hz = SAMPLE_RATE as f32 / N_FFT as f32;

    (0..N_MELS)
        .map(|m| {
            let (lo, center, hi) = (edges[m], edges[m + 1], edges[m + 2]);
            let mut first_bin = None;
            let mut weights = Vec::new();
            for bin in 0..N_BINS {
                let hz = bin as f32 * bin_hz;
                let w = if hz > lo && hz <= center {
                    (hz - lo) / (center - lo)
                } else if hz > center && hz < hi {
                    (hi - hz) / (hi - center)
                } else {
                    0.0
                };
                if w > 0.0 {
                    first_bin.get_or_insert(bin);
                    weights.push(w);
                } else if first_bin.is_some() {
                    break;
                }
            }
            // The lowest filters can be narrower than one FFT bin; give them
            // the nearest bin so no band is silent.
            if weights.is_empty() {
                first_bin = Some(libm::roundf(center / bin_hz) as usize);
                weights.push(1.0);
            }
            MelFilter {
                first_bin: first_bin.unwrap(),
                weights,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, len: usize, amplitude: f32) -> Vec<i16> {
        (0..len)
            .map(|n| {
                let t = n as f32 / SAMPLE_RATE as f32;
                (amplitude * 32767.0 * libm::sinf(2.0 * core::f32::consts::PI * hz * t)) as i16
            })
            .collect()
    }

    #[test]
    fn frame_count_matches_hops() {
        let mut frontend = Frontend::new();
        let mut frames = 0;
        frontend.push_slice(&vec![0; samples_for_frames(10)], |_| frames += 1);
        assert_eq!(frames, 10);
    }

    #[test]
    fn tone_peaks_in_expected_band() {
        let mut frontend = Frontend::new();
        let mut last = [0.0; N_MELS];
        frontend.push_slice(&tone(1000.0, 4000, 0.5), |f| last = *f);
        let peak = (0..N_MELS)
            .max_by(|&a, &b| last[a].total_cmp(&last[b]))
            .unwrap();
        let centers: Vec<f32> = (0..N_MELS)
            .map(|m| {
                let (lo, hi) = (hz_to_mel(F_MIN), hz_to_mel(F_MAX));
                mel_to_hz(lo + (hi - lo) * (m + 1) as f32 / (N_MELS + 1) as f32)
            })
            .collect();
        assert!(
            (centers[peak] - 1000.0).abs() < 150.0,
            "peak at {}",
            centers[peak]
        );
    }

    #[test]
    fn gain_shifts_log_energies_uniformly() {
        let (mut a, mut b) = (Frontend::new(), Frontend::new());
        let (mut fa, mut fb) = ([0.0; N_MELS], [0.0; N_MELS]);
        a.push_slice(&tone(700.0, 4000, 0.4), |f| fa = *f);
        b.push_slice(&tone(700.0, 4000, 0.1), |f| fb = *f);
        // 12 dB in amplitude is a factor of 16 in power: ln(16) ~= 2.77.
        let peak = (0..N_MELS)
            .max_by(|&x, &y| fa[x].total_cmp(&fa[y]))
            .unwrap();
        assert!((fa[peak] - fb[peak] - libm::logf(16.0)).abs() < 0.05);
    }

    #[test]
    fn every_filter_has_weights() {
        for filter in mel_filters() {
            assert!(!filter.weights.is_empty());
            assert!(filter.first_bin + filter.weights.len() <= N_BINS);
        }
    }
}
