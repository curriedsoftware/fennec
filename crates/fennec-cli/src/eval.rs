//! Evaluates a model the way the device runs it: streaming audio through the
//! same front end and detector, then counting detections.

use fennec_core::SAMPLE_RATE;
use fennec_core::detector::{Detector, DetectorConfig};
use fennec_core::model::Model;
use rand::rngs::Xoshiro256PlusPlus;
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rayon::prelude::*;

use crate::audio::{active_rms, db_to_gain, rms, to_i16};
use crate::dataset::{Augmenter, Dataset, Split};

const THRESHOLDS: std::ops::RangeInclusive<u32> = 30..=99;
/// Detector settings used on the device (see `DetectorConfig::for_model`).
pub const CONSECUTIVE: usize = 2;
const REFRACTORY_EVALS: usize = 15;

pub struct Report {
    pub hours: f32,
    /// Per threshold in `THRESHOLDS`, in hundredths.
    pub false_accepts: Vec<usize>,
    pub recall: Vec<f32>,
    pub real_recall: Option<Vec<f32>>,
}

/// Probabilities of every model evaluation over `audio`.
pub fn scores(model: &Model<'_>, audio: &[f32]) -> Vec<f32> {
    let mut config = DetectorConfig::for_model(model);
    config.threshold = 2.0; // never trigger; we only want the scores
    let mut detector = Detector::new(model.clone(), config);
    let mut out = Vec::new();
    let samples: Vec<i16> = audio.iter().map(|&s| to_i16(s)).collect();
    for chunk in samples.chunks(160) {
        detector.push(chunk);
        if let Some(p) = detector.take_evaluation() {
            out.push(p);
        }
    }
    out
}

/// Scores after requiring `consecutive` hits: a detection at threshold `t`
/// happens wherever the smoothed score is at least `t`.
fn smooth(scores: &[f32], consecutive: usize) -> Vec<f32> {
    scores
        .windows(consecutive.max(1))
        .map(|w| w.iter().copied().fold(f32::MAX, f32::min))
        .collect()
}

fn count_detections(smoothed: &[f32], threshold: f32) -> usize {
    let mut count = 0;
    let mut cooldown = 0;
    for &s in smoothed {
        cooldown = usize::saturating_sub(cooldown, 1);
        if s >= threshold && cooldown == 0 {
            count += 1;
            cooldown = REFRACTORY_EVALS;
        }
    }
    count
}

fn thresholds() -> impl Iterator<Item = f32> {
    THRESHOLDS.map(|t| t as f32 / 100.0)
}

/// Fraction of clips detected, per threshold.
fn measure_recall(
    model: &Model<'_>,
    clips: &[Vec<f32>],
    aug: &Augmenter<'_>,
    seed: u64,
    consecutive: usize,
) -> Vec<f32> {
    let peaks: Vec<f32> = clips
        .par_iter()
        .enumerate()
        .map(|(i, clip)| {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed ^ i as u64);
            let lead = SAMPLE_RATE as usize;
            let len = lead + clip.len() + SAMPLE_RATE as usize;
            let mut audio = aug.background(len, &mut rng);
            // Speech 10 to 20 dB above the background.
            let gain = rms(&audio).max(1e-6) * db_to_gain(rng.random_range(10.0..20.0))
                / active_rms(clip).max(1e-6);
            for (dst, &s) in audio[lead..].iter_mut().zip(clip) {
                *dst += s * gain;
            }
            let peak = active_rms(&audio).max(1e-6);
            if peak > 0.5 {
                audio.iter_mut().for_each(|s| *s *= 0.5 / peak);
            }
            smooth(&scores(model, &audio), consecutive)
                .into_iter()
                .fold(0.0, f32::max)
        })
        .collect();
    thresholds()
        .map(|t| peaks.iter().filter(|&&p| p >= t).count() as f32 / peaks.len().max(1) as f32)
        .collect()
}

