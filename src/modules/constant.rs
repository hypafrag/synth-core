//! Constant generator.

use crate::model::Params;
use crate::processing::{Module, ProcessCtx, Tail};

/// Emits a constant value on its output every sample. This is how a steady value (a "knob")
/// enters the graph: wire a `ConstGenerator` into a `Sample` input.
///
/// Ports — output: `0` = `out`. Param: `value` (number).
pub struct ConstGenerator {
    value: f32,
}

impl ConstGenerator {
    pub fn new(value: f32) -> Self {
        Self { value }
    }

    pub fn from_params(params: &Params) -> Result<Self, String> {
        let value = params
            .get("value")
            .and_then(|v| v.as_f64())
            .ok_or("const_generator requires a numeric `value` param")? as f32;
        Ok(Self::new(value))
    }
}

impl Module for ConstGenerator {
    fn output_ports(&self) -> Vec<String> {
        vec!["out".to_string()]
    }

    fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail {
        let frames = ctx.frames;
        ctx.outputs[0][..frames].fill(self.value);
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_constant() {
        let mut c = ConstGenerator::new(0.5);
        let mut out = vec![0.0f32; 4];
        let inputs: [&[f32]; 0] = [];
        let mut outs: [&mut [f32]; 1] = [&mut out[..]];
        let mut ctx = ProcessCtx {
            frames: 4,
            sample_rate: 44100.0,
            time: 0.0,
            inputs: &inputs,
            outputs: &mut outs,
        };
        assert_eq!(c.process(&mut ctx), Tail::Done);
        assert_eq!(out, vec![0.5; 4]);
    }
}
