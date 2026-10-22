//! Trains a wake word model with candle and exports it as `.fkw`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::{Conv1d, Conv1dConfig, Linear, Optimizer, ParamsAdamW, VarBuilder, VarMap};
use fennec_core::frontend::N_MELS;
use fennec_core::model::{ConvShape, Header, Model, NAME_LEN, Scratch, aligned_copy, encode};

use crate::dataset::{Augmenter, Dataset, batch};
use crate::eval;

/// The network. At `width` 1.0 it has about 50k weights and needs 1.5M
/// multiply-adds per evaluation; cost grows with the square of `width`.
pub fn convs(width: f32) -> Vec<ConvShape> {
    let ch = |c: usize| ((c as f32 * width).round() as usize).max(4);
    let (a, b) = (ch(48), ch(64));
    let layer = |in_channels, out_channels, kernel, stride| ConvShape {
        in_channels,
        out_channels,
        kernel,
        stride,
    };
    vec![
        layer(N_MELS, a, 5, 2),
        layer(a, a, 5, 2),
        layer(a, b, 5, 2),
        layer(b, b, 3, 1),
    ]
}

pub struct TrainOptions {
    pub data: PathBuf,
    pub out: PathBuf,
    pub backgrounds: Vec<PathBuf>,
    /// Extra directories of non-speech sounds (coughs, doors...).
    pub sounds: Vec<PathBuf>,
    pub steps: usize,
    pub batch_size: usize,
    pub learning_rate: f64,
    /// Window length in 10 ms frames.
    pub frames: usize,
    /// Scales the number of channels of every layer.
    pub width: f32,
    pub eval_minutes: f32,
    pub max_false_accepts_per_hour: f32,
    pub seed: u64,
}

struct Net {
    convs: Vec<Conv1d>,
    shapes: Vec<ConvShape>,
    dense: Linear,
    input_scale: f64,
}

impl Net {
    fn new(vb: VarBuilder, convs: &[ConvShape], input_scale: f64) -> candle_core::Result<Self> {
        let layers = convs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                candle_nn::conv1d(
                    c.in_channels,
                    c.out_channels,
                    c.kernel,
                    Conv1dConfig {
                        stride: c.stride,
                        ..Default::default()
                    },
                    vb.pp(format!("conv{i}")),
                )
            })
            .collect::<candle_core::Result<_>>()?;
        let channels = convs.last().unwrap().out_channels;
        let dense = candle_nn::linear(2 * channels, 1, vb.pp("dense"))?;
        Ok(Self {
            convs: layers,
            shapes: convs.to_vec(),
            dense,
            input_scale,
        })
    }

    /// `x` is `[batch, N_MELS, frames]`; returns logits `[batch]`.
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let mean = x.mean_keepdim(D::Minus1)?.mean_keepdim(D::Minus2)?;
        let mut x = (x.broadcast_sub(&mean)? * self.input_scale)?;
        for (conv, shape) in self.convs.iter().zip(&self.shapes) {
            let bias = conv.bias().unwrap().reshape((1, shape.out_channels, 1))?;
            x = conv1d(&x, conv.weight(), shape.stride)?
                .broadcast_add(&bias)?
                .relu()?;
        }
        let pooled = Tensor::cat(&[x.mean(D::Minus1)?, x.max(D::Minus1)?], 1)?;
        self.dense.forward(&pooled)?.squeeze(1)
    }

    /// Weights in `.fkw` order.
    fn export(&self) -> candle_core::Result<Vec<f32>> {
        let mut weights = Vec::new();
        for conv in &self.convs {
            weights.extend(conv.weight().flatten_all()?.to_vec1::<f32>()?);
            weights.extend(conv.bias().unwrap().to_vec1::<f32>()?);
        }
        weights.extend(self.dense.weight().flatten_all()?.to_vec1::<f32>()?);
        weights.extend(self.dense.bias().unwrap().to_vec1::<f32>()?);
        Ok(weights)
    }
}

const WARMUP_STEPS: usize = 300;
/// From this step on, the negatives the model confuses most are replayed
/// more often; the list is refreshed every `MINING_EVERY` steps.
const MINING_START: usize = 1000;
const MINING_EVERY: usize = 500;
/// Share of the negative clips kept as hard negatives.
const HARD_FRACTION: f32 = 0.1;

/// Scores every training negative and returns the highest scoring ones.
fn mine_hard_negatives(net: &Net, aug: &Augmenter<'_>, seed: u64) -> anyhow::Result<Vec<usize>> {
    use rand::SeedableRng;
    use rayon::prelude::*;
    let count = aug.split.negative_clips();
    let mut scores = Vec::with_capacity(count);
    for start in (0..count).step_by(512) {
        let end = (start + 512).min(count);
        let feats: Vec<f32> = (start..end)
            .into_par_iter()
            .flat_map_iter(|i| {
                let mut rng = rand::rngs::Xoshiro256PlusPlus::seed_from_u64(seed ^ i as u64);
                crate::dataset::features(&aug.negative_window(i, &mut rng), aug.frames)
            })
            .collect();
        let x = Tensor::from_vec(feats, (end - start, N_MELS, aug.frames), &Device::Cpu)?;
        scores.extend(net.forward(&x)?.to_vec1::<f32>()?);
    }
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    order.truncate(((count as f32 * HARD_FRACTION) as usize).max(1));
    Ok(order)
}
const MAX_GRAD_NORM: f64 = 1.0;

