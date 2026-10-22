//! Loads a dataset and turns it into augmented training windows.
//!
//! A positive window ends shortly after the wake word, which is what the
//! streaming detector sees right after it was said. Negative windows hold
//! other speech, fragments of the wake word, sudden sounds like coughs or
//! knocks, or just background noise.

use std::path::{Path, PathBuf};

use anyhow::bail;
use fennec_core::frontend::{Frontend, N_MELS, samples_for_frames};
use rand::seq::IndexedRandom;
use rand::{Rng, RngExt};
use rayon::prelude::*;

use crate::audio::{
    self, Filter, active_rms, db_to_gain, filter, read_wav, rms, shift_voice, time_stretch,
    trim_silence, trim_to_utterance,
};
use crate::sounds;

pub struct Dataset {
    pub phrase: String,
    pub train: Split,
    pub val: Split,
    /// Long recordings of the environment (TV, kitchen, street...).
    pub backgrounds: Vec<Vec<f32>>,
    /// Training negatives made from the recorded wake word clips.
    pub derived_negatives: usize,
}

#[derive(Default)]
pub struct Split {
    /// Trimmed wake word clips.
    pub positives: Vec<Vec<f32>>,
    /// Wake word clips recorded by people rather than synthesized.
    pub real_positives: Vec<Vec<f32>>,
    pub negatives: Vec<Vec<f32>>,
    /// Recorded non-speech sounds (coughs, doors, dishes...), played close
    /// to the microphone in negative windows.
    pub sounds: Vec<Vec<f32>>,
}

impl Split {
    /// The negatives that hard-negative mining scores: the speech clips,
    /// then the recorded sounds.
    pub fn negative_clips(&self) -> usize {
        self.negatives.len() + self.sounds.len()
    }

    pub fn negative_clip(&self, index: usize) -> &[f32] {
        match index.checked_sub(self.negatives.len()) {
            Some(i) => &self.sounds[i],
            None => &self.negatives[index],
        }
    }
}

/// Every `VAL_EVERY`-th clip goes to the validation split.
const VAL_EVERY: usize = 10;

/// Training and validation clips.
type Clips = (Vec<Vec<f32>>, Vec<Vec<f32>>);

/// Whether the `index`-th of `count` files in a directory is held out.
fn is_validation(index: usize, count: usize) -> bool {
    // Small sets of real recordings still need a few validation clips.
    let every = if count < 50 { 4 } else { VAL_EVERY };
    index % every == every - 1
}

/// The held-out files of a dataset directory.
pub fn validation_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let files = audio::wav_files(dir)?;
    let count = files.len();
    Ok(files
        .into_iter()
        .enumerate()
        .filter(|&(i, _)| is_validation(i, count))
        .map(|(_, path)| path)
        .collect())
}

/// Shortens a clip to the part that matters.
type Trim = fn(&[f32]) -> &[f32];

/// Loads every clip of a directory into the training or the validation
/// split. With `held_out`, all of them go into the validation split.
fn load_dir(dir: &Path, trim: Option<Trim>, held_out: bool) -> anyhow::Result<Clips> {
    let files = audio::wav_files(dir)?;
    let clips: Vec<(usize, Vec<f32>)> = files
        .par_iter()
        .enumerate()
        .filter_map(|(i, path)| match read_wav(path) {
            Ok(clip) => {
                let clip = match trim {
                    Some(trim) => trim(&clip).to_vec(),
                    None => clip,
                };
                (clip.len() > 1600).then_some((i, clip))
            }
            Err(e) => {
                eprintln!("skipping {}: {e:#}", path.display());
                None
            }
        })
        .collect();
    let mut train = Vec::new();
    let mut val = Vec::new();
    for (i, clip) in clips {
        if held_out || is_validation(i, files.len()) {
            &mut val
        } else {
            &mut train
        }
        .push(clip);
    }
    Ok((train, val))
}

