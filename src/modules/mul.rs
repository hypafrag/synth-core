//! `mul` — multiplies its inputs (a VCA when one is an envelope). A single **variadic** input
//! `in` wired any number of times; the output is the product of all connected inputs. With nothing
//! wired the engine feeds one zero input, so the product is silence — there is no fabricated
//! default (see `docs/architecture/10-module-contract.md`).

use crate::model::Params;
use crate::module::{Icon, Inputs, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
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
            inputs: Inputs::Variadic(PortDesc::sample("in")),
            outputs: vec![PortDesc::sample("out")],
        }
    }

    fn init_state(_p: &Params) {}

    fn process(_state: &mut (), ctx: &ModuleCtx) -> Tail {
        let frames = ctx.frames;
        let out = ctx.output(0);
        // The variadic `in` always materializes at least one input: when nothing is wired the
        // compiler feeds a single zero buffer (the same fallback every unconnected input gets), so
        // an unwired `mul` multiplies by 0 and falls to silence through its own math — there is no
        // special-cased default here.
        let first = ctx.input(0);
        out[..frames].copy_from_slice(&first[..frames]);
        for port in 1..ctx.num_inputs() {
            let inp = ctx.input(port);
            out[..frames]
                .iter_mut()
                .zip(&inp[..frames])
                .for_each(|(o, &v)| *o *= v);
        }
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

    #[test]
    fn mul_is_variadic() {
        let mul = ModuleEntry::of::<Mul>();
        // Any number of connected inputs multiply together. (The no-input case is a compiler
        // concern — an unwired variadic gets one zero input — covered by the engine-level test
        // `variadic_port_with_no_wires_is_silent`.)
        assert_eq!(run_one(&mul, &Params::new(), &[2.0, 3.0, 4.0], 4), vec![24.0; 4]);
        assert_eq!(run_one(&mul, &Params::new(), &[5.0], 4), vec![5.0; 4]);
    }

    #[test]
    fn mul_declares_one_variadic_input() {
        assert!(matches!(Mul::describe(&Params::new()).inputs, Inputs::Variadic(_)));
    }
}
