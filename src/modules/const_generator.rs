//! `const_generator` — emits a constant. Param `value`.

use crate::model::Params;
use crate::module::{Icon, Inputs, ModuleCtx, ModuleDesc, ModuleType, PortDesc, SynthModuleParams};
use crate::processing::Tail;

pub struct Const;

/// `const_generator`'s read-only config: the constant value it emits. The descriptor and the
/// YAML-dict → struct conversion are synthesized from this struct (see `SynthModuleParams`).
#[repr(C)]
#[derive(Clone, Copy, Default, SynthModuleParams)]
pub struct ConstParams {
    #[param(label = "Value", default = 0.0)]
    pub value: f32,
}

impl ModuleType for Const {
    type State = ();
    type Params = ConstParams;
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
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00001111111111111111111111110000,
        0b00001111111111111111111111110000,
        0b00001111111111111111111111110000,
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
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_p: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: Inputs::Fixed(vec![]),
            outputs: vec![PortDesc::sample("out")],
            params: ConstParams::param_descs(),
        }
    }

    fn init_state(_p: &Params) {}

    fn process(_state: &mut (), params: &ConstParams, ctx: &ModuleCtx) -> Tail {
        ctx.output(0)[..ctx.frames].fill(params.value);
        Tail::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ParamValue;

    #[test]
    fn descriptor_is_synthesized_from_the_struct() {
        let d = ConstParams::param_descs();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "value");
        assert_eq!(d[0].label, "Value");
        assert_eq!(d[0].default, ParamValue::Float(0.0));
    }

    #[test]
    fn from_values_reads_the_value_or_falls_back_to_the_declared_default() {
        let mut p = Params::new();
        p.insert("value".into(), ParamValue::Float(2.5));
        assert_eq!(ConstParams::from_values(&p).value, 2.5);
        // The single declared default (0.0) fills an absent key — no second copy of the default.
        assert_eq!(ConstParams::from_values(&Params::new()).value, 0.0);
    }
}