impl Dataset {
    pub fn load(
        data: &Path,
        background_dirs: &[PathBuf],
        sound_dirs: &[PathBuf],
    ) -> anyhow::Result<Self> {
        let meta = crate::synth::read_meta(data)?;
        let (positives, val_positives) =
            load_dir(&data.join("positive"), Some(trim_silence), false)?;
        // Recordings can hold more than the wake word, like the clicks of
        // the device's button.
        let (real, mut val_real) =
            load_dir(&data.join("positive-real"), Some(trim_to_utterance), false)?;
        // Recordings kept apart to test models, never trained on.
        let (_, test_real) = load_dir(&data.join("positive-test"), Some(trim_to_utterance), true)?;
        val_real.extend(test_real);
        let (negatives, val_negatives) = load_dir(&data.join("negative"), None, false)?;
        let (real_neg, val_real_neg) = load_dir(&data.join("negative-real"), None, false)?;
        let derived = derived_negatives(&real, &negatives);
        let mut sounds = Vec::new();
        let mut val_sounds = Vec::new();
        for dir in std::iter::once(data.join("sounds")).chain(sound_dirs.iter().cloned()) {
            let (train, val) = load_dir(&dir, Some(trim_silence), false)?;
            sounds.extend(train);
            val_sounds.extend(val);
        }

        let mut backgrounds = Vec::new();
        let mut dirs = vec![data.join("background")];
        dirs.extend(background_dirs.iter().cloned());
        for dir in dirs {
            for path in audio::wav_files(&dir)? {
                let clip = read_wav(&path)?;
                if clip.len() >= samples_for_frames(2) {
                    backgrounds.push(clip);
                }
            }
        }

        if positives.len() + real.len() < 20 {
            bail!(
                "{} needs at least 20 wake word clips; run `fennec synth` first",
                data.display()
            );
        }
        if negatives.len() + real_neg.len() < 20 {
            bail!(
                "{} needs at least 20 negative clips; run `fennec synth` first",
                data.display()
            );
        }
        Ok(Self {
            phrase: meta.phrase,
            train: Split {
                positives,
                real_positives: real,
                negatives: negatives
                    .into_iter()
                    .chain(real_neg)
                    .chain(derived.iter().cloned())
                    .collect(),
                sounds,
            },
            val: Split {
                positives: val_positives,
                real_positives: val_real,
                negatives: val_negatives.into_iter().chain(val_real_neg).collect(),
                sounds: val_sounds,
            },
            backgrounds,
            derived_negatives: derived.len(),
        })
    }

    pub fn summary(&self) -> String {
        let minutes = self
            .backgrounds
            .iter()
            .map(|b| b.len() as f32)
            .fold(0.0, |a, b| a + b)
            / fennec_core::SAMPLE_RATE as f32
            / 60.0;
        format!(
            "{} synthetic + {} recorded wake word clips, {} negative clips ({} made from the recordings), \
             {} sound clips, {minutes:.1} min of background \
             (validation: {} + {} wake word, {} negative, {} sound)",
            self.train.positives.len(),
            self.train.real_positives.len(),
            self.train.negatives.len(),
            self.derived_negatives,
            self.train.sounds.len(),
            self.val.positives.len(),
            self.val.real_positives.len(),
            self.val.negatives.len(),
            self.val.sounds.len(),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Positive,
    Negative,
}

pub struct Augmenter<'a> {
    pub split: &'a Split,
    pub backgrounds: &'a [Vec<f32>],
    pub frames: usize,
    /// Indices (see `Split::negative_clip`) of negative clips the model
    /// currently confuses with the wake word; they are replayed more often.
    pub hard_negatives: &'a [usize],
}

impl Augmenter<'_> {
    pub fn window_len(&self) -> usize {
        samples_for_frames(self.frames)
    }

    fn pick_positive(&self, rng: &mut impl Rng) -> &[f32] {
        let real = &self.split.real_positives;
        if !real.is_empty() && (self.split.positives.is_empty() || rng.random_bool(0.3)) {
            real.choose(rng).unwrap()
        } else {
            self.split.positives.choose(rng).unwrap()
        }
    }

