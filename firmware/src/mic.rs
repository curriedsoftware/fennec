//! The Atom Echo's SPM1423 PDM microphone.

use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::gpio::{InputPin, OutputPin};
use esp_idf_svc::hal::i2s::config::{
    Config, DataBitWidth, PdmRxClkConfig, PdmRxConfig, PdmRxGpioConfig, PdmRxSlotConfig,
    PdmSlotMask, SlotMode,
};
use esp_idf_svc::hal::i2s::{I2s, I2sDriver, I2sRx};
use esp_idf_svc::sys::EspError;

use fennec_core::SAMPLE_RATE;

pub struct Mic<'d> {
    driver: I2sDriver<'d, I2sRx>,
    gain: f32,
    // One-pole high-pass filter state, removing the DC offset of the PDM
    // decimator.
    last_in: f32,
    last_out: f32,
    raw: Vec<u8>,
}

impl<'d> Mic<'d> {
    pub fn new(
        i2s: impl I2s + 'd,
        clk: impl OutputPin + 'd,
        din: impl InputPin + 'd,
        right_channel: bool,
        gain_db: f32,
    ) -> Result<Self, EspError> {
        let mask = if right_channel {
            PdmSlotMask::Right
        } else {
            PdmSlotMask::Left
        };
        let config = PdmRxConfig::new(
            // 12 buffers of 256 samples hold 192 ms of audio, enough to ride
            // out a wake word inference.
            Config::default()
                .dma_buffer_count(12)
                .frames_per_buffer(256),
            PdmRxClkConfig::from_sample_rate_hz(SAMPLE_RATE),
            PdmRxSlotConfig::from_bits_per_sample_and_slot_mode(
                DataBitWidth::Bits16,
                SlotMode::Mono,
            )
            .slot_mode_mask(SlotMode::Mono, mask),
            PdmRxGpioConfig::new(false),
        );
        let mut driver = I2sDriver::new_pdm_rx(i2s, &config, clk, din)?;
        driver.rx_enable()?;
        Ok(Self {
            driver,
            gain: 10f32.powf(gain_db / 20.0),
            last_in: 0.0,
            last_out: 0.0,
            raw: Vec::new(),
        })
    }

    /// Fills `samples` with fresh audio, blocking until it is available.
    pub fn read(&mut self, samples: &mut [i16]) -> Result<(), EspError> {
        self.raw.resize(samples.len() * 2, 0);
        let mut filled = 0;
        while filled < self.raw.len() {
            filled += self.driver.read(&mut self.raw[filled..], BLOCK)?;
        }
        for (out, bytes) in samples.iter_mut().zip(self.raw.chunks_exact(2)) {
            let x = i16::from_le_bytes([bytes[0], bytes[1]]) as f32;
            let y = x - self.last_in + 0.995 * self.last_out;
            self.last_in = x;
            self.last_out = y;
            *out = (y * self.gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        Ok(())
    }

    /// Throws away audio that queued up while the device was busy.
    pub fn discard(&mut self, ms: u32) -> Result<(), EspError> {
        let mut buf = [0i16; 160];
        for _ in 0..ms / 10 {
            self.read(&mut buf)?;
        }
        Ok(())
    }
}
