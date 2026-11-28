//! The host's ray tracing end to end on the card: structures built by the
//! host's driver, walked by a program of our own writer with ray queries;
//! and what building and refitting them costs.

use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;

use super::build::{Gpu, Structure, instance, moved, triangles, uav_barrier};
use victory_shader::emit::{
    COMMITTED_NOTHING, COMMITTED_TRIANGLE_HIT, PROBE_GROUP, PROBE_HIT_DWORDS, PROBE_HITS, PROBE_RAY_DWORDS,
    PROBE_RAYS, PROBE_STRUCTURE, ROOT_CBV_SPACE, ray_query_probe,
};

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// The probe's root signature: its two root SRVs and its root UAV, in
/// [`ROOT_CBV_SPACE`] as the translation binds root views.
fn probe_root(gpu: &Gpu) -> ID3D12RootSignature {
    let view = |ty, register: u8| D3D12_ROOT_PARAMETER {
        ParameterType: ty,
        Anonymous: D3D12_ROOT_PARAMETER_0 {
            Descriptor: D3D12_ROOT_DESCRIPTOR {
                ShaderRegister: u32::from(register),
                RegisterSpace: ROOT_CBV_SPACE,
            },
        },
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
    };
    let params = [
        view(D3D12_ROOT_PARAMETER_TYPE_SRV, PROBE_RAYS),
        view(D3D12_ROOT_PARAMETER_TYPE_SRV, PROBE_STRUCTURE),
        view(D3D12_ROOT_PARAMETER_TYPE_UAV, PROBE_HITS),
    ];
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: params.len() as u32,
        pParameters: params.as_ptr(),
        ..Default::default()
    };
    let (mut blob, mut errors): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    // SAFETY: the description and its parameters outlive the calls.
    unsafe {
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, Some(&mut errors))
            .expect("root signature");
        let blob = blob.unwrap();
        let bytes = std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        gpu.device.CreateRootSignature(0, bytes).expect("root signature")
    }
}

fn probe_pipeline(gpu: &Gpu, root: &ID3D12RootSignature, dxil: &[u8]) -> ID3D12PipelineState {
    // SAFETY: the description and the code outlive the call.
    unsafe {
        gpu.device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: dxil.as_ptr().cast(),
                    BytecodeLength: dxil.len(),
                },
                ..Default::default()
            })
            .expect("the driver refuses the ray query program")
    }
}

/// A ray of the probe and what it must commit.
struct Case {
    origin: [f32; 3],
    t_min: f32,
    direction: [f32; 3],
    t_max: f32,
    mask: u32,
    refused: u32,
    abort: bool,
    /// `None`: nothing committed; else t, instance index and id,
    /// primitive, geometry, barycentrics.
    want: Option<(f32, u32, u32, u32, u32, [f32; 2])>,
}

const NONE: u32 = !0;

fn case(origin: [f32; 3], direction: [f32; 3]) -> Case {
    Case {
        origin,
        t_min: 0.0,
        direction,
        t_max: 100.0,
        mask: 0xff,
        refused: NONE,
        abort: false,
        want: None,
    }
}

