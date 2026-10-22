//! Device configuration, stored in its own flash partition.
//!
//! The firmware image is the same for every device; Wi-Fi, Telegram and
//! audio settings are written separately by `fennec flash` or `fennec ota`.
//!
//! # Layout
//!
//! ```text
//! magic "FNCF", version (u32), payload length (u32), payload, CRC-32 (u32)
//! payload: entries of key (u8), length (u16), value bytes
//! ```
//!
//! Integers are little endian. Unknown keys are skipped, so older firmware
//! can read configurations written by newer tools.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::endpoint::EndpointConfig;

pub const MAGIC: [u8; 4] = *b"FNCF";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub wifi_ssid: String,
    pub wifi_password: String,
    /// The channel the network is on (1-13), scanned first when connecting.
    pub wifi_channel: Option<u8>,
    pub bot_token: String,
    /// A numeric chat id or `@channelname`.
    pub chat_id: String,
    pub message_thread_id: Option<i64>,
    pub device_name: String,
    pub led_brightness: u8,
    pub wake_word_enabled: bool,
    /// Overrides the threshold stored in the model.
    pub wake_threshold: Option<f32>,
    pub wake_consecutive: u8,
    pub mic_gain_db: f32,
    pub mic_right_channel: bool,
    pub endpoint: EndpointConfig,
    /// Audio from before a button press to put at the start of the
    /// recording, e.g. a wake word the model missed; 0 disables it.
    pub button_preroll_ms: u32,
    /// Audio from before a wake word detection to put at the start of the
    /// recording, e.g. the sound behind a false wake-up; 0 disables it.
    pub wake_preroll_ms: u32,
    /// Password for over-the-air updates; empty disables them.
    pub ota_password: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            wifi_channel: None,
            bot_token: String::new(),
            chat_id: String::new(),
            message_thread_id: None,
            device_name: "atom-echo".into(),
            led_brightness: 40,
            wake_word_enabled: true,
            wake_threshold: None,
            wake_consecutive: 2,
            mic_gain_db: 12.0,
            mic_right_channel: true,
            endpoint: EndpointConfig::default(),
            button_preroll_ms: 0,
            wake_preroll_ms: 0,
            ota_password: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The partition holds no configuration (e.g. it was never written).
    Missing,
    Version(u32),
    Corrupt,
    Invalid(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing => write!(f, "no configuration has been written to this device"),
            Error::Version(v) => write!(f, "unsupported configuration version {v}"),
            Error::Corrupt => write!(f, "the stored configuration is corrupt"),
            Error::Invalid(why) => write!(f, "invalid configuration: {why}"),
        }
    }
}

impl core::error::Error for Error {}

mod key {
    pub const WIFI_SSID: u8 = 1;
    pub const WIFI_PASSWORD: u8 = 2;
    pub const BOT_TOKEN: u8 = 3;
    pub const CHAT_ID: u8 = 4;
    pub const MESSAGE_THREAD_ID: u8 = 5;
    pub const DEVICE_NAME: u8 = 6;
    pub const LED_BRIGHTNESS: u8 = 7;
    pub const WAKE_WORD_ENABLED: u8 = 8;
    pub const WAKE_THRESHOLD: u8 = 9;
    pub const WAKE_CONSECUTIVE: u8 = 10;
    pub const MIC_GAIN_DB: u8 = 11;
    pub const MIC_RIGHT_CHANNEL: u8 = 12;
    pub const SPEECH_MARGIN_DB: u8 = 13;
    pub const SILENCE_MS: u8 = 14;
    pub const NO_SPEECH_MS: u8 = 15;
    pub const MAX_MS: u8 = 16;
    pub const OTA_PASSWORD: u8 = 17;
    pub const WIFI_CHANNEL: u8 = 18;
    pub const BUTTON_PREROLL_MS: u8 = 19;
    pub const WAKE_PREROLL_MS: u8 = 20;
}

