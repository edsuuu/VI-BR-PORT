//! The title's tiled heap is seen twice: the title maps each of its pages at
//! a CPU address with `XMemMapPhysicalPagesScatter` (the physical page
//! numbers its own table holds) and, in the same call, at the GPU address
//! with `CopyPageMappingsBatchX` from its pool of pages (title+0xf2c470: the
//! scatter at +0xf2cc3e, the batch at +0xf2cc7f). On the console both
//! addresses show the same physical page. Here the GPU address takes the
//! page the pool shows at that index ([`crate::queue`]); if that is ever
//! another page than the one the CPU address got, what DirectStorage writes
//! (by the CPU address or by page number) lands in the CPU page and the GPU
//! address keeps the bytes of whoever had its page before: a texture whose
//! tiles show another texture, with nothing left to tell of.
//!
//! The CPU address is the GPU one less a distance found by vote: the other
//! addresses that show the pool page being mapped, just before the GPU
//! address is given it (the scatter came first). The distance that comes up
//! most is the CPU view's ([`DELTA`]).
//!
//! Each GPU page a batch maps is checked against its CPU twin, and counted;
//! `VICTORY_MAP_FROM_TWIN=1` (off by default) maps it from the twin instead
//! of the pool. `VICTORY_MAP_TRACE=1` (off by default) logs, for each
//! texture read from heap pages a batch mapped, which mapping it read.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};

const PAGE: usize = 64 << 10;

/// The GPU address of a heap page less its CPU twin's; 0 until known.
static DELTA: AtomicI64 = AtomicI64::new(0);

/// Votes for the distance while it is not known.
static VOTES: Mutex<Vec<(i64, u32)>> = Mutex::new(Vec::new());

/// Since the start: batch calls, pages mapped from the pool and checked,
/// of them the pool's page is the twin's, another, the twin shows nothing;
/// pages mapped from the twin (`VICTORY_MAP_FROM_TWIN=1`); pages checked
/// before the distance was known, and of them the pages whose GPU address
/// showed the pool page already (the CPU view at the same address).
pub(crate) static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
const CALLS: usize = 0;
const CHECKED: usize = 1;
const AGREE: usize = 2;
const DIFFER: usize = 3;
const EMPTY: usize = 4;
const FROM_TWIN: usize = 5;
const UNKNOWN: usize = 6;
const SAME: usize = 7;

/// The batch call that last changed each GPU page (by address).
static MAPPED_AT: Mutex<Option<HashMap<usize, u64>>> = Mutex::new(None);

/// Whether the GPU pages of a batch are mapped from their CPU twin when it
/// shows another page than the pool (`VICTORY_MAP_FROM_TWIN=1`; off by
/// default, the pool's page as before).
fn from_twin() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_MAP_FROM_TWIN").is_some_and(|v| v == "1"))
}

/// Whether textures read from pages a batch mapped are logged with the
/// mapping they read (`VICTORY_MAP_TRACE=1`; off by default).
pub(crate) fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_MAP_TRACE").is_some_and(|v| v == "1"))
}

/// The CPU twin of GPU page `page`, if the distance is known.
fn twin_of(page: usize) -> Option<usize> {
    let delta = DELTA.load(Relaxed);
    (delta != 0).then(|| (page as i64).wrapping_sub(delta) as usize)
}

/// Counts a vote for `delta`; whether it is now the distance: 64 votes or
/// more, and four times as many as any other.
fn vote(votes: &mut Vec<(i64, u32)>, delta: i64) -> bool {
    match votes.iter_mut().find(|v| v.0 == delta) {
        Some(v) => v.1 += 1,
        None => votes.push((delta, 1)),
    }
    let mut best = [0u32; 2];
    let mut top = 0;
    for &(d, n) in votes.iter() {
        if n > best[0] {
            best = [n, best[0]];
            top = d;
        } else if n > best[1] {
            best[1] = n;
        }
    }
    top == delta && best[0] >= 64 && best[0] >= 4 * best[1]
}