    /// How a clip changes between takes, speakers, distances, microphones
    /// and rooms. Negatives get the same changes, so that none of them gives
    /// the wake word away.
    fn vary(&self, clip: &[f32], rng: &mut impl Rng) -> Vec<f32> {
        let mut clip = clip.to_vec();
        if rng.random_bool(0.4) {
            clip = time_stretch(&clip, rng.random_range(0.85..1.15));
        }
        if rng.random_bool(0.4) {
            clip = shift_voice(&clip, rng.random_range(-3.0..3.0));
        }
        match rng.random_range(0..100) {
            0..25 => {
                // Across the room: duller, and more room than voice.
                filter(
                    &mut clip,
                    Filter::LowPass,
                    rng.random_range(2500.0..7000.0),
                    0.707,
                );
                clip = reverb(&clip, rng.random_range(0.3..0.7), rng);
            }
            25..40 => {
                // Close to the microphone: more bass, no room.
                let boost = rng.random_range(3.0..9.0);
                filter(
                    &mut clip,
                    Filter::LowShelf(boost),
                    rng.random_range(150.0..300.0),
                    0.707,
                );
            }
            40..60 => clip = reverb(&clip, 1.0, rng),
            _ => {}
        }
        if rng.random_bool(0.3) {
            // A cheaper or different microphone.
            filter(
                &mut clip,
                Filter::HighPass,
                rng.random_range(60.0..300.0),
                0.707,
            );
            let freq = rng.random_range(300.0..4000.0);
            let q = rng.random_range(0.7..2.0);
            filter(
                &mut clip,
                Filter::Peak(rng.random_range(-6.0..6.0)),
                freq,
                q,
            );
        }
        clip
    }

    /// A sudden non-speech sound: a recorded one from `sounds/`, or a
    /// synthetic cough, knock, clatter, beep or breath.
    pub fn sound(&self, rng: &mut impl Rng) -> Vec<f32> {
        match self.split.sounds.choose(rng) {
            Some(clip) if rng.random_bool(0.6) => clip.clone(),
            _ => sounds::event(rng),
        }
    }

    /// Background noise with unit RMS.
    fn unit_background(&self, len: usize, rng: &mut impl Rng) -> Vec<f32> {
        let mut out = match rng.random_range(0..100) {
            0..40 if !self.backgrounds.is_empty() => {
                let source = self.backgrounds.choose(rng).unwrap();
                crop(source, len, rng)
            }
            0..25 => colored_noise(len, rng.random_range(-1.0..2.0), rng),
            25..40 => hum(len, rng),
            40..60 => {
                // Distant speech.
                let mut babble = vec![0.0; len];
                for _ in 0..rng.random_range(1..4) {
                    let clip = self.split.negatives.choose(rng).unwrap();
                    let clip = crop(clip, len, rng);
                    babble.iter_mut().zip(&clip).for_each(|(b, s)| *b += s);
                }
                babble
            }
            _ => colored_noise(len, 1.0, rng),
        };
        let level = rms(&out);
        if level < 1e-9 {
            // A silent background recording; fall back to pink noise.
            out = colored_noise(len, 1.0, rng);
        }
        let level = rms(&out).max(1e-9);
        out.iter_mut().for_each(|s| *s /= level);
        out
    }

    /// Background noise at a random level between -70 and -30 dBFS.
    pub fn background(&self, len: usize, rng: &mut impl Rng) -> Vec<f32> {
        let mut out = self.unit_background(len, rng);
        let gain = db_to_gain(rng.random_range(-70.0..-30.0));
        out.iter_mut().for_each(|s| *s *= gain);
        out
    }

