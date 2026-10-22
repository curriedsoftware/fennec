//! The Atom Echo's NS4168 I2S amplifier and speaker.

use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::gpio::{AnyIOPin, InputPin, OutputPin};
use esp_idf_svc::hal::i2s::config::{
    Config, DataBitWidth, SlotMode, StdClkConfig, StdConfig, StdGpioConfig, StdSlotConfig,
};
use esp_idf_svc::hal::i2s::{I2s, I2sDriver, I2sTx};
use esp_idf_svc::sys::EspError;

use fennec_core::SAMPLE_RATE;

const DMA_BUFFERS: u32 = 6;
const FRAMES_PER_BUFFER: u32 = 256;

pub struct Speaker<'d> {
    driver: I2sDriver<'d, I2sTx>,
    volume: f32,
    raw: Vec<u8>,
}

impl<'d> Speaker<'d> {
    /// `volume` goes from 0.0 (silent) to 1.0 (full scale, very loud).
    pub fn new(
        i2s: impl I2s + 'd,
        bclk: impl InputPin + OutputPin + 'd,
        ws: impl InputPin + OutputPin + 'd,
        dout: impl OutputPin + 'd,
        volume: f32,
    ) -> Result<Self, EspError> {
        let config = StdConfig::new(
            // Send silence when nothing is queued, instead of repeating the
            // last buffer.
            Config::default()
                .dma_buffer_count(DMA_BUFFERS)
                .frames_per_buffer(FRAMES_PER_BUFFER)
                .auto_clear(true),
            StdClkConfig::from_sample_rate_hz(SAMPLE_RATE),
            StdSlotConfig::philips_slot_default(DataBitWidth::Bits16, SlotMode::Stereo),
            StdGpioConfig::default(),
        );
        let mut driver = I2sDriver::new_std_tx(i2s, &config, bclk, dout, AnyIOPin::none(), ws)?;
        driver.tx_enable()?;
        Ok(Self {
            driver,
            volume: volume.clamp(0.0, 1.0),
            raw: Vec::new(),
        })
    }

    /// Queues mono `samples`, blocking until they fit in the DMA buffers.
    pub fn write(&mut self, samples: &[i16]) -> Result<(), EspError> {
        // The amplifier plays one slot; fill both so it does not matter which.
        self.raw.clear();
        for &sample in samples {
            let bytes = ((sample as f32 * self.volume) as i16).to_le_bytes();
            self.raw.extend_from_slice(&bytes);
            self.raw.extend_from_slice(&bytes);
        }
        self.driver.write_all(&self.raw, BLOCK)
    }

    /// Waits until everything queued has been played. Dropping the speaker
    /// earlier cuts off the end.
    pub fn flush(&mut self) -> Result<(), EspError> {
        let silence = [0i16; FRAMES_PER_BUFFER as usize];
        for _ in 0..DMA_BUFFERS {
            self.write(&silence)?;
        }
        Ok(())
    }

    /// Plays a short rising two-note chime: a test of the speaker until
    /// replies are played.
    pub fn chime(&mut self) -> Result<(), EspError> {
        for (frequency, ms) in [(880.0, 120), (1320.0, 180)] {
            self.tone(frequency, ms)?;
        }
        self.flush()
    }

    fn tone(&mut self, frequency: f32, ms: u32) -> Result<(), EspError> {
        let total = (SAMPLE_RATE * ms / 1000) as usize;
        // Fade in and out over 5 ms, so the note does not click.
        let fade = (SAMPLE_RATE / 200) as usize;
        let step = core::f32::consts::TAU * frequency / SAMPLE_RATE as f32;
        let mut chunk = [0i16; FRAMES_PER_BUFFER as usize];
        let mut n = 0;
        while n < total {
            let len = chunk.len().min(total - n);
            for (i, out) in chunk[..len].iter_mut().enumerate() {
                let t = n + i;
                let envelope = (t.min(total - 1 - t) as f32 / fade as f32).min(1.0);
                *out = ((t as f32 * step).sin() * envelope * i16::MAX as f32) as i16;
            }
            self.write(&chunk[..len])?;
            n += len;
        }
        Ok(())
    }
}
