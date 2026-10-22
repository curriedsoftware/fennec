//! Opus encoding (libopus, see `components/opus`), for voice notes. Only
//! built with the `opus` feature.

use anyhow::{Context, bail};

use fennec_core::SAMPLE_RATE;

/// The parts of `opus.h` used here.
mod ffi {
    use core::ffi::c_int;

    #[repr(C)]
    pub struct OpusEncoder {
        _private: [u8; 0],
    }

    pub const OPUS_OK: c_int = 0;
    pub const OPUS_APPLICATION_VOIP: c_int = 2048;
    pub const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
    pub const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
    pub const OPUS_SET_SIGNAL_REQUEST: c_int = 4024;
    pub const OPUS_GET_LOOKAHEAD_REQUEST: c_int = 4027;
    pub const OPUS_SIGNAL_VOICE: c_int = 3001;

    unsafe extern "C" {
        pub fn opus_encoder_create(
            fs: i32,
            channels: c_int,
            application: c_int,
            error: *mut c_int,
        ) -> *mut OpusEncoder;
        pub fn opus_encode(
            st: *mut OpusEncoder,
            pcm: *const i16,
            frame_size: c_int,
            data: *mut u8,
            max_data_bytes: i32,
        ) -> i32;
        pub fn opus_encoder_destroy(st: *mut OpusEncoder);
        pub fn opus_encoder_ctl(st: *mut OpusEncoder, request: c_int, ...) -> c_int;
    }
}

/// Samples per Opus frame: 20 ms.
pub const FRAME: usize = SAMPLE_RATE as usize / 50;
/// Largest packet one frame can produce at the bitrates used here.
pub const MAX_PACKET: usize = 400;

pub struct Encoder {
    raw: *mut ffi::OpusEncoder,
}

// SAFETY: the encoder state is only reached through `&mut self`.
unsafe impl Send for Encoder {}

impl Encoder {
    /// A mono speech encoder at `bitrate` bits per second. `complexity` goes
    /// from 0 (fastest) to 10.
    pub fn new(bitrate: i32, complexity: i32) -> anyhow::Result<Self> {
        let mut error = 0;
        let raw = unsafe {
            ffi::opus_encoder_create(
                SAMPLE_RATE as i32,
                1,
                ffi::OPUS_APPLICATION_VOIP,
                &mut error,
            )
        };
        if raw.is_null() || error != ffi::OPUS_OK {
            bail!("cannot create an Opus encoder (error {error})");
        }
        let encoder = Self { raw };
        encoder.ctl(ffi::OPUS_SET_BITRATE_REQUEST, bitrate)?;
        encoder.ctl(ffi::OPUS_SET_COMPLEXITY_REQUEST, complexity)?;
        encoder.ctl(ffi::OPUS_SET_SIGNAL_REQUEST, ffi::OPUS_SIGNAL_VOICE)?;
        Ok(encoder)
    }

    fn ctl(&self, request: i32, value: i32) -> anyhow::Result<()> {
        let result = unsafe { ffi::opus_encoder_ctl(self.raw, request, value) };
        if result != ffi::OPUS_OK {
            bail!("opus_encoder_ctl({request}, {value}) failed ({result})");
        }
        Ok(())
    }

    /// Samples the decoder should skip at the start (the encoder's delay).
    pub fn pre_skip(&self) -> anyhow::Result<u16> {
        let mut lookahead: i32 = 0;
        let result = unsafe {
            ffi::opus_encoder_ctl(
                self.raw,
                ffi::OPUS_GET_LOOKAHEAD_REQUEST,
                &mut lookahead as *mut i32,
            )
        };
        if result != ffi::OPUS_OK {
            bail!("cannot read the Opus lookahead ({result})");
        }
        // Ogg Opus counts it at 48 kHz.
        u16::try_from(lookahead * (48_000 / SAMPLE_RATE as i32)).context("lookahead too long")
    }

    /// Encodes one 20 ms frame into `packet`, returning the packet's length.
    pub fn encode(&mut self, frame: &[i16; FRAME], packet: &mut [u8]) -> anyhow::Result<usize> {
        let len = unsafe {
            ffi::opus_encode(
                self.raw,
                frame.as_ptr(),
                FRAME as i32,
                packet.as_mut_ptr(),
                packet.len() as i32,
            )
        };
        if len < 0 {
            bail!("opus_encode failed ({len})");
        }
        Ok(len as usize)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_encoder_destroy(self.raw) };
    }
}
