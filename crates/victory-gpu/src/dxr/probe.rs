//! The probe (`VICTORY_DXR_PROBE=1`, off by default; only with
//! `VICTORY_DXR_NATIVE`): a picture of the host's top level, and the same
//! rays fired at the console's.
//!
//! Once every `VICTORY_DXR_PROBE_FRAMES` frames (300 by default), right after
//! the host builds the top level the probe follows (`VICTORY_DXR_PROBE_TLAS`,
//! the title's destination address in hex, or else the one with the most
//! instances seen), a program of our own writer
//! (`victory_shader::emit::ray_query_probe`) fires one ray per pixel of a
//! 320 x 180 panorama (all directions, rows from the up axis
//! `VICTORY_DXR_PROBE_UP`, `z` by default, to its opposite) from an eye
//! (`VICTORY_DXR_PROBE_EYE=x,y,z`, or else the middle of the instances'
//! places, each axis's median) at it with ray queries. The distances and the
//! instances hit are saved as two PNGs in `VICTORY_DXR_PROBE_DIR` (by
//! default the folder of the frame pictures, `VICTORY_CAPTURE`, or else
//! `dxr-probe` in the working directory).
//!
//! The critic's item 6: the probe also fires a grid of 24 x 12 of the same
//! rays at the **console's** top level of the same destination with a walk
//! on the CPU ([`super::walk`]), reading the console's structures back from
//! the card page by page (`crate::memory::home`): the top level's pages in
//! the list that built it, right after the title's builder wrote them, the
//! bottom levels' as the walk finds it needs them, in rounds of at most 256
//! pages (16 MiB) at the title's next builder calls. Then one line compares
//! the two ray by ray (hit or miss, distance, instance id and contribution,
//! geometry, primitive, barycentrics; the console's facing), and a text file
//! beside the pictures lists every ray. What moves between the frames of the
//! rounds (a refit bottom level) differs by that much.
//!
//! The critic's item 3 (the console's bottom levels lie in a frame of their
//! own, turned and scaled against the title's object space): of the hits
//! both sides make at the same distance in the same instance, the line also
//! says what that frame is (the instance node's turn times the title's
//! instance transform: the same axes, the axes swapped, turned over or
//! scaled, or something else; mirrored or not), how often the triangle's
//! flip bit goes with the mirroring, what Direct3D's front face is from the
//! console's winding, and the bottom levels' frame words. The host's own
//! facing is compared once the probe's program writes it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;

use super::native::Structure;
use super::top::INSTANCE;
use super::walk::{self, Pages, Walked};
use crate::host::HostList;
use victory_shader::emit::{
    COMMITTED_TRIANGLE_HIT, PROBE_GROUP, PROBE_HIT_DWORDS, PROBE_HITS, PROBE_RAY_DWORDS, PROBE_RAYS, PROBE_STRUCTURE,
    ROOT_CBV_SPACE, ray_query_probe,
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const RAYS: u32 = WIDTH * HEIGHT;
/// The rays also fired at the console's structure: a grid over the picture.
const GRID: (u32, u32) = (24, 12);
/// The console's memory is read back in pages of this size (`crate::memory`).
const PAGE: u64 = 1 << 16;
/// Pages read back at most a round, and rounds at most a probe.
const ROUND_PAGES: usize = 256;
const ROUNDS: u32 = 16;
const T_MAX: f32 = 1.0e5;
/// Frames after a submission its results are read in (every submission is
/// waited for long before).
const SETTLE: u64 = 8;
/// Frames after which a probe whose list was never submitted is given up.
const GIVE_UP: u64 = 900;

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_DXR_PROBE").is_some_and(|v| v == "1");
        if on && !super::native_on() {
            tracing::warn!("VICTORY_DXR_PROBE=1 needs VICTORY_DXR_NATIVE: no probe");
        }
        on && super::native_on() && super::tier() >= super::TIER_1_1
    })
}

fn every() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("VICTORY_DXR_PROBE_FRAMES").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(300).max(30)
    })
}

/// `VICTORY_DXR_PROBE_DIR`; else the folder of the title's frame pictures
/// (`VICTORY_CAPTURE`, `crate::capture`), so the probe's lie beside the
/// title's of about the same frames; else `dxr-probe` in the working
/// directory.
fn folder() -> PathBuf {
    std::env::var_os("VICTORY_DXR_PROBE_DIR")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("VICTORY_CAPTURE").filter(|v| !v.is_empty()))
        .map_or_else(|| PathBuf::from("dxr-probe"), PathBuf::from)
}

fn eye_asked() -> Option<[f32; 3]> {
    let v = std::env::var("VICTORY_DXR_PROBE_EYE").ok()?;
    let xyz: Vec<f32> = v.split(',').filter_map(|c| c.trim().parse().ok()).collect();
    (xyz.len() == 3).then(|| [xyz[0], xyz[1], xyz[2]])
}

fn up() -> usize {
    match std::env::var("VICTORY_DXR_PROBE_UP").as_deref().map(str::trim) {
        Ok("x") => 0,
        Ok("y") => 1,
        _ => 2,
    }
}

