//! `adsr_envelope` — gate-driven linear ADSR. Inputs: gate, attack(s), decay(s), sustain(level),
//! release(s). Output: envelope level in `[0, 1]`. Reports `Tail::Active` until the release
//! completes, so it keeps a released voice alive through its tail.

use crate::model::Params;
use crate::module::{Icon, Inputs, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Adsr;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AdsrState {
    /// 0 idle, 1 attack, 2 decay, 3 sustain, 4 release.
    pub stage: u32,
    pub level: f32,
    pub prev_gate: f32,
    /// Per-sample decrement captured at the release edge (linear from the held level).
    pub rel_rate: f32,
}

impl ModuleType for Adsr {
    type State = AdsrState;
    type Params = ();
    const ICON: Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000011000000000000000000000,
        0b00000000011100000000000000000000,
        0b00000000011100000000000000000000,
        0b00000000111110000000000000000000,
        0b00000000110110000000000000000000,
        0b00000000110111000000000000000000,
        0b00000000110011000000000000000000,
        0b00000001110011100000000000000000,
        0b00000001100001100000000000000000,
        0b00000001100001110000000000000000,
        0b00000001100000111111111000000000,
        0b00000001100000111111111100000000,
        0b00000011100000000000001100000000,
        0b00000011000000000000001110000000,
        0b00000011000000000000000110000000,
        0b00000011000000000000000111000000,
        0b00000111000000000000000011000000,
        0b00000110000000000000000011100000,
        0b00000110000000000000000001100000,
        0b00000110000000000000000001110000,
        0b00001110000000000000000000110000,
        0b00001100000000000000000000111000,
        0b00001100000000000000000000011000,
        0b00001100000000000000000000011000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_p: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: Inputs::Fixed(vec![
                PortDesc::sample("gate"),
                PortDesc::sample("attack"),
                PortDesc::sample("decay"),
                PortDesc::sample("sustain"),
                PortDesc::sample("release"),
            ]),
            outputs: vec![PortDesc::sample("out")],
            params: vec![],
        }
    }

    fn init_state(_p: &Params) -> AdsrState {
        AdsrState {
            stage: 0,
            level: 0.0,
            prev_gate: 0.0,
            rel_rate: 0.0,
        }
    }

    fn process(s: &mut AdsrState, _params: &(), ctx: &ModuleCtx) -> Tail {
        let frames = ctx.frames;
        let dt = 1.0 / ctx.sample_rate;
        let gate = ctx.input(0);
        let attack = ctx.input(1);
        let decay = ctx.input(2);
        let sustain = ctx.input(3);
        let release = ctx.input(4);
        let out = ctx.output(0);
        for i in 0..frames {
            let g = gate[i];
            // Gate edges.
            if s.prev_gate <= 0.0 && g > 0.0 {
                s.stage = 1; // attack
            } else if s.prev_gate > 0.0 && g <= 0.0 && s.stage != 0 {
                s.stage = 4; // release
                let rel = release[i].max(0.0);
                s.rel_rate = if rel > 0.0 { s.level * dt / rel } else { s.level };
            }
            s.prev_gate = g;

            let sus = sustain[i].clamp(0.0, 1.0);
            // Advance stages; zero-length stages (instant attack/decay) cascade within the
            // sample via `continue` (they consume no time), timed stages `break`.
            loop {
                match s.stage {
                    1 => {
                        let a = attack[i].max(0.0);
                        if a <= 0.0 {
                            s.level = 1.0;
                            s.stage = 2;
                            continue;
                        }
                        s.level += dt / a;
                        if s.level >= 1.0 {
                            s.level = 1.0;
                            s.stage = 2;
                        }
                        break;
                    }
                    2 => {
                        let d = decay[i].max(0.0);
                        if d <= 0.0 {
                            s.level = sus;
                            s.stage = 3;
                            continue;
                        }
                        s.level -= dt * (1.0 - sus) / d;
                        if s.level <= sus {
                            s.level = sus;
                            s.stage = 3;
                        }
                        break;
                    }
                    3 => {
                        s.level = sus;
                        break;
                    }
                    4 => {
                        s.level -= s.rel_rate;
                        if s.level <= 0.0 {
                            s.level = 0.0;
                            s.stage = 0;
                        }
                        break;
                    }
                    _ => {
                        s.level = 0.0;
                        break;
                    }
                }
            }
            out[i] = s.level;
        }
        if s.stage == 0 {
            Tail::Done
        } else {
            Tail::Active
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Params;
    use crate::module::ModuleEntry;
    use crate::module::test_support::run_one;

    #[test]
    fn adsr_instant_attack_decays_to_sustain() {
        // attack=0, decay=0, sustain=0.5, gate held -> sustain level every sample.
        let adsr = ModuleEntry::of::<Adsr>();
        let out = run_one(&adsr, &Params::new(), &[1.0, 0.0, 0.0, 0.5, 0.0], 4);
        assert_eq!(out, vec![0.5; 4]);
    }
}
