//! The behavior/state module model (see `docs/architecture/10-module-contract.md`).
//!
//! A module type bundles a **descriptor** (ports), a POD **state** (one per voice, living in
//! the plan buffer), and a stateless **process** function. Each type is *erased* into a
//! [`ModuleEntry`] (a `ProcessFn` for the plan's function table + an `init_state` byte-packer),
//! so there are no trait objects in the hot path.

use std::collections::HashMap;

use crate::model::{ParamValue, Params};
use crate::plan::{self, ProcessFn, Record, TickCtx, VoicedPlan};
use crate::processing::Tail;

/// A 32×32 monochrome module icon. Each of the 32 entries is one row, top to bottom; within a row
/// bit `(31 - x)` is pixel column `x` (so a `0b…` literal reads left-to-right as the picture).
/// `1` = foreground, `0` = background. Pure data — no rendering dependency — so it stays
/// headless-safe; the UI rasterizes it (see `docs/architecture/13-ui-module-api.md`).
pub type Icon = [u32; 32];

/// An all-background icon — the default when a module defines none.
pub const BLANK_ICON: Icon = [0; 32];

/// A port: a named connection point. All ports carry the one unified channel type — a continuous
/// stream of float samples — so there is no per-port "kind" to match (see
/// `docs/architecture/04-signal-and-ports.md`).
pub struct PortDesc {
    pub name: String,
}

impl PortDesc {
    pub fn sample(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

/// A module's inputs: either an exact named list, or a single port that repeats.
///
/// A [`Inputs::Variadic`] module declares one input port that the patch may wire any number of
/// times (by the same name); each wire materializes one input, and only connected inputs reach
/// the engine (no padding). Its `process` **must** be order-independent (commutative and
/// associative) over its inputs — the engine is free to materialize and compact them in any order
/// (see `docs/architecture/10-module-contract.md`).
pub enum Inputs {
    Fixed(Vec<PortDesc>),
    Variadic(PortDesc),
}

impl Inputs {
    /// The fixed port list, or `&[]` for a variadic module (whose count comes from the wiring,
    /// not the descriptor). Used by the UI/compiler to enumerate a node's *declared* inputs.
    pub fn fixed(&self) -> &[PortDesc] {
        match self {
            Inputs::Fixed(ports) => ports,
            Inputs::Variadic(_) => &[],
        }
    }
}

/// The editable kind of a [`ParamDesc`] — what widget the generic node editor builds for it
/// (`docs/architecture/13-ui-module-api.md`). Options are owned (not `&'static`) so a module may
/// compute them at `describe` time, e.g. enumerated audio devices.
pub enum ParamKind {
    /// A continuous number, optionally bounded (a numeric field / drag value).
    Float { min: f32, max: f32 },
    /// A whole number, optionally bounded.
    Int { min: i64, max: i64 },
    /// A toggle.
    Bool,
    /// One of a fixed set of string options (a dropdown). The stored value is the chosen string.
    Choice(Vec<String>),
}

/// A module's non-signal configuration parameter, as the UI presents it: a stable `name` (the key
/// in [`crate::model::Node::params`]), a display `label`, its editable `kind`, and the `default`
/// shown when the node has no stored value. The UI builds an editor generically from this — no
/// per-module UI code (`docs/architecture/13-ui-module-api.md`).
pub struct ParamDesc {
    pub name: String,
    pub label: String,
    pub kind: ParamKind,
    pub default: ParamValue,
}

/// A module's typed param block. Implemented by `#[derive(SynthModuleParams)]` from a plain struct
/// whose fields carry `#[param(...)]` attributes, so the module never hand-writes its descriptor
/// list or its YAML-dict → struct conversion — the struct is the single source of truth (field
/// name = param name, type = `ParamKind`, `default` attribute = both the descriptor default and
/// the conversion fallback). The paramless block `()` has a blanket impl below.
pub trait SynthModuleParams: Copy + Default {
    /// The param descriptors for the generic node editor (`docs/architecture/13-ui-module-api.md`).
    fn param_descs() -> Vec<ParamDesc>;
    /// Build the typed block from the patch's stored values, filling absent keys with the declared
    /// defaults. Runs off the audio thread (at plan build).
    fn from_values(values: &Params) -> Self;
}

impl SynthModuleParams for () {
    fn param_descs() -> Vec<ParamDesc> {
        Vec::new()
    }
    fn from_values(_values: &Params) -> Self {}
}

/// Generic coercion from a dynamic [`ParamValue`] to a concrete param field type. Used by the
/// generated `from_values`; keeps the dict-parsing rules in one place rather than in module code.
pub trait FromParamValue: Sized {
    fn from_param_value(v: &ParamValue) -> Option<Self>;
}

impl FromParamValue for f32 {
    fn from_param_value(v: &ParamValue) -> Option<f32> {
        v.as_f64().map(|x| x as f32)
    }
}
impl FromParamValue for f64 {
    fn from_param_value(v: &ParamValue) -> Option<f64> {
        v.as_f64()
    }
}
impl FromParamValue for i64 {
    fn from_param_value(v: &ParamValue) -> Option<i64> {
        v.as_i64()
    }
}
impl FromParamValue for bool {
    fn from_param_value(v: &ParamValue) -> Option<bool> {
        v.as_bool()
    }
}

/// `#[derive(SynthModuleParams)]` for module param structs (re-exported so module crates write a
/// single `use synth_core::module::SynthModuleParams;` for both the trait and the derive).
pub use synth_macros::SynthModuleParams;

/// A module type's ports and editable params, given its current params (both may depend on
/// structural/param values — e.g. enumerated device options).
pub struct ModuleDesc {
    pub inputs: Inputs,
    pub outputs: Vec<PortDesc>,
    pub params: Vec<ParamDesc>,
}

/// Per-tick access a module's `process` gets: timing + its input/output buffers.
///
/// `input` and `output` hand out slices into the plan buffer; a module must not read and write
/// the same buffer (inputs and outputs are always distinct records in a topological plan).
pub struct ModuleCtx<'p> {
    pub frames: usize,
    pub sample_rate: f32,
    pub time: f64,
    base: *mut u8,
    rec: &'p Record,
    block_size: usize,
}

impl<'p> ModuleCtx<'p> {
    pub fn input(&self, port: usize) -> &'p [f32] {
        unsafe { plan::input(self.base, self.rec, port, self.frames) }
    }

