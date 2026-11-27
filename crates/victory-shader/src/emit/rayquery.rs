//! The host's inline ray tracing: DXIL ray queries (shader model 6.5,
//! `D3D12_RAYTRACING_TIER_1_1`) against a host acceleration structure bound
//! as a root SRV.
//!
//! These are building blocks, and nothing the translation makes calls them
//! yet. The title's ray programs walk the console's own structures in loops
//! around `image_bvh_intersect_ray` (`intersect.rs`); a program that asks
//! the host's hardware instead needs the host's structure (built from the
//! same triangles and instances) bound where the console's was, and its
//! walk replaced by a query made with these calls.
//!
//! What the calls are, as DXC declares them (`DxilConstants.h`,
//! `DxilOperations.cpp`):
//!
//! - `allocateRayQuery(i32 178, i32 flags)` returns the query, an `i32`;
//!   the flags are the `RAY_FLAG`s fixed for the query's whole life and must
//!   be a constant;
//! - `rayQuery_TraceRayInline(179, query, structure, flags, mask, origin
//!   xyz, t min, direction xyz, t max)` starts it;
//! - `rayQuery_Proceed.i1(180, query)` walks to the next candidate the
//!   program must judge (a hit on a non-opaque triangle or a procedural
//!   box) and is false when the walk is over; `rayQuery_Abort` (181) ends it,
//!   `rayQuery_CommitNonOpaqueTriangleHit` (182) accepts the candidate;
//! - the state of the query, through `rayQuery_StateScalar` and
//!   `rayQuery_StateVector` ([`State`], [`Shader::ray_query_barycentric`]).
//!
//! The walk calls are declared `nounwind` (they change the query); the
//! state calls `readonly`, which the module's clean-up treats as reads that
//! a call with effects ends (`bitcode::opt::effect`): one is never reused
//! across a `Proceed`.

use super::bitcode::{Attr, Bin, Cast, Term, V, pred};
use super::shader::{ROOT_CBV_SPACE, Shader, flags};

/// DXIL opcodes of the ray query operations.
pub(crate) mod op {
    pub const ALLOCATE: u32 = 178;
    pub const TRACE_RAY_INLINE: u32 = 179;
    pub const PROCEED: u32 = 180;
    pub const ABORT: u32 = 181;
    pub const COMMIT_NON_OPAQUE_TRIANGLE_HIT: u32 = 182;
    pub const CANDIDATE_TRIANGLE_BARYCENTRICS: u32 = 193;
    pub const COMMITTED_TRIANGLE_BARYCENTRICS: u32 = 194;
}

/// `COMMITTED_STATUS`: nothing committed.
pub const COMMITTED_NOTHING: u32 = 0;
/// `COMMITTED_STATUS`: a triangle hit committed.
pub const COMMITTED_TRIANGLE_HIT: u32 = 1;
/// `CANDIDATE_TYPE`: a non-opaque triangle.
pub const CANDIDATE_NON_OPAQUE_TRIANGLE: u32 = 0;

/// A scalar of a query's state (`rayQuery_StateScalar`).
// Building blocks: the probe reads only some; a walk replaced by a query reads more.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    CommittedStatus,
    CandidateType,
    CandidateTriangleRayT,
    CommittedRayT,
    CandidateInstanceIndex,
    CandidateInstanceId,
    CandidateGeometryIndex,
    CandidatePrimitiveIndex,
    CommittedInstanceIndex,
    CommittedInstanceId,
    CommittedGeometryIndex,
    CommittedPrimitiveIndex,
    CandidateTriangleFrontFace,
    CommittedTriangleFrontFace,
    RayTMin,
}

/// The type a state scalar comes as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scalar {
    I32,
    F32,
    I1,
}

impl State {
    /// Its opcode and type.
    pub(crate) fn opcode(self) -> (u32, Scalar) {
        use Scalar::*;
        match self {
            State::CommittedStatus => (184, I32),
            State::CandidateType => (185, I32),
            State::CandidateTriangleFrontFace => (191, I1),
            State::CommittedTriangleFrontFace => (192, I1),
            State::RayTMin => (198, F32),
            State::CandidateTriangleRayT => (199, F32),
            State::CommittedRayT => (200, F32),
            State::CandidateInstanceIndex => (201, I32),
            State::CandidateInstanceId => (202, I32),
            State::CandidateGeometryIndex => (203, I32),
            State::CandidatePrimitiveIndex => (204, I32),
            State::CommittedInstanceIndex => (207, I32),
            State::CommittedInstanceId => (208, I32),
            State::CommittedGeometryIndex => (209, I32),
            State::CommittedPrimitiveIndex => (210, I32),
        }
    }
}