/// The scene: one bottom level of two geometries, three triangles, all
/// non-opaque (the program judges every candidate):
///
/// - geometry 0, primitive 0: (0,0,0) (1,0,0) (0,1,0), in the plane z = 0;
/// - geometry 0, primitive 1: the same at z = 2;
/// - geometry 1, primitive 0: the same at z = 4.
///
/// Two instances of it: index 0 where it is, id 7, mask 1; index 1 moved
/// by 10 along x, id 42, mask 2, forced opaque (its hits are committed
/// without the program).
///
/// Each ray's answer, worked out by hand: a ray along z from (x, y, z0)
/// meets the plane z = c at t = (c - z0) / dz, the barycentrics are the
/// hit's (x, y) within the instance (the weights of the second and third
/// vertices).
fn cases() -> Vec<Case> {
    let quarter = [0.25, 0.25, -1.0];
    let up = [0.0, 0.0, 1.0];
    vec![
        // The nearest triangle.
        Case { want: Some((1.0, 0, 7, 0, 0, [0.25, 0.25])), ..case(quarter, up) },
        // Primitive 0 refused (in both geometries): the one at z = 2.
        Case { refused: 0, want: Some((3.0, 0, 7, 1, 0, [0.25, 0.25])), ..case(quarter, up) },
        // The same, too short to reach it.
        Case { refused: 0, t_max: 2.5, ..case(quarter, up) },
        // From between the planes: geometry 1.
        Case { want: Some((1.0, 0, 7, 0, 1, [0.25, 0.25])), ..case([0.25, 0.25, 3.0], up) },
        // The opaque instance: its hit is committed although the program
        // would refuse primitive 0.
        Case { refused: 0, want: Some((1.0, 1, 42, 0, 0, [0.5, 0.25])), ..case([10.5, 0.25, -1.0], up) },
        // Aborted at the first candidate: nothing committed.
        Case { abort: true, ..case(quarter, up) },
        // Past every triangle.
        case([5.0, 5.0, -1.0], up),
        // At a slant: (0.25, 0.25, -1) + t (0.1, 0.2, 1) meets z = 0 at t = 1.
        Case { want: Some((1.0, 0, 7, 0, 0, [0.35, 0.45])), ..case(quarter, [0.1, 0.2, 1.0]) },
        // From above, downwards: geometry 1 first (no face is culled).
        Case { want: Some((1.0, 0, 7, 0, 1, [0.25, 0.25])), ..case([0.25, 0.25, 5.0], [0.0, 0.0, -1.0]) },
        // The opaque instance's mask is 2: a ray of mask 1 misses it.
        Case { mask: 1, ..case([10.5, 0.25, -1.0], up) },
        // Instance 0's mask is 1: a ray of mask 2 misses it.
        Case { mask: 2, ..case(quarter, up) },
        // Starting at t 1.5: past z = 0.
        Case { t_min: 1.5, want: Some((3.0, 0, 7, 1, 0, [0.25, 0.25])), ..case(quarter, up) },
    ]
}

