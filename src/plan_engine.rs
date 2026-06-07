//! Compiles a [`Patch`] into a processing plan and runs it.
//!
//! Two backends share one front end (`docs/architecture/06-processing-plan.md`,
//! `09-polyphony.md`):
//!
//! - **Mono** — no voice source: every node is a shared record; the sink sums its sources once.
//! - **Poly** — exactly one voice source (a `PolyphonicModule`): a two-traversal split puts
//!   nodes downstream of the source into per-voice slots and the rest into shared records. Each
//!   block the source emits per-voice outputs, the walk runs the active slots, and the sink sums
//!   the active slots.

use std::collections::HashMap;

use crate::model::Patch;
use crate::module::{PolyphonicModule, Registry, SlotManager, SourceCtx};
use crate::plan::{Plan, ProcessFn, RecordSpec, Source, VoiceRecordSpec, VoiceSource, VoicedPlan};

const AUDIO_OUTPUT: &str = "audio_output";
const DEFAULT_MAX_VOICES: usize = 16;

#[derive(Debug)]
pub enum EngineError {
    Build { node: String, msg: String },
    UnknownNode(String),
    UnknownPort { node: String, port: String },
    NoAudioOutput,
    MultipleSources,
    Cycle,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Build { node, msg } => write!(f, "node '{node}': {msg}"),
            EngineError::UnknownNode(n) => write!(f, "wire references unknown node '{n}'"),
            EngineError::UnknownPort { node, port } => {
                write!(f, "node '{node}' has no port '{port}'")
            }
            EngineError::NoAudioOutput => write!(f, "patch has no audio_output node"),
            EngineError::MultipleSources => {
                write!(f, "patch has more than one voice source (not yet supported)")
            }
            EngineError::Cycle => {
                write!(f, "patch graph has a cycle (feedback is not yet supported)")
            }
        }
    }
}

impl std::error::Error for EngineError {}

/// A producer feeding an output channel.
#[derive(Clone, Copy)]
enum SinkSrc {
    /// A shared record's output (summed once).
    Shared(usize, usize),
    /// A per-voice record's output (summed over the active slots).
    Voice(usize, usize),
}

struct Mono {
    plan: Plan,
    fns: Vec<ProcessFn>,
    sink: Vec<Option<(usize, usize)>>,
}

struct Poly {
    plan: VoicedPlan,
    fns: Vec<ProcessFn>,
    source: Box<dyn PolyphonicModule>,
    slots: SlotManager,
    template_states: Vec<Vec<u8>>,
    sink: Vec<Option<SinkSrc>>,
    slot_alive: Vec<bool>,
}

enum Backend {
    Mono(Mono),
    Poly(Poly),
}

/// A compiled, runnable patch built on the processing plan.
pub struct PlanEngine {
    backend: Backend,
    channels: usize,
    sample_rate: f32,
    capacity: usize,
    elapsed_frames: u64,
}

/// Wires resolved against node port names, shared by both backends.
struct Resolved {
    /// Per node, per input port: the (producer node, producer output port), or `None`.
    in_srcs: Vec<Vec<Option<(usize, usize)>>>,
    out_names: Vec<Vec<String>>,
    order: Vec<usize>,
    sink: usize,
    channels: usize,
    sample_rate: f32,
}

