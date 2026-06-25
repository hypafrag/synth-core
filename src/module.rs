//! The behavior/state module model (see `docs/architecture/10-module-contract.md`).
//!
//! A module type bundles a **descriptor** (ports), a POD **state** (one per voice, living in
//! the plan buffer), and a stateless **process** function. Each type is *erased* into a
//! [`ModuleEntry`] (a `ProcessFn` for the plan's function table + an `init_state` byte-packer),
//! so there are no trait objects in the hot path.

use std::collections::HashMap;

use crate::model::Params;
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

/// A module type's ports, given its params (ports may depend on structural params).
pub struct ModuleDesc {
    pub inputs: Vec<PortDesc>,
    pub outputs: Vec<PortDesc>,
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

    pub fn output(&self, port: usize) -> &'p mut [f32] {
        unsafe { plan::output(self.base, self.rec, port, self.block_size, self.frames) }
    }
}

/// A module: a descriptor, a POD per-voice state, and stateless behavior.
pub trait ModuleType {
    type State: Copy;

    /// The module's 32×32 icon (defaults to blank).
    const ICON: Icon = BLANK_ICON;

    fn describe(params: &Params) -> ModuleDesc;
    fn init_state(params: &Params) -> Self::State;
    fn process(state: &mut Self::State, ctx: &ModuleCtx) -> Tail;
}

/// Type-erased process for the plan's function table.
unsafe fn erased_process<M: ModuleType>(base: *mut u8, tick: &TickCtx, rec: &Record) -> Tail {
    let state = unsafe { &mut *(base.add(rec.state as usize) as *mut M::State) };
    let ctx = ModuleCtx {
        frames: tick.frames,
        sample_rate: tick.sample_rate,
        time: tick.time,
        base,
        rec,
        block_size: tick.block_size,
    };
    M::process(state, &ctx)
}

fn erased_init_bytes<M: ModuleType>(params: &Params) -> Vec<u8> {
    plan::state_bytes(M::init_state(params))
}

/// One module type erased to plain function pointers (no generics, no trait objects).
pub struct ModuleEntry {
    pub describe: fn(&Params) -> ModuleDesc,
    pub init_bytes: fn(&Params) -> Vec<u8>,
    pub process: ProcessFn,
    pub state_size: usize,
    pub icon: Icon,
}

impl ModuleEntry {
    pub fn of<M: ModuleType>() -> Self {
        Self {
            describe: M::describe,
            init_bytes: erased_init_bytes::<M>,
            process: erased_process::<M>,
            state_size: std::mem::size_of::<M::State>(),
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
                inputs: vec![],
                num_outputs: 1,
            });
            srcs.push(Source::Port(i, 0));
        }
        fns.push(entry.process);
        records.push(RecordSpec {
            fn_index: 1,
            state: (entry.init_bytes)(params),
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

