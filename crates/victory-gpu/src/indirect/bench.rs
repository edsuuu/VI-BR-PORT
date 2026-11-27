//! A frame's worth of one-record indirect draws on the host GPU, clamped one
//! at a time and in windows, recorded as `main` does (the scratch buffer
//! back to the common state after every execution, the clamp charging the
//! budget record by record) and as this tree does (readable until the next
//! clamp, one charge per group). Ignored by default; run it with
//! `cargo test -p victory-gpu --release --lib indirect::bench -- --ignored --nocapture`.
//! It needs no title files: the draws are tiny triangles of ours.

use super::*;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIFactory4,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::{Interface, PCSTR};

/// Draws a frame (the title's gameplay frames hold about 3,200 indirect
/// executions, #107), records a window holds, and how far apart the
/// records are (the title keeps them 32 bytes apart).
const DRAWS: u32 = 3200;
const WINDOW: u32 = 32;
const STEP: u32 = 32;
const RUNS: usize = 9;

const SHADERS: &str = r#"
#define RS "RootFlags(0)"
[RootSignature(RS)]
float4 vs(uint v : SV_VertexID, uint i : SV_InstanceID) : SV_Position {
float2 p = float2((v & 1) ? 0.05 : 0.0, (v & 2) ? 0.05 : 0.0) - 0.5;
return float4(p, 0.5, 1);
}
float4 ps() : SV_Target { return float4(1, 0.5, 0.25, 1); }
"#;

fn compile(entry: &str, target: &str) -> Vec<u8> {
    let entry = std::ffi::CString::new(entry).unwrap();
    let target = std::ffi::CString::new(target).unwrap();
    let mut code: Option<ID3DBlob> = None;
    // SAFETY: the source and names outlive the call.
    unsafe {
        D3DCompile(
            SHADERS.as_ptr().cast(),
            SHADERS.len(),
            s!("bench"),
            None,
            None,
            PCSTR(entry.as_ptr().cast()),
            PCSTR(target.as_ptr().cast()),
            0,
            0,
            &mut code,
            None,
        )
        .unwrap();
        let code = code.unwrap();
        std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
            .to_vec()
    }
}

/// The clamp as it was before it charged the budget once per group
/// (record by record, every thread racing for the budget word), kept
/// here to measure against.
const OLD_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)"
cbuffer Record : register(b0) {
uint kind;
uint stride;
uint records;
uint args_offset;
// Bit 0: a count buffer bounds the records. Bit 1: the records are a
// window clamped ahead of their draws: one that asks for more than can
// run is left empty and costs nothing.
uint has_count;
uint count_offset;
};
ByteAddressBuffer src_args : register(t0);
ByteAddressBuffer count_buf : register(t1);
RWByteAddressBuffer dst_args : register(u0);
// The list's work so far (vertex instances, threads): past the budget,
// records run empty.
RWByteAddressBuffer budget : register(u1);
static const uint BUDGET = 1u << 27;
// Groups of one dispatch; more run empty.
static const uint GROUPS = 1u << 20;
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
uint eff_count = records;
bool ahead = (has_count & 2u) != 0;
if ((has_count & 1u) != 0) {
    eff_count = min(eff_count, count_buf.Load(count_offset));
}
if (id.x >= eff_count) return;
uint src_at = args_offset + id.x * stride;
uint dst_at = id.x * stride;
uint cost;
uint2 draw_counts = uint2(0, 0);
uint3 groups = uint3(0, 0, 0);
if (kind <= 1) {
    // Draws: vertex or index count, then instances.
    uint2 asked = src_args.Load2(src_at);
    if (ahead && (asked.x > (1u << 24) || asked.y > (1u << 24))) {
        asked = uint2(0, 0);
    }
    uint count = min(asked.x, 1u << 24);
    uint instances = min(asked.y, (1u << 26) / max(count, 1u));
    cost = count * instances;
    draw_counts = uint2(count, instances);
} else if (kind == 2) {
    uint3 g = min(src_args.Load3(src_at), uint3(65535u, 65535u, 65535u));
    // x * y fits 32 bits; the whole count may not.
    if (g.z != 0 && g.x * g.y > GROUPS / g.z) {
        g = uint3(0, 0, 0);
    }
    cost = g.x * g.y * g.z * 64u;
    groups = g;
} else {
    // Mesh dispatches: the console's are one-dimensional.
    uint3 g = src_args.Load3(src_at);
    // Ahead of its draw, a record over the most a dispatch runs is a
    // slot the title has not filled (the title leaves all
    // ones there): empty, and not counted below either.
    if (ahead && g.x > 65535u) {
        g = uint3(0, 0, 0);
    }
    // Diagnostics, in the budget buffer's next words: the largest
    // group count a mesh record asked for, and the groups asked in all.
    uint was;
    budget.InterlockedMax(4, g.x, was);
    budget.InterlockedAdd(8, min(g.x, 1u << 24), was);
    g = uint3(min(g.x, 65535u), min(g.y, 1u), min(g.z, 1u));
    cost = g.x * g.y * g.z * 128u;
    groups = g;
}
bool over = false;
if (cost > 0) {
    uint before;
    budget.InterlockedOr(0, 0, before);
    [allow_uav_condition]
    while (before < BUDGET) {
        uint next = (BUDGET - before <= cost) ? BUDGET : (before + cost);
        uint prev;
        budget.InterlockedCompareExchange(0, before, next, prev);
        if (prev == before) break;
        before = prev;
    }
    over = before >= BUDGET || BUDGET - before < cost;
}
if (kind <= 1) {
    if (over) {
        draw_counts = uint2(0, 0);
    }
    dst_args.Store2(dst_at, draw_counts);
    for (uint off = 8; off < stride; off += 4) {
        dst_args.Store(dst_at + off, src_args.Load(src_at + off));
    }
} else {
    if (over) {
        groups = uint3(0, 0, 0);
    }
    dst_args.Store3(dst_at, groups);
    for (uint off = 12; off < stride; off += 4) {
        dst_args.Store(dst_at + off, src_args.Load(src_at + off));
    }
}
}
"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Barriers {
    /// Back to the common state after every execution (as before).
    Old,
    /// Readable as arguments until the next clamp, common at the end.
    New,
}