    /// Generates one window of audio.
    pub fn audio(&self, kind: Kind, rng: &mut impl Rng) -> Vec<f32> {
        let len = self.window_len();
        let mut window = vec![0.0; len];
        let speech_db: f32 = rng.random_range(-40.0..-12.0);
        let speech_gain = db_to_gain(speech_db);
        let mut has_speech = false;
        let mut place = |clip: &[f32], end: isize, window: &mut [f32]| {
            mix_at(window, clip, end, speech_gain / active_rms(clip).max(1e-9));
            has_speech = true;
        };
        let rate = fennec_core::SAMPLE_RATE as f32;

        match kind {
            Kind::Positive => {
                let mut clip = self.vary(self.pick_positive(rng), rng);
                if rng.random_bool(0.2) {
                    // A take that starts a little late or stops a little
                    // early is still the wake word; cutting more is not.
                    let start = (clip.len() as f32 * rng.random_range(0.0..0.08)) as usize;
                    let end =
                        clip.len() - (clip.len() as f32 * rng.random_range(0.0..0.05)) as usize;
                    clip = clip[start..end].to_vec();
                }
                let offset = rng.random_range(0.0..0.45) * rate;
                place(&clip, len as isize - offset as isize, &mut window);
                if rng.random_bool(0.1) {
                    // Someone coughs or puts a cup down nearby: the wake
                    // word is still the wake word.
                    let sound = self.vary(&self.sound(rng), rng);
                    let gain = speech_gain * db_to_gain(rng.random_range(-20.0..0.0))
                        / active_rms(&sound).max(1e-9);
                    let end = rng.random_range(0..len + sound.len()) as isize;
                    mix_at(&mut window, &sound, end, gain);
                }
            }
            Kind::Negative => match rng.random_range(0..100) {
                0..45 => {
                    let source = match self.hard_negatives.choose(rng) {
                        Some(&i) if rng.random_bool(0.5) => self.split.negative_clip(i),
                        _ => self.split.negatives.choose(rng).unwrap(),
                    };
                    let clip = self.vary(source, rng);
                    let end = rng.random_range(0..len + clip.len()) as isize;
                    place(&clip, end, &mut window);
                }
                45..60 => {
                    // The beginning of the wake word: must not fire early.
                    let clip = self.vary(self.pick_positive(rng), rng);
                    let keep = (clip.len() as f32 * rng.random_range(0.3..0.7)) as usize;
                    let offset = rng.random_range(0.0..0.3) * rate;
                    place(&clip[..keep], len as isize - offset as isize, &mut window);
                }
                60..70 => {
                    // The end of the wake word without its beginning.
                    let clip = self.vary(self.pick_positive(rng), rng);
                    let skip = (clip.len() as f32 * rng.random_range(0.35..0.7)) as usize;
                    let offset = rng.random_range(0.0..0.3) * rate;
                    place(&clip[skip..], len as isize - offset as isize, &mut window);
                }
                70..85 => {
                    // Coughs, knocks, beeps...: sudden sounds close to the
                    // microphone, which background noise never is.
                    for _ in 0..rng.random_range(1..4) {
                        let clip = self.vary(&self.sound(rng), rng);
                        let end = rng.random_range(0..len + clip.len()) as isize;
                        place(&clip, end, &mut window);
                    }
                }
                _ => {}
            },
        }

        // Mostly comfortable signal-to-noise ratios, sometimes hard ones.
        let background_db = if has_speech {
            let snr = if rng.random_bool(0.85) {
                rng.random_range(5.0..35.0)
            } else {
                rng.random_range(-3.0..5.0)
            };
            speech_db - snr
        } else {
            rng.random_range(-70.0..-25.0)
        };
        let background = self.unit_background(len, rng);
        let gain = db_to_gain(background_db);
        window
            .iter_mut()
            .zip(&background)
            .for_each(|(w, b)| *w += b * gain);

        // Occasionally clip like an overdriven microphone.
        let limit = if rng.random_bool(0.05) {
            rng.random_range(0.05..0.5)
        } else {
            1.0
        };
        window.iter_mut().for_each(|s| *s = s.clamp(-limit, limit));
        window
    }
}

impl Augmenter<'_> {
    /// The `index`-th negative clip ending near the end of a window, which is
    /// where it would most likely be mistaken for the wake word.
    pub fn negative_window(&self, index: usize, rng: &mut impl Rng) -> Vec<f32> {
        let len = self.window_len();
        let clip = self.split.negative_clip(index);
        let gain = db_to_gain(rng.random_range(-35.0..-15.0)) / active_rms(clip).max(1e-9);
        let mut window = vec![0.0; len];
        let end = len - (rng.random_range(0.0..0.3) * fennec_core::SAMPLE_RATE as f32) as usize;
        let start = end.saturating_sub(clip.len());
        let skip = clip.len().saturating_sub(end - start);
        for (dst, &s) in window[start..end].iter_mut().zip(&clip[skip..]) {
            *dst = s * gain;
        }
        let noise = self.unit_background(len, rng);
        let noise_gain = db_to_gain(rng.random_range(-60.0..-40.0));
        window
            .iter_mut()
            .zip(&noise)
            .for_each(|(w, n)| *w += n * noise_gain);
        window
    }
}