/// A continuous stream of background noise with held-out negative speech
/// (which includes near-misses like the first word of the wake word with
/// another word) or a sudden sound (a cough, a door...) every few seconds.
fn negative_stream(aug: &Augmenter<'_>, split: &Split, seconds: usize, seed: u64) -> Vec<f32> {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    let len = seconds * SAMPLE_RATE as usize;
    let mut audio = aug.background(len, &mut rng);
    let floor = rms(&audio).max(1e-6);
    let mut pos = 0usize;
    while pos < len {
        pos += rng.random_range(SAMPLE_RATE as usize / 2..3 * SAMPLE_RATE as usize);
        let sound;
        let clip = if rng.random_bool(0.25) {
            sound = aug.sound(&mut rng);
            &sound
        } else {
            split.negatives.choose(&mut rng).unwrap()
        };
        let gain = floor * db_to_gain(rng.random_range(5.0..25.0)) / active_rms(clip).max(1e-6);
        for (i, &s) in clip.iter().enumerate() {
            if let Some(dst) = audio.get_mut(pos + i) {
                *dst += s * gain;
            }
        }
        pos += clip.len();
    }
    let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.99 {
        audio.iter_mut().for_each(|s| *s *= 0.99 / peak);
    }
    audio
}

pub fn evaluate(
    model: &Model<'_>,
    dataset: &Dataset,
    minutes: f32,
    seed: u64,
    consecutive: usize,
) -> anyhow::Result<Report> {
    let aug = Augmenter {
        split: &dataset.val,
        backgrounds: &dataset.backgrounds,
        frames: model.header.frames,
        hard_negatives: &[],
    };
    let segments = (minutes.max(1.0)).ceil() as usize;
    let false_accepts: Vec<Vec<usize>> = (0..segments)
        .into_par_iter()
        .map(|i| {
            let audio = negative_stream(&aug, &dataset.val, 60, seed ^ (0xbad + i as u64));
            let smoothed = smooth(&scores(model, &audio), consecutive);
            thresholds()
                .map(|t| count_detections(&smoothed, t))
                .collect()
        })
        .collect();
    let false_accepts = (0..THRESHOLDS.count())
        .map(|t| false_accepts.iter().map(|s| s[t]).sum())
        .collect();

    let recall = measure_recall(model, &dataset.val.positives, &aug, seed, consecutive);
    let real_recall = (!dataset.val.real_positives.is_empty())
        .then(|| measure_recall(model, &dataset.val.real_positives, &aug, seed, consecutive));
    Ok(Report {
        hours: segments as f32 / 60.0,
        false_accepts,
        recall,
        real_recall,
    })
}

/// Highest smoothed score of `clip` played `snr_db` above background noise
/// from `dataset`.
pub fn peak_score(
    model: &Model<'_>,
    dataset: &Dataset,
    clip: &[f32],
    snr_db: f32,
    seed: u64,
) -> f32 {
    let aug = Augmenter {
        split: &dataset.val,
        backgrounds: &dataset.backgrounds,
        frames: model.header.frames,
        hard_negatives: &[],
    };
    peak_score_with(model, &aug, clip, snr_db, seed)
}

fn peak_score_with(
    model: &Model<'_>,
    aug: &Augmenter<'_>,
    clip: &[f32],
    snr_db: f32,
    seed: u64,
) -> f32 {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    let lead = SAMPLE_RATE as usize;
    let mut audio = aug.background(lead + clip.len() + lead, &mut rng);
    let gain = rms(&audio).max(1e-6) * db_to_gain(snr_db) / active_rms(clip).max(1e-6);
    for (dst, &s) in audio[lead..].iter_mut().zip(clip) {
        *dst += s * gain;
    }
    let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.99 {
        audio.iter_mut().for_each(|s| *s *= 0.99 / peak);
    }
    smooth(&scores(model, &audio), CONSECUTIVE)
        .into_iter()
        .fold(0.0, f32::max)
}