impl Config {
    /// Checks the values the firmware relies on.
    pub fn validate(&self) -> Result<(), Error> {
        if self.wifi_ssid.is_empty() || self.wifi_ssid.len() > 32 {
            return Err(Error::Invalid("wifi.ssid must be 1 to 32 bytes long"));
        }
        if self.wifi_password.len() > 64 {
            return Err(Error::Invalid(
                "wifi.password must be at most 64 bytes long",
            ));
        }
        if let Some(channel) = self.wifi_channel
            && !(1..=13).contains(&channel)
        {
            return Err(Error::Invalid("wifi.channel must be between 1 and 13"));
        }
        if !self.bot_token.contains(':') || self.bot_token.contains(char::is_whitespace) {
            return Err(Error::Invalid(
                "telegram.bot_token does not look like a token from @BotFather (`123456:ABC...`)",
            ));
        }
        if self.chat_id.is_empty() {
            return Err(Error::Invalid("telegram.chat_id is missing"));
        }
        if self.device_name.is_empty()
            || !self
                .device_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::Invalid(
                "device.name must be non-empty and use only letters, digits, '-' and '_'",
            ));
        }
        if !(0.0..=40.0).contains(&self.mic_gain_db) {
            return Err(Error::Invalid("audio.mic_gain_db must be between 0 and 40"));
        }
        if !(1_000..=30_000).contains(&self.endpoint.max_ms) {
            return Err(Error::Invalid(
                "audio.max_recording_seconds must be between 1 and 30",
            ));
        }
        if self.button_preroll_ms > 5_000 {
            return Err(Error::Invalid(
                "audio.button_preroll_ms must be at most 5000",
            ));
        }
        if self.wake_preroll_ms > 5_000 {
            return Err(Error::Invalid("audio.wake_preroll_ms must be at most 5000"));
        }
        if let Some(t) = self.wake_threshold
            && !(0.0..1.0).contains(&t)
        {
            return Err(Error::Invalid(
                "wake_word.threshold must be between 0 and 1",
            ));
        }
        if self.wake_consecutive == 0 {
            return Err(Error::Invalid("wake_word.consecutive must be at least 1"));
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        let mut put = |key: u8, value: &[u8]| {
            payload.push(key);
            payload.extend_from_slice(&(value.len() as u16).to_le_bytes());
            payload.extend_from_slice(value);
        };
        put(key::WIFI_SSID, self.wifi_ssid.as_bytes());
        put(key::WIFI_PASSWORD, self.wifi_password.as_bytes());
        if let Some(channel) = self.wifi_channel {
            put(key::WIFI_CHANNEL, &[channel]);
        }
        put(key::BOT_TOKEN, self.bot_token.as_bytes());
        put(key::CHAT_ID, self.chat_id.as_bytes());
        if let Some(thread) = self.message_thread_id {
            put(key::MESSAGE_THREAD_ID, &thread.to_le_bytes());
        }
        put(key::DEVICE_NAME, self.device_name.as_bytes());
        put(key::LED_BRIGHTNESS, &[self.led_brightness]);
        put(key::WAKE_WORD_ENABLED, &[self.wake_word_enabled as u8]);
        if let Some(t) = self.wake_threshold {
            put(key::WAKE_THRESHOLD, &t.to_le_bytes());
        }
        put(key::WAKE_CONSECUTIVE, &[self.wake_consecutive]);
        put(key::MIC_GAIN_DB, &self.mic_gain_db.to_le_bytes());
        put(key::MIC_RIGHT_CHANNEL, &[self.mic_right_channel as u8]);
        put(
            key::SPEECH_MARGIN_DB,
            &self.endpoint.speech_margin_db.to_le_bytes(),
        );
        put(key::SILENCE_MS, &self.endpoint.silence_ms.to_le_bytes());
        put(key::NO_SPEECH_MS, &self.endpoint.no_speech_ms.to_le_bytes());
        put(key::MAX_MS, &self.endpoint.max_ms.to_le_bytes());
        put(key::BUTTON_PREROLL_MS, &self.button_preroll_ms.to_le_bytes());
        put(key::WAKE_PREROLL_MS, &self.wake_preroll_ms.to_le_bytes());
        put(key::OTA_PASSWORD, self.ota_password.as_bytes());

        let mut out = Vec::with_capacity(16 + payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&crc32(&payload).to_le_bytes());
        out
    }

    /// Parses a configuration. Trailing bytes (e.g. the rest of the flash
    /// partition) are ignored.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.get(..4) != Some(&MAGIC) {
            return Err(Error::Missing);
        }
        let word = |at: usize| -> Result<u32, Error> {
            let b = bytes.get(at..at + 4).ok_or(Error::Corrupt)?;
            Ok(u32::from_le_bytes(b.try_into().unwrap()))
        };
        let version = word(4)?;
        if version != VERSION {
            return Err(Error::Version(version));
        }
        let len = word(8)? as usize;
        let payload = bytes.get(12..12 + len).ok_or(Error::Corrupt)?;
        if word(12 + len)? != crc32(payload) {
            return Err(Error::Corrupt);
        }

        let mut config = Config::default();
        let mut rest = payload;
        while !rest.is_empty() {
            let [key, l0, l1, tail @ ..] = rest else {
                return Err(Error::Corrupt);
            };
            let len = u16::from_le_bytes([*l0, *l1]) as usize;
            let value = tail.get(..len).ok_or(Error::Corrupt)?;
            rest = &tail[len..];
            config.set(*key, value)?;
        }
        config.validate()?;
        Ok(config)
    }

    fn set(&mut self, key: u8, value: &[u8]) -> Result<(), Error> {
        let text = || {
            core::str::from_utf8(value)
                .map(String::from)
                .map_err(|_| Error::Corrupt)
        };
        let f32_value = || {
            value
                .try_into()
                .map(f32::from_le_bytes)
                .map_err(|_| Error::Corrupt)
        };
        let u32_value = || {
            value
                .try_into()
                .map(u32::from_le_bytes)
                .map_err(|_| Error::Corrupt)
        };
        let byte = || value.first().copied().ok_or(Error::Corrupt);
        match key {
            key::WIFI_SSID => self.wifi_ssid = text()?,
            key::WIFI_PASSWORD => self.wifi_password = text()?,
            key::WIFI_CHANNEL => self.wifi_channel = Some(byte()?),
            key::BOT_TOKEN => self.bot_token = text()?,
            key::CHAT_ID => self.chat_id = text()?,
            key::MESSAGE_THREAD_ID => {
                self.message_thread_id = Some(i64::from_le_bytes(
                    value.try_into().map_err(|_| Error::Corrupt)?,
                ))
            }
            key::DEVICE_NAME => self.device_name = text()?,
            key::LED_BRIGHTNESS => self.led_brightness = byte()?,
            key::WAKE_WORD_ENABLED => self.wake_word_enabled = byte()? != 0,
            key::WAKE_THRESHOLD => self.wake_threshold = Some(f32_value()?),
            key::WAKE_CONSECUTIVE => self.wake_consecutive = byte()?,
            key::MIC_GAIN_DB => self.mic_gain_db = f32_value()?,
            key::MIC_RIGHT_CHANNEL => self.mic_right_channel = byte()? != 0,
            key::SPEECH_MARGIN_DB => self.endpoint.speech_margin_db = f32_value()?,
            key::SILENCE_MS => self.endpoint.silence_ms = u32_value()?,
            key::NO_SPEECH_MS => self.endpoint.no_speech_ms = u32_value()?,
            key::MAX_MS => self.endpoint.max_ms = u32_value()?,
            key::BUTTON_PREROLL_MS => self.button_preroll_ms = u32_value()?,
            key::WAKE_PREROLL_MS => self.wake_preroll_ms = u32_value()?,
            key::OTA_PASSWORD => self.ota_password = text()?,
            _ => {}
        }
        Ok(())
    }
}

/// CRC-32 (IEEE 802.3), bit by bit: the configuration is tiny.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            wifi_ssid: "home".into(),
            wifi_password: "pa\tss\nword".into(),
            wifi_channel: Some(1),
            bot_token: "123:abc".into(),
            chat_id: "-1001234".into(),
            message_thread_id: Some(42),
            device_name: "kitchen".into(),
            wake_threshold: Some(0.85),
            button_preroll_ms: 3_000,
            wake_preroll_ms: 2_000,
            ota_password: "s3cret".into(),
            ..Default::default()
        }
    }

    #[test]
    fn roundtrip() {
        let config = sample();
        let mut bytes = config.encode();
        // The partition is longer than the configuration.
        bytes.extend([0xff; 100]);
        assert_eq!(Config::decode(&bytes).unwrap(), config);
    }

    #[test]
    fn detects_corruption() {
        let mut bytes = sample().encode();
        bytes[20] ^= 1;
        assert_eq!(Config::decode(&bytes), Err(Error::Corrupt));
        assert_eq!(Config::decode(&[0xff; 64]), Err(Error::Missing));
    }

    #[test]
    fn crc_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
