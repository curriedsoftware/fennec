//! Fennec firmware for the M5Stack Atom Echo.
//!
//! Listens for the wake word (or a button press), records what is said next
//! and posts it as a WAV document to a Telegram chat. After a successful
//! upload the speaker plays a chime; it shares a pin with the microphone, so
//! the two take turns (see `audio.rs`).
//!
//! The image is the same for every device. The configuration and the wake
//! word model live in their own flash partitions (see `partitions.csv`),
//! written by `fennec flash` over USB or `fennec ota` over Wi-Fi.

mod audio;
mod led;
mod mic;
#[cfg(feature = "opus")]
mod opus;
mod ota;
mod preroll;
#[cfg(not(feature = "opus"))]
mod recording;
mod speaker;
mod storage;
#[cfg(feature = "telegram")]
mod telegram;
#[cfg(feature = "opus")]
mod voice;
mod wifi;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::gpio::{InputMode, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::ota::EspOta;

use fennec_core::config::Config;
use fennec_core::detector::{Detector, DetectorConfig};
use fennec_core::endpoint::{self, Endpoint, Endpointer};

use audio::Audio;
use led::{Led, Status};
use mic::Mic;
use preroll::Preroll;
#[cfg(not(feature = "opus"))]
pub use recording::Recording;
#[cfg(feature = "opus")]
pub use voice::Recording;
#[cfg(all(feature = "telegram", not(feature = "opus")))]
use recording::UPLOAD;
#[cfg(feature = "opus")]
use voice::UPLOAD;

/// Volume of the speaker test chime, until it is configurable.
const SPEAKER_VOLUME: f32 = 0.3;

#[derive(Debug, Clone, Copy)]
enum Trigger {
    WakeWord(f32),
    Button,
}

impl Trigger {
    fn describe(self) -> String {
        match self {
            Trigger::WakeWord(p) => format!("wake_word score={p:.2}"),
            Trigger::Button => "button".into(),
        }
    }
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("fennec {}", env!("CARGO_PKG_VERSION"));

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;
    let mut led = Led::new(pins.gpio27, 40)?;

    let config = match storage::load_config() {
        Ok(config) => config,
        Err(e) => {
            log::error!("{e:#}; write one with `fennec flash --config <file>`");
            blink_forever(&mut led);
        }
    };
    log::info!("device {:?}", config.device_name);
    led.set_brightness(config.led_brightness);
    led.set(Status::Connecting);

    // The button pulls GPIO39 low; the board has its own pull-up.
    let button = PinDriver::input(pins.gpio39, Pull::Floating)?;

    let mut wifi = wifi::Wifi::new(peripherals.modem, &config)?;
    while let Err(e) = wifi.ensure_connected() {
        log::error!("{e:#}");
        led.flash(Status::Error, 300, Status::Connecting);
        std::thread::sleep(Duration::from_secs(5));
    }
    // Reaching the network proves an updated image works; without this the
    // bootloader would go back to the previous one on the next reset.
    if let Err(e) = EspOta::new().and_then(|mut ota| ota.mark_running_slot_valid()) {
        log::warn!("cannot mark this firmware as valid: {e}");
    }

    let model = storage::load_model();
    let wake_word = match &model {
        Ok(model) => model.header.name().to_string(),
        Err(e) => {
            log::warn!("{e:#}; only the button will start a recording");
            String::new()
        }
    };
    let _server = ota::start(
        config.ota_password.clone(),
        ota::Status {
            name: config.device_name.clone(),
            wake_word: wake_word.clone(),
        },
    )?;
    if config.ota_password.is_empty() {
        log::info!("over-the-air updates are disabled (no ota.password)");
    }

    let mut audio = Audio {
        i2s: peripherals.i2s0,
        gpio33: pins.gpio33,
        gpio23: pins.gpio23,
        gpio19: pins.gpio19,
        gpio22: pins.gpio22,
    };
    let mut mic = audio.mic(&config)?;

    let mut detector = match model {
        Ok(model) if config.wake_word_enabled => {
            let started = Instant::now();
            let silence = vec![0.0; fennec_core::frontend::N_MELS * model.header.frames];
            let p_silence = model.probability(&silence, &mut Default::default());
            let inference = started.elapsed();
            // The score on silence is the same on every machine running this
            // model; compare it with `fennec inspect` to check the maths.
            log::info!(
                "wake word inference takes {} ms (score on silence {p_silence:.4})",
                inference.as_millis()
            );
            if inference > Duration::from_millis(80) {
                log::warn!("inference is too slow to run every 100 ms; detections may lag");
            }
            let mut detector_config = DetectorConfig::for_model(&model);
            if let Some(threshold) = config.wake_threshold {
                detector_config.threshold = threshold;
            }
            detector_config.consecutive = config.wake_consecutive as usize;
            log::info!(
                "listening for {wake_word:?} (threshold {:.2})",
                detector_config.threshold
            );
            Some(Detector::new(model, detector_config))
        }
        _ => {
            log::info!("wake word disabled; press the button to talk");
            None
        }
    };
    let mut endpointer = Endpointer::new(config.endpoint);