/// Where a recorded wake word is quietest between 25% and 70% of its length,
/// usually the gap between two words or syllables.
fn word_boundary(clip: &[f32]) -> usize {
    const FRAME: usize = 160;
    let frames = clip.len() / FRAME;
    let quietest = (frames / 4..(frames * 7 / 10).max(frames / 4 + 1))
        .min_by(|&a, &b| {
            let level = |i: usize| rms(&clip[i * FRAME..((i + 1) * FRAME).min(clip.len())]);
            level(a).total_cmp(&level(b))
        })
        .unwrap_or(frames / 2);
    (quietest * FRAME + FRAME / 2).min(clip.len())
}

/// Negatives made from recordings of the wake word: the same voice and
/// microphone, saying something that is not the wake word. They join the
/// other negatives, so hard-negative mining replays the ones that fool the
/// model.
fn derived_negatives(takes: &[Vec<f32>], others: &[Vec<f32>]) -> Vec<Vec<f32>> {
    use rand::SeedableRng;
    let mut out = Vec::new();
    for (i, take) in takes.iter().enumerate() {
        let mut rng = rand::rngs::Xoshiro256PlusPlus::seed_from_u64(0xde71 ^ i as u64);
        let split = word_boundary(take);
        let (head, tail) = take.split_at(split);
        // Played backwards: the same sounds in the wrong order.
        out.push(take.iter().rev().copied().collect());
        // The words swapped, "fennec hey".
        out.push([tail, head].concat());
        // The first word twice, "hey hey".
        out.push([head, head].concat());
        let mut other = || {
            let clip = others.choose(&mut rng)?;
            Some(clip.split_at(clip.len() / 2))
        };
        // The first word followed by something else, "hey <other words>".
        for _ in 0..4 {
            if let Some((_, end)) = other() {
                out.push([head, end].concat());
            }
        }
        // Something else followed by the end of the wake word.
        for _ in 0..2 {
            if let Some((start, _)) = other() {
                out.push([start, tail].concat());
            }
        }
    }
    out.retain(|clip| clip.len() > 1600);
    out
}

/// Log-mel features laid out as `[N_MELS][frames]`, computed with the same
/// front end as the device.
pub fn features(audio: &[f32], frames: usize) -> Vec<f32> {
    let mut frontend = Frontend::new();
    let mut out = vec![0.0; N_MELS * frames];
    let mut t = 0;
    let samples: Vec<i16> = audio.iter().map(|&s| audio::to_i16(s)).collect();
    frontend.push_slice(&samples, |frame| {
        if t < frames {
            for (m, &v) in frame.iter().enumerate() {
                out[m * frames + t] = v;
            }
        }
        t += 1;
    });
    debug_assert_eq!(t, frames);
    out
}

/// Adds `clip` times `gain` to `window` so that it ends at `end`, which may
/// be outside the window.
fn mix_at(window: &mut [f32], clip: &[f32], end: isize, gain: f32) {
    let start = end - clip.len() as isize;
    for (i, &s) in clip.iter().enumerate() {
        let pos = start + i as isize;
        if pos >= 0 && (pos as usize) < window.len() {
            window[pos as usize] += s * gain;
        }
    }
}

fn crop(source: &[f32], len: usize, rng: &mut impl Rng) -> Vec<f32> {
    if source.len() >= len {
        let start = rng.random_range(0..=source.len() - len);
        source[start..start + len].to_vec()
    } else {
        let mut out = vec![0.0; len];
        let start = rng.random_range(0..=len - source.len());
        out[start..start + source.len()].copy_from_slice(source);
        out
    }
}