fn chosen() -> Option<u64> {
    let v = std::env::var("VICTORY_DXR_PROBE_TLAS").ok()?;
    u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()
}

/// The direction of pixel (`x`, `y`) of the panorama.
fn direction(x: u32, y: u32) -> [f32; 3] {
    use std::f32::consts::PI;
    let phi = 2.0 * PI * (x as f32 + 0.5) / WIDTH as f32 - PI;
    let theta = PI / 2.0 - PI * (y as f32 + 0.5) / HEIGHT as f32;
    let u = up();
    let (a, b) = ((u + 1) % 3, (u + 2) % 3);
    let mut d = [0.0; 3];
    d[u] = theta.sin();
    d[a] = theta.cos() * phi.cos();
    d[b] = theta.cos() * phi.sin();
    d
}

/// The pixels of the grid's rays.
fn grid() -> Vec<(u32, u32)> {
    let (gx, gy) = GRID;
    (0..gy).flat_map(|j| (0..gx).map(move |i| (i * WIDTH / gx + WIDTH / gx / 2, j * HEIGHT / gy + HEIGHT / gy / 2))).collect()
}

/// What the probe's program runs with.
struct Kit {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    hits: ID3D12Resource,
    hits_back: ID3D12Resource,
    hits_data: *const u32,
    pages_back: ID3D12Resource,
    pages_data: *const u8,
}

// SAFETY: D3D12 objects are free-threaded; the mappings are read only, under
// the probe's lock.
unsafe impl Send for Kit {}
// SAFETY: as above.
unsafe impl Sync for Kit {}

fn buffer(device: &ID3D12Device, size: u64, heap: D3D12_HEAP_TYPE, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> Option<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    };
    let mut b: Option<ID3D12Resource> = None;
    // SAFETY: a plain committed buffer of ours.
    unsafe {
        device
            .CreateCommittedResource(&D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() }, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut b)
            .ok()?;
    }
    b
}

fn kit() -> Option<&'static Kit> {
    static KIT: OnceLock<Option<Kit>> = OnceLock::new();
    KIT.get_or_init(|| {
        let made = make_kit();
        if made.is_none() {
            tracing::warn!("host ray tracing: the probe's program or buffers could not be made; no probe");
        }
        made
    })
    .as_ref()
}

