//! The single-buffer processing plan (see `docs/architecture/06-processing-plan.md`).
//!
//! One contiguous byte buffer holds every module's state and output buffers plus the schedule,
//! in topological order. The audio callback just walks it; all layout happens beforehand.
//! References inside the buffer are `u32` byte offsets (relocatable); dispatch is a `u32` index
//! into a function table.

use crate::processing::Tail;

pub const STATE_ALIGN: usize = 16;
pub const BUF_ALIGN: usize = 64;
pub const RECORD_ALIGN: usize = 64;

const TERMINATOR: u32 = 0xFFFF_FFFF;
const HEADER_WORDS: usize = 5; // skip, fn_index, state_size, num_inputs, num_outputs
const F32: usize = std::mem::size_of::<f32>();

#[inline]
fn align_up(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

/// Per-tick context handed to every process function.
pub struct TickCtx {
    pub frames: usize,
    pub block_size: usize,
    pub sample_rate: f32,
    pub time: f64,
}

/// Resolved offsets of one record's regions, passed to its process function.
pub struct Record {
    pub state: u32,
    pub inputs: u32,
    pub num_inputs: u32,
    pub outputs: u32,
    pub num_outputs: u32,
}

/// Type-erased per-record behavior. Resolves its state/inputs/outputs from `base` + the offsets
/// in `rec` (helpers below), then does the work. Must not allocate or lock.
pub type ProcessFn = unsafe fn(base: *mut u8, ctx: &TickCtx, rec: &Record) -> Tail;

/// A module's `state` reinterpreted as `T`.
///
/// # Safety
/// The record's state region must hold a valid, correctly-aligned `T`.
#[inline]
pub unsafe fn state_mut<'a, T>(base: *mut u8, rec: &Record) -> &'a mut T {
    unsafe { &mut *(base.add(rec.state as usize) as *mut T) }
}

/// Input port `port` as a `frames`-long slice (resolved through its stored offset).
///
/// # Safety
/// `port < rec.num_inputs`; the referenced buffer holds at least `frames` `f32`s.
#[inline]
pub unsafe fn input<'a>(base: *mut u8, rec: &Record, port: usize, frames: usize) -> &'a [f32] {
    unsafe {
        let buf_off = *(base.add(rec.inputs as usize) as *const u32).add(port) as usize;
        std::slice::from_raw_parts(base.add(buf_off) as *const f32, frames)
    }
}

/// Output port `port` as a `frames`-long mutable slice.
///
/// # Safety
/// `port < rec.num_outputs`; output regions never overlap inputs in a topological plan.
#[inline]
pub unsafe fn output<'a>(
    base: *mut u8,
    rec: &Record,
    port: usize,
    block_size: usize,
    frames: usize,
) -> &'a mut [f32] {
    let off = rec.outputs as usize + port * block_size * F32;
    unsafe { std::slice::from_raw_parts_mut(base.add(off) as *mut f32, frames) }
}

/// Where an input port reads from, while building (resolved to an offset).
#[derive(Clone, Copy)]
pub enum Source {
    /// `(record index, output port)` of a producer earlier in the plan.
    Port(usize, usize),
    /// Unconnected — reads the shared zero buffer.
    Zero,
}

/// One record to lay out, in topological order.
pub struct RecordSpec {
    pub fn_index: u32,
    /// Initial state bytes (its length is the state size).
    pub state: Vec<u8>,
    /// One source per input port, in port order.
    pub inputs: Vec<Source>,
    pub num_outputs: usize,
}

/// The compiled plan buffer.
pub struct Plan {
    buf: Vec<u8>,
    block_size: usize,
    records_start: usize,
    /// Output-region offset of each record's port 0 (port k is `+ k * block_size * 4`).
    out_offsets: Vec<u32>,
}

