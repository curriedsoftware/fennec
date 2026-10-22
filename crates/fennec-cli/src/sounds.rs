//! Synthetic non-speech sounds: coughs, knocks, claps, clatter, beeps and
//! breaths. They are short, loud and close to the microphone, unlike the
//! background noise, and every negative window can hold them, so the model
//! learns that a sudden sound on its own is not the wake word. Recordings in
//! `sounds/` do the same with real sounds; these work without any.

use std::f32::consts::TAU;

use fennec_core::SAMPLE_RATE;
use rand::{Rng, RngExt};

use crate::audio::{Filter, filter};

const RATE: f32 = SAMPLE_RATE as f32;

/// One random sound event, with a peak of about 1.
pub fn event(rng: &mut impl Rng) -> Vec<f32> {
    let mut out = match rng.random_range(0..100) {
        0..35 => cough(rng),
        35..55 => knocks(rng),
        55..70 => clatter(rng),
        70..85 => beeps(rng),
        _ => breath(rng),
    };
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-9);
    out.iter_mut().for_each(|s| *s /= peak);
    out
}

fn samples(seconds: f32) -> usize {
    (seconds * RATE) as usize
}

fn white(len: usize, rng: &mut impl Rng) -> Vec<f32> {
    (0..len).map(|_| rng.random_range(-1.0..1.0)).collect()
}

/// Adds `burst` to `out` starting at `at`, growing `out` as needed.
fn add_at(out: &mut Vec<f32>, at: usize, burst: &[f32]) {
    if out.len() < at + burst.len() {
        out.resize(at + burst.len(), 0.0);
    }
    out[at..at + burst.len()]
        .iter_mut()
        .zip(burst)
        .for_each(|(o, s)| *o += s);
}

/// One to three coughs, or a throat clearing: noise through the vocal
/// tract, starting abruptly and fading out, sometimes with a voiced tail.
fn cough(rng: &mut impl Rng) -> Vec<f32> {
    let mut out = Vec::new();
    let mut at = 0;
    for _ in 0..rng.random_range(1..4) {
        let len = samples(rng.random_range(0.15..0.45));
        let attack = samples(rng.random_range(0.005..0.02)).max(1);
        let decay = rng.random_range(0.04..0.15) * RATE;
        let mut burst = white(len, rng);
        // A voiced tail, like "uhh" at the end of a cough.
        if rng.random_bool(0.5) {
            let pitch = rng.random_range(90.0..250.0);
            for (n, s) in burst.iter_mut().enumerate() {
                let phase = (n as f32 * pitch / RATE).fract();
                *s += 0.8 * (1.0 - 2.0 * phase);
            }
        }
        for (formant, gain) in [
            (rng.random_range(350.0..900.0), 12.0),
            (rng.random_range(1100.0..2500.0), 9.0),
            (rng.random_range(2500.0..3800.0), 6.0),
        ] {
            filter(&mut burst, Filter::Peak(gain), formant, 3.0);
        }
        filter(
            &mut burst,
            Filter::LowPass,
            rng.random_range(3000.0..6500.0),
            0.707,
        );
        filter(&mut burst, Filter::HighPass, 120.0, 0.707);
        for (n, s) in burst.iter_mut().enumerate() {
            let rise = (n as f32 / attack as f32).min(1.0);
            *s *= rise * (-(n.saturating_sub(attack) as f32) / decay).exp();
        }
        add_at(&mut out, at, &burst);
        at += len + samples(rng.random_range(0.05..0.3));
    }
    out
}