fn make_kit() -> Option<Kit> {
    let device = &crate::host::host()?.device;
    let view = |ty, register: u8| D3D12_ROOT_PARAMETER {
        ParameterType: ty,
        Anonymous: D3D12_ROOT_PARAMETER_0 {
            Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: u32::from(register), RegisterSpace: ROOT_CBV_SPACE },
        },
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
    };
    let params = [
        view(D3D12_ROOT_PARAMETER_TYPE_SRV, PROBE_RAYS),
        view(D3D12_ROOT_PARAMETER_TYPE_SRV, PROBE_STRUCTURE),
        view(D3D12_ROOT_PARAMETER_TYPE_UAV, PROBE_HITS),
    ];
    let desc = D3D12_ROOT_SIGNATURE_DESC { NumParameters: params.len() as u32, pParameters: params.as_ptr(), ..Default::default() };
    let (mut blob, mut errors): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    let dxil = ray_query_probe(RAYS);
    // SAFETY: the descriptions, the blob and the code outlive the calls.
    let (root, pso) = unsafe {
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, Some(&mut errors)).ok()?;
        let blob = blob?;
        let bytes = std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes).ok()?;
        let pso: ID3D12PipelineState = device
            .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: dxil.as_ptr().cast(), BytecodeLength: dxil.len() },
                ..Default::default()
            })
            .ok()?;
        (root, pso)
    };
    let hit_bytes = u64::from(RAYS * PROBE_HIT_DWORDS * 4);
    let hits = buffer(device, hit_bytes, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
    let hits_back = buffer(device, hit_bytes, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
    let pages_back = buffer(device, ROUND_PAGES as u64 * PAGE, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
    crate::host::name(&hits, || "host ray tracing: the probe's hits".into());
    let (mut a, mut b) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: readback buffers of ours, mapped for good.
    unsafe {
        hits_back.Map(0, None, Some(&mut a)).ok()?;
        pages_back.Map(0, None, Some(&mut b)).ok()?;
    }
    Some(Kit { root, pso, hits, hits_back, hits_data: a.cast_const().cast(), pages_back, pages_data: b.cast_const().cast() })
}

/// What the step at the probe's submission found.
struct Sent {
    frame: u64,
    eye: [f32; 3],
    /// The instance descriptions the host's top level was built from.
    instances: Vec<u8>,
}

/// Pages asked for in one round, each with whether it has a place on the
/// card, and the frame its list was submitted in (0: not yet).
struct Round {
    pages: Vec<(u64, bool)>,
    sent: Arc<AtomicU64>,
}

/// The console's memory read back so far.
#[derive(Default)]
struct Cache {
    pages: HashMap<u64, Box<[u8]>>,
    /// Pages that had no place on the card when they were asked for, and
    /// how often (a page gets its place at a submission, `crate::memory`:
    /// one the walk needs is asked for again in the next rounds).
    absent: HashMap<u64, u32>,
}

/// Rounds a page with no place is asked for in, at most.
const ABSENT_TRIES: u32 = 3;

impl Cache {
    /// Whether `page` had no place each time it was asked for.
    fn gone(&self, page: u64) -> bool {
        self.absent.get(&page).is_some_and(|&n| n >= ABSENT_TRIES)
    }
}

impl Pages for Cache {
    fn read(&self, address: u64, out: &mut [u8]) -> Result<(), u64> {
        let mut done = 0;
        while done < out.len() {
            let at = address + done as u64;
            let page = at & !(PAGE - 1);
            let Some(bytes) = self.pages.get(&page) else { return Err(page) };
            let from = (at - page) as usize;
            let n = (out.len() - done).min(PAGE as usize - from);
            out[done..done + n].copy_from_slice(&bytes[from..from + n]);
            done += n;
        }
        Ok(())
    }
}

struct Run {
    number: u64,
    dest: u64,
    count: u32,
    fired: u64,
    sent: Arc<Mutex<Option<Sent>>>,
    /// The host's hits, once read back ([`HIT`] dwords each).
    host: Option<Vec<u32>>,
    cache: Cache,
    round: Option<Round>,
    rounds: u32,
    want: Vec<u64>,
}

#[derive(Default)]
struct State {
    run: Option<Run>,
    next: u64,
    biggest: (u64, u32),
    number: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn state() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The console's top level of `count` instances takes at most about so many
/// bytes from its address (header, boxes, instance nodes of 128 bytes).
fn top_bytes(count: u32) -> u64 {
    (u64::from(count) * 256 + (64 << 10)).min(16 << 20)
}

/// Records copies of `pages` (addresses) of the console's memory into the
/// probe's readback buffer at this place of the list.
fn record_round(host: &HostList, list: &ID3D12GraphicsCommandList4, kit: &Kit, pages: Vec<u64>) -> Round {
    let mut out = Vec::with_capacity(pages.len().min(ROUND_PAGES));
    for (slot, page) in pages.into_iter().take(ROUND_PAGES).enumerate() {
        let home = crate::memory::home(page);
        if let Some((buffer, offset, _)) = &home {
            // SAFETY: an open list; the copy's buffer holds the whole page there
            // and is kept by the list; the readback buffer is ours.
            unsafe { list.CopyBufferRegion(&kit.pages_back, slot as u64 * PAGE, buffer, *offset, PAGE) };
            host.keep(buffer);
        }
        out.push((page, home.is_some()));
    }
    let sent = Arc::new(AtomicU64::new(0));
    let at = sent.clone();
    host.defer_host(move || at.store(crate::frame::current().max(1), Relaxed));
    Round { pages: out, sent }
}

/// At the start of a host recording: a round of pages the walk asked for.
pub(super) fn read_pages(host: &HostList, list: &ID3D12GraphicsCommandList4) {
    if !on() {
        return;
    }
    let mut guard = state();
    let Some(run) = guard.as_mut().and_then(|s| s.run.as_mut()) else { return };
    if run.round.is_some() || run.want.is_empty() {
        return;
    }
    let Some(kit) = kit() else { return };
    let pages = std::mem::take(&mut run.want);
    run.round = Some(record_round(host, list, kit, pages));
    run.rounds += 1;
}

/// After the host built the top level of `dest` (`count` instances, whose
/// rewritten descriptions are at `descs` once submitted): the probe, if it
/// is due and follows this one.
pub(super) fn after_top(host: &HostList, list: &ID3D12GraphicsCommandList4, dest: u64, count: u32, top: &Arc<Structure>, descs: usize) {
    if !on() {
        return;
    }
    let mut guard = state();
    let st = guard.get_or_insert_with(State::default);
    match chosen() {
        Some(c) if c == dest => st.biggest = (dest, count),
        None if count >= st.biggest.1 || st.biggest.0 == dest => st.biggest = (dest, count),
        _ => {}
    }
    let now = crate::frame::current();
    if st.run.is_some() || now < st.next || dest != st.biggest.0 || count == 0 {
        return;
    }
    let Some(kit) = kit() else { return };
    let Some((rays_gpu, rays_cpu)) = host.scratch(u64::from(RAYS * PROBE_RAY_DWORDS * 4)) else { return };
    let sent = Arc::new(Mutex::new(None));
    {
        let (sent, rays) = (sent.clone(), rays_cpu as usize);
        // After the rewrite of the instances (a step recorded before this).
        host.defer_host(move || {
            // SAFETY: the list's upload memory, `count` descriptions, written
            // at this submission by the rewrite.
            let instances = unsafe { std::slice::from_raw_parts(descs as *const u8, count as usize * INSTANCE) }.to_vec();
            let eye = eye_asked().unwrap_or_else(|| middle(&instances));
            // SAFETY: the list's upload memory of `RAYS` rays.
            let out = unsafe { std::slice::from_raw_parts_mut(rays as *mut u32, (RAYS * PROBE_RAY_DWORDS) as usize) };
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let d = direction(x, y);
                    let at = ((y * WIDTH + x) * PROBE_RAY_DWORDS) as usize;
                    out[at..at + 12].copy_from_slice(&[
                        eye[0].to_bits(),
                        eye[1].to_bits(),
                        eye[2].to_bits(),
                        0f32.to_bits(),
                        d[0].to_bits(),
                        d[1].to_bits(),
                        d[2].to_bits(),
                        T_MAX.to_bits(),
                        0xff,
                        !0,
                        0,
                        0,
                    ]);
                }
            }
            *sent.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sent { frame: crate::frame::current().max(1), eye, instances });
        });
    }
    let bytes = u64::from(RAYS * PROBE_HIT_DWORDS * 4);
    // SAFETY: an open list; the root signature, pipeline and buffers are ours
    // (statics) or the list's (the rays, the structure it holds); the hits
    // buffer is in the unordered access state between probes.
    unsafe {
        list.SetComputeRootSignature(&kit.root);
        list.SetPipelineState(&kit.pso);
        list.SetComputeRootShaderResourceView(u32::from(PROBE_RAYS), rays_gpu);
        list.SetComputeRootShaderResourceView(u32::from(PROBE_STRUCTURE), top.gpu());
        list.SetComputeRootUnorderedAccessView(u32::from(PROBE_HITS), kit.hits.GetGPUVirtualAddress());
        list.Dispatch(RAYS.div_ceil(PROBE_GROUP), 1, 1);
        list.ResourceBarrier(&[crate::host::transition(&kit.hits, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
        list.CopyBufferRegion(&kit.hits_back, 0, &kit.hits, 0, bytes);
        list.ResourceBarrier(&[crate::host::transition(&kit.hits, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)]);
    }
    // The console's top level as the title's builder left it in this list.
    let first = dest & !(PAGE - 1);
    let pages: Vec<u64> = (0..top_bytes(count).div_ceil(PAGE) + 1).map(|k| first + k * PAGE).collect();
    let round = record_round(host, list, kit, pages);
    st.number += 1;
    tracing::info!(number = st.number, dest = format_args!("{dest:#x}"), count, "host ray tracing: a probe fired at the host's top level");
    st.run = Some(Run {
        number: st.number,
        dest,
        count,
        fired: now,
        sent,
        host: None,
        cache: Cache::default(),
        round: Some(round),
        rounds: 1,
        want: Vec::new(),
    });
}

/// The median of each axis of the places of the active instances.
fn middle(instances: &[u8]) -> [f32; 3] {
    let f = |d: &[u8], k: usize| f32::from_le_bytes(d[4 * k..4 * k + 4].try_into().unwrap_or_default());
    let active: Vec<&[u8]> = instances.chunks_exact(INSTANCE).filter(|d| d[51] != 0).collect();
    std::array::from_fn(|axis| {
        let mut v: Vec<f32> = active.iter().map(|d| f(d, 4 * axis + 3)).filter(|x| x.is_finite()).collect();
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    })
}

/// Once a frame: what came back is taken, walked and compared.
pub(super) fn tick() {
    if !on() {
        return;
    }
    let mut guard = state();
    let Some(st) = guard.as_mut() else { return };
    let Some(run) = st.run.as_mut() else { return };
    let now = crate::frame::current();
    if now > run.fired + GIVE_UP {
        tracing::warn!(number = run.number, "host ray tracing: a probe's list was not submitted in time; given up");
        st.run = None;
        st.next = now + every();
        return;
    }
    let Some(kit) = kit() else { return };
    if run.host.is_none() {
        let ready = run.sent.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| now >= s.frame + SETTLE);
        if !ready {
            return;
        }
        // SAFETY: the readback mapping of `RAYS` hits, written frames ago.
        let all = unsafe { std::slice::from_raw_parts(kit.hits_data, (RAYS * PROBE_HIT_DWORDS) as usize) };
        let hits = all.to_vec();
        pictures(run, &hits);
        run.host = Some(hits);
    }
    if let Some(round) = &run.round {
        let at = round.sent.load(Relaxed);
        if at == 0 || now < at + SETTLE {
            return;
        }
        for (slot, &(page, there)) in round.pages.iter().enumerate() {
            if !there {
                *run.cache.absent.entry(page).or_insert(0) += 1;
                continue;
            }
            // SAFETY: inside the readback mapping, written frames ago.
            let bytes = unsafe { std::slice::from_raw_parts(kit.pages_data.add(slot * PAGE as usize), PAGE as usize) };
            run.cache.pages.insert(page, bytes.into());
        }
        run.round = None;
    }
    if !run.want.is_empty() {
        return;
    }
    let guard = run.sent.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sent) = guard.as_ref() else { return };
    let rays = grid();
    let walked: Vec<Walked> = rays
        .iter()
        .map(|&(x, y)| walk::walk(&run.cache, run.dest, sent.eye, direction(x, y), 0.0, T_MAX, 0xff))
        .collect();
    let missing: BTreeSet<u64> = walked
        .iter()
        .filter_map(|w| match *w {
            Walked::Missing(p) if !run.cache.gone(p) => Some(p),
            _ => None,
        })
        .collect();
    if !missing.is_empty() && run.rounds < ROUNDS {
        run.want = missing.into_iter().take(ROUND_PAGES).collect();
        return;
    }
    let host = run.host.as_deref().unwrap_or_default();
    compare(run, sent, &rays, &walked, host);
    drop(guard);
    st.run = None;
    st.next = now + every();
}

/// Dwords of one ray's result from the probe's program: committed status,
/// t, instance index, instance id, primitive, geometry, the two
/// barycentrics; a ninth, if the program writes one, is taken as
/// `CommittedTriangleFrontFace` (it does not today: `emit/rayquery.rs`).
const HIT: usize = PROBE_HIT_DWORDS as usize;

/// What the host's query found for one ray.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HostHit {
    t: f32,
    index: u32,
    id: u32,
    primitive: u32,
    geometry: u32,
    uv: [f32; 2],
    /// Front-facing, if the program says.
    front: Option<bool>,
}

