//! The host's top levels (`VICTORY_DXR_NATIVE`): the title's instance
//! descriptions with each bottom level's address rewritten from the title's
//! destination to the host structure built for it.
//!
//! The rewrite is done on the CPU **at the list's submission**, not when the
//! build is recorded (the critic's item 10): the instances lie in title
//! memory the title's CPU may write until it submits (a list's upload
//! memory). At submission the step of ours that rewrites them copies them
//! into upload memory of the list, which the host's build reads when the
//! list runs; each bottom level is the newest host version of its
//! destination whose build was submitted before ([`super::table`]), and an
//! instance whose bottom level the host has not got is made inactive (no
//! structure, mask 0) and counted. The top level holds the bottom levels
//! it named for as long as it is held itself.
//!
//! shortcut: instances the title's shaders write (on the card) are not seen
//! by the CPU; if a run shows most instances naming no host structure, the
//! rewrite has to move to a compute pass of ours.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use windows::Win32::Graphics::Direct3D12::*;

use super::native::{self, C, Gpu, Held, Refusal, Refused, Structure};
use super::table::{Shape, Table};
use super::{Build, Level, Origin};
use crate::host::HostList;

/// Bytes of a `D3D12_RAYTRACING_INSTANCE_DESC`.
pub(crate) const INSTANCE: usize = 64;

/// Records the host's build of the top level `b` (after a barrier the caller
/// put after the bottom levels), and the rewrite of its instances at
/// submission. The structure built and where its instances go (the upload
/// memory's CPU address).
pub(super) fn top(
    host: &HostList,
    list: &ID3D12GraphicsCommandList4,
    gpu: &Gpu,
    table: &mut Table<Structure>,
    b: &Build,
    frame: u64,
    held: &mut Held,
) -> Result<(Arc<Structure>, usize), Refused> {
    let Level::Top { instances, count, layout } = b.level else {
        return Err((Refusal::OtherGeometry, "not a top level".into()));
    };
    if layout != 0 {
        return Err((Refusal::InstanceLayout, format!("{count} instances at {instances:#x}")));
    }
    let bytes = count as usize * INSTANCE;
    let (descs, cpu) = match count {
        0 => (0, 0),
        _ => {
            let (gpu, cpu) =
                host.scratch(bytes as u64).ok_or((Refusal::Memory, format!("no upload memory for {count} instances")))?;
            (gpu, cpu as usize)
        }
    };
    let flags = native::host_flags(b.flags, Origin::Batched);
    let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
        Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
        Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(flags as i32),
        NumDescs: count,
        DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
        Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 { InstanceDescs: descs },
    };
    let sizes = native::prebuild(gpu, &inputs)?;
    let s = Structure::new(native::lease_structure(sizes.ResultDataMaxSizeInBytes)?);
    let scratch = native::lease_scratch(sizes.ScratchDataSizeInBytes)?;
    if count > 0 {
        let top = s.clone();
        host.defer_host_read(instances as usize, bytes, move || rewrite(instances, count as usize, cpu, &top));
    }
    let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
        DestAccelerationStructureData: s.gpu(),
        Inputs: inputs,
        SourceAccelerationStructureData: 0,
        ScratchAccelerationStructureData: scratch.gpu,
    };
    // SAFETY: an open list; the instances are upload memory the list holds,
    // written at its submission; the structure and scratch are held by the
    // list (`held`).
    unsafe { list.BuildRaytracingAccelerationStructure(&desc, None) };
    table.built(b.dest, flags, Shape::Top(count), s.clone(), frame);
    held.leases.push(scratch);
    held.made.push(s.clone());
    native::add(C::TopBuilt, 1);
    native::add(C::Instances, u64::from(count));
    Ok((s, cpu))
}