/// Scales gradients down so their global L2 norm is at most `max_norm`.
fn clip_gradients(
    grads: &mut candle_core::backprop::GradStore,
    vars: &[candle_core::Var],
    max_norm: f64,
) -> candle_core::Result<()> {
    let mut total = 0.0f64;
    for var in vars {
        if let Some(g) = grads.get(var.as_tensor()) {
            total += g.sqr()?.sum_all()?.to_scalar::<f32>()? as f64;
        }
    }
    let norm = total.sqrt();
    if norm > max_norm {
        let scale = max_norm / norm;
        for var in vars {
            if let Some(g) = grads.remove(var.as_tensor()) {
                grads.insert(var.as_tensor(), (g * scale)?);
            }
        }
    }
    Ok(())
}

/// A 1-D convolution built from shifted slices and a matrix product.
///
/// candle 0.11 computes wrong kernel gradients for `Tensor::conv1d` when the
/// batch has more than one item, so training uses this equivalent instead.
fn conv1d(x: &Tensor, weight: &Tensor, stride: usize) -> candle_core::Result<Tensor> {
    let (batch, channels, len) = x.dims3()?;
    let (out_channels, _, kernel) = weight.dims3()?;
    let out_len = (len - kernel) / stride + 1;
    let taps = (0..kernel)
        .map(|k| {
            let span = x.narrow(2, k, (out_len - 1) * stride + 1)?;
            if stride == 1 {
                return Ok(span);
            }
            span.pad_with_zeros(2, 0, stride - 1)?
                .reshape((batch, channels, out_len, stride))?
                .narrow(3, 0, 1)?
                .squeeze(3)
        })
        .collect::<candle_core::Result<Vec<_>>>()?;
    // One large matrix product is much faster than one per batch item.
    let patches = Tensor::stack(&taps, 1)?
        .transpose(0, 2)?
        .contiguous()?
        .reshape((channels * kernel, batch * out_len))?;
    weight
        .reshape((out_channels, channels * kernel))?
        .matmul(&patches)?
        .reshape((out_channels, batch, out_len))?
        .transpose(0, 1)
}

/// Numerically stable binary cross-entropy on logits.
fn bce_with_logits(logits: &Tensor, targets: &Tensor) -> candle_core::Result<Tensor> {
    let loss =
        ((logits.relu()? - (logits * targets)?)? + (logits.abs()?.neg()?.exp()? + 1.0)?.log()?)?;
    loss.mean_all()
}

/// The first CUDA GPU when built with the `cuda` feature and one is usable,
/// otherwise the CPU.
fn device() -> Device {
    if candle_core::utils::cuda_is_available() {
        match Device::new_cuda(0) {
            Ok(device) => {
                println!("training on the GPU");
                return device;
            }
            Err(err) => println!("no usable GPU ({err}), training on the CPU"),
        }
    } else {
        println!("training on the CPU (built without the `cuda` feature)");
    }
    Device::Cpu
}

