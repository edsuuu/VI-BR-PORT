//! The table of the host's own structures: the title's destination address
//! of a structure (what its builder was told to build into, and what its
//! programs and its top levels name) to the host structure built from the
//! same description, with what decides whether an update can be a refit.
//!
//! Plain bookkeeping, generic over what a host structure is (`V`), so that
//! it is tested without a card. Each build into a destination is a new
//! *version* with a new generation; a refit in place keeps the version and
//! its generation. A version is usable by what the CPU resolves at
//! submission (a top level's instances, `super::top`) only once its build
//! has been submitted: until then the newest submitted one before it is
//! kept beside it.

use std::collections::HashMap;
use std::sync::Arc;

/// `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE`.
pub(crate) const ALLOW_UPDATE: u32 = 0x1;
/// `..._PERFORM_UPDATE`.
pub(crate) const PERFORM_UPDATE: u32 = 0x20;

/// What a host structure version says of itself.
pub(crate) trait Version {
    /// Whether the list that builds it has been submitted (its build runs
    /// before anything submitted after).
    fn submitted(&self) -> bool;
}

/// One geometry of a bottom level as far as an update's legality goes: the
/// Direct3D rule is that an update keeps every count, format and flag of
/// the build it starts from (vertex positions, the index values and the
/// transform's values may change; whether there is a transform may not).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GeometryShape {
    pub kind: u32,
    pub flags: u32,
    pub index_format: u32,
    pub vertex_format: u32,
    pub index_count: u32,
    pub vertex_count: u32,
    pub transform: bool,
}

/// What a structure is made of, for [`decide`]: the geometries of a bottom
/// level, or a top level's instance count.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Shape {
    Bottom(Vec<GeometryShape>),
    Top(u32),
}

impl Shape {
    pub(crate) fn top(&self) -> bool {
        matches!(self, Shape::Top(_))
    }
}

/// How the host does what the title asked of a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// A build from scratch, asked for as one.
    Build,
    /// An update asked for and possible: a refit of the source's host
    /// structure, into itself (`in_place`) or into a new one for the
    /// destination.
    Refit { in_place: bool },
    /// An update asked for that the host cannot do as one: built from
    /// scratch, for this reason.
    Rebuild(Why),
}

/// Why an update is built from scratch on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Why {
    /// The host has no structure at the source address.
    NoSource,
    /// The source's host structure was built without `ALLOW_UPDATE`.
    NotUpdatable,
    /// The update's build flags differ from the source's.
    OtherFlags,
    /// Another level, or other counts, formats or geometry flags.
    OtherShape,
}

impl Why {
    pub(crate) const ALL: [Why; 4] = [Why::NoSource, Why::NotUpdatable, Why::OtherFlags, Why::OtherShape];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Why::NoSource => "no host structure at the source",
            Why::NotUpdatable => "the source was built without ALLOW_UPDATE",
            Why::OtherFlags => "other build flags than the source's",
            Why::OtherShape => "another shape than the source's",
        }
    }
}

/// What the table knows of a destination.
pub(crate) struct Entry<V> {
    /// The build flags of the current version (without `PERFORM_UPDATE`).
    pub flags: u32,
    pub shape: Shape,
    /// The current version's generation: another number at every build into
    /// the destination, the same through refits in place.
    pub generation: u64,
    /// The frame the destination was last built, refit or named by a top
    /// level in.
    pub last_used: u64,
    pub current: Arc<V>,
    /// The newest version before `current` whose build was submitted, while
    /// `current`'s is not.
    pub previous: Option<Arc<V>>,
}

/// Destinations to their host structures.
pub(crate) struct Table<V> {
    entries: HashMap<u64, Entry<V>>,
    generations: u64,
}

impl<V> Default for Table<V> {
    fn default() -> Self {
        Table { entries: HashMap::new(), generations: 0 }
    }
}

/// Whether an update of `source`'s structure with `flags` and `shape` can be
/// a refit on the host ([`Decision`]): `update` says whether the title asked
/// for one (`PERFORM_UPDATE`).
pub(crate) fn decide<V: Version>(update: bool, dest: u64, source: u64, flags: u32, shape: &Shape, table: &Table<V>) -> Decision {
    if !update {
        return Decision::Build;
    }
    // (An update whose source is 0 updates the destination in place.)
    let from = if source == 0 { dest } else { source };
    let Some(entry) = table.get(from) else {
        return Decision::Rebuild(Why::NoSource);
    };
    if entry.flags & ALLOW_UPDATE == 0 {
        return Decision::Rebuild(Why::NotUpdatable);
    }
    if entry.flags != flags & !PERFORM_UPDATE {
        return Decision::Rebuild(Why::OtherFlags);
    }
    if entry.shape != *shape {
        return Decision::Rebuild(Why::OtherShape);
    }
    Decision::Refit { in_place: from == dest }
}

