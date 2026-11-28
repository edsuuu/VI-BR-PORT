//! The host's own ray tracing (DirectX Raytracing): acceleration structures
//! the host's driver builds from triangle lists and instances, which ray
//! queries in compute programs walk in hardware.
//!
//! Today the title's structure builder (`XboxBvhBuilder.dll`) builds the
//! console's own structures in title memory with compute programs we
//! translate, and the title's ray programs walk them themselves around the
//! console's node test (`victory_shader::emit`, `intersect.rs`). The
//! builder's entry points take the standard D3D12 build descriptions
//! (triangles, indices, instance descriptions), so the host can build the
//! same structures with its own hardware's builder.
//!
//! With `VICTORY_DXR_NATIVE=shadow` (or `1`; unset: off, the default, and
//! nothing here runs but [`log_tier`] at start-up) the listening post in
//! front of the title's builder (`victory_hle::modules::bvh_builder`) hands
//! every description it decodes to [`record`] and [`copy`], which build
//! the host's structures at the same place of the same title list as the
//! console's ([`crate::list::external`]): they inherit the title's own
//! ordering of skinning, builds and reads. No program reads them yet; a
//! five-second line says what was built (`native::line`).
//!
//! - `native`: the recording at the title list's place (barriers, copies of
//!   structures, what a list holds) and the five-second line;
//! - `bottom`: bottom levels, from the geometry found in the host copies of
//!   title memory (`expand`, with `VICTORY_DXR_EXPAND=1`: the console's
//!   packed indices and quantized positions expanded first, see
//!   [`packed`]); `top`: top levels, their instances rewritten at
//!   submission to name the host's bottom levels;
//! - `count`: what the line counts, and why builds were refused; `stamps`:
//!   the device's ray-tracing interface and the card's time of the builds;
//! - `table`: the title's destination address to the host structure, with
//!   generations and the refit-or-rebuild decision;
//! - `pool`: the card memory structures and scratch memory are cut from;
//! - `scratch` (`VICTORY_DXR_SCRATCH_MB`): the builds of one list sharing
//!   their scratch memory;
//! - `probe` (`VICTORY_DXR_PROBE=1`): a picture of a host top level made with
//!   ray queries of our own writer, and the same rays walked on the CPU
//!   through the console's structure read back from the card (`walk`),
//!   compared ray by ray;
//! - [`query`] (`VICTORY_RT_QUERY`): the host's top level bound where a
//!   translated program's ray query reads it (`t252`), or an empty one;
//! - [`packed`]: the console's packed indices and quantized positions, and
//!   their expansion into formats the host's builder takes.
//!
//! The tests prove the host side end to end on the card with programs of
//! our own writer (`victory_shader::emit::ray_query_probe`) and measure
//! what builds cost.

use windows::Win32::Graphics::Direct3D12::{
    D3D12_FEATURE_D3D12_OPTIONS5, D3D12_FEATURE_DATA_D3D12_OPTIONS5, ID3D12Device,
};

mod bottom;
mod count;
mod expand;
mod native;
pub mod packed;
mod pool;
mod probe;
mod scratch;
pub(crate) mod query;
mod stamps;
mod table;
mod top;
mod walk;

pub(crate) use native::in_recording;

/// A presented frame.
pub(crate) fn frame() {
    native::frame();
    query::frame();
}

/// The five-second lines: the host's builds (with [`native_on`]) and what
/// programs that ask the host's hardware were given (once one asked).
pub(crate) fn line() {
    native::line();
    query::line();
}

/// `D3D12_RAYTRACING_TIER_1_1`: inline ray tracing (ray queries in any
/// program), which a translated program needs to ask the host's hardware.
pub(crate) const TIER_1_1: i32 = 11;

/// The tier [`log_tier`] found (`D3D12_RAYTRACING_TIER_1_0` is 10).
static TIER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The tier [`log_tier`] found.
pub(crate) fn tier() -> i32 {
    TIER.load(std::sync::atomic::Ordering::Relaxed)
}

