//! The SK6812 RGB LED under the Atom Echo's button.

use esp_idf_svc::hal::gpio::OutputPin;
use esp_idf_svc::hal::rmt::config::{TransmitConfig, TxChannelConfig};
use esp_idf_svc::hal::rmt::encoder::{BytesEncoder, BytesEncoderConfig};
use esp_idf_svc::hal::rmt::{PinState, Symbol, TxChannelDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;

use core::time::Duration;

const RESOLUTION: Hertz = Hertz(10_000_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Off,
    /// Connecting to Wi-Fi.
    Connecting,
    /// Waiting for the wake word or a button press.
    Idle,
    /// Recording a command.
    Listening,
    /// Sending the recording to Telegram.
    Uploading,
    /// Receiving an over-the-air update.
    Updating,
    Success,
    Error,
}

impl Status {
    fn rgb(self) -> (u8, u8, u8) {
        match self {
            Status::Off => (0, 0, 0),
            Status::Connecting => (255, 120, 0),
            // A faint glow, so it is visible that the device is alive.
            Status::Idle => (0, 0, 40),
            Status::Listening => (0, 200, 255),
            Status::Uploading => (160, 0, 255),
            Status::Updating => (255, 255, 255),
            Status::Success => (0, 255, 40),
            Status::Error => (255, 0, 0),
        }
    }
}

pub struct Led<'d> {
    channel: TxChannelDriver<'d>,
    encoder: BytesEncoder,
    brightness: u8,
}

impl<'d> Led<'d> {
    pub fn new(pin: impl OutputPin + 'd, brightness: u8) -> Result<Self, EspError> {
        let channel = TxChannelDriver::new(
            pin,
            &TxChannelConfig {
                resolution: RESOLUTION,
                ..Default::default()
            },
        )?;
        let symbol = |high_ns, low_ns| {
            Symbol::new_with(
                RESOLUTION,
                PinState::High,
                Duration::from_nanos(high_ns),
                PinState::Low,
                Duration::from_nanos(low_ns),
            )
            .unwrap()
        };
        let encoder = BytesEncoder::with_config(&BytesEncoderConfig {
            bit0: symbol(300, 900),
            bit1: symbol(600, 600),
            msb_first: true,
            ..Default::default()
        })?;
        Ok(Self {
            channel,
            encoder,
            brightness,
        })
    }

    pub fn set_brightness(&mut self, brightness: u8) {
        self.brightness = brightness;
    }

    pub fn set(&mut self, status: Status) {
        let (r, g, b) = status.rgb();
        let scale = |v: u8| (v as u16 * self.brightness as u16 / 255) as u8;
        // SK6812 expects green, red, blue.
        let grb = [scale(g), scale(r), scale(b)];
        if let Err(e) =
            self.channel
                .send_and_wait(&mut self.encoder, &grb, &TransmitConfig::default())
        {
            log::warn!("cannot update LED: {e}");
        }
    }

    /// Shows `status` for `ms` milliseconds, then `then`.
    pub fn flash(&mut self, status: Status, ms: u32, then: Status) {
        self.set(status);
        std::thread::sleep(Duration::from_millis(ms as u64));
        self.set(then);
    }
}
