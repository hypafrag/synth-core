//! Audio output sink.

use crate::model::Params;
use crate::processing::{Module, ProcessCtx, Tail};

/// The output sink: one input per channel (`ch0`, `ch1`, …). It produces no signal of its
/// own — the engine reads this node's input buffers each block and writes them to the audio
/// device. The `sample_rate` param is read by the engine to configure the device and prepare
/// every module.
///
/// Params: `channels` (default 2), `sample_rate` (Hz, default 44100; read by the engine).
pub struct AudioOutput {
    channels: usize,
}

impl AudioOutput {
    pub fn new(channels: usize) -> Self {
        Self { channels }
    }

    pub fn from_params(params: &Params) -> Result<Self, String> {
        let channels = params.get("channels").and_then(|v| v.as_i64()).unwrap_or(2);
        if channels < 1 {
            return Err("audio_output `channels` must be >= 1".to_string());
        }
        Ok(Self::new(channels as usize))
    }

    pub fn channels(&self) -> usize {
        self.channels
    }
}

impl Module for AudioOutput {
    fn input_ports(&self) -> Vec<String> {
        (0..self.channels).map(|c| format!("ch{c}")).collect()
    }

    fn process(&mut self, _ctx: &mut ProcessCtx<'_>) -> Tail {
        // Sink: the engine reads this node's input buffers into the device.
        Tail::Done
    }
}
