//! The host's bottom levels: the title's geometry found in the host copies
//! of title memory (which copy: the critic's item 9, see `super::native`),
//! then a build, a refit or a build from scratch of an update.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

use super::count::{C, REBUILT, Refusal, add};
use super::native::{Held, Refused, Structure, lease_scratch, lease_structure, prebuild};
use super::stamps::Gpu;
use super::table::{self, Decision, GeometryShape, PERFORM_UPDATE, Shape, Table, Why};
use super::{Build, Geometry, Level, Origin, Triangles};
use crate::host::HostList;

/// Bytes of a vertex's position in a vertex format the host builds from, and
/// the alignment of its components; formats of ray-tracing tier 1.1 only on
/// a host that has it.
fn vertex_format(format: u32) -> Option<(u64, u64)> {
    let tier_1_1 = super::tier() >= super::TIER_1_1;
    match format {
        // R32G32B32_FLOAT, R32G32_FLOAT
        6 => Some((12, 4)),
        16 => Some((8, 4)),
        // R16G16B16A16_FLOAT, R16G16_FLOAT, R16G16B16A16_SNORM, R16G16_SNORM
        10 | 13 => Some((8, 2)),
        34 | 37 => Some((4, 2)),
        // Tier 1.1: R16G16B16A16_UNORM, R16G16_UNORM, R10G10B10A2_UNORM,
        // R8G8B8A8_UNORM/SNORM, R8G8_UNORM/SNORM.
        11 if tier_1_1 => Some((8, 2)),
        35 if tier_1_1 => Some((4, 2)),
        24 if tier_1_1 => Some((4, 4)),
        28 | 31 if tier_1_1 => Some((4, 1)),
        49 | 51 if tier_1_1 => Some((2, 1)),
        _ => None,
    }
}

/// The host GPU address of `len` bytes of title memory at `address`, in the
/// host copy a build is to read (see the module), brought up to date when
/// `host`'s list is submitted.
fn resolve(host: &HostList, address: u64, len: u64) -> Result<u64, Refused> {
    let mut written: Vec<Arc<crate::shadow::Shadow>> = Vec::new();
    for copy in crate::shadow::copies_over(address, len) {
        if copy.written() && !written.iter().any(|w| Arc::ptr_eq(w, &copy)) {
            written.push(copy);
        }
    }
    if written.len() > 1 {
        return Err((Refusal::SeveralWritten, format!("{address:#x}+{len:#x} in {} copies", written.len())));
    }
    let drawn = crate::shadow::find_any(address, len);
    let copy = match (written.pop(), drawn) {
        (Some(w), drawn) => {
            if !drawn.as_ref().is_some_and(|d| Arc::ptr_eq(d, &w)) {
                add(C::ShadersCopy, 1);
            }
            w
        }
        (None, Some(d)) => d,
        (None, None) => return Err((Refusal::NoCopy, format!("{address:#x}+{len:#x}"))),
    };
    if copy.offset_in(address).is_none() || copy.left(address) < len {
        return Err((Refusal::NoCopy, format!("{address:#x}+{len:#x} past its copy")));
    }
    // Outside title buffers, a page the shaders that find memory by address
    // hold in an arena of ours may have bytes they stored that only the card
    // has (`crate::memory`): counted, read as a draw would.
    const PAGE: u64 = 1 << 16;
    if crate::shadow::in_buffer(address, len).is_none()
        && (address / PAGE..=(address + len - 1) / PAGE).take(64).any(|p| crate::memory::home(p * PAGE).is_some_and(|h| h.2))
    {
        add(C::ByAddress, 1);
    }
    copy.refresh(host, address, len);
    Ok(copy.gpu(address))
}

/// The host's description of a triangle geometry, and its shape.
fn triangles(host: &HostList, t: &Triangles) -> Result<(D3D12_RAYTRACING_GEOMETRY_DESC, GeometryShape), Refused> {
    let (size, align) = vertex_format(t.vertex_format)
        .ok_or_else(|| (Refusal::VertexFormat, crate::dxr::format_name(t.vertex_format)))?;
    let index = match t.index_format {
        0 => 0,
        57 => 2,
        42 => 4,
        f => return Err((Refusal::IndexFormat, crate::dxr::format_name(f))),
    };
    let unaligned = !t.vertex_stride.is_multiple_of(align)
        || !t.vertex_buffer.is_multiple_of(align)
        || (index > 0 && !t.index_buffer.is_multiple_of(index))
        || !t.transform.is_multiple_of(16);
    if unaligned {
        return Err((
            Refusal::Unaligned,
            format!("vertices {:#x} stride {}, indices {:#x}, transform {:#x}", t.vertex_buffer, t.vertex_stride, t.index_buffer, t.transform),
        ));
    }
    let vertices = match t.vertex_count {
        0 => 0,
        n => resolve(host, t.vertex_buffer, t.vertex_stride * u64::from(n - 1) + size)?,
    };
    let indices = if index > 0 && t.index_count > 0 { resolve(host, t.index_buffer, index * u64::from(t.index_count))? } else { 0 };
    let transform = if t.transform != 0 { resolve(host, t.transform, 48)? } else { 0 };
    let desc = D3D12_RAYTRACING_GEOMETRY_DESC {
        Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
        Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(t.flags as i32),
        Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
            Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                Transform3x4: transform,
                IndexFormat: DXGI_FORMAT(t.index_format as i32),
                VertexFormat: DXGI_FORMAT(t.vertex_format as i32),
                IndexCount: if index > 0 { t.index_count } else { 0 },
                VertexCount: t.vertex_count,
                IndexBuffer: indices,
                VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: vertices, StrideInBytes: t.vertex_stride },
            },
        },
    };
    let shape = GeometryShape {
        kind: 0,
        flags: t.flags,
        index_format: t.index_format,
        vertex_format: t.vertex_format,
        index_count: t.index_count,
        vertex_count: t.vertex_count,
        transform: t.transform != 0,
    };
    Ok((desc, shape))
}

