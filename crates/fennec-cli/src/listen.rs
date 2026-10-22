//! Runs the detector on a WAV file or a live raw audio stream.

use std::io::Read;
use std::path::Path;

use fennec_core::SAMPLE_RATE;
use fennec_core::detector::{Detector, DetectorConfig};
use fennec_core::model::Model;

use crate::audio::{read_wav, to_i16};

pub fn listen(
    model: Model<'_>,
    input: &Path,
    threshold: Option<f32>,
    show_scores: bool,
) -> anyhow::Result<()> {
    let mut config = DetectorConfig::for_model(&model);
    if let Some(t) = threshold {
        config.threshold = t;
    }
    println!(
        "listening for {:?} (threshold {:.2})",
        model.header.name(),
        config.threshold
    );
    let mut detector = Detector::new(model, config);
    let mut samples_seen = 0usize;
    let mut on_chunk = |chunk: &[i16], detector: &mut Detector<'_>| {
        let detection = detector.push(chunk);
        samples_seen += chunk.len();
        let t = samples_seen as f32 / SAMPLE_RATE as f32;
        if show_scores && let Some(p) = detector.take_evaluation() {
            let bar = "#".repeat((p * 40.0) as usize);
            println!("{t:8.2}s  {p:.3}  {bar}");
        }
        if let Some(d) = detection {
            println!("{t:8.2}s  DETECTED (probability {:.3})", d.probability);
        }
    };

    if input == Path::new("-") {
        let mut stdin = std::io::stdin().lock();
        let mut bytes = [0u8; 320];
        loop {
            match stdin.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let chunk: Vec<i16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| i16::from_le_bytes(b))
                .collect();
            on_chunk(&chunk, &mut detector);
        }
    } else {
        let samples: Vec<i16> = read_wav(input)?.iter().map(|&s| to_i16(s)).collect();
        for chunk in samples.chunks(160) {
            on_chunk(chunk, &mut detector);
        }
    }
    Ok(())
}
