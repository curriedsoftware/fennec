//! `fennec enroll`: a wake word model tuned to your voice from a handful of
//! recordings, in one command.
//!
//! The synthetic dataset stays the base of the model. A few takes of your
//! voice anchor it to how you say the wake word and to your microphone;
//! training varies them (speed, pitch, distance, microphone, room, noise)
//! and makes negatives out of them (`dataset::derived_negatives`).

use std::io::{BufRead, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::bail;

use crate::audio::{self, active_rms, trim_to_utterance};
use crate::record::{RecordKind, capture, next_index};

pub struct EnrollOptions {
    pub phrase: Option<String>,
    pub data: PathBuf,
    pub voices: PathBuf,
    pub out: PathBuf,
    /// Recordings of the wake word to use instead of recording new ones.
    pub files: Vec<PathBuf>,
    pub takes: usize,
    pub test_takes: usize,
    pub room_seconds: u32,
    pub device: Option<String>,
    pub confusables: Vec<String>,
    pub steps: usize,
    pub workers: usize,
    pub python: String,
    pub seed: u64,
}

/// Length of each recorded take.
const TAKE_SECONDS: u32 = 3;

pub fn enroll(opts: &EnrollOptions) -> anyhow::Result<()> {
    let phrase = prepare_dataset(opts)?;
    let train_dir = opts.data.join(RecordKind::WakeWord.dir());
    let test_dir = opts.data.join(RecordKind::Test.dir());
    std::fs::create_dir_all(&train_dir)?;
    std::fs::create_dir_all(&test_dir)?;

    let mut test_files = Vec::new();
    if opts.files.is_empty() {
        println!(
            "\nRecording {} takes of {phrase:?} to train on and {} to test with.\n\
             Say it the way you normally would. Moving a little closer or further\n\
             between takes helps.",
            opts.takes, opts.test_takes
        );
        let mut lengths = Vec::new();
        for i in 0..opts.takes {
            let label = format!("take {}/{}", i + 1, opts.takes);
            record_take(&train_dir, &label, &phrase, opts, &mut lengths)?;
        }
        for i in 0..opts.test_takes {
            let label = format!("test take {}/{}", i + 1, opts.test_takes);
            test_files.push(record_take(&test_dir, &label, &phrase, opts, &mut lengths)?);
        }
        if opts.room_seconds > 0 {
            let dir = opts.data.join(RecordKind::Background.dir());
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{:04}.wav", next_index(&dir)?));
            println!(
                "\nNow {} s of your room as it usually sounds (TV, music, kitchen...).\n\
                 Don't say the wake word. Press Enter to start.",
                opts.room_seconds
            );
            wait_for_enter()?;
            capture(&path, opts.room_seconds, opts.device.as_deref())?;
            println!("saved {}", path.display());
        }
    } else {
        test_files = import_takes(opts, &train_dir, &test_dir)?;
    }

    println!();
    crate::train::train(&crate::train::TrainOptions {
        data: opts.data.clone(),
        out: opts.out.clone(),
        backgrounds: Vec::new(),
        sounds: Vec::new(),
        steps: opts.steps,
        batch_size: 256,
        learning_rate: 1e-3,
        frames: 150,
        width: 1.0,
        eval_minutes: 60.0,
        max_false_accepts_per_hour: 0.5,
        seed: opts.seed,
    })?;

    if !test_files.is_empty() {
        let (words, len) = crate::train::read_model(&opts.out)?;
        let model = crate::load_model(&words, len)?;
        let dataset = crate::dataset::Dataset::load(&opts.data, &[], &[])?;
        let threshold = model.header.threshold;
        println!(
            "\nyour test takes, threshold {threshold:.2} (\"recall (recorded)\" above mixes \
             noise at 10 to 20 dB):"
        );
        println!("  {:>7}  {:>14}", "quiet", "noise at 10 dB");
        for (i, path) in test_files.iter().enumerate() {
            let clip = trim_to_utterance(&audio::read_wav(path)?).to_vec();
            let [quiet, noisy] = [40.0, 10.0]
                .map(|snr| crate::eval::peak_score(&model, &dataset, &clip, snr, i as u64));
            let mark = |score: f32| if score >= threshold { "ok" } else { "missed" };
            println!(
                "  {quiet:.3} {:<6} {noisy:.3} {:<6}  {}",
                mark(quiet),
                mark(noisy),
                path.display()
            );
        }
    }
    println!(
        "\nTry it live:\n  arecord -q -f S16_LE -r 16000 -c 1 -t raw | fennec listen --scores {} -",
        opts.out.display()
    );
    Ok(())
}

/// Makes sure `opts.data` holds a synthetic dataset for the phrase, and
/// returns the phrase.
fn prepare_dataset(opts: &EnrollOptions) -> anyhow::Result<String> {
    if opts.data.join("meta.json").exists() {
        let phrase = crate::synth::read_meta(&opts.data)?.phrase;
        if let Some(wanted) = &opts.phrase
            && wanted.trim().to_lowercase() != phrase
        {
            bail!(
                "{} is a dataset for {phrase:?}, not {wanted:?}; pass another --data",
                opts.data.display()
            );
        }
        println!(
            "using the dataset for {phrase:?} in {}",
            opts.data.display()
        );
        return Ok(phrase);
    }
    let Some(phrase) = &opts.phrase else {
        bail!(
            "{} has no dataset yet; pass --phrase to create one",
            opts.data.display()
        );
    };
    let has_voices = opts.voices.is_dir()
        && std::fs::read_dir(&opts.voices)?
            .filter_map(Result::ok)
            .any(|e| e.path().extension().is_some_and(|x| x == "onnx"));
    if !has_voices {
        let names: Vec<String> = crate::voices::DEFAULT_VOICES
            .iter()
            .map(|s| s.to_string())
            .collect();
        crate::voices::download_voices(&opts.voices, &names)?;
    }
    crate::synth::synth(&crate::synth::SynthOptions {
        phrase: phrase.clone(),
        out: opts.data.clone(),
        voices: opts.voices.clone(),
        positives: 4000,
        negatives: 6000,
        confusables: opts.confusables.clone(),
        workers: opts.workers,
        python: opts.python.clone(),
        seed: opts.seed,
    })?;
    Ok(phrase.trim().to_lowercase())
}

/// Records one take into `dir`, again until it sounds usable or you keep it
/// anyway. Returns its path.
fn record_take(
    dir: &Path,
    label: &str,
    phrase: &str,
    opts: &EnrollOptions,
    lengths: &mut Vec<f32>,
) -> anyhow::Result<PathBuf> {
    let path = dir.join(format!("{:04}.wav", next_index(dir)?));
    loop {
        println!("\n[{label}] press Enter, then say {phrase:?} ({TAKE_SECONDS} s)");
        wait_for_enter()?;
        capture(&path, TAKE_SECONDS, opts.device.as_deref())?;
        let clip = audio::read_wav(&path)?;
        match check_take(&clip, lengths) {
            Ok((_, length)) => {
                lengths.push(length);
                println!("  ok ({length:.2} s of speech)");
                return Ok(path);
            }
            Err(problem) => {
                print!("  {problem}\n  record it again? [Y/n] ");
                std::io::stdout().flush()?;
                let mut answer = String::new();
                std::io::stdin().lock().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("n") {
                    lengths.push(trim_to_utterance(&clip).len() as f32 / 16_000.0);
                    return Ok(path);
                }
            }
        }
    }
}

/// Copies recordings you already have (any format `fennec` reads) into the
/// dataset; the last `test_takes` of them only test the model. Returns the
/// test takes.
fn import_takes(
    opts: &EnrollOptions,
    train_dir: &Path,
    test_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let test_count = if opts.files.len() > opts.test_takes {
        opts.test_takes
    } else {
        if opts.test_takes > 0 {
            println!(
                "only {} recording(s): training on all of them and keeping none to test with",
                opts.files.len()
            );
        }
        0
    };
    let mut lengths = Vec::new();
    let mut tests = Vec::new();
    for (i, file) in opts.files.iter().enumerate() {
        // Stored whole: training finds the wake word in it, past the clicks
        // of the device's button.
        let clip = audio::read_wav(file)?;
        let note = match check_take(&clip, &lengths) {
            Ok((_, length)) => {
                lengths.push(length);
                format!("{length:.2} s of speech")
            }
            Err(problem) => format!("warning: {problem}"),
        };
        let is_test = i >= opts.files.len() - test_count;
        let dir = if is_test { test_dir } else { train_dir };
        let path = dir.join(format!("{:04}.wav", next_index(dir)?));
        audio::write_wav(&path, &clip)?;
        println!("{} -> {} ({note})", file.display(), path.display());
        if is_test {
            tests.push(path);
        }
    }
    Ok(tests)
}

fn wait_for_enter() -> anyhow::Result<()> {
    std::io::stdin().lock().lines().next().transpose()?;
    Ok(())
}

/// Checks that a recording holds one clear, complete wake word. Returns
/// where it is in the recording and how long it is, in seconds, or what is
/// wrong with it. `lengths` are the lengths of the previous takes.
fn check_take(clip: &[f32], lengths: &[f32]) -> Result<(Range<usize>, f32), String> {
    let quiet = |level: f32| {
        let db = 20.0 * level.max(1e-9).log10();
        (db < -45.0)
            .then(|| format!("it is very quiet ({db:.0} dBFS): move closer or turn the input up"))
    };
    let Some(range) = audio::find_utterance(clip) else {
        return Err(quiet(active_rms(clip)).unwrap_or("there is hardly any speech in it".into()));
    };
    let speech = &clip[range.clone()];
    // A clipped recording has many samples at full scale, not just a peak
    // that touches it.
    let clipped = speech.iter().filter(|s| s.abs() >= 0.98).count();
    if clipped > speech.len() / 1000 {
        return Err(
            "it is so loud that it clips: move back a little or turn the input down".into(),
        );
    }
    let level = active_rms(speech);
    if let Some(problem) = quiet(level) {
        return Err(problem);
    }
    if range.start == 0 {
        return Err("the speech starts right away; it may be cut off at the start".into());
    }
    if range.end == clip.len() {
        return Err("the speech runs to the very end; it may be cut off".into());
    }
    let length = speech.len() as f32 / 16_000.0;
    if length < 0.3 {
        return Err("there is hardly any speech in it".into());
    }
    if length > 2.5 {
        return Err(format!(
            "{length:.1} s of speech is long for a wake word; say just the wake word"
        ));
    }
    let snr = 20.0 * (level / audio::noise_floor(clip)).log10();
    if snr < 15.0 {
        return Err(format!(
            "the room is loud compared to your voice ({snr:.0} dB): move closer or make it quieter"
        ));
    }
    if !lengths.is_empty() {
        let mut sorted = lengths.to_vec();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        if length > median * 1.7 || length < median * 0.6 {
            return Err(format!(
                "{length:.2} s of speech, but your other takes have about {median:.2} s; \
                 was it the wake word, said once?"
            ));
        }
    }
    Ok((range, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half a second of quiet noise, `seconds` of a voice-like tone, and
    /// half a second of quiet noise again.
    fn take(seconds: f32, amplitude: f32) -> Vec<f32> {
        let noise = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| ((i * 7919) % 101) as f32 / 101.0 * 0.002 - 0.001)
                .collect()
        };
        let voice: Vec<f32> = (0..(seconds * 16_000.0) as usize)
            .map(|n| (n as f32 * 0.1).sin() * amplitude)
            .collect();
        [noise(8000), voice, noise(8000)].concat()
    }

    #[test]
    fn accepts_a_clean_take() {
        let (_, length) = check_take(&take(0.8, 0.3), &[]).unwrap();
        // 30 ms of margin on each side.
        assert!((length - 0.86).abs() < 0.03, "{length}");
    }

    #[test]
    fn rejects_bad_takes() {
        assert!(
            check_take(&take(0.8, 1.2), &[])
                .unwrap_err()
                .contains("clips")
        );
        assert!(
            check_take(&take(0.8, 0.003), &[])
                .unwrap_err()
                .contains("quiet")
        );
        assert!(
            check_take(&take(0.1, 0.3), &[])
                .unwrap_err()
                .contains("hardly")
        );
        assert!(
            check_take(&take(0.8, 0.3), &[2.0])
                .unwrap_err()
                .contains("other takes")
        );
        let cut = take(0.8, 0.3)[8000..].to_vec();
        assert!(check_take(&cut, &[]).unwrap_err().contains("cut off"));
    }
}
