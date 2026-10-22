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

/// A bout of one to three coughs, or a throat clearing, from one person.
///
/// A cough has the rhythm of a short word: an explosive burst, a noisy
/// stretch through the open mouth, and often a voiced end ("kh-uh"). Two of
/// them in a row sound a lot like two syllables, so the bouts keep the same
/// voice and mouth throughout, as a person would.
fn cough(rng: &mut impl Rng) -> Vec<f32> {
    let pitch = rng.random_range(85.0..260.0);
    let tract = [
        rng.random_range(350.0..900.0),
        rng.random_range(1000.0..2200.0),
        rng.random_range(2300.0..3500.0),
    ];
    if rng.random_bool(0.25) {
        return throat_clearing(pitch, rng);
    }
    let mut out = Vec::new();
    let mut at = 0;
    for i in 0..rng.random_range(1..4) {
        let level = if i == 0 { 1.0 } else { rng.random_range(0.4..1.0) };
        let burst: Vec<f32> = one_cough(pitch, tract, rng)
            .iter()
            .map(|s| s * level)
            .collect();
        add_at(&mut out, at, &burst);
        at += burst.len() * rng.random_range(5..9) / 10 + samples(rng.random_range(0.03..0.3));
    }
    out
}

fn one_cough(pitch: f32, tract: [f32; 3], rng: &mut impl Rng) -> Vec<f32> {
    let explosion = samples(rng.random_range(0.015..0.04));
    let len = explosion + samples(rng.random_range(0.1..0.3));
    let decay = rng.random_range(0.04..0.15) * RATE;
    let attack = samples(rng.random_range(0.001..0.006)).max(1);
    let mut out = white(len, rng);
    for (n, s) in out.iter_mut().enumerate() {
        let rise = (n as f32 / attack as f32).min(1.0);
        // The explosion is louder than the rest of the cough.
        let boost = if n < explosion { 1.8 } else { 1.0 };
        *s *= boost * rise * (-(n.saturating_sub(explosion) as f32) / decay).exp();
    }
    if rng.random_bool(0.6) {
        // The vocal folds join in after the explosion, then fade out.
        let voiced = voice(
            pitch * rng.random_range(1.0..1.4),
            pitch * rng.random_range(0.7..1.0),
            samples(rng.random_range(0.06..0.25)),
            rng,
        );
        let level = rng.random_range(0.3..1.2);
        let voiced: Vec<f32> = voiced.iter().map(|s| s * level).collect();
        add_at(&mut out, explosion + samples(rng.random_range(0.0..0.08)), &voiced);
    }
    vocal_tract(&mut out, tract, rng);
    out
}

/// "Ahem" or "hm-hm": one or two voiced humps through a mostly closed mouth.
fn throat_clearing(pitch: f32, rng: &mut impl Rng) -> Vec<f32> {
    let mut out = Vec::new();
    let mut at = 0;
    let rise = rng.random_range(0.9..1.3);
    for _ in 0..rng.random_range(1..3) {
        let len = samples(rng.random_range(0.12..0.35));
        let mut hump = voice(pitch * rise, pitch * rng.random_range(0.8..1.1), len, rng);
        // A little breath noise through the throat.
        for s in hump.iter_mut() {
            *s += rng.random_range(-0.15..0.15);
        }
        for (n, s) in hump.iter_mut().enumerate() {
            *s *= (std::f32::consts::PI * n as f32 / len as f32).sin().sqrt();
        }
        let tract = [
            rng.random_range(220.0..500.0),
            rng.random_range(900.0..1600.0),
            rng.random_range(2200.0..3000.0),
        ];
        vocal_tract(&mut hump, tract, rng);
        add_at(&mut out, at, &hump);
        at += len + samples(rng.random_range(0.06..0.25));
    }
    out
}

/// A glottal pulse train gliding from `from` to `to` Hz, with the small
/// cycle to cycle wobble of a real voice.
fn voice(from: f32, to: f32, len: usize, rng: &mut impl Rng) -> Vec<f32> {
    let mut phase = 0.0f32;
    let mut jitter = 1.0;
    (0..len)
        .map(|n| {
            let progress = n as f32 / len as f32;
            phase += (from + (to - from) * progress) * jitter / RATE;
            if phase >= 1.0 {
                phase -= 1.0;
                jitter = rng.random_range(0.97..1.03);
            }
            1.0 - 2.0 * phase
        })
        .collect()
}

/// Shapes a sound with the resonances of a throat and mouth.
fn vocal_tract(clip: &mut [f32], formants: [f32; 3], rng: &mut impl Rng) {
    for (formant, gain) in formants.into_iter().zip([12.0, 9.0, 6.0]) {
        filter(clip, Filter::Peak(gain), formant, 3.0);
    }
    filter(
        clip,
        Filter::LowPass,
        rng.random_range(3000.0..6500.0),
        0.707,
    );
    filter(clip, Filter::HighPass, 80.0, 0.707);
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
