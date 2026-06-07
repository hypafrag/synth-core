//! Shared processing types.
//!
//! The module model lives in `module.rs` (behavior/state) and the runtime layout in `plan.rs`
//! (`docs/architecture/06-processing-plan.md`, `10-module-contract.md`). This module holds the
//! one type both share.

/// Whether a module is still producing output that must keep its voice alive.
///
/// A released voice is freed once no module in it reports `Active` (`docs/architecture/
/// 09-polyphony.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    Active,
    Done,
}
