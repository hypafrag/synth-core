//! The pipeline model: the serializable patch (nodes, wires, prefabs, layout).
//!
//! This mirrors `docs/architecture/10-pipeline-model.md`. It is pure data — no engine or
//! audio concepts — and (de)serializes to YAML.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A patch: the user-authored graph plus optional reusable prefabs and editor layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefabs: Vec<Prefab>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub wires: Vec<Wire>,
    /// Optional editor layout: node id -> [x, y] relative to canvas center. Omitted for headless.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layout: BTreeMap<String, [f64; 2]>,
}

fn default_version() -> u32 {
    1
}

/// One module instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    /// Names a built-in module or a prefab.
    #[serde(rename = "type")]
    pub ty: String,
    /// Non-signal configuration. Constants are never stored here — wire a `const_generator`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, ParamValue>,
}

/// A non-signal parameter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl ParamValue {
    /// Numeric value as f64 (`Int` or `Float`).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ParamValue::Int(i) => Some(*i as f64),
            ParamValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Numeric value as i64 (`Int`, or a truncated `Float`).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ParamValue::Int(i) => Some(*i),
            ParamValue::Float(f) => Some(*f as i64),
            _ => None,
        }
    }
}

/// A node's parameter map.
pub type Params = BTreeMap<String, ParamValue>;

/// A wire from one output port to one input port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wire {
    pub from: Endpoint,
    pub to: Endpoint,
}

/// A `[node, port]` reference. Serializes as a two-element YAML sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint(pub String, pub String);

impl Endpoint {
    pub fn node(&self) -> &str {
        &self.0
    }
    pub fn port(&self) -> &str {
        &self.1
    }
}

/// A reusable composite module: a named subgraph instantiated like a built-in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prefab {
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub wires: Vec<Wire>,
    #[serde(default)]
    pub exposed: Vec<Exposed>,
}

/// A port a prefab presents outward; `reference` points at an internal node port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exposed {
    pub name: String,
    pub reference: Endpoint,
}

/// Error parsing or emitting a patch.
#[derive(Debug)]
pub struct PatchError(String);

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "patch error: {}", self.0)
    }
}

impl std::error::Error for PatchError {}

impl Patch {
    /// Parse a patch from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self, PatchError> {
        serde_yaml_ng::from_str(yaml).map_err(|e| PatchError(e.to_string()))
    }

    /// Serialize a patch to YAML.
    pub fn to_yaml(&self) -> Result<String, PatchError> {
        serde_yaml_ng::to_string(self).map_err(|e| PatchError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONST_TONE: &str = r#"
version: 1
nodes:
  - id: freq
    type: const_generator
    params: { value: 440.0 }
  - id: amp
    type: const_generator
    params: { value: 0.5 }
  - id: osc1
    type: sine_oscillator
  - id: out1
    type: audio_output
    params: { device: default, channels: 2 }
wires:
  - { from: [freq, out], to: [osc1, frequency] }
  - { from: [amp,  out], to: [osc1, amplitude] }
  - { from: [osc1, out], to: [out1, ch0] }
  - { from: [osc1, out], to: [out1, ch1] }
layout:
  freq: [-160, -40]
  amp:  [-160, 40]
  osc1: [0, 0]
  out1: [160, 0]
"#;

    #[test]
    fn parses_const_tone() {
        let patch = Patch::from_yaml(CONST_TONE).expect("parse");
        assert_eq!(patch.version, 1);
        assert_eq!(patch.nodes.len(), 4);
        assert_eq!(patch.wires.len(), 4);
        assert_eq!(patch.layout.len(), 4);

        let freq = &patch.nodes[0];
        assert_eq!(freq.id, "freq");
        assert_eq!(freq.ty, "const_generator");
        assert_eq!(freq.params.get("value"), Some(&ParamValue::Float(440.0)));

        let out = &patch.nodes[3];
        assert_eq!(out.params.get("channels"), Some(&ParamValue::Int(2)));
        assert_eq!(
            out.params.get("device"),
            Some(&ParamValue::Str("default".to_string()))
        );

        let w = &patch.wires[0];
        assert_eq!(w.from.node(), "freq");
        assert_eq!(w.from.port(), "out");
        assert_eq!(w.to.node(), "osc1");
        assert_eq!(w.to.port(), "frequency");

        assert_eq!(patch.layout.get("osc1"), Some(&[0.0, 0.0]));
    }

    #[test]
    fn parses_prefab() {
        let yaml = r#"
prefabs:
  - name: voice
    nodes:
      - id: osc
        type: sine_oscillator
      - id: vca
        type: vca
    wires:
      - { from: [osc, out], to: [vca, in] }
    exposed:
      - { name: pitch, reference: [osc, frequency] }
      - { name: out,   reference: [vca, out] }
nodes:
  - id: v1
    type: voice
"#;
        let patch = Patch::from_yaml(yaml).expect("parse");
        assert_eq!(patch.version, 1); // defaulted
        assert_eq!(patch.prefabs.len(), 1);
        let voice = &patch.prefabs[0];
        assert_eq!(voice.name, "voice");
        assert_eq!(voice.nodes.len(), 2);
        assert_eq!(voice.exposed[0].name, "pitch");
        assert_eq!(voice.exposed[0].reference.node(), "osc");
        assert_eq!(voice.exposed[0].reference.port(), "frequency");
        assert_eq!(patch.nodes[0].ty, "voice");
    }

    #[test]
    fn round_trip() {
        let patch = Patch::from_yaml(CONST_TONE).expect("parse");
        let yaml = patch.to_yaml().expect("emit");
        let again = Patch::from_yaml(&yaml).expect("reparse");
        assert_eq!(patch, again);
    }
}
