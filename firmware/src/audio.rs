//! The I2S peripheral and pins shared by the microphone and the speaker.
//!
//! The microphone clock and the speaker's word select are both wired to
//! GPIO33, so only one of them can be driven at a time. Each is borrowed from
//! here and must be dropped before the other is opened.

use esp_idf_svc::hal::gpio::{Gpio19, Gpio22, Gpio23, Gpio33};
use esp_idf_svc::hal::i2s::I2S0;
use esp_idf_svc::sys::EspError;

use fennec_core::config::Config;

use crate::mic::Mic;
use crate::speaker::Speaker;

pub struct Audio {
    pub i2s: I2S0<'static>,
    /// Microphone clock, and speaker word select.
    pub gpio33: Gpio33<'static>,
    /// Microphone data.
    pub gpio23: Gpio23<'static>,
    /// Speaker bit clock.
    pub gpio19: Gpio19<'static>,
    /// Speaker data.
    pub gpio22: Gpio22<'static>,
}

impl Audio {
    pub fn mic(&mut self, config: &Config) -> Result<Mic<'_>, EspError> {
        // SAFETY: the drivers borrowing these are dropped, never forgotten.
        unsafe {
            Mic::new(
                self.i2s.reborrow(),
                self.gpio33.reborrow(),
                self.gpio23.reborrow(),
                config.mic_right_channel,
                config.mic_gain_db,
            )
        }
    }

    pub fn speaker(&mut self, volume: f32) -> Result<Speaker<'_>, EspError> {
        // SAFETY: the drivers borrowing these are dropped, never forgotten.
        unsafe {
            Speaker::new(
                self.i2s.reborrow(),
                self.gpio19.reborrow(),
                self.gpio33.reborrow(),
                self.gpio22.reborrow(),
                volume,
            )
        }
    }
}
