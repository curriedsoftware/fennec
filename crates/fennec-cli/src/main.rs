//! `fennec`: build and test wake word models for the Fennec firmware.

mod audio;
mod dataset;
mod device;
mod enroll;
mod eval;
mod listen;
mod record;
mod sounds;
mod synth;
mod train;
mod voices;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use fennec_core::model::Model;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download Piper voices used to synthesize training data.
    Voices {
        /// Where to store the voices.
        #[arg(long, default_value = "voices")]
        dir: PathBuf,
        /// Voices to download, e.g. `es_ES-sharvard-medium`. Defaults to a
        /// set of English voices with more than a thousand speakers.
        names: Vec<String>,
    },
    /// Synthesize wake word and negative clips with Piper.
    Synth {
        /// The wake word, e.g. "hey fennec".
        #[arg(long)]
        phrase: String,
        /// Dataset directory to create or extend.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "voices")]
        voices: PathBuf,
        /// Number of wake word clips.
        #[arg(long, default_value_t = 4000)]
        positives: usize,
        /// Number of clips of other speech.
        #[arg(long, default_value_t = 6000)]
        negatives: usize,
        /// Phrases that sound like the wake word but are not, e.g. "hey
        /// fenwick". Can be repeated.
        #[arg(long = "confusable")]
        confusables: Vec<String>,
        /// Piper processes per voice.
        #[arg(long, default_value_t = default_workers())]
        workers: usize,
        /// Python interpreter that can `import piper`.
        #[arg(long, env = "FENNEC_PYTHON", default_value = "python3")]
        python: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Record your own samples with `arecord` to improve a dataset.
    Record {
        /// Dataset directory created by `fennec synth`.
        #[arg(long)]
        data: PathBuf,
        #[arg(long, value_enum, default_value = "wake-word")]
        kind: record::RecordKind,
        #[arg(long, default_value_t = 20)]
        count: usize,
        /// Length of each recording.
        #[arg(long, default_value_t = 2)]
        seconds: u32,
        /// ALSA capture device, e.g. `plughw:1,0`.
        #[arg(long)]
        device: Option<String>,
    },
    /// Record a few takes of your voice (or use recordings you have) and
    /// train a model with them, synthesizing the dataset first if needed.
    Enroll {
        /// The wake word. Needed only when the dataset does not exist yet.
        #[arg(long)]
        phrase: Option<String>,
        /// Dataset directory; created with `fennec synth` if it is missing.
        #[arg(long)]
        data: PathBuf,
        /// Where the Piper voices are, or go if they are missing.
        #[arg(long, default_value = "voices")]
        voices: PathBuf,
        /// Output model file.
        #[arg(long)]
        out: PathBuf,
        /// Takes to train on.
        #[arg(long, default_value_t = 3)]
        takes: usize,
        /// Takes kept apart to test the model.
        #[arg(long, default_value_t = 1)]
        test_takes: usize,
        /// Seconds of room sound to record; 0 to skip.
        #[arg(long, default_value_t = 30)]
        room_seconds: u32,
        /// ALSA capture device, e.g. `plughw:1,0`.
        #[arg(long)]
        device: Option<String>,
        /// Phrases that sound like the wake word, for a new dataset. Can be
        /// repeated.
        #[arg(long = "confusable")]
        confusables: Vec<String>,
        #[arg(long, default_value_t = 6000)]
        steps: usize,
        #[arg(long, default_value_t = default_workers())]
        workers: usize,
        #[arg(long, env = "FENNEC_PYTHON", default_value = "python3")]
        python: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Recordings of the wake word to use instead of recording new ones,
        /// e.g. voice notes the device uploaded. The last `--test-takes` of
        /// them only test the model.
        files: Vec<PathBuf>,
    },
    /// Train a model and write it as a `.fkw` file.
    Train {
        /// Dataset directory created by `fennec synth`.
        #[arg(long)]
        data: PathBuf,
        /// Output model file.
        #[arg(long)]
        out: PathBuf,
        /// Extra directories of background recordings (WAV). Can be repeated.
        #[arg(long = "background")]
        backgrounds: Vec<PathBuf>,
        /// Extra directories of non-speech sounds the wake word must ignore,
        /// like coughs, doors or dishes (WAV). Can be repeated.
        #[arg(long = "sounds")]
        sounds: Vec<PathBuf>,
        #[arg(long, default_value_t = 6000)]
        steps: usize,
        #[arg(long, default_value_t = 256)]
        batch_size: usize,
        #[arg(long, default_value_t = 1e-3)]
        learning_rate: f64,
        /// Window the model looks at, in 10 ms frames.
        #[arg(long, default_value_t = 150)]
        frames: usize,
        /// Scales the network: 0.7 halves the compute, 1.4 doubles it.
        #[arg(long, default_value_t = 1.0)]
        width: f32,
        /// Minutes of held-out audio used to measure false accepts.
        #[arg(long, default_value_t = 60.0)]
        eval_minutes: f32,
        /// False accepts per hour allowed when choosing the threshold.
        #[arg(long, default_value_t = 0.5)]
        max_false_accepts: f32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Measure an existing model on a dataset's held-out clips.
    Eval {
        model: PathBuf,
        #[arg(long)]
        data: PathBuf,
        #[arg(long = "background")]
        backgrounds: Vec<PathBuf>,
        #[arg(long = "sounds")]
        sounds: Vec<PathBuf>,
        #[arg(long, default_value_t = 60.0)]
        eval_minutes: f32,
        /// Also list the held-out negative clips that score highest.
        #[arg(long, value_name = "N")]
        worst: Option<usize>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Evaluations in a row that must reach the threshold, as
        /// `wake_word.consecutive` on the device.
        #[arg(long, default_value_t = eval::CONSECUTIVE)]
        consecutive: usize,
    },
    /// Run a model on a WAV file, or on raw 16 kHz 16-bit mono audio from
    /// stdin when INPUT is `-` (e.g. piped from `arecord`).
    Listen {
        model: PathBuf,
        #[arg(default_value = "-")]
        input: PathBuf,
        /// Override the threshold stored in the model.
        #[arg(long)]
        threshold: Option<f32>,
        /// Print every score, not just detections.
        #[arg(long)]
        scores: bool,
    },
    /// Write augmented training windows as WAV files, to hear what the model
    /// is trained on.
    Preview {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long = "background")]
        backgrounds: Vec<PathBuf>,
        #[arg(long = "sounds")]
        sounds: Vec<PathBuf>,
        #[arg(long, default_value_t = 20)]
        count: usize,
        #[arg(long, default_value_t = 150)]
        frames: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Flash a device over USB: firmware, configuration and wake word model.
    Flash {
        /// Device configuration (see firmware/fennec.example.toml).
        #[arg(long)]
        config: PathBuf,
        /// Wake word model; defaults to `wake_word.model` in the
        /// configuration.
        #[arg(long)]
        model: Option<PathBuf>,
        /// Directory with a built firmware (`nix build .#firmware`). Defaults
        /// to the firmware's cargo target directory.
        #[arg(long, env = "FENNEC_FIRMWARE_DIR")]
        firmware_dir: Option<PathBuf>,
        /// Serial port, e.g. /dev/ttyUSB0.
        #[arg(long)]
        port: Option<String>,
        /// Do not open the serial monitor afterwards.
        #[arg(long)]
        no_monitor: bool,
    },
    /// Update a device over Wi-Fi. It needs `ota.password` in its
    /// configuration.
    Ota {
        /// The device's address: an IP, or `fennec-<name>` if your router
        /// resolves DHCP host names.
        #[arg(long)]
        host: String,
        /// Device configuration, for the password and the settings to send.
        #[arg(long)]
        config: PathBuf,
        /// Send the firmware.
        #[arg(long)]
        firmware: bool,
        /// Send the settings from the configuration file.
        #[arg(long)]
        settings: bool,
        /// Send the wake word model.
        #[arg(long)]
        model: bool,
        /// Send this model instead of `wake_word.model`.
        #[arg(long, requires = "model")]
        model_file: Option<PathBuf>,
        /// Send firmware, settings and model.
        #[arg(long)]
        all: bool,
        #[arg(long, env = "FENNEC_FIRMWARE_DIR")]
        firmware_dir: Option<PathBuf>,
    },
    /// Encode a device configuration into the binary stored on the device.
    Config {
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Show what a `.fkw` file contains.
    Inspect { model: PathBuf },
}

fn default_workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(1, 32))
}

