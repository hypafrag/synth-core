//! `sawtooth_generator` — phase-accumulating rising sawtooth. Inputs: frequency (Hz), amplitude.

use std::f32::consts::{PI, TAU};

use crate::model::Params;
use crate::module::{Icon, Inputs, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Sawtooth;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SawtoothState {
    pub phase: f32,
}

impl ModuleType for Sawtooth {
    type State = SawtoothState;
    const ICON: Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000110000000000110000,
        0b00000000000001110000000001110000,
        0b00000000000011010000000011010000,
        0b00000000000110010000000110010000,
        0b00000000001100010000001100010000,
        0b00000000011000010000011000010000,
        0b00000000110000010000110000010000,
        0b00000001100000010001100000010000,
        0b00000011000000010011000000010000,
        0b00000110000000010110000000010000,
        0b00001100000000011100000000010000,
        0b00011000000000011000000000010000,
        0b00010000000000010000000000010000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_p: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: Inputs::Fixed(vec![PortDesc::sample("frequency"), PortDesc::sample("amplitude")]),
            outputs: vec![PortDesc::sample("out")],
            params: vec![],
        }
    }

    fn init_state(_p: &Params) -> SawtoothState {
        SawtoothState { phase: 0.0 }
    }

    fn process(state: &mut SawtoothState, ctx: &ModuleCtx) -> Tail {
        let sr = ctx.sample_rate;
        let frames = ctx.frames;
        let freq = ctx.input(0);
        let amp = ctx.input(1);
        let out = ctx.output(0);
        state.phase = out[..frames]
            .iter_mut()
            .zip(&freq[..frames])
            .zip(&amp[..frames])
            .fold(state.phase, |phase, ((o, &f), &a)| {
                let phase = (phase + TAU * f / sr).rem_euclid(TAU);
                // Rising ramp: phase/PI spans [0, 2), so this spans [-1, 1).
                *o = (phase / PI - 1.0) * a;
                phase
            });
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ParamValue;
    use crate::module::ModuleEntry;
    use crate::module::test_support::params;
    use crate::plan::{Plan, RecordSpec, Source};

    #[test]
    fn ramps_through_the_plan() {
        // const(freq=1), const(amp=1) feed a saw at sr 8 -> phase advances PI/4 per sample.
        let cst = ModuleEntry::of::<crate::modules::const_generator::Const>();
        let saw = ModuleEntry::of::<Sawtooth>();
        let fns = &[cst.process, saw.process];
        let freq = params(&[("value", ParamValue::Float(1.0))]);
        let amp = params(&[("value", ParamValue::Float(1.0))]);
        let records = vec![
            RecordSpec { fn_index: 0, state: (cst.init_bytes)(&freq), inputs: vec![], num_outputs: 1 },
            RecordSpec { fn_index: 0, state: (cst.init_bytes)(&amp), inputs: vec![], num_outputs: 1 },
            RecordSpec {
                fn_index: 1,
                state: (saw.init_bytes)(&Params::new()),
                inputs: vec![Source::Port(0, 0), Source::Port(1, 0)],
                num_outputs: 1,
            },
        ];
        let mut plan = Plan::build(8, &records);
        plan.run(fns, 8.0, 0.0, 8);
        let out = plan.buffer_at(plan.output_offset(2, 0), 8);
        // Mid-cycle samples (away from the PI / TAU boundaries, which are FP-fragile).
        let eps = 1e-4;
        assert!((out[0] + 0.75).abs() < eps); // phase = PI/4 -> -0.75
        assert!((out[1] + 0.5).abs() < eps); // phase = PI/2 -> -0.5
        assert!((out[5] - 0.5).abs() < eps); // phase = 3PI/2 -> +0.5
        assert!((out[6] - 0.75).abs() < eps); // phase = 7PI/4 -> +0.75
        // Rising ramp within a cycle.
        assert!(out[0] < out[1] && out[1] < out[5] && out[5] < out[6]);
    }
}
