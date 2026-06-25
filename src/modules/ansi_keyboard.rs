//! `ansi_keyboard` — a polyphonic voice source driven by the computer (ANSI) keyboard.
//!
//! Maps the home row (ASDF...) to natural notes and the QWERTY row to semitones, mirroring a
//! piano layout.  The QWERTY row is physically shifted ~0.5 key-widths left relative to the home
//! row on a standard ANSI keyboard, so W falls between A and S (→ C#), E between S and D (→ D#),
//! R between D and F (no black key at the E–F boundary → unvoiced), T between F and G (→ F#),
//! and so on.  Q and I are similarly unvoiced (left of C and at the B–C boundary).
//!
//! Number keys 1–9 select the base octave (default 4 → A key = C4 = middle C, H key = A4 = 440 Hz).
//! Shift transposes new notes +1 octave; Ctrl transposes −1 octave; both together cancel out.
//! Already-held notes keep their original pitch regardless of modifier changes.
//!
//! **Mode (required `toggle` bool param, no default — each host states it):**
//! - `toggle: true` — the backtick/grave key (`) enables and disables keyboard reading, starting
//!   off. While enabled, terminal echo is suppressed so key names don't appear in the terminal
//!   output; echo is restored when disabled or when the module is dropped. This is the CLI mode.
//! - `toggle: false` — reads continuously (starts enabled) and the backtick key is ignored. GUI
//!   hosts use this, since they gate audio with their own play/stop control. (Echo suppression is
//!   already a no-op in non-tty contexts.)
//!
//! Uses `device_query` to poll OS-level key state once per audio block — works identically in
//! CLI and GUI hosts.  On macOS the OS prompts for Accessibility permission the first time a
//! patch containing this module is run.

use std::collections::{HashMap, HashSet};

use device_query::{DeviceQuery, DeviceState, Keycode};

use crate::model::Params;
use crate::module::{
    Inputs, ModuleDesc, OsPermission, PolyphonicModule, PortDesc, SourceCtx, SourceError,
    SourceType, VoiceId,
};
use crate::processing::Tail;

const DEFAULT_OCTAVE: i32 = 4;

fn note_to_hz(note: u8) -> f32 {
    440.0 * 2f32.powf((note as f32 - 69.0) / 12.0)
}

/// Semitone offset from C for each voiced key.  `None` = unvoiced (Q, R, I — piano gaps).
///
/// Home row covers C through E one octave up; QWERTY row fills in the black keys between them,
/// leaving Q (left of C), R (E–F boundary), and I (B–C boundary) silent.
fn key_to_semitone(key: Keycode) -> Option<i32> {
    match key {
        // Home row: natural notes (white keys)
        Keycode::A => Some(0),          // C
        Keycode::S => Some(2),          // D
        Keycode::D => Some(4),          // E
        Keycode::F => Some(5),          // F
        Keycode::G => Some(7),          // G
        Keycode::H => Some(9),          // A
        Keycode::J => Some(11),         // B
        Keycode::K => Some(12),         // C  (octave + 1)
        Keycode::L => Some(14),         // D  (octave + 1)
        Keycode::Semicolon => Some(16), // E  (octave + 1)
        // QWERTY row: semitones (black keys); Q, R, I sit at piano gaps → unvoiced
        Keycode::W => Some(1),          // C#
        Keycode::E => Some(3),          // D#
        // R → E–F boundary: no black key
        Keycode::T => Some(6),          // F#
        Keycode::Y => Some(8),          // G#
        Keycode::U => Some(10),         // A#
        // I → B–C boundary: no black key
        Keycode::O => Some(13),         // C# (octave + 1)
        Keycode::P => Some(15),         // D# (octave + 1)
        _ => None,
    }
}

fn key_to_octave(key: Keycode) -> Option<i32> {
    match key {
        Keycode::Key1 => Some(1),
        Keycode::Key2 => Some(2),
        Keycode::Key3 => Some(3),
        Keycode::Key4 => Some(4),
        Keycode::Key5 => Some(5),
        Keycode::Key6 => Some(6),
        Keycode::Key7 => Some(7),
        Keycode::Key8 => Some(8),
        Keycode::Key9 => Some(9),
        _ => None,
    }
}

/// MIDI note for a semitone offset in the given octave.  C in octave n = (n+1)*12, so C4 = 60.
fn midi_note(octave: i32, semitone: i32) -> u8 {
    ((octave + 1) * 12 + semitone).clamp(0, 127) as u8
}

struct HeldVoice {
    voice: VoiceId,
    /// Captured at note-on so release is correct even if the octave changes while the key is held.
    note: u8,
}