/// The ray of a [`Shader::trace_ray_inline`], as values of the program.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InlineRay {
    /// `RAY_FLAG`s added to the query's own (an `i32`).
    pub flags: V,
    /// The instance inclusion mask (an `i32`, its low 8 bits).
    pub mask: V,
    pub origin: [V; 3],
    pub t_min: V,
    pub direction: [V; 3],
    pub t_max: V,
}

impl Shader {
    /// The handle of the host ray-tracing structure bound as the root SRV
    /// of root parameter `slot` (`t<slot>` in [`ROOT_CBV_SPACE`]; a root SRV
    /// may name an acceleration structure by its address), made in the entry
    /// block.
    pub(crate) fn structure_handle(&mut self, slot: u8) -> V {
        if let Some(&(_, h)) = self.structures.iter().find(|r| r.0 == slot) {
            return h;
        }
        assert!(
            !self.root_srvs.iter().any(|r| r.0 == slot),
            "root parameter {slot} is a raw buffer already"
        );
        let r = self.m.i32(u32::from(slot));
        let bind = [r, r, self.m.i32(ROOT_CBV_SPACE), self.m.i8(0)];
        let bind = self.m.aggregate(self.t.bind, &bind);
        let f = self.m.bool(false);
        let block = self.entry;
        let h = self.op(
            block,
            "createHandleFromBinding",
            Attr::ReadNone,
            self.t.handle,
            217,
            &[bind, r, f],
        );
        // Resource kind 16 (RTAccelerationStructure), nothing else.
        let h = self.annotate(block, h, 16, 0);
        self.structures.push((slot, h));
        h
    }

    /// A new ray query with the constant `ray_flags`.
    pub(crate) fn ray_query(&mut self, block: usize, ray_flags: u32) -> V {
        self.flags |= flags::RAYTRACING_1_1;
        let f = self.m.i32(ray_flags);
        let i32t = self.t.i32;
        self.op(block, "allocateRayQuery", Attr::NoUnwind, i32t, op::ALLOCATE, &[f])
    }

    /// Starts `query` along `ray` through `structure`
    /// ([`Shader::structure_handle`]).
    pub(crate) fn trace_ray_inline(&mut self, block: usize, query: V, structure: V, ray: &InlineRay) {
        let mut args = vec![query, structure, ray.flags, ray.mask];
        args.extend(ray.origin);
        args.push(ray.t_min);
        args.extend(ray.direction);
        args.push(ray.t_max);
        let void = self.t.void;
        self.op(
            block,
            "rayQuery_TraceRayInline",
            Attr::NoUnwind,
            void,
            op::TRACE_RAY_INLINE,
            &args,
        );
    }

    /// Walks `query` on: true when a candidate waits for the program's
    /// judgement, false when the walk is over.
    pub(crate) fn ray_query_proceed(&mut self, block: usize, query: V) -> V {
        let i1 = self.t.i1;
        self.op(block, "rayQuery_Proceed.i1", Attr::NoUnwind, i1, op::PROCEED, &[query])
    }

    /// Ends the walk of `query` (its next `Proceed` is false).
    pub(crate) fn ray_query_abort(&mut self, block: usize, query: V) {
        let void = self.t.void;
        self.op(block, "rayQuery_Abort", Attr::NoUnwind, void, op::ABORT, &[query]);
    }

    /// Accepts the non-opaque triangle `query` stands at.
    pub(crate) fn ray_query_commit_triangle(&mut self, block: usize, query: V) {
        let void = self.t.void;
        self.op(
            block,
            "rayQuery_CommitNonOpaqueTriangleHit",
            Attr::NoUnwind,
            void,
            op::COMMIT_NON_OPAQUE_TRIANGLE_HIT,
            &[query],
        );
    }

