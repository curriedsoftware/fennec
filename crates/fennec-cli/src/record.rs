//! Records real samples with ALSA's `arecord`.

use std::io::BufRead;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RecordKind {
    /// You saying the wake word (stored in `positive-real/`).
    WakeWord,
    /// You saying the wake word, kept only to test models (stored in
    /// `positive-test/`).
    Test,
    /// You saying anything else (stored in `negative-real/`).
    Negative,
    /// The sound of the room: TV, music, kitchen... (stored in `background/`).
    Background,
    /// A sudden sound close to the device: a cough, a clap, a door... (stored
    /// in `sounds/`).
    Sound,
}

impl RecordKind {
    pub fn dir(self) -> &'static str {
        match self {
            RecordKind::WakeWord => "positive-real",
            RecordKind::Test => "positive-test",
            RecordKind::Negative => "negative-real",
            RecordKind::Background => "background",
            RecordKind::Sound => "sounds",
        }
    }
}

pub fn record(
    data: &Path,
    kind: RecordKind,
    count: usize,
    seconds: u32,
    device: Option<&str>,
) -> anyhow::Result<()> {
    let meta = crate::synth::read_meta(data)?;
    let dir = data.join(kind.dir());
    std::fs::create_dir_all(&dir)?;
    let first = next_index(&dir)?;
    let prompt = match kind {
        RecordKind::WakeWord | RecordKind::Test => format!("say {:?}", meta.phrase),
        RecordKind::Negative => "say anything except the wake word".into(),
        RecordKind::Background => "stay quiet and let the room make its usual noise".into(),
        RecordKind::Sound => "cough, clap, knock, close a door... but don't speak".into(),
    };

    let stdin = std::io::stdin();
    for i in 0..count {
        let path = dir.join(format!("{:04}.wav", first + i));
        println!(
            "[{}/{count}] press Enter, then {prompt} ({seconds}s)",
            i + 1
        );
        stdin.lock().lines().next().transpose()?;
        capture(&path, seconds, device)?;
        println!("saved {}", path.display());
    }
    Ok(())
}

/// The number of the next recording in `dir`, so that new files never
/// replace old ones.
pub fn next_index(dir: &Path) -> anyhow::Result<usize> {
    Ok(crate::audio::wav_files(dir)?
        .iter()
        .filter_map(|p| p.file_stem()?.to_str()?.parse::<usize>().ok())
        .map(|i| i + 1)
        .max()
        .unwrap_or(0))
}

/// Records `seconds` of 16 kHz mono audio into `path`.
pub fn capture(path: &Path, seconds: u32, device: Option<&str>) -> anyhow::Result<()> {
    let mut cmd = Command::new("arecord");
    cmd.args(["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-d"])
        .arg(seconds.to_string());
    if let Some(device) = device {
        cmd.args(["-D", device]);
    }
    let status = cmd
        .arg(path)
        .status()
        .context("cannot run arecord; it is in the alsa-utils package")?;
    if !status.success() {
        bail!("arecord failed ({status})");
    }
    Ok(())
}