#[test]
#[ignore = "benchmark; see the module notes"]
fn bench_indirect_draw_barriers() {
    let Some(host) = super::host() else {
        eprintln!("no host GPU");
        return;
    };
    let device = &host.device;
    let clamps = [
        make_from(device, OLD_SOURCE).expect("the old clamp"),
        make_for(device).expect("the clamp"),
    ];
    // SAFETY: plain D3D12 object creation with valid descriptions; the
    // objects outlive every list recorded and run below.
    unsafe {
        let adapter = {
            let luid = device.GetAdapterLuid();
            CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                .and_then(|f| {
                    f.EnumAdapterByLuid::<windows::Win32::Graphics::Dxgi::IDXGIAdapter1>(luid)
                })
                .and_then(|a| a.GetDesc1())
                .map(|d| {
                    String::from_utf16_lossy(&d.Description)
                        .trim_end_matches('\0')
                        .to_owned()
                })
                .unwrap_or_else(|_| "an unnamed adapter".to_owned())
        };
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                ..Default::default()
            })
            .unwrap();
        let allocator: ID3D12CommandAllocator = device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .unwrap();
        let list: ID3D12GraphicsCommandList = device
            .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
            .unwrap();
        list.Close().unwrap();
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
        let event = CreateEventW(None, false, false, None).unwrap();
        let frequency = queue.GetTimestampFrequency().unwrap() as f64;

        // The pipeline: tiny triangles into a small target.
        let (vs, ps) = (compile("vs", "vs_5_1"), compile("ps", "ps_5_1"));
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs).unwrap();
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vs.as_ptr().cast(),
                BytecodeLength: vs.len(),
            },
            PS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: ps.as_ptr().cast(),
                BytecodeLength: ps.len(),
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        desc.BlendState.RenderTarget[0].RenderTargetWriteMask =
            D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
        desc.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        let pso: ID3D12PipelineState = device.CreateGraphicsPipelineState(&desc).unwrap();
        let target_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 256,
            Height: 256,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            ..Default::default()
        };
        let mut target: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &target_desc,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                None,
                &mut target,
            )
            .unwrap();
        let target = target.unwrap();
        let rtvs: ID3D12DescriptorHeap = device
            .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: 1,
                ..Default::default()
            })
            .unwrap();
        let rtv = rtvs.GetCPUDescriptorHandleForHeapStart();
        device.CreateRenderTargetView(&target, None, rtv);

        // The title's records: three vertices, one instance, 32 bytes apart.
        let (src, src_ptr) = host.shared_buffer(u64::from(DRAWS * STEP)).unwrap();
        for i in 0..DRAWS as usize {
            let words = [3u32, 1, 0, 0];
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                src_ptr.add(i * STEP as usize),
                16,
            );
        }
        let (budget, budget_ptr) = host.shared_buffer(256).unwrap();
        let budget_gpu = budget.GetGPUVirtualAddress();
        // The same words in the card's own memory (zeroed when made; the
        // runs below charge far less than the budget).
        let card_budget = host.uav_buffer(256).unwrap();
        let card_budget_gpu = card_budget.GetGPUVirtualAddress();
        let scratch = host
            .uav_buffer(u64::from(WINDOW * STEP).max(64 << 10))
            .unwrap();
        let signature = {
            let arg = D3D12_INDIRECT_ARGUMENT_DESC {
                Type: D3D12_INDIRECT_ARGUMENT_TYPE_DRAW,
                ..Default::default()
            };
            let desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: 16,
                NumArgumentDescs: 1,
                pArgumentDescs: &arg,
                NodeMask: 0,
            };
            let mut sig: Option<ID3D12CommandSignature> = None;
            device
                .CreateCommandSignature(&desc, None::<&ID3D12RootSignature>, &mut sig)
                .unwrap();
            sig.unwrap()
        };
        let stamps: ID3D12QueryHeap = {
            let mut heap: Option<ID3D12QueryHeap> = None;
            device
                .CreateQueryHeap(
                    &D3D12_QUERY_HEAP_DESC {
                        Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                        Count: 2,
                        NodeMask: 0,
                    },
                    &mut heap,
                )
                .unwrap();
            heap.unwrap()
        };
        let mut back: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_READBACK,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: 16,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut back,
            )
            .unwrap();
        let back = back.unwrap();
        let mut submitted = 0u64;

        // One frame's lists: GPU ms (timestamps around the list), CPU ms
        // recording it, ms from submission to the end of its run.
        let mut frame = |barriers: Barriers, clamp: &Clamp, window: u32, card: bool| -> [f64; 3] {
            allocator.Reset().unwrap();
            list.Reset(&allocator, None).unwrap();
            let recording = std::time::Instant::now();
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            list.OMSetRenderTargets(1, Some(&rtv), false, None);
            list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: 256.0,
                Height: 256.0,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: 256,
                bottom: 256,
            }]);
            list.IASetPrimitiveTopology(
                windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            );
            list.SetGraphicsRootSignature(&root);
            let mut state = D3D12_RESOURCE_STATE_COMMON;
            for first in (0..DRAWS).step_by(window as usize) {
                let records = window.min(DRAWS - first);
                assert!(record_clamp_as(
                    clamp,
                    &list,
                    (&scratch, state),
                    if card { card_budget_gpu } else { budget_gpu },
                    (&src, u64::from(first * STEP)),
                    None,
                    (0, STEP, records),
                    window > 1,
                ));
                state = D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
                list.SetPipelineState(&pso);
                for k in 0..records {
                    list.ExecuteIndirect(&signature, 1, &scratch, u64::from(k * STEP), None, 0);
                    if barriers == Barriers::Old {
                        list.ResourceBarrier(&[transition(
                            &scratch,
                            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                            D3D12_RESOURCE_STATE_COMMON,
                        )]);
                        state = D3D12_RESOURCE_STATE_COMMON;
                    }
                }
            }
            if state != D3D12_RESOURCE_STATE_COMMON {
                list.ResourceBarrier(&[transition(&scratch, state, D3D12_RESOURCE_STATE_COMMON)]);
            }
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
            list.Close().unwrap();
            let recorded = recording.elapsed().as_secs_f64() * 1e3;
            let running = std::time::Instant::now();
            queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().unwrap())]);
            submitted += 1;
            queue.Signal(&fence, submitted).unwrap();
            fence.SetEventOnCompletion(submitted, event).unwrap();
            WaitForSingleObject(event, INFINITE);
            let ran = running.elapsed().as_secs_f64() * 1e3;
            let mut data = std::ptr::null_mut();
            back.Map(0, None, Some(&mut data)).unwrap();
            let t = std::slice::from_raw_parts(data.cast::<u64>(), 2);
            let gpu = t[1].saturating_sub(t[0]) as f64 / frequency * 1e3;
            back.Unmap(0, None);
            (budget_ptr as *mut u32).write_volatile(0);
            [gpu, recorded, ran]
        };

        // What `main` records, then this branch; the windows a step at a
        // time (barriers, then the clamp's one charge per group).
        let (old, new) = (&clamps[0], &clamps[1]);
        // The last two keep the budget in the card's memory instead of
        // memory the CPU maps (where the list's budget is now).
        let cases: [(&str, Barriers, &Clamp, u32, bool); 7] = [
            ("one at a time, main", Barriers::Old, old, 1, false),
            ("one at a time, this", Barriers::New, new, 1, false),
            ("windows of 32, main", Barriers::Old, old, WINDOW, false),
            ("windows, new barriers", Barriers::New, old, WINDOW, false),
            ("windows of 32, this", Barriers::New, new, WINDOW, false),
            ("one at a time, card", Barriers::New, new, 1, true),
            ("windows of 32, card", Barriers::New, new, WINDOW, true),
        ];
        // Warm up, then the cases in turn, so drift falls on all alike.
        for &(_, barriers, clamp, window, card) in &cases {
            frame(barriers, clamp, window, card);
        }
        let mut times: Vec<Vec<[f64; 3]>> = vec![Vec::new(); cases.len()];
        for _ in 0..RUNS {
            for (i, &(_, barriers, clamp, window, card)) in cases.iter().enumerate() {
                times[i].push(frame(barriers, clamp, window, card));
            }
        }
        let median = |v: &[[f64; 3]], k: usize| {
            let mut x: Vec<f64> = v.iter().map(|t| t[k]).collect();
            x.sort_by(f64::total_cmp);
            x[x.len() / 2]
        };
        eprintln!(
            "indirect draw barriers on {adapter}: {DRAWS} one-record draws a frame, \
             medians of {RUNS} frames"
        );
        for (i, (name, ..)) in cases.iter().enumerate() {
            let gpu = median(&times[i], 0);
            eprintln!(
                "  {name:<22} GPU {gpu:7.2} ms ({:5.2} us a draw), recording {:6.2} ms, \
                 submit to done {:7.2} ms",
                gpu * 1e3 / f64::from(DRAWS),
                median(&times[i], 1),
                median(&times[i], 2),
            );
        }
        for (from, to) in [(0, 1), (2, 3), (3, 4), (2, 4), (1, 5), (4, 6)] {
            let (a, b) = (median(&times[from], 0), median(&times[to], 0));
            eprintln!(
                "  {} -> {}: GPU time {:+.1} %",
                cases[from].0,
                cases[to].0,
                (b / a - 1.0) * 100.0
            );
        }
    }
}