pub fn train(opts: &TrainOptions) -> anyhow::Result<()> {
    let dataset = Dataset::load(&opts.data, &opts.backgrounds, &opts.sounds)?;
    println!("wake word {:?}: {}", dataset.phrase, dataset.summary());

    let mut hard_negatives: Vec<usize> = Vec::new();
    let train_aug = Augmenter {
        split: &dataset.train,
        backgrounds: &dataset.backgrounds,
        frames: opts.frames,
        hard_negatives: &[],
    };
    let device = device();
    let convs = convs(opts.width);
    let to_tensor =
        |feats: Vec<f32>, n: usize| Tensor::from_vec(feats, (n, N_MELS, opts.frames), &device);

    // Scale inputs to unit variance once the window mean is removed.
    let (sample, _) = batch(&train_aug, 512, 0.3, opts.seed ^ 0x5ca1e);
    let input_scale = 1.0 / std_without_window_mean(&sample, opts.frames).max(1e-3);

    let varmap = VarMap::new();
    let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
    let net = Net::new(vb, &convs, input_scale as f64)?;
    let mut optimizer = candle_nn::AdamW::new(
        varmap.all_vars(),
        ParamsAdamW {
            lr: opts.learning_rate,
            weight_decay: 1e-3,
            ..Default::default()
        },
    )?;

    let (val_feats, val_labels) = batch(
        &Augmenter {
            split: &dataset.val,
            backgrounds: &dataset.backgrounds,
            frames: opts.frames,
            hard_negatives: &[],
        },
        1024,
        0.3,
        opts.seed ^ 0xfa1,
    );
    let val_x = to_tensor(val_feats.clone(), val_labels.len())?;
    let val_y = Tensor::new(val_labels.as_slice(), &device)?;

    let vars = varmap.all_vars();
    let started = Instant::now();
    for step in 1..=opts.steps {
        // Linear warmup, then cosine decay to 5% of the initial rate.
        let warmup = (step as f64 / WARMUP_STEPS as f64).min(1.0);
        let progress = step as f64 / opts.steps as f64;
        let lr = opts.learning_rate
            * warmup
            * (0.05 + 0.95 * 0.5 * (1.0 + (std::f64::consts::PI * progress).cos()));
        optimizer.set_learning_rate(lr);

        if step >= MINING_START && step % MINING_EVERY == 0 {
            hard_negatives = mine_hard_negatives(&net, &train_aug, opts.seed ^ step as u64)?;
        }
        let aug = Augmenter {
            hard_negatives: &hard_negatives,
            ..train_aug
        };
        let (feats, labels) = batch(
            &aug,
            opts.batch_size,
            0.3,
            opts.seed.wrapping_add(step as u64 * 7919),
        );
        let x = to_tensor(feats, labels.len())?;
        let y = Tensor::new(labels.as_slice(), &device)?;
        let loss = bce_with_logits(&net.forward(&x)?, &y)?;
        let mut grads = loss.backward()?;
        clip_gradients(&mut grads, &vars, MAX_GRAD_NORM)?;
        optimizer.step(&grads)?;

        if step % 100 == 0 || step == opts.steps {
            let logits = net.forward(&val_x)?;
            let val_loss = bce_with_logits(&logits, &val_y)?.to_scalar::<f32>()?;
            let accuracy = logits
                .ge(0.0)?
                .to_dtype(DType::F32)?
                .eq(&val_y)?
                .to_dtype(DType::F32)?
                .mean_all()?
                .to_scalar::<f32>()?;
            println!(
                "step {step:>5}/{}  loss {:.4}  val loss {val_loss:.4}  val accuracy {:.2}%  lr {lr:.2e}  [{:.0}s]",
                opts.steps,
                loss.to_scalar::<f32>()?,
                accuracy * 100.0,
                started.elapsed().as_secs_f32()
            );
        }
    }

    let mut header = Header {
        frames: opts.frames,
        threshold: 0.5,
        input_scale,
        name: [0; NAME_LEN],
        convs,
    };
    header.set_name(&dataset.phrase);
    let weights = net.export()?;

    // The device must compute exactly what candle computed.
    let words = aligned_copy(&encode(&header, &weights)?);
    let model = Model::from_bytes(bytemuck_bytes(&words))?;
    let candle_logits = net.forward(&val_x.narrow(0, 0, 32)?)?.to_vec1::<f32>()?;
    let mut scratch = Scratch::default();
    let window = N_MELS * opts.frames;
    let max_diff = candle_logits
        .iter()
        .enumerate()
        .map(|(i, l)| {
            (model.infer(&val_feats[i * window..(i + 1) * window], &mut scratch) - l).abs()
        })
        .fold(0.0f32, f32::max);
    anyhow::ensure!(
        max_diff < 1e-2,
        "exported model disagrees with candle by {max_diff}"
    );

    println!("\nevaluating the streaming detector on held-out data...");
    let report = eval::evaluate(
        &model,
        &dataset,
        opts.eval_minutes,
        opts.seed,
        eval::CONSECUTIVE,
    )?;
    let threshold = report.pick_threshold(opts.max_false_accepts_per_hour);
    report.print(threshold);
    header.threshold = threshold;

    let bytes = encode(&header, &weights)?;
    if let Some(dir) = opts.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&opts.out, &bytes)
        .with_context(|| format!("cannot write {}", opts.out.display()))?;
    println!(
        "\nwrote {} ({} KB, threshold {threshold:.2}) in {:.0}s",
        opts.out.display(),
        bytes.len() / 1024,
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

fn std_without_window_mean(feats: &[f32], frames: usize) -> f32 {
    let window = N_MELS * frames;
    let mut sum_sq = 0.0f64;
    for w in feats.chunks(window) {
        let mean = w.iter().sum::<f32>() / window as f32;
        sum_sq += w.iter().map(|v| ((v - mean) as f64).powi(2)).sum::<f64>();
    }
    (sum_sq / feats.len() as f64).sqrt() as f32
}

pub fn bytemuck_bytes(words: &[u32]) -> &[u8] {
    bytemuck::cast_slice(words)
}

/// Loads a `.fkw` file into aligned storage.
pub fn read_model(path: &Path) -> anyhow::Result<(Vec<u32>, usize)> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok((aligned_copy(&bytes), bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv1d_matches_candle_forward() {
        let device = Device::Cpu;
        let x = Tensor::randn(0f32, 1.0, (3, 5, 23), &device).unwrap();
        let w = Tensor::randn(0f32, 1.0, (4, 5, 3), &device).unwrap();
        for stride in [1, 2, 3] {
            let ours = conv1d(&x, &w, stride).unwrap();
            let theirs = x.conv1d(&w, 0, stride, 1, 1).unwrap();
            let diff = (ours - theirs).unwrap().abs().unwrap().max_all().unwrap();
            assert!(diff.to_scalar::<f32>().unwrap() < 1e-4, "stride {stride}");
        }
    }
}