    led.set(Status::Idle);
    let mut near_miss = NearMiss::default();
    let mut preroll = Preroll::new(config.button_preroll_ms.max(config.wake_preroll_ms));
    let mut frame = [0i16; endpoint::FRAME];
    let mut button_was_down = false;
    let mut updating = false;
    loop {
        mic.read(&mut frame)?;
        if ota::UPDATING.load(Ordering::Relaxed) {
            if !updating {
                updating = true;
                led.set(Status::Updating);
                // The update may need the memory.
                preroll.clear();
            }
            continue;
        }
        if updating {
            // A failed update; the device restarts after successful ones.
            updating = false;
            led.set(Status::Idle);
        }
        preroll.push(&frame);

        let button_down = button.is_low();
        let pressed = button_down && !button_was_down;
        button_was_down = button_down;

        let trigger = if pressed {
            Some(Trigger::Button)
        } else {
            detector.as_mut().and_then(|d| {
                let detection = d.push(&frame);
                if let Some(p) = d.take_evaluation() {
                    near_miss.observe(p, d.config.threshold);
                }
                detection.map(|d| Trigger::WakeWord(d.probability))
            })
        };
        let Some(trigger) = trigger else {
            endpointer.observe_background(&frame);
            continue;
        };

        log::info!("triggered by {}", trigger.describe());
        near_miss = NearMiss::default();
        led.set(Status::Listening);
        let preroll_ms = match trigger {
            Trigger::Button => config.button_preroll_ms,
            Trigger::WakeWord(_) => config.wake_preroll_ms,
        };
        let queued = if preroll_ms > 0 {
            preroll.start_queue(preroll_ms);
            Some(&mut preroll)
        } else {
            preroll.clear();
            None
        };
        let (recording, end, preroll_seconds) = record(&mut mic, &mut endpointer, &button, queued)?;
        preroll.clear();
        log::info!(
            "recorded {:.1}s ({end:?}, {preroll_seconds:.1}s before the trigger), {} bytes, {} bytes of heap free",
            recording.seconds(),
            recording.bytes(),
            unsafe { esp_idf_svc::sys::esp_get_free_heap_size() }
        );

        // Audio from before the trigger is worth sending on its own: it may
        // hold a wake word that was missed, or what caused a false wake-up.
        if end == Endpoint::NoSpeech && preroll_seconds == 0.0 {
            led.flash(Status::Off, 400, Status::Idle);
        } else {
            led.set(Status::Uploading);
            match upload(&mut wifi, &config, &recording, trigger, preroll_seconds) {
                Ok(()) => {
                    led.flash(Status::Success, 600, Status::Idle);
                    // The speaker needs the microphone's clock pin.
                    drop(mic);
                    if let Err(e) = audio.speaker(SPEAKER_VOLUME).and_then(|mut s| s.chime()) {
                        log::error!("cannot play the chime: {e}");
                    }
                    mic = audio.mic(&config)?;
                }
                Err(e) => {
                    log::error!("upload failed: {e:#}");
                    led.flash(Status::Error, 1500, Status::Idle);
                }
            }
        }
        drop(recording);

        // Audio queued while busy is stale; drop it and start over.
        mic.discard(300)?;
        if let Some(detector) = &mut detector {
            detector.reset();
        }
        button_was_down = button.is_low();
    }
}

/// Logs the highest wake word score of every 2 s that came within reach of
/// the threshold without triggering, to help choose `wake_word.threshold`
/// and `wake_word.consecutive`. A peak above the threshold means it was
/// reached, but not on enough consecutive evaluations.
#[derive(Default)]
struct NearMiss {
    peak: f32,
    evaluations: u32,
}

impl NearMiss {
    /// Evaluations run every 100 ms.
    const PERIOD: u32 = 20;