    /// Number of materialized inputs for this record. For a variadic module this is the count of
    /// connected wires; iterate `0..ctx.num_inputs()` rather than hard-coding port indices.
    pub fn num_inputs(&self) -> usize {
        self.rec.num_inputs as usize
    }

    pub fn output(&self, port: usize) -> &'p mut [f32] {
        unsafe { plan::output(self.base, self.rec, port, self.block_size, self.frames) }
    }
}

/// A module: a descriptor, a POD per-voice state, a read-only param block, and stateless behavior.
pub trait ModuleType {
    type State: Copy;

    /// The module's read-only configuration, packed into the plan's params section (one block per
    /// module, shared by its voices) and handed to `process`. `()` for modules with no params.
    /// Normally a struct with `#[derive(SynthModuleParams)]`.
    type Params: SynthModuleParams;

    /// The module's 32×32 icon (defaults to blank).
    const ICON: Icon = BLANK_ICON;

    fn describe(params: &Params) -> ModuleDesc;
    fn init_state(params: &Params) -> Self::State;

    /// Build the typed param block from the patch's stored values (off the audio thread). The
    /// default delegates to the derived/blanket conversion — modules need not override it.
    fn init_params(params: &Params) -> Self::Params {
        Self::Params::from_values(params)
    }

    fn process(state: &mut Self::State, params: &Self::Params, ctx: &ModuleCtx) -> Tail;
}

