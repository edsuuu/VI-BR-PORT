//! The console's packed geometry expanded for the host's builds
//! (`VICTORY_DXR_EXPAND=1`, off by default; only with `VICTORY_DXR_NATIVE`):
//! index format 53 becomes `R32_UINT` and quantized positions become
//! `R32G32B32_FLOAT` ([`super::packed`]), in upload memory of the list that
//! records the build, which the host's builder reads.
//!
//! When: the build is recorded at the title list's place of it, naming that
//! upload memory; the expansion itself is a step of the list's submission
//! (`HostList::defer_host_read`), which reads the title's data in title
//! memory as it is then, as the instances of a top level are
//! (`super::top`). A structure the title unpacks (static) is expanded once,
//! when the list that unpacks it is submitted; a structure the title builds
//! or updates is expanded at every build and update. The upload memory goes
//! back to the list's pool when the list is reset: nothing stays on the card
//! but the built structure.
//!
//! Refused (counted by reason in the five-second line), so that nothing
//! decoded wrong is built: data the CPU cannot read when the build is
//! recorded; data in a host copy that shaders write (what the CPU sees may
//! be old there: those need a compute pass of ours over the copy, not made);
//! a layout off what the builder's stages read (fewer triangle records than
//! triangles, a stride below a position). Counted but expanded: data in
//! pages of by-address memory the card holds (`crate::memory`), as a build
//! of unpacked data reads them (`super::bottom`).

use std::sync::OnceLock;

use super::Triangles;
use super::count::{C, Refusal, add};
use super::native::Refused;
use super::packed;
use crate::host::HostList;

/// `VICTORY_DXR_EXPAND=1`.
pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("VICTORY_DXR_EXPAND").is_ok_and(|v| v.trim() == "1");
        if on && super::native_on() {
            tracing::info!(
                "VICTORY_DXR_EXPAND: the host's builds expand the console's packed indices (format 53) and quantized positions first"
            );
        }
        on
    })
}

/// `VICTORY_DXR_EXPAND_WRITTEN=1` (off by default: such data are refused):
/// packed data in a host copy shaders were declared to write are expanded
/// from the CPU's view of title memory anyway, and counted. What a list's
/// shaders write into such a copy is copied back into title memory once
/// the list has run (`Shadow::gpu_writes`; in place they write title memory
/// itself), so the CPU's view lags only by the lists still running when the
/// expanding one is submitted. Memory shaders write by address is not
/// brought back; that is counted apart (`in pages of by-address memory`),
/// as before.
fn written_too() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let asked = std::env::var("VICTORY_DXR_EXPAND_WRITTEN").is_ok_and(|v| v.trim() == "1");
        if asked && on() {
            tracing::info!("VICTORY_DXR_EXPAND_WRITTEN: packed data in host copies shaders write are expanded from title memory");
        }
        asked
    })
}

/// What one build's expansions add to the five-second line once it is
/// recorded.
#[derive(Default)]
pub(super) struct Tally {
    index_lists: u64,
    triangles: u64,
    position_lists: u64,
    vertices: u64,
    bytes: u64,
    by_address: u64,
    written: u64,
}

impl Tally {
    /// The build was recorded: its expansions count.
    pub(super) fn count(self) {
        add(C::ExpandedIndexLists, self.index_lists);
        add(C::ExpandedTriangles, self.triangles);
        add(C::ExpandedPositionLists, self.position_lists);
        add(C::ExpandedVertices, self.vertices);
        add(C::ExpandedBytes, self.bytes);
        add(C::ExpandedByAddress, self.by_address);
        add(C::ExpandedWritten, self.written);
    }
}

/// Whether `len` bytes of title memory at `at` can be read by the CPU now.
fn readable(at: u64, len: u64) -> bool {
    at != 0 && usize::try_from(len).is_ok_and(|len| crate::texture::committed(at as *const u8, len))
}

/// `len` bytes of title memory at `at`, which [`readable`] said are there.
///
/// # Safety
/// Committed, readable memory of the process, all of it, while the slice
/// lives.
unsafe fn title<'a>(at: u64, len: u64) -> &'a [u8] {
    // SAFETY: as the caller says.
    unsafe { std::slice::from_raw_parts(at as *const u8, len as usize) }
}

/// The checks a range of packed data passes before a build names its
/// expansion: readable now, in no host copy shaders write. Counts pages of
/// by-address memory the card holds.
fn checked(what: &str, at: u64, len: u64, tally: &mut Tally) -> Result<(), Refused> {
    if !readable(at, len) {
        return Err((
            Refusal::PackedUnreadable,
            format!("{what} at {at:#x}+{len:#x}"),
        ));
    }
    if crate::shadow::copies_over(at, len)
        .iter()
        .any(|c| c.written())
    {
        if !written_too() {
            return Err((
                Refusal::PackedWritten,
                format!("{what} at {at:#x}+{len:#x}"),
            ));
        }
        tally.written += 1;
    }
    const PAGE: u64 = 1 << 16;
    if crate::shadow::in_buffer(at, len).is_none()
        && (at / PAGE..=(at + len - 1) / PAGE)
            .take(64)
            .any(|p| crate::memory::home(p * PAGE).is_some_and(|h| h.2))
    {
        tally.by_address += 1;
    }
    Ok(())
}

