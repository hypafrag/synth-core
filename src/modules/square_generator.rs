//! `square_generator` — phase-accumulating square wave (50% duty). Inputs: frequency (Hz),
//! amplitude.

use std::f32::consts::{PI, TAU};

use crate::model::Params;
use crate::module::{Icon, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Square;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SquareState {
    pub phase: f32,
}

impl ModuleType for Square {
    type State = SquareState;
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
        0b00011111110000011111110000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000011111110000011111111000,
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
            inputs: vec![PortDesc::sample("frequency"), PortDesc::sample("amplitude")],
            outputs: vec![PortDesc::sample("out")],
        }
    }

    fn init_state(_p: &Params) -> SquareState {
        SquareState { phase: 0.0 }
    }

    fn process(state: &mut SquareState, ctx: &ModuleCtx) -> Tail {
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
                // High for the first half of the cycle, low for the second.
                *o = if phase < PI { a } else { -a };
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
    fn toggles_through_the_plan() {
        // const(freq=1), const(amp=1) feed a square at sr 8 -> phase advances PI/4 per sample.
        let cst = ModuleEntry::of::<crate::modules::const_generator::Const>();
        let sq = ModuleEntry::of::<Square>();
        let fns = &[cst.process, sq.process];
        let freq = params(&[("value", ParamValue::Float(1.0))]);
        let amp = params(&[("value", ParamValue::Float(1.0))]);
        let records = vec![
            RecordSpec { fn_index: 0, state: (cst.init_bytes)(&freq), inputs: vec![], num_outputs: 1 },
            RecordSpec { fn_index: 0, state: (cst.init_bytes)(&amp), inputs: vec![], num_outputs: 1 },
            RecordSpec {
                fn_index: 1,
                state: (sq.init_bytes)(&Params::new()),
                inputs: vec![Source::Port(0, 0), Source::Port(1, 0)],
                num_outputs: 1,
            },
        ];
        let mut plan = Plan::build(8, &records);
        plan.run(fns, 8.0, 0.0, 8);
        let out = plan.buffer_at(plan.output_offset(2, 0), 8);
        // First half of the cycle (phase < PI) is high, second half low. Samples 3 and 7 sit on the
        // PI / TAU boundaries (FP-fragile), so assert the unambiguous ones.
        assert_eq!(out[0], 1.0); // PI/4
        assert_eq!(out[1], 1.0); // PI/2
        assert_eq!(out[2], 1.0); // 3PI/4
        assert_eq!(out[4], -1.0); // 5PI/4
        assert_eq!(out[5], -1.0); // 3PI/2
        assert_eq!(out[6], -1.0); // 7PI/4
    }
}
