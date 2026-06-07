//! Range mapper.

use crate::processing::{Module, ProcessCtx, Tail};

/// Linearly maps a bipolar input in `[-1, 1]` to `[low, high]`: `in = -1` → `low`, `in = 0`
/// → midpoint, `in = 1` → `high`. Values outside `[-1, 1]` extrapolate. Typical use: scale an
/// LFO (which is bipolar) to a parameter range, e.g. `[-1, 1]` → `[400, 480]` Hz.
///
/// `low` and `high` are `Sample` inputs (so the target range can itself be modulated).
///
/// Ports — inputs: `0` = in, `1` = low, `2` = high; output: `0` = out.
pub struct Range;

impl Range {
    pub const IN: usize = 0;
    pub const LOW: usize = 1;
    pub const HIGH: usize = 2;
    pub const OUT: usize = 0;

    pub fn new() -> Self {
        Self
    }
}

impl Default for Range {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Range {
    fn input_ports(&self) -> Vec<String> {
        vec!["in".to_string(), "low".to_string(), "high".to_string()]
    }

    fn output_ports(&self) -> Vec<String> {
        vec!["out".to_string()]
    }

    fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail {
        let frames = ctx.frames;
        let input = ctx.inputs[Self::IN];
        let low = ctx.inputs[Self::LOW];
        let high = ctx.inputs[Self::HIGH];
        let out = &mut *ctx.outputs[Self::OUT];

        out[..frames]
            .iter_mut()
            .zip(&input[..frames])
            .zip(&low[..frames])
            .zip(&high[..frames])
            .for_each(|(((sample, &x), &lo), &hi)| {
                *sample = lo + (x + 1.0) * 0.5 * (hi - lo);
            });

        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(r: &mut Range, input: &[f32], low: &[f32], high: &[f32]) -> Vec<f32> {
        let n = input.len();
        let mut out = vec![0.0f32; n];
        let inputs: [&[f32]; 3] = [input, low, high];
        let mut outs: [&mut [f32]; 1] = [&mut out[..]];
        let mut ctx = ProcessCtx {
            frames: n,
            sample_rate: 44100.0,
            time: 0.0,
            inputs: &inputs,
            outputs: &mut outs,
        };
        assert_eq!(r.process(&mut ctx), Tail::Done);
        out
    }

    #[test]
    fn maps_bipolar_to_range() {
        let mut r = Range::new();
        let out = run(&mut r, &[-1.0, 0.0, 1.0], &[400.0; 3], &[480.0; 3]);
        assert_eq!(out[0], 400.0); // -1 -> low
        assert_eq!(out[1], 440.0); //  0 -> midpoint
        assert_eq!(out[2], 480.0); //  1 -> high
    }
}