impl PlanEngine {
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Compile a patch into a plan. `capacity` is the maximum block size (frames).
    pub fn build(patch: &Patch, registry: &Registry, capacity: usize) -> Result<Self, EngineError> {
        let r = resolve(patch, registry, capacity)?;

        let sources: Vec<usize> = patch
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, nd)| registry.is_source(&nd.ty))
            .map(|(i, _)| i)
            .collect();
        if sources.len() > 1 {
            return Err(EngineError::MultipleSources);
        }

        let backend = match sources.first() {
            None => Backend::Mono(build_mono(patch, registry, &r, capacity)),
            Some(&source) => {
                Backend::Poly(build_poly(patch, registry, &r, source, capacity)?)
            }
        };

        Ok(PlanEngine {
            backend,
            channels: r.channels,
            sample_rate: r.sample_rate,
            capacity,
            elapsed_frames: 0,
        })
    }

    /// Process one block, writing `frames` interleaved frames into `device`
    /// (`device.len()` must be `frames * channels`).
    pub fn process_block(&mut self, device: &mut [f32], frames: usize) {
        let frames = frames.min(self.capacity);
        let time = self.elapsed_frames as f64 / self.sample_rate as f64;
        let channels = self.channels;
        let sr = self.sample_rate;

        match &mut self.backend {
            Backend::Mono(m) => {
                m.plan.run(&m.fns, sr, time, frames);
                for c in 0..channels {
                    match m.sink[c] {
                        Some((rec, port)) => {
                            let off = m.plan.output_offset(rec, port);
                            let buf = m.plan.buffer_at(off, frames);
                            for i in 0..frames {
                                device[i * channels + c] = buf[i];
                            }
                        }
                        None => {
                            for i in 0..frames {
                                device[i * channels + c] = 0.0;
                            }
                        }
                    }
                }
            }
            Backend::Poly(p) => {
                {
                    let mut ctx = SourceCtx::new(
                        frames,
                        sr,
                        time,
                        &mut p.plan,
                        &mut p.slots,
                        &p.template_states,
                    );
                    p.source.process(&mut ctx);
                }
                p.plan.run(&p.fns, sr, time, frames, &mut p.slot_alive);
                for slot in p.slots.reap(&p.slot_alive) {
                    p.plan.set_slot_active(slot, false);
                }

                let max_voices = p.plan.max_voices();
                for c in 0..channels {
                    let out_base = c;
                    for i in 0..frames {
                        device[i * channels + out_base] = 0.0;
                    }
                    match p.sink[c] {
                        Some(SinkSrc::Shared(rec, port)) => {
                            let buf = p.plan.shared_output(rec, port, frames);
                            for i in 0..frames {
                                device[i * channels + c] = buf[i];
                            }
                        }
                        Some(SinkSrc::Voice(rec, port)) => {
                            for slot in 0..max_voices {
                                if !p.slots.is_active(slot) {
                                    continue;
                                }
                                let buf = p.plan.voice_output(slot, rec, port, frames);
                                for i in 0..frames {
                                    device[i * channels + c] += buf[i];
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
        }

        self.elapsed_frames += frames as u64;
    }
}

/// Resolve port names + wires + topological order; find the sink and global config.
fn resolve(patch: &Patch, registry: &Registry, _capacity: usize) -> Result<Resolved, EngineError> {
    let n = patch.nodes.len();

    let mut index = HashMap::new();
    for (i, node) in patch.nodes.iter().enumerate() {
        index.insert(node.id.as_str(), i);
    }

    let sink = patch
        .nodes
        .iter()
        .position(|nd| nd.ty == AUDIO_OUTPUT)
        .ok_or(EngineError::NoAudioOutput)?;
    let sink_params = &patch.nodes[sink].params;
    let channels = sink_params
        .get("channels")
        .and_then(|v| v.as_i64())
        .unwrap_or(2)
        .max(1) as usize;
    let sample_rate = sink_params
        .get("sample_rate")
        .and_then(|v| v.as_f64())
        .unwrap_or(44100.0) as f32;

    let mut in_names: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut out_names: Vec<Vec<String>> = vec![Vec::new(); n];
    for (i, node) in patch.nodes.iter().enumerate() {
        if i == sink {
            in_names[i] = (0..channels).map(|c| format!("ch{c}")).collect();
            continue;
        }
        let desc = if let Some(src) = registry.source(&node.ty) {
            (src.describe)(&node.params)
        } else if let Some(entry) = registry.get(&node.ty) {
            (entry.describe)(&node.params)
        } else {
            return Err(EngineError::Build {
                node: node.id.clone(),
                msg: format!("unknown module type '{}'", node.ty),
            });
        };
        in_names[i] = desc.inputs.iter().map(|p| p.name.clone()).collect();
        out_names[i] = desc.outputs.iter().map(|p| p.name.clone()).collect();
    }

    let mut in_srcs: Vec<Vec<Option<(usize, usize)>>> =
        (0..n).map(|i| vec![None; in_names[i].len()]).collect();
    let mut edges = Vec::new();
    for w in &patch.wires {
        let fi = *index
            .get(w.from.node())
            .ok_or_else(|| EngineError::UnknownNode(w.from.node().to_string()))?;
        let ti = *index
            .get(w.to.node())
            .ok_or_else(|| EngineError::UnknownNode(w.to.node().to_string()))?;
        let fp = out_names[fi]
            .iter()
            .position(|p| p == w.from.port())
            .ok_or_else(|| EngineError::UnknownPort {
                node: w.from.node().to_string(),
                port: w.from.port().to_string(),
            })?;
        let tp = in_names[ti]
            .iter()
            .position(|p| p == w.to.port())
            .ok_or_else(|| EngineError::UnknownPort {
                node: w.to.node().to_string(),
                port: w.to.port().to_string(),
            })?;
        in_srcs[ti][tp] = Some((fi, fp));
        edges.push((fi, ti));
    }

    let order = topo_sort(n, &edges)?;

    Ok(Resolved {
        in_srcs,
        out_names,
        order,
        sink,
        channels,
        sample_rate,
    })
}

fn build_mono(patch: &Patch, registry: &Registry, r: &Resolved, capacity: usize) -> Mono {
    let n = patch.nodes.len();
    let mut fns: Vec<ProcessFn> = Vec::new();
    let mut type_to_fn: HashMap<&str, u32> = HashMap::new();
    let mut records = Vec::new();
    let mut rec_of_node = vec![usize::MAX; n];

    for &node in &r.order {
        if node == r.sink {
            continue;
        }
        let nd = &patch.nodes[node];
        let entry = registry.get(&nd.ty).expect("type checked in resolve");
        let fn_index = fn_index_of(&mut fns, &mut type_to_fn, nd.ty.as_str(), entry.process);
        let inputs = r.in_srcs[node]
            .iter()
            .map(|src| match src {
                Some((pnode, pport)) => Source::Port(rec_of_node[*pnode], *pport),
                None => Source::Zero,
            })
            .collect();
        rec_of_node[node] = records.len();
        records.push(RecordSpec {
            fn_index,
            state: (entry.init_bytes)(&nd.params),
            inputs,
            num_outputs: r.out_names[node].len(),
        });
    }

    let sink = r.in_srcs[r.sink]
        .iter()
        .map(|src| src.map(|(pnode, pport)| (rec_of_node[pnode], pport)))
        .collect();

    Mono {
        plan: Plan::build(capacity, &records),
        fns,
        sink,
    }
}

fn build_poly(
    patch: &Patch,
    registry: &Registry,
    r: &Resolved,
    source: usize,
    capacity: usize,
) -> Result<Poly, EngineError> {
    let n = patch.nodes.len();

    // Nodes downstream of the source (forward reachability) become per-voice records.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (to, srcs) in r.in_srcs.iter().enumerate() {
        for s in srcs.iter().flatten() {
            adj[s.0].push(to);
        }
    }
    let mut is_voice = vec![false; n];
    let mut stack = vec![source];
    while let Some(node) = stack.pop() {
        for &next in &adj[node] {
            if next != r.sink && !is_voice[next] && next != source {
                is_voice[next] = true;
                stack.push(next);
            }
        }
    }

    let mut fns: Vec<ProcessFn> = Vec::new();
    let mut type_to_fn: HashMap<&str, u32> = HashMap::new();
    let mut shared = Vec::new();
    let mut voice = Vec::new();
    let mut template_states: Vec<Vec<u8>> = Vec::new();
    let mut shared_rec = vec![usize::MAX; n];
    let mut voice_rec = vec![usize::MAX; n];

    for &node in &r.order {
        if node == r.sink || node == source {
            continue;
        }
        let nd = &patch.nodes[node];
        let entry = registry.get(&nd.ty).expect("type checked in resolve");
        let fn_index = fn_index_of(&mut fns, &mut type_to_fn, nd.ty.as_str(), entry.process);
        let state = (entry.init_bytes)(&nd.params);
        let num_outputs = r.out_names[node].len();

        if is_voice[node] {
            let inputs = r.in_srcs[node]
                .iter()
                .map(|src| match src {
                    None => VoiceSource::Zero,
                    Some((pnode, pport)) => {
                        if *pnode == source {
                            VoiceSource::SourceOut(*pport)
                        } else if is_voice[*pnode] {
                            VoiceSource::Voice(voice_rec[*pnode], *pport)
                        } else {
                            VoiceSource::Shared(shared_rec[*pnode], *pport)
                        }
                    }
                })
                .collect();
            voice_rec[node] = voice.len();
            template_states.push(state.clone());
            voice.push(VoiceRecordSpec {
                fn_index,
                state,
                inputs,
                num_outputs,
            });
        } else {
            let inputs = r.in_srcs[node]
                .iter()
                .map(|src| match src {
                    Some((pnode, pport)) => Source::Port(shared_rec[*pnode], *pport),
                    None => Source::Zero,
                })
                .collect();
            shared_rec[node] = shared.len();
            shared.push(RecordSpec {
                fn_index,
                state,
                inputs,
                num_outputs,
            });
        }
    }

    let num_source_outputs = r.out_names[source].len();

    let sink = r.in_srcs[r.sink]
        .iter()
        .map(|src| {
            src.and_then(|(pnode, pport)| {
                if is_voice[pnode] {
                    Some(SinkSrc::Voice(voice_rec[pnode], pport))
                } else if pnode == source {
                    None // a source output wired straight to audio is not supported
                } else {
                    Some(SinkSrc::Shared(shared_rec[pnode], pport))
                }
            })
        })
        .collect();

    let plan = VoicedPlan::build(
        capacity,
        &shared,
        num_source_outputs,
        &voice,
        DEFAULT_MAX_VOICES,
    );
    let source_module = (registry
        .source(&patch.nodes[source].ty)
        .expect("source checked")
        .make)(&patch.nodes[source].params);

    Ok(Poly {
        plan,
        fns,
        source: source_module,
        slots: SlotManager::new(DEFAULT_MAX_VOICES),
        template_states,
        sink,
        slot_alive: vec![false; DEFAULT_MAX_VOICES],
    })
}

fn fn_index_of<'a>(
    fns: &mut Vec<ProcessFn>,
    type_to_fn: &mut HashMap<&'a str, u32>,
    ty: &'a str,
    process: ProcessFn,
) -> u32 {
    *type_to_fn.entry(ty).or_insert_with(|| {
        fns.push(process);
        (fns.len() - 1) as u32
    })
}

/// Kahn's algorithm; returns nodes in dependency order or [`EngineError::Cycle`].
fn topo_sort(n: usize, edges: &[(usize, usize)]) -> Result<Vec<usize>, EngineError> {
    let mut indegree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(from, to) in edges {
        adj[from].push(to);
        indegree[to] += 1;
    }

    let mut queue: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(node) = queue.pop() {
        order.push(node);
        for &next in &adj[node] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                queue.push(next);
            }
        }
    }

    if order.len() == n {
        Ok(order)
    } else {
        Err(EngineError::Cycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Params;
    use crate::module::{ModuleDesc, PolyphonicModule, PortDesc, SourceCtx, SourceType, VoiceId};
    use crate::processing::Tail;

    const PURE_TONE: &str = r#"
nodes:
  - id: freq
    type: const_generator
    params: { value: 1.0 }
  - id: amp
    type: const_generator
    params: { value: 0.5 }
  - id: osc
    type: sine_generator
  - id: out
    type: audio_output
    params: { sample_rate: 4, channels: 2 }
wires:
  - { from: [freq, out], to: [osc, frequency] }
  - { from: [amp,  out], to: [osc, amplitude] }
  - { from: [osc, out], to: [out, ch0] }
  - { from: [osc, out], to: [out, ch1] }
"#;

    #[test]
    fn renders_pure_tone_to_stereo() {
        let patch = Patch::from_yaml(PURE_TONE).unwrap();
        let mut engine = PlanEngine::build(&patch, &Registry::with_builtins(), 64).unwrap();
        assert_eq!(engine.channels(), 2);
        assert_eq!(engine.sample_rate(), 4.0);

        let frames = 4;
        let mut device = vec![0.0f32; frames * 2];
        engine.process_block(&mut device, frames);

        for i in 0..frames {
            assert_eq!(device[i * 2], device[i * 2 + 1]);
        }
        let eps = 1e-5;
        assert!((device[0] - 0.5).abs() < eps);
        assert!(device[2].abs() < eps);
        assert!((device[4] + 0.5).abs() < eps);
        assert!(device[6].abs() < eps);
    }

    #[test]
    fn missing_audio_output_errors() {
        let patch = Patch::from_yaml("nodes:\n  - id: a\n    type: sine_generator\n").unwrap();
        let result = PlanEngine::build(&patch, &Registry::with_builtins(), 64);
        assert!(matches!(result, Err(EngineError::NoAudioOutput)));
    }

    #[test]
    fn unconnected_channel_is_silent() {
        let yaml = r#"
nodes:
  - id: freq
    type: const_generator
    params: { value: 1.0 }
  - id: amp
    type: const_generator
    params: { value: 0.5 }
  - id: osc
    type: sine_generator
  - id: out
    type: audio_output
    params: { sample_rate: 4, channels: 2 }
wires:
  - { from: [freq, out], to: [osc, frequency] }
  - { from: [amp,  out], to: [osc, amplitude] }
  - { from: [osc, out], to: [out, ch0] }
"#;
        let patch = Patch::from_yaml(yaml).unwrap();
        let mut engine = PlanEngine::build(&patch, &Registry::with_builtins(), 64).unwrap();
        let mut device = vec![0.0f32; 4 * 2];
        engine.process_block(&mut device, 4);
        for i in 0..4 {
            assert_eq!(device[i * 2 + 1], 0.0);
        }
        assert!((device[0] - 0.5).abs() < 1e-5);
    }

    // A scripted voice source: plays `pitch` (held) on block 0, releases on block `release_at`.
    struct ScriptKeys {
        block: u64,
        release_at: u64,
        pitch: f32,
        voice: Option<VoiceId>,
    }
    impl PolyphonicModule for ScriptKeys {
        fn process(&mut self, ctx: &mut SourceCtx) -> Tail {
            if self.block == 0 {
                if let Some(v) = ctx.allocate() {
                    self.voice = Some(v);
                }
            }
            if let Some(v) = self.voice {
                let frames = ctx.frames;
                let gate = if self.block >= self.release_at { 0.0 } else { 1.0 };
                ctx.voice_output(v, 0)[..frames].fill(self.pitch); // pitch
                ctx.voice_output(v, 1)[..frames].fill(gate); // gate
                if self.block == self.release_at {
                    ctx.release(v);
                }
            }
            self.block += 1;
            Tail::Active
        }
    }

    struct ScriptKeysType;
    impl SourceType for ScriptKeysType {
        type Module = ScriptKeys;
        fn describe(_p: &Params) -> ModuleDesc {
            ModuleDesc {
                inputs: vec![],
                outputs: vec![PortDesc::sample("pitch"), PortDesc::sample("gate")],
            }
        }
        fn make(_p: &Params) -> ScriptKeys {
            ScriptKeys {
                block: 0,
                release_at: 2,
                pitch: 0.25,
                voice: None,
            }
        }
    }

    fn poly_registry() -> Registry {
        let mut r = Registry::with_builtins();
        r.register_source::<ScriptKeysType>("script_keys");
        r
    }

    #[test]
    fn poly_voice_plays_then_frees_after_release() {
        // keys -> mul(pitch * gate) -> audio. gate held blocks 0..1, released at block 2.
        let yaml = r#"
nodes:
  - id: keys
    type: script_keys
  - id: vca
    type: mul
  - id: out
    type: audio_output
    params: { sample_rate: 100, channels: 1 }
wires:
  - { from: [keys, pitch], to: [vca, a] }
  - { from: [keys, gate],  to: [vca, b] }
  - { from: [vca, out], to: [out, ch0] }
"#;
        let patch = Patch::from_yaml(yaml).unwrap();
        let mut engine = PlanEngine::build(&patch, &poly_registry(), 64).unwrap();
        assert_eq!(engine.channels(), 1);

        let frames = 4;
        let mut device = vec![0.0f32; frames];

        // Block 0: voice active, gate=1 -> 0.25 * 1.
        engine.process_block(&mut device, frames);
        assert!((device[0] - 0.25).abs() < 1e-6);

        // Block 1: still held.
        engine.process_block(&mut device, frames);
        assert!((device[0] - 0.25).abs() < 1e-6);

        // Block 2: released, gate=0 -> 0. mul is stateless (Tail::Done) so the slot frees.
        engine.process_block(&mut device, frames);
        assert!(device[0].abs() < 1e-6);

        // Block 3: slot freed; source no longer writes -> silence.
        engine.process_block(&mut device, frames);
        assert!(device[0].abs() < 1e-6);
    }
}