/// Type-erased process for the plan's function table.
unsafe fn erased_process<M: ModuleType>(base: *mut u8, tick: &TickCtx, rec: &Record) -> Tail {
    let state = unsafe { &mut *(base.add(rec.state as usize) as *mut M::State) };
    let params = unsafe { &*(base.add(rec.params as usize) as *const M::Params) };
    let ctx = ModuleCtx {
        frames: tick.frames,
        sample_rate: tick.sample_rate,
        time: tick.time,
        base,
        rec,
        block_size: tick.block_size,
    };
    M::process(state, params, &ctx)
}

fn erased_init_bytes<M: ModuleType>(params: &Params) -> Vec<u8> {
    plan::state_bytes(M::init_state(params))
}

fn erased_init_params<M: ModuleType>(params: &Params) -> Vec<u8> {
    plan::state_bytes(M::init_params(params))
}

/// One module type erased to plain function pointers (no generics, no trait objects).
pub struct ModuleEntry {
    pub describe: fn(&Params) -> ModuleDesc,
    pub init_bytes: fn(&Params) -> Vec<u8>,
    pub init_params: fn(&Params) -> Vec<u8>,
    pub process: ProcessFn,
    pub state_size: usize,
    pub params_size: usize,
    pub icon: Icon,
}

impl ModuleEntry {
    pub fn of<M: ModuleType>() -> Self {
        Self {
            describe: M::describe,
            init_bytes: erased_init_bytes::<M>,
            init_params: erased_init_params::<M>,
            process: erased_process::<M>,
            state_size: std::mem::size_of::<M::State>(),
            params_size: std::mem::size_of::<M::Params>(),
            icon: M::ICON,
        }
    }
}

// ---------------------------------------------------------------------------
// Voice sources (polyphonic modules)
// ---------------------------------------------------------------------------

/// A voice handle = a plan slot index (opaque to modules).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VoiceId(pub usize);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Free,
    Held,
    Released,
}

/// Engine-side voice bookkeeping: which slots are free / held / released. Pure bookkeeping —
/// it never touches the plan buffer (the engine reflects allocations into the plan).
pub struct SlotManager {
    state: Vec<SlotState>,
    free: Vec<usize>,
}

impl SlotManager {
    pub fn new(max_voices: usize) -> Self {
        Self {
            state: vec![SlotState::Free; max_voices],
            free: (0..max_voices).rev().collect(),
        }
    }

    fn alloc(&mut self) -> Option<usize> {
        let slot = self.free.pop()?;
        self.state[slot] = SlotState::Held;
        Some(slot)
    }

    fn release(&mut self, slot: usize) {
        if self.state[slot] == SlotState::Held {
            self.state[slot] = SlotState::Released;
        }
    }

    /// Whether slot `slot` currently holds a voice (so the engine sums it at the output).
    pub fn is_active(&self, slot: usize) -> bool {
        self.state[slot] != SlotState::Free
    }

    /// Free released slots whose voice records all reported `Tail::Done` (`!alive`). Returns the
    /// freed slots so the engine can disable them in the plan.
    pub fn reap(&mut self, alive: &[bool]) -> Vec<usize> {
        let mut freed = Vec::new();
        for slot in 0..self.state.len() {
            if self.state[slot] == SlotState::Released && !alive[slot] {
                self.state[slot] = SlotState::Free;
                self.free.push(slot);
                freed.push(slot);
            }
        }
        freed
    }
}

/// What a [`PolyphonicModule`]'s `process` gets each block: timing, voice allocation, and
/// per-voice source-output buffers. The source drives allocation; the engine owns the slots.
pub struct SourceCtx<'a> {
    pub frames: usize,
    pub sample_rate: f32,
    pub time: f64,
    plan: &'a mut VoicedPlan,
    slots: &'a mut SlotManager,
    /// Fresh initial state for each voice record (re-stamped on spawn).
    template_states: &'a [Vec<u8>],
}