/// Which of the two shows what, for the check: the same page, another, or
/// nothing at the twin.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Seen {
    Agree,
    Differ,
    Empty,
}

fn compare(pool: Option<(usize, u64)>, twin: Option<(usize, u64)>) -> Seen {
    match (pool, twin) {
        (_, None) => Seen::Empty,
        (a, b) if a == b => Seen::Agree,
        _ => Seen::Differ,
    }
}

/// The ranges `(to, from, len)` a batch is about to map: each GPU page that
/// takes a pool page is checked against its CPU twin (or, while the
/// distance is not known, votes for it). Returns the ranges to map: as
/// given, or with the twin as the source of a page whose twin shows another
/// page ([`from_twin`]).
pub(crate) fn before(ranges: Vec<(u64, u64, u64)>) -> Vec<(u64, u64, u64)> {
    before_with(ranges, from_twin())
}

/// [`before`]; `swap`: [`from_twin`].
fn before_with(ranges: Vec<(u64, u64, u64)>, swap: bool) -> Vec<(u64, u64, u64)> {
    COUNTS[CALLS].fetch_add(1, Relaxed);
    let mut out: Vec<(u64, u64, u64)> = Vec::with_capacity(ranges.len());
    for (to, from, len) in ranges {
        if from == 0 {
            out.push((to, from, len));
            continue;
        }
        let pages: Vec<(u64, u64)> = (0..len)
            .step_by(PAGE)
            .map(|k| {
                let (page, source) = ((to + k) as usize, (from + k) as usize);
                let taken = match twin_of(page) {
                    None => {
                        COUNTS[UNKNOWN].fetch_add(1, Relaxed);
                        learn(page, source);
                        source
                    }
                    Some(twin) => check(page, source, twin, swap),
                };
                (page as u64, taken as u64)
            })
            .collect();
        // As given unless a page is taken from elsewhere.
        if pages.iter().zip((0..len).step_by(PAGE)).all(|(&(_, taken), k)| taken == from + k) {
            out.push((to, from, len));
        } else {
            out.extend(pages.into_iter().map(|(page, taken)| (page, taken, PAGE as u64)));
        }
    }
    out
}

/// A vote for each distance from `page` to another address that shows the
/// pool page `source` (the pool's own excepted).
fn learn(page: usize, source: usize) {
    let shown = crate::inplace::pages_of(source);
    let mut votes = VOTES.lock().unwrap_or_else(|e| e.into_inner());
    for at in shown {
        if at == page {
            COUNTS[SAME].fetch_add(1, Relaxed);
            continue;
        }
        if at == source {
            continue;
        }
        let delta = (page as i64).wrapping_sub(at as i64);
        if delta != 0 && vote(&mut votes, delta) && DELTA.compare_exchange(0, delta, Relaxed, Relaxed).is_ok() {
            tracing::info!(
                gpu = format_args!("{page:#x}"),
                cpu = format_args!("{at:#x}"),
                distance = format_args!("{delta:#x}"),
                "the title's tiled heap: its CPU view found (pages mapped by CopyPageMappingsBatchX are checked against it)"
            );
        }
    }
}

/// Checks GPU page `page` about to take pool page `source` against its CPU
/// twin; the source to map it from.
fn check(page: usize, source: usize, twin: usize, swap: bool) -> usize {
    COUNTS[CHECKED].fetch_add(1, Relaxed);
    let (pool, cpu) = (crate::inplace::page_view(source), crate::inplace::page_view(twin));
    let seen = compare(pool, cpu);
    COUNTS[match seen {
        Seen::Agree => AGREE,
        Seen::Differ => DIFFER,
        Seen::Empty => EMPTY,
    }]
    .fetch_add(1, Relaxed);
    if seen != Seen::Differ {
        return source;
    }
    let n = COUNTS[DIFFER].load(Relaxed);
    if n <= 16 || n.is_power_of_two() {
        tracing::warn!(
            gpu = format_args!("{page:#x}"),
            cpu = format_args!("{twin:#x}"),
            pool = format_args!("{source:#x}"),
            pool_shows = ?pool,
            cpu_shows = ?cpu,
            mapped_from = if swap { "the CPU view" } else { "the pool" },
            n,
            "a page of the tiled heap would show another page than its CPU view (VICTORY_MAP_FROM_TWIN=1: mapped from the CPU view)"
        );
    }
    if swap {
        COUNTS[FROM_TWIN].fetch_add(1, Relaxed);
        twin
    } else {
        source
    }
}

