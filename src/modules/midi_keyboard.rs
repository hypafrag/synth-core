//! `midi_keyboard` — a polyphonic voice source driven by a MIDI input device.
//!
//! A single shared instance (built off the audio thread) holds the MIDI connection and a
//! lock-free queue. Its MIDI callback pushes note events into the queue; `process` drains them
//! each block, allocating a voice per held note and writing that voice's `pitch` (Hz), `gate`,
//! and `velocity` outputs. See `docs/architecture/10-module-contract.md` (voice sources).

use std::collections::HashMap;

use midir::{MidiInput, MidiInputConnection};
use rtrb::{Consumer, RingBuffer};

use crate::model::Params;
use crate::module::{ModuleDesc, PolyphonicModule, PortDesc, SourceCtx, SourceError, SourceType, VoiceId};
use crate::processing::Tail;

const QUEUE_CAPACITY: usize = 1024;

/// A note event from the MIDI callback (POD, queued lock-free).
#[derive(Clone, Copy)]
struct NoteEvent {
    note: u8,
    velocity: u8,
    on: bool,
}

/// Convert a MIDI note number to frequency in Hz (A4 = note 69 = 440 Hz, equal temperament).
fn note_to_hz(note: u8) -> f32 {
    440.0 * 2f32.powf((note as f32 - 69.0) / 12.0)
}

struct HeldVoice {
    voice: VoiceId,
    velocity: f32,
}

pub struct MidiKeyboard {
    /// Kept alive so the callback keeps running; `None` if no device could be opened.
    _conn: Option<MidiInputConnection<()>>,
    events: Consumer<NoteEvent>,
    /// note number -> the voice currently playing it.
    held: HashMap<u8, HeldVoice>,
}

impl PolyphonicModule for MidiKeyboard {
    fn process(&mut self, ctx: &mut SourceCtx) -> Tail {
        let frames = ctx.frames;

        // Drain MIDI events. (Sample-accurate placement within the block is future work; for now
        // an event takes effect at the start of the block it is observed in.)
        while let Ok(ev) = self.events.pop() {
            if ev.on {
                // Re-triggering a held note: release the old voice first.
                if let Some(old) = self.held.remove(&ev.note) {
                    ctx.voice_output(old.voice, 1)[..frames].fill(0.0);
                    ctx.release(old.voice);
                }
                if let Some(voice) = ctx.allocate() {
                    self.held.insert(
                        ev.note,
                        HeldVoice {
                            voice,
                            velocity: ev.velocity as f32 / 127.0,
                        },
                    );
                }
                // else: all voices in use — note dropped (engine growth is future work).
            } else if let Some(held) = self.held.remove(&ev.note) {
                // Note-off: write gate 0 once and release; the engine keeps the voice alive
                // through its tail, reading the held pitch / gate 0 that stay in the buffer.
                ctx.voice_output(held.voice, 1)[..frames].fill(0.0);
                ctx.release(held.voice);
            }
        }

        // Refresh every held voice's outputs (held buffers persist, but writing keeps them
        // correct across plan rebuilds and is cheap).
        for (&note, held) in &self.held {
            let pitch = note_to_hz(note);
            let velocity = held.velocity;
            let voice = held.voice;
            ctx.voice_output(voice, 0)[..frames].fill(pitch);
            ctx.voice_output(voice, 1)[..frames].fill(1.0);
            ctx.voice_output(voice, 2)[..frames].fill(velocity);
        }

        Tail::Active
    }
}

/// Open a MIDI input connection, selecting a port by substring of the `port` param (first port
/// if unset/empty). On any failure returns a keyboard with no connection (silent) rather than
/// failing the build, so a patch loads even with no MIDI device attached.
fn open(params: &Params) -> MidiKeyboard {
    let (producer, events) = RingBuffer::<NoteEvent>::new(QUEUE_CAPACITY);
    let mut producer = producer;

    let conn = (|| {
        let midi_in = MidiInput::new("synth-midi-keyboard").ok()?;
        let ports = midi_in.ports();
        let want = params.get("port").and_then(|v| match v {
            crate::model::ParamValue::Str(s) => Some(s.as_str()),
            _ => None,
        });
        let port = ports.iter().find(|p| match want {
            Some(w) if !w.is_empty() => midi_in
                .port_name(p)
                .map(|n| n.contains(w))
                .unwrap_or(false),
            _ => true,
        })?;
        let port = port.clone();
        midi_in
            .connect(
                &port,
                "synth-midi-in",
                move |_timestamp, message, _| {
                    if message.len() < 3 {
                        return;
                    }
                    let status = message[0] & 0xF0;
                    let note = message[1];
                    let velocity = message[2];
                    let event = match status {
                        0x90 => NoteEvent {
                            note,
                            velocity,
                            on: velocity > 0,
                        },
                        0x80 => NoteEvent {
                            note,
                            velocity,
                            on: false,
                        },
                        _ => return,
                    };
                    let _ = producer.push(event);
                },
                (),
            )
            .ok()
    })();

    MidiKeyboard {
        _conn: conn,
        events,
        held: HashMap::new(),
    }
}

/// The registered type: `pitch`/`gate`/`velocity` outputs, no inputs.
pub struct MidiKeyboardType;

impl SourceType for MidiKeyboardType {
    type Module = MidiKeyboard;
    const ICON: crate::module::Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00011111111111111111111111111000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010011010011010011010011001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00010000010000010000010000001000,
        0b00011111111111111111111111111000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
    ];

    fn describe(_params: &Params) -> ModuleDesc {
        ModuleDesc {
            inputs: vec![],
            outputs: vec![
                PortDesc::sample("pitch"),
                PortDesc::sample("gate"),
                PortDesc::sample("velocity"),
            ],
        }
    }

    fn make(params: &Params) -> Result<MidiKeyboard, SourceError> {
        Ok(open(params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_69_is_a440() {
        assert!((note_to_hz(69) - 440.0).abs() < 1e-3);
        assert!((note_to_hz(81) - 880.0).abs() < 1e-3); // one octave up
        assert!((note_to_hz(57) - 220.0).abs() < 1e-3); // one octave down
    }

    #[test]
    fn builds_without_a_device() {
        // make() must never fail the build even with no MIDI hardware.
        let kb = MidiKeyboardType::make(&Params::new()).unwrap();
        assert!(kb.held.is_empty());
    }
}