/// The same tiny triangles with a root constant each (their column on the
/// target), for executions whose records carry it.
const SHADERS_WITH_CONSTANT: &str = r#"
#define RS "RootConstants(num32BitConstants=1, b0)"
cbuffer Draw : register(b0) { uint column; };
[RootSignature(RS)]
float4 vs(uint v : SV_VertexID) : SV_Position {
float2 p = float2((v & 1) ? 0.05 : 0.0, (v & 2) ? 0.05 : 0.0) - 0.5 + float2((column & 15) * 0.05, 0.0);
return float4(p, 0.5, 1);
}
float4 ps() : SV_Target { return float4(1, 0.5, 0.25, 1); }
"#;

/// What a frame's one-record indirect draws cost the card by how they are
/// issued: as direct draws, one execution a record (the title's way) with
/// the records in the card's memory or in memory shared with the PC (as
/// title memory used in place is), and several records an execution, with
/// and without a root constant carried in each record. Ignored by default;
/// `cargo test -p victory-gpu --release --lib indirect::bench::bench_indirect_merged -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark; see the module notes"]
fn bench_indirect_merged() {
    let Some(host) = super::host() else {
        eprintln!("no host GPU");
        return;
    };
    let device = &host.device;
    let compile_from = |source: &str, entry: &str, target: &str| -> Vec<u8> {
        let entry = std::ffi::CString::new(entry).unwrap();
        let target = std::ffi::CString::new(target).unwrap();
        let mut code: Option<ID3DBlob> = None;
        // SAFETY: the source and names outlive the call.
        unsafe {
            D3DCompile(source.as_ptr().cast(), source.len(), s!("bench"), None, None, PCSTR(entry.as_ptr().cast()), PCSTR(target.as_ptr().cast()), 0, 0, &mut code, None).unwrap();
            let code = code.unwrap();
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize()).to_vec()
        }
    };
    // SAFETY: plain D3D12 object creation with valid descriptions; the
    // objects outlive every list recorded and run below.
    unsafe {
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
            .unwrap();
        let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
        let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).unwrap();
        list.Close().unwrap();
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
        let event = CreateEventW(None, false, false, None).unwrap();
        let frequency = queue.GetTimestampFrequency().unwrap() as f64;

        let pipeline = |source: &str| -> (ID3D12RootSignature, ID3D12PipelineState) {
            let (vs, ps) = (compile_from(source, "vs", "vs_5_1"), compile_from(source, "ps", "ps_5_1"));
            let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs).unwrap();
            let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                VS: D3D12_SHADER_BYTECODE { pShaderBytecode: vs.as_ptr().cast(), BytecodeLength: vs.len() },
                PS: D3D12_SHADER_BYTECODE { pShaderBytecode: ps.as_ptr().cast(), BytecodeLength: ps.len() },
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            desc.BlendState.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
            desc.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
            let pso: ID3D12PipelineState = device.CreateGraphicsPipelineState(&desc).unwrap();
            (root, pso)
        };
        let (plain_root, plain) = pipeline(SHADERS);
        let (constant_root, with_constant) = pipeline(SHADERS_WITH_CONSTANT);

        let mut target: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: 256,
                    Height: 256,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                None,
                &mut target,
            )
            .unwrap();
        let target = target.unwrap();
        let rtvs: ID3D12DescriptorHeap = device
            .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV, NumDescriptors: 1, ..Default::default() })
            .unwrap();
        let rtv = rtvs.GetCPUDescriptorHandleForHeapStart();
        device.CreateRenderTargetView(&target, None, rtv);

        // The records, 32 bytes apart: a word for the root constant, then
        // three vertices and one instance. In shared memory, and a copy of
        // them in the card's own.
        let bytes = u64::from(DRAWS * STEP);
        let (shared, shared_ptr) = host.shared_buffer(bytes).unwrap();
        for i in 0..DRAWS as usize {
            let words = [i as u32, 3u32, 1, 0, 0, 0, 0, 0];
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), shared_ptr.add(i * STEP as usize), 32);
        }
        let card = host.uav_buffer(bytes).unwrap();

        let signature = |arguments: &[D3D12_INDIRECT_ARGUMENT_DESC], root: Option<&ID3D12RootSignature>| -> ID3D12CommandSignature {
            let desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: STEP,
                NumArgumentDescs: arguments.len() as u32,
                pArgumentDescs: arguments.as_ptr(),
                NodeMask: 0,
            };
            let mut sig: Option<ID3D12CommandSignature> = None;
            device.CreateCommandSignature(&desc, root, &mut sig).unwrap();
            sig.unwrap()
        };
        let draw = D3D12_INDIRECT_ARGUMENT_DESC { Type: D3D12_INDIRECT_ARGUMENT_TYPE_DRAW, ..Default::default() };
        let constant = D3D12_INDIRECT_ARGUMENT_DESC {
            Type: D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT,
            Anonymous: D3D12_INDIRECT_ARGUMENT_DESC_0 {
                Constant: D3D12_INDIRECT_ARGUMENT_DESC_0_1 { RootParameterIndex: 0, DestOffsetIn32BitValues: 0, Num32BitValuesToSet: 1 },
            },
        };
        // The draw alone (its record after the constant's word), and the
        // constant with it.
        let draws = signature(&[draw], None);
        let constant_draws = signature(&[constant, draw], Some(&constant_root));

        let stamps: ID3D12QueryHeap = {
            let mut heap: Option<ID3D12QueryHeap> = None;
            device
                .CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 }, &mut heap)
                .unwrap();
            heap.unwrap()
        };
        let mut back: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: 16,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut back,
            )
            .unwrap();
        let back = back.unwrap();
        let mut submitted = 0u64;
        let mut run = |record: &dyn Fn(&ID3D12GraphicsCommandList)| -> [f64; 2] {
            allocator.Reset().unwrap();
            list.Reset(&allocator, None).unwrap();
            let recording = std::time::Instant::now();
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            list.OMSetRenderTargets(1, Some(&rtv), false, None);
            list.RSSetViewports(&[D3D12_VIEWPORT { Width: 256.0, Height: 256.0, MaxDepth: 1.0, ..Default::default() }]);
            list.RSSetScissorRects(&[RECT { left: 0, top: 0, right: 256, bottom: 256 }]);
            list.IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            record(&list);
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
            list.Close().unwrap();
            let recorded = recording.elapsed().as_secs_f64() * 1e3;
            queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().unwrap())]);
            submitted += 1;
            queue.Signal(&fence, submitted).unwrap();
            fence.SetEventOnCompletion(submitted, event).unwrap();
            WaitForSingleObject(event, INFINITE);
            let mut data = std::ptr::null_mut();
            back.Map(0, None, Some(&mut data)).unwrap();
            let t = std::slice::from_raw_parts(data.cast::<u64>(), 2);
            let gpu = t[1].saturating_sub(t[0]) as f64 / frequency * 1e3;
            back.Unmap(0, None);
            [gpu, recorded]
        };
        // The card's copy of the records (both buffers leave the common
        // state implicitly and come back to it when the list has run).
        run(&|l| l.CopyBufferRegion(&card, 0, &shared, 0, bytes));

        // Records an execution, from which buffer, with the constant in the
        // record (or set before each execution: `None` for direct draws).
        #[derive(Clone, Copy)]
        enum Way {
            Direct,
            Plain(u32, bool),
            ConstantSet(bool),
            ConstantCarried(u32, bool),
        }
        let cases: [(&str, Way); 12] = [
            ("direct draws", Way::Direct),
            ("1 a call, card", Way::Plain(1, true)),
            ("1 a call, shared", Way::Plain(1, false)),
            ("8 a call, card", Way::Plain(8, true)),
            ("32 a call, card", Way::Plain(32, true)),
            ("32 a call, shared", Way::Plain(32, false)),
            ("all in one, card", Way::Plain(DRAWS, true)),
            ("constant set, 1 a call, card", Way::ConstantSet(true)),
            ("constant set, 1 a call, shared", Way::ConstantSet(false)),
            ("constant carried, 8 a call, card", Way::ConstantCarried(8, true)),
            ("constant carried, 32 a call, card", Way::ConstantCarried(32, true)),
            ("constant carried, 32 a call, shared", Way::ConstantCarried(32, false)),
        ];
        let frame = |l: &ID3D12GraphicsCommandList, way: Way| {
            let records = |in_card: bool| if in_card { &card } else { &shared };
            match way {
                Way::Direct => {
                    l.SetGraphicsRootSignature(&plain_root);
                    l.SetPipelineState(&plain);
                    for _ in 0..DRAWS {
                        l.DrawInstanced(3, 1, 0, 0);
                    }
                }
                Way::Plain(each, in_card) => {
                    l.SetGraphicsRootSignature(&plain_root);
                    l.SetPipelineState(&plain);
                    for first in (0..DRAWS).step_by(each as usize) {
                        l.ExecuteIndirect(&draws, each.min(DRAWS - first), records(in_card), u64::from(first * STEP + 4), None, 0);
                    }
                }
                Way::ConstantSet(in_card) => {
                    l.SetGraphicsRootSignature(&constant_root);
                    l.SetPipelineState(&with_constant);
                    for i in 0..DRAWS {
                        l.SetGraphicsRoot32BitConstant(0, i, 0);
                        l.ExecuteIndirect(&draws, 1, records(in_card), u64::from(i * STEP + 4), None, 0);
                    }
                }
                Way::ConstantCarried(each, in_card) => {
                    l.SetGraphicsRootSignature(&constant_root);
                    l.SetPipelineState(&with_constant);
                    for first in (0..DRAWS).step_by(each as usize) {
                        l.ExecuteIndirect(&constant_draws, each.min(DRAWS - first), records(in_card), u64::from(first * STEP), None, 0);
                    }
                }
            }
        };
        for &(_, way) in &cases {
            run(&|l| frame(l, way));
        }
        let mut times: Vec<Vec<[f64; 2]>> = vec![Vec::new(); cases.len()];
        for _ in 0..RUNS {
            for (i, &(_, way)) in cases.iter().enumerate() {
                times[i].push(run(&|l| frame(l, way)));
            }
        }
        // The least of the runs too: another program may hold the card.
        let of = |v: &[[f64; 2]], k: usize| {
            let mut x: Vec<f64> = v.iter().map(|t| t[k]).collect();
            x.sort_by(f64::total_cmp);
            (x[x.len() / 2], x[0])
        };
        eprintln!("indirect draws by how they are issued: {DRAWS} draws a frame, {RUNS} frames each");
        for (i, (name, _)) in cases.iter().enumerate() {
            let (gpu, least) = of(&times[i], 0);
            eprintln!(
                "  {name:<36} GPU median {gpu:7.2} ms ({:5.2} us a draw), least {least:7.2} ms, recording {:6.2} ms",
                gpu * 1e3 / f64::from(DRAWS),
                of(&times[i], 1).0,
            );
        }
    }
}

