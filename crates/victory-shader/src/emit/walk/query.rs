//! A site that stands for a walk taken out of the program (`replace.rs`), as
//! the host's ray query: a child module of the lowering, so it shares the
//! emitter's state (as `intersect.rs` does).
//!
//! What it makes, under the site's lane guard:
//!
//! ```text
//! q = AllocateRayQuery(flags)        flags: opaque, first hit or not, culling
//! TraceRayInline(q, t<SLOT_STRUCTURE>, 0, mask, origin, t min, direction, extent)
//! while (Proceed(q)) {}              (every triangle opaque: nothing to judge)
//! hit = CommittedStatus(q) == COMMITTED_TRIANGLE_HIT
//! each output: t / a triangle's node pointer / the entry value / a constant
//! ```
//!
//! The structure is the host's, bound as the root SRV `t<SLOT_STRUCTURE>`
//! for the title's view whose descriptor the program loads at a fixed place
//! of a table: the user-data dword of kind `USER_STRUCTURE` names that
//! table and dword to the host (`shader.rs`).

use super::super::rayquery::{COMMITTED_TRIANGLE_HIT, InlineRay, State};
use super::super::shader::{SLOT_STRUCTURE, USER_STRUCTURE};
use super::super::walk::replace::{OutKind, Query, TMin};
use super::{Cast, Inst, Lower, Reg, RootType, Term, V, Why, pred};

impl<'a> Lower<'a> {
    /// The site at `(b, i)`, which stands for the walk `q` describes.
    pub(super) fn ray_query(&mut self, inst: &Inst, b: usize, i: usize, q: &Query) -> Result<(), Why> {
        if self.paired.is_some() {
            // (each host lane carries two console lanes: one query a lane)
            return Err("a host ray query in the paired form");
        }
        // Where the title's structure view is: the descriptor's table and dword.
        let p = self.x.p;
        let site = p
            .blocks
            .iter()
            .enumerate()
            .find_map(|(lb, block)| {
                block.insts.iter().position(|x| x.machine.offset == q.descriptor_load).map(|li| (lb, li))
            })
            .ok_or("the structure's descriptor load is gone")?;
        let a = self.scalar.get(&site).copied().ok_or("an untracked structure descriptor")?;
        let rp = *self.x.native.layout.get(a.param).ok_or("a structure descriptor of no root parameter")?;
        if rp.root != RootType::Table || a.dynamic.is_some() || a.offset % 4 != 0 {
            return Err("a structure descriptor not at a fixed place of a table");
        }
        let _ = self.host_word(RootType::Other(USER_STRUCTURE), rp.slot, a.offset / 4);
        let structure = self.s.structure_handle(SLOT_STRUCTURE);

        let guard = self.guard(b, i)?;
        // (what the site reads in a register: its value at the region's entry)
        let word = |l: &mut Self, r: Reg| -> Result<V, Why> {
            let v = inst.read(r).ok_or("a register the site does not read")?;
            l.i32_of(v)
        };
        let value = |l: &mut Self, r: Reg| -> Result<V, Why> {
            let x = word(l, r)?;
            Ok(l.as_float(x))
        };
        let origin = [value(self, q.origin[0])?, value(self, q.origin[1])?, value(self, q.origin[2])?];
        let direction = [value(self, q.direction[0])?, value(self, q.direction[1])?, value(self, q.direction[2])?];
        let t_max = value(self, q.extent)?;
        let mut entries = Vec::with_capacity(q.outputs.len());
        for o in &q.outputs {
            entries.push(word(self, o.reg)?);
        }
        let t_min = match q.t_min {
            TMin::Const(k) => self.s.m.f32(k),
            TMin::Reg(r) => value(self, r)?,
        };
        let mask = self.s.m.i32(u32::from(q.mask));
        let none = self.s.m.i32(0);

        let gate = self.begin_guarded(guard);
        let head = self.cur;
        let query = self.s.ray_query(head, q.flags);
        let ray = InlineRay { flags: none, mask, origin, t_min, direction, t_max };
        self.s.trace_ray_inline(head, query, structure, &ray);
        let (walk, after) = (self.s.m.new_block(), self.s.m.new_block());
        self.s.m.terminate(head, Term::Br(walk));
        let more = self.s.ray_query_proceed(walk, query);
        self.s.m.terminate(walk, Term::CondBr(more, walk, after));
        self.cur = after;
        let status = self.s.ray_query_state(after, query, State::CommittedStatus);
        let triangle = self.s.m.i32(COMMITTED_TRIANGLE_HIT);
        let hit = self.s.m.cmp(after, pred::EQ, status, triangle);
        let t = self.s.ray_query_state(after, query, State::CommittedRayT);
        let i32t = self.s.t.i32;
        let t = self.s.m.cast(after, Cast::Bitcast, t, i32t);
        let mut results = Vec::with_capacity(q.outputs.len());
        for (o, &entry) in q.outputs.iter().zip(&entries) {
            let v = match o.kind {
                OutKind::T => self.s.m.select(after, hit, t, entry),
                OutKind::Hit(mark) => {
                    let mark = self.s.m.i32(mark);
                    self.s.m.select(after, hit, mark, entry)
                }
                OutKind::Entry => entry,
                OutKind::Constant(k) => self.s.m.i32(k),
            };
            results.push((v, entry));
        }
        let results = self.end_guarded(gate, &results);
        for (o, r) in q.outputs.iter().zip(results) {
            self.write(inst, o.reg, r, guard);
        }
        Ok(())
    }
}
