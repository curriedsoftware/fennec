//! The `.fkw` wake word model format and its inference.
//!
//! A model is a small 1-D convolutional network over a window of log-mel
//! frames:
//!
//! ```text
//! input [N_MELS x frames]
//!   -> subtract the window mean, multiply by `input_scale`
//!   -> conv1d (+ ReLU), repeated
//!   -> mean and max over time, concatenated
//!   -> dense -> logit
//! ```
//!
//! Subtracting the mean of the window makes the model independent of the
//! microphone gain, since a gain change shifts every log-mel value equally.
//!
//! # File layout
//!
//! All values are little endian and 4 bytes wide, so a 4-byte aligned file
//! can be used in place (the firmware keeps the weights in flash).
//!
//! ```text
//! magic "FNKW", version (u32), n_mels (u32), frames (u32),
//! sample_rate (u32), hop (u32), window (u32), n_fft (u32),
//! threshold (f32), input_scale (f32), name ([u8; 32], NUL padded),
//! n_conv (u32), n_conv x [in, out, kernel, stride] (u32 each),
//! dense_in (u32),
//! weights (f32): for each conv: weight [out][in][kernel], bias [out];
//!                dense: weight [dense_in], bias [1]
//! ```

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::SAMPLE_RATE;
use crate::frontend::{HOP, N_FFT, N_MELS, WINDOW};

pub const MAGIC: [u8; 4] = *b"FNKW";
pub const VERSION: u32 = 1;
pub const NAME_LEN: usize = 32;
pub const MAX_CONV_LAYERS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvShape {
    pub in_channels: usize,
    pub out_channels: usize,
    pub kernel: usize,
    pub stride: usize,
}

impl ConvShape {
    pub fn output_len(&self, input_len: usize) -> usize {
        (input_len - self.kernel) / self.stride + 1
    }

    fn weight_count(&self) -> usize {
        self.out_channels * self.in_channels * self.kernel
    }
}

/// Everything about a model except its weights.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub frames: usize,
    pub threshold: f32,
    pub input_scale: f32,
    pub name: [u8; NAME_LEN],
    pub convs: Vec<ConvShape>,
}

impl Header {
    pub fn name(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }

    pub fn set_name(&mut self, name: &str) {
        self.name = [0; NAME_LEN];
        let mut len = name.len().min(NAME_LEN);
        while !name.is_char_boundary(len) {
            len -= 1;
        }
        self.name[..len].copy_from_slice(&name.as_bytes()[..len]);
    }

    /// Time steps left after the last convolution.
    pub fn final_len(&self) -> usize {
        self.convs
            .iter()
            .fold(self.frames, |len, conv| conv.output_len(len))
    }

    pub fn dense_in(&self) -> usize {
        2 * self.convs.last().map_or(N_MELS, |c| c.out_channels)
    }

    /// Number of `f32` weights the network has.
    pub fn weight_count(&self) -> usize {
        self.convs
            .iter()
            .map(|c| c.weight_count() + c.out_channels)
            .sum::<usize>()
            + self.dense_in()
            + 1
    }

    fn validate(&self) -> Result<(), Error> {
        if self.convs.is_empty() || self.convs.len() > MAX_CONV_LAYERS {
            return Err(Error::Shape);
        }
        let mut channels = N_MELS;
        let mut len = self.frames;
        for conv in &self.convs {
            if conv.in_channels != channels
                || conv.kernel == 0
                || conv.stride == 0
                || conv.kernel > len
            {
                return Err(Error::Shape);
            }
            channels = conv.out_channels;
            len = conv.output_len(len);
        }
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        4 * (10 + NAME_LEN / 4 + 1 + 4 * self.convs.len() + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a `.fkw` file.
    Magic,
    /// Written by an incompatible version of Fennec.
    Version(u32),
    /// Uses a different audio front end than this build.
    Frontend,
    /// The layer shapes do not fit together.
    Shape,
    /// Shorter than its header says.
    Truncated,
    /// The weights are not 4-byte aligned in memory.
    Unaligned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Magic => write!(f, "not a Fennec wake word model"),
            Error::Version(v) => write!(f, "unsupported model version {v} (expected {VERSION})"),
            Error::Frontend => write!(f, "model was trained with a different audio front end"),
            Error::Shape => write!(f, "invalid layer shapes"),
            Error::Truncated => write!(f, "model file is truncated"),
            Error::Unaligned => write!(f, "model data is not 4-byte aligned"),
        }
    }
}