/// Prints the held-out negative clips the model likes most, and how it
/// reacts to the beginning of the wake word on its own.
pub fn diagnose(
    model: &Model<'_>,
    dataset: &Dataset,
    data: &std::path::Path,
    sound_dirs: &[std::path::PathBuf],
    worst: usize,
) -> anyhow::Result<()> {
    let aug = Augmenter {
        split: &dataset.val,
        backgrounds: &dataset.backgrounds,
        frames: model.header.frames,
        hard_negatives: &[],
    };
    let threshold = model.header.threshold;
    let mut files = crate::dataset::validation_files(&data.join("negative"))?;
    for dir in [data.join("negative-real"), data.join("sounds")]
        .iter()
        .chain(sound_dirs)
    {
        files.extend(crate::dataset::validation_files(dir)?);
    }
    let mut scored: Vec<(f32, std::path::PathBuf)> = files
        .par_iter()
        .enumerate()
        .filter_map(|(i, path)| {
            let clip = crate::audio::read_wav(path).ok()?;
            Some((
                peak_score_with(model, &aug, &clip, 15.0, i as u64),
                path.clone(),
            ))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let above = scored.iter().filter(|(s, _)| *s >= threshold).count();
    println!(
        "\n{above} of {} held-out negative clips reach the threshold ({threshold:.2}). Highest scores:",
        scored.len()
    );
    for (score, path) in scored.iter().take(worst) {
        // `fennec synth` writes what each clip says next to it.
        match std::fs::read_to_string(path.with_extension("txt")) {
            Ok(text) => println!("  {score:.3}  {}    {:?}", path.display(), text.trim()),
            Err(_) => println!("  {score:.3}  {}", path.display()),
        }
    }

    println!("\nbeginning of the wake word alone (share reaching the threshold):");
    for fraction in [0.4f32, 0.5, 0.6, 0.7, 0.8] {
        let hits = dataset
            .val
            .positives
            .par_iter()
            .enumerate()
            .filter(|(i, clip)| {
                let cut = &clip[..(clip.len() as f32 * fraction) as usize];
                peak_score_with(model, &aug, cut, 15.0, *i as u64) >= threshold
            })
            .count();
        println!(
            "  first {:>3.0}%: {:>5.1}%",
            fraction * 100.0,
            100.0 * hits as f32 / dataset.val.positives.len().max(1) as f32
        );
    }
    Ok(())
}

impl Report {
    fn index(t: f32) -> usize {
        ((t * 100.0).round() as u32).clamp(*THRESHOLDS.start(), *THRESHOLDS.end()) as usize
            - *THRESHOLDS.start() as usize
    }

    pub fn false_accepts_per_hour(&self, t: f32) -> f32 {
        self.false_accepts[Self::index(t)] as f32 / self.hours
    }

    /// The lowest threshold (at least 0.5) whose false accept rate is within
    /// `max_per_hour`; lower thresholds detect the wake word more reliably.
    pub fn pick_threshold(&self, max_per_hour: f32) -> f32 {
        thresholds()
            .filter(|&t| t >= 0.5)
            .find(|&t| self.false_accepts_per_hour(t) <= max_per_hour)
            .unwrap_or(0.99)
    }

    pub fn print(&self, chosen: f32) {
        println!(
            "{:>9}  {:>14}  {:>12}{}",
            "threshold",
            "false acc./h",
            "recall",
            if self.real_recall.is_some() {
                "  recall (recorded)"
            } else {
                ""
            }
        );
        let mut rows: Vec<f32> = vec![0.5, 0.6, 0.7, 0.8, 0.9, 0.95, 0.99, chosen];
        rows.sort_by(f32::total_cmp);
        rows.dedup();
        for t in rows {
            let i = Self::index(t);
            print!(
                "{:>9.2}  {:>14.2}  {:>11.1}%",
                t,
                self.false_accepts_per_hour(t),
                self.recall[i] * 100.0
            );
            if let Some(real) = &self.real_recall {
                print!("  {:>17.1}%", real[i] * 100.0);
            }
            println!("{}", if t == chosen { "   <- chosen" } else { "" });
        }
        println!(
            "(false accepts measured on {:.2} h of held-out synthetic speech and noise)",
            self.hours
        );
    }
}