impl<'a> SourceCtx<'a> {
    pub fn new(
        frames: usize,
        sample_rate: f32,
        time: f64,
        plan: &'a mut VoicedPlan,
        slots: &'a mut SlotManager,
        template_states: &'a [Vec<u8>],
    ) -> Self {
        Self {
            frames,
            sample_rate,
            time,
            plan,
            slots,
            template_states,
        }
    }

    /// Claim a free voice: re-inits its slot state, clears its source outputs, enables it.
    /// `None` when every slot is in use (the engine grows the plan off-RT; retry next block).
    pub fn allocate(&mut self) -> Option<VoiceId> {
        let slot = self.slots.alloc()?;
        for (rec, state) in self.template_states.iter().enumerate() {
            self.plan.set_voice_state(slot, rec, state);
        }
        self.plan.clear_source_outputs(slot, self.frames);
        self.plan.set_slot_active(slot, true);
        Some(VoiceId(slot))
    }

    /// Mark a voice released (note-off). The engine frees the slot once its records are `Done`.
    pub fn release(&mut self, voice: VoiceId) {
        self.slots.release(voice.0);
    }

    /// A voice's source-output buffer for `port` (e.g. pitch/gate/velocity), to write into.
    pub fn voice_output(&mut self, voice: VoiceId, port: usize) -> &mut [f32] {
        self.plan.source_output_mut(voice.0, port, self.frames)
    }
}

/// A running voice source instance (e.g. a MIDI keyboard): a single shared object holding
/// resources, called once per block. Constructed off the audio thread via [`SourceType`].
pub trait PolyphonicModule: Send {
    fn process(&mut self, ctx: &mut SourceCtx) -> Tail;
}

/// An OS-level permission a module may require.  The host (CLI, UI) resolves this to a
/// human-readable message appropriate for its context — the core never produces user strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsPermission {
    /// macOS Accessibility (needed for global key-state polling via `device_query`).
    Accessibility,
}

/// Error returned by [`SourceType::make`].  Carries structured facts; no user-facing strings.
#[derive(Debug)]
pub enum SourceError {
    PermissionDenied(OsPermission),
    Other(String),
}

/// The static description + constructor of a voice-source type (the source analogue of
/// [`ModuleType`]).
pub trait SourceType {
    type Module: PolyphonicModule + 'static;
    /// The source's 32×32 icon (defaults to blank).
    const ICON: Icon = BLANK_ICON;
    /// Output ports (e.g. pitch/gate/velocity); a source has no inputs.
    fn describe(params: &Params) -> ModuleDesc;
    /// Construct the module, or return a structured [`SourceError`].  The engine surfaces this
    /// as a typed [`crate::plan_engine::EngineError`] variant; each host maps it to the
    /// appropriate user-facing message.
    fn make(params: &Params) -> Result<Self::Module, SourceError>;
}

/// A voice-source type erased to function pointers.
pub struct SourceEntry {
    pub describe: fn(&Params) -> ModuleDesc,
    pub make: fn(&Params) -> Result<Box<dyn PolyphonicModule>, SourceError>,
    pub icon: Icon,
}

impl SourceEntry {
    pub fn of<S: SourceType>() -> Self {
        Self {
            describe: S::describe,
            make: |p| S::make(p).map(|m| Box::new(m) as Box<dyn PolyphonicModule>),
            icon: S::ICON,
        }
    }
}

