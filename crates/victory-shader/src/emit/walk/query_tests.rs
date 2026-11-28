//! The host ray query that stands for a walk taken out of a program
//! (`replace.rs`, `query.rs`), end to end: a synthetic compute program with
//! one node test, which the lowering is told stands for a walk, lowered to
//! DXIL (validated by `dxil.dll` when present) and run on the card against
//! a structure the host's driver built of two triangles. Each thread's
//! results must be what the walk would have handed on: the hit's t and a
//! triangle's node pointer, or the values from before on a miss.
//!
//! The faces a walk culls are judged against the CPU reference of the
//! console's node test ([`crate::bvh::triangle`]): a walk compiled with the
//! culling factor 1 keeps the triangles whose denominator is positive, -1
//! those whose denominator is negative (`replace.rs`), and the query's flags
//! for each must keep the same ones on the host.

use windows::Win32::Graphics::Direct3D12::*;

use super::replace::{CULL_BACK, CULL_FRONT, FIRST_HIT, FORCE_OPAQUE, OutKind, Output, Query, SKIP_PROCEDURAL, TMin};
use super::with_queries;
use crate::bvh::{self, Ray};
use crate::emit::host::{Dispatch, Host, Root, Stride, View};
use crate::emit::intersect_tests::build_compute_shader;
use crate::emit::{SLOT_STRUCTURE, USER_STRUCTURE};
use crate::ir::{self, Reg};
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};

/// Threads of the one group, each with its own record.
const THREADS: u32 = 32;
/// Dwords of a thread's record: the node pointer (two dwords), the ray's
/// extent, origin, direction and inverse direction, whether the thread runs
/// the instruction, three unused.
const RECORD: usize = 16;
/// What the result registers hold before the instruction.
const BEFORE: [u32; 4] = [1, 2, 3, 4];
/// What the query leaves in the hit's node pointer register at a hit.
const MARK: u32 = 1;

/// The program: per thread, the record's operands in v1-v12, the node test's
/// results in v13-v16 (in the threads whose record says so), stored at 16
/// bytes a thread. Dword 16 of its table is the structure's view, which the
/// program loads and does not use, as the title's do once the walk is out.
fn program() -> Vec<u32> {
    let mut w = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0xF408_0302,
        0xFA00_0020, // s_load_dwordx4 s[12:15], s[4:5], 0x20
        0xF408_0402,
        0xFA00_0040, // s_load_dwordx4 s[16:19], s[4:5], 0x40
        0x3428_0086, // v_lshlrev_b32 v20, 6, v0
        0xE038_1000,
        0x8002_0114, // buffer_load_dwordx4 v[1:4], v20, s[8:11], 0 offen
        0xE038_1010,
        0x8002_0514, // buffer_load_dwordx4 v[5:8], v20, s[8:11], 0 offen offset:16
        0xE038_1020,
        0x8002_0914, // buffer_load_dwordx4 v[9:12], v20, s[8:11], 0 offen offset:32
        0xE030_1030,
        0x8002_1114, // buffer_load_dword v17, v20, s[8:11], 0 offen offset:48
    ];
    for k in 0..4u32 {
        // v_mov_b32 v13+k, 1+k
        w.push(0x7E00_0200 | (13 + k) << 17 | (0x81 + k));
    }
    w.extend([
        0xBE98_03FF,
        0x0000_0000, // s_mov_b32 s24, 0
        0xBE99_0B81, // s_brev_b32 s25, 1
        0xBE9A_03C1, // s_mov_b32 s26, -1
        0xBE9B_03FF,
        0x8900_03FF, // s_mov_b32 s27, 0x890003ff
        0xBE94_037E, // s_mov_b32 s20, exec_lo
        0x7DAA_2280, // v_cmpx_ne_u32 exec_lo, 0, v17
        // image_bvh64_intersect_ray v[13:16], [v1 .. v12], s[24:27]
        0xF19C_9F07,
        0x0006_0D01,
        0x0504_0302,
        0x0908_0706,
        0x000C_0B0A,
        0xBEFE_0314, // s_mov_b32 exec_lo, s20
        0x342A_0084, // v_lshlrev_b32 v21, 4, v0
        0xE078_1000,
        0x8003_0D15, // buffer_store_dwordx4 v[13:16], v21, s[12:15], 0 offen
        0xBF81_0000, // s_endpgm
    ]);
    w
}