impl core::error::Error for Error {}

/// A model whose weights borrow from the encoded bytes.
#[derive(Clone)]
pub struct Model<'a> {
    pub header: Header,
    weights: &'a [f32],
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or(Error::Truncated)?;
        self.pos += 4;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn usize(&mut self) -> Result<usize, Error> {
        self.u32().map(|v| v as usize)
    }

    fn f32(&mut self) -> Result<f32, Error> {
        self.u32().map(f32::from_bits)
    }
}

impl<'a> Model<'a> {
    /// Parses a model. `bytes` must be 4-byte aligned; see
    /// [`aligned_copy`] for loading one from a file.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.get(..4) != Some(&MAGIC) {
            return Err(Error::Magic);
        }
        let mut r = Reader { bytes, pos: 4 };
        let version = r.u32()?;
        if version != VERSION {
            return Err(Error::Version(version));
        }
        let n_mels = r.usize()?;
        let frames = r.usize()?;
        let frontend = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
        if n_mels != N_MELS || frontend != [SAMPLE_RATE, HOP as u32, WINDOW as u32, N_FFT as u32] {
            return Err(Error::Frontend);
        }
        let threshold = r.f32()?;
        let input_scale = r.f32()?;
        let mut name = [0; NAME_LEN];
        name.copy_from_slice(bytes.get(r.pos..r.pos + NAME_LEN).ok_or(Error::Truncated)?);
        r.pos += NAME_LEN;
        let n_conv = r.usize()?;
        if n_conv == 0 || n_conv > MAX_CONV_LAYERS {
            return Err(Error::Shape);
        }
        let mut convs = Vec::with_capacity(n_conv);
        for _ in 0..n_conv {
            convs.push(ConvShape {
                in_channels: r.usize()?,
                out_channels: r.usize()?,
                kernel: r.usize()?,
                stride: r.usize()?,
            });
        }
        let header = Header {
            frames,
            threshold,
            input_scale,
            name,
            convs,
        };
        header.validate()?;
        if r.usize()? != header.dense_in() {
            return Err(Error::Shape);
        }
        let body = bytes
            .get(r.pos..r.pos + 4 * header.weight_count())
            .ok_or(Error::Truncated)?;
        let weights = bytemuck::try_cast_slice(body).map_err(|_| Error::Unaligned)?;
        Ok(Self { header, weights })
    }

    /// `f32` values needed for each of the two activation buffers of
    /// [`Model::infer`]: the largest convolution output.
    pub fn scratch_len(&self) -> usize {
        let mut len = self.header.frames;
        let mut largest = 0;
        for conv in &self.header.convs {
            len = conv.output_len(len);
            largest = largest.max(conv.out_channels * len);
        }
        largest
    }

    /// Runs the network on `features`, laid out as `[N_MELS][frames]`, and
    /// returns the logit. `scratch` is resized as needed and can be reused
    /// between calls to avoid allocations.
    pub fn infer(&self, features: &[f32], scratch: &mut Scratch) -> f32 {
        let header = &self.header;
        assert_eq!(features.len(), N_MELS * header.frames);
        let largest = self.scratch_len();
        scratch.a.resize(largest, 0.0);
        scratch.b.resize(largest, 0.0);

        // The first layer sees `(x - mean) * input_scale`. That is folded
        // into its weights, so `features` is used in place without a copy.
        let mean = features.iter().sum::<f32>() / features.len() as f32;
        let normalize = Normalize {
            mean,
            scale: header.input_scale,
        };

        let mut weights = self.weights;
        let mut len = header.frames;
        let (mut input, mut output) = (&mut scratch.a, &mut scratch.b);
        for (i, conv) in header.convs.iter().enumerate() {
            let (w, rest) = weights.split_at(conv.weight_count());
            let (b, rest) = rest.split_at(conv.out_channels);
            weights = rest;
            let out_len = conv.output_len(len);
            if i == 0 {
                conv1d_relu(conv, w, b, features, len, output, Some(normalize));
            } else {
                conv1d_relu(
                    conv,
                    w,
                    b,
                    &input[..conv.in_channels * len],
                    len,
                    output,
                    None,
                );
            }
            core::mem::swap(&mut input, &mut output);
            len = out_len;
        }

        let channels = header.convs.last().unwrap().out_channels;
        let (dense_w, dense_b) = weights.split_at(header.dense_in());
        let mut logit = dense_b[0];
        for c in 0..channels {
            let row = &input[c * len..(c + 1) * len];
            let mean = row.iter().sum::<f32>() / len as f32;
            let max = row.iter().copied().fold(f32::MIN, f32::max);
            logit += dense_w[c] * mean + dense_w[channels + c] * max;
        }
        logit
    }

    /// Probability that the window ends with the wake word.
    pub fn probability(&self, features: &[f32], scratch: &mut Scratch) -> f32 {
        sigmoid(self.infer(features, scratch))
    }
}

