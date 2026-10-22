//! WAV input and output, resampling and level helpers.

use std::f32::consts::PI;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use fennec_core::{SAMPLE_RATE, adpcm};

/// Reads a WAV file as mono `f32` samples in `[-1, 1]` at 16 kHz.
///
/// Besides what `hound` reads, this accepts both kinds of recordings the
/// device uploads: IMA ADPCM WAV files, and Ogg Opus voice notes (decoded
/// with `ffmpeg`, which the development shell provides).
pub fn read_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot open {}", path.display()))?;
    if bytes.starts_with(b"OggS") {
        return read_with_ffmpeg(path);
    }
    if let Some((sample_rate, samples)) =
        read_adpcm(&bytes).with_context(|| format!("invalid {}", path.display()))?
    {
        let mono: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
        return Ok(resample(&mono, sample_rate, SAMPLE_RATE));
    }
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    if channels == 0 {
        bail!("{} has no channels", path.display());
    }
    let mono: Vec<f32> = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok(resample(&mono, spec.sample_rate, SAMPLE_RATE))
}

fn read_with_ffmpeg(path: &Path) -> anyhow::Result<Vec<f32>> {
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "s16le", "-ac", "1", "-ar"])
        .arg(SAMPLE_RATE.to_string())
        .arg("-")
        .output()
        .context("cannot run ffmpeg to decode an Ogg file; is it installed?")?;
    if !output.status.success() {
        bail!(
            "ffmpeg cannot decode {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output
        .stdout
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect())
}

/// Decodes a mono IMA ADPCM WAV with Fennec's block size. Returns `None` for
/// any other format.
fn read_adpcm(bytes: &[u8]) -> anyhow::Result<Option<(u32, Vec<i16>)>> {
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Ok(None);
    }
    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let (mut format, mut samples, mut data) = (None, None, None);
    let mut rest = &bytes[12..];
    while rest.len() >= 8 {
        let len = u32_at(rest, 4) as usize;
        let body = rest.get(8..8 + len).context("truncated chunk")?;
        match &rest[..4] {
            b"fmt " if len >= 16 => format = Some(body),
            b"fact" if len >= 4 => samples = Some(u32_at(body, 0) as usize),
            b"data" => data = Some(body),
            _ => {}
        }
        rest = &rest[(8 + len + len % 2).min(rest.len())..];
    }
    let format = format.context("no fmt chunk")?;
    if u16_at(format, 0) != 0x0011 {
        return Ok(None);
    }
    if u16_at(format, 2) != 1 || u16_at(format, 12) as usize != adpcm::BLOCK_ALIGN {
        bail!(
            "only mono IMA ADPCM with {}-byte blocks is supported",
            adpcm::BLOCK_ALIGN
        );
    }
    let data = data.context("no data chunk")?;
    let blocks = data.len() / adpcm::BLOCK_ALIGN;
    let samples = samples.unwrap_or(blocks * adpcm::SAMPLES_PER_BLOCK);
    Ok(Some((u32_at(format, 4), adpcm::decode(data, samples))))
}

pub fn write_wav(path: &Path, samples: &[f32]) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample(to_i16(s))?;
    }
    writer.finalize()?;
    Ok(())
}

pub fn to_i16(sample: f32) -> i16 {
    (sample * 32767.0).round().clamp(-32768.0, 32767.0) as i16
}

