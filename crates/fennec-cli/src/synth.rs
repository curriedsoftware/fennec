//! Synthetic training data from Piper text-to-speech.
//!
//! Each Piper voice runs in a few long-lived Python workers that read jobs as
//! JSON lines, so a model is loaded once per worker instead of once per clip.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use indicatif::{ProgressBar, ProgressStyle};
use rand::rngs::Xoshiro256PlusPlus;
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::voices::{Voice, load_voices};

const WORKER: &str = r#"
import json, sys, wave
import onnxruntime
from piper import PiperVoice, SynthesisConfig
voice = PiperVoice.load(sys.argv[1])
# Fennec runs many workers in parallel; one thread each is much faster than
# letting every worker spread over all cores.
options = onnxruntime.SessionOptions()
options.intra_op_num_threads = 1
options.inter_op_num_threads = 1
voice.session = onnxruntime.InferenceSession(
    sys.argv[1], sess_options=options, providers=["CPUExecutionProvider"]
)
for line in sys.stdin:
    job = json.loads(line)
    config = SynthesisConfig(
        speaker_id=job["speaker"],
        length_scale=job["length_scale"],
        noise_scale=job["noise_scale"],
        noise_w_scale=job["noise_w"],
    )
    with wave.open(job["out"] + ".part", "wb") as f:
        voice.synthesize_wav(job["text"], f, syn_config=config)
    print(job["out"], flush=True)
"#;

const WORDS: &str = include_str!("../data/negative_words.txt");
const SENTENCES: &str = include_str!("../data/negative_sentences.txt");

/// Describes a dataset directory; written by `fennec synth`.
#[derive(Serialize, Deserialize)]
pub struct Meta {
    pub phrase: String,
}

pub fn read_meta(data: &Path) -> anyhow::Result<Meta> {
    let path = data.join("meta.json");
    let text = fs::read_to_string(&path).with_context(|| {
        format!(
            "cannot read {}; is this a dataset made by `fennec synth`?",
            path.display()
        )
    })?;
    Ok(serde_json::from_str(&text)?)
}

pub struct SynthOptions {
    pub phrase: String,
    pub out: PathBuf,
    pub voices: PathBuf,
    pub positives: usize,
    pub negatives: usize,
    pub confusables: Vec<String>,
    pub workers: usize,
    pub python: String,
    pub seed: u64,
}

#[derive(Serialize)]
struct Job {
    text: String,
    speaker: Option<usize>,
    length_scale: f32,
    noise_scale: f32,
    noise_w: f32,
    out: String,
}

fn positive_texts(phrase: &str) -> Vec<String> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let mut texts = vec![
        phrase.to_string(),
        format!("{phrase}."),
        format!("{phrase}!"),
        format!("{phrase}?"),
        format!("{phrase}..."),
    ];
    if words.len() > 1 {
        texts.push(format!("{}, {}.", words[0], words[1..].join(" ")));
        texts.push(format!("{}... {}!", words[0], words[1..].join(" ")));
    }
    texts
}

/// Things that are not the wake word but share parts of it.
fn partial_texts(phrase: &str) -> Vec<String> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let mut texts: Vec<String> = words.iter().map(|w| format!("{w}.")).collect();
    for n in 1..words.len() {
        texts.push(words[..n].join(" "));
        texts.push(words[n..].join(" "));
    }
    if words.len() > 1 {
        let mut reversed = words.clone();
        reversed.reverse();
        texts.push(reversed.join(" "));
    }
    // The last word cut short, e.g. "hey fenn".
    if let Some(last) = words.last()
        && last.chars().count() > 3
    {
        let cut: String = last.chars().take(last.chars().count() * 2 / 3).collect();
        let mut prefix = words[..words.len() - 1].to_vec();
        prefix.push(&cut);
        texts.push(prefix.join(" "));
    }
    texts
}