impl<V: Version> Table<V> {
    pub(crate) fn get(&self, dest: u64) -> Option<&Entry<V>> {
        self.entries.get(&dest)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&u64, &Entry<V>)> {
        self.entries.iter()
    }

    /// A build (or a refit into another structure) into `dest` was recorded:
    /// `version` is its current version from now on, with a new generation,
    /// which is returned. The version it replaces is kept beside it while
    /// `version`'s build is not submitted, if it was itself (or the one kept
    /// beside it, if not).
    pub(crate) fn built(&mut self, dest: u64, flags: u32, shape: Shape, version: Arc<V>, frame: u64) -> u64 {
        self.generations += 1;
        let generation = self.generations;
        let previous = self.entries.remove(&dest).and_then(|old| {
            if old.current.submitted() { Some(old.current) } else { old.previous }
        });
        self.entries.insert(
            dest,
            Entry { flags: flags & !PERFORM_UPDATE, shape, generation, last_used: frame, current: version, previous },
        );
        generation
    }

    /// A refit of `dest`'s structure into itself was recorded.
    pub(crate) fn refit_in_place(&mut self, dest: u64, frame: u64) -> Option<u64> {
        let entry = self.entries.get_mut(&dest)?;
        entry.last_used = frame;
        Some(entry.generation)
    }

    /// The version of `dest` what is submitted now can use: the current one
    /// once its build was submitted, else the one kept beside it. Names the
    /// destination as used in `frame`.
    pub(crate) fn usable(&mut self, dest: u64, frame: u64) -> Option<Arc<V>> {
        let entry = self.entries.get_mut(&dest)?;
        entry.last_used = entry.last_used.max(frame);
        if entry.current.submitted() {
            // Nothing submitted from now on needs the one before.
            entry.previous = None;
            return Some(entry.current.clone());
        }
        entry.previous.clone()
    }

    /// The destination's structure is gone (the title built over it at
    /// another level, or its memory was given up).
    pub(crate) fn forget(&mut self, dest: u64) -> Option<Entry<V>> {
        self.entries.remove(&dest)
    }