/// Reusable activation buffers for [`Model::infer`].
#[derive(Default)]
pub struct Scratch {
    a: Vec<f32>,
    b: Vec<f32>,
}

pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + libm::expf(-x))
}

/// `a * b + c`. On the ESP32 this is a single `madd.s`, which Rust only emits
/// when asked: without it every product waits for its own `mul.s`.
#[inline(always)]
fn mul_add(a: f32, b: f32, c: f32) -> f32 {
    #[cfg(target_arch = "xtensa")]
    {
        core::intrinsics::fmuladdf32(a, b, c)
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        a * b + c
    }
}

#[derive(Clone, Copy)]
struct Normalize {
    mean: f32,
    scale: f32,
}

fn conv1d_relu(
    conv: &ConvShape,
    weight: &[f32],
    bias: &[f32],
    input: &[f32],
    len: usize,
    output: &mut [f32],
    normalize: Option<Normalize>,
) {
    let out_len = conv.output_len(len);
    let (k, stride) = (conv.kernel, conv.stride);
    for o in 0..conv.out_channels {
        let out_row = &mut output[o * out_len..(o + 1) * out_len];
        let filter = &weight[o * conv.in_channels * k..(o + 1) * conv.in_channels * k];
        // Four outputs at a time, summed over every input channel in
        // registers: each weight is loaded once per block, and the four sums
        // do not wait on each other. This is what makes inference fit in its
        // 100 ms on the ESP32.
        let mut blocks = out_row.chunks_exact_mut(4);
        for (b, block) in (&mut blocks).enumerate() {
            let start = b * 4 * stride;
            let mut acc = [0.0f32; 4];
            for (w, in_row) in filter.chunks_exact(k).zip(input.chunks_exact(len)) {
                let x = &in_row[start..start + 3 * stride + k];
                for (j, &wj) in w.iter().enumerate() {
                    acc[0] = mul_add(wj, x[j], acc[0]);
                    acc[1] = mul_add(wj, x[stride + j], acc[1]);
                    acc[2] = mul_add(wj, x[2 * stride + j], acc[2]);
                    acc[3] = mul_add(wj, x[3 * stride + j], acc[3]);
                }
            }
            block.copy_from_slice(&acc);
        }
        let done = out_len / 4 * 4;
        for (n, acc) in blocks.into_remainder().iter_mut().enumerate() {
            let start = (done + n) * stride;
            *acc = 0.0;
            for (w, in_row) in filter.chunks_exact(k).zip(input.chunks_exact(len)) {
                let x = &in_row[start..start + k];
                *acc += w.iter().zip(x).map(|(w, x)| w * x).sum::<f32>();
            }
        }
        // conv(s * (x - m)) = s * conv(x) - s * m * sum(w)
        let (scale, offset) = match normalize {
            Some(n) => (n.scale, n.scale * n.mean * filter.iter().sum::<f32>()),
            None => (1.0, 0.0),
        };
        for v in out_row.iter_mut() {
            *v = (scale * *v - offset + bias[o]).max(0.0);
        }
    }
}

/// Encodes a model. `weights` must hold [`Header::weight_count`] values in
/// the order described in the module documentation.
pub fn encode(header: &Header, weights: &[f32]) -> Result<Vec<u8>, Error> {
    header.validate()?;
    if weights.len() != header.weight_count() {
        return Err(Error::Shape);
    }
    let mut out = Vec::with_capacity(header.encoded_len() + 4 * weights.len());
    let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    for v in [
        u32::from_le_bytes(MAGIC),
        VERSION,
        N_MELS as u32,
        header.frames as u32,
        SAMPLE_RATE,
        HOP as u32,
        WINDOW as u32,
        N_FFT as u32,
        header.threshold.to_bits(),
        header.input_scale.to_bits(),
    ] {
        put(v);
    }
    out.extend_from_slice(&header.name);
    let mut put = |v: usize| out.extend_from_slice(&(v as u32).to_le_bytes());
    put(header.convs.len());
    for conv in &header.convs {
        put(conv.in_channels);
        put(conv.out_channels);
        put(conv.kernel);
        put(conv.stride);
    }
    put(header.dense_in());
    for w in weights {
        out.extend_from_slice(&w.to_le_bytes());
    }
    Ok(out)
}

