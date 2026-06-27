//! `sine_generator` — phase-accumulating sine. Inputs: frequency (Hz), amplitude.

use std::f32::consts::TAU;

use crate::model::Params;
use crate::module::{Icon, Inputs, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Sine;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SineState {
    pub phase: f32,
}

impl ModuleType for Sine {
    type State = SineState;
    const ICON: Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000111000000000000000000000,
        0b00000000111100000000000000000000,
        0b00000001000110000000000000000000,
        0b00000011000010000000000000000000,
        0b00000010000001000000000000000000,
        0b00000100000001000000000000000000,
        0b00000100000000100000000000000000,
        0b00000000000000100000000000000000,
        0b00001000000000000000000000000000,
        0b00001000000000010000000000000000,
        0b00010000000000010000000000001000,
        0b00010000000000001000000000001000,
        0b00000000000000001000000000010000,
        0b00000000000000000000000000010000,
        0b00000000000000000100000000000000,
        0b00000000000000000100000000100000,
        0b00000000000000000010000000100000,
        0b00000000000000000010000001000000,
        0b00000000000000000001000011000000,
        0b00000000000000000001100010000000,
        0b00000000000000000000111100000000,
        0b00000000000000000000011100000000,
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

    fn init_state(_p: &Params) -> SineState {
        SineState { phase: 0.0 }
    }

    fn process(state: &mut SineState, ctx: &ModuleCtx) -> Tail {
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
                *o = phase.sin() * a;
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
    fn describes_two_inputs() {
        let p = Params::new();
        assert_eq!(Sine::describe(&p).inputs.fixed().len(), 2);
        assert_eq!(Sine::describe(&p).inputs.fixed()[0].name, "frequency");
    }

    #[test]
    fn const_into_sine_through_the_plan() {
        // const(freq=1) and const(amp=1) feed a sine, at sr 8 -> sin advances TAU/8 per sample.
        let cst = ModuleEntry::of::<crate::modules::const_generator::Const>();
        let sine = ModuleEntry::of::<Sine>();
        let fns = &[cst.process, sine.process];

        let freq = params(&[("value", ParamValue::Float(1.0))]);
        let amp = params(&[("value", ParamValue::Float(1.0))]);

        let records = vec![
            RecordSpec {
                fn_index: 0,
                state: (cst.init_bytes)(&freq),
                inputs: vec![],
                num_outputs: 1,
            },
            RecordSpec {
                fn_index: 0,
                state: (cst.init_bytes)(&amp),
                inputs: vec![],
                num_outputs: 1,
            },
            RecordSpec {
                fn_index: 1,
                state: (sine.init_bytes)(&Params::new()),
                inputs: vec![Source::Port(0, 0), Source::Port(1, 0)],
                num_outputs: 1,
            },
        ];
        let mut plan = Plan::build(8, &records);
        plan.run(fns, 8.0, 0.0, 8);

        let out = plan.buffer_at(plan.output_offset(2, 0), 8);
        let eps = 1e-5;
        assert!((out[1] - 1.0).abs() < eps); // sin(pi/2)
        assert!(out[3].abs() < eps); // sin(pi)
        assert!((out[5] + 1.0).abs() < eps); // sin(3pi/2)
    }
}