fn native() -> NativeProgram {
    let table = |kind, at| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords: at,
        lower_bound: 0,
        stride: 4,
        dwords: 4,
        user_data: 4,
        slot: 1,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 22,
        layout: vec![
            RootParam { root: RootType::Constants, table_offset_dwords: 0, sgprs: 4, base_register: 0, user_data: 0, slot: 0 },
            RootParam { root: RootType::Table, table_offset_dwords: 0, sgprs: 1, base_register: 0, user_data: 4, slot: 1 },
        ],
        used: vec![table(ResourceType::Srv, 0), table(ResourceType::Uav, 8), table(ResourceType::Srv, 16)],
        io: StageIo::Cs(ComputeIo { rsrc2: 5 << 1, threads: [THREADS, 1, 1] }),
        code: program().iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// The query that stands for the program's node test, with `flags`: the
/// site's ray at its entry; the results t (v13), the hit's node pointer
/// (v14, [`MARK`] at a hit) and two left as they were (v15, v16).
fn query(native: &NativeProgram, flags: u32) -> Query {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let insts = || p.blocks.iter().flat_map(|b| b.insts.iter());
    let site = insts().find(|i| i.name().starts_with("image_bvh")).expect("the node test");
    let structure = insts()
        .find(|i| i.name() == "s_load_dwordx4" && i.write(Reg::S(16)).is_some())
        .expect("the structure's view load");
    let kinds = [OutKind::T, OutKind::Hit(MARK), OutKind::Entry, OutKind::Entry];
    let outputs = (0..4u8).map(|k| Output { reg: Reg::V(13 + k), kind: kinds[usize::from(k)] }).collect();
    Query {
        offset: site.machine.offset,
        flags,
        mask: 0xff,
        t_min: TMin::Const(0),
        extent: Reg::V(3),
        origin: [Reg::V(4), Reg::V(5), Reg::V(6)],
        direction: [Reg::V(7), Reg::V(8), Reg::V(9)],
        descriptor_load: structure.machine.offset,
        outputs,
    }
}

/// The triangles: `A` in the plane z = 2, `B` in z = 4 with the other
/// winding. Seen along +z (x right, y up), A runs counter-clockwise: the
/// node test's denominator is negative for it and DXR calls it a back face.
const A: [[f32; 3]; 3] = [[0.0, 0.0, 2.0], [1.0, 0.0, 2.0], [0.0, 1.0, 2.0]];
const B: [[f32; 3]; 3] = [[0.0, 0.0, 4.0], [0.0, 1.0, 4.0], [1.0, 0.0, 4.0]];

/// A thread's ray (origin, direction, extent); `None` leaves it out.
type Case = Option<([f32; 3], [f32; 3], f32)>;

fn cases() -> Vec<Case> {
    let quarter = |z| [0.25, 0.25, z];
    let up = [0.0, 0.0, 1.0];
    vec![
        // Through both from below: A first.
        Some((quarter(0.0), up, 100.0)),
        // From between them: B.
        Some((quarter(3.0), up, 100.0)),
        // Too short to reach A.
        Some((quarter(0.0), up, 1.5)),
        // Past both.
        Some(([5.0, 5.0, 0.0], up, 100.0)),
        // Left out.
        None,
        // Through both from above: B first.
        Some((quarter(10.0), [0.0, 0.0, -1.0], 100.0)),
        // At a slant from below.
        Some(([0.1, 0.1, 0.0], [0.02, 0.03, 1.0], 100.0)),
    ]
}

/// What the walk the query stands for hands on for `case` (the culling
/// factor `cull`, as the walk was compiled; `first`: whether it stops at its
/// first hit, in which case any hit's t will do): t (float bits) and whether
/// something was hit, or `None` for a miss.
fn walk(case: &Case, cull: i32) -> Vec<Option<u32>> {
    let Some((origin, direction, extent)) = *case else {
        return vec![None];
    };
    let ray = Ray::new(origin, direction, extent);
    let mut hits = Vec::new();
    for p in [A, B] {
        let [num, den, _, _] = bvh::triangle(&p, &ray);
        if den == 0.0 || num.is_nan() || den.is_nan() {
            continue;
        }
        let t = num / den;
        // (the walk: rejected when t < t min, t >= the extent, or culled)
        let culled = (den.signum() as i32) * cull < 0;
        if t >= 0.0 && t < extent && !culled {
            hits.push(t);
        }
    }
    hits.sort_by(f32::total_cmp);
    if hits.is_empty() { vec![None] } else { hits.iter().map(|t| Some(t.to_bits())).collect() }
}

/// A query with each of the flags a walk taken out can have runs on the card
/// against the host's structure and hands on what the walk would: the
/// closest hit's t, or any hit's for a walk that stops at its first, the
/// node pointer register's mark at a hit and its value from before on a
/// miss, and the faces the walk's culling factor culls culled.
#[test]
fn a_walk_taken_out_is_a_host_ray_query_that_hits_what_the_walk_would() {
    let native = native();
    // (the reference must see both windings, or the culling proves nothing)
    let ray = Ray::new([0.25, 0.25, 0.0], [0.0, 0.0, 1.0], 100.0);
    assert!(bvh::triangle(&A, &ray)[1] * bvh::triangle(&B, &ray)[1] < 0.0, "the windings");
    let variants = [
        ("closest", FORCE_OPAQUE | SKIP_PROCEDURAL, 0, false),
        ("closest, culling 1", FORCE_OPAQUE | SKIP_PROCEDURAL | CULL_BACK, 1, false),
        ("closest, culling -1", FORCE_OPAQUE | SKIP_PROCEDURAL | CULL_FRONT, -1, false),
        ("first hit", FORCE_OPAQUE | SKIP_PROCEDURAL | FIRST_HIT, 0, true),
    ];
    let mut shaders = Vec::new();
    for &(label, flags, cull, first) in &variants {
        let q = query(&native, flags);
        let (shader, _) =
            with_queries(vec![q], || build_compute_shader(&native)).unwrap_or_else(|e| panic!("{label}: {e}"));
        // (the host is told where the title's structure is: the table's dword 16)
        let asks = shader.user_data.iter().find(|u| u.root == RootType::Other(USER_STRUCTURE));
        assert!(asks.is_some_and(|u| u.slot == 1 && u.dword == 16), "{label}: {:?}", shader.user_data);
        shaders.push((label, shader, cull, first));
    }
    let _one = crate::emit::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; skipped");
        return;
    };
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    // SAFETY: the feature query writes the structure of the given size.
    let tier = unsafe {
        host.device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS5,
            (&raw mut options).cast(),
            size_of_val(&options) as u32,
        )
    };
    if tier.is_err() || options.RaytracingTier.0 < D3D12_RAYTRACING_TIER_1_1.0 {
        eprintln!("no ray tracing tier 1.1; skipped");
        return;
    }
    let cases = cases();
    let mut records = vec![0u32; THREADS as usize * RECORD];
    for (t, c) in cases.iter().enumerate() {
        let Some((origin, direction, extent)) = *c else { continue };
        let r = &mut records[RECORD * t..RECORD * (t + 1)];
        r[2] = extent.to_bits();
        for a in 0..3 {
            r[3 + a] = origin[a].to_bits();
            r[6 + a] = direction[a].to_bits();
            r[9 + a] = (1.0 / direction[a]).to_bits();
        }
        r[12] = 1;
    }
    for (label, shader, cull, first) in &shaders {
        let (root, pso) = host.compute_pipeline(shader).unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        // The structure's root parameter: after the user data, in the order the
        // host's root signature lists root views (by slot, then kind).
        let mut views: Vec<(u8, u8)> = shader.root_cbvs.iter().map(|&r| (r, 0)).collect();
        views.extend(shader.root_srvs.iter().map(|&r| (r, 1)));
        views.push((SLOT_STRUCTURE, 1));
        views.extend(shader.root_uavs.iter().map(|&r| (r, 2)));
        views.sort_unstable();
        views.dedup();
        let at = 1 + views.iter().position(|&v| v == (SLOT_STRUCTURE, 1)).expect("the structure") as u32;
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![
                        (at, Root::Structure(vec![A, B])),
                        (0, Root::Constants(vec![0; shader.user_data_dwords().max(5) as usize])),
                    ],
                    views: &[
                        View::Buffer { data: &records, stride: Stride::Raw },
                        View::RawUav(4 * THREADS),
                        View::Buffer { data: &[0; 4], stride: Stride::Raw },
                    ],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        for t in 0..THREADS as usize {
            let case = cases.get(t).copied().flatten();
            let got = &out[0][4 * t..4 * t + 4];
            let hits = walk(&case, *cull);
            let ok = match (case, hits[0]) {
                // (left out, or a miss: what the registers held)
                (None, _) | (Some(_), None) => got == BEFORE,
                (Some(_), Some(closest)) => {
                    let near = |want: u32| (f32::from_bits(got[0]) - f32::from_bits(want)).abs() < 1e-4;
                    let t_ok = if *first { hits.iter().flatten().any(|&h| near(h)) } else { near(closest) };
                    t_ok && got[1..] == [MARK, BEFORE[2], BEFORE[3]]
                }
            };
            assert!(
                ok,
                "{label}, thread {t}: {case:?}\n got {got:x?} (t {})\nwant {:?} or {BEFORE:x?} on a miss",
                f32::from_bits(got[0]),
                hits.iter().map(|h| h.map(f32::from_bits)).collect::<Vec<_>>()
            );
        }
    }
}