/// The ranges a batch mapped (call number `call`, from 1): noted by page
/// for [`read`].
pub(crate) fn mapped(call: u64, ranges: &[(u64, u64, u64)]) {
    if !trace() {
        return;
    }
    let mut at = MAPPED_AT.lock().unwrap_or_else(|e| e.into_inner());
    let at = at.get_or_insert_with(HashMap::new);
    for &(to, _, len) in ranges {
        for k in (0..len).step_by(PAGE) {
            at.insert((to + k) as usize, call);
        }
    }
}

/// The batch calls so far.
pub(crate) fn calls() -> u64 {
    COUNTS[CALLS].load(Relaxed)
}

/// A texture's staging buffer took `[at, at + len)` of title memory
/// ([`trace`]): if pages a batch mapped are in it, which mapping it read,
/// once for each newest batch call among them.
pub(crate) fn read(at: usize, len: usize) {
    if !trace() || len == 0 {
        return;
    }
    let first = at - at % PAGE;
    let pages: Vec<usize> = (first..at + len).step_by(PAGE).collect();
    let (remapped, newest) = {
        let mapped = MAPPED_AT.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mapped) = mapped.as_ref() else {
            return;
        };
        let calls: Vec<u64> = pages.iter().filter_map(|p| mapped.get(p).copied()).collect();
        (calls.len(), calls.iter().copied().max())
    };
    let Some(newest) = newest else {
        return;
    };
    static SAID: Mutex<Option<HashMap<usize, u64>>> = Mutex::new(None);
    {
        let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
        let said = said.get_or_insert_with(HashMap::new);
        if said.insert(at, newest) == Some(newest) {
            return;
        }
    }
    static LINES: AtomicU64 = AtomicU64::new(0);
    let lines = LINES.fetch_add(1, Relaxed) + 1;
    if lines > 20_000 && !lines.is_power_of_two() {
        return;
    }
    let mut seen = [0usize; 3];
    let mut first_differ: Option<(usize, Option<(usize, u64)>, Option<(usize, u64)>)> = None;
    for &page in &pages {
        let Some(twin) = twin_of(page) else {
            continue;
        };
        let (gpu, cpu) = (crate::inplace::page_view(page), crate::inplace::page_view(twin));
        let s = compare(gpu, cpu);
        seen[s as usize] += 1;
        if s == Seen::Differ && first_differ.is_none() {
            first_differ = Some((page - first, gpu, cpu));
        }
    }
    let line = format!(
        "a texture was read from pages of the tiled heap: at={at:#x} bytes={len} pages={} remapped={remapped} \
         by_call={newest} calls_now={} against_cpu_view: agree={} differ={} empty={} first_differ={first_differ:?} \
         (VICTORY_MAP_TRACE)",
        pages.len(),
        calls(),
        seen[Seen::Agree as usize],
        seen[Seen::Differ as usize],
        seen[Seen::Empty as usize],
    );
    if first_differ.is_some() {
        tracing::warn!("{line}");
    } else {
        tracing::info!("{line}");
    }
}