    /// A scalar of `query`'s state.
    pub(crate) fn ray_query_state(&mut self, block: usize, query: V, state: State) -> V {
        let (opcode, scalar) = state.opcode();
        let (name, ty) = match scalar {
            Scalar::I32 => ("rayQuery_StateScalar.i32", self.t.i32),
            Scalar::F32 => ("rayQuery_StateScalar.f32", self.t.f32),
            Scalar::I1 => ("rayQuery_StateScalar.i1", self.t.i1),
        };
        self.op(block, name, Attr::ReadOnly, ty, opcode, &[query])
    }

    /// Component `component` (0 or 1) of the barycentrics of `query`'s
    /// committed triangle hit, or of its candidate's: the weights of the
    /// triangle's second and third vertices.
    pub(crate) fn ray_query_barycentric(&mut self, block: usize, query: V, committed: bool, component: u8) -> V {
        let opcode = if committed {
            op::COMMITTED_TRIANGLE_BARYCENTRICS
        } else {
            op::CANDIDATE_TRIANGLE_BARYCENTRICS
        };
        let c = self.m.i8(component);
        let f32t = self.t.f32;
        self.op(block, "rayQuery_StateVector.f32", Attr::ReadOnly, f32t, opcode, &[query, c])
    }
}

/// Root parameters of [`ray_query_probe`], all in [`ROOT_CBV_SPACE`]: the
/// rays (a raw buffer, `t0`), the structure (`t1`), the hits (a raw buffer
/// written, `u2`).
pub const PROBE_RAYS: u8 = 0;
/// See [`PROBE_RAYS`].
pub const PROBE_STRUCTURE: u8 = 1;
/// See [`PROBE_RAYS`].
pub const PROBE_HITS: u8 = 2;
/// Dwords per ray of [`ray_query_probe`]: origin xyz, t min, direction
/// xyz, t max (floats); the instance mask; a primitive index whose
/// candidates are refused (`!0`: none); abort at the first candidate (not
/// 0); one unused.
pub const PROBE_RAY_DWORDS: u32 = 12;
/// Dwords per hit of [`ray_query_probe`]: the committed status, then, for a
/// triangle hit (zeros otherwise), t (float bits), the instance's index and
/// id, the primitive and geometry indices, the barycentrics (float bits).
pub const PROBE_HIT_DWORDS: u32 = 8;
/// Threads per group of [`ray_query_probe`].
pub const PROBE_GROUP: u32 = 32;