/// The host's hit of pixel `i`, if it hit.
fn host_hit(hits: &[u32], i: usize) -> Option<HostHit> {
    let h = hits.get(i * HIT..i * HIT + HIT)?;
    (h[0] == COMMITTED_TRIANGLE_HIT).then(|| HostHit {
        t: f32::from_bits(h[1]),
        index: h[2],
        id: h[3],
        primitive: h[4],
        geometry: h[5],
        uv: [f32::from_bits(h[6]), f32::from_bits(h[7])],
        front: h.get(8).map(|&f| f != 0),
    })
}

fn pictures(run: &Run, hits: &[u32]) {
    let far = (0..RAYS as usize).filter_map(|i| host_hit(hits, i).map(|h| h.t)).fold(1.0f32, f32::max);
    let mut depth = Vec::with_capacity((RAYS * 3) as usize);
    let mut ids = Vec::with_capacity((RAYS * 3) as usize);
    for i in 0..RAYS as usize {
        match host_hit(hits, i) {
            Some(HostHit { t, index, .. }) => {
                let v = (255.0 * (1.0 - (1.0 + t.max(0.0)).ln() / (1.0 + far).ln())).clamp(16.0, 255.0) as u8;
                depth.extend([v, v, v]);
                let h = (index.wrapping_add(1)).wrapping_mul(0x9E37_79B9);
                ids.extend([(h >> 24) as u8 | 0x40, (h >> 16) as u8 | 0x40, (h >> 8) as u8 | 0x40]);
            }
            None => {
                depth.extend([0, 0, 40]);
                ids.extend([0, 0, 0]);
            }
        }
    }
    let dir = folder();
    let _ = std::fs::create_dir_all(&dir);
    let stem = format!("dxr-probe-{:03}-f{}", run.number, run.fired);
    for (what, rgb) in [("depth", depth), ("instances", ids)] {
        let path = dir.join(format!("{stem}-{what}.png"));
        match std::fs::write(&path, png(WIDTH, HEIGHT, &rgb)) {
            Ok(()) => tracing::info!(path = %path.display(), "host ray tracing: the probe's picture of the host's top level"),
            Err(e) => tracing::warn!(error = %e, path = %path.display(), "host ray tracing: the probe's picture not saved"),
        }
    }
}