/// For the 5-second statistics: nothing until a batch was called.
pub(crate) fn line() {
    let c: Vec<u64> = COUNTS.iter().map(|n| n.load(Relaxed)).collect();
    if c[CALLS] == 0 {
        return;
    }
    let delta = DELTA.load(Relaxed);
    tracing::info!(
        "the tiled heap against its CPU view, since the start: {} CopyPageMappingsBatchX calls; {} pages taken from the pool \
         checked: {} the CPU view's page, {} another page, {} the CPU view empty; {} mapped from the CPU view \
         (VICTORY_MAP_FROM_TWIN=1); {} before the CPU view was found ({} of them shown at the GPU address already: \
         the CPU view there too; distance {})",
        c[CALLS],
        c[CHECKED],
        c[AGREE],
        c[DIFFER],
        c[EMPTY],
        c[FROM_TWIN],
        c[UNKNOWN],
        c[SAME],
        if delta == 0 { "not known".to_string() } else { format!("{delta:#x}") }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_distance_is_the_one_most_pages_vote_for() {
        let mut votes = Vec::new();
        // Stale addresses scatter their votes.
        for k in 0..20 {
            assert!(!vote(&mut votes, 0x1000_0000 + k * 0x10000));
        }
        let wins: Vec<bool> = (0..64).map(|_| vote(&mut votes, 0x2_0000_0000)).collect();
        assert_eq!(wins.iter().position(|&w| w), Some(63));
        // Another distance as common does not win.
        let mut even = Vec::new();
        for _ in 0..64 {
            vote(&mut even, 7);
            vote(&mut even, 9);
        }
        assert!(!vote(&mut even, 7));
    }

    #[test]
    fn pages_are_compared_by_what_they_show() {
        assert_eq!(compare(Some((1, 0x10000)), Some((1, 0x10000))), Seen::Agree);
        assert_eq!(compare(Some((1, 0x10000)), Some((1, 0x20000))), Seen::Differ);
        assert_eq!(compare(Some((1, 0x10000)), Some((2, 0x10000))), Seen::Differ);
        assert_eq!(compare(None, Some((2, 0x10000))), Seen::Differ);
        assert_eq!(compare(Some((1, 0x10000)), None), Seen::Empty);
    }

    /// A GPU page whose pool page is not what its CPU twin shows is counted,
    /// and taken from the twin only with the switch; one that agrees is
    /// mapped as given.
    #[test]
    fn a_page_unlike_its_cpu_twin_is_found() {
        const SECTION: usize = 0x7e7e_7e7e;
        let (gpu, cpu, pool) = (0x7e10_0000_0000usize, 0x7e00_0000_0000usize, 0x7e20_0000_0000usize);
        // Pool pages 0 and 1; the twin of GPU page 0 shows pool page 5, that
        // of GPU page 1 shows pool page 1.
        crate::inplace::title_memory_view(pool, 2 * PAGE, SECTION, 0);
        crate::inplace::title_memory_view(cpu, PAGE, SECTION, 5 * PAGE as u64);
        crate::inplace::title_memory_view(cpu + PAGE, PAGE, SECTION, PAGE as u64);
        DELTA.store((gpu - cpu) as i64, Relaxed);
        let ranges = vec![(gpu as u64, pool as u64, 2 * PAGE as u64), (gpu as u64 + 0x100_0000, 0, PAGE as u64)];
        let before = COUNTS.each_ref().map(|n| n.load(Relaxed));
        assert_eq!(before_with(ranges.clone(), false), ranges);
        let after = COUNTS.each_ref().map(|n| n.load(Relaxed));
        assert!(after[DIFFER] > before[DIFFER] && after[AGREE] > before[AGREE]);
        assert_eq!(
            before_with(ranges, true),
            [
                (gpu as u64, cpu as u64, PAGE as u64),
                (gpu as u64 + PAGE as u64, pool as u64 + PAGE as u64, PAGE as u64),
                (gpu as u64 + 0x100_0000, 0, PAGE as u64)
            ]
        );
        crate::inplace::views_gone(pool, 2 * PAGE);
        crate::inplace::views_gone(cpu, 2 * PAGE);
    }
}