/// Logs the host's ray-tracing tier and returns it (0: none; 10 tier 1.0,
/// 11 tier 1.1, 12 tier 1.2).
pub(crate) fn log_tier(device: &ID3D12Device) -> i32 {
    let mut o5 = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    // SAFETY: a plain feature query into a structure of its size.
    let ok = unsafe {
        device.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS5, (&raw mut o5).cast(), size_of_val(&o5) as u32)
    }
    .is_ok();
    let tier = if ok { o5.RaytracingTier.0 } else { 0 };
    let name = match tier {
        0 => "none",
        10 => "1.0",
        11 => "1.1",
        12 => "1.2",
        _ => "newer",
    };
    tracing::info!(tier = name, inline = tier >= TIER_1_1, "host ray tracing");
    TIER.store(tier, std::sync::atomic::Ordering::Relaxed);
    tier
}

/// A DXGI format a structure build may be given, by name; others by number.
pub(crate) fn format_name(f: u32) -> String {
    let name = match f {
        0 => "UNKNOWN",
        2 => "R32G32B32A32_FLOAT",
        6 => "R32G32B32_FLOAT",
        10 => "R16G16B16A16_FLOAT",
        11 => "R16G16B16A16_UNORM",
        13 => "R16G16B16A16_SNORM",
        16 => "R32G32_FLOAT",
        24 => "R10G10B10A2_UNORM",
        28 => "R8G8B8A8_UNORM",
        31 => "R8G8B8A8_SNORM",
        34 => "R16G16_FLOAT",
        35 => "R16G16_UNORM",
        37 => "R16G16_SNORM",
        42 => "R32_UINT",
        49 => "R8G8_UNORM",
        51 => "R8G8_SNORM",
        57 => "R16_UINT",
        _ => return format!("format {f}"),
    };
    name.into()
}

/// What `VICTORY_DXR_NATIVE` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Native {
    /// Unset (the default): the host builds nothing.
    Off,
    /// `shadow`: the title's own builder runs as before and the host builds
    /// the same structures beside it, which nothing reads yet.
    Shadow,
    /// `1`: only the host builds (the listening post stops calling the
    /// title's builder; here the same as `shadow`).
    Only,
}

/// `VICTORY_DXR_NATIVE` (see [`Native`]; anything else is off). With
/// `VICTORY_RT_MISS=1` it is off whatever it says: every ray misses there,
/// and the comparisons of runs made with it keep their meaning.
pub fn native() -> Native {
    static MODE: std::sync::OnceLock<Native> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        let asked = match std::env::var("VICTORY_DXR_NATIVE").as_deref().map(str::trim) {
            Ok("shadow") => Native::Shadow,
            Ok("1") => Native::Only,
            _ => Native::Off,
        };
        if asked != Native::Off && victory_shader::emit::rt_miss() {
            tracing::warn!("VICTORY_RT_MISS=1 turns VICTORY_DXR_NATIVE off: the host builds no structures");
            return Native::Off;
        }
        if asked != Native::Off {
            tracing::info!(
                mode = ?asked,
                "VICTORY_DXR_NATIVE: the host builds the title's ray-tracing structures beside the console's"
            );
        }
        asked
    })
}

/// Whether the host builds structures ([`native`] is not off).
pub fn native_on() -> bool {
    native() != Native::Off
}

/// One triangle geometry of a bottom-level build, as Direct3D's
/// `D3D12_RAYTRACING_GEOMETRY_DESC` of type triangles holds it (title GPU
/// addresses, which are the title's CPU addresses).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Triangles {
    /// `D3D12_RAYTRACING_GEOMETRY_FLAGS` (1: opaque, 2: no duplicate any-hit).
    pub flags: u32,
    /// A 3x4 row-major matrix, or 0.
    pub transform: u64,
    /// DXGI formats.
    pub index_format: u32,
    pub vertex_format: u32,
    pub index_count: u32,
    pub vertex_count: u32,
    pub index_buffer: u64,
    pub vertex_buffer: u64,
    pub vertex_stride: u64,
}

/// One geometry of a bottom-level build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Geometry {
    Triangles(Triangles),
    /// Procedural boxes (`D3D12_RAYTRACING_GEOMETRY_AABBS_DESC`).
    Boxes { flags: u32, count: u64, start: u64, stride: u64 },
    /// A kind the listening post does not know.
    Other { kind: u32, flags: u32 },
}