/// Noise with a `1/f^exponent` power spectrum: 0 is white, 1 pink, 2 brown.
fn colored_noise(len: usize, exponent: f32, rng: &mut impl Rng) -> Vec<f32> {
    // Sum of octave-spaced random walks approximates the slope well enough.
    let mut out = vec![0.0; len];
    for octave in 0..10 {
        let period = 1usize << octave;
        let weight = (period as f32).powf(exponent / 2.0);
        let mut value = 0.0;
        for (i, s) in out.iter_mut().enumerate() {
            if i % period == 0 {
                value = rng.random_range(-1.0..1.0);
            }
            *s += value * weight;
        }
    }
    let mean = out.iter().sum::<f32>() / len as f32;
    out.iter_mut().for_each(|s| *s -= mean);
    out
}

/// Mains hum with harmonics, plus a little hiss.
fn hum(len: usize, rng: &mut impl Rng) -> Vec<f32> {
    let base = if rng.random_bool(0.5) { 50.0 } else { 60.0 };
    let phase: f32 = rng.random_range(0.0..std::f32::consts::TAU);
    (0..len)
        .map(|n| {
            let t = n as f32 / fennec_core::SAMPLE_RATE as f32;
            (1..6)
                .map(|h| (std::f32::consts::TAU * base * h as f32 * t + phase).sin() / h as f32)
                .sum::<f32>()
                + rng.random_range(-0.05..0.05)
        })
        .collect()
}

/// A cheap room: a handful of decaying early reflections, with the direct
/// sound scaled by `direct` (lower sounds further away).
fn reverb(clip: &[f32], direct: f32, rng: &mut impl Rng) -> Vec<f32> {
    let rt60 = rng.random_range(0.15..0.7f32);
    let tail = (rt60 * 0.5 * fennec_core::SAMPLE_RATE as f32) as usize;
    let mut out = vec![0.0; clip.len() + tail];
    out[..clip.len()]
        .iter_mut()
        .zip(clip)
        .for_each(|(o, &s)| *o = s * direct);
    for _ in 0..24 {
        let delay = rng.random_range(80..tail.max(81));
        let t = delay as f32 / fennec_core::SAMPLE_RATE as f32;
        let gain = 10f32.powf(-3.0 * t / rt60) * rng.random_range(-0.6..0.6);
        for (i, &s) in clip.iter().enumerate() {
            out[i + delay] += s * gain;
        }
    }
    out
}

/// Features and labels for a batch, generated in parallel.
pub fn batch(
    augmenter: &Augmenter<'_>,
    size: usize,
    positive_fraction: f64,
    seed: u64,
) -> (Vec<f32>, Vec<f32>) {
    let examples: Vec<(Vec<f32>, f32)> = (0..size)
        .into_par_iter()
        .map(|i| {
            use rand::SeedableRng;
            let mut rng = rand::rngs::Xoshiro256PlusPlus::seed_from_u64(
                seed ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15),
            );
            let kind = if rng.random_bool(positive_fraction) {
                Kind::Positive
            } else {
                Kind::Negative
            };
            let audio = augmenter.audio(kind, &mut rng);
            (
                features(&audio, augmenter.frames),
                (kind == Kind::Positive) as u8 as f32,
            )
        })
        .collect();
    let mut feats = Vec::with_capacity(size * N_MELS * augmenter.frames);
    let mut labels = Vec::with_capacity(size);
    for (f, l) in examples {
        feats.extend(f);
        labels.push(l);
    }
    (feats, labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "hey" and "fennec" as two tones with a short gap between them.
    fn two_words() -> Vec<f32> {
        let word = |hz: f32, len: usize| -> Vec<f32> {
            (0..len)
                .map(|n| (n as f32 * hz / 16_000.0 * std::f32::consts::TAU).sin() * 0.3)
                .collect()
        };
        [word(300.0, 4000), vec![0.0; 1600], word(500.0, 8000)].concat()
    }

    #[test]
    fn finds_the_gap_between_words() {
        let at = word_boundary(&two_words());
        assert!((4000..5600).contains(&at), "{at}");
    }

    #[test]
    fn derives_negatives_from_takes() {
        let others = vec![vec![0.1; 8000]; 3];
        let derived = derived_negatives(&[two_words()], &others);
        assert_eq!(derived.len(), 9);
        let take = two_words();
        assert!(derived.iter().all(|clip| clip != &take));
    }
}
