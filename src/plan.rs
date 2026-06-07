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