pub struct AnsiKeyboard {
    device: DeviceState,
    prev_keys: HashSet<Keycode>,
    octave: i32,
    /// Keyed by physical key so release works correctly across octave and modifier changes.
    held: HashMap<Keycode, HeldVoice>,
    /// Whether keyboard reading is active.  When `toggle` is set, the grave/backtick key flips this.
    enabled: bool,
    /// Whether the backtick/grave key toggles `enabled`. When false the keyboard reads continuously
    /// and the grave key is ignored. Set from the required `toggle` param (see module docs).
    toggle: bool,
    /// Terminal settings saved on enable, restored on disable or drop (Unix only).
    #[cfg(unix)]
    saved_termios: Option<libc::termios>,
}

impl Drop for AnsiKeyboard {
    fn drop(&mut self) {
        #[cfg(unix)]
        restore_echo(&mut self.saved_termios);
    }
}

impl PolyphonicModule for AnsiKeyboard {
    fn process(&mut self, ctx: &mut SourceCtx) -> Tail {
        let frames = ctx.frames;
        let current: HashSet<Keycode> = self.device.get_keys().into_iter().collect();

        // Collect edge sets up front so we don't borrow `current` and `prev_keys` simultaneously.
        let pressed: Vec<Keycode> = current.difference(&self.prev_keys).copied().collect();
        let released: Vec<Keycode> = self.prev_keys.difference(&current).copied().collect();

        // Toggle on the press edge of grave/backtick (only in toggle mode).
        if self.toggle && pressed.contains(&Keycode::Grave) {
            self.enabled = !self.enabled;
            if self.enabled {
                #[cfg(unix)]
                suppress_echo(&mut self.saved_termios);
            } else {
                #[cfg(unix)]
                restore_echo(&mut self.saved_termios);
                // Release all held voices so notes don't sustain while the keyboard is inactive.
                for (_, held) in self.held.drain() {
                    ctx.voice_output(held.voice, 1)[..frames].fill(0.0);
                    ctx.release(held.voice);
                }
            }
        }

        // Always update prev_keys so re-enabling has a clean baseline (no spurious note-ons for
        // keys that were already held before the toggle).
        self.prev_keys = current;

        if !self.enabled {
            return Tail::Active;
        }

        let current = &self.prev_keys; // reborrow after move

        // Octave: update on the press edge of any number key.
        for &key in &pressed {
            if let Some(oct) = key_to_octave(key) {
                self.octave = oct;
            }
        }

        // Modifier offset: Shift = +1 octave, Ctrl = −1, both = 0 (cancel).
        // Checked against `current` so a modifier pressed in the same block as a note key works.
        let shift = current.contains(&Keycode::LShift) || current.contains(&Keycode::RShift);
        let ctrl = current.contains(&Keycode::LControl) || current.contains(&Keycode::RControl);
        let octave_offset: i32 = match (shift, ctrl) {
            (true, false) => 1,
            (false, true) => -1,
            _ => 0,
        };
        let effective_octave = self.octave + octave_offset;

        // Note-on: newly pressed note keys.
        for &key in &pressed {
            let Some(semitone) = key_to_semitone(key) else { continue };
            let note = midi_note(effective_octave, semitone);
            // Re-trigger: release the old voice on this key first.
            if let Some(old) = self.held.remove(&key) {
                ctx.voice_output(old.voice, 1)[..frames].fill(0.0);
                ctx.release(old.voice);
            }
            if let Some(voice) = ctx.allocate() {
                self.held.insert(key, HeldVoice { voice, note });
            }
        }

        // Note-off: released note keys.
        for &key in &released {
            if let Some(held) = self.held.remove(&key) {
                ctx.voice_output(held.voice, 1)[..frames].fill(0.0);
                ctx.release(held.voice);
            }
        }

        // Refresh outputs for every held voice.
        for held in self.held.values() {
            ctx.voice_output(held.voice, 0)[..frames].fill(note_to_hz(held.note));
            ctx.voice_output(held.voice, 1)[..frames].fill(1.0);
        }

        Tail::Active
    }
}

/// Disable terminal echo and save the original settings.  No-op if stdin is not a tty.
#[cfg(unix)]
fn suppress_echo(saved: &mut Option<libc::termios>) {
    unsafe {
        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
            return;
        }
        *saved = Some(t);
        t.c_lflag &= !(libc::ECHO | libc::ECHONL);
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
    }
}