/// A compute program (a DXIL container) that shoots `rays` rays with ray
/// queries, one per thread, and writes what each committed: a test of the
/// host's inline ray tracing with programs of our own writer.
///
/// Every candidate the walk stops at (a non-opaque triangle) is accepted
/// unless its primitive index is the ray's refused one, so an opaque
/// instance shows hits the program never judged and a refused primitive
/// shows the walk going on past it.
pub fn ray_query_probe(rays: u32) -> Vec<u8> {
    let mut s = Shader::new(false);
    s.threads = Some([PROBE_GROUP, 1, 1]);
    let (i32t, f32t) = (s.t.i32, s.t.f32);
    let entry = s.entry;
    let x = s.m.i32(0);
    let index = s.op(entry, "threadId.i32", Attr::ReadNone, i32t, 93, &[x]);
    let count = s.m.i32(rays);
    let inside = s.m.cmp(entry, pred::IULT, index, count);
    let (shoot, head, judge, refuse_test, commit, abort, next, after, details, write, done) = (
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
        s.m.new_block(),
    );
    let rays_h = s.root_srv_handle(PROBE_RAYS);
    let structure = s.structure_handle(PROBE_STRUCTURE);
    let hits_h = s.root_uav_handle(PROBE_HITS);
    s.m.terminate(entry, Term::CondBr(inside, shoot, done));

    // The ray: three loads of four dwords.
    let b = shoot;
    let stride = s.m.i32(PROBE_RAY_DWORDS * 4);
    let at = s.m.bin(b, Bin::Mul, index, stride);
    let (iu, all, align) = (s.m.undef(i32t), s.m.i8(0xf), s.m.i32(4));
    let res = s.t.res_ret;
    let mut words = Vec::new();
    for k in 0..3 {
        let off = s.m.i32(16 * k);
        let byte = s.m.bin(b, Bin::Add, at, off);
        let r = s.op(b, "rawBufferLoad.f32", Attr::ReadOnly, res, 139, &[rays_h, byte, iu, all, align]);
        for c in 0..4 {
            words.push(s.m.extract(b, r, c));
        }
    }
    let int = |s: &mut Shader, v: V| s.m.cast(b, Cast::Bitcast, v, i32t);
    let (mask, refused, aborting) = (int(&mut s, words[8]), int(&mut s, words[9]), int(&mut s, words[10]));
    let query = s.ray_query(b, 0);
    let zero = s.m.i32(0);
    let ray = InlineRay {
        flags: zero,
        mask,
        origin: [words[0], words[1], words[2]],
        t_min: words[3],
        direction: [words[4], words[5], words[6]],
        t_max: words[7],
    };
    s.trace_ray_inline(b, query, structure, &ray);
    s.m.terminate(b, Term::Br(head));

    // The walk.
    let more = s.ray_query_proceed(head, query);
    s.m.terminate(head, Term::CondBr(more, judge, after));
    let kind = s.ray_query_state(judge, query, State::CandidateType);
    let triangle = s.m.i32(CANDIDATE_NON_OPAQUE_TRIANGLE);
    let triangle = s.m.cmp(judge, pred::EQ, kind, triangle);
    let stop = s.m.cmp(judge, pred::NE, aborting, zero);
    s.m.terminate(judge, Term::CondBr(stop, abort, refuse_test));
    s.ray_query_abort(abort, query);
    s.m.terminate(abort, Term::Br(next));
    let primitive = s.ray_query_state(refuse_test, query, State::CandidatePrimitiveIndex);
    let kept = s.m.cmp(refuse_test, pred::NE, primitive, refused);
    let kept = s.m.bin(refuse_test, Bin::And, kept, triangle);
    s.m.terminate(refuse_test, Term::CondBr(kept, commit, next));
    s.ray_query_commit_triangle(commit, query);
    s.m.terminate(commit, Term::Br(next));
    s.m.terminate(next, Term::Br(head));

    // What was committed.
    let status = s.ray_query_state(after, query, State::CommittedStatus);
    let hit = s.m.i32(COMMITTED_TRIANGLE_HIT);
    let hit = s.m.cmp(after, pred::EQ, status, hit);
    s.m.terminate(after, Term::CondBr(hit, details, write));
    let d = details;
    let t = s.ray_query_state(d, query, State::CommittedRayT);
    let mut found = vec![s.m.cast(d, Cast::Bitcast, t, i32t)];
    for state in [
        State::CommittedInstanceIndex,
        State::CommittedInstanceId,
        State::CommittedPrimitiveIndex,
        State::CommittedGeometryIndex,
    ] {
        found.push(s.ray_query_state(d, query, state));
    }
    for c in 0..2 {
        let u = s.ray_query_barycentric(d, query, true, c);
        found.push(s.m.cast(d, Cast::Bitcast, u, i32t));
    }
    s.m.terminate(d, Term::Br(write));
    let _ = f32t;
    let mut out = vec![status];
    for v in found {
        let p = s.m.phi(write, i32t);
        s.m.set_phi(p, vec![(v, details), (zero, after)]);
        out.push(p);
    }
    let stride = s.m.i32(PROBE_HIT_DWORDS * 4);
    let at = s.m.bin(write, Bin::Mul, index, stride);
    let void = s.t.void;
    for k in 0..2 {
        let off = s.m.i32(16 * k);
        let byte = s.m.bin(write, Bin::Add, at, off);
        let mut args = vec![hits_h, byte, iu];
        args.extend(&out[4 * k as usize..4 * k as usize + 4]);
        args.extend([all, align]);
        s.op(write, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &args);
    }
    s.m.terminate(write, Term::Br(done));
    s.m.terminate(done, Term::Ret);
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxbc::{Container, FourCC};

    /// Whether a container's bitcode names `name` (a function in its value
    /// symbol table, a struct type): unabbreviated records carry each
    /// character as 6-bit VBR operands, as `bitcode::chars` writes them.
    fn declares(dxil: &[u8], name: &str) -> bool {
        let c = Container::parse(dxil).unwrap();
        contains_record_string(c.part(FourCC::DXIL).unwrap().data, name)
    }

    /// Whether `bits` holds `name` as a run of 6-bit VBR operands at any bit
    /// offset (the writer's unabbreviated records).
    fn contains_record_string(bits: &[u8], name: &str) -> bool {
        let bit = |i: usize| bits[i / 8] >> (i % 8) & 1;
        let total = bits.len() * 8;
        // Each character: one or two 6-bit chunks, low bits first.
        let encode = |c: u8| -> Vec<u8> {
            let c = u32::from(c);
            let mut out = Vec::new();
            if c < 32 {
                out.extend((0..6).map(|k| (c >> k & 1) as u8));
            } else {
                let lo = c & 31 | 32;
                out.extend((0..6).map(|k| (lo >> k & 1) as u8));
                let hi = c >> 5;
                out.extend((0..6).map(|k| (hi >> k & 1) as u8));
            }
            out
        };
        let pattern: Vec<u8> = name.bytes().flat_map(encode).collect();
        (0..total.saturating_sub(pattern.len())).any(|at| pattern.iter().enumerate().all(|(k, &b)| bit(at + k) == b))
    }

    /// The probe is a shader model 6.6 compute program that declares the ray
    /// query operations, the structure as a raw SRV of kind 16 and the
    /// tier 1.1 feature, and that dxil.dll (where there is one) accepts.
    #[test]
    fn the_probe_is_a_valid_ray_query_program() {
        let dxil = ray_query_probe(7);
        let c = Container::parse(&dxil).unwrap();
        // Program version: compute (5), shader model 6.6.
        assert_eq!(c.part(FourCC::DXIL).unwrap().data[..4], [0x66, 0, 5, 0]);
        let sfi0 = u64::from_le_bytes(c.part(FourCC::SFI0).unwrap().data.try_into().unwrap());
        assert_ne!(sfi0 & 0x10_0000, 0, "SFI0 {sfi0:#x}: no ray tracing tier 1.1");
        // PSV0: after the 48-byte runtime info, the binding count and size,
        // then the bindings: the raw SRV t0, the structure t1, the UAV u2.
        let psv = c.part(FourCC::PSV0).unwrap().data;
        let dword = |i: usize| u32::from_le_bytes(psv[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!((dword(13), dword(14)), (3, 24));
        let binding = |n: usize| [0, 1, 2, 3, 4].map(|k| dword(15 + 6 * n + k));
        assert_eq!(binding(0), [4, ROOT_CBV_SPACE, 0, 0, 11]);
        assert_eq!(binding(1), [4, ROOT_CBV_SPACE, 1, 1, 16]);
        assert_eq!(binding(2), [7, ROOT_CBV_SPACE, 2, 2, 11]);
        for name in [
            "dx.op.allocateRayQuery",
            "dx.op.rayQuery_TraceRayInline",
            "dx.op.rayQuery_Proceed.i1",
            "dx.op.rayQuery_Abort",
            "dx.op.rayQuery_CommitNonOpaqueTriangleHit",
            "dx.op.rayQuery_StateScalar.i32",
            "dx.op.rayQuery_StateScalar.f32",
            "dx.op.rayQuery_StateVector.f32",
            "struct.RaytracingAccelerationStructure",
        ] {
            assert!(declares(&dxil, name), "{name} is not declared");
        }
        #[cfg(windows)]
        if let Ok(mut v) =
            super::super::validator::Validator::load(super::super::validator::Validator::default_path().as_deref())
        {
            v.validate(&mut dxil.clone()).unwrap();
        }
    }

    /// Every state scalar the blocks offer, and both barycentric forms, in
    /// one program the validator accepts (the opcodes and overloads are
    /// DXC's: a wrong pair is refused by name).
    #[test]
    fn every_state_call_validates() {
        let mut s = Shader::new(false);
        s.threads = Some([32, 1, 1]);
        let b = s.entry;
        let structure = s.structure_handle(1);
        let out = s.root_uav_handle(2);
        // RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, a constant the query keeps.
        let q = s.ray_query(b, 0x200);
        let (zero, one, ff) = (s.m.i32(0), s.f1(), s.m.i32(0xff));
        let ray = InlineRay {
            flags: zero,
            mask: ff,
            origin: [s.f0(), s.f0(), s.f0()],
            t_min: s.f0(),
            direction: [s.f0(), s.f0(), one],
            t_max: one,
        };
        s.trace_ray_inline(b, q, structure, &ray);
        let _ = s.ray_query_proceed(b, q);
        let mut sum = zero;
        let i32t = s.t.i32;
        for state in [
            State::CommittedStatus,
            State::CandidateType,
            State::CandidateTriangleRayT,
            State::CommittedRayT,
            State::CandidateInstanceIndex,
            State::CandidateInstanceId,
            State::CandidateGeometryIndex,
            State::CandidatePrimitiveIndex,
            State::CommittedInstanceIndex,
            State::CommittedInstanceId,
            State::CommittedGeometryIndex,
            State::CommittedPrimitiveIndex,
            State::CandidateTriangleFrontFace,
            State::CommittedTriangleFrontFace,
            State::RayTMin,
        ] {
            let v = s.ray_query_state(b, q, state);
            let v = match state.opcode().1 {
                Scalar::I32 => v,
                Scalar::F32 => s.m.cast(b, Cast::Bitcast, v, i32t),
                Scalar::I1 => s.m.cast(b, Cast::ZExt, v, i32t),
            };
            sum = s.m.bin(b, Bin::Add, sum, v);
        }
        for committed in [false, true] {
            for c in 0..2 {
                let v = s.ray_query_barycentric(b, q, committed, c);
                let v = s.m.cast(b, Cast::Bitcast, v, i32t);
                sum = s.m.bin(b, Bin::Xor, sum, v);
            }
        }
        s.ray_query_commit_triangle(b, q);
        s.ray_query_abort(b, q);
        let (iu, mask, align) = (s.m.undef(i32t), s.m.i8(1), s.m.i32(4));
        let void = s.t.void;
        s.op(b, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &[out, zero, iu, sum, iu, iu, iu, mask, align]);
        s.m.terminate(b, Term::Ret);
        let mut dxil = s.finish();
        #[cfg(windows)]
        if let Ok(mut v) =
            super::super::validator::Validator::load(super::super::validator::Validator::default_path().as_deref())
        {
            v.validate(&mut dxil).unwrap();
        }
        let _ = &mut dxil;
    }

    /// A program without ray queries declares neither the feature nor a
    /// structure, and its bytes are those the writer made before the ray
    /// query blocks (pinned from the writer at VIctory 5e4d8890).
    #[test]
    fn programs_without_queries_are_unchanged() {
        let plain = || {
            let mut s = Shader::new(false);
            s.threads = Some([32, 1, 1]);
            let b = s.entry;
            let out = s.root_uav_handle(2);
            let i32t = s.t.i32;
            let (zero, iu, mask, align) = (s.m.i32(0), s.m.undef(i32t), s.m.i8(1), s.m.i32(4));
            let void = s.t.void;
            s.op(b, "rawBufferStore.i32", Attr::NoUnwind, void, 140, &[out, zero, iu, zero, iu, iu, iu, mask, align]);
            s.m.terminate(b, Term::Ret);
            s.finish()
        };
        let dxil = plain();
        let fnv = dxil.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        assert_eq!((dxil.len(), fnv), PLAIN_BEFORE);
        let c = Container::parse(&dxil).unwrap();
        let sfi0 = u64::from_le_bytes(c.part(FourCC::SFI0).unwrap().data.try_into().unwrap());
        assert_eq!(sfi0 & 0x10_0000, 0);
        assert!(!declares(&dxil, "struct.RaytracingAccelerationStructure"));
    }

    /// Length and FNV-1a of [`programs_without_queries_are_unchanged`]'s
    /// program as the writer made it before this module.
    const PLAIN_BEFORE: (usize, u64) = (1724, 0x4985_25b9_5481_d11b);

    impl Shader {
        fn f0(&mut self) -> V {
            self.m.f32(0)
        }
        fn f1(&mut self) -> V {
            self.m.f32(1.0f32.to_bits())
        }
    }
}
