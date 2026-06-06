//! Sine generator.

use crate::processing::{Module, PrepareCfg, ProcessCtx, Tail};
use std::f32::consts::TAU;

/// Generates a sine wave whose frequency and amplitude are controlled sample-by-sample by
/// input signals.
///
/// It keeps the current phase (radians) as state. Each sample it computes the per-sample
/// phase increment from the frequency at that sample, advances the phase, and emits
/// `sin(phase) * amplitude`.
///
/// Ports — inputs: `0` = frequency (Hz), `1` = amplitude; output: `0` = signal.
pub struct SineGenerator {
    phase: f32,
    sample_rate: f32,
}

impl SineGenerator {
    pub const FREQUENCY: usize = 0;
    pub const AMPLITUDE: usize = 1;
    pub const OUT: usize = 0;

    pub fn new() -> Self {
        Self {
            phase: 0.0,
            sample_rate: 0.0,
        }
    }
}

impl Default for SineGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SineGenerator {
    fn prepare(&mut self, cfg: &PrepareCfg) {
        self.sample_rate = cfg.sample_rate;
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }

    fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail {
        let frames = ctx.frames;
        let freq = ctx.inputs[Self::FREQUENCY];
        let amp = ctx.inputs[Self::AMPLITUDE];
        let out = &mut *ctx.outputs[Self::OUT];

        for i in 0..frames {
            let increment = TAU * freq[i] / self.sample_rate;
            self.phase = (self.phase + increment).rem_euclid(TAU);
            out[i] = self.phase.sin() * amp[i];
        }

        // A bare oscillator never keeps a voice alive on its own (an envelope does).
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one block with the given frequency/amplitude buffers, returning the output.
    fn run(sine: &mut SineGenerator, freq: &[f32], amp: &[f32]) -> Vec<f32> {
        let n = freq.len();
        let mut out = vec![0.0f32; n];
        let inputs: [&[f32]; 2] = [freq, amp];
        let mut outs: [&mut [f32]; 1] = [&mut out[..]];
        let mut ctx = ProcessCtx {
            frames: n,
            inputs: &inputs,
            outputs: &mut outs,
        };
        let tail = sine.process(&mut ctx);
        assert_eq!(tail, Tail::Done);
        out
    }

    #[test]
    fn produces_sine_at_constant_frequency() {
        // sample_rate 8, 1 Hz => phase advances TAU/8 per sample (increment-then-output).
        let mut sine = SineGenerator::new();
        sine.prepare(&PrepareCfg {
            sample_rate: 8.0,
            max_frames: 8,
        });
        let out = run(&mut sine, &[1.0; 8], &[1.0; 8]);
        let eps = 1e-5;
        assert!((out[1] - 1.0).abs() < eps); // sin(pi/2)
        assert!(out[3].abs() < eps); // sin(pi)
        assert!((out[5] + 1.0).abs() < eps); // sin(3pi/2)
    }

    #[test]
    fn amplitude_scales_per_sample() {
        let mut sine = SineGenerator::new();
        sine.prepare(&PrepareCfg {
            sample_rate: 8.0,
            max_frames: 4,
        });
        let out = run(&mut sine, &[1.0; 4], &[0.0, 0.5, 0.0, 0.0]);
        assert_eq!(out[0], 0.0); // amplitude 0 -> silent
        assert!((out[1] - 0.5).abs() < 1e-5); // sin(pi/2) * 0.5
    }

    #[test]
    fn frequency_controls_rate() {
        // inc = TAU * 25 / 100 = TAU/4 = pi/2 -> first sample is sin(pi/2) = 1.
        let mut sine = SineGenerator::new();
        sine.prepare(&PrepareCfg {
            sample_rate: 100.0,
            max_frames: 1,
        });
        let out = run(&mut sine, &[25.0], &[1.0]);
        assert!((out[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn phase_is_retained_across_blocks() {
        // Two 1-sample blocks at 1 Hz / sr 8 must match one 2-sample block.
        let cfg = PrepareCfg {
            sample_rate: 8.0,
            max_frames: 2,
        };
        let mut a = SineGenerator::new();
        a.prepare(&cfg);
        let s0 = run(&mut a, &[1.0], &[1.0])[0];
        let s1 = run(&mut a, &[1.0], &[1.0])[0];

        let mut b = SineGenerator::new();
        b.prepare(&cfg);
        let both = run(&mut b, &[1.0, 1.0], &[1.0, 1.0]);

        assert!((s0 - both[0]).abs() < 1e-6);
        assert!((s1 - both[1]).abs() < 1e-6);
    }
}
