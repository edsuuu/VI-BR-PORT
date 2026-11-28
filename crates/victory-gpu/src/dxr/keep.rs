//! Bottom levels let go and made again (`VICTORY_DXR_KEEP=1`; off by
//! default, and then nothing here runs).
//!
//! The title builds or unpacks a bottom level once and names it in its top
//! levels for as long as it keeps it: an unpacked one is never asked again
//! while it stays streamed in. Without this, a bottom level the host let go
//! (none of its top levels named it for `IDLE` frames, `super::native`), or
//! one it refused for want of card memory (`VICTORY_DXR_NATIVE_MB`: the
//! prison's load asks more than the ceiling holds), is gone for good: every
//! instance naming it later is made inactive.
//!
//! With the switch:
//! - every bottom level the host builds leaves its recipe here: the title's
//!   description (its geometries in title memory) and where it came from,
//!   as a build from scratch; so does one refused for want of memory;
//! - a top level whose instance names a destination the host has no
//!   structure for but a recipe of asks for it ([`want`]); the next
//!   recording of ours, in whatever list, makes it again first, from the
//!   title memory as it is then (`super::native::record_on`), so that the
//!   top levels after it name it;
//! - when card memory is short, bottom levels no top level named for
//!   [`EVICT_IDLE`] frames are let go, the least lately named first
//!   ([`evict`]), before a build is refused: what the top levels name now
//!   wins over what they named long ago.
//!
//! What is made again reads the title's geometry as it is then: for an
//! unpacked structure (static) the same; for one the title built from data
//! it changed since without asking again, what it is now.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use super::count::{C, add};
use super::table::{PERFORM_UPDATE, Table, Version};
use super::{Build, Origin};

/// `VICTORY_DXR_KEEP=1`.
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("VICTORY_DXR_KEEP").is_ok_and(|v| v.trim() == "1");
        if on && super::native_on() {
            tracing::info!(
                "VICTORY_DXR_KEEP: bottom levels the host let go or had no memory for are made again when a top level names them"
            );
        }
        on
    })
}

/// How a bottom level is built from scratch again.
#[derive(Debug)]
pub(super) struct Recipe {
    pub(super) origin: Origin,
    pub(super) build: Build,
}

/// Recipes kept at most (past it, the quarter least lately named go).
const RECIPES_MOST: usize = 1 << 17;

/// Bottom levels made again at most a recording.
const REMADE_MOST: usize = 64;

/// A bottom level no top level named for so many frames may be let go when
/// card memory is short.
const EVICT_IDLE: u64 = 30;

#[derive(Default)]
struct Book {
    /// By destination: the recipe and the frame it was last built or asked
    /// for in.
    recipes: HashMap<u64, (Arc<Recipe>, u64)>,
    /// Destinations a top level named that the host has a recipe but no
    /// structure for.
    wanted: BTreeSet<u64>,
}

impl Book {
    fn remember(&mut self, origin: Origin, b: &Build, frame: u64) {
        let build = Build { source: 0, flags: b.flags & !PERFORM_UPDATE, ..b.clone() };
        self.recipes.insert(b.dest, (Arc::new(Recipe { origin, build }), frame));
        self.wanted.remove(&b.dest);
        if self.recipes.len() > RECIPES_MOST {
            let mut ages: Vec<u64> = self.recipes.values().map(|r| r.1).collect();
            ages.sort_unstable();
            let cut = ages[ages.len() / 4];
            self.recipes.retain(|_, r| r.1 > cut);
        }
    }

    fn forget(&mut self, dest: u64) {
        self.recipes.remove(&dest);
        self.wanted.remove(&dest);
    }

    fn copied(&mut self, dest: u64, source: u64, frame: u64) {
        match self.recipes.get(&source).map(|r| r.0.clone()) {
            Some(r) => {
                self.recipes.insert(dest, (r, frame));
            }
            None => {
                self.recipes.remove(&dest);
            }
        }
    }

    /// Whether `dest` has a recipe (and is asked for from now on), and
    /// whether it was asked for anew.
    fn want(&mut self, dest: u64, frame: u64) -> (bool, bool) {
        match self.recipes.get_mut(&dest) {
            Some(r) => {
                r.1 = frame;
                (true, self.wanted.insert(dest))
            }
            None => (false, false),
        }
    }

    fn wanted(&mut self, made: impl Fn(u64) -> bool) -> Vec<(u64, Arc<Recipe>)> {
        let mut out = Vec::new();
        while out.len() < REMADE_MOST {
            let Some(dest) = self.wanted.pop_first() else { break };
            if made(dest) {
                continue;
            }
            if let Some((r, _)) = self.recipes.get(&dest) {
                out.push((dest, r.clone()));
            }
        }
        out
    }
}

static BOOK: Mutex<Option<Book>> = Mutex::new(None);

fn book<R>(f: impl FnOnce(&mut Book) -> R) -> R {
    let mut b = BOOK.lock().unwrap_or_else(|e| e.into_inner());
    f(b.get_or_insert_with(Book::default))
}

/// The host built the bottom level `b` (or refused it for want of memory):
/// its recipe, as a build from scratch.
pub(super) fn remember(origin: Origin, b: &Build, frame: u64) {
    if on() {
        book(|k| k.remember(origin, b, frame));
    }
}

/// What the title asked at `dest` is not to be made again (a build the host
/// refused for another reason than memory: made again it would be refused
/// the same).
pub(super) fn forget(dest: u64) {
    if on() {
        book(|k| k.forget(dest));
    }
}

