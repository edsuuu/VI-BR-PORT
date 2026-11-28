//! The title's ray walks, read in the console code: where a program walks
//! the console's ray-tracing structure in software, and what a ray query of
//! the host would have to stand for there (milestone M3a of the native
//! ray-tracing design).
//!
//! - [`report`] is `translate --walk-report`, the census of the walks; it
//!   changes no translation.
//! - With `VICTORY_RT_QUERY` naming a family ([`families`]), the walks of that
//!   family the census finds fit are taken out of the program before it is
//!   translated ([`take_out`], `replace.rs`), each as one instruction the
//!   lowering makes a host ray query of (`query.rs`). Off by default: every
//!   translation is then the same as without this module.

mod cfg;
#[cfg(all(test, windows))]
mod query_tests;
pub mod recognize;
pub(crate) mod replace;

pub use recognize::{ExitKind, Family, Mold, Walk, recognize};

use crate::ir::Program;
use crate::native::{HwStage, NativeProgram};
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

/// The families whose walks are taken out of the programs and made host ray
/// queries: `VICTORY_RT_QUERY`, a list of family names (`a0`, the only one
/// taken out so far; the others are accepted and do nothing yet), or `all`.
/// Off (empty) by default. `VICTORY_RT_MISS=1` wins: nothing is taken out.
/// A program so translated needs the host's structure bound where the
/// title's was (the user-data dword `USER_STRUCTURE`, `shader.rs`). Part of
/// the translation: the shader cache keys on it.
pub fn families() -> &'static [Family] {
    static ON: std::sync::OnceLock<Vec<Family>> = std::sync::OnceLock::new();
    ON.get_or_init(|| parse_families(std::env::var("VICTORY_RT_QUERY").ok().as_deref()))
}

/// `VICTORY_RT_QUERY`'s families. Pure, for testing.
fn parse_families(value: Option<&str>) -> Vec<Family> {
    let Some(value) = value else {
        return Vec::new();
    };
    if value.trim().eq_ignore_ascii_case("all") {
        return vec![Family::A0, Family::AF, Family::AA, Family::B0, Family::BF, Family::BA];
    }
    let mut out: Vec<Family> = value.split(',').filter_map(Family::parse).collect();
    out.dedup();
    out
}

thread_local! {
    /// See [`with_queries`].
    static QUERIES: std::cell::RefCell<Vec<replace::Query>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The program `p` (a compute program, `native`) with the walks
/// [`families`] names taken out, and the queries that stand for them; `None`
/// when the switch is off or no walk is taken out.
pub(crate) fn take_out(native: &NativeProgram, p: &Program, st: &Structured) -> Option<(Program, Vec<replace::Query>)> {
    let families = families();
    if families.is_empty() || native.stage != HwStage::Cs || crate::emit::rt_miss() {
        return None;
    }
    replace::replace(p, st, families)
}

/// Runs `f` (a translation of a program [`take_out`] made) with the queries
/// that stand for its walks, which the lowering finds by their sites'
/// offsets ([`query_at`]).
pub(crate) fn with_queries<R>(queries: Vec<replace::Query>, f: impl FnOnce() -> R) -> R {
    let was = QUERIES.replace(queries);
    let r = f();
    QUERIES.set(was);
    r
}

/// The query that stands for the walk whose site is at byte `offset` of the
/// program being translated, if one does.
pub(crate) fn query_at(offset: usize) -> Option<replace::Query> {
    QUERIES.with_borrow(|q| q.iter().find(|q| q.offset == offset).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_switch_parses_families() {
        assert!(parse_families(None).is_empty());
        assert!(parse_families(Some("")).is_empty());
        assert_eq!(parse_families(Some("a0")), vec![Family::A0]);
        assert_eq!(parse_families(Some("A0, b0,zz")), vec![Family::A0, Family::B0]);
        assert_eq!(parse_families(Some("all")).len(), 6);
    }
}
