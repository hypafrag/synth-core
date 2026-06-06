//! The engine: compiles a [`Patch`] into a runnable graph and produces audio blocks.
//!
//! Each call to [`Engine::process_block`] is one tick: every node is evaluated once in
//! dependency order (its output cached), then the audio-output sink's input buffers are
//! interleaved into the device buffer. See `docs/architecture/07-execution-and-events.md`.

use std::collections::{HashMap, HashSet};

use crate::model::Patch;
use crate::processing::{PrepareCfg, ProcessCtx};
use crate::registry::Registry;

const AUDIO_OUTPUT: &str = "audio_output";

#[derive(Debug)]
pub enum EngineError {
    Build { node: String, msg: String },
    UnknownNode(String),
    UnknownPort { node: String, port: String },
    NoAudioOutput,
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
            EngineError::Cycle => write!(f, "patch graph has a cycle (feedback is not yet supported)"),
        }
    }
}

impl std::error::Error for EngineError {}

/// A compiled, runnable patch.
pub struct Engine {
    modules: Vec<Box<dyn crate::processing::Module>>,
    order: Vec<usize>,
    /// node -> output-port-index -> buffer id
    out_bufs: Vec<Vec<usize>>,
    /// node -> input-port-index -> source buffer id (None = unconnected)
    in_srcs: Vec<Vec<Option<usize>>>,
    pool: Vec<Vec<f32>>,
    zero: Vec<f32>,
    sink: usize,
    channels: usize,
    sample_rate: f32,
    capacity: usize,
    elapsed_frames: u64,
}

impl Engine {
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Compile a patch into an engine. `capacity` is the maximum block size (frames).
    pub fn build(patch: &Patch, registry: &Registry, capacity: usize) -> Result<Self, EngineError> {
        let n = patch.nodes.len();

        let mut index = HashMap::new();
        for (i, node) in patch.nodes.iter().enumerate() {
            index.insert(node.id.as_str(), i);
        }

        // Create modules.
        let mut modules = Vec::with_capacity(n);
        for node in &patch.nodes {
            let m = registry
                .create(&node.ty, &node.params)
                .map_err(|msg| EngineError::Build {
                    node: node.id.clone(),
                    msg,
                })?;
            modules.push(m);
        }

        let in_names: Vec<Vec<String>> = modules.iter().map(|m| m.input_ports()).collect();
        let out_names: Vec<Vec<String>> = modules.iter().map(|m| m.output_ports()).collect();

        // Allocate a buffer id per output port.
        let mut out_bufs = vec![Vec::new(); n];
        let mut buf_count = 0usize;
        for (i, ports) in out_names.iter().enumerate() {
            for _ in ports {
                out_bufs[i].push(buf_count);
                buf_count += 1;
            }
        }

        // Resolve wires into input sources and node edges.
        let mut in_srcs: Vec<Vec<Option<usize>>> =
            (0..n).map(|i| vec![None; in_names[i].len()]).collect();
        let mut edges = HashSet::new();
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
            in_srcs[ti][tp] = Some(out_bufs[fi][fp]);
            edges.insert((fi, ti));
        }

        let order = topo_sort(n, &edges)?;

        // The audio sink and the global sample rate / channel count.
        let sink = patch
            .nodes
            .iter()
            .position(|nd| nd.ty == AUDIO_OUTPUT)
            .ok_or(EngineError::NoAudioOutput)?;
        let sink_params = &patch.nodes[sink].params;
        let sample_rate = sink_params
            .get("sample_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(44100.0) as f32;
        let channels = in_names[sink].len();

        let cfg = PrepareCfg {
            sample_rate,
            max_frames: capacity,
        };
        for m in &mut modules {
            m.prepare(&cfg);
        }

        let pool = (0..buf_count).map(|_| vec![0.0f32; capacity]).collect();
        let zero = vec![0.0f32; capacity];

        Ok(Engine {
            modules,
            order,
            out_bufs,
            in_srcs,
            pool,
            zero,
            sink,
            channels,
            sample_rate,
            capacity,
            elapsed_frames: 0,
        })
    }

