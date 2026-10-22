//! Code shared between the Fennec firmware and the `fennec` CLI.
//!
//! Everything that decides whether the device hears its wake word lives here,
//! so the training pipeline extracts features and evaluates models with
//! exactly the same code that runs on the microcontroller.
//!
//! - [`frontend`]: streaming log-mel feature extraction.
//! - [`model`]: the `.fkw` model format and its inference.
//! - [`detector`]: a streaming wake word detector built on both.
//! - [`endpoint`]: decides when the user stopped talking.
//! - [`adpcm`]: IMA ADPCM encoding, used to keep recordings small in RAM.
//! - [`config`]: the device configuration stored in flash.
//! - [`ogg`]: Ogg Opus pages, for voice notes.

#![no_std]
// The ESP toolchain is nightly-capable; `model` uses a fused multiply-add
// intrinsic there that stable Rust does not expose.
#![cfg_attr(target_arch = "xtensa", feature(core_intrinsics))]
#![cfg_attr(target_arch = "xtensa", allow(internal_features))]

extern crate alloc;

pub mod adpcm;
pub mod config;
pub mod detector;
pub mod endpoint;
pub mod frontend;
pub mod model;
pub mod ogg;

/// Sample rate of all audio handled by Fennec.
pub const SAMPLE_RATE: u32 = 16_000;