/// Audio files (WAV, and Ogg voice notes from the device) directly inside
/// `dir`, sorted. A missing directory is empty.
pub fn wav_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    files.retain(|p| {
        p.extension().is_some_and(|e| {
            ["wav", "ogg", "oga"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
    });
    files.sort();
    Ok(files)
}

/// Windowed-sinc resampler; good enough for speech, and dependency free.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    // Low-pass at the lower of the two Nyquist frequencies.
    let cutoff = ratio.min(1.0) as f32 * 0.95;
    const HALF_TAPS: i64 = 24;
    let len = (input.len() as f64 * ratio).floor() as usize;
    (0..len)
        .map(|n| {
            let center = n as f64 / ratio;
            let base = center.floor() as i64;
            let mut acc = 0.0;
            let mut norm = 0.0;
            for k in (base - HALF_TAPS + 1)..=(base + HALF_TAPS) {
                if k < 0 || k as usize >= input.len() {
                    continue;
                }
                let x = (k as f64 - center) as f32;
                let sinc = if x.abs() < 1e-6 {
                    1.0
                } else {
                    (PI * cutoff * x).sin() / (PI * cutoff * x)
                };
                let window = 0.5 + 0.5 * (PI * x / HALF_TAPS as f32).cos();
                let w = sinc * window;
                acc += input[k as usize] * w;
                norm += w;
            }
            if norm.abs() > 1e-6 { acc / norm } else { 0.0 }
        })
        .collect()
}

/// Changes speed by `factor` (> 1 is faster) and keeps the pitch, with
/// WSOLA: windows are taken from about where the new speed says, nudged to
/// line up with the audio already written, and overlap-added.
pub fn time_stretch(input: &[f32], factor: f32) -> Vec<f32> {
    const WINDOW: usize = 320;
    const HOP: usize = WINDOW / 2;
    const SEARCH: usize = 80;
    if input.len() < 2 * WINDOW {
        return input.to_vec();
    }
    let window: Vec<f32> = (0..WINDOW)
        .map(|i| (PI * i as f32 / WINDOW as f32).sin().powi(2))
        .collect();
    let len = (input.len() as f32 / factor) as usize;
    let last = input.len() - WINDOW;
    let mut out = vec![0.0; len + WINDOW];
    let mut prev = 0;
    for out_pos in (0..len).step_by(HOP) {
        let pos = if out_pos == 0 {
            0
        } else {
            // Where the previous window would naturally continue.
            let natural = (prev + HOP).min(last);
            let target = ((out_pos as f32 * factor) as usize).min(last);
            let lo = target.saturating_sub(SEARCH);
            let hi = (target + SEARCH).min(last);
            let reference = &input[natural..natural + HOP];
            (lo..=hi)
                .map(|p| {
                    let dot: f32 = input[p..p + HOP]
                        .iter()
                        .zip(reference)
                        .map(|(a, b)| a * b)
                        .sum();
                    (p, dot)
                })
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap()
                .0
        };
        for ((dst, &s), &w) in out[out_pos..].iter_mut().zip(&input[pos..]).zip(&window) {
            *dst += s * w;
        }
        prev = pos;
    }
    // Only the first window covers the first half window, with its fade-in.
    out[..HOP].copy_from_slice(&input[..HOP]);
    out.truncate(len);
    out
}

/// Makes a voice higher or lower by `semitones` at the same speed. Pitch and
/// formants move together, which sounds like a smaller or larger speaker.
pub fn shift_voice(input: &[f32], semitones: f32) -> Vec<f32> {
    let ratio = 2f32.powf(semitones / 12.0);
    let from = (SAMPLE_RATE as f32 * ratio).round() as u32;
    let shifted = resample(input, from, SAMPLE_RATE);
    time_stretch(&shifted, shifted.len() as f32 / input.len().max(1) as f32)
}

/// Second-order filters from the Audio EQ Cookbook.
#[derive(Clone, Copy, Debug)]
pub enum Filter {
    LowPass,
    HighPass,
    /// Boost or cut, in dB, around the frequency.
    Peak(f32),
    /// Boost or cut, in dB, below the frequency.
    LowShelf(f32),
}

/// Runs `samples` through `filter` at `freq` Hz in place.
pub fn filter(samples: &mut [f32], filter: Filter, freq: f32, q: f32) {
    let w0 = 2.0 * PI * freq / SAMPLE_RATE as f32;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * q);
    let [b0, b1, b2, a0, a1, a2] = match filter {
        Filter::LowPass => [
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        ],
        Filter::HighPass => [
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        ],
        Filter::Peak(db) => {
            let a = 10f32.powf(db / 40.0);
            [
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ]
        }
        Filter::LowShelf(db) => {
            let a = 10f32.powf(db / 40.0);
            let k = 2.0 * a.sqrt() * alpha;
            [
                a * ((a + 1.0) - (a - 1.0) * cos + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - k),
                (a + 1.0) + (a - 1.0) * cos + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - k,
            ]
        }
    };
    let (b0, b1, b2, a1, a2) = (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for s in samples {
        let x = *s;
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        (x2, x1, y2, y1) = (x1, x, y1, y);
        *s = y;
    }
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// RMS of the loudest part of a clip, ignoring leading and trailing silence.
pub fn active_rms(samples: &[f32]) -> f32 {
    let mut frames: Vec<f32> = samples.chunks(160).map(rms).collect();
    frames.sort_by(|a, b| b.total_cmp(a));
    let loud = &frames[..frames.len().div_ceil(2)];
    (loud.iter().map(|r| r * r).sum::<f32>() / loud.len().max(1) as f32).sqrt()
}

/// Where the wake word is in a recording of it: the loudest stretch of sound
/// well above the room's noise floor. Bursts shorter than 80 ms, like the
/// Atom Echo's button being pressed and released, are ignored, and sounds
/// less than 0.4 s apart, like two words, are joined. The range includes
/// 30 ms of margin on each side; `None` means nothing stands out.
pub fn find_utterance(samples: &[f32]) -> Option<Range<usize>> {
    const FRAME: usize = 160;
    const MIN_RUN: usize = 8;
    const MAX_GAP: usize = 40;
    const MARGIN: usize = 3;
    let levels: Vec<f32> = samples.chunks(FRAME).map(rms).collect();
    let threshold = noise_floor(samples) * db_to_gain(15.0);
    let mut runs: Vec<Range<usize>> = Vec::new();
    let mut start = None;
    for (i, &level) in levels.iter().chain([&0.0]).enumerate() {
        match (level > threshold, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s >= MIN_RUN {
                    match runs.last_mut() {
                        Some(last) if s - last.end <= MAX_GAP => last.end = i,
                        _ => runs.push(s..i),
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    let energy = |r: &Range<usize>| levels[r.clone()].iter().map(|l| l * l).sum::<f32>();
    let best = runs.iter().max_by(|a, b| energy(a).total_cmp(&energy(b)))?;
    let from = best.start.saturating_sub(MARGIN) * FRAME;
    let to = ((best.end + MARGIN) * FRAME).min(samples.len());
    Some(from..to)
}

/// The wake word in a recording of it (see `find_utterance`), or the
/// recording without its leading and trailing silence.
pub fn trim_to_utterance(samples: &[f32]) -> &[f32] {
    match find_utterance(samples) {
        Some(range) => &samples[range],
        None => trim_silence(samples),
    }
}

/// The level of the quietest 10% of a recording, in 10 ms frames.
pub fn noise_floor(samples: &[f32]) -> f32 {
    let mut levels: Vec<f32> = samples.chunks(160).map(rms).collect();
    levels.sort_by(f32::total_cmp);
    levels
        .get(levels.len() / 10)
        .copied()
        .unwrap_or(0.0)
        .max(1e-5)
}

/// Cuts leading and trailing silence, keeping a little margin.
pub fn trim_silence(samples: &[f32]) -> &[f32] {
    const FRAME: usize = 160;
    let levels: Vec<f32> = samples.chunks(FRAME).map(rms).collect();
    let peak = levels.iter().copied().fold(0.0, f32::max);
    if peak == 0.0 {
        return samples;
    }
    let threshold = peak * db_to_gain(-35.0);
    let first = levels.iter().position(|&l| l > threshold).unwrap_or(0);
    let last = levels
        .iter()
        .rposition(|&l| l > threshold)
        .unwrap_or(levels.len() - 1);
    let margin = 3;
    let start = first.saturating_sub(margin) * FRAME;
    let end = ((last + 1 + margin) * FRAME).min(samples.len());
    &samples[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_tones() {
        let tone: Vec<f32> = (0..22050)
            .map(|n| (2.0 * PI * 1000.0 * n as f32 / 22050.0).sin() * 0.5)
            .collect();
        let out = resample(&tone, 22050, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!((rms(&out[100..15_900]) - rms(&tone)).abs() < 0.01);
    }

    #[test]
    fn reads_device_recordings() {
        // Built the way the firmware builds its uploads.
        let pcm: Vec<i16> = (0..5000)
            .map(|n| ((n as f32 * 0.05).sin() * 8000.0) as i16)
            .collect();
        let mut encoder = adpcm::Encoder::new();
        let mut blocks = Vec::new();
        for &s in &pcm {
            if let Some(block) = encoder.push(s) {
                blocks.extend_from_slice(block);
            }
        }
        if let Some(block) = encoder.finish() {
            blocks.extend_from_slice(block);
        }
        let mut wav =
            adpcm::wav_header(SAMPLE_RATE, pcm.len(), blocks.len() / adpcm::BLOCK_ALIGN).to_vec();
        wav.extend_from_slice(&blocks);
        let path = std::env::temp_dir().join(format!("fennec-adpcm-{}.wav", std::process::id()));
        std::fs::write(&path, &wav).unwrap();
        let read = read_wav(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let want: Vec<f32> = adpcm::decode(&blocks, pcm.len())
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();
        assert_eq!(read, want);
    }

    /// Frequency of a tone, from its zero crossings.
    fn frequency(samples: &[f32]) -> f32 {
        let crossings = samples
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        crossings as f32 / 2.0 / (samples.len() as f32 / SAMPLE_RATE as f32)
    }

    fn tone(hz: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| (2.0 * PI * hz * n as f32 / SAMPLE_RATE as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn time_stretch_keeps_pitch() {
        let input = tone(220.0, 16_000);
        for factor in [0.85, 1.15] {
            let out = time_stretch(&input, factor);
            assert_eq!(out.len(), (16_000.0 / factor) as usize);
            let f = frequency(&out[400..out.len() - 400]);
            assert!((f - 220.0).abs() < 8.0, "{factor}: {f} Hz");
            assert!((rms(&out[400..out.len() - 400]) - rms(&input)).abs() < 0.05);
        }
    }

    #[test]
    fn shift_voice_changes_pitch_not_length() {
        let input = tone(220.0, 16_000);
        let out = shift_voice(&input, 12.0);
        assert!(out.len().abs_diff(input.len()) < 20, "{}", out.len());
        let f = frequency(&out[400..out.len() - 400]);
        assert!((f - 440.0).abs() < 15.0, "{f} Hz");
    }

    #[test]
    fn filters_shape_the_spectrum() {
        let gain = |hz: f32, f: Filter, at: f32| {
            let mut t = tone(hz, 16_000);
            filter(&mut t, f, at, 0.707);
            rms(&t[1600..]) / rms(&tone(hz, 16_000)[1600..])
        };
        assert!(gain(200.0, Filter::LowPass, 2000.0) > 0.95);
        assert!(gain(6000.0, Filter::LowPass, 2000.0) < 0.15);
        assert!(gain(100.0, Filter::HighPass, 400.0) < 0.15);
        let boost = gain(1000.0, Filter::Peak(6.0), 1000.0);
        assert!((boost - db_to_gain(6.0)).abs() < 0.1, "{boost}");
        let shelf = gain(80.0, Filter::LowShelf(6.0), 300.0);
        assert!((shelf - db_to_gain(6.0)).abs() < 0.15, "{shelf}");
    }

    #[test]
    fn finds_the_wake_word_between_button_clicks() {
        let quiet = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| ((i * 7919) % 101) as f32 / 101.0 * 0.002 - 0.001)
                .collect()
        };
        let click = || -> Vec<f32> {
            (0..480)
                .map(|i| if i % 2 == 0 { 0.8 } else { -0.8 })
                .collect()
        };
        // Press, release, then "hey", a short pause, "fennec", and silence.
        let recording = [
            click(),
            quiet(1200),
            click(),
            quiet(8000),
            tone(300.0, 3200),
            quiet(2400),
            tone(500.0, 6400),
            quiet(16_000),
        ]
        .concat();
        let speech_start = 480 + 1200 + 480 + 8000;
        let speech_end = speech_start + 3200 + 2400 + 6400;
        let found = find_utterance(&recording).unwrap();
        assert!(found.start.abs_diff(speech_start) <= 640, "{found:?}");
        assert!(found.end.abs_diff(speech_end) <= 640, "{found:?}");
    }

    #[test]
    fn trims_silence() {
        let mut clip = vec![0.0; 8000];
        clip.extend((0..4000).map(|n| (n as f32 * 0.3).sin() * 0.5));
        clip.extend(vec![0.0; 8000]);
        let trimmed = trim_silence(&clip);
        assert!(
            trimmed.len() < 5500 && trimmed.len() >= 4000,
            "{}",
            trimmed.len()
        );
    }
}