    /// Process one block, writing `frames` interleaved frames into `device`
    /// (`device.len()` must be `frames * channels`).
    pub fn process_block(&mut self, device: &mut [f32], frames: usize) {
        let frames = frames.min(self.capacity);
        let time = self.elapsed_frames as f64 / self.sample_rate as f64;

        for k in 0..self.order.len() {
            let node = self.order[k];
            self.run_node(node, frames, time);
        }
        self.elapsed_frames += frames as u64;

        // Interleave the sink's per-channel inputs into the device buffer.
        let channels = self.channels;
        for c in 0..channels {
            match self.in_srcs[self.sink].get(c).copied().flatten() {
                Some(buf) => {
                    for i in 0..frames {
                        device[i * channels + c] = self.pool[buf][i];
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

    fn run_node(&mut self, node: usize, frames: usize, time: f64) {
        // Take this node's output buffers out of the pool so we can hold them mutably while
        // borrowing the pool immutably for inputs.
        let mut outs: Vec<Vec<f32>> = self.out_bufs[node]
            .iter()
            .map(|&id| std::mem::take(&mut self.pool[id]))
            .collect();

        {
            let inputs: Vec<&[f32]> = self.in_srcs[node]
                .iter()
                .map(|src| match src {
                    Some(id) => &self.pool[*id][..frames],
                    None => &self.zero[..frames],
                })
                .collect();
            let mut out_refs: Vec<&mut [f32]> =
                outs.iter_mut().map(|b| &mut b[..frames]).collect();
            let mut ctx = ProcessCtx {
                frames,
                sample_rate: self.sample_rate,
                time,
                inputs: &inputs,
                outputs: &mut out_refs,
            };
            self.modules[node].process(&mut ctx);
        }

        for (i, buf) in outs.into_iter().enumerate() {
            let id = self.out_bufs[node][i];
            self.pool[id] = buf;
        }
    }
}

/// Kahn's algorithm; returns nodes in dependency order or [`EngineError::Cycle`].
fn topo_sort(n: usize, edges: &HashSet<(usize, usize)>) -> Result<Vec<usize>, EngineError> {
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
    use crate::model::Patch;
    use crate::processing::{Module, ProcessCtx, Tail};

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
        let mut engine = Engine::build(&patch, &Registry::with_builtins(), 64).unwrap();
        assert_eq!(engine.channels(), 2);
        assert_eq!(engine.sample_rate(), 4.0);

        let frames = 4;
        let mut device = vec![0.0f32; frames * 2];
        engine.process_block(&mut device, frames);

        // Both channels carry the same signal (fan-out).
        for i in 0..frames {
            assert_eq!(device[i * 2], device[i * 2 + 1]);
        }

        // sr=4, freq=1, amp 0.5, time-based phase = 2*pi*(i/4):
        // i0: sin(0)=0, i1: sin(pi/2)*0.5=0.5, i2: sin(pi)=0, i3: sin(3pi/2)*0.5=-0.5.
        let eps = 1e-5;
        assert!(device[0].abs() < eps);
        assert!((device[2] - 0.5).abs() < eps);
        assert!(device[4].abs() < eps);
        assert!((device[6] + 0.5).abs() < eps);
    }

    #[test]
    fn missing_audio_output_errors() {
        let patch = Patch::from_yaml("nodes:\n  - id: a\n    type: sine_generator\n").unwrap();
        let result = Engine::build(&patch, &Registry::with_builtins(), 64);
        assert!(matches!(result, Err(EngineError::NoAudioOutput)));
    }

    /// Test module that emits the current block time (seconds) on its output.
    struct TimeProbe;
    impl Module for TimeProbe {
        fn output_ports(&self) -> Vec<String> {
            vec!["out".to_string()]
        }
        fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail {
            let t = ctx.time as f32;
            ctx.outputs[0][..ctx.frames].fill(t);
            Tail::Done
        }
    }

    #[test]
    fn time_advances_per_block() {
        let mut registry = Registry::with_builtins();
        registry.register("time_probe", Box::new(|_| Ok(Box::new(TimeProbe))));
        let yaml = "nodes:\n  - id: t\n    type: time_probe\n  - id: out\n    type: audio_output\n    params: { sample_rate: 100, channels: 1 }\nwires:\n  - { from: [t, out], to: [out, ch0] }\n";
        let patch = Patch::from_yaml(yaml).unwrap();
        let mut engine = Engine::build(&patch, &registry, 64).unwrap();

        let frames = 10;
        let mut device = vec![0.0f32; frames];
        engine.process_block(&mut device, frames);
        assert_eq!(device[0], 0.0); // first block starts at t = 0

        engine.process_block(&mut device, frames);
        assert!((device[0] - 0.1).abs() < 1e-6); // 10 frames / 100 Hz = 0.1 s
    }
}
