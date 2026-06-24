//! `range` — maps bipolar `[-1, 1]` to `[low, high]`. Inputs: in, low, high.

use crate::model::Params;
use crate::module::{Icon, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Range;

impl ModuleType for Range {
    type State = ();
    const ICON: Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000111111111111111110000000,
        0b00000000111111111111111110000000,
        0b00000000000000001100000000000000,
        0b00000000000000011110000000000000,
        0b00000000000000111110000000000000,
        0b00000000000001111111000000000000,
        0b00000000000001111011000000000000,
        0b00000000000000011000000000000000,
        0b00000000000000011000000000000000,
        0b00000000000000011000000000000000,
        0b00000000000000011000000000000000,
        0b00000000000000011000000000000000,
        0b00000000000000011000000000000000,
        0b00000000000000011000000000000000,
        0b00000000000001111011000000000000,
        0b00000000000001111111000000000000,
        0b00000000000000111110000000000000,
        0b00000000000000011110000000000000,
        0b00000000111111111111111110000000,
        0b00000000111111111111111110000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_p: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: vec![
                PortDesc::sample("in"),
                PortDesc::sample("low"),
                PortDesc::sample("high"),
            ],
            outputs: vec![PortDesc::sample("out")],
        }
    }

    fn init_state(_p: &Params) {}

    fn process(_state: &mut (), ctx: &ModuleCtx) -> Tail {
        let frames = ctx.frames;
        let input = ctx.input(0);
        let low = ctx.input(1);
        let high = ctx.input(2);
        let out = ctx.output(0);
        out[..frames]
            .iter_mut()
            .zip(&input[..frames])
            .zip(&low[..frames])
            .zip(&high[..frames])
            .for_each(|(((o, &x), &lo), &hi)| {
                *o = lo + (x + 1.0) * 0.5 * (hi - lo);
            });
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Params;

    #[test]
    fn describes_three_inputs() {
        let p = Params::new();
        assert_eq!(Range::describe(&p).inputs.len(), 3);
    }
}