/// A decaying noise click that rings at a few resonances, like a knuckle on
/// a door, a hand clap or a cup put down on a table.
fn hit(rng: &mut impl Rng, ring: std::ops::Range<f32>) -> Vec<f32> {
    let len = samples(ring.end * 3.0);
    let click = rng.random_range(0.001..0.008) * RATE;
    let mut out: Vec<f32> = white(len, rng)
        .into_iter()
        .enumerate()
        .map(|(n, s)| s * (-(n as f32) / click).exp())
        .collect();
    for _ in 0..rng.random_range(1..4) {
        let freq = rng.random_range(150.0..5000.0);
        let decay = rng.random_range(ring.clone()) * RATE;
        let level = rng.random_range(0.2..1.0);
        let phase = rng.random_range(0.0..TAU);
        for (n, s) in out.iter_mut().enumerate() {
            let t = n as f32;
            *s += level * (TAU * freq * t / RATE + phase).sin() * (-t / decay).exp();
        }
    }
    out
}

/// Knocks or claps: the same hit a few times at a steady pace.
fn knocks(rng: &mut impl Rng) -> Vec<f32> {
    let hit = hit(rng, 0.01..0.06);
    let pace = samples(rng.random_range(0.1..0.4));
    let mut out = Vec::new();
    for i in 0..rng.random_range(1..6) {
        let level = rng.random_range(0.6..1.0);
        let hit: Vec<f32> = hit.iter().map(|s| s * level).collect();
        add_at(&mut out, i * pace, &hit);
    }
    out
}

/// Dishes, cutlery, keys: different hits at irregular times.
fn clatter(rng: &mut impl Rng) -> Vec<f32> {
    let mut out = Vec::new();
    let mut at = 0;
    for _ in 0..rng.random_range(2..8) {
        let level = rng.random_range(0.3..1.0);
        let hit: Vec<f32> = hit(rng, 0.03..0.3).iter().map(|s| s * level).collect();
        add_at(&mut out, at, &hit);
        at += samples(rng.random_range(0.02..0.25));
    }
    out
}

/// Appliance beeps, phone tones or a whistle: tones with a few harmonics,
/// sometimes gliding in pitch.
fn beeps(rng: &mut impl Rng) -> Vec<f32> {
    let freq = rng.random_range(400.0..3500.0);
    let glide = if rng.random_bool(0.3) {
        rng.random_range(-0.5..0.5)
    } else {
        0.0
    };
    let len = samples(rng.random_range(0.06..0.6));
    let gap = samples(rng.random_range(0.05..0.3));
    let harmonics = rng.random_range(1..4);
    let mut out = Vec::new();
    for i in 0..rng.random_range(1..5) {
        let mut phase = 0.0f32;
        let tone: Vec<f32> = (0..len)
            .map(|n| {
                let progress = n as f32 / len as f32;
                phase += TAU * freq * (1.0 + glide * progress) / RATE;
                // 5 ms fades, so that the tone has no clicks of its own.
                let fade = (n.min(len - n) as f32 / (0.005 * RATE)).min(1.0);
                (1..=harmonics)
                    .map(|h| (phase * h as f32).sin() / h as f32)
                    .sum::<f32>()
                    * fade
            })
            .collect();
        add_at(&mut out, i * (len + gap), &tone);
    }
    out
}

/// Breathing in or out, a sigh or a sniff: filtered noise that swells and
/// fades.
fn breath(rng: &mut impl Rng) -> Vec<f32> {
    let len = samples(rng.random_range(0.2..1.0));
    let mut out = white(len, rng);
    filter(
        &mut out,
        Filter::Peak(10.0),
        rng.random_range(500.0..2500.0),
        1.0,
    );
    filter(
        &mut out,
        Filter::LowPass,
        rng.random_range(2000.0..6000.0),
        0.707,
    );
    filter(&mut out, Filter::HighPass, 200.0, 0.707);
    for (n, s) in out.iter_mut().enumerate() {
        *s *= (std::f32::consts::PI * n as f32 / len as f32).sin();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn events_are_short_and_audible() {
        let mut rng = rand::rngs::Xoshiro256PlusPlus::seed_from_u64(7);
        for _ in 0..200 {
            let clip = event(&mut rng);
            assert!(clip.len() > samples(0.02), "{}", clip.len());
            assert!(clip.len() < samples(4.0), "{}", clip.len());
            assert!(clip.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
            assert!(crate::audio::rms(&clip) > 0.01);
        }
    }
}
