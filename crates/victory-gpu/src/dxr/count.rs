//! What the host's own builds count between two five-second lines
//! (`super::native::line`), and why a build was refused.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::table::Why;

/// Why a build was not done on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The title's list records nowhere (no host device or list).
    NoList,
    /// The host device has no ray tracing (or not the tier a format needs).
    NoDevice,
    /// A copy list (structures are built on direct and compute lists).
    CopyList,
    /// The geometry descriptions could not all be read.
    NotWhole,
    VertexFormat,
    IndexFormat,
    /// An address or stride off what Direct3D asks.
    Unaligned,
    /// No host copy holds the data.
    NoCopy,
    /// More than one host copy holding the data was written by shaders.
    SeveralWritten,
    OtherGeometry,
    /// The host's driver sizes no structure for the inputs.
    Prebuild,
    /// The memory ceiling, or no memory for another chunk.
    Memory,
    /// Instances laid out other than in an array.
    InstanceLayout,
    /// A copy whose source the host has no structure for.
    CopySource,
    /// A copy mode other than a clone or a compaction.
    CopyMode,
    /// Packed data to expand (`VICTORY_DXR_EXPAND`) the CPU cannot read.
    PackedUnreadable,
    /// Packed data in a host copy shaders write: what the CPU sees may be
    /// old there (a compute pass would expand it; not made).
    PackedWritten,
    /// Packed data off the layout the builder's stages read.
    PackedLayout,
}

impl Refusal {
    pub(super) const ALL: [Refusal; 18] = [
        Refusal::NoList,
        Refusal::NoDevice,
        Refusal::CopyList,
        Refusal::NotWhole,
        Refusal::VertexFormat,
        Refusal::IndexFormat,
        Refusal::Unaligned,
        Refusal::NoCopy,
        Refusal::SeveralWritten,
        Refusal::OtherGeometry,
        Refusal::Prebuild,
        Refusal::Memory,
        Refusal::InstanceLayout,
        Refusal::CopySource,
        Refusal::CopyMode,
        Refusal::PackedUnreadable,
        Refusal::PackedWritten,
        Refusal::PackedLayout,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Refusal::NoList => "the list records nowhere",
            Refusal::NoDevice => "no host ray tracing",
            Refusal::CopyList => "a copy list",
            Refusal::NotWhole => "descriptions not all readable",
            Refusal::VertexFormat => "a vertex format the host does not build from",
            Refusal::IndexFormat => "an index format the host does not build from",
            Refusal::Unaligned => "data off its alignment",
            Refusal::NoCopy => "data in no host copy",
            Refusal::SeveralWritten => "data in several host copies shaders write",
            Refusal::OtherGeometry => "a geometry of another kind",
            Refusal::Prebuild => "the host sizes no structure",
            Refusal::Memory => "no card memory (VICTORY_DXR_NATIVE_MB)",
            Refusal::InstanceLayout => "instances as an array of pointers",
            Refusal::CopySource => "a copy of a structure the host has not",
            Refusal::CopyMode => "a copy mode other than clone or compact",
            Refusal::PackedUnreadable => "packed data the CPU cannot read",
            Refusal::PackedWritten => "packed data in a host copy shaders write (a compute pass would expand it)",
            Refusal::PackedLayout => "packed data off the layout the builder's stages read",
        }
    }
}

/// What is counted between two lines.
#[derive(Clone, Copy)]
pub(super) enum C {
    AskedBottom,
    AskedBottomUpdates,
    AskedTop,
    AskedTopUpdates,
    AskedUnpacked,
    AskedCopies,
    Built,
    Refit,
    RefitInPlace,
    Rebuilt,
    Triangles,
    RefitTriangles,
    Boxes,
    Copies,
    TopBuilt,
    Instances,
    /// Instances naming a bottom level the host has not got (made inactive).
    InstancesUnknown,
    /// Of them, in memory the CPU could not read at submission.
    InstancesUnreadable,
    /// Instances whose flags held the console's own bits (taken off).
    InstancesConsoleFlags,
    /// Builds whose flags held bits the PC does not know (taken off).
    BuildsConsoleFlags,
    /// Data read from the copy shaders wrote where a draw would read another.
    ShadersCopy,
    /// Data outside title buffers with a page in an arena of by-address
    /// memory (what shaders stored there by address the copy read lacks).
    ByAddress,
    CardNanos,
    Timed,
    /// Expansions (`VICTORY_DXR_EXPAND`, `super::expand`) of the builds
    /// recorded: index-53 lists and their triangles, quantized position
    /// lists and their vertices, the upload memory they take, and those
    /// reading pages of by-address memory the card holds.
    ExpandedIndexLists,
    ExpandedTriangles,
    ExpandedPositionLists,
    ExpandedVertices,
    ExpandedBytes,
    ExpandedByAddress,
    /// At submission: expansions whose data the CPU could not read then
    /// (written so that nothing is hit), triangles made degenerate,
    /// positions not finite.
    ExpandedUnreadable,
    ExpandedDegenerate,
    ExpandedNotFinite,
    /// Barriers recorded for builds to take the scratch memory of builds
    /// before them again (`super::scratch`).
    ScratchWaits,
    /// `super::keep`: destinations top levels named that the host had a
    /// recipe but no structure for; bottom levels made again, and those
    /// refused; bottom levels let go for want of memory.
    Wanted,
    Remade,
    RemadeRefused,
    Evicted,
    Swept,
}

pub(super) const COUNTS: usize = C::Swept as usize + 1;
pub(super) static COUNT: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];
pub(super) static REFUSED: [AtomicU64; Refusal::ALL.len()] = [const { AtomicU64::new(0) }; Refusal::ALL.len()];
pub(super) static REBUILT: [AtomicU64; Why::ALL.len()] = [const { AtomicU64::new(0) }; Why::ALL.len()];
pub(super) static FRAMES: AtomicU64 = AtomicU64::new(0);

pub(super) fn add(c: C, n: u64) {
    COUNT[c as usize].fetch_add(n, Relaxed);
}

/// Refusals said in full, of each reason.
const SAID: u64 = 4;

pub(super) fn refuse(r: Refusal, what: impl FnOnce() -> String) {
    let i = Refusal::ALL.iter().position(|&x| x == r).unwrap_or(0);
    REFUSED[i].fetch_add(1, Relaxed);
    if SAID_ALL[i].fetch_add(1, Relaxed) < SAID {
        tracing::info!("host ray tracing: refused ({}): {}", r.name(), what());
    }
}

/// Refusals said in full so far, by reason (the counts above are taken by
/// every line).
static SAID_ALL: [AtomicU64; Refusal::ALL.len()] = [const { AtomicU64::new(0) }; Refusal::ALL.len()];