/// The triangles a geometry holds.
fn triangle_count(t: &Triangles) -> u64 {
    u64::from(if t.index_format == 0 { t.vertex_count } else { t.index_count }) / 3
}

/// `..._BUILD_FLAG_PREFER_FAST_TRACE` and `_FAST_BUILD`, which Direct3D does
/// not take together.
const FAST_TRACE: u32 = 0x4;
const FAST_BUILD: u32 = 0x8;

/// The flags the host builds with: the title's, but never both preferences
/// (Direct3D refuses that; the walks matter more), and for an unpacked
/// structure (built once, walked for as long as it is streamed in) a
/// preference for fast walks if it has none.
pub(super) fn host_flags(flags: u32, origin: Origin) -> u32 {
    let mut flags = flags & !PERFORM_UPDATE;
    if flags & FAST_TRACE != 0 {
        flags &= !FAST_BUILD;
    }
    if origin == Origin::Rehydrated && flags & (FAST_TRACE | FAST_BUILD) == 0 {
        flags |= FAST_TRACE;
    }
    flags
}

/// Bottom levels said in full (the host's sizes beside the title's ask).
const DESCRIBED: u64 = 64;
static DESCRIBED_SO_FAR: AtomicU64 = AtomicU64::new(0);

#[allow(clippy::too_many_arguments)]
pub(super) fn bottom(
    host: &HostList,
    list: &ID3D12GraphicsCommandList4,
    gpu: &Gpu,
    table: &mut Table<Structure>,
    b: &Build,
    origin: Origin,
    frame: u64,
    held: &mut Held,
) -> Result<(), Refused> {
    let Level::Bottom { geometries, whole } = &b.level else {
        return Ok(());
    };
    if !whole {
        return Err((Refusal::NotWhole, format!("{} geometries read", geometries.len())));
    }
    let mut descs = Vec::with_capacity(geometries.len());
    let mut shapes = Vec::with_capacity(geometries.len());
    let (mut tris, mut boxes) = (0u64, 0u64);
    for g in geometries {
        match *g {
            Geometry::Triangles(t) => {
                let (desc, shape) = triangles(host, &t)?;
                tris += triangle_count(&t);
                descs.push(desc);
                shapes.push(shape);
            }
            Geometry::Boxes { flags, count, start, stride } => {
                if !start.is_multiple_of(8) || !stride.is_multiple_of(8) || stride < 24 {
                    return Err((Refusal::Unaligned, format!("boxes at {start:#x} stride {stride}")));
                }
                let at = if count > 0 { resolve(host, start, stride * (count - 1) + 24)? } else { 0 };
                boxes += count;
                descs.push(D3D12_RAYTRACING_GEOMETRY_DESC {
                    Type: D3D12_RAYTRACING_GEOMETRY_TYPE_PROCEDURAL_PRIMITIVE_AABBS,
                    Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(flags as i32),
                    Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                        AABBs: D3D12_RAYTRACING_GEOMETRY_AABBS_DESC {
                            AABBCount: count,
                            AABBs: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE { StartAddress: at, StrideInBytes: stride },
                        },
                    },
                });
                shapes.push(GeometryShape {
                    kind: 1,
                    flags,
                    index_format: 0,
                    vertex_format: 0,
                    index_count: 0,
                    vertex_count: count as u32,
                    transform: false,
                });
            }
            Geometry::Other { kind, .. } => return Err((Refusal::OtherGeometry, format!("type {kind}"))),
        }
    }
    let update = origin == Origin::Batched && b.flags & PERFORM_UPDATE != 0;
    let flags = host_flags(b.flags, origin);
    let shape = Shape::Bottom(shapes);
    let decision = table::decide(update, b.dest, b.source, flags, &shape, table);
    let refit = matches!(decision, Decision::Refit { .. });
    let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
        Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS((flags | if refit { PERFORM_UPDATE } else { 0 }) as i32),
        NumDescs: descs.len() as u32,
        DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
        Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { pGeometryDescs: descs.as_ptr() },
    };
    let sizes = prebuild(gpu, &inputs)?;
    let (dest, source, made) = match decision {
        Decision::Refit { in_place } => {
            let from = if b.source == 0 { b.dest } else { b.source };
            let src = table.get(from).map(|e| e.current.clone()).ok_or((Refusal::Prebuild, "a source gone".to_string()))?;
            held.leases.push(src.lease.clone());
            if in_place {
                (src.gpu(), src.gpu(), None)
            } else {
                let lease = lease_structure(sizes.ResultDataMaxSizeInBytes.max(src.lease.len()))?;
                let s = Structure::new(lease);
                (s.gpu(), src.gpu(), Some(s))
            }
        }
        Decision::Build | Decision::Rebuild(_) => {
            let s = Structure::new(lease_structure(sizes.ResultDataMaxSizeInBytes)?);
            (s.gpu(), 0, Some(s))
        }
    };
    let scratch_len = if refit { sizes.UpdateScratchDataSizeInBytes } else { sizes.ScratchDataSizeInBytes };
    let scratch = lease_scratch(scratch_len)?;
    let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
        DestAccelerationStructureData: dest,
        Inputs: inputs,
        SourceAccelerationStructureData: source,
        ScratchAccelerationStructureData: scratch.gpu,
    };
    // SAFETY: an open list; the description and the geometry descriptions it
    // points at outlive the call; every address is of a buffer the list holds
    // until it is reset (the copies by their refresh, ours by `held`), each in
    // the state a build needs.
    unsafe { list.BuildRaytracingAccelerationStructure(&desc, None) };
    held.leases.push(scratch);
    match (decision, made) {
        (Decision::Refit { in_place: true }, _) => {
            table.refit_in_place(b.dest, frame);
            add(C::Refit, 1);
            add(C::RefitInPlace, 1);
            add(C::RefitTriangles, tris);
        }
        (_, Some(s)) => {
            table.built(b.dest, flags, shape, s.clone(), frame);
            held.made.push(s);
            match decision {
                Decision::Refit { .. } => {
                    add(C::Refit, 1);
                    add(C::RefitTriangles, tris);
                }
                Decision::Rebuild(why) => {
                    add(C::Built, 1);
                    add(C::Rebuilt, 1);
                    add(C::Triangles, tris);
                    REBUILT[Why::ALL.iter().position(|&w| w == why).unwrap_or(0)].fetch_add(1, Relaxed);
                }
                Decision::Build => {
                    add(C::Built, 1);
                    add(C::Triangles, tris);
                }
            }
        }
        (_, None) => {}
    }
    add(C::Boxes, boxes);
    if DESCRIBED_SO_FAR.fetch_add(1, Relaxed) < DESCRIBED {
        tracing::info!(
            "host ray tracing: bottom level dest {:#x} ({origin:?}, {} geometries, {tris} triangles, {boxes} boxes, flags {:#x}): {decision:?}; host sizes result {} scratch {} update scratch {}",
            b.dest,
            descs.len(),
            flags,
            sizes.ResultDataMaxSizeInBytes,
            sizes.ScratchDataSizeInBytes,
            sizes.UpdateScratchDataSizeInBytes,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::native::ALLOW_COMPACTION;
    use super::*;

    #[test]
    fn the_host_builds_with_the_titles_flags_but_never_both_preferences() {
        // The update bit is the decision's, not the build's.
        assert_eq!(host_flags(0x1 | PERFORM_UPDATE, Origin::Batched), 0x1);
        // Both preferences: the fast walk wins.
        assert_eq!(host_flags(FAST_TRACE | FAST_BUILD | ALLOW_COMPACTION, Origin::Batched), FAST_TRACE | ALLOW_COMPACTION);
        // Unpacked structures are walked for long: fast walks unless asked otherwise.
        assert_eq!(host_flags(ALLOW_COMPACTION, Origin::Rehydrated), FAST_TRACE | ALLOW_COMPACTION);
        assert_eq!(host_flags(FAST_BUILD, Origin::Rehydrated), FAST_BUILD);
        assert_eq!(host_flags(0, Origin::Batched), 0);
    }

    #[test]
    fn vertex_formats_of_tier_one_are_taken_and_others_named() {
        assert_eq!(vertex_format(6), Some((12, 4)));
        assert_eq!(vertex_format(10), Some((8, 2)));
        assert_eq!(vertex_format(2), None);
        assert_eq!(crate::dxr::format_name(2), "R32G32B32A32_FLOAT");
        assert_eq!(triangle_count(&Triangles { index_format: 57, index_count: 300, vertex_count: 7, ..Default::default() }), 100);
        assert_eq!(triangle_count(&Triangles { vertex_count: 9, ..Default::default() }), 3);
    }
}