    /// Takes out the destinations not used for `idle` frames before `frame`.
    pub(crate) fn sweep(&mut self, frame: u64, idle: u64) -> Vec<Entry<V>> {
        let old: Vec<u64> =
            self.entries.iter().filter(|(_, e)| e.last_used.saturating_add(idle) < frame).map(|(&k, _)| k).collect();
        old.into_iter().filter_map(|k| self.entries.remove(&k)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A version that is submitted when the test says so.
    #[derive(Default)]
    struct Fake {
        n: u32,
        sent: AtomicBool,
    }

    impl Version for Fake {
        fn submitted(&self) -> bool {
            self.sent.load(Ordering::Relaxed)
        }
    }

    fn version(n: u32) -> Arc<Fake> {
        Arc::new(Fake { n, sent: AtomicBool::new(false) })
    }

    fn tri(vertex_count: u32) -> GeometryShape {
        GeometryShape {
            kind: 0,
            flags: 1,
            index_format: 57,
            vertex_format: 6,
            index_count: 300,
            vertex_count,
            transform: false,
        }
    }

    #[test]
    fn every_build_is_a_new_generation_and_a_refit_in_place_keeps_it() {
        let mut t = Table::<Fake>::default();
        let shape = Shape::Bottom(vec![tri(100)]);
        let g1 = t.built(0x1000, ALLOW_UPDATE, shape.clone(), version(1), 1);
        let g2 = t.built(0x2000, ALLOW_UPDATE, shape.clone(), version(2), 1);
        let g3 = t.built(0x1000, ALLOW_UPDATE, shape.clone(), version(3), 2);
        assert!(g1 < g2 && g2 < g3);
        assert_eq!(t.refit_in_place(0x1000, 3), Some(g3));
        assert_eq!(t.get(0x1000).unwrap().generation, g3);
        assert_eq!(t.get(0x1000).unwrap().last_used, 3);
        assert_eq!(t.get(0x1000).unwrap().current.n, 3);
        assert_eq!(t.refit_in_place(0x3000, 3), None);
        // The update bit is not part of what was built.
        t.built(0x4000, ALLOW_UPDATE | PERFORM_UPDATE, shape, version(4), 3);
        assert_eq!(t.get(0x4000).unwrap().flags, ALLOW_UPDATE);
    }

    #[test]
    fn what_is_submitted_uses_the_newest_version_whose_build_was_submitted() {
        let mut t = Table::<Fake>::default();
        let shape = Shape::Top(10);
        let first = version(1);
        t.built(0x1000, 0, shape.clone(), first.clone(), 1);
        // Not submitted yet: nothing to use.
        assert!(t.usable(0x1000, 1).is_none());
        first.sent.store(true, Ordering::Relaxed);
        assert_eq!(t.usable(0x1000, 1).unwrap().n, 1);
        // A second build recorded before the first is replaced on the card:
        // until it is submitted the first is what can be used.
        let second = version(2);
        t.built(0x1000, 0, shape.clone(), second.clone(), 2);
        assert_eq!(t.usable(0x1000, 2).unwrap().n, 1);
        // A third recorded while the second is not submitted: the first is
        // still the newest submitted one.
        let third = version(3);
        t.built(0x1000, 0, shape, third.clone(), 2);
        assert_eq!(t.usable(0x1000, 2).unwrap().n, 1);
        third.sent.store(true, Ordering::Relaxed);
        assert_eq!(t.usable(0x1000, 3).unwrap().n, 3);
        // And the one kept beside it is let go.
        assert!(t.get(0x1000).unwrap().previous.is_none());
        assert_eq!(Arc::strong_count(&first), 1);
    }

    #[test]
    fn an_update_is_a_refit_only_from_an_updatable_source_of_the_same_shape_and_flags() {
        let mut t = Table::<Fake>::default();
        let shape = Shape::Bottom(vec![tri(100), tri(50)]);
        let fast = 0x4;
        t.built(0x1000, ALLOW_UPDATE | fast, shape.clone(), version(1), 1);
        t.built(0x2000, fast, shape.clone(), version(2), 1);
        let up = ALLOW_UPDATE | fast | PERFORM_UPDATE;
        // Not asked for: a build.
        assert_eq!(decide(false, 0x1000, 0, ALLOW_UPDATE | fast, &shape, &t), Decision::Build);
        // In place, by the source being the destination or 0.
        assert_eq!(decide(true, 0x1000, 0x1000, up, &shape, &t), Decision::Refit { in_place: true });
        assert_eq!(decide(true, 0x1000, 0, up, &shape, &t), Decision::Refit { in_place: true });
        // From another structure into a destination of its own.
        assert_eq!(decide(true, 0x5000, 0x1000, up, &shape, &t), Decision::Refit { in_place: false });
        // No source on the host.
        assert_eq!(decide(true, 0x5000, 0x9000, up, &shape, &t), Decision::Rebuild(Why::NoSource));
        // Built without ALLOW_UPDATE.
        assert_eq!(decide(true, 0x2000, 0x2000, up, &shape, &t), Decision::Rebuild(Why::NotUpdatable));
        // Other flags.
        assert_eq!(decide(true, 0x1000, 0x1000, ALLOW_UPDATE | PERFORM_UPDATE, &shape, &t), Decision::Rebuild(Why::OtherFlags));
        // Another vertex count, another geometry count, another level.
        let moved = Shape::Bottom(vec![tri(101), tri(50)]);
        assert_eq!(decide(true, 0x1000, 0x1000, up, &moved, &t), Decision::Rebuild(Why::OtherShape));
        let fewer = Shape::Bottom(vec![tri(100)]);
        assert_eq!(decide(true, 0x1000, 0x1000, up, &fewer, &t), Decision::Rebuild(Why::OtherShape));
        assert_eq!(decide(true, 0x1000, 0x1000, up, &Shape::Top(2), &t), Decision::Rebuild(Why::OtherShape));
        // Gaining a transform is another shape too.
        let mut with = tri(100);
        with.transform = true;
        let transformed = Shape::Bottom(vec![with, tri(50)]);
        assert_eq!(decide(true, 0x1000, 0x1000, up, &transformed, &t), Decision::Rebuild(Why::OtherShape));
    }

    #[test]
    fn destinations_nothing_used_for_a_while_are_swept() {
        let mut t = Table::<Fake>::default();
        let shape = Shape::Top(1);
        t.built(0x1000, 0, shape.clone(), version(1), 10);
        t.built(0x2000, 0, shape.clone(), version(2), 10);
        // A top level naming 0x2000 later keeps it.
        let named = version(3);
        named.sent.store(true, Ordering::Relaxed);
        t.built(0x3000, 0, shape, named, 10);
        t.usable(0x3000, 300);
        t.refit_in_place(0x2000, 290);
        let gone = t.sweep(300, 256);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].current.n, 1);
        assert!(t.get(0x1000).is_none() && t.get(0x2000).is_some() && t.get(0x3000).is_some());
        assert_eq!(t.len(), 2);
        assert!(t.forget(0x2000).is_some());
        assert_eq!(t.len(), 1);
    }
}