fn load_model(words: &[u32], len: usize) -> anyhow::Result<Model<'_>> {
    Ok(Model::from_bytes(&train::bytemuck_bytes(words)[..len])?)
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Voices { dir, names } => {
            let names = if names.is_empty() {
                voices::DEFAULT_VOICES
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                names
            };
            voices::download_voices(&dir, &names)
        }
        Command::Synth {
            phrase,
            out,
            voices,
            positives,
            negatives,
            confusables,
            workers,
            python,
            seed,
        } => synth::synth(&synth::SynthOptions {
            phrase,
            out,
            voices,
            positives,
            negatives,
            confusables,
            workers,
            python,
            seed,
        }),
        Command::Record {
            data,
            kind,
            count,
            seconds,
            device,
        } => record::record(&data, kind, count, seconds, device.as_deref()),
        Command::Enroll {
            phrase,
            data,
            voices,
            out,
            takes,
            test_takes,
            room_seconds,
            device,
            confusables,
            steps,
            workers,
            python,
            seed,
            files,
        } => enroll::enroll(&enroll::EnrollOptions {
            phrase,
            data,
            voices,
            out,
            files,
            takes,
            test_takes,
            room_seconds,
            device,
            confusables,
            steps,
            workers,
            python,
            seed,
        }),
        Command::Train {
            data,
            out,
            backgrounds,
            sounds,
            steps,
            batch_size,
            learning_rate,
            frames,
            width,
            eval_minutes,
            max_false_accepts,
            seed,
        } => train::train(&train::TrainOptions {
            data,
            out,
            backgrounds,
            sounds,
            steps,
            batch_size,
            learning_rate,
            frames,
            width,
            eval_minutes,
            max_false_accepts_per_hour: max_false_accepts,
            seed,
        }),
        Command::Eval {
            model,
            data,
            backgrounds,
            sounds,
            eval_minutes,
            worst,
            seed,
            consecutive,
        } => {
            let (words, len) = train::read_model(&model)?;
            let model = load_model(&words, len)?;
            let dataset = dataset::Dataset::load(&data, &backgrounds, &sounds)?;
            println!("{}", dataset.summary());
            let report = eval::evaluate(&model, &dataset, eval_minutes, seed, consecutive)?;
            report.print(model.header.threshold);
            eval::false_wakes(&model, &data, model.header.threshold)?;
            if let Some(worst) = worst {
                eval::diagnose(&model, &dataset, &data, &sounds, worst)?;
            }
            Ok(())
        }
        Command::Listen {
            model,
            input,
            threshold,
            scores,
        } => {
            let (words, len) = train::read_model(&model)?;
            listen::listen(load_model(&words, len)?, &input, threshold, scores)
        }
        Command::Preview {
            data,
            out,
            backgrounds,
            sounds,
            count,
            frames,
            seed,
        } => {
            use rand::SeedableRng;
            let dataset = dataset::Dataset::load(&data, &backgrounds, &sounds)?;
            let aug = dataset::Augmenter {
                split: &dataset.train,
                backgrounds: &dataset.backgrounds,
                frames,
                hard_negatives: &[],
            };
            std::fs::create_dir_all(&out)?;
            let mut rng = rand::rngs::Xoshiro256PlusPlus::seed_from_u64(seed);
            for i in 0..count {
                for (kind, name) in [
                    (dataset::Kind::Positive, "positive"),
                    (dataset::Kind::Negative, "negative"),
                ] {
                    let path = out.join(format!("{name}-{i:03}.wav"));
                    audio::write_wav(&path, &aug.audio(kind, &mut rng))?;
                }
            }
            println!("wrote {} windows to {}", 2 * count, out.display());
            Ok(())
        }
        Command::Flash {
            config,
            model,
            firmware_dir,
            port,
            no_monitor,
        } => device::flash(&device::FlashOptions {
            config: &config,
            model: model.as_deref(),
            firmware_dir: firmware_dir.as_deref(),
            port: port.as_deref(),
            monitor: !no_monitor,
        }),
        Command::Ota {
            host,
            config,
            firmware,
            settings,
            model,
            model_file,
            all,
            firmware_dir,
        } => device::ota(&device::OtaOptions {
            host: &host,
            config: &config,
            firmware: firmware || all,
            settings: settings || all,
            model: model || all,
            model_file: model_file.as_deref(),
            firmware_dir: firmware_dir.as_deref(),
        }),
        Command::Config { config, out } => device::write_config(&config, &out),
        Command::Inspect { model: path } => {
            let (words, len) = train::read_model(&path)?;
            let model = load_model(&words, len)?;
            let h = &model.header;
            println!("{}", path.display());
            println!("  wake word:   {:?}", h.name());
            println!("  threshold:   {:.2}", h.threshold);
            println!(
                "  window:      {} frames ({:.2}s)",
                h.frames,
                h.frames as f32 / 100.0
            );
            println!("  weights:     {} ({} KB)", h.weight_count(), len / 1024);
            for (i, c) in h.convs.iter().enumerate() {
                println!(
                    "  conv {i}:      {} -> {} channels, kernel {}, stride {}",
                    c.in_channels, c.out_channels, c.kernel, c.stride
                );
            }
            // The firmware logs the same number at startup; if they differ,
            // the device computes something else than the training tools.
            let silence = vec![0.0; fennec_core::frontend::N_MELS * h.frames];
            let p = model.probability(&silence, &mut Default::default());
            println!("  silence:     score {p:.4}");
            Ok(())
        }
    }
}