/// Copies `bytes` into 4-byte aligned storage so it can be passed to
/// [`Model::from_bytes`].
pub fn aligned_copy(bytes: &[u8]) -> Vec<u32> {
    let mut words = vec![0u32; bytes.len().div_ceil(4)];
    bytemuck::cast_slice_mut::<u32, u8>(&mut words)[..bytes.len()].copy_from_slice(bytes);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        let mut header = Header {
            frames: 20,
            threshold: 0.8,
            input_scale: 0.5,
            name: [0; NAME_LEN],
            convs: vec![
                ConvShape {
                    in_channels: N_MELS,
                    out_channels: 4,
                    kernel: 3,
                    stride: 2,
                },
                ConvShape {
                    in_channels: 4,
                    out_channels: 3,
                    kernel: 3,
                    stride: 1,
                },
            ],
        };
        header.set_name("hey fennec");
        header
    }

    fn weights(header: &Header) -> Vec<f32> {
        (0..header.weight_count())
            .map(|i| ((i * 7919) % 13) as f32 / 13.0 - 0.4)
            .collect()
    }

    #[test]
    fn roundtrip() {
        let header = header();
        let bytes = encode(&header, &weights(&header)).unwrap();
        let words = aligned_copy(&bytes);
        let model = Model::from_bytes(&bytemuck::cast_slice(&words)[..bytes.len()]).unwrap();
        assert_eq!(model.header, header);
        assert_eq!(model.header.name(), "hey fennec");
    }

    /// Straightforward reference implementation of the network.
    fn reference(header: &Header, weights: &[f32], features: &[f32]) -> f32 {
        let mean = features.iter().sum::<f32>() / features.len() as f32;
        let mut x: Vec<Vec<f32>> = features
            .chunks(header.frames)
            .map(|row| {
                row.iter()
                    .map(|v| (v - mean) * header.input_scale)
                    .collect()
            })
            .collect();
        let mut w = weights;
        for conv in &header.convs {
            let len = x[0].len();
            let out_len = conv.output_len(len);
            let mut y = vec![vec![0.0; out_len]; conv.out_channels];
            for o in 0..conv.out_channels {
                for t in 0..out_len {
                    let mut acc = w[conv.weight_count() + o];
                    for i in 0..conv.in_channels {
                        for j in 0..conv.kernel {
                            acc += w[(o * conv.in_channels + i) * conv.kernel + j]
                                * x[i][t * conv.stride + j];
                        }
                    }
                    y[o][t] = acc.max(0.0);
                }
            }
            w = &w[conv.weight_count() + conv.out_channels..];
            x = y;
        }
        let c = x.len();
        let mut logit = w[2 * c];
        for (i, row) in x.iter().enumerate() {
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            let max = row.iter().copied().fold(f32::MIN, f32::max);
            logit += w[i] * mean + w[c + i] * max;
        }
        logit
    }

    #[test]
    fn inference_matches_reference() {
        let header = header();
        let weights = weights(&header);
        let bytes = encode(&header, &weights).unwrap();
        let words = aligned_copy(&bytes);
        let model = Model::from_bytes(&bytemuck::cast_slice(&words)[..bytes.len()]).unwrap();
        let features: Vec<f32> = (0..N_MELS * header.frames)
            .map(|i| ((i * 31) % 17) as f32 - 8.0)
            .collect();
        let got = model.infer(&features, &mut Scratch::default());
        let want = reference(&header, &weights, &features);
        assert!((got - want).abs() < 1e-3, "{got} != {want}");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Model::from_bytes(b"nope").err(), Some(Error::Magic));
        let header = header();
        let bytes = encode(&header, &weights(&header)).unwrap();
        let words = aligned_copy(&bytes);
        let bytes = &bytemuck::cast_slice(&words)[..bytes.len() - 4];
        assert_eq!(Model::from_bytes(bytes).err(), Some(Error::Truncated));
    }
}
