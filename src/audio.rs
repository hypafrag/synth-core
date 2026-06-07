//! Real-time audio output backend (cpal).
//!
//! Drives a [`PlanEngine`] from the default output device's callback. The audio device callback
//! is the single clock (see `docs/architecture/07-execution-and-events.md`).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::plan_engine::PlanEngine;

#[derive(Debug)]
pub enum AudioError {
    NoDevice,
    Build(String),
    Play(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no default audio output device"),
            AudioError::Build(e) => write!(f, "failed to build audio stream: {e}"),
            AudioError::Play(e) => write!(f, "failed to start audio stream: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Start playing `engine` on the default output device. The returned stream must be kept
/// alive for audio to keep running.
pub fn run_default_output(mut engine: PlanEngine) -> Result<cpal::Stream, AudioError> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or(AudioError::NoDevice)?;

    let channels = engine.channels();
    let config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: engine.sample_rate() as u32,
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = if channels > 0 { data.len() / channels } else { 0 };
                engine.process_block(data, frames);
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(|e| AudioError::Build(e.to_string()))?;

    stream.play().map_err(|e| AudioError::Play(e.to_string()))?;
    Ok(stream)
}