/// A compute program of next to nothing: one word stored.
const TOUCH_CS: &str = r#"
#define RS "UAV(u0)"
RWByteAddressBuffer words : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() { words.Store(0, 1); }
"#;

/// What it costs the card to have something between a frame's small draws:
/// a wait for every earlier shader write (the barrier the layer records for
/// each of the title's barrier groups), a state change of a small buffer, a
/// timestamp (what the timing runs put behind every command), a compute
/// dispatch of next to nothing (what a clamp of indirect arguments is), and
/// the dispatch between two waits (a clamp as it is recorded). The draws
/// alone overlap on the card; the difference to them, by how many times the
/// thing was put in, is what one costs. Ignored by default;
/// `cargo test -p victory-gpu --release --lib indirect::bench::bench_draw_interruptions -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark; see the module notes"]
fn bench_draw_interruptions() {
    let Some(host) = super::host() else {
        eprintln!("no host GPU");
        return;
    };
    let device = &host.device;
    let compile_from = |source: &str, entry: &str, target: &str| -> Vec<u8> {
        let entry = std::ffi::CString::new(entry).unwrap();
        let target = std::ffi::CString::new(target).unwrap();
        let mut code: Option<ID3DBlob> = None;
        // SAFETY: the source and names outlive the call.
        unsafe {
            D3DCompile(source.as_ptr().cast(), source.len(), s!("bench"), None, None, PCSTR(entry.as_ptr().cast()), PCSTR(target.as_ptr().cast()), 0, 0, &mut code, None).unwrap();
            let code = code.unwrap();
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize()).to_vec()
        }
    };
    // SAFETY: plain D3D12 object creation with valid descriptions; the
    // objects outlive every list recorded and run below.
    unsafe {
        let queue: ID3D12CommandQueue = device
            .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
            .unwrap();
        let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
        let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).unwrap();
        list.Close().unwrap();
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
        let event = CreateEventW(None, false, false, None).unwrap();
        let frequency = queue.GetTimestampFrequency().unwrap() as f64;

        let (vs, ps) = (compile_from(SHADERS, "vs", "vs_5_1"), compile_from(SHADERS, "ps", "ps_5_1"));
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs).unwrap();
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            VS: D3D12_SHADER_BYTECODE { pShaderBytecode: vs.as_ptr().cast(), BytecodeLength: vs.len() },
            PS: D3D12_SHADER_BYTECODE { pShaderBytecode: ps.as_ptr().cast(), BytecodeLength: ps.len() },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        desc.BlendState.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
        desc.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        let pso: ID3D12PipelineState = device.CreateGraphicsPipelineState(&desc).unwrap();
        let cs = compile_from(TOUCH_CS, "main", "cs_5_1");
        let touch_root: ID3D12RootSignature = device.CreateRootSignature(0, &cs).unwrap();
        let touch: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&touch_root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cs.as_ptr().cast(), BytecodeLength: cs.len() },
                ..Default::default()
            })
            .unwrap();
        let words = host.uav_buffer(256).unwrap();
        let words_gpu = words.GetGPUVirtualAddress();
        let small = host.uav_buffer(64 << 10).unwrap();

        let mut target: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: 256,
                    Height: 256,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                None,
                &mut target,
            )
            .unwrap();
        let target = target.unwrap();
        let rtvs: ID3D12DescriptorHeap = device
            .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV, NumDescriptors: 1, ..Default::default() })
            .unwrap();
        let rtv = rtvs.GetCPUDescriptorHandleForHeapStart();
        device.CreateRenderTargetView(&target, None, rtv);

        // Two stamps around the frame, and one for behind every draw.
        let stamps: ID3D12QueryHeap = {
            let mut heap: Option<ID3D12QueryHeap> = None;
            device
                .CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: DRAWS + 2, NodeMask: 0 }, &mut heap)
                .unwrap();
            heap.unwrap()
        };
        let mut back: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: 16,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut back,
            )
            .unwrap();
        let back = back.unwrap();

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Between {
            Nothing,
            Wait,
            StateChange,
            Stamp,
            Dispatch,
            WaitDispatchWait,
        }
        // What, and after every how many draws.
        let cases: [(&str, Between, u32); 11] = [
            ("the draws alone", Between::Nothing, 1),
            ("a wait behind every draw", Between::Wait, 1),
            ("a wait behind every 8th", Between::Wait, 8),
            ("a state change behind every draw", Between::StateChange, 1),
            ("a state change behind every 8th", Between::StateChange, 8),
            ("a timestamp behind every draw", Between::Stamp, 1),
            ("a dispatch behind every draw", Between::Dispatch, 1),
            ("a dispatch behind every 8th", Between::Dispatch, 8),
            ("wait, dispatch, wait behind every draw", Between::WaitDispatchWait, 1),
            ("wait, dispatch, wait behind every 8th", Between::WaitDispatchWait, 8),
            ("wait, dispatch, wait behind every 32nd", Between::WaitDispatchWait, 32),
        ];
        let mut submitted = 0u64;
        let mut run = |between: Between, every: u32| -> [f64; 2] {
            allocator.Reset().unwrap();
            list.Reset(&allocator, None).unwrap();
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            list.OMSetRenderTargets(1, Some(&rtv), false, None);
            list.RSSetViewports(&[D3D12_VIEWPORT { Width: 256.0, Height: 256.0, MaxDepth: 1.0, ..Default::default() }]);
            list.RSSetScissorRects(&[RECT { left: 0, top: 0, right: 256, bottom: 256 }]);
            list.IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            list.SetGraphicsRootSignature(&root);
            list.SetPipelineState(&pso);
            list.SetComputeRootSignature(&touch_root);
            list.SetComputeRootUnorderedAccessView(0, words_gpu);
            let mut state = D3D12_RESOURCE_STATE_COMMON;
            for i in 0..DRAWS {
                list.DrawInstanced(3, 1, 0, 0);
                if (i + 1) % every != 0 {
                    continue;
                }
                match between {
                    Between::Nothing => {}
                    Between::Wait => list.ResourceBarrier(&[uav_barrier()]),
                    Between::StateChange => {
                        let next = if state == D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT {
                            D3D12_RESOURCE_STATE_UNORDERED_ACCESS
                        } else {
                            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT
                        };
                        list.ResourceBarrier(&[transition(&small, state, next)]);
                        state = next;
                    }
                    Between::Stamp => list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 2 + i),
                    Between::Dispatch => {
                        list.SetPipelineState(&touch);
                        list.Dispatch(1, 1, 1);
                        list.SetPipelineState(&pso);
                    }
                    Between::WaitDispatchWait => {
                        list.ResourceBarrier(&[uav_barrier()]);
                        list.SetPipelineState(&touch);
                        list.Dispatch(1, 1, 1);
                        list.ResourceBarrier(&[uav_barrier()]);
                        list.SetPipelineState(&pso);
                    }
                }
            }
            if state != D3D12_RESOURCE_STATE_COMMON {
                list.ResourceBarrier(&[transition(&small, state, D3D12_RESOURCE_STATE_COMMON)]);
            }
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &back, 0);
            list.Close().unwrap();
            let running = std::time::Instant::now();
            queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>().unwrap())]);
            submitted += 1;
            queue.Signal(&fence, submitted).unwrap();
            fence.SetEventOnCompletion(submitted, event).unwrap();
            WaitForSingleObject(event, INFINITE);
            let ran = running.elapsed().as_secs_f64() * 1e3;
            let mut data = std::ptr::null_mut();
            back.Map(0, None, Some(&mut data)).unwrap();
            let t = std::slice::from_raw_parts(data.cast::<u64>(), 2);
            let gpu = t[1].saturating_sub(t[0]) as f64 / frequency * 1e3;
            back.Unmap(0, None);
            [gpu, ran]
        };
        for &(_, between, every) in &cases {
            run(between, every);
        }
        let mut times: Vec<Vec<[f64; 2]>> = vec![Vec::new(); cases.len()];
        for _ in 0..RUNS {
            for (i, &(_, between, every)) in cases.iter().enumerate() {
                times[i].push(run(between, every));
            }
        }
        // The median and the least of the runs: another program may hold the card.
        let of = |v: &[[f64; 2]], k: usize| {
            let mut x: Vec<f64> = v.iter().map(|t| t[k]).collect();
            x.sort_by(f64::total_cmp);
            (x[x.len() / 2], x[0])
        };
        let base = of(&times[0], 0).1;
        eprintln!("what stands between {DRAWS} small draws of a frame, {RUNS} frames each");
        for (i, &(name, _, every)) in cases.iter().enumerate() {
            let (gpu, least) = of(&times[i], 0);
            let (done, done_least) = of(&times[i], 1);
            eprintln!(
                "  {name:<40} GPU median {gpu:7.2} ms, least {least:7.2} ms ({:6.2} us each over the draws alone); submit to done {done:7.2} ms, least {done_least:7.2}",
                (least - base) * 1e3 / f64::from(DRAWS / every),
            );
        }
    }
}