/// What a build is of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Level {
    /// The geometries as read from the title's description, and whether all
    /// of them could be read.
    Bottom { geometries: Vec<Geometry>, whole: bool },
    /// `count` instance descriptions at `instances` (title memory), laid out
    /// as `D3D12_ELEMENTS_LAYOUT` `layout` says (0: an array).
    Top { instances: u64, count: u32, layout: u32 },
}

/// One build the title asked its builder for: Direct3D's
/// `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC`, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    pub dest: u64,
    /// The structure an update starts from (0: none, or the destination).
    pub source: u64,
    /// `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS`.
    pub flags: u32,
    pub level: Level,
}

/// Which of the title's calls a build comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// `BuildRaytracingAccelerationStructureBatched2X`.
    Batched,
    /// `RehydrateRaytracingAccelerationStructureBatched2X`: a structure the
    /// title shipped packed, unpacked in place; the host builds it from the
    /// geometries its description still names, from scratch.
    Rehydrated,
}

/// The title recorded `builds` into its list `list` (the command list it
/// passed its builder, one of ours): with [`native_on`], the host records its
/// own builds of the same at this place of the list. Nothing otherwise.
/// Call it after the title's builder recorded its own work, so that what
/// the host records comes after it.
pub fn record(list: *mut std::ffi::c_void, origin: Origin, builds: Vec<Build>) {
    native::record(list.cast_const().cast(), origin, builds);
}

/// The title copied the structure at `source` to `dest` in its list `list`
/// (`D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE` `mode`: 0 a clone, 1
/// a compaction): with [`native_on`], the host copies its own structure the
/// same way at this place of the list.
pub fn copy(list: *mut std::ffi::c_void, dest: u64, source: u64, mode: i32) {
    native::copy(list.cast_const().cast(), dest, source, mode);
}

/// Building on the host: test-only for now (nothing in the layer builds a
/// host structure yet).
#[cfg(test)]
pub(crate) mod build {
    use std::mem::ManuallyDrop;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    /// A device of the default adapter with a queue, one list and
    /// timestamps, for the tests.
    pub(crate) struct Gpu {
        pub device: ID3D12Device5,
        pub queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        pub list: ID3D12GraphicsCommandList4,
        fence: ID3D12Fence,
        event: HANDLE,
        done: std::cell::Cell<u64>,
        pub stamps: ID3D12QueryHeap,
        /// Ticks per second of the queue's timestamps.
        pub frequency: u64,
        pub card: String,
    }

    impl Drop for Gpu {
        fn drop(&mut self) {
            // SAFETY: our own event.
            unsafe {
                let _ = CloseHandle(self.event);
            }
        }
    }

    /// A built structure: its buffer, the scratch it was built with (large
    /// enough for an update as well) and its sizes.
    pub(crate) struct Structure {
        pub result: ID3D12Resource,
        pub scratch: ID3D12Resource,
        pub sizes: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO,
        /// What it was built from (the inputs point into it).
        geometry: Vec<D3D12_RAYTRACING_GEOMETRY_DESC>,
        kind: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE,
        flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS,
        instances: (u64, u32),
    }

    impl Structure {
        pub fn address(&self) -> u64 {
            // SAFETY: a plain query of a buffer of ours.
            unsafe { self.result.GetGPUVirtualAddress() }
        }

