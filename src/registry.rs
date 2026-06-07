//! Module registry: maps a patch's `type` string to a factory that builds a module.
//!
//! All modules are registered statically (see `docs/architecture/09-module-contract.md`).

use std::collections::HashMap;

use crate::model::Params;
use crate::modules::{AudioOutput, ConstGenerator, Range, SineGenerator};
use crate::processing::Module;

type Factory = Box<dyn Fn(&Params) -> Result<Box<dyn Module>, String> + Send + Sync>;

/// Maps module type ids to factories.
pub struct Registry {
    factories: HashMap<String, Factory>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    pub fn register(&mut self, type_id: &str, factory: Factory) {
        self.factories.insert(type_id.to_string(), factory);
    }

    /// Build a module of `type_id` from `params`.
    pub fn create(&self, type_id: &str, params: &Params) -> Result<Box<dyn Module>, String> {
        match self.factories.get(type_id) {
            Some(factory) => factory(params),
            None => Err(format!("unknown module type '{type_id}'")),
        }
    }

    /// A registry with all built-in modules.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(
            "const_generator",
            Box::new(|p| Ok(Box::new(ConstGenerator::from_params(p)?))),
        );
        r.register(
            "sine_generator",
            Box::new(|_p| Ok(Box::new(SineGenerator::new()))),
        );
        r.register("range", Box::new(|_p| Ok(Box::new(Range::new()))));
        r.register(
            "audio_output",
            Box::new(|p| Ok(Box::new(AudioOutput::from_params(p)?))),
        );
        r
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_builtins()
    }
}