/// The title copied the structure at `source` to `dest`: the copy is made
/// again as its source.
pub(super) fn copied(dest: u64, source: u64, frame: u64) {
    if on() {
        book(|k| k.copied(dest, source, frame));
    }
}

/// A top level names `dest`, which the host has no structure for: whether
/// it has a recipe of it (then it is made again at the next recording).
pub(super) fn want(dest: u64, frame: u64) -> bool {
    if !on() {
        return false;
    }
    let (known, anew) = book(|k| k.want(dest, frame));
    if anew {
        add(C::Wanted, 1);
    }
    known
}

/// The destinations to make again now with their recipes (at most
/// [`REMADE_MOST`]), those the table has a structure for again left out.
pub(super) fn wanted<V: Version>(table: &Table<V>) -> Vec<(u64, Arc<Recipe>)> {
    if !on() {
        return Vec::new();
    }
    book(|k| k.wanted(|dest| table.get(dest).is_some()))
}

/// Card memory is short by `len` bytes: lets go of bottom levels no top
/// level named for [`EVICT_IDLE`] frames before `frame`, the least lately
/// named first, until about `len` bytes are given back to the pool
/// (`freed` says what letting one go gives back now: nothing while a list
/// or a top level still holds it). How many it let go.
pub(super) fn evict<V: Version>(table: &mut Table<V>, frame: u64, len: u64, freed: impl Fn(&Arc<V>) -> u64) -> usize {
    if !on() {
        return 0;
    }
    let n = evict_idle(table, frame, len, freed);
    add(C::Evicted, n as u64);
    n
}

fn evict_idle<V: Version>(table: &mut Table<V>, frame: u64, len: u64, freed: impl Fn(&Arc<V>) -> u64) -> usize {
    let mut idle: Vec<(u64, u64)> = table
        .entries()
        .filter(|(_, e)| !e.shape.top() && e.last_used.saturating_add(EVICT_IDLE) < frame)
        .map(|(&dest, e)| (e.last_used, dest))
        .collect();
    idle.sort_unstable();
    let (mut given, mut n) = (0, 0);
    for (_, dest) in idle {
        if given >= len {
            break;
        }
        if let Some(e) = table.forget(dest) {
            given += freed(&e.current);
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxr::Level;
    use crate::dxr::table::Shape;

    fn bottom(dest: u64, flags: u32) -> Build {
        Build { dest, source: dest, flags, level: Level::Bottom { geometries: Vec::new(), whole: true } }
    }

    #[test]
    fn a_recipe_named_without_a_structure_is_made_again_once() {
        let mut k = Book::default();
        // An update is kept as a build from scratch of the same.
        k.remember(Origin::Batched, &bottom(0x1000, 0x1 | PERFORM_UPDATE), 5);
        let r = &k.recipes[&0x1000].0;
        assert_eq!((r.build.source, r.build.flags, r.origin), (0, 0x1, Origin::Batched));
        k.remember(Origin::Rehydrated, &bottom(0x2000, 0x4), 5);
        // Asked for by two top levels: once.
        assert_eq!(k.want(0x1000, 9), (true, true));
        assert_eq!(k.want(0x1000, 9), (true, false));
        assert_eq!(k.want(0x3000, 9), (false, false));
        assert_eq!(k.want(0x2000, 9), (true, true));
        // 0x2000 has a structure again by then: left out.
        let now = k.wanted(|d| d == 0x2000);
        assert_eq!(now.iter().map(|w| w.0).collect::<Vec<_>>(), vec![0x1000]);
        assert!(k.wanted.is_empty());
        // A copy is made again as its source; a refusal forgets.
        k.copied(0x5000, 0x2000, 10);
        assert_eq!(k.recipes[&0x5000].0.origin, Origin::Rehydrated);
        k.forget(0x2000);
        assert_eq!(k.want(0x2000, 11), (false, false));
        k.copied(0x6000, 0x2000, 11);
        assert!(!k.recipes.contains_key(&0x6000));
    }

    #[derive(Default)]
    struct Fake;

    impl Version for Fake {
        fn submitted(&self) -> bool {
            true
        }
    }

    #[test]
    fn short_memory_lets_go_of_the_least_lately_named_idle_bottom_levels() {
        let mut t = Table::<Fake>::default();
        let shape = Shape::Bottom(Vec::new());
        for (dest, frame) in [(0x1000, 10), (0x2000, 5), (0x3000, 60), (0x4000, 1)] {
            t.built(dest, 0, shape.clone(), Arc::new(Fake), frame);
        }
        // A top level idle as long: never.
        t.built(0x9000, 0, Shape::Top(4), Arc::new(Fake), 0);
        // At frame 70, 0x3000 is not idle for 30 frames; the others are, the
        // oldest first: 0x4000 then 0x2000 give 100 bytes each.
        assert_eq!(evict_idle(&mut t, 70, 150, |_| 100), 2);
        assert!(t.get(0x4000).is_none() && t.get(0x2000).is_none());
        assert!(t.get(0x1000).is_some() && t.get(0x3000).is_some() && t.get(0x9000).is_some());
        // What gives nothing back now (held elsewhere) does not end it.
        assert_eq!(evict_idle(&mut t, 70, 1, |_| 0), 1);
        assert_eq!(t.len(), 2);
    }
}
