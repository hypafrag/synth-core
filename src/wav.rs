//! Offline WAV render backend.
//!
//! Drives an [`Engine`] block-by-block as fast as possible and writes the interleaved output
//! to a WAV file. This is the offline counterpart to the real-time `audio` backend: the same
//! engine and the same `audio_output` sink, with the samples going to a file instead of a
//! device.

use std::path::Path;

use crate::engine::Engine;

#[derive(Debug)]
pub struct WavError(String);

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wav render error: {}", self.0)
    }
}

impl std::error::Error for WavError {}

impl From<hound::Error> for WavError {
    fn from(e: hound::Error) -> Self {
        WavError(e.to_string())
    }
}

const BLOCK: usize = 1024;

/// Render `seconds` of audio from `engine` to a 16-bit PCM WAV at `path`.
pub fn render_to_wav(mut engine: Engine, path: &Path, seconds: f64) -> Result<(), WavError> {
    let sample_rate = engine.sample_rate();
    let channels = engine.channels();
    let total_frames = (seconds.max(0.0) * sample_rate as f64).round() as u64;

    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate: sample_rate as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;

    let mut buf = vec![0.0f32; BLOCK * channels];
    let mut written = 0u64;
    while written < total_frames {
        let frames = ((total_frames - written) as usize).min(BLOCK);
        let slice = &mut buf[..frames * channels];
        engine.process_block(slice, frames);
        for &sample in slice.iter() {
            let clamped = sample.clamp(-1.0, 1.0);
            writer.write_sample((clamped * i16::MAX as f32) as i16)?;
        }
        written += frames as u64;
    }

    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Patch;
    use crate::registry::Registry;

    #[test]
    fn renders_pure_tone_wav() {
        let yaml = r#"
nodes:
  - id: freq
    type: const_generator
    params: { value: 440.0 }
  - id: amp
    type: const_generator
    params: { value: 0.5 }
  - id: osc
    type: sine_generator
  - id: out
    type: audio_output
    params: { sample_rate: 8000, channels: 2 }
wires:
  - { from: [freq, out], to: [osc, frequency] }
  - { from: [amp,  out], to: [osc, amplitude] }
  - { from: [osc, out], to: [out, ch0] }
  - { from: [osc, out], to: [out, ch1] }
"#;
        let patch = Patch::from_yaml(yaml).unwrap();
        let engine = Engine::build(&patch, &Registry::with_builtins(), 16384).unwrap();

        let path = std::env::temp_dir().join("synth_core_wav_test.wav");
        render_to_wav(engine, &path, 0.1).unwrap();

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 8000);
        assert_eq!(reader.spec().channels, 2);
        // 0.1 s * 8000 Hz = 800 frames * 2 channels = 1600 samples.
        assert_eq!(reader.len(), 1600);

        let samples: Vec<i16> = reader.into_samples::<i16>().map(|s| s.unwrap()).collect();
        assert!(samples.iter().any(|&s| s != 0)); // produced sound

        std::fs::remove_file(&path).ok();
    }
}