impl Plan {
    /// Lay out `records` (topological order) into one buffer.
    pub fn build(block_size: usize, records: &[RecordSpec]) -> Plan {
        // Header: one u32 (block_size), then a shared zero buffer for unconnected inputs.
        let mut size = align_up(4, BUF_ALIGN);
        let zero_off = size;
        size += block_size * F32;
        size = align_up(size, RECORD_ALIGN);
        let records_start = size;

        // Compute each record's region offsets.
        let mut rec_off = Vec::with_capacity(records.len());
        let mut out_offsets = Vec::with_capacity(records.len());
        for r in records {
            rec_off.push(size);
            let state_off = align_up(size + HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + r.state.len(), 4);
            let outputs_off = align_up(inputs_off + r.inputs.len() * 4, BUF_ALIGN);
            out_offsets.push(outputs_off as u32);
            size = align_up(outputs_off + r.num_outputs * block_size * F32, RECORD_ALIGN);
        }
        let terminator_off = size;
        size = align_up(size + 4, RECORD_ALIGN);

        let mut buf = vec![0u8; size];
        write_u32(&mut buf, 0, block_size as u32);

        for (i, r) in records.iter().enumerate() {
            let start = rec_off[i];
            let state_off = align_up(start + HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + r.state.len(), 4);

            write_u32(&mut buf, start, 0); // skip = active
            write_u32(&mut buf, start + 4, r.fn_index);
            write_u32(&mut buf, start + 8, r.state.len() as u32);
            write_u32(&mut buf, start + 12, r.inputs.len() as u32);
            write_u32(&mut buf, start + 16, r.num_outputs as u32);

            buf[state_off..state_off + r.state.len()].copy_from_slice(&r.state);

            for (k, src) in r.inputs.iter().enumerate() {
                let buf_off = match *src {
                    Source::Port(rec, port) => out_offsets[rec] + (port * block_size * F32) as u32,
                    Source::Zero => zero_off as u32,
                };
                write_u32(&mut buf, inputs_off + k * 4, buf_off);
            }
        }

        write_u32(&mut buf, terminator_off, TERMINATOR);

        Plan {
            buf,
            block_size,
            records_start,
            out_offsets,
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Byte offset of `record`'s output port `port`.
    pub fn output_offset(&self, record: usize, port: usize) -> u32 {
        self.out_offsets[record] + (port * self.block_size * F32) as u32
    }

    /// Read an output buffer by absolute byte offset.
    pub fn buffer_at(&self, offset: u32, frames: usize) -> &[f32] {
        let ptr = self.buf[offset as usize..].as_ptr() as *const f32;
        unsafe { std::slice::from_raw_parts(ptr, frames) }
    }

    /// Walk the plan once, dispatching each active record through `fns`.
    pub fn run(&mut self, fns: &[ProcessFn], sample_rate: f32, time: f64, frames: usize) {
        let frames = frames.min(self.block_size);
        let ctx = TickCtx {
            frames,
            block_size: self.block_size,
            sample_rate,
            time,
        };
        let base = self.buf.as_mut_ptr();
        let mut off = self.records_start;
        loop {
            let skip = unsafe { read_u32_ptr(base, off) };
            if skip == TERMINATOR {
                break;
            }
            if skip != 0 {
                off = skip as usize;
                continue;
            }
            let fn_index = unsafe { read_u32_ptr(base, off + 4) };
            let state_size = unsafe { read_u32_ptr(base, off + 8) } as usize;
            let num_inputs = unsafe { read_u32_ptr(base, off + 12) };
            let num_outputs = unsafe { read_u32_ptr(base, off + 16) };

            let state_off = align_up(off + HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + state_size, 4);
            let outputs_off = align_up(inputs_off + num_inputs as usize * 4, BUF_ALIGN);

            let rec = Record {
                state: state_off as u32,
                inputs: inputs_off as u32,
                num_inputs,
                outputs: outputs_off as u32,
                num_outputs,
            };
            unsafe { fns[fn_index as usize](base, &ctx, &rec) };

            let rec_end = outputs_off + num_outputs as usize * self.block_size * F32;
            off = align_up(rec_end, RECORD_ALIGN);
        }
    }
}

/// Where a per-voice input reads from, while building a voiced plan.
#[derive(Clone, Copy)]
pub enum VoiceSource {
    /// A shared (mono) record's output: `(shared record index, output port)`.
    Shared(usize, usize),
    /// One of the voice source's per-voice outputs for this slot: `(output port)`.
    SourceOut(usize),
    /// Another per-voice record within the same slot: `(voice record index, output port)`.
    Voice(usize, usize),
    /// Unconnected — the shared zero buffer.
    Zero,
}

/// One per-voice record to lay out (stamped once per slot).
pub struct VoiceRecordSpec {
    pub fn_index: u32,
    pub state: Vec<u8>,
    pub inputs: Vec<VoiceSource>,
    pub num_outputs: usize,
}

/// A plan with a voice source: the shared (mono) records, then the per-voice region.
///
/// The per-voice region groups records **by module**: all `max_voices` copies of one voice
/// record sit contiguously, then the next module's, and so on — e.g. for `lfo -> tone` the
/// layout is `lfo_v0 lfo_v1 … lfo_vN  tone_v0 tone_v1 … tone_vN`. Running a module's voices back
/// to back keeps its `process` (and code) hot and lets the voices be batched.
///
/// The walk runs the shared records, then each module's voices in turn. A disabled voice's record
/// jumps to the next voice of the same module via its skip field, so disabling voice `k` writes
/// one skip per module. The source writes each active slot's source-output buffers before the
/// walk; the engine sums the active voices' outputs after it.
pub struct VoicedPlan {
    buf: Vec<u8>,
    block_size: usize,
    records_start: usize,
    shared_out_offsets: Vec<u32>,
    // Voice region (records grouped by module — see the struct doc):
    first_voice_off: usize,
    terminator_off: usize,
    source_out_base: usize,
    source_out_stride: usize,
    num_source_outputs: usize,
    /// Per voice module: absolute byte offset of its voice-0 record.
    voice_block_base: Vec<usize>,
    /// Per voice module: byte stride from one voice's record to the next.
    voice_stride: Vec<usize>,
    /// Per voice module: within-record byte offset of the port-0 output buffer.
    voice_out_within: Vec<u32>,
    /// Per voice module: within-record byte offset of the state region.
    voice_state_within: Vec<u32>,
    max_voices: usize,
}

impl VoicedPlan {
    /// Lay out the shared records, the per-voice slot template, and `max_voices` slots.
    /// All slots start **disabled** (no voices playing); the engine enables them on allocate.
    pub fn build(
        block_size: usize,
        shared: &[RecordSpec],
        num_source_outputs: usize,
        voice: &[VoiceRecordSpec],
        max_voices: usize,
    ) -> VoicedPlan {
        let mut size = align_up(4, BUF_ALIGN);
        let zero_off = size;
        size += block_size * F32;

        // Per-slot source-output buffers, kept **outside** the walk path (written by the source,
        // never read as a record), so the walk flows shared records → voice slots uninterrupted.
        size = align_up(size, BUF_ALIGN);
        let source_out_base = size;
        let source_out_stride = align_up(num_source_outputs * block_size * F32, BUF_ALIGN);
        size += source_out_stride * max_voices;

        size = align_up(size, RECORD_ALIGN);
        let records_start = size;

        // Shared records (offsets absolute, like the flat plan).
        let mut shared_rec_off = Vec::with_capacity(shared.len());
        let mut shared_out_offsets = Vec::with_capacity(shared.len());
        for r in shared {
            shared_rec_off.push(size);
            let state_off = align_up(size + HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + r.state.len(), 4);
            let outputs_off = align_up(inputs_off + r.inputs.len() * 4, BUF_ALIGN);
            shared_out_offsets.push(outputs_off as u32);
            size = align_up(outputs_off + r.num_outputs * block_size * F32, RECORD_ALIGN);
        }

        // Voice region: one contiguous block of `max_voices` records per voice module, so a
        // module's voices sit together (lfo_v0 … lfo_vN, then tone_v0 … tone_vN). The walk flows
        // straight in from the shared records.
        let first_voice_off = size;

        let mut voice_block_base = Vec::with_capacity(voice.len());
        let mut voice_stride = Vec::with_capacity(voice.len());
        let mut voice_state_within = Vec::with_capacity(voice.len());
        let mut voice_out_within = Vec::with_capacity(voice.len());
        for r in voice {
            let base = size;
            voice_block_base.push(base);
            let state_off = align_up(HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + r.state.len(), 4);
            let outputs_off = align_up(inputs_off + r.inputs.len() * 4, BUF_ALIGN);
            voice_state_within.push(state_off as u32);
            voice_out_within.push(outputs_off as u32);
            let stride = align_up(outputs_off + r.num_outputs * block_size * F32, RECORD_ALIGN);
            voice_stride.push(stride);
            size = base + stride * max_voices;
        }
        let terminator_off = size;
        size = align_up(size + 4, RECORD_ALIGN);

        let mut buf = vec![0u8; size];
        write_u32(&mut buf, 0, block_size as u32);

        // Write shared records.
        for (i, r) in shared.iter().enumerate() {
            let start = shared_rec_off[i];
            let state_off = align_up(start + HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + r.state.len(), 4);
            write_u32(&mut buf, start, 0);
            write_u32(&mut buf, start + 4, r.fn_index);
            write_u32(&mut buf, start + 8, r.state.len() as u32);
            write_u32(&mut buf, start + 12, r.inputs.len() as u32);
            write_u32(&mut buf, start + 16, r.num_outputs as u32);
            buf[state_off..state_off + r.state.len()].copy_from_slice(&r.state);
            for (k, src) in r.inputs.iter().enumerate() {
                let buf_off = match *src {
                    Source::Port(rec, port) => {
                        shared_out_offsets[rec] + (port * block_size * F32) as u32
                    }
                    Source::Zero => zero_off as u32,
                };
                write_u32(&mut buf, inputs_off + k * 4, buf_off);
            }
        }

        // Stamp every voice of every module, then disable all voices. A voice record reading
        // another voice record (`VoiceSource::Voice`) reads that module's copy *in the same slot*.
        for (m, r) in voice.iter().enumerate() {
            for slot in 0..max_voices {
                let start = voice_block_base[m] + slot * voice_stride[m];
                let state_off = start + voice_state_within[m] as usize;
                let inputs_off = align_up(state_off + r.state.len(), 4);
                write_u32(&mut buf, start, 0);
                write_u32(&mut buf, start + 4, r.fn_index);
                write_u32(&mut buf, start + 8, r.state.len() as u32);
                write_u32(&mut buf, start + 12, r.inputs.len() as u32);
                write_u32(&mut buf, start + 16, r.num_outputs as u32);
                buf[state_off..state_off + r.state.len()].copy_from_slice(&r.state);
                for (k, src) in r.inputs.iter().enumerate() {
                    let buf_off = match *src {
                        VoiceSource::Shared(rec, port) => {
                            shared_out_offsets[rec] + (port * block_size * F32) as u32
                        }
                        VoiceSource::SourceOut(port) => {
                            (source_out_base + slot * source_out_stride + port * block_size * F32)
                                as u32
                        }
                        VoiceSource::Voice(rec, port) => {
                            (voice_block_base[rec]
                                + slot * voice_stride[rec]
                                + voice_out_within[rec] as usize
                                + port * block_size * F32) as u32
                        }
                        VoiceSource::Zero => zero_off as u32,
                    };
                    write_u32(&mut buf, inputs_off + k * 4, buf_off);
                }
            }
        }

        write_u32(&mut buf, terminator_off, TERMINATOR);

        let mut plan = VoicedPlan {
            buf,
            block_size,
            records_start,
            shared_out_offsets,
            first_voice_off,
            terminator_off,
            source_out_base,
            source_out_stride,
            num_source_outputs,
            voice_block_base,
            voice_stride,
            voice_out_within,
            voice_state_within,
            max_voices,
        };
        for slot in 0..max_voices {
            plan.set_slot_active(slot, false);
        }
        plan
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn max_voices(&self) -> usize {
        self.max_voices
    }

    /// Enable (skip = 0) or disable (skip = jump to the same module's next voice) voice `slot`.
    /// A voice spans one record per module, so toggling it writes one skip per module.
    pub fn set_slot_active(&mut self, slot: usize, active: bool) {
        for m in 0..self.voice_block_base.len() {
            let off = self.voice_block_base[m] + slot * self.voice_stride[m];
            let skip = if active {
                0
            } else {
                (self.voice_block_base[m] + (slot + 1) * self.voice_stride[m]) as u32
            };
            write_u32(&mut self.buf, off, skip);
        }
    }

    /// A shared record's output buffer.
    pub fn shared_output(&self, record: usize, port: usize, frames: usize) -> &[f32] {
        let off = self.shared_out_offsets[record] as usize + port * self.block_size * F32;
        self.slice_at(off, frames)
    }

    /// Slot `slot`'s source-output buffer for `port` (written by the source before the walk).
    pub fn source_output_mut(&mut self, slot: usize, port: usize, frames: usize) -> &mut [f32] {
        let off = self.source_out_base + slot * self.source_out_stride + port * self.block_size * F32;
        let ptr = self.buf[off..].as_mut_ptr() as *mut f32;
        unsafe { std::slice::from_raw_parts_mut(ptr, frames) }
    }

    /// A per-voice record's output buffer in voice `slot`.
    pub fn voice_output(&self, slot: usize, record: usize, port: usize, frames: usize) -> &[f32] {
        let off = self.voice_block_base[record]
            + slot * self.voice_stride[record]
            + self.voice_out_within[record] as usize
            + port * self.block_size * F32;
        self.slice_at(off, frames)
    }

    fn slice_at(&self, off: usize, frames: usize) -> &[f32] {
        let ptr = self.buf[off..].as_ptr() as *const f32;
        unsafe { std::slice::from_raw_parts(ptr, frames) }
    }

    /// Overwrite a slot's voice-record state with fresh bytes (on voice spawn). `state.len()`
    /// must match the record's state size.
    pub fn set_voice_state(&mut self, slot: usize, record: usize, state: &[u8]) {
        let off = self.voice_block_base[record]
            + slot * self.voice_stride[record]
            + self.voice_state_within[record] as usize;
        self.buf[off..off + state.len()].copy_from_slice(state);
    }

    /// Zero a slot's source-output buffers (on spawn, before the source writes).
    pub fn clear_source_outputs(&mut self, slot: usize, frames: usize) {
        for port in 0..self.num_source_outputs {
            self.source_output_mut(slot, port, frames).fill(0.0);
        }
    }

    /// Walk the plan once. `slot_alive[k]` is set to whether any record in active slot `k`
    /// reported `Tail::Active` (for the engine's free-on-`Done` decision). Disabled slots stay
    /// `false`. `slot_alive.len()` must be `max_voices`.
    pub fn run(
        &mut self,
        fns: &[ProcessFn],
        sample_rate: f32,
        time: f64,
        frames: usize,
        slot_alive: &mut [bool],
    ) {
        let frames = frames.min(self.block_size);
        let ctx = TickCtx {
            frames,
            block_size: self.block_size,
            sample_rate,
            time,
        };
        slot_alive.iter_mut().for_each(|a| *a = false);
        let base = self.buf.as_mut_ptr();
        let mut off = self.records_start;
        loop {
            let skip = unsafe { read_u32_ptr(base, off) };
            if skip == TERMINATOR {
                break;
            }
            if skip != 0 {
                off = skip as usize;
                continue;
            }
            let fn_index = unsafe { read_u32_ptr(base, off + 4) };
            let state_size = unsafe { read_u32_ptr(base, off + 8) } as usize;
            let num_inputs = unsafe { read_u32_ptr(base, off + 12) };
            let num_outputs = unsafe { read_u32_ptr(base, off + 16) };

            let state_off = align_up(off + HEADER_WORDS * 4, STATE_ALIGN);
            let inputs_off = align_up(state_off + state_size, 4);
            let outputs_off = align_up(inputs_off + num_inputs as usize * 4, BUF_ALIGN);

            let rec = Record {
                state: state_off as u32,
                inputs: inputs_off as u32,
                num_inputs,
                outputs: outputs_off as u32,
                num_outputs,
            };
            let tail = unsafe { fns[fn_index as usize](base, &ctx, &rec) };

            if matches!(tail, Tail::Active)
                && off >= self.first_voice_off
                && off < self.terminator_off
            {
                // Which voice is this record? Find its module block, then the slot within it.
                for m in 0..self.voice_block_base.len() {
                    let base = self.voice_block_base[m];
                    let span = self.voice_stride[m] * self.max_voices;
                    if off >= base && off < base + span {
                        slot_alive[(off - base) / self.voice_stride[m]] = true;
                        break;
                    }
                }
            }

            let rec_end = outputs_off + num_outputs as usize * self.block_size * F32;
            off = align_up(rec_end, RECORD_ALIGN);
        }
    }
}

/// Pack a `Copy` POD value into state bytes for a [`RecordSpec`].
pub fn state_bytes<T: Copy>(value: T) -> Vec<u8> {
    let size = std::mem::size_of::<T>();
    let mut v = vec![0u8; size];
    unsafe {
        std::ptr::copy_nonoverlapping(&value as *const T as *const u8, v.as_mut_ptr(), size);
    }
    v
}

fn write_u32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_ne_bytes());
}

#[inline]
unsafe fn read_u32_ptr(base: *const u8, off: usize) -> u32 {
    unsafe {
        let mut bytes = [0u8; 4];
        std::ptr::copy_nonoverlapping(base.add(off), bytes.as_mut_ptr(), 4);
        u32::from_ne_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ConstState {
        value: f32,
    }

    unsafe fn const_fn(base: *mut u8, ctx: &TickCtx, rec: &Record) -> Tail {
        unsafe {
            let value = state_mut::<ConstState>(base, rec).value;
            output(base, rec, 0, ctx.block_size, ctx.frames).fill(value);
        }
        Tail::Done
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GainState {
        gain: f32,
    }

    unsafe fn gain_fn(base: *mut u8, ctx: &TickCtx, rec: &Record) -> Tail {
        unsafe {
            let gain = state_mut::<GainState>(base, rec).gain;
            let inp = input(base, rec, 0, ctx.frames);
            let out = output(base, rec, 0, ctx.block_size, ctx.frames);
            for (o, &i) in out.iter_mut().zip(inp) {
                *o = i * gain;
            }
        }
        Tail::Done
    }

    #[test]
    fn const_record_fills_output() {
        let fns: &[ProcessFn] = &[const_fn];
        let records = vec![RecordSpec {
            fn_index: 0,
            state: state_bytes(ConstState { value: 0.5 }),
            inputs: vec![],
            num_outputs: 1,
        }];
        let mut plan = Plan::build(4, &records);
        plan.run(fns, 44100.0, 0.0, 4);
        assert_eq!(plan.buffer_at(plan.output_offset(0, 0), 4), &[0.5; 4]);
    }

    #[test]
    fn input_resolves_to_producer_output() {
        let fns: &[ProcessFn] = &[const_fn, gain_fn];
        let records = vec![
            RecordSpec {
                fn_index: 0,
                state: state_bytes(ConstState { value: 2.0 }),
                inputs: vec![],
                num_outputs: 1,
            },
            RecordSpec {
                fn_index: 1,
                state: state_bytes(GainState { gain: 3.0 }),
                inputs: vec![Source::Port(0, 0)],
                num_outputs: 1,
            },
        ];
        let mut plan = Plan::build(8, &records);
        plan.run(fns, 44100.0, 0.0, 8);
        assert_eq!(plan.buffer_at(plan.output_offset(1, 0), 8), &[6.0; 8]); // 2.0 * 3.0
    }

    #[test]
    fn voices_are_isolated_and_summed_over_active_slots() {
        // Each voice runs one `gain` record reading its slot's source output (port 0) times 2.
        let fns: &[ProcessFn] = &[gain_fn];
        let voice = vec![VoiceRecordSpec {
            fn_index: 0,
            state: state_bytes(GainState { gain: 2.0 }),
            inputs: vec![VoiceSource::SourceOut(0)],
            num_outputs: 1,
        }];
        let mut plan = VoicedPlan::build(4, &[], 1, &voice, 3);
        let mut alive = [false; 3];

        // Activate two voices with different source pitches.
        plan.set_slot_active(0, true);
        plan.set_slot_active(1, true);
        plan.source_output_mut(0, 0, 4).fill(2.0);
        plan.source_output_mut(1, 0, 4).fill(3.0);

        plan.run(fns, 44100.0, 0.0, 4, &mut alive);

        // Isolation: each slot's record sees only its own source output.
        assert_eq!(plan.voice_output(0, 0, 0, 4), &[4.0; 4]); // 2.0 * 2
        assert_eq!(plan.voice_output(1, 0, 0, 4), &[6.0; 4]); // 3.0 * 2

        // Sum over active slots = 4 + 6 = 10. Slot 2 is disabled (not summed).
        let sum: f32 = (0..3)
            .map(|s| plan.voice_output(s, 0, 0, 4)[0])
            .zip([true, true, false])
            .map(|(v, active)| if active { v } else { 0.0 })
            .sum();
        assert_eq!(sum, 10.0);

        // Disable slot 1: the walker jumps it, so its output is stale but not summed; slot 0 runs.
        plan.set_slot_active(1, false);
        plan.source_output_mut(0, 0, 4).fill(5.0);
        plan.run(fns, 44100.0, 0.0, 4, &mut alive);
        assert_eq!(plan.voice_output(0, 0, 0, 4), &[10.0; 4]); // 5.0 * 2, slot 0 re-ran
    }

    #[test]
    fn voice_chain_reads_same_slot_upstream() {
        // Two voice records form a chain: A = source*2, B = A*3. With records grouped by module
        // (A0 A1 A2, then B0 B1 B2), voice K's B must read voice K's A — never another voice's.
        let fns: &[ProcessFn] = &[gain_fn];
        let voice = vec![
            VoiceRecordSpec {
                fn_index: 0,
                state: state_bytes(GainState { gain: 2.0 }),
                inputs: vec![VoiceSource::SourceOut(0)],
                num_outputs: 1,
            },
            VoiceRecordSpec {
                fn_index: 0,
                state: state_bytes(GainState { gain: 3.0 }),
                inputs: vec![VoiceSource::Voice(0, 0)], // reads record A's output, same slot
                num_outputs: 1,
            },
        ];
        let mut plan = VoicedPlan::build(4, &[], 1, &voice, 3);
        let mut alive = [false; 3];

        plan.set_slot_active(0, true);
        plan.set_slot_active(1, true);
        plan.source_output_mut(0, 0, 4).fill(1.0);
        plan.source_output_mut(1, 0, 4).fill(10.0);

        plan.run(fns, 44100.0, 0.0, 4, &mut alive);

        // A: 1*2=2 and 10*2=20.  B: reads same slot's A, *3 => 6 and 60 (no cross-voice bleed).
        assert_eq!(plan.voice_output(0, 0, 0, 4), &[2.0; 4]);
        assert_eq!(plan.voice_output(1, 0, 0, 4), &[20.0; 4]);
        assert_eq!(plan.voice_output(0, 1, 0, 4), &[6.0; 4]);
        assert_eq!(plan.voice_output(1, 1, 0, 4), &[60.0; 4]);
    }

    #[test]
    fn unconnected_input_reads_zero() {
        let fns: &[ProcessFn] = &[gain_fn];
        let records = vec![RecordSpec {
            fn_index: 0,
            state: state_bytes(GainState { gain: 3.0 }),
            inputs: vec![Source::Zero],
            num_outputs: 1,
        }];
        let mut plan = Plan::build(4, &records);
        plan.run(fns, 44100.0, 0.0, 4);
        assert_eq!(plan.buffer_at(plan.output_offset(0, 0), 4), &[0.0; 4]); // 0 * 3 = 0
    }
}
