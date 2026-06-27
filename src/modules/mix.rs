//! `mix` — sums its inputs. A single **variadic** input `in` wired any number of times; the
//! output is the sum of all connected inputs. With nothing wired the engine feeds one zero input,
//! so the sum is silence — no fabricated default. This is the canonical fan-in node: combining
//! signals is always an explicit `mix`, never hidden summing on an ordinary port
//! (see `docs/architecture/04-signal-and-ports.md`).

use crate::model::Params;
use crate::module::{Icon, Inputs, ModuleCtx, ModuleDesc, ModuleType, PortDesc};
use crate::processing::Tail;

pub struct Mix;

impl ModuleType for Mix {
    type State = ();
    type Params = ();
    const ICON: Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000011111111111111110000000000,
        0b00000011111111111111110000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
        0b00000000000001100000000000000000,
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
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_p: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: Inputs::Variadic(PortDesc::sample("in")),
            outputs: vec![PortDesc::sample("out")],
            params: vec![],
        }
    }

    fn init_state(_p: &Params) {}

    fn process(_state: &mut (), _params: &(), ctx: &ModuleCtx) -> Tail {
        let frames = ctx.frames;
        let out = ctx.output(0);
        // Start from silence and accumulate every connected input. With nothing connected the
        // output stays silent — a variadic module never fabricates a default signal.
        out[..frames].fill(0.0);
        for port in 0..ctx.num_inputs() {
            let inp = ctx.input(port);
            out[..frames]
                .iter_mut()
                .zip(&inp[..frames])
                .for_each(|(o, &v)| *o += v);
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
    fn mix_sums_any_count() {
        let mix = ModuleEntry::of::<Mix>();
        assert_eq!(run_one(&mix, &Params::new(), &[1.0, 2.0, 3.0], 4), vec![6.0; 4]);
        assert_eq!(run_one(&mix, &Params::new(), &[7.0], 4), vec![7.0; 4]);
        // No inputs connected: silence.
        assert_eq!(run_one(&mix, &Params::new(), &[], 4), vec![0.0; 4]);
    }

    #[test]
    fn mix_declares_one_variadic_input() {
        assert!(matches!(Mix::describe(&Params::new()).inputs, Inputs::Variadic(_)));
    }
}
