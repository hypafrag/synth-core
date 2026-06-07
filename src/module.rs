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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignalKind {
    Sample,
    Event,
}

pub struct PortDesc {
    pub name: String,
    pub kind: SignalKind,
}

impl PortDesc {
    pub fn sample(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: SignalKind::Sample,
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
}

impl ModuleEntry {
    pub fn of<M: ModuleType>() -> Self {
        Self {
            describe: M::describe,
            init_bytes: erased_init_bytes::<M>,
            process: erased_process::<M>,
            state_size: std::mem::size_of::<M::State>(),
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

/// The static description + constructor of a voice-source type (the source analogue of
/// [`ModuleType`]).
pub trait SourceType {
    type Module: PolyphonicModule + 'static;
    /// Output ports (e.g. pitch/gate/velocity); a source has no inputs.
    fn describe(params: &Params) -> ModuleDesc;
    fn make(params: &Params) -> Self::Module;
}

/// A voice-source type erased to function pointers.
pub struct SourceEntry {
    pub describe: fn(&Params) -> ModuleDesc,
    pub make: fn(&Params) -> Box<dyn PolyphonicModule>,
}

impl SourceEntry {
    pub fn of<S: SourceType>() -> Self {
        Self {
            describe: S::describe,
            make: |p| Box::new(S::make(p)),
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

    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register::<builtins::Const>("const_generator");
        r.register::<builtins::Sine>("sine_generator");
        r.register::<builtins::Range>("range");
        r.register::<builtins::Mul>("mul");
        r.register::<builtins::Adsr>("adsr_envelope");
        r.register_source::<crate::midi_keyboard::MidiKeyboardType>("midi_keyboard");
        r
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Built-in module types (behavior/state form).
pub mod builtins {
    use super::*;
    use std::f32::consts::TAU;

    /// `const_generator` — emits a constant. Param `value`.
    pub struct Const;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ConstState {
        pub value: f32,
    }
    impl ModuleType for Const {
        type State = ConstState;
        fn describe(_p: &Params) -> ModuleDesc {
            ModuleDesc {
                inputs: vec![],
                outputs: vec![PortDesc::sample("out")],
            }
        }
        fn init_state(p: &Params) -> ConstState {
            let value = p.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            ConstState { value }
        }
        fn process(state: &mut ConstState, ctx: &ModuleCtx) -> Tail {
            ctx.output(0)[..ctx.frames].fill(state.value);
            Tail::Done
        }
    }

    /// `sine_generator` — phase-accumulating sine. Inputs: frequency (Hz), amplitude.
    pub struct Sine;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SineState {
        pub phase: f32,
    }
    impl ModuleType for Sine {
        type State = SineState;
        fn describe(_p: &Params) -> ModuleDesc {
            ModuleDesc {
                inputs: vec![PortDesc::sample("frequency"), PortDesc::sample("amplitude")],
                outputs: vec![PortDesc::sample("out")],
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

    /// `mul` — multiplies two signals (a VCA when one input is an envelope). Inputs: a, b.
    pub struct Mul;
    impl ModuleType for Mul {
        type State = ();
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

    /// `adsr_envelope` — gate-driven linear ADSR. Inputs: gate, attack(s), decay(s),
    /// sustain(level), release(s). Output: envelope level in `[0, 1]`. Reports `Tail::Active`
    /// until the release completes, so it keeps a released voice alive through its tail.
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
        fn describe(_p: &Params) -> ModuleDesc {
            ModuleDesc {
                inputs: vec![
                    PortDesc::sample("gate"),
                    PortDesc::sample("attack"),
                    PortDesc::sample("decay"),
                    PortDesc::sample("sustain"),
                    PortDesc::sample("release"),
                ],
                outputs: vec![PortDesc::sample("out")],
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
        fn process(s: &mut AdsrState, ctx: &ModuleCtx) -> Tail {
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

    /// `range` — maps bipolar `[-1, 1]` to `[low, high]`. Inputs: in, low, high.
    pub struct Range;
    impl ModuleType for Range {
        type State = ();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParamValue, Params};
    use crate::plan::{Plan, RecordSpec, Source};

    fn params(pairs: &[(&str, ParamValue)]) -> Params {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn descriptors() {
        let p = Params::new();
        assert_eq!(builtins::Sine::describe(&p).inputs.len(), 2);
        assert_eq!(builtins::Sine::describe(&p).inputs[0].name, "frequency");
        assert_eq!(builtins::Range::describe(&p).inputs.len(), 3);
    }

    /// Build a plan of constant-fed inputs into one module and return its output.
    fn run_one(entry: &ModuleEntry, params: &Params, inputs: &[f32], frames: usize) -> Vec<f32> {
        let cst = ModuleEntry::of::<builtins::Const>();
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

    fn params_value(v: f32) -> Params {
        params(&[("value", ParamValue::Float(v as f64))])
    }

    #[test]
    fn mul_multiplies() {
        let mul = ModuleEntry::of::<builtins::Mul>();
        let out = run_one(&mul, &Params::new(), &[3.0, 4.0], 4);
        assert_eq!(out, vec![12.0; 4]);
    }

    #[test]
    fn adsr_instant_attack_decays_to_sustain() {
        // attack=0, decay=0, sustain=0.5, gate held -> sustain level every sample.
        let adsr = ModuleEntry::of::<builtins::Adsr>();
        let out = run_one(&adsr, &Params::new(), &[1.0, 0.0, 0.0, 0.5, 0.0], 4);
        assert_eq!(out, vec![0.5; 4]);
    }

    #[test]
    fn const_into_sine_through_the_plan() {
        // const(freq=1) and const(amp=1) feed a sine, at sr 8 -> sin advances TAU/8 per sample.
        let cst = ModuleEntry::of::<builtins::Const>();
        let sine = ModuleEntry::of::<builtins::Sine>();
        let fns: &[ProcessFn] = &[cst.process, sine.process];

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