/// Whether two distances are the same but for rounding.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-3 * a.abs().max(b.abs()).max(1.0)
}

/// How the console's frame of a bottom level stands to the title's object
/// space, seen from one hit (the critic's item 3): the instance node's turn
/// times the title's instance transform (3x3s) maps the title's object
/// space into the frame the console's nodes are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    /// The title's own axes (but for rounding).
    Same,
    /// Its axes swapped, turned over or scaled (one entry a row and a
    /// column), as the builder's 64 turns of the axes and per-axis scales
    /// would give.
    Axes,
    /// Anything else (another instance's transform, or a turn the walk read
    /// wrong).
    Other,
}

/// The [`Frame`] of `p` (rows of three), and whether it mirrors (its
/// determinant is negative: every triangle winds the other way round in
/// the console's frame than in the title's object space).
fn frame(p: &[f32; 9]) -> (Frame, bool) {
    let det = p[0] * (p[4] * p[8] - p[5] * p[7]) - p[1] * (p[3] * p[8] - p[5] * p[6]) + p[2] * (p[3] * p[7] - p[4] * p[6]);
    let same = p.iter().enumerate().all(|(k, &v)| (v - if k % 4 == 0 { 1.0 } else { 0.0 }).abs() <= 1e-3);
    if same {
        return (Frame::Same, false);
    }
    let mut columns = [false; 3];
    let axes = p.chunks_exact(3).all(|row| {
        let (c, big) = row.iter().map(|v| v.abs()).enumerate().fold((0, 0.0f32), |a, b| if b.1 > a.1 { b } else { a });
        let alone = big > 0.0 && big.is_finite() && row.iter().enumerate().all(|(k, v)| k == c || v.abs() <= 1e-3 * big);
        alone && !std::mem::replace(&mut columns[c], true)
    });
    (if axes { Frame::Axes } else { Frame::Other }, det < 0.0)
}

