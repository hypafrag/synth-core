//! The module processing interface.
//!
//! A tick processes a block of `frames` samples. Modules read input port buffers and write
//! output port buffers, iterating sample-by-sample where needed (e.g. for feedback or
//! per-sample frequency control). See `docs/architecture/05-processing-model.md`
//! (block contract, frame count may be 1) and `09-module-contract.md`.

/// Configuration handed to a module before processing. All allocation/sizing happens here so
/// that [`Module::process`] never allocates or locks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrepareCfg {
    pub sample_rate: f32,
    pub max_frames: usize,
}

/// Whether a module is still producing output that must keep its voice alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    Active,
    Done,
}

/// Per-tick IO: input and output sample buffers for one block, indexed by port order.
pub struct ProcessCtx<'a> {
    pub frames: usize,
    pub inputs: &'a [&'a [f32]],
    pub outputs: &'a mut [&'a mut [f32]],
}

/// A runnable module instance.
pub trait Module: Send {
    /// Set sample rate / max block size; allocate here, never in [`Module::process`].
    fn prepare(&mut self, cfg: &PrepareCfg);
    /// Clear internal state (phase, filter memory, …).
    fn reset(&mut self);
    /// Produce one block of output from the inputs; report liveness.
    fn process(&mut self, ctx: &mut ProcessCtx<'_>) -> Tail;
}