fn negative_text(phrase: &str, confusables: &[String], rng: &mut impl RngExt) -> String {
    let words: Vec<&str> = WORDS.split_whitespace().collect();
    let sentences: Vec<&str> = SENTENCES.lines().filter(|l| !l.trim().is_empty()).collect();
    let first = phrase.split_whitespace().next().unwrap_or("hey");
    // Most negatives are close to the wake word: those are the ones that
    // teach the model where its boundaries are.
    match rng.random_range(0..100) {
        0..20 => partial_texts(phrase).choose(rng).unwrap().clone(),
        20..35 if !confusables.is_empty() => confusables.choose(rng).unwrap().clone(),
        20..50 => format!("{first} {}", words.choose(rng).unwrap()),
        50..75 => sentences.choose(rng).unwrap().to_string(),
        _ => {
            let n = rng.random_range(1..=4);
            (0..n)
                .map(|_| *words.choose(rng).unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

fn job(text: String, voice: &Voice, out: &Path, rng: &mut impl RngExt) -> Job {
    Job {
        text,
        speaker: (voice.speakers > 1).then(|| rng.random_range(0..voice.speakers)),
        length_scale: rng.random_range(0.75..1.4),
        noise_scale: rng.random_range(0.4..1.0),
        noise_w: rng.random_range(0.5..1.2),
        out: out.display().to_string(),
    }
}

/// Splits `total` clips between voices, favouring voices with many speakers.
fn shares(voices: &[Voice], total: usize) -> Vec<usize> {
    let weights: Vec<f64> = voices
        .iter()
        .map(|v| 1.0 + (v.speakers as f64).ln())
        .collect();
    let sum: f64 = weights.iter().sum();
    let mut shares: Vec<usize> = weights
        .iter()
        .map(|w| (total as f64 * w / sum).floor() as usize)
        .collect();
    let missing = total - shares.iter().sum::<usize>();
    for share in shares.iter_mut().take(missing) {
        *share += 1;
    }
    shares
}

pub fn synth(opts: &SynthOptions) -> anyhow::Result<()> {
    let phrase = opts.phrase.trim().to_lowercase();
    if phrase.is_empty() {
        bail!("the wake word phrase is empty");
    }
    let voices = load_voices(&opts.voices)?;
    let positive_dir = opts.out.join("positive");
    let negative_dir = opts.out.join("negative");
    fs::create_dir_all(&positive_dir)?;
    fs::create_dir_all(&negative_dir)?;
    fs::write(
        opts.out.join("meta.json"),
        serde_json::to_string_pretty(&Meta {
            phrase: phrase.clone(),
        })?,
    )?;

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(opts.seed);
    let positives = positive_texts(&phrase);
    let pos_shares = shares(&voices, opts.positives);
    let neg_shares = shares(&voices, opts.negatives);

    let mut plan: Vec<(&Voice, Vec<Job>)> = Vec::new();
    for ((voice, &n_pos), &n_neg) in voices.iter().zip(&pos_shares).zip(&neg_shares) {
        let mut jobs = Vec::new();
        for i in 0..n_pos {
            let out = positive_dir.join(format!("{}-{i:05}.wav", voice.name));
            let text = positives.choose(&mut rng).unwrap().clone();
            jobs.push(job(text, voice, &out, &mut rng));
        }
        for i in 0..n_neg {
            let out = negative_dir.join(format!("{}-{i:05}.wav", voice.name));
            let text = negative_text(&phrase, &opts.confusables, &mut rng);
            jobs.push(job(text, voice, &out, &mut rng));
        }
        // Generating the jobs above keeps the random sequence identical, so
        // an interrupted run resumes with the same texts.
        jobs.retain(|j| !Path::new(&j.out).exists());
        for job in &jobs {
            // What each clip says, for `fennec eval --worst`.
            fs::write(Path::new(&job.out).with_extension("txt"), &job.text)?;
        }
        plan.push((voice, jobs));
    }

    let total: usize = plan.iter().map(|(_, jobs)| jobs.len()).sum();
    println!(
        "synthesizing {total} clips for {phrase:?} with {} voices ({} speakers)",
        voices.len(),
        voices.iter().map(|v| v.speakers).sum::<usize>()
    );
    let bar = ProgressBar::new(total as u64).with_style(
        ProgressStyle::with_template(
            "{bar:40} {pos}/{len} [{elapsed_precise} < {eta_precise}] {msg}",
        )
        .unwrap(),
    );
    for (voice, jobs) in plan {
        if jobs.is_empty() {
            continue;
        }
        bar.set_message(voice.name.clone());
        run_voice(voice, jobs, opts, &bar)?;
    }
    bar.finish();
    Ok(())
}

fn run_voice(
    voice: &Voice,
    jobs: Vec<Job>,
    opts: &SynthOptions,
    bar: &ProgressBar,
) -> anyhow::Result<()> {
    let workers = opts.workers.clamp(1, jobs.len().div_ceil(10).max(1));
    let mut batches: Vec<Vec<Job>> = (0..workers).map(|_| Vec::new()).collect();
    for (i, job) in jobs.into_iter().enumerate() {
        batches[i % workers].push(job);
    }

    std::thread::scope(|scope| {
        let handles: Vec<_> = batches
            .into_iter()
            .map(|batch| scope.spawn(move || run_worker(voice, batch, &opts.python, bar)))
            .collect();
        handles
            .into_iter()
            .try_for_each(|h| h.join().expect("worker thread panicked"))
    })
}

fn run_worker(
    voice: &Voice,
    jobs: Vec<Job>,
    python: &str,
    bar: &ProgressBar,
) -> anyhow::Result<()> {
    let mut child = Command::new(python)
        .arg("-c")
        .arg(WORKER)
        .arg(&voice.model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "cannot run {python:?}; run fennec inside the environment that provides piper-tts"
            )
        })?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for job in &jobs {
            writeln!(stdin, "{}", serde_json::to_string(job).unwrap())?;
        }
        Ok(())
    });
    for line in BufReader::new(stdout).lines() {
        let out = line?;
        fs::rename(format!("{out}.part"), &out)?;
        bar.inc(1);
    }
    let status = child.wait()?;
    writer.join().unwrap().ok();
    if !status.success() {
        bail!("piper worker for {} failed ({status})", voice.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partials_do_not_contain_the_phrase() {
        for text in partial_texts("hey fennec") {
            assert!(!text.contains("hey fennec"), "{text}");
        }
    }
}
