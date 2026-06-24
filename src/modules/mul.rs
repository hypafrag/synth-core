//! `mul` — multiplies two signals (a VCA when one input is an envelope). Inputs: a, b.

use crate::model::Params;
use crate::module::{Icon, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Mul;

impl ModuleType for Mul {
    type State = ();
    const ICON: Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000001100000000000000011000000,
        0b00000001110000000000000111000000,
        0b00000000111000000000001110000000,
        0b00000000011100000000011100000000,
        0b00000000001110000000111000000000,
        0b00000000000111000001110000000000,
        0b00000000000011100011100000000000,
        0b00000000000001110111000000000000,
        0b00000000000000111110000000000000,
        0b00000000000000011100000000000000,
        0b00000000000000111110000000000000,
        0b00000000000001110111000000000000,
        0b00000000000011100011100000000000,
        0b00000000000111000001110000000000,
        0b00000000001110000000111000000000,
        0b00000000011100000000011100000000,
        0b00000000111000000000001110000000,
        0b00000001110000000000000111000000,
        0b00000001100000000000000011000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_p: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: vec![PortDesc::sample("a"), PortDesc::sample("b")],
            outputs: vec![PortDesc::sample("out")],
        }
    }

    fn init_state(_p: &Params) {}

    fn process(_state: &mut (), ctx: &ModuleCtx) -> Tail {
        let frames = ctx.frames;
        let a = ctx.input(0);
        let b = ctx.input(1);
        let out = ctx.output(0);
        out[..frames]
            .iter_mut()
            .zip(&a[..frames])
            .zip(&b[..frames])
            .for_each(|((o, &a), &b)| *o = a * b);
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Params;
    use crate::module::ModuleEntry;
    use crate::module::test_support::run_one;

    #[test]
    fn mul_multiplies() {
        let mul = ModuleEntry::of::<Mul>();
        let out = run_one(&mul, &Params::new(), &[3.0, 4.0], 4);
        assert_eq!(out, vec![12.0; 4]);
    }
}