/// The 3x3 of a title instance description's transform (rows of three).
fn title_turn(d: &[u8]) -> [f32; 9] {
    let f = |k: usize| f32::from_le_bytes(d[4 * k..4 * k + 4].try_into().unwrap_or_default());
    [f(0), f(1), f(2), f(4), f(5), f(6), f(8), f(9), f(10)]
}

/// `a b` of two 3x3s (rows of three).
fn times(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
    std::array::from_fn(|k| (0..3).map(|i| a[3 * (k / 3) + i] * b[3 * i + k % 3]).sum())
}

/// `D3D12_RAYTRACING_INSTANCE_FLAG_TRIANGLE_FRONT_COUNTERCLOCKWISE`.
const FRONT_COUNTERCLOCKWISE: u8 = 0x2;

/// A triangle's roles byte: bit 5 (the flip bit of the public layout of this
/// node format, `nativo-programas.md` 2.4).
const ROLES_FLIP: u32 = 0x20;

/// The facing of a hit both sides made at the same distance in the same
/// instance: the console's frame against the title's object space and what
/// Direct3D's front face is from the console's winding (Direct3D takes the
/// winding in object space, clockwise from the origin unless the instance
/// says counterclockwise; the console's denominator is the winding in its
/// own frame, which a mirroring frame turns round).
struct Facing {
    frame: Frame,
    mirrored: bool,
    flip: bool,
    front: bool,
}

fn facing(c: &walk::Hit, instance: &[u8]) -> Facing {
    let title = title_turn(instance);
    let p = c.turn.map_or(title, |m| times(&m, &title));
    let (frame, mirrored) = frame(&p);
    let counterclockwise = instance[55] & FRONT_COUNTERCLOCKWISE != 0;
    Facing { frame, mirrored, flip: c.roles & ROLES_FLIP != 0, front: c.clockwise ^ mirrored ^ counterclockwise }
}