/// At submission: the title's `count` instances at `instances`, rewritten
/// into `to` (see the module).
fn rewrite(instances: u64, count: usize, to: usize, top: &Arc<Structure>) {
    let len = count * INSTANCE;
    // SAFETY: upload memory of the list, `len` bytes, written only here.
    let out = unsafe { std::slice::from_raw_parts_mut(to as *mut u8, len) };
    if !crate::texture::committed(instances as *const u8, len) {
        out.fill(0);
        native::add(C::InstancesUnknown, count as u64);
        native::add(C::InstancesUnreadable, count as u64);
        return;
    }
    // SAFETY: committed title memory of that length, read as bytes.
    out.copy_from_slice(unsafe { std::slice::from_raw_parts(instances as *const u8, len) });
    let frame = crate::frame::current();
    let (mut named, unknown) = {
        let mut table = native::table();
        rename(out, |blas| table.usable(blas, frame).map(|v| (v.gpu(), v)))
    };
    named.sort_by_key(|n| Arc::as_ptr(n) as usize);
    named.dedup_by(|a, b| Arc::ptr_eq(a, b));
    *top.named.lock().unwrap_or_else(|e| e.into_inner()) = named;
    native::add(C::InstancesUnknown, unknown);
    static SAID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if unknown > 0 && SAID.fetch_add(1, Relaxed) < 4 {
        let first = out.chunks_exact(INSTANCE).position(|d| d[51] == 0 && d[56..64] == [0; 8]);
        // (The title's address of the first unknown one is in its own copy.)
        let title = first.map(|k| {
            // SAFETY: as above.
            let d = unsafe { std::slice::from_raw_parts((instances as usize + k * INSTANCE) as *const u8, INSTANCE) };
            u64::from_le_bytes(d[56..64].try_into().unwrap_or_default())
        });
        tracing::info!(
            "host ray tracing: a top level of {count} instances at {instances:#x}: {unknown} name no host structure (the first names {:#x})",
            title.unwrap_or(0)
        );
    }
}

/// Each instance description of `out` names its bottom level by the
/// address `usable` gives for the title's (and the handle that holds it);
/// one `usable` has nothing for is made inactive: no structure, mask 0.
/// The handles named (runs of the same one once), and how many were not.
fn rename<H: Clone>(out: &mut [u8], mut usable: impl FnMut(u64) -> Option<(u64, H)>) -> (Vec<H>, u64) {
    let mut named = Vec::new();
    let mut unknown = 0;
    let mut last: (u64, Option<(u64, H)>) = (u64::MAX, None);
    for d in out.chunks_exact_mut(INSTANCE) {
        let blas = u64::from_le_bytes(d[56..64].try_into().unwrap_or_default());
        if blas != last.0 {
            last = (blas, usable(blas));
            if let Some((_, h)) = &last.1 {
                named.push(h.clone());
            }
        }
        match &last.1 {
            Some((at, _)) => d[56..64].copy_from_slice(&at.to_le_bytes()),
            None => {
                d[51] = 0;
                d[56..64].fill(0);
                unknown += 1;
            }
        }
    }
    (named, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(id: u32, mask: u8, blas: u64) -> Vec<u8> {
        let mut d = vec![0u8; INSTANCE];
        d[0..4].copy_from_slice(&1f32.to_le_bytes());
        d[48..52].copy_from_slice(&(id | u32::from(mask) << 24).to_le_bytes());
        d[52..56].copy_from_slice(&(3u32 | 0x04 << 24).to_le_bytes());
        d[56..64].copy_from_slice(&blas.to_le_bytes());
        d
    }

    #[test]
    fn instances_name_the_hosts_bottom_levels_or_go_inactive() {
        let mut out = [instance(7, 0xff, 0x1000), instance(8, 0x0f, 0x1000), instance(9, 0x01, 0x2000), instance(10, 0xff, 0x3000)].concat();
        let asked = std::cell::Cell::new(0);
        let (named, unknown) = rename(&mut out, |blas| {
            asked.set(asked.get() + 1);
            match blas {
                0x1000 => Some((0xa_0000, 'a')),
                0x3000 => Some((0xc_0000, 'c')),
                _ => None,
            }
        });
        // A run of the same bottom level is looked up once.
        assert_eq!(asked.get(), 3);
        assert_eq!((named, unknown), (vec!['a', 'c'], 1));
        let at = |k: usize| u64::from_le_bytes(out[k * INSTANCE + 56..k * INSTANCE + 64].try_into().unwrap());
        assert_eq!([at(0), at(1), at(2), at(3)], [0xa_0000, 0xa_0000, 0, 0xc_0000]);
        // Everything else passes as the title gave it; the unknown one has mask 0.
        assert_eq!(out[51], 0xff);
        assert_eq!(out[INSTANCE + 51], 0x0f);
        assert_eq!(out[2 * INSTANCE + 51], 0);
        assert_eq!(&out[2 * INSTANCE + 48..2 * INSTANCE + 51], &[9, 0, 0]);
        assert_eq!(&out[INSTANCE + 52..INSTANCE + 56], &(3u32 | 0x04 << 24).to_le_bytes());
        assert_eq!(&out[0..4], &1f32.to_le_bytes());
    }
}
