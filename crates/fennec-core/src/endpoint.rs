//! Decides when a spoken command is over.
//!
//! The endpointer compares the energy of each 10 ms frame with a running
//! estimate of the background noise. A recording ends after a stretch of
//! silence following speech, when nobody speaks at all, or when it gets too
//! long.

/// Samples per endpointer frame (10 ms).
pub const FRAME: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndpointConfig {
    /// How far above the noise floor a frame must be to count as speech.
    pub speech_margin_db: f32,
    /// Silence after speech that ends the recording.
    pub silence_ms: u32,
    /// Give up if no speech starts within this time.
    pub no_speech_ms: u32,
    /// Hard limit on the recording length.
    pub max_ms: u32,
    /// Speech shorter than this is treated as noise.
    pub min_speech_ms: u32,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            speech_margin_db: 9.0,
            silence_ms: 1200,
            no_speech_ms: 4000,
            max_ms: 12_000,
            min_speech_ms: 200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// Keep recording.
    Continue,
    /// The user spoke and then stopped.
    Silence,
    /// The recording hit its length limit.
    MaxLength,
    /// Nothing was said.
    NoSpeech,
}

pub struct Endpointer {
    pub config: EndpointConfig,
    noise_floor_db: f32,
    calibrated: bool,
    elapsed_ms: u32,
    speech_ms: u32,
    silence_ms: u32,
}

/// Energy of `frame` in dB relative to full scale.
pub fn level_db(frame: &[i16]) -> f32 {
    let energy: f32 = frame
        .iter()
        .map(|&s| {
            let x = s as f32 / 32768.0;
            x * x
        })
        .sum::<f32>()
        / frame.len().max(1) as f32;
    10.0 * libm::log10f(energy + 1e-10)
}

impl Endpointer {
    pub fn new(config: EndpointConfig) -> Self {
        Self {
            config,
            noise_floor_db: -60.0,
            calibrated: false,
            elapsed_ms: 0,
            speech_ms: 0,
            silence_ms: 0,
        }
    }

    pub fn noise_floor_db(&self) -> f32 {
        self.noise_floor_db
    }

    /// Updates the noise estimate with audio heard while idle. The floor
    /// follows quieter frames quickly and louder ones slowly, so speech
    /// barely moves it.
    pub fn observe_background(&mut self, frame: &[i16]) {
        let db = level_db(frame).max(-90.0);
        if !self.calibrated {
            self.noise_floor_db = db;
            self.calibrated = true;
        }
        let rate = if db < self.noise_floor_db {
            0.05
        } else {
            0.002
        };
        self.noise_floor_db += rate * (db - self.noise_floor_db);
    }

    /// Starts a new recording.
    pub fn start(&mut self) {
        self.elapsed_ms = 0;
        self.speech_ms = 0;
        self.silence_ms = 0;
    }

    /// Whether enough speech was heard to be worth keeping.
    pub fn heard_speech(&self) -> bool {
        self.speech_ms >= self.config.min_speech_ms
    }

    /// Feeds one recorded frame of [`FRAME`] samples.
    pub fn feed(&mut self, frame: &[i16]) -> Endpoint {
        let ms = (frame.len() * 1000 / crate::SAMPLE_RATE as usize) as u32;
        self.elapsed_ms += ms;
        if level_db(frame) > self.noise_floor_db + self.config.speech_margin_db {
            self.speech_ms += ms;
            self.silence_ms = 0;
        } else {
            self.silence_ms += ms;
        }

        if self.elapsed_ms >= self.config.max_ms {
            return if self.heard_speech() {
                Endpoint::MaxLength
            } else {
                Endpoint::NoSpeech
            };
        }
        if self.heard_speech() {
            if self.silence_ms >= self.config.silence_ms {
                return Endpoint::Silence;
            }
        } else if self.elapsed_ms >= self.config.no_speech_ms {
            return Endpoint::NoSpeech;
        }
        Endpoint::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(amplitude: f32) -> [i16; FRAME] {
        core::array::from_fn(|n| {
            (amplitude * 32767.0 * libm::sinf(n as f32 * 2.0 * core::f32::consts::PI / 32.0)) as i16
        })
    }

    fn run(
        endpointer: &mut Endpointer,
        frames: impl IntoIterator<Item = [i16; FRAME]>,
    ) -> (Endpoint, u32) {
        endpointer.start();
        for f in frames {
            let e = endpointer.feed(&f);
            if e != Endpoint::Continue {
                return (e, endpointer.elapsed_ms);
            }
        }
        (Endpoint::Continue, endpointer.elapsed_ms)
    }

    fn calibrated() -> Endpointer {
        let mut e = Endpointer::new(EndpointConfig::default());
        for _ in 0..500 {
            e.observe_background(&frame(0.002));
        }
        e
    }

    #[test]
    fn learns_noise_floor() {
        let e = calibrated();
        // A sine at amplitude a has power a^2 / 2.
        let expected = 10.0 * libm::log10f(0.002 * 0.002 / 2.0);
        assert!(
            (e.noise_floor_db() - expected).abs() < 1.0,
            "{}",
            e.noise_floor_db()
        );
    }

    #[test]
    fn ends_after_silence() {
        let mut e = calibrated();
        let speech = core::iter::repeat_n(frame(0.2), 150);
        let quiet = core::iter::repeat_n(frame(0.002), 500);
        let (end, at) = run(&mut e, speech.chain(quiet));
        assert_eq!(end, Endpoint::Silence);
        assert_eq!(at, 1500 + 1200);
    }

    #[test]
    fn gives_up_without_speech() {
        let mut e = calibrated();
        let (end, at) = run(&mut e, core::iter::repeat_n(frame(0.002), 1000));
        assert_eq!(end, Endpoint::NoSpeech);
        assert_eq!(at, 4000);
    }

    #[test]
    fn caps_length() {
        let mut e = calibrated();
        let (end, at) = run(&mut e, core::iter::repeat_n(frame(0.2), 2000));
        assert_eq!(end, Endpoint::MaxLength);
        assert_eq!(at, 12_000);
    }
}
