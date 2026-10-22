//! Piper voices used to synthesize training data.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

/// Voices downloaded when none are given. Together they cover more than a
/// thousand speakers, including non-native English speakers (L2-ARCTIC).
pub const DEFAULT_VOICES: &[&str] = &[
    "en_US-libritts_r-medium",
    "en_GB-vctk-medium",
    "en_US-l2arctic-medium",
    "en_US-arctic-medium",
    "en_US-lessac-medium",
    "en_GB-alba-medium",
];

const BASE_URL: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main";

pub struct Voice {
    pub name: String,
    pub model: PathBuf,
    pub speakers: usize,
}

#[derive(Deserialize)]
struct VoiceConfig {
    #[serde(default = "one")]
    num_speakers: usize,
}

fn one() -> usize {
    1
}

/// `en_US-libritts_r-medium` lives at `en/en_US/libritts_r/medium/`.
fn voice_url(name: &str, file: &str) -> anyhow::Result<String> {
    let parts: Vec<&str> = name.splitn(3, '-').collect();
    let [locale, dataset, quality] = parts[..] else {
        bail!("voice names look like `en_US-lessac-medium`, got {name:?}");
    };
    let language = locale.split('_').next().unwrap();
    Ok(format!(
        "{BASE_URL}/{language}/{locale}/{dataset}/{quality}/{file}"
    ))
}

fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(concat!("fennec/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut response = client.get(url).send()?.error_for_status()?;
    let partial = dest.with_extension("part");
    let mut file = fs::File::create(&partial)?;
    response.copy_to(&mut file)?;
    file.flush()?;
    fs::rename(&partial, dest)?;
    Ok(())
}

/// Downloads `names` into `dir`, skipping voices that are already there.
pub fn download_voices(dir: &Path, names: &[String]) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    for name in names {
        for file in [format!("{name}.onnx.json"), format!("{name}.onnx")] {
            let dest = dir.join(&file);
            if dest.exists() {
                println!("{file}: already downloaded");
                continue;
            }
            let url = voice_url(name, &file)?;
            println!("{file}: downloading {url}");
            download(&url, &dest).with_context(|| format!("cannot download {url}"))?;
        }
    }
    Ok(())
}

/// Voices found in `dir`.
pub fn load_voices(dir: &Path) -> anyhow::Result<Vec<Voice>> {
    let mut voices = Vec::new();
    let entries = fs::read_dir(dir)
        .with_context(|| format!("cannot read voice directory {}", dir.display()))?;
    for entry in entries {
        let model = entry?.path();
        if model.extension().is_none_or(|e| e != "onnx") {
            continue;
        }
        let config_path = model.with_extension("onnx.json");
        let config: VoiceConfig = serde_json::from_str(
            &fs::read_to_string(&config_path)
                .with_context(|| format!("missing {}", config_path.display()))?,
        )?;
        voices.push(Voice {
            name: model.file_stem().unwrap().to_string_lossy().into_owned(),
            model,
            speakers: config.num_speakers.max(1),
        });
    }
    voices.sort_by(|a, b| a.name.cmp(&b.name));
    if voices.is_empty() {
        bail!(
            "no Piper voices in {}; run `fennec voices` to download some",
            dir.display()
        );
    }
    Ok(voices)
}
