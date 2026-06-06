//! Sine generator.

use crate::processing::{Module, ProcessCtx, Tail};
use std::f64::consts::TAU;

/// Generates a sine wave whose frequency and amplitude are controlled sample-by-sample by
/// input signals.
///
/// It is **stateless**: each sample's phase is computed from the block time
/// (`ctx.time`, voice-local) and the frequency at that sample — `sin(2π · f · t) · amp`. The
/// time is fractioned into cycles before the `sin`, so the argument stays bounded regardless
/// of how long the stream has run.
///
/// Note: phase comes from `f · t`, not an integral of `f` over time, so this is exact for a
/// steady (or slowly changing) frequency but is not a phase-accumulating FM oscillator.
///
/// Ports — inputs: `0` = frequency (Hz), `1` = amplitude; output: `0` = signal.
pub struct SineGenerator;

impl SineGenerator {
    pub const FREQUENCY: usize = 0;
    pub const AMPLITUDE: usize = 1;
    pub const OUT: usize = 0;

    pub fn new() -> Self {
        Self
    }
}

impl Default for SineGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SineGenerator {
    fn input_ports(&self) -> Vec<String> {
        vec!["frequency".to_string(), "amplitude".to_string()]
    }

    fn output_ports(&self) -> Vec<String> {
        vec!["out".to_string()]
    }

    fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail {
        let frames = ctx.frames;
        let sample_rate = ctx.sample_rate as f64;
        let t0 = ctx.time;
        let freq = ctx.inputs[Self::FREQUENCY];
        let amp = ctx.inputs[Self::AMPLITUDE];
        let out = &mut *ctx.outputs[Self::OUT];

        out[..frames]
            .iter_mut()
            .zip(&freq[..frames])
            .zip(&amp[..frames])
            .enumerate()
            .for_each(|(i, ((sample, &f), &a))| {
                let t = t0 + i as f64 / sample_rate;
                // Fractional cycles keep the sin argument in [0, 2π) for precision.
                let phase = TAU * (f as f64 * t).rem_euclid(1.0);
                *sample = phase.sin() as f32 * a;
            });

        // A bare oscillator never keeps a voice alive on its own (an envelope does).
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one block starting at `time` (seconds), at `sample_rate`.
    fn run(
        sine: &mut SineGenerator,
        sample_rate: f32,
        time: f64,
        freq: &[f32],
        amp: &[f32],
    ) -> Vec<f32> {
        let n = freq.len();
        let mut out = vec![0.0f32; n];
        let inputs: [&[f32]; 2] = [freq, amp];
        let mut outs: [&mut [f32]; 1] = [&mut out[..]];
        let mut ctx = ProcessCtx {
            frames: n,
            sample_rate,
            time,
            inputs: &inputs,
            outputs: &mut outs,
        };
        let tail = sine.process(&mut ctx);
        assert_eq!(tail, Tail::Done);
        out
    }

    #[test]
    fn produces_sine_at_constant_frequency() {
        // sr 8, 1 Hz, from t=0: phase = 2π·(i/8), so sin hits 0,…,1 at i=2, 0 at i=4, -1 at i=6.
        let mut sine = SineGenerator::new();
        let out = run(&mut sine, 8.0, 0.0, &[1.0; 8], &[1.0; 8]);
        let eps = 1e-5;
        assert!(out[0].abs() < eps); // sin(0)
        assert!((out[2] - 1.0).abs() < eps); // sin(pi/2)
        assert!(out[4].abs() < eps); // sin(pi)
        assert!((out[6] + 1.0).abs() < eps); // sin(3pi/2)
    }

    #[test]
    fn amplitude_scales_per_sample() {
        // sr 4, 1 Hz: at i=1 the phase is pi/2 (sin = 1), so out[1] = amplitude there.
        let mut sine = SineGenerator::new();
        let out = run(&mut sine, 4.0, 0.0, &[1.0; 4], &[0.0, 0.5, 0.0, 0.0]);
        assert_eq!(out[0], 0.0); // amplitude 0 (and sin(0)=0)
        assert!((out[1] - 0.5).abs() < 1e-5); // sin(pi/2) * 0.5
    }

    #[test]
    fn frequency_controls_rate() {
        // 25 Hz at sr 100: phase advances pi/2 per sample, so out[1] = sin(pi/2) = 1.
        let mut sine = SineGenerator::new();
        let out = run(&mut sine, 100.0, 0.0, &[25.0, 25.0], &[1.0, 1.0]);
        assert!((out[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn time_makes_blocks_continuous() {
        // Two consecutive 1-sample blocks (advancing `time`) match one 2-sample block.
        let sr = 8.0;
        let mut a = SineGenerator::new();
        let s0 = run(&mut a, sr, 0.0, &[1.0], &[1.0])[0];
        let s1 = run(&mut a, sr, 1.0 / sr as f64, &[1.0], &[1.0])[0];

        let mut b = SineGenerator::new();
        let both = run(&mut b, sr, 0.0, &[1.0, 1.0], &[1.0, 1.0]);

        assert!((s0 - both[0]).abs() < 1e-6);
        assert!((s1 - both[1]).abs() < 1e-6);
    }

    #[test]
    fn is_stateless() {
        // Stateless: same inputs and time always give the same output.
        let mut a = SineGenerator::new();
        let first = run(&mut a, 44100.0, 1.5, &[440.0; 16], &[1.0; 16]);
        let second = run(&mut a, 44100.0, 1.5, &[440.0; 16], &[1.0; 16]);
        assert_eq!(first, second);
    }
}