fn compare(run: &Run, sent: &Sent, rays: &[(u32, u32)], walked: &[Walked], hits: &[u32]) {
    let instance = |index: u32| sent.instances.get(index as usize * INSTANCE..index as usize * INSTANCE + INSTANCE);
    let contribution = |index: u32| instance(index).map(|d| u32::from_le_bytes(d[52..56].try_into().unwrap_or_default()) & 0xff_ffff);
    let [mut both, mut neither, mut host_only, mut console_only, mut unfinished, mut absent] = [0u32; 6];
    let [mut t_same, mut id_same, mut contribution_same, mut geometry_same, mut primitive_same, mut uv_same, mut clockwise] =
        [0u32; 7];
    // Of the hits at the same distance and instance id: the frames, the
    // facings, and the host's facing where the program says it.
    let [mut faced, mut frame_same, mut frame_axes, mut frame_other, mut mirrored, mut flipped, mut flip_agrees, mut front] =
        [0u32; 8];
    let [mut host_faced, mut host_front, mut face_same] = [0u32; 3];
    // The frame words of the bottom levels the console's rays hit.
    let mut frame_words: BTreeMap<u32, u32> = BTreeMap::new();
    let mut listing = String::new();
    let mut first = Vec::new();
    for (k, (&(x, y), w)) in rays.iter().zip(walked).enumerate() {
        let i = (y * WIDTH + x) as usize;
        let h = host_hit(hits, i);
        if let Walked::Hit(c) = w {
            *frame_words.entry(c.frame_word).or_insert(0) += 1;
        }
        let line = match (h, w) {
            (Some(HostHit { t, index, id, primitive, geometry, uv, front: host_says }), Walked::Hit(c)) => {
                both += 1;
                let hc = contribution(index).unwrap_or(u32::MAX);
                let same = [
                    near(t, c.t),
                    id == c.id,
                    hc == c.contribution,
                    geometry == c.geometry,
                    primitive == c.primitive,
                    (uv[0] - c.barycentrics[0]).abs() <= 1e-3 && (uv[1] - c.barycentrics[1]).abs() <= 1e-3,
                ];
                for (n, s) in [&mut t_same, &mut id_same, &mut contribution_same, &mut geometry_same, &mut primitive_same, &mut uv_same]
                    .into_iter()
                    .zip(same)
                {
                    *n += u32::from(s);
                }
                clockwise += u32::from(c.clockwise);
                let mut face_differs = false;
                let mut faces = String::new();
                if same[0] && same[1]
                    && let Some(d) = instance(index)
                {
                    let f = facing(c, d);
                    faced += 1;
                    match f.frame {
                        Frame::Same => frame_same += 1,
                        Frame::Axes => frame_axes += 1,
                        Frame::Other => frame_other += 1,
                    }
                    mirrored += u32::from(f.mirrored);
                    flipped += u32::from(f.flip);
                    flip_agrees += u32::from(f.flip == f.mirrored);
                    front += u32::from(f.front);
                    let _ = write!(faces, "; frame {:?}{}, front as Direct3D says from the console's winding {}", f.frame, if f.mirrored { " mirrored" } else { "" }, f.front);
                    if let Some(hf) = host_says {
                        host_faced += 1;
                        host_front += u32::from(hf);
                        face_same += u32::from(hf == f.front);
                        face_differs = hf != f.front;
                        let _ = write!(faces, ", the host's {hf}");
                    }
                }
                let text = format!(
                    "host t {t} instance {index} id {id} contribution {hc} geometry {geometry} primitive {primitive} uv {:.4} {:.4}; \
                     console t {} id {} contribution {} geometry {} (flags {:#x}) primitive {} uv {:.4} {:.4} clockwise {} roles {:#x} instance node {:#x} (flags {:#x}) frame word {:#x}{faces}",
                    uv[0], uv[1], c.t, c.id, c.contribution, c.geometry, c.geometry_flags, c.primitive, c.barycentrics[0], c.barycentrics[1],
                    c.clockwise, c.roles, c.instance, c.instance_flags, c.frame_word
                );
                if (face_differs || same.iter().any(|s| !s)) && first.len() < 8 {
                    first.push(format!("ray {k} ({x}, {y}): {text}"));
                }
                text
            }
            (None, Walked::Miss) => {
                neither += 1;
                "both miss".into()
            }
            (Some(HostHit { t, index, id, .. }), Walked::Miss) => {
                host_only += 1;
                let text = format!("host only: t {t} instance {index} id {id}");
                if first.len() < 8 {
                    first.push(format!("ray {k} ({x}, {y}): {text}"));
                }
                text
            }
            (None, Walked::Hit(c)) => {
                console_only += 1;
                let text = format!("console only: t {} id {} geometry {} primitive {}", c.t, c.id, c.geometry, c.primitive);
                if first.len() < 8 {
                    first.push(format!("ray {k} ({x}, {y}): {text}"));
                }
                text
            }
            (_, Walked::Missing(p)) => {
                let gone = run.cache.gone(*p);
                if gone {
                    absent += 1;
                } else {
                    unfinished += 1;
                }
                format!("console walk unfinished: page {p:#x} {}", if gone { "has no place" } else { "not read" })
            }
            (_, Walked::Broken(why)) => {
                unfinished += 1;
                format!("console walk broken: {why}")
            }
        };
        let d = direction(x, y);
        let _ = writeln!(listing, "ray {k} pixel ({x}, {y}) direction ({:.4} {:.4} {:.4}): {line}", d[0], d[1], d[2]);
    }
    let host_facing = if host_faced > 0 {
        format!("; the host's query says front-facing {host_front} of {host_faced}, the same as from the console's winding {face_same}")
    } else {
        format!("; the host's facing is not read back (the probe program writes {HIT} dwords a ray)")
    };
    let mut words: Vec<(u32, u32)> = frame_words.into_iter().collect();
    words.sort_by_key(|&(w, n)| (std::cmp::Reverse(n), w));
    let words: Vec<String> = words.iter().take(6).map(|(w, n)| format!("{w:#x} x{n}")).collect();
    tracing::info!(
        "host ray tracing probe {} (frame {}, top level {:#x} of {} instances, eye ({:.1} {:.1} {:.1}), {} page rounds, {} pages read): \
         of {} rays against the host's and the console's structures both hit {both}, both miss {neither}, only the host hit {host_only}, \
         only the console {console_only}, console walk unfinished {unfinished} and {absent} at pages with no place; of the {both} both hit \
         the same distance {t_same}, instance id {id_same}, contribution {contribution_same}, geometry {geometry_same}, primitive \
         {primitive_same}, barycentrics {uv_same}; the console's hits clockwise {clockwise}; of the {faced} at the same distance and \
         instance id, the console's frame of the bottom level (its instance node's turn times the title's transform) is the title's \
         object space in {frame_same}, the axes swapped, turned over or scaled in {frame_axes} and something else in {frame_other}, \
         mirrored in {mirrored}; the triangle's flip bit set in {flipped} (the same as the mirroring in {flip_agrees}); front-facing as \
         Direct3D says from the console's winding {front}{host_facing}; the frame words (+60) of the bottom levels the console's rays \
         hit: {}",
        run.number,
        sent.frame,
        run.dest,
        run.count,
        sent.eye[0],
        sent.eye[1],
        sent.eye[2],
        run.rounds,
        run.cache.pages.len(),
        rays.len(),
        if words.is_empty() { "none".to_string() } else { words.join(", ") },
    );
    for f in &first {
        tracing::info!("host ray tracing probe {}: {f}", run.number);
    }
    let path = folder().join(format!("dxr-probe-{:03}-f{}-rays.txt", run.number, run.fired));
    let _ = std::fs::write(&path, listing);
}