/// A program of our own writer walks structures the host's driver built,
/// with ray queries, and commits what the triangles, instances, masks,
/// flags and its own judgement say, as worked out by hand.
#[test]
fn ray_queries_of_our_writer_hit_what_the_host_built() {
    let Some(gpu) = Gpu::new() else {
        eprintln!("no hardware adapter with ray tracing tier 1.1; skipped");
        return;
    };
    eprintln!("on {}", gpu.card);
    let triangle = |z: f32| [0.0, 0.0, z, 1.0, 0.0, z, 0.0, 1.0, z];
    let vertices = [triangle(0.0), triangle(2.0), triangle(4.0)].concat();
    let vertices = gpu.resident(&f32s(&vertices));
    // SAFETY: plain queries of buffers of ours.
    let at = unsafe { vertices.GetGPUVirtualAddress() };
    let bottom = gpu.bottom(
        vec![
            triangles(at, 6, None, D3D12_RAYTRACING_GEOMETRY_FLAG_NONE),
            triangles(at + 6 * 12, 3, None, D3D12_RAYTRACING_GEOMETRY_FLAG_NONE),
        ],
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
    );
    let opaque = D3D12_RAYTRACING_INSTANCE_FLAG_FORCE_OPAQUE.0 as u8;
    let instances = [
        instance(moved(0.0, 0.0, 0.0), 7, 1, 0, bottom.address()),
        instance(moved(10.0, 0.0, 0.0), 42, 2, opaque, bottom.address()),
    ]
    .concat();
    let instances = gpu.resident(&instances);
    // SAFETY: as above.
    let top = gpu.top(
        unsafe { instances.GetGPUVirtualAddress() },
        2,
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
    );

    let cases = cases();
    let mut rays = Vec::new();
    for c in &cases {
        let mut words: Vec<u32> = [c.origin.as_slice(), &[c.t_min], &c.direction, &[c.t_max]]
            .concat()
            .iter()
            .map(|f| f.to_bits())
            .collect();
        words.extend([c.mask, c.refused, u32::from(c.abort), 0]);
        assert_eq!(words.len() as u32, PROBE_RAY_DWORDS);
        rays.extend(words.iter().flat_map(|w| w.to_le_bytes()));
    }
    let rays = gpu.resident(&rays);
    let hit_bytes = u64::from(PROBE_HIT_DWORDS * 4) * cases.len() as u64;
    let hits = gpu.buffer(
        hit_bytes,
        D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
    );
    let dxil = ray_query_probe(cases.len() as u32);
    let root = probe_root(&gpu);
    let pso = probe_pipeline(&gpu, &root, &dxil);
    gpu.run(|list| unsafe {
        // SAFETY: everything recorded lives until `run` has waited.
        bottom.record(list, false);
        top.record(list, false);
        list.SetComputeRootSignature(&root);
        list.SetPipelineState(&pso);
        list.SetComputeRootShaderResourceView(u32::from(PROBE_RAYS), rays.GetGPUVirtualAddress());
        list.SetComputeRootShaderResourceView(u32::from(PROBE_STRUCTURE), top.address());
        list.SetComputeRootUnorderedAccessView(u32::from(PROBE_HITS), hits.GetGPUVirtualAddress());
        list.Dispatch((cases.len() as u32).div_ceil(PROBE_GROUP), 1, 1);
        list.ResourceBarrier(&[uav_barrier(&hits)]);
    });
    let out = gpu.read(&hits, hit_bytes, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    let words: Vec<u32> = out.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
    let mut wrong = Vec::new();
    for (n, (c, hit)) in cases.iter().zip(words.chunks(PROBE_HIT_DWORDS as usize)).enumerate() {
        let f = |w: u32| f32::from_bits(w);
        eprintln!(
            "ray {n}: status {} t {} instance {} id {} primitive {} geometry {} barycentrics {} {}",
            hit[0],
            f(hit[1]),
            hit[2],
            hit[3],
            hit[4],
            hit[5],
            f(hit[6]),
            f(hit[7])
        );
        let ok = match c.want {
            None => hit[0] == COMMITTED_NOTHING,
            Some((t, index, id, primitive, geometry, [u, v])) => {
                hit[0] == COMMITTED_TRIANGLE_HIT
                    && (f(hit[1]) - t).abs() < 1e-5
                    && [hit[2], hit[3], hit[4], hit[5]] == [index, id, primitive, geometry]
                    && (f(hit[6]) - u).abs() < 1e-5
                    && (f(hit[7]) - v).abs() < 1e-5
            }
        };
        if !ok {
            wrong.push(n);
        }
    }
    assert!(wrong.is_empty(), "rays {wrong:?} committed what they should not");
}

/// The probe's rays `cases` shot at the structure at `structure` (built
/// before the list runs), and the dwords each ray committed.
fn shoot(gpu: &Gpu, structure: u64, cases: &[Case], before: impl FnOnce(&ID3D12GraphicsCommandList4)) -> Vec<Vec<u32>> {
    let mut rays = Vec::new();
    for c in cases {
        let mut words: Vec<u32> = [c.origin.as_slice(), &[c.t_min], &c.direction, &[c.t_max]]
            .concat()
            .iter()
            .map(|f| f.to_bits())
            .collect();
        words.extend([c.mask, c.refused, u32::from(c.abort), 0]);
        rays.extend(words.iter().flat_map(|w| w.to_le_bytes()));
    }
    let rays = gpu.resident(&rays);
    let hit_bytes = u64::from(PROBE_HIT_DWORDS * 4) * cases.len() as u64;
    let hits = gpu.buffer(
        hit_bytes,
        D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
    );
    let dxil = ray_query_probe(cases.len() as u32);
    let root = probe_root(gpu);
    let pso = probe_pipeline(gpu, &root, &dxil);
    gpu.run(|list| unsafe {
        // SAFETY: everything recorded lives until `run` has waited.
        before(list);
        list.SetComputeRootSignature(&root);
        list.SetPipelineState(&pso);
        list.SetComputeRootShaderResourceView(u32::from(PROBE_RAYS), rays.GetGPUVirtualAddress());
        list.SetComputeRootShaderResourceView(u32::from(PROBE_STRUCTURE), structure);
        list.SetComputeRootUnorderedAccessView(u32::from(PROBE_HITS), hits.GetGPUVirtualAddress());
        list.Dispatch((cases.len() as u32).div_ceil(PROBE_GROUP), 1, 1);
        list.ResourceBarrier(&[uav_barrier(&hits)]);
    });
    let out = gpu.read(&hits, hit_bytes, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    let words: Vec<u32> = out.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
    words.chunks(PROBE_HIT_DWORDS as usize).map(<[u32]>::to_vec).collect()
}

/// The empty top level a translated program's ray query reads where the
/// host has no structure of its own (`super::query`): built once on a queue
/// of its own and waited for, then read by a list on another queue; every
/// ray misses, the ones that hit the scene above too.
#[test]
fn the_empty_top_level_is_built_on_its_own_queue_and_every_ray_misses() {
    let Some(gpu) = Gpu::new() else {
        eprintln!("no hardware adapter with ray tracing tier 1.1; skipped");
        return;
    };
    let device: windows::Win32::Graphics::Direct3D12::ID3D12Device =
        windows::core::Interface::cast(&gpu.device).expect("a device");
    let (empty, at) = super::query::empty_on(&device).expect("the empty top level");
    // SAFETY: a plain query of a buffer of ours.
    assert_eq!(at, unsafe { empty.GetGPUVirtualAddress() });
    assert!(at.is_multiple_of(256));
    let cases = cases();
    let hits = shoot(&gpu, at, &cases, |_| {});
    for (n, hit) in hits.iter().enumerate() {
        assert_eq!(hit[0], COMMITTED_NOTHING, "ray {n} committed something in the empty structure");
    }
}

/// A grid of `quads` squares (two triangles each) in the plane z = `z`, as
/// vertices and `u32` indices.
fn grid(quads: u32, z: f32) -> (Vec<f32>, Vec<u32>) {
    let w = (f64::from(quads).sqrt().ceil() as u32).max(1);
    let h = quads.div_ceil(w);
    let mut vertices = Vec::with_capacity(((w + 1) * (h + 1) * 3) as usize);
    for y in 0..=h {
        for x in 0..=w {
            // A little relief, so the boxes are not all flat.
            let bump = ((x * 7 + y * 13) % 5) as f32 * 0.01;
            vertices.extend([x as f32, y as f32, z + bump]);
        }
    }
    let mut indices = Vec::with_capacity((w * h * 6) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = y * (w + 1) + x;
            let (b, c, d) = (a + 1, a + w + 1, a + w + 2);
            indices.extend([a, b, c, b, d, c]);
        }
    }
    (vertices, indices)
}

/// The median of what `time` measures over `runs` runs, in milliseconds.
fn median_ms(runs: usize, mut time: impl FnMut() -> f64) -> (f64, f64, f64) {
    let mut all: Vec<f64> = (0..runs).map(|_| time() * 1e3).collect();
    all.sort_by(f64::total_cmp);
    (all[0], all[runs / 2], all[runs - 1])
}

/// The GPU time of what `record` records, from timestamps before and after.
fn gpu_seconds(gpu: &Gpu, record: impl FnOnce(&ID3D12GraphicsCommandList4)) -> f64 {
    let back = gpu.buffer(16, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST);
    gpu.run(|list| unsafe {
        // SAFETY: the query heap and buffer live as long as `gpu`.
        list.EndQuery(&gpu.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
        record(list);
        list.EndQuery(&gpu.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
        list.ResolveQueryData(&gpu.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
    });
    let mut stamps = [0u64; 2];
    // SAFETY: the mapping is 16 bytes and the list has finished.
    unsafe {
        let mut p = std::ptr::null_mut();
        back.Map(0, None, Some(&mut p)).expect("map");
        std::ptr::copy_nonoverlapping(p.cast::<u64>(), stamps.as_mut_ptr(), 2);
        back.Unmap(0, None);
    }
    gpu.stamp_seconds(&stamps, 0, 1)
}

const RUNS: usize = 7;

/// What builds and refits cost on this card: bottom levels of a thousand,
/// a hundred thousand and a million triangles (built for fast walks and
/// for fast builds, both refittable), a thousand bottom levels of a
/// thousand triangles in one list, and top levels of a thousand and ten
/// thousand instances. The fastest, median and slowest of seven runs.
#[test]
#[ignore = "measurement: cargo test --release -p victory-gpu dxr -- --ignored --nocapture"]
fn build_and_refit_times() {
    let Some(gpu) = Gpu::new() else {
        eprintln!("no hardware adapter with ray tracing tier 1.1; skipped");
        return;
    };
    println!("on {}, {} runs each: fastest / median / slowest, ms", gpu.card, RUNS);
    let update = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE;
    let trace = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE;
    let fast = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_BUILD;
    let show = |what: &str, (lo, mid, hi): (f64, f64, f64)| println!("{what:<58} {lo:8.3} {mid:8.3} {hi:8.3}");
    let mut small = None;
    for wanted in [1_000u32, 100_000, 1_000_000] {
        let (vertices, indices) = grid(wanted / 2, 0.0);
        let tris = indices.len() / 3;
        // The refit's input: the same mesh moved a little, as a skinned one
        // would be from one frame to the next.
        let shifted: Vec<f32> = vertices.iter().enumerate().map(|(i, v)| if i % 3 == 2 { v + 0.05 } else { *v }).collect();
        let (a, b) = (gpu.resident(&f32s(&vertices)), gpu.resident(&f32s(&shifted)));
        let ix = gpu.resident(&indices.iter().flat_map(|i| i.to_le_bytes()).collect::<Vec<u8>>());
        let count = (vertices.len() / 3) as u32;
        for (name, flags) in [("fast trace", trace | update), ("fast build", fast | update)] {
            // SAFETY: plain queries of buffers of ours.
            let geometry = |v: &windows::Win32::Graphics::Direct3D12::ID3D12Resource| unsafe {
                triangles(
                    v.GetGPUVirtualAddress(),
                    count,
                    Some((ix.GetGPUVirtualAddress(), indices.len() as u32)),
                    D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
                )
            };
            let s = gpu.bottom(vec![geometry(&a)], flags);
            let sizes = s.sizes;
            show(&format!("bottom {tris} triangles, {name}: build"), median_ms(RUNS, || gpu_seconds(&gpu, |l| s.record(l, false))));
            // A refit reads the inputs named when it is recorded: the moved mesh.
            let moved_s = Structure::rebound(&s, vec![geometry(&b)]);
            show(&format!("bottom {tris} triangles, {name}: refit"), median_ms(RUNS, || gpu_seconds(&gpu, |l| moved_s.record(l, true))));
            println!(
                "    sizes: result {} KiB, scratch {} KiB, update scratch {} KiB",
                sizes.ResultDataMaxSizeInBytes >> 10,
                sizes.ScratchDataSizeInBytes >> 10,
                sizes.UpdateScratchDataSizeInBytes >> 10
            );
            if wanted == 1_000 && name == "fast trace" {
                small = Some((moved_s, a.clone(), ix.clone(), count, indices.len() as u32));
            }
        }
    }
    let (small, a, ix, count, index_count) = small.unwrap();
    // A thousand bottom levels of a thousand triangles each, one list, each
    // with its own buffers (as a frame of many meshes would be).
    {
        // SAFETY: plain queries of buffers of ours.
        let geometry = unsafe {
            triangles(a.GetGPUVirtualAddress(), count, Some((ix.GetGPUVirtualAddress(), index_count)), D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE)
        };
        let many: Vec<Structure> = (0..1000).map(|_| gpu.bottom(vec![geometry], trace | update)).collect();
        let record = |l: &ID3D12GraphicsCommandList4, update: bool| {
            // The builds may overlap: one barrier after them all.
            for s in &many {
                s.record_unfenced(l, update);
            }
            let barriers: Vec<_> = many.iter().map(|s| uav_barrier(&s.result)).collect();
            // SAFETY: the structures live as long as the list's run.
            unsafe { l.ResourceBarrier(&barriers) };
        };
        show("1000 bottoms x 1000 triangles, fast trace: build", median_ms(RUNS, || gpu_seconds(&gpu, |l| record(l, false))));
        show("1000 bottoms x 1000 triangles, fast trace: refit", median_ms(RUNS, || gpu_seconds(&gpu, |l| record(l, true))));
    }
    // Top levels over the small bottom level, the instances on a grid.
    for n in [1_000u32, 10_000] {
        let side = (f64::from(n).sqrt().ceil()) as u32;
        let descs: Vec<u8> = (0..n)
            .flat_map(|i| instance(moved((i % side) as f32 * 40.0, (i / side) as f32 * 40.0, 0.0), i, 0xff, 0, small.address()))
            .collect();
        let descs = gpu.resident(&descs);
        for (name, flags) in [("fast trace", trace | update), ("fast build", fast | update)] {
            // SAFETY: as above.
            let top = gpu.top(unsafe { descs.GetGPUVirtualAddress() }, n, flags);
            show(&format!("top {n} instances, {name}: build"), median_ms(RUNS, || gpu_seconds(&gpu, |l| top.record(l, false))));
            show(&format!("top {n} instances, {name}: refit"), median_ms(RUNS, || gpu_seconds(&gpu, |l| top.record(l, true))));
            println!("    sizes: result {} KiB, scratch {} KiB", top.sizes.ResultDataMaxSizeInBytes >> 10, top.sizes.ScratchDataSizeInBytes >> 10);
        }
    }
}