/// Upload memory of `host`'s list for `len` bytes: the GPU address and the
/// CPU's, as an address (the list holds it until it is reset).
fn upload(host: &HostList, len: u64) -> Result<(u64, usize), Refused> {
    host.scratch(len)
        .map(|(gpu, cpu)| (gpu, cpu as usize))
        .ok_or((
            Refusal::Memory,
            format!("no upload memory for an expansion of {len} bytes"),
        ))
}

/// The geometry's packed indices (format 53) as `R32_UINT`, `IndexCount / 3`
/// triangles, in upload memory of `host`'s list written at its submission:
/// the GPU address.
pub(super) fn indices(host: &HostList, t: &Triangles, tally: &mut Tally) -> Result<u64, Refused> {
    let at = t.index_buffer;
    let triangles = t.index_count / 3;
    let flip = t.vertex_stride & packed::FLIP != 0;
    let vertices = t.vertex_count;
    if !readable(at, 8) {
        return Err((
            Refusal::PackedUnreadable,
            format!("packed indices at {at:#x}"),
        ));
    }
    // SAFETY: 8 readable bytes.
    let (blocks, records) = packed::header(unsafe { title(at, 8) }).unwrap_or_default();
    if records < triangles {
        return Err((
            Refusal::PackedLayout,
            format!("{records} triangle records for {triangles} triangles at {at:#x}"),
        ));
    }
    let head = packed::records_end(blocks, records);
    if !readable(at, head) {
        return Err((
            Refusal::PackedUnreadable,
            format!("the records of packed indices at {at:#x}+{head:#x}"),
        ));
    }
    // SAFETY: `head` readable bytes.
    let span = packed::indices_span(unsafe { title(at, head) }, triangles).unwrap_or(head);
    checked("packed indices", at, span, tally)?;
    let len = 12 * u64::from(triangles);
    let (gpu, cpu) = upload(host, len)?;
    host.defer_host_read(at as usize, span as usize, move || {
        // SAFETY: upload memory of the list, `len` bytes, written only here.
        let out =
            unsafe { std::slice::from_raw_parts_mut(cpu as *mut u32, 3 * triangles as usize) };
        if !readable(at, span) {
            out.fill(0);
            add(C::ExpandedUnreadable, 1);
            return;
        }
        // SAFETY: readable now, read during the step.
        let odd =
            packed::expand_indices(unsafe { title(at, span) }, triangles, flip, vertices, out);
        add(C::ExpandedDegenerate, u64::from(odd.degenerate));
    });
    tally.index_lists += 1;
    tally.triangles += u64::from(triangles);
    tally.bytes += len;
    Ok(gpu)
}

/// The geometry's quantized positions as `R32G32B32_FLOAT`, 12 bytes apart,
/// in upload memory of `host`'s list written at its submission: the GPU
/// address.
pub(super) fn positions(host: &HostList, t: &Triangles, tally: &mut Tally) -> Result<u64, Refused> {
    let stride = t.vertex_stride & packed::STRIDE;
    let count = t.vertex_count;
    if stride < packed::POSITION {
        return Err((
            Refusal::PackedLayout,
            format!(
                "quantized positions {stride} bytes apart at {:#x}",
                t.vertex_buffer
            ),
        ));
    }
    let Some(at) = t.vertex_buffer.checked_sub(packed::QUANTIZATION) else {
        return Err((
            Refusal::PackedLayout,
            format!("quantized positions at {:#x}", t.vertex_buffer),
        ));
    };
    let span = packed::positions_span(stride, count);
    checked("quantized positions", at, span, tally)?;
    let len = 12 * u64::from(count);
    let (gpu, cpu) = upload(host, len)?;
    host.defer_host_read(at as usize, span as usize, move || {
        // SAFETY: upload memory of the list, `len` bytes, written only here.
        let out = unsafe { std::slice::from_raw_parts_mut(cpu as *mut f32, 3 * count as usize) };
        if !readable(at, span) {
            for v in out.chunks_exact_mut(3) {
                v.copy_from_slice(&[f32::NAN, 0.0, 0.0]);
            }
            add(C::ExpandedUnreadable, 1);
            return;
        }
        // SAFETY: readable now, read during the step.
        let odd = packed::expand_positions(unsafe { title(at, span) }, stride, count, out);
        add(C::ExpandedNotFinite, u64::from(odd.not_finite));
    });
    tally.position_lists += 1;
    tally.vertices += u64::from(count);
    tally.bytes += len;
    Ok(gpu)
}