/// Restore terminal settings saved by [`suppress_echo`].
#[cfg(unix)]
fn restore_echo(saved: &mut Option<libc::termios>) {
    if let Some(t) = saved.take() {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) };
    }
}

pub struct AnsiKeyboardType;

impl SourceType for AnsiKeyboardType {
    type Module = AnsiKeyboard;
    const ICON: crate::module::Icon = [
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
        0b00011111111111111111111111111000,
        0b00010000000000000000000000001000,
        0b00010000000000000000000000001000,
        0b00010011001100110011001100001000,
        0b00010000000000000000000000001000,
        0b00010000000000000000000000001000,
        0b00010011001100110011001100001000,
        0b00010000000000000000000000001000,
        0b00010000000000000000000000001000,
        0b00010011001100110011001100001000,
        0b00010000000000000000000000001000,
        0b00010000000000000000000000001000,
        0b00011111111111111111111111111000,
        0b00000000000000000000000000000000,
        0b00000000000000000000000000000000,
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
            inputs: Inputs::Fixed(vec![]),
            outputs: vec![
                PortDesc::sample("pitch"),
                PortDesc::sample("gate"),
            ],
        }
    }

    fn make(params: &Params) -> Result<AnsiKeyboard, SourceError> {
        // Required, no default: the host must state whether the backtick key toggles reading.
        let toggle = params
            .get("toggle")
            .and_then(crate::model::ParamValue::as_bool)
            .ok_or_else(|| {
                SourceError::Other(
                    "ansi_keyboard requires a `toggle` bool param (true = backtick toggles \
                     reading, false = always on)"
                        .to_string(),
                )
            })?;
        let device = DeviceState::checked_new()
            .ok_or(SourceError::PermissionDenied(OsPermission::Accessibility))?;
        Ok(AnsiKeyboard {
            device,
            prev_keys: HashSet::new(),
            octave: DEFAULT_OCTAVE,
            held: HashMap::new(),
            // In toggle mode start off (backtick turns it on); otherwise read continuously.
            enabled: !toggle,
            toggle,
            #[cfg(unix)]
            saved_termios: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_c_layout() {
        // Default octave 4: A = C4 = MIDI 60 (middle C).
        assert_eq!(midi_note(DEFAULT_OCTAVE, key_to_semitone(Keycode::A).unwrap()), 60);
        // W = C#4 = MIDI 61.
        assert_eq!(midi_note(DEFAULT_OCTAVE, key_to_semitone(Keycode::W).unwrap()), 61);
        // H = A4 = MIDI 69 = 440 Hz.
        assert_eq!(midi_note(DEFAULT_OCTAVE, key_to_semitone(Keycode::H).unwrap()), 69);
        // Semicolon = E5 = MIDI 76.
        assert_eq!(midi_note(DEFAULT_OCTAVE, key_to_semitone(Keycode::Semicolon).unwrap()), 76);
    }

    #[test]
    fn piano_gaps_are_unvoiced() {
        assert!(key_to_semitone(Keycode::Q).is_none()); // left of C
        assert!(key_to_semitone(Keycode::R).is_none()); // E–F boundary
        assert!(key_to_semitone(Keycode::I).is_none()); // B–C boundary
    }

    #[test]
    fn grave_is_not_a_note() {
        // Grave is the enable/disable toggle, never a note.
        assert!(key_to_semitone(Keycode::Grave).is_none());
    }

    #[test]
    fn octave_keys() {
        assert_eq!(key_to_octave(Keycode::Key1), Some(1));
        assert_eq!(key_to_octave(Keycode::Key4), Some(4));
        assert_eq!(key_to_octave(Keycode::Key9), Some(9));
        assert_eq!(key_to_octave(Keycode::A), None);
    }

    #[test]
    fn note_to_hz_a440() {
        assert!((note_to_hz(69) - 440.0).abs() < 1e-3);
    }

    #[test]
    fn octave_shift_changes_note() {
        assert_eq!(midi_note(3, key_to_semitone(Keycode::A).unwrap()), 48); // C3
        assert_eq!(midi_note(5, key_to_semitone(Keycode::A).unwrap()), 72); // C5
    }

    #[test]
    fn modifier_octave_offset() {
        let base = key_to_semitone(Keycode::A).unwrap(); // C, semitone 0
        assert_eq!(midi_note(DEFAULT_OCTAVE + 1, base), 72); // Shift: C5
        assert_eq!(midi_note(DEFAULT_OCTAVE - 1, base), 48); // Ctrl:  C3
        assert_eq!(midi_note(DEFAULT_OCTAVE, base), 60);     // Both:  C4
    }
}