        fn inputs(&self, update: bool) -> D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            let mut flags = self.flags;
            if update {
                flags |= D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PERFORM_UPDATE;
            }
            let top = self.kind == D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL;
            D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: self.kind,
                Flags: flags,
                NumDescs: if top { self.instances.1 } else { self.geometry.len() as u32 },
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: if top {
                    D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: self.instances.0 }
                } else {
                    D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { pGeometryDescs: self.geometry.as_ptr() }
                },
            }
        }

        /// The same structure (its buffers) with other inputs of the same
        /// counts: what a refit of it from moved vertices reads.
        pub fn rebound(&self, geometry: Vec<D3D12_RAYTRACING_GEOMETRY_DESC>) -> Structure {
            assert_eq!(geometry.len(), self.geometry.len());
            Structure {
                result: self.result.clone(),
                scratch: self.scratch.clone(),
                sizes: self.sizes,
                geometry,
                kind: self.kind,
                flags: self.flags,
                instances: self.instances,
            }
        }

        /// Records its build (or, with `update`, its refit in place from the
        /// inputs' current contents), then a barrier that makes it readable
        /// by what follows.
        pub fn record(&self, list: &ID3D12GraphicsCommandList4, update: bool) {
            self.record_unfenced(list, update);
            // SAFETY: the barrier names a buffer of ours.
            unsafe { list.ResourceBarrier(&[uav_barrier(&self.result)]) };
        }

        /// [`Structure::record`] without the barrier (builds into different
        /// buffers may overlap; the caller orders them with what reads them).
        pub fn record_unfenced(&self, list: &ID3D12GraphicsCommandList4, update: bool) {
            let address = self.address();
            let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                DestAccelerationStructureData: address,
                Inputs: self.inputs(update),
                SourceAccelerationStructureData: if update { address } else { 0 },
                // SAFETY: a plain query of a buffer of ours.
                ScratchAccelerationStructureData: unsafe { self.scratch.GetGPUVirtualAddress() },
            };
            // SAFETY: the description and what it points to outlive the call;
            // the buffers are in the states a build needs.
            unsafe { list.BuildRaytracingAccelerationStructure(&desc, None) };
        }
    }

    /// A UAV barrier on `r` (what orders builds and the walks that read them).
    pub(crate) fn uav_barrier(r: &ID3D12Resource) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                    // SAFETY: borrowed for the barrier's life, not released.
                    pResource: unsafe { std::mem::transmute_copy(r) },
                }),
            },
        }
    }

    pub(crate) fn transition(r: &ID3D12Resource, from: D3D12_RESOURCE_STATES, to: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    // SAFETY: borrowed for the barrier's life, not released.
                    pResource: unsafe { std::mem::transmute_copy(r) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: from,
                    StateAfter: to,
                }),
            },
        }
    }

    /// Triangles of three `f32`s per vertex at `vertices`, indexed by
    /// `u32`s at `indices` when there are any (`index_count` of them).
    pub(crate) fn triangles(
        vertices: u64,
        vertex_count: u32,
        indices: Option<(u64, u32)>,
        flags: D3D12_RAYTRACING_GEOMETRY_FLAGS,
    ) -> D3D12_RAYTRACING_GEOMETRY_DESC {
        let (index_buffer, index_count, format) = match indices {
            Some((at, n)) => (at, n, DXGI_FORMAT_R32_UINT),
            None => (0, 0, DXGI_FORMAT_UNKNOWN),
        };
        D3D12_RAYTRACING_GEOMETRY_DESC {
            Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
            Flags: flags,
            Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                    Transform3x4: 0,
                    IndexFormat: format,
                    VertexFormat: DXGI_FORMAT_R32G32B32_FLOAT,
                    IndexCount: index_count,
                    VertexCount: vertex_count,
                    IndexBuffer: index_buffer,
                    VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: vertices, StrideInBytes: 12 },
                },
            },
        }
    }

    /// One `D3D12_RAYTRACING_INSTANCE_DESC`, as the 64 bytes the build
    /// reads: a row-major 3x4 transform, the id (24 bits) and mask, the hit
    /// group offset (24 bits, 0) and the instance flags, the bottom level's
    /// address.
    pub(crate) fn instance(transform: [f32; 12], id: u32, mask: u8, flags: u8, bottom: u64) -> [u8; 64] {
        let mut out = [0u8; 64];
        for (k, f) in transform.iter().enumerate() {
            out[4 * k..4 * k + 4].copy_from_slice(&f.to_le_bytes());
        }
        out[48..52].copy_from_slice(&(id & 0xff_ffff | u32::from(mask) << 24).to_le_bytes());
        out[52..56].copy_from_slice(&(u32::from(flags) << 24).to_le_bytes());
        out[56..64].copy_from_slice(&bottom.to_le_bytes());
        out
    }

    /// The identity transform moved by `x`, `y`, `z`.
    pub(crate) fn moved(x: f32, y: f32, z: f32) -> [f32; 12] {
        [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, z]
    }

    impl Gpu {
        /// `None` without a hardware adapter with tier 1.1 ray tracing.
        pub fn new() -> Option<Self> {
            // SAFETY: plain object creation on a device of our own.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let desc = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                    .and_then(|factory| factory.EnumAdapterByLuid::<IDXGIAdapter1>(device.GetAdapterLuid()))
                    .ok()?
                    .GetDesc1()
                    .ok()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    return None;
                }
                let tier = super::log_tier(&device);
                if tier < super::TIER_1_1 {
                    return None;
                }
                // The highest shader model the driver takes (6.9 adds a hit
                // triangle's vertex positions to ray queries); a runtime
                // that does not know a model refuses the question.
                let shader_model = (0x60..=0x69u32)
                    .rev()
                    .find_map(|m| {
                        let mut sm = D3D12_FEATURE_DATA_SHADER_MODEL {
                            HighestShaderModel: windows::Win32::Graphics::Direct3D12::D3D_SHADER_MODEL(m as i32),
                        };
                        device
                            .CheckFeatureSupport(D3D12_FEATURE_SHADER_MODEL, (&raw mut sm).cast(), size_of_val(&sm) as u32)
                            .is_ok()
                            .then_some(sm.HighestShaderModel.0 as u32)
                    })
                    .unwrap_or(0);
                let card = format!(
                    "{}, ray tracing tier {}.{}, shader model {}.{}",
                    String::from_utf16_lossy(&desc.Description).trim_end_matches('\0'),
                    tier / 10,
                    tier % 10,
                    shader_model >> 4,
                    shader_model & 15
                );
                let device: ID3D12Device5 = device.cast().ok()?;
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).ok()?;
                let list: ID3D12GraphicsCommandList4 =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).ok()?;
                // Made open: closed, so each run starts with a reset.
                list.Close().ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;
                let mut stamps: Option<ID3D12QueryHeap> = None;
                device
                    .CreateQueryHeap(
                        &D3D12_QUERY_HEAP_DESC {
                            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                            Count: 2,
                            NodeMask: 0,
                        },
                        &mut stamps,
                    )
                    .ok()?;
                let frequency = queue.GetTimestampFrequency().ok()?;
                Some(Gpu {
                    device,
                    queue,
                    allocator,
                    list,
                    fence,
                    event,
                    done: std::cell::Cell::new(0),
                    stamps: stamps?,
                    frequency,
                    card,
                })
            }
        }

        /// A buffer of `size` bytes.
        pub fn buffer(
            &self,
            size: u64,
            heap: D3D12_HEAP_TYPE,
            flags: D3D12_RESOURCE_FLAGS,
            state: D3D12_RESOURCE_STATES,
        ) -> ID3D12Resource {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(256),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: flags,
                ..Default::default()
            };
            let mut r: Option<ID3D12Resource> = None;
            // SAFETY: a committed buffer of ours.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES {
                            Type: heap,
                            ..Default::default()
                        },
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        state,
                        None,
                        &mut r,
                    )
                    .expect("a buffer");
            }
            r.expect("a buffer")
        }

        /// An upload buffer holding `bytes`.
        pub fn upload(&self, bytes: &[u8]) -> ID3D12Resource {
            let b = self.buffer(
                bytes.len() as u64,
                D3D12_HEAP_TYPE_UPLOAD,
                D3D12_RESOURCE_FLAG_NONE,
                D3D12_RESOURCE_STATE_GENERIC_READ,
            );
            // SAFETY: the mapping is at least `bytes.len()` long.
            unsafe {
                let mut p = std::ptr::null_mut();
                b.Map(0, None, Some(&mut p)).expect("map");
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
                b.Unmap(0, None);
            }
            b
        }

        /// A buffer in video memory holding `bytes`, in the state build
        /// inputs and shader reads need (copied from an upload buffer with
        /// a list of its own, so the bytes are there when it returns).
        pub fn resident(&self, bytes: &[u8]) -> ID3D12Resource {
            let from = self.upload(bytes);
            let to = self.buffer(
                bytes.len() as u64,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_FLAG_NONE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            self.run(|list| unsafe {
                // SAFETY: both buffers live until `run` has waited.
                list.CopyBufferRegion(&to, 0, &from, 0, bytes.len() as u64);
                list.ResourceBarrier(&[transition(
                    &to,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                )]);
            });
            to
        }

        /// The bytes of `r` (a buffer in `state`), read back.
        pub fn read(&self, r: &ID3D12Resource, size: u64, state: D3D12_RESOURCE_STATES) -> Vec<u8> {
            let back = self.buffer(size, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST);
            self.run(|list| unsafe {
                // SAFETY: both buffers live until `run` has waited.
                list.ResourceBarrier(&[transition(r, state, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                list.CopyBufferRegion(&back, 0, r, 0, size);
                list.ResourceBarrier(&[transition(r, D3D12_RESOURCE_STATE_COPY_SOURCE, state)]);
            });
            let mut out = vec![0u8; size as usize];
            // SAFETY: the mapping is `size` long and the copy has finished.
            unsafe {
                let mut p = std::ptr::null_mut();
                back.Map(0, None, Some(&mut p)).expect("map");
                std::ptr::copy_nonoverlapping(p.cast::<u8>(), out.as_mut_ptr(), out.len());
                back.Unmap(0, None);
            }
            out
        }

        /// Records with `record`, runs the list and waits for it.
        pub fn run(&self, record: impl FnOnce(&ID3D12GraphicsCommandList4)) {
            // SAFETY: one list at a time; the wait keeps everything it used
            // alive until the GPU is done.
            unsafe {
                self.allocator.Reset().expect("allocator reset");
                self.list.Reset(&self.allocator, None).expect("list reset");
                record(&self.list);
                self.list.Close().expect("list close");
                self.queue.ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>().unwrap())]);
                let n = self.done.get() + 1;
                self.done.set(n);
                self.queue.Signal(&self.fence, n).expect("signal");
                if self.fence.GetCompletedValue() < n {
                    self.fence.SetEventOnCompletion(n, self.event).expect("event");
                    WaitForSingleObject(self.event, INFINITE);
                }
                if let Err(e) = self.device.GetDeviceRemovedReason() {
                    panic!("device removed: {e}");
                }
            }
        }

        /// A structure's buffers, sized as the driver says for `inputs`.
        fn allocate(
            &self,
            inputs: &D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
        ) -> (ID3D12Resource, ID3D12Resource, D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO) {
            let mut sizes = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            // SAFETY: the inputs point at descriptions that outlive the call.
            unsafe { self.device.GetRaytracingAccelerationStructurePrebuildInfo(inputs, &mut sizes) };
            assert_ne!(sizes.ResultDataMaxSizeInBytes, 0, "the driver sizes no structure");
            let result = self.buffer(
                sizes.ResultDataMaxSizeInBytes,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
            );
            let scratch = self.buffer(
                sizes.ScratchDataSizeInBytes.max(sizes.UpdateScratchDataSizeInBytes),
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
            (result, scratch, sizes)
        }

        /// A bottom level of `geometry` (not built yet: [`Structure::record`]).
        pub fn bottom(
            &self,
            geometry: Vec<D3D12_RAYTRACING_GEOMETRY_DESC>,
            flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS,
        ) -> Structure {
            let mut s = Structure {
                result: self.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ),
                scratch: self.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ),
                sizes: Default::default(),
                geometry,
                kind: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                flags,
                instances: (0, 0),
            };
            (s.result, s.scratch, s.sizes) = self.allocate(&s.inputs(false));
            s
        }

        /// A top level of `count` instance descriptions at `instances`.
        pub fn top(&self, instances: u64, count: u32, flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS) -> Structure {
            let mut s = Structure {
                result: self.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ),
                scratch: self.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ),
                sizes: Default::default(),
                geometry: Vec::new(),
                kind: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
                flags,
                instances: (instances, count),
            };
            (s.result, s.scratch, s.sizes) = self.allocate(&s.inputs(false));
            s
        }

        /// The seconds between timestamps `from` and `to` of `stamps`.
        pub fn stamp_seconds(&self, stamps: &[u64], from: usize, to: usize) -> f64 {
            stamps[to].saturating_sub(stamps[from]) as f64 / self.frequency as f64
        }
    }
}

#[cfg(test)]
mod tests;
