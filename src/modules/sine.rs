//! Sine generator.

use crate::processing::{Module, ProcessCtx, Tail};
use std::f32::consts::TAU;

/// Generates a sine wave whose frequency and amplitude are controlled sample-by-sample by
/// input signals.
///
/// It keeps the current phase (radians) as state and **accumulates** it: each sample it adds
/// the per-sample increment `2π · frequency / sample_rate`, then emits `sin(phase) · amplitude`.
/// Accumulation integrates frequency over time, so frequency modulation (vibrato, FM) is
/// correct — the modulation depth stays constant instead of growing with elapsed time.
///
/// Ports — inputs: `0` = frequency (Hz), `1` = amplitude; output: `0` = signal.
pub struct SineGenerator {
    phase: f32,
}

impl SineGenerator {
    pub const FREQUENCY: usize = 0;
    pub const AMPLITUDE: usize = 1;
    pub const OUT: usize = 0;

    pub fn new() -> Self {
        Self { phase: 0.0 }
    }
}

impl Default for SineGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SineGenerator {
    fn reset(&mut self) {
        self.phase = 0.0;
    }

    fn input_ports(&self) -> Vec<String> {
        vec!["frequency".to_string(), "amplitude".to_string()]
    }

    fn output_ports(&self) -> Vec<String> {
        vec!["out".to_string()]
    }

    fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail {
        let frames = ctx.frames;
        let sample_rate = ctx.sample_rate;
        let freq = ctx.inputs[Self::FREQUENCY];
        let amp = ctx.inputs[Self::AMPLITUDE];
        let out = &mut *ctx.outputs[Self::OUT];

        // Thread the phase through the block with a fold: each step advances the phase by the
        // per-sample increment (from the frequency at that sample) and writes
        // sin(phase) * amplitude. The fold's result is the phase carried to the next block.
        self.phase = out[..frames]
            .iter_mut()
            .zip(&freq[..frames])
            .zip(&amp[..frames])
            .fold(self.phase, |phase, ((sample, &f), &a)| {
                let phase = (phase + TAU * f / sample_rate).rem_euclid(TAU);
                *sample = phase.sin() * a;
                phase
            });

        // A bare oscillator never keeps a voice alive on its own (an envelope does).
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one block at `sample_rate` with the given frequency/amplitude buffers.
    fn run(sine: &mut SineGenerator, sample_rate: f32, freq: &[f32], amp: &[f32]) -> Vec<f32> {
        let n = freq.len();
        let mut out = vec![0.0f32; n];
        let inputs: [&[f32]; 2] = [freq, amp];
        let mut outs: [&mut [f32]; 1] = [&mut out[..]];
        let mut ctx = ProcessCtx {
            frames: n,
            sample_rate,
            time: 0.0,
            inputs: &inputs,
            outputs: &mut outs,
        };
        let tail = sine.process(&mut ctx);
        assert_eq!(tail, Tail::Done);
        out
    }

    #[test]
    fn produces_sine_at_constant_frequency() {
        // sr 8, 1 Hz => phase advances TAU/8 per sample (increment-then-output).
        let mut sine = SineGenerator::new();
        let out = run(&mut sine, 8.0, &[1.0; 8], &[1.0; 8]);
        let eps = 1e-5;
        assert!((out[1] - 1.0).abs() < eps); // sin(pi/2)
        assert!(out[3].abs() < eps); // sin(pi)
        assert!((out[5] + 1.0).abs() < eps); // sin(3pi/2)
    }

    #[test]
    fn amplitude_scales_per_sample() {
        let mut sine = SineGenerator::new();
        let out = run(&mut sine, 8.0, &[1.0; 4], &[0.0, 0.5, 0.0, 0.0]);
        assert_eq!(out[0], 0.0); // amplitude 0 -> silent
        assert!((out[1] - 0.5).abs() < 1e-5); // sin(pi/2) * 0.5
    }

    #[test]
    fn frequency_controls_rate() {
        // inc = TAU * 25 / 100 = TAU/4 = pi/2 -> first sample is sin(pi/2) = 1.
        let mut sine = SineGenerator::new();
        let out = run(&mut sine, 100.0, &[25.0], &[1.0]);
        assert!((out[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn phase_is_retained_across_blocks() {
        // Two 1-sample blocks at 1 Hz / sr 8 must match one 2-sample block.
        let mut a = SineGenerator::new();
        let s0 = run(&mut a, 8.0, &[1.0], &[1.0])[0];
        let s1 = run(&mut a, 8.0, &[1.0], &[1.0])[0];

        let mut b = SineGenerator::new();
        let both = run(&mut b, 8.0, &[1.0, 1.0], &[1.0, 1.0]);

        assert!((s0 - both[0]).abs() < 1e-6);
        assert!((s1 - both[1]).abs() < 1e-6);
    }

    #[test]
    fn modulation_depth_does_not_grow_with_time() {
        // Frequency modulation must integrate, so the per-sample phase step stays bounded by
        // the current frequency no matter how long the oscillator has run. We feed a fixed
        // frequency far into the stream and check the step matches that frequency exactly
        // (a `f * t` oscillator would instead show ever-larger steps).
        let sr = 1000.0;
        let mut sine = SineGenerator::new();
        // Run 100_000 samples at 50 Hz to advance phase well into the stream.
        let _ = run(&mut sine, sr, &vec![50.0; 100_000], &vec![1.0; 100_000]);
        let phase_before = sine.phase;
        // One more sample at 50 Hz: the step must be exactly TAU * 50 / 1000.
        let _ = run(&mut sine, sr, &[50.0], &[1.0]);
        let expected_step = TAU * 50.0 / sr;
        let step = (sine.phase - phase_before).rem_euclid(TAU);
        assert!((step - expected_step).abs() < 1e-3);
    }
}