    fn observe(&mut self, probability: f32, threshold: f32) {
        self.peak = self.peak.max(probability);
        self.evaluations += 1;
        if self.evaluations < Self::PERIOD {
            return;
        }
        if self.peak >= threshold / 2.0 {
            log::info!(
                "wake word score peaked at {:.2} (threshold {threshold:.2})",
                self.peak
            );
        }
        *self = Self::default();
    }
}

/// Without a configuration there is nothing to do but say so.
fn blink_forever(led: &mut Led<'_>) -> ! {
    loop {
        led.flash(Status::Error, 150, Status::Off);
        std::thread::sleep(Duration::from_millis(150));
        led.flash(Status::Error, 150, Status::Off);
        std::thread::sleep(Duration::from_millis(1500));
    }
}

fn record(
    mic: &mut Mic<'_>,
    endpointer: &mut Endpointer,
    button: &PinDriver<'_, impl InputMode>,
    mut preroll: Option<&mut Preroll>,
) -> anyhow::Result<(Recording, Endpoint, f32)> {
    let mut recording = Recording::new()?;
    let preroll_seconds = preroll.as_ref().map_or(0.0, |p| p.seconds());
    let mut frame = [0i16; endpoint::FRAME];
    endpointer.start();
    // Ignore the press that may have started the recording, and contact
    // bounce right after it.
    const BUTTON_IGNORE_FRAMES: usize = 30;
    let mut button_was_down = true;
    let mut frames = 0usize;
    let end = loop {
        mic.read(&mut frame)?;
        frames += 1;
        let button_down = button.is_low();
        let pressed = button_down && !button_was_down && frames > BUTTON_IGNORE_FRAMES;
        button_was_down = button_down;

        let stored = match preroll.as_deref_mut() {
            Some(preroll) => {
                preroll.push(&frame);
                feed(preroll, &mut recording)
            }
            None => recording.push(&frame),
        };
        let end = endpointer.feed(&frame);
        if pressed {
            break if endpointer.heard_speech() {
                Endpoint::Silence
            } else {
                Endpoint::NoSpeech
            };
        }
        if !stored {
            break Endpoint::MaxLength;
        }
        if end != Endpoint::Continue {
            break end;
        }
    };
    if let Some(preroll) = preroll {
        preroll.finish();
        while let Some(frame) = preroll.peek() {
            if !recording.push(&frame) {
                break;
            }
            preroll.advance();
        }
    }
    recording.finish();
    Ok((recording, end, preroll_seconds))
}

/// Hands the recording as much queued audio as it takes without waiting,
/// so that the microphone keeps being read. Returns `false` once the
/// recording is full.
fn feed(preroll: &mut Preroll, recording: &mut Recording) -> bool {
    while let Some(frame) = preroll.peek() {
        match recording.try_push(&frame) {
            Some(true) => preroll.advance(),
            Some(false) => return false,
            None => break,
        }
    }
    true
}

#[cfg(feature = "telegram")]
fn upload(
    wifi: &mut wifi::Wifi<'_>,
    config: &Config,
    recording: &Recording,
    trigger: Trigger,
    preroll_seconds: f32,
) -> anyhow::Result<()> {
    wifi.ensure_connected()?;
    let mut caption = format!(
        "#fennec device={} trigger={} duration={:.1}s",
        config.device_name,
        trigger.describe(),
        recording.seconds()
    );
    if preroll_seconds > 0.0 {
        caption += &format!(" preroll={preroll_seconds:.1}s");
    }
    let name = format!("{}-{}", config.device_name, uptime_seconds());
    let started = Instant::now();
    let mut result = telegram::send(config, recording, &UPLOAD, &caption, &name);
    if let Err(e) = &result {
        log::warn!("upload failed ({e:#}), retrying once");
        std::thread::sleep(Duration::from_secs(1));
        wifi.ensure_connected()?;
        result = telegram::send(config, recording, &UPLOAD, &caption, &name);
    }
    if result.is_ok() {
        log::info!("uploaded in {:.1}s", started.elapsed().as_secs_f32());
    }
    result
}

/// Without the `telegram` feature there is nowhere to send recordings to.
#[cfg(not(feature = "telegram"))]
fn upload(
    _wifi: &mut wifi::Wifi<'_>,
    _config: &Config,
    recording: &Recording,
    trigger: Trigger,
    _preroll_seconds: f32,
) -> anyhow::Result<()> {
    log::info!(
        "built without the `telegram` feature: dropping {:.1}s recording ({})",
        recording.seconds(),
        trigger.describe()
    );
    Ok(())
}

#[cfg_attr(not(feature = "telegram"), allow(dead_code))]
fn uptime_seconds() -> u64 {
    (unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1_000_000) as u64
}
