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
    let sample_rate = engine.sample_rate() as u32;
    let config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate,
        buffer_size: pick_buffer_size(&device, sample_rate),
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

/// Choose an explicit output buffer size.
///
/// `BufferSize::Default` leaves the period size to the backend, and on ALSA (e.g. Raspberry Pi)
/// that period can be tiny — a few dozen frames — so the callback deadline is missed continuously
/// and the device reports xruns even for a trivial patch. Requesting a comfortable buffer of
/// ~20 ms, clamped to the device's supported range, gives the callback enough headroom. If the
/// device doesn't report a range (or reports `Unknown`), fall back to the backend default.
fn pick_buffer_size(device: &cpal::Device, sample_rate: u32) -> cpal::BufferSize {
    use cpal::traits::DeviceTrait;

    // Target ~20 ms of headroom.
    let target = (sample_rate as f32 * 0.020) as u32;

    let range = device
        .supported_output_configs()
        .ok()
        .into_iter()
        .flatten()
        .find_map(|c| match c.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            cpal::SupportedBufferSize::Unknown => None,
        });

    match range {
        Some((min, max)) => cpal::BufferSize::Fixed(target.clamp(min, max)),
        None => cpal::BufferSize::Default,
    }
}