/// Maps a patch's `type` id to its [`ModuleEntry`] (build-time only).
pub struct Registry {
    entries: HashMap<String, ModuleEntry>,
    sources: HashMap<String, SourceEntry>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            sources: HashMap::new(),
        }
    }

    pub fn register<M: ModuleType>(&mut self, type_id: &str) {
        self.entries.insert(type_id.to_string(), ModuleEntry::of::<M>());
    }

    pub fn register_source<S: SourceType>(&mut self, type_id: &str) {
        self.sources.insert(type_id.to_string(), SourceEntry::of::<S>());
    }

    pub fn get(&self, type_id: &str) -> Option<&ModuleEntry> {
        self.entries.get(type_id)
    }

    pub fn source(&self, type_id: &str) -> Option<&SourceEntry> {
        self.sources.get(type_id)
    }

    pub fn is_source(&self, type_id: &str) -> bool {
        self.sources.contains_key(type_id)
    }

    /// Type ids of all registered processing modules (not voice sources). Order is unspecified
    /// (`HashMap` iteration); callers that want a stable list should sort. Used to populate the
    /// UI module palette.
    pub fn module_type_ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Type ids of all registered voice sources (polyphonic modules). See `module_type_ids`.
    pub fn source_type_ids(&self) -> impl Iterator<Item = &str> {
        self.sources.keys().map(String::as_str)
    }

    /// The icon for a module or source type, if registered.
    pub fn icon(&self, type_id: &str) -> Option<Icon> {
        self.entries
            .get(type_id)
            .map(|e| e.icon)
            .or_else(|| self.sources.get(type_id).map(|s| s.icon))
    }

    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register::<crate::modules::const_generator::Const>("const_generator");
        r.register::<crate::modules::sine_generator::Sine>("sine_generator");
        r.register::<crate::modules::sawtooth_generator::Sawtooth>("sawtooth_generator");
        r.register::<crate::modules::square_generator::Square>("square_generator");
        r.register::<crate::modules::range::Range>("range");
        r.register::<crate::modules::mul::Mul>("mul");
        r.register::<crate::modules::mix::Mix>("mix");
        r.register::<crate::modules::adsr_envelope::Adsr>("adsr_envelope");
        r.register_source::<crate::modules::ansi_keyboard::AnsiKeyboardType>("ansi_keyboard");
        r.register_source::<crate::modules::midi_keyboard::MidiKeyboardType>("midi_keyboard");
        r
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Test-only harness shared by built-in modules' unit tests: build a tiny plan that feeds
/// constant inputs into one module under test and read back its output.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::model::ParamValue;
    use crate::plan::{Plan, RecordSpec, Source};

    pub(crate) fn params(pairs: &[(&str, ParamValue)]) -> Params {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    pub(crate) fn params_value(v: f32) -> Params {
        params(&[("value", ParamValue::Float(v as f64))])
    }

    /// Build a plan of constant-fed inputs into one module and return its output.
    pub(crate) fn run_one(entry: &ModuleEntry, params: &Params, inputs: &[f32], frames: usize) -> Vec<f32> {
        let cst = ModuleEntry::of::<crate::modules::const_generator::Const>();
        let mut fns: Vec<ProcessFn> = vec![cst.process];
        let mut records: Vec<RecordSpec> = Vec::new();
        let mut srcs = Vec::new();
        for (i, &v) in inputs.iter().enumerate() {
            records.push(RecordSpec {
                fn_index: 0,
                state: (cst.init_bytes)(&params_value(v)),
                params: (cst.init_params)(&params_value(v)),
                inputs: vec![],
                num_outputs: 1,
            });
            srcs.push(Source::Port(i, 0));
        }
        fns.push(entry.process);
        records.push(RecordSpec {
            fn_index: 1,
            state: (entry.init_bytes)(params),
            params: (entry.init_params)(params),
            inputs: srcs,
            num_outputs: 1,
        });
        let target = records.len() - 1;
        let mut plan = Plan::build(frames, &records);
        plan.run(&fns, 100.0, 0.0, frames);
        plan.buffer_at(plan.output_offset(target, 0), frames).to_vec()
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn enumerates_builtin_modules_and_sources() {
        let r = Registry::with_builtins();
        let modules: Vec<&str> = r.module_type_ids().collect();
        let sources: Vec<&str> = r.source_type_ids().collect();

        // Processing modules show up in module_type_ids, not source_type_ids.
        assert!(modules.contains(&"sine_generator"));
        assert!(modules.contains(&"mul"));
        assert!(!modules.contains(&"midi_keyboard"));

        // Voice sources show up in source_type_ids only.
        assert!(sources.contains(&"midi_keyboard"));
        assert!(!sources.contains(&"sine_generator"));
    }
}