/// A PNG of `width x height` pixels of 8-bit RGB.
fn png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row = width as usize * 3;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgb.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    let mut packed = vec![0u8; zlib_rs::compress_bound(raw.len())];
    let (idat, _) = zlib_rs::compress_slice(&mut packed, &raw, zlib_rs::DeflateConfig::new(6));
    chunk(&mut out, b"IDAT", idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend(kind);
    png.extend(data);
    let mut crc = !0u32;
    for &b in &png[start..] {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    png.extend((!crc).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panorama_covers_every_direction_and_the_grid_lies_in_it() {
        // Unit directions; the first row looks up, the last down (z up).
        for (x, y) in [(0, 0), (160, 90), (319, 179)] {
            let d = direction(x, y);
            assert!(((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() - 1.0).abs() < 1e-5);
        }
        assert!(direction(10, 0)[2] > 0.99 && direction(10, 179)[2] < -0.99);
        let g = grid();
        assert_eq!(g.len(), (GRID.0 * GRID.1) as usize);
        assert!(g.iter().all(|&(x, y)| x < WIDTH && y < HEIGHT));
    }

    #[test]
    fn the_cache_reads_across_pages_and_names_the_missing_one() {
        let mut c = Cache::default();
        c.pages.insert(0x10000, vec![1u8; PAGE as usize].into());
        c.pages.insert(0x20000, vec![2u8; PAGE as usize].into());
        let mut out = [0u8; 8];
        c.read(0x1fffc, &mut out).unwrap();
        assert_eq!(out, [1, 1, 1, 1, 2, 2, 2, 2]);
        assert_eq!(c.read(0x2fffc, &mut out), Err(0x30000));
    }

    #[test]
    fn the_middle_is_the_median_of_the_active_instances() {
        let mut d = vec![0u8; 3 * INSTANCE];
        for (k, x) in [5.0f32, 1.0, 100.0].iter().enumerate() {
            d[k * INSTANCE + 12..k * INSTANCE + 16].copy_from_slice(&x.to_le_bytes());
            d[k * INSTANCE + 51] = 0xff;
        }
        // The last one inactive.
        d[2 * INSTANCE + 51] = 0;
        assert_eq!(middle(&d)[0], 5.0);
    }

    #[test]
    fn the_consoles_frame_is_told_from_its_turn_and_the_titles_transform() {
        assert_eq!(frame(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]), (Frame::Same, false));
        // x and y swapped and scaled by 2: the axes, mirrored.
        assert_eq!(frame(&[0.0, 2.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0]), (Frame::Axes, true));
        // A quarter turn about z and scales: the axes, not mirrored.
        assert_eq!(frame(&[0.0, -4.0, 0.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.5]), (Frame::Axes, false));
        // A turn of 30 degrees is something else; so are two rows on one axis.
        let (s, c) = (0.5f32, 0.75f32.sqrt());
        let title = [c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0];
        assert_eq!(frame(&title).0, Frame::Other);
        assert_eq!(frame(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]).0, Frame::Other);
        assert_eq!(frame(&[f32::NAN; 9]).0, Frame::Other);
        // An instance node's turn that undoes the title's 30 degrees, then
        // swaps x and y: the console's frame is the title's axes swapped.
        let back = [c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0];
        let swap = [0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let turn = times(&swap, &back);
        assert_eq!(frame(&times(&turn, &title)), (Frame::Axes, true));
        // Facing: clockwise in a mirroring frame is counterclockwise in the
        // title's object space, a back face unless the instance turns it.
        let mut d = vec![0u8; INSTANCE];
        for k in [0, 5, 10] {
            d[4 * k..4 * k + 4].copy_from_slice(&1f32.to_le_bytes());
        }
        let hit = walk::Hit {
            t: 1.0,
            barycentrics: [0.25, 0.25],
            id: 0,
            contribution: 0,
            instance_flags: 0,
            geometry: 0,
            geometry_flags: 0,
            primitive: 0,
            clockwise: true,
            roles: ROLES_FLIP | 0x9,
            instance: 0,
            turn: Some(swap),
            frame_word: 0,
        };
        let f = facing(&hit, &d);
        assert_eq!((f.frame, f.mirrored, f.flip, f.front), (Frame::Axes, true, true, false));
        d[55] = FRONT_COUNTERCLOCKWISE;
        assert!(facing(&hit, &d).front);
        // No turn and the title's identity: the console's winding as it is.
        let plain = walk::Hit { turn: None, roles: 0x9, ..hit };
        d[55] = 0;
        let f = facing(&plain, &d);
        assert_eq!((f.frame, f.mirrored, f.flip, f.front), (Frame::Same, false, false, true));
    }

    #[test]
    fn a_hit_takes_a_ninth_dword_as_the_hosts_facing_if_there_is_one() {
        let mut hits = vec![0u32; 2 * HIT];
        hits[HIT..HIT + 8].copy_from_slice(&[COMMITTED_TRIANGLE_HIT, 2f32.to_bits(), 3, 7, 11, 1, 0.5f32.to_bits(), 0.25f32.to_bits()]);
        assert_eq!(host_hit(&hits, 0), None);
        let h = host_hit(&hits, 1).unwrap();
        assert_eq!((h.t, h.index, h.id, h.primitive, h.geometry, h.uv), (2.0, 3, 7, 11, 1, [0.5, 0.25]));
        assert_eq!(h.front.is_some(), HIT > 8);
        assert_eq!(host_hit(&hits, 2), None);
    }

    #[test]
    fn a_png_has_its_chunks() {
        let p = png(2, 1, &[255, 0, 0, 0, 255, 0]);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert!(p.windows(4).any(|w| w == b"IDAT") && p.ends_with(&[0xae, 0x42, 0x60, 0x82]));
    }
}
