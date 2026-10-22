//! Configuring, flashing and updating devices.
//!
//! The firmware image is the same for every device. What makes a device
//! yours (Wi-Fi, Telegram, audio settings and the wake word model) lives in
//! two flash partitions that these commands write: over USB with
//! `espflash`, or over Wi-Fi through the firmware's update endpoint.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use fennec_core::config::Config;
use fennec_core::endpoint::EndpointConfig;
use serde::Deserialize;

/// The device configuration file (`fennec.toml`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    wifi: Wifi,
    telegram: Telegram,
    #[serde(default)]
    device: Device,
    #[serde(default)]
    wake_word: WakeWord,
    #[serde(default)]
    audio: Audio,
    #[serde(default)]
    ota: Ota,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wifi {
    ssid: String,
    #[serde(default)]
    password: String,
    channel: Option<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Telegram {
    bot_token: String,
    chat_id: ChatId,
    message_thread_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChatId {
    Id(i64),
    Name(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Device {
    name: String,
    led_brightness: u8,
}

impl Default for Device {
    fn default() -> Self {
        let defaults = Config::default();
        Self {
            name: defaults.device_name,
            led_brightness: defaults.led_brightness,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct WakeWord {
    enabled: bool,
    /// Relative to the configuration file.
    model: PathBuf,
    threshold: Option<f32>,
    consecutive: u8,
}

impl Default for WakeWord {
    fn default() -> Self {
        Self {
            enabled: true,
            model: PathBuf::new(),
            threshold: None,
            consecutive: Config::default().wake_consecutive,
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum MicChannel {
    Left,
    Right,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Audio {
    mic_gain_db: f32,
    mic_channel: MicChannel,
    silence_ms: u32,
    no_speech_ms: u32,
    max_recording_seconds: u32,
    speech_margin_db: f32,
    button_preroll_ms: u32,
    wake_preroll_ms: u32,
}

impl Default for Audio {
    fn default() -> Self {
        let defaults = Config::default();
        Self {
            mic_gain_db: defaults.mic_gain_db,
            mic_channel: MicChannel::Right,
            silence_ms: defaults.endpoint.silence_ms,
            no_speech_ms: defaults.endpoint.no_speech_ms,
            max_recording_seconds: defaults.endpoint.max_ms / 1000,
            speech_margin_db: defaults.endpoint.speech_margin_db,
            button_preroll_ms: defaults.button_preroll_ms,
            wake_preroll_ms: defaults.wake_preroll_ms,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct Ota {
    password: String,
}

/// A parsed and validated device configuration file.
pub struct DeviceConfig {
    pub config: Config,
    /// The wake word model to install, if the file names one.
    pub model: Option<PathBuf>,
}

impl DeviceConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let file: File =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let config = Config {
            wifi_ssid: file.wifi.ssid,
            wifi_password: file.wifi.password,
            wifi_channel: file.wifi.channel,
            bot_token: file.telegram.bot_token,
            chat_id: match file.telegram.chat_id {
                ChatId::Id(id) => id.to_string(),
                ChatId::Name(name) => name,
            },
            message_thread_id: file.telegram.message_thread_id,
            device_name: file.device.name,
            led_brightness: file.device.led_brightness,
            wake_word_enabled: file.wake_word.enabled,
            wake_threshold: file.wake_word.threshold,
            wake_consecutive: file.wake_word.consecutive,
            mic_gain_db: file.audio.mic_gain_db,
            mic_right_channel: matches!(file.audio.mic_channel, MicChannel::Right),
            endpoint: EndpointConfig {
                speech_margin_db: file.audio.speech_margin_db,
                silence_ms: file.audio.silence_ms,
                no_speech_ms: file.audio.no_speech_ms,
                max_ms: file.audio.max_recording_seconds.saturating_mul(1000),
                ..EndpointConfig::default()
            },
            button_preroll_ms: file.audio.button_preroll_ms,
            wake_preroll_ms: file.audio.wake_preroll_ms,
            ota_password: file.ota.password,
        };
        config
            .validate()
            .with_context(|| format!("invalid {}", path.display()))?;
        let model = (!file.wake_word.model.as_os_str().is_empty()).then(|| {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&file.wake_word.model)
        });
        if let Some(model) = &model {
            check_model(model)?;
        }
        Ok(Self { config, model })
    }
}

fn check_model(path: &Path) -> anyhow::Result<()> {
    let (words, len) = crate::train::read_model(path)?;
    fennec_core::model::Model::from_bytes(&crate::train::bytemuck_bytes(&words)[..len])
        .with_context(|| format!("invalid wake word model {}", path.display()))?;
    Ok(())
}

/// Where the built firmware lives.
pub struct Firmware {
    pub elf: PathBuf,
    /// App image for over-the-air updates; made from `elf` when missing.
    pub image: Option<PathBuf>,
    pub bootloader: PathBuf,
    pub partitions: PathBuf,
}

impl Firmware {
    /// Finds the firmware in `dir` (the output of `nix build .#firmware`),
    /// or else in the firmware's cargo target directory.
    pub fn locate(dir: Option<&Path>) -> anyhow::Result<Self> {
        if let Some(dir) = dir {
            let firmware = Self {
                elf: dir.join("fennec-firmware.elf"),
                image: Some(dir.join("fennec-firmware.bin")),
                bootloader: dir.join("bootloader.bin"),
                partitions: dir.join("partitions.csv"),
            };
            for path in [&firmware.elf, &firmware.bootloader, &firmware.partitions] {
                if !path.exists() {
                    bail!("{} is missing", path.display());
                }
            }
            return Ok(firmware);
        }
        let root = Path::new("firmware");
        let release = root.join("target/xtensa-esp32-espidf/release");
        let elf = release.join("fennec-firmware");
        if !elf.exists() {
            bail!(
                "no firmware at {}; build it with `cargo build --release` in firmware/, \
                 or pass --firmware-dir (e.g. the result of `nix build .#firmware`)",
                elf.display()
            );
        }
        // esp-idf-sys builds the bootloader next to its own output; take the
        // newest one.
        let bootloader = std::fs::read_dir(release.join("build"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("out/build/bootloader/bootloader.bin"))
            .filter(|p| p.exists())
            .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
            .context("cannot find the bootloader built by esp-idf-sys")?;
        Ok(Self {
            elf,
            image: None,
            bootloader,
            partitions: root.join("partitions.csv"),
        })
    }

    /// The app image for over-the-air updates.
    fn image(&self, scratch: &Path) -> anyhow::Result<PathBuf> {
        if let Some(image) = &self.image
            && image.exists()
        {
            return Ok(image.clone());
        }
        let image = scratch.join("fennec-firmware.bin");
        run(Command::new("espflash")
            .args(["save-image", "--chip", "esp32", "--flash-freq", FLASH_FREQ])
            .arg("--partition-table")
            .arg(&self.partitions)
            .arg(&self.elf)
            .arg(&image))?;
        Ok(image)
    }
}

/// Offset of the partition named `label` in an ESP-IDF partition table CSV.
fn partition_offset(csv: &Path, label: &str) -> anyhow::Result<u32> {
    let text = std::fs::read_to_string(csv)?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.first() == Some(&label) {
            let offset = fields.get(3).context("partition without offset")?;
            let offset = offset.strip_prefix("0x").unwrap_or(offset);
            return Ok(u32::from_str_radix(offset, 16)?);
        }
    }
    bail!("no {label} partition in {}", csv.display())
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let status = cmd
        .status()
        .with_context(|| format!("cannot run {program}; is it installed?"))?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}

/// SPI flash clock written into the bootloader and app image headers. It must
/// match `CONFIG_ESPTOOLPY_FLASHFREQ` in `firmware/sdkconfig.defaults`; the
/// wake word model is read from flash on every inference.
const FLASH_FREQ: &str = "80mhz";

fn espflash(port: Option<&str>, args: &[&str]) -> Command {
    let mut cmd = Command::new("espflash");
    cmd.args(args);
    if let Some(port) = port {
        cmd.args(["--port", port]);
    }
    cmd
}

fn scratch_dir() -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Writes a device configuration partition image.
pub fn write_config(config: &Path, out: &Path) -> anyhow::Result<()> {
    let device = DeviceConfig::load(config)?;
    std::fs::write(out, device.config.encode())?;
    println!("wrote {}", out.display());
    Ok(())
}

pub struct FlashOptions<'a> {
    pub config: &'a Path,
    pub model: Option<&'a Path>,
    pub firmware_dir: Option<&'a Path>,
    pub port: Option<&'a str>,
    pub monitor: bool,
}

/// Flashes firmware, configuration and model over USB.
pub fn flash(opts: &FlashOptions<'_>) -> anyhow::Result<()> {
    let device = DeviceConfig::load(opts.config)?;
    let model = opts.model.map(Path::to_path_buf).or(device.model);
    if let Some(model) = &model {
        check_model(model)?;
    }
    let firmware = Firmware::locate(opts.firmware_dir)?;
    let scratch = scratch_dir()?;
    let config_bin = scratch.join("config.bin");
    std::fs::write(&config_bin, device.config.encode())?;

    println!("flashing firmware...");
    let mut cmd = espflash(
        opts.port,
        &[
            "flash",
            "--chip",
            "esp32",
            "--flash-freq",
            FLASH_FREQ,
            "--after",
            "no-reset",
        ],
    );
    cmd.arg("--bootloader")
        .arg(&firmware.bootloader)
        .arg("--partition-table")
        .arg(&firmware.partitions)
        // Boot the slot that is being written, not one left by an update.
        .args(["--erase-parts", "otadata"])
        .arg(&firmware.elf);
    run(&mut cmd)?;

    println!(
        "writing configuration for {:?}...",
        device.config.device_name
    );
    let offset = partition_offset(&firmware.partitions, "fennec_cfg")?;
    let after = if model.is_some() {
        "no-reset"
    } else {
        "hard-reset"
    };
    run(espflash(
        opts.port,
        &["write-bin", "--chip", "esp32", "--after", after],
    )
    .arg(format!("{offset:#x}"))
    .arg(&config_bin))?;

    if let Some(model) = &model {
        println!("writing wake word model {}...", model.display());
        let offset = partition_offset(&firmware.partitions, "fennec_model")?;
        run(espflash(opts.port, &["write-bin", "--chip", "esp32"])
            .arg(format!("{offset:#x}"))
            .arg(model))?;
    } else {
        println!("no wake word model given; keeping the one on the device, if any");
    }
    std::fs::remove_dir_all(&scratch).ok();

    if opts.monitor {
        run(&mut espflash(opts.port, &["monitor", "--chip", "esp32"]))?;
    }
    Ok(())
}

pub struct OtaOptions<'a> {
    pub host: &'a str,
    pub config: &'a Path,
    pub firmware: bool,
    pub settings: bool,
    pub model: bool,
    /// Model to send instead of the one named in the configuration.
    pub model_file: Option<&'a Path>,
    pub firmware_dir: Option<&'a Path>,
}

fn base_url(host: &str) -> String {
    if host.starts_with("http://") || host.starts_with("https://") {
        host.trim_end_matches('/').to_string()
    } else {
        format!("http://{host}")
    }
}

fn status(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<String> {
    Ok(client
        .get(format!("{url}/"))
        .timeout(Duration::from_secs(3))
        .send()?
        .error_for_status()?
        .text()?)
}

/// Sends updates over Wi-Fi, one at a time: the device restarts after each.
pub fn ota(opts: &OtaOptions<'_>) -> anyhow::Result<()> {
    let device = DeviceConfig::load(opts.config)?;
    let password = &device.config.ota_password;
    if password.is_empty() {
        bail!(
            "{} has no ota.password; set one and flash it over USB once",
            opts.config.display()
        );
    }
    let url = base_url(opts.host);
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("fennec/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let before =
        status(&client, &url).with_context(|| format!("cannot reach the device at {url}"))?;
    println!("device: {}", before.trim());

    let scratch = scratch_dir()?;
    let mut updates: Vec<(&str, Vec<u8>)> = Vec::new();
    if opts.firmware {
        let firmware = Firmware::locate(opts.firmware_dir)?;
        updates.push(("firmware", std::fs::read(firmware.image(&scratch)?)?));
    }
    if opts.settings {
        updates.push(("config", device.config.encode()));
    }
    if opts.model {
        let model = opts
            .model_file
            .map(Path::to_path_buf)
            .or(device.model.clone())
            .context("no model to send: set wake_word.model or pass --model-file")?;
        check_model(&model)?;
        updates.push(("model", std::fs::read(&model)?));
    }
    std::fs::remove_dir_all(&scratch).ok();
    if updates.is_empty() {
        bail!("nothing to update; pass --firmware, --settings, --model or --all");
    }

    for (kind, body) in updates {
        println!("sending {kind} ({} KB)...", body.len() / 1024);
        let response = client
            .post(format!("{url}/update/{kind}"))
            .bearer_auth(password)
            .timeout(Duration::from_secs(300))
            .body(body)
            .send()
            .with_context(|| format!("cannot send the {kind} update"))?;
        let code = response.status();
        let text = response.text().unwrap_or_default();
        if !code.is_success() {
            bail!(
                "the device refused the {kind} update ({code}): {}",
                text.trim()
            );
        }
        println!("{}", text.trim());
        wait_for_restart(&client, &url)?;
    }
    Ok(())
}

fn wait_for_restart(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<()> {
    std::thread::sleep(Duration::from_secs(3));
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(90) {
        if let Ok(status) = status(client, url) {
            println!("back online: {}", status.trim());
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    bail!("the device did not come back within 90 s; check its serial log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../firmware/fennec.example.toml");
        let device = DeviceConfig::load(&path).unwrap();
        assert_eq!(device.config.device_name, "kitchen");
        let encoded = device.config.encode();
        assert_eq!(Config::decode(&encoded).unwrap(), device.config);
    }

    #[test]
    fn finds_partition_offsets() {
        let csv = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../firmware/partitions.csv");
        assert_eq!(partition_offset(&csv, "fennec_cfg").unwrap(), 0x360000);
        assert_eq!(partition_offset(&csv, "fennec_model").unwrap(), 0x370000);
    }
}
