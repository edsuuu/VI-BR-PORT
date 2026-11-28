//! The title's ray walks, read in the console code: where a program walks
//! the console's ray-tracing structure in software, and what a ray query of
//! the host would have to stand for there (milestone M3a of the native
//! ray-tracing design).
//!
//! For now this only reads: [`report`] is `translate --walk-report`, the
//! census of the walks, and nothing it finds changes a translation.

mod cfg;
pub mod recognize;

pub use recognize::{ExitKind, Family, Mold, Walk, recognize};

use crate::ir::Program;
use crate::structure::Structured;

/// `translate --walk-report`: the report lines of one program, `label`
/// naming it (none for a program without node tests); `st` is its structure,
/// which the exit kind needs.
pub fn report(label: &str, p: &Program, st: Option<&Structured>) -> Vec<String> {
    let mut out = Vec::new();
    for (n, w) in recognize(p, st).iter().enumerate() {
        out.push(w.summary(label, n));
        out.extend(w.lines(label, n));
    }
    out
}
