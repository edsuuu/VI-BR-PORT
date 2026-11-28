//! The descriptions the title hands its structure builder, read from bytes.
//!
//! Everything Direct3D 12 defines on every platform is laid out as the
//! public headers say (`d3d12.h`: the build inputs and description, the
//! geometry descriptions, the instance description, the prebuild sizes and
//! the postbuild requests). The console's own, which are not public, are
//! read as the builder itself reads them (its decompiled code; addresses
//! are those of the retail `XboxBvhBuilder.dll`, image base 0x180000000):
//!
//! - `D3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2`, read
//!   here as [`Batched`]: a build description, a count and an address of
//!   postbuild requests, and the size and address of a list of extension
//!   records ([`Stream`], [`Record`]), 80 bytes in all;
//! - `D3D12XBOX_XDXR_STREAM_DESC`, which the prebuild call takes beside the
//!   inputs: its first 16 bytes are the same size and address of extension
//!   records (the prebuild copies them to +0x40 of an entry it makes up,
//!   `GetRaytracingAccelerationStructurePrebuildInfo` at 0x180021880);
//! - the header of a structure's shipped ("dehydrated") data, which the
//!   rehydrate reads on the CPU ([`Dehydrated`], with the frame its nodes
//!   are in, [`Placement`]).
//!
//! The readers take memory through [`Memory`], so the tests can hand them
//! bytes of their own and a run hands them the title's (`super::Process`).

use std::fmt::Write as _;

/// Where the bytes at a title address come from.
pub trait Memory {
    /// `len` bytes at `at`, or nothing if they cannot all be read.
    fn bytes(&self, at: u64, len: usize) -> Option<Vec<u8>>;
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or_default())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_bits(u32_at(b, at))
}

// D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE.
pub const TOP_LEVEL: u32 = 0;
pub const BOTTOM_LEVEL: u32 = 1;

// D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS.
pub const ALLOW_UPDATE: u32 = 0x1;
pub const ALLOW_COMPACTION: u32 = 0x2;
pub const PREFER_FAST_TRACE: u32 = 0x4;
pub const PREFER_FAST_BUILD: u32 = 0x8;
pub const MINIMIZE_MEMORY: u32 = 0x10;
pub const PERFORM_UPDATE: u32 = 0x20;
/// The console's own: build a structure to be shipped and unpacked later.
pub const DEHYDRATE: u32 = 0x4000_0000;

const BUILD_FLAGS: [(u32, &str); 7] = [
    (ALLOW_UPDATE, "ALLOW_UPDATE"),
    (ALLOW_COMPACTION, "ALLOW_COMPACTION"),
    (PREFER_FAST_TRACE, "PREFER_FAST_TRACE"),
    (PREFER_FAST_BUILD, "PREFER_FAST_BUILD"),
    (MINIMIZE_MEMORY, "MINIMIZE_MEMORY"),
    (PERFORM_UPDATE, "PERFORM_UPDATE"),
    (DEHYDRATE, "DEHYDRATE"),
];

// D3D12_ELEMENTS_LAYOUT.
pub const ARRAY: u32 = 0;
pub const ARRAY_OF_POINTERS: u32 = 1;

// D3D12_RAYTRACING_GEOMETRY_TYPE and _FLAGS.
const TRIANGLES: u32 = 0;
const AABBS: u32 = 1;
const OPAQUE: u32 = 0x1;
const GEOMETRY_FLAGS: [(u32, &str); 2] = [(OPAQUE, "OPAQUE"), (0x2, "NO_DUPLICATE_ANYHIT")];

// D3D12_RAYTRACING_INSTANCE_FLAGS.
const INSTANCE_FLAGS: [(u32, &str); 4] = [
    (0x1, "TRIANGLE_CULL_DISABLE"),
    (0x2, "TRIANGLE_FRONT_COUNTERCLOCKWISE"),
    (0x4, "FORCE_OPAQUE"),
    (0x8, "FORCE_NON_OPAQUE"),
];

/// The names of the bits of `flags` that `names` knows, then what is left
/// in hex; `NONE` for none.
fn bits(flags: u32, names: &[(u32, &str)]) -> String {
    let mut text = String::new();
    let mut left = flags;
    for &(bit, name) in names {
        if flags & bit != 0 {
            if !text.is_empty() {
                text.push('|');
            }
            text.push_str(name);
            left &= !bit;
        }
    }
    if left != 0 {
        if !text.is_empty() {
            text.push('|');
        }
        let _ = write!(text, "{left:#x}");
    }
    if text.is_empty() { "NONE".into() } else { text }
}

/// Build flags by name.
pub fn build_flags(flags: u32) -> String {
    bits(flags, &BUILD_FLAGS)
}

/// The DXGI formats a structure build can be given, by name; others by
/// number.
pub fn format(f: u32) -> String {
    let name = match f {
        0 => "UNKNOWN",
        2 => "R32G32B32A32_FLOAT",
        6 => "R32G32B32_FLOAT",
        10 => "R16G16B16A16_FLOAT",
        13 => "R16G16B16A16_SNORM",
        16 => "R32G32_FLOAT",
        24 => "R10G10B10A2_UNORM",
        28 => "R8G8B8A8_UNORM",
        31 => "R8G8B8A8_SNORM",
        34 => "R16G16_FLOAT",
        37 => "R16G16_SNORM",
        42 => "R32_UINT",
        49 => "R8G8_UNORM",
        51 => "R8G8_SNORM",
        57 => "R16_UINT",
        _ => return format!("format {f}"),
    };
    name.into()
}

/// `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS` (24 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    pub kind: u32,
    pub flags: u32,
    /// Instances (top level) or geometries (bottom level).
    pub count: u32,
    pub layout: u32,
    /// Top level: the GPU address of the instances. Bottom level: the CPU
    /// address of the geometry descriptions (or of pointers to them).
    pub descs: u64,
}

pub const INPUTS: usize = 24;

impl Inputs {
    pub fn parse(b: &[u8]) -> Inputs {
        Inputs {
            kind: u32_at(b, 0),
            flags: u32_at(b, 4),
            count: u32_at(b, 8),
            layout: u32_at(b, 12),
            descs: u64_at(b, 16),
        }
    }

    pub fn top(&self) -> bool {
        self.kind == TOP_LEVEL
    }

    pub fn update(&self) -> bool {
        self.flags & PERFORM_UPDATE != 0
    }

    pub fn level(&self) -> &'static str {
        match self.kind {
            TOP_LEVEL => "top",
            BOTTOM_LEVEL => "bottom",
            _ => "level?",
        }
    }
}

/// `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC` (48 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildDesc {
    pub dest: u64,
    pub inputs: Inputs,
    /// The structure an update starts from (0 for a build).
    pub source: u64,
    pub scratch: u64,
}

pub const BUILD_DESC: usize = 48;

impl BuildDesc {
    pub fn parse(b: &[u8]) -> BuildDesc {
        BuildDesc {
            dest: u64_at(b, 0),
            inputs: Inputs::parse(&b[8..8 + INPUTS]),
            source: u64_at(b, 32),
            scratch: u64_at(b, 40),
        }
    }
}

/// One entry of a batched build, as the builder reads it: entries are 80
/// bytes apart (0x18000c750); the postbuild requests at +0x30/+0x38 are
/// walked in `BuildRaytracingAccelerationStructureBatched2X` (0x1800216a0),
/// the extension records at +0x40/+0x48 at 0x18000c780, 0x18000c820 and
/// 0x18000cc00.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Batched {
    pub build: BuildDesc,
    pub postbuild_count: u32,
    /// The dword after the count: padding (nothing in the builder reads it).
    pub word: u32,
    pub postbuild: u64,
    pub extensions: Stream,
}

pub const BATCHED: usize = 80;

impl Batched {
    pub fn parse(b: &[u8]) -> Batched {
        Batched {
            build: BuildDesc::parse(&b[..BUILD_DESC]),
            postbuild_count: u32_at(b, 48),
            word: u32_at(b, 52),
            postbuild: u64_at(b, 56),
            extensions: Stream::parse(&b[64..80]),
        }
    }
}

/// Where a list of extension records is: its size in bytes, then its
/// address (+0x40 and +0x48 of a batched entry; the first 16 bytes of the
/// prebuild's stream description).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    pub bytes: u64,
    pub at: u64,
}

pub const STREAM: usize = 16;

impl Stream {
    pub fn parse(b: &[u8]) -> Stream {
        Stream {
            bytes: u64_at(b, 0),
            at: u64_at(b, 8),
        }
    }

    /// No list: the builder reads records only when both are set.
    pub fn empty(&self) -> bool {
        self.bytes == 0 || self.at == 0
    }
}

/// One extension record: 16 bytes, the kind in the first dword. Every walk
/// of the list in the builder skips the kinds it does not want among 0, 1
/// and 2, takes the first record of the kind it wants, and stops at a
/// record of any other kind. What each field does there (the builder's own
/// switches, globals that nothing in its code writes, at zero). The kinds
/// are the GDK's three build parameter records (`..._PARAMS1/2/3`), whose
/// fields it names; the names below say in our words what it gives them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Record {
    /// Kind 0 (`PARAMS1`), thresholds. `box16_inflation` (+8, a float): the
    /// error a box may grow by when stored in 16-bit floats; below 0 the
    /// build flags choose it (0.0 with PREFER_FAST_TRACE and not
    /// MINIMIZE_MEMORY, 1.01 the other way round, 1.005 otherwise); above
    /// 10.0 on a bottom level with MINIMIZE_MEMORY it turns on a build mode
    /// bit (bit 9 of the build's flags word; the prebuild sizes with it).
    /// Prebuild 0x180011b80, batched 0x18000cc00, which also keeps
    /// `quad_join` (+4, a float: how much a box may grow for two adjacent
    /// triangles to be paired; -1.0 without the record) and `treelet`
    /// (+0xc..+0xf: the tree optimisation's first level, its levels, its
    /// step between rounds and its limit of rounds) in the build's record
    /// for the stages.
    Thresholds {
        quad_join: f32,
        box16_inflation: f32,
        treelet: [u8; 4],
    },
    /// Kind 1 (`PARAMS2`), sizes. `instances` (+8, the instances the
    /// builder allocates for): a top level without ALLOW_UPDATE and with 2
    /// or more instances is built for this many instead of `NumDescs`, if
    /// it is not 0 (0x18000c780). `bytes[1]` (+0xd): the k-DOP tessellation
    /// factor N of a bottom level (`level` here), at most 15, which adds
    /// N²·0x90 bytes to the result, the map of 2·N² apex points of 12 bytes
    /// on each of the six cube faces (the same as a shipped header's
    /// `(h >> 20 & 0xf)²·0x90`; prebuild 0x180011b80, batched 0x18000cc00,
    /// which keeps N in bits 10..13 of the build's flags word); 0 leaves
    /// the builder's default (no k-DOP). `bytes[2]` (+0xe): not 0 on a top
    /// level turns off an experimental top-level optimisation, clearing bit
    /// 0x40 of that word (0x18000cc00). `bytes[0]` (+0xc, the offline
    /// builder's threads) and `bytes[3]` (+0xf, an automatic rotation of
    /// bottom levels) are not read by this builder.
    Sizes { instances: u32, bytes: [u8; 4] },
    /// Kind 2 (`PARAMS3`), the top level's instance splitting. `extra`
    /// (+4): if not 0xffffffff, the extra instances made by splitting on a
    /// top level without ALLOW_UPDATE and with 2 or more instances; without
    /// it 35 % of the instances with PREFER_FAST_TRACE and not
    /// MINIMIZE_MEMORY, else none (0x18000c820; rebraid in GPURT's words).
    /// `split16` (+8) and `isolate` (+0xc), floats: the fraction split into
    /// 16 and a surface-area ratio for isolating, which the batched build
    /// keeps in the build's record (0x18000cc00; 0.2 and 0.75 without the
    /// record, plausibly the builder's own values for the GDK's "internal
    /// default" -1 and 0 [inf]). The prebuild skips this kind.
    Split { extra: u32, split16: f32, isolate: f32 },
    /// Any other kind: the builder stops reading the list here.
    Other { kind: u32, words: [u32; 3] },
}

pub const RECORD: usize = 16;
/// Records read at most from one list.
pub const RECORDS_READ: usize = 64;

impl Record {
    pub fn parse(b: &[u8]) -> Record {
        let word = |at| u32_at(b, at);
        let bytes = [b[12], b[13], b[14], b[15]];
        match word(0) {
            0 => Record::Thresholds {
                quad_join: f32_at(b, 4),
                box16_inflation: f32_at(b, 8),
                treelet: bytes,
            },
            1 => Record::Sizes {
                instances: word(8),
                bytes,
            },
            2 => Record::Split {
                extra: word(4),
                split16: f32_at(b, 8),
                isolate: f32_at(b, 12),
            },
            kind => Record::Other {
                kind,
                words: [word(4), word(8), word(12)],
            },
        }
    }

    pub fn kind(&self) -> u32 {
        match *self {
            Record::Thresholds { .. } => 0,
            Record::Sizes { .. } => 1,
            Record::Split { .. } => 2,
            Record::Other { kind, .. } => kind,
        }
    }

    pub fn describe(&self) -> String {
        match *self {
            Record::Thresholds {
                quad_join,
                box16_inflation,
                treelet,
            } => format!(
                "kind 0 box16 inflation {box16_inflation} quad join {quad_join} (treelet {:02x} {:02x} {:02x} {:02x})",
                treelet[0], treelet[1], treelet[2], treelet[3]
            ),
            Record::Sizes { instances, bytes } => format!(
                "kind 1 instances {instances} k-DOP N {} (+c {:02x} +e {:02x} +f {:02x})",
                bytes[1], bytes[0], bytes[2], bytes[3]
            ),
            Record::Split {
                extra,
                split16,
                isolate,
            } => format!(
                "kind 2 extra instances {} (split16 {split16}, isolate {isolate})",
                if extra == u32::MAX {
                    "none".to_string()
                } else {
                    extra.to_string()
                },
            ),
            Record::Other { kind, words } => format!(
                "KIND {kind} (+4 {:#x} +8 {:#x} +c {:#x}): the builder stops reading here",
                words[0], words[1], words[2]
            ),
        }
    }
}

/// The records of a list as the builder walks them: up to the first of a
/// kind it does not know (kept, last), at most [`RECORDS_READ`]. A size
/// that is not a multiple of 16 still has its last part record read (the
/// walks go while a record starts before the end). `None`: not readable.
pub fn records(memory: &dyn Memory, stream: &Stream) -> Option<Vec<Record>> {
    if stream.empty() {
        return Some(Vec::new());
    }
    let n = (stream.bytes.div_ceil(RECORD as u64) as usize).min(RECORDS_READ);
    let b = memory.bytes(stream.at, n * RECORD)?;
    let mut out = Vec::with_capacity(n);
    for one in b.chunks_exact(RECORD) {
        let r = Record::parse(one);
        out.push(r);
        if r.kind() > 2 {
            break;
        }
    }
    Some(out)
}

/// The first record of `kind` in a walk that passes over the other two
/// kinds the builder knows and stops at any other.
fn first(records: &[Record], kind: u32) -> Option<&Record> {
    records
        .iter()
        .take_while(|r| r.kind() <= 2)
        .find(|r| r.kind() == kind)
}

/// What the builder makes of a build's inputs and its extension records:
/// the sizes and modes it builds with (its own switches at zero).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Taken {
    /// Top level: the instances it builds for, `NumDescs` or kind 1's.
    pub instances: Option<u32>,
    /// Top level: the extra instances for splitting.
    pub extra: Option<u32>,
    /// Bottom level: the k-DOP tessellation factor N (`None`: the
    /// builder's default, which nothing in its code sets).
    pub level: Option<u32>,
    /// The 16-bit box inflation threshold.
    pub box16_inflation: f32,
    /// Bottom level: the build mode bit kind 0 turns on.
    pub mode: bool,
}

/// The 16-bit box inflation threshold a build without a kind-0 record (or
/// with one below 0) gets from its flags (0x180011b80, 0x18000cc00).
fn default_box16_inflation(flags: u32) -> f32 {
    match (flags & PREFER_FAST_TRACE != 0, flags & MINIMIZE_MEMORY != 0) {
        (true, false) => 0.0,
        (false, true) => 1.01,
        _ => 1.005,
    }
}

pub fn taken(inputs: &Inputs, records: &[Record]) -> Taken {
    let fixed = inputs.flags & ALLOW_UPDATE == 0 && inputs.count >= 2;
    let (mut instances, mut extra, mut level) = (None, None, None);
    if inputs.top() {
        // 0x18000c780: the walk ends at the first kind-1 record.
        instances = Some(match first(records, 1) {
            Some(&Record::Sizes { instances: n, .. }) if n != 0 && fixed => n,
            _ => inputs.count,
        });
        // 0x18000c820.
        extra = Some(if !fixed {
            0
        } else {
            match first(records, 2) {
                Some(&Record::Split { extra: n, .. }) if n != u32::MAX => n,
                _ if inputs.flags & (PREFER_FAST_TRACE | MINIMIZE_MEMORY) == PREFER_FAST_TRACE => {
                    (inputs.count as f32 * 0.35) as u32
                }
                _ => 0,
            }
        });
    } else if let Some(&Record::Sizes { bytes, .. }) = first(records, 1)
        && bytes[1] != 0
    {
        level = Some(u32::from(bytes[1]).min(15));
    }
    // Only a value below 0 falls back (a NaN is kept, as the builder's
    // comparison has it).
    let box16_inflation = match first(records, 0) {
        Some(&Record::Thresholds { box16_inflation, .. })
            if box16_inflation.partial_cmp(&0.0) != Some(std::cmp::Ordering::Less) =>
        {
            box16_inflation
        }
        _ => default_box16_inflation(inputs.flags),
    };
    Taken {
        instances,
        extra,
        level,
        box16_inflation,
        mode: !inputs.top() && inputs.flags & MINIMIZE_MEMORY != 0 && box16_inflation > 10.0,
    }
}

impl Taken {
    pub fn describe(&self) -> String {
        let mut text = format!("box16 inflation {}", self.box16_inflation);
        if let Some(n) = self.instances {
            let _ = write!(text, ", {n} instances");
        }
        if let Some(n) = self.extra {
            let _ = write!(text, " + {n} extra for splitting");
        }
        match self.level {
            Some(n) => {
                let _ = write!(text, ", k-DOP N {n} (+{} bytes)", n * n * 0x90);
            }
            None if self.instances.is_none() => text.push_str(", the default k-DOP"),
            None => {}
        }
        if self.mode {
            text.push_str(", MODE BIT ON");
        }
        text
    }
}

/// The extension records of a list in one line of the trace: each record,
/// what the builder takes from them for these inputs, and what does not fit
/// the layout (a size not a multiple of 16, a kind it does not know, a list
/// it cannot read).
pub fn extensions(memory: &dyn Memory, stream: &Stream, inputs: &Inputs) -> String {
    describe_extensions(stream, records(memory, stream).as_deref(), inputs)
}

/// [`extensions`] with the records already read ([`records`]).
pub fn describe_extensions(stream: &Stream, records: Option<&[Record]>, inputs: &Inputs) -> String {
    if stream.empty() {
        return if stream.bytes == 0 && stream.at == 0 {
            "no extension records".to_string()
        } else {
            format!(
                "NO LIST: extension bytes {} at {:#x}",
                stream.bytes, stream.at
            )
        };
    }
    let mut text = format!(
        "{} bytes of extension records at {:#x}",
        stream.bytes, stream.at
    );
    if !stream.bytes.is_multiple_of(RECORD as u64) {
        text.push_str(" (SIZE NOT A MULTIPLE OF 16)");
    }
    match records {
        Some(r) => {
            for one in r {
                let _ = write!(text, "; {}", one.describe());
            }
            let _ = write!(text, "; the builder takes {}", taken(inputs, r).describe());
        }
        None => text.push_str(": NOT READABLE"),
    }
    text
}

/// The first 64 bytes of a structure's shipped ("dehydrated") data, as the
/// rehydrate reads them on the CPU (`FUN_180014700`, 0x180014700): at
/// `p[i]` of `RehydrateRaytracingAccelerationStructureBatched2X` (the
/// fourth argument, one address a structure), or at the description's
/// `SourceAccelerationStructureData` when `p` is null. The rehydrate first
/// copies a header of this size from the description's source to its
/// destination on the card, so (if `p[i]` holds what the source does
/// [inf]) its +0x30 and +0x3c are the words a ray program reads at +48
/// (the root node) and +60 of the structure.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dehydrated {
    /// +0: [`DEHYDRATED_BIT`] set, or the whole call answers
    /// `E_INVALIDARG`; bits 20..23 the k-DOP tessellation factor N (the
    /// "level n" of [`Record::Sizes`]).
    pub first: u32,
    /// +0xc: what the leaf stage is dispatched over, 2048 a group (its
    /// leaves [inf]).
    pub leaves: u32,
    /// +0x10..+0x27: the structure's box, its lower corner then its upper
    /// (what the centring of [`Placement`] reads).
    pub low: [f32; 3],
    pub high: [f32; 3],
    /// +0x2c: if not 0, the header is `size` (+0x38) bytes long; if 0,
    /// n²·0x90 + 0x40.
    pub sized: u32,
    pub size: u32,
    /// +0x30: the root node pointer, its kind in the low 3 bits.
    pub root: u32,
    /// +0x3c.
    pub placement: Placement,
}

pub const DEHYDRATED: usize = 64;
/// The bit of the first dword the rehydrate wants set.
pub const DEHYDRATED_BIT: u32 = 1 << 24;

impl Dehydrated {
    pub fn parse(b: &[u8]) -> Dehydrated {
        let f = |at| f32_at(b, at);
        Dehydrated {
            first: u32_at(b, 0),
            leaves: u32_at(b, 0xc),
            low: [f(0x10), f(0x14), f(0x18)],
            high: [f(0x1c), f(0x20), f(0x24)],
            sized: u32_at(b, 0x2c),
            size: u32_at(b, 0x38),
            root: u32_at(b, 0x30),
            placement: Placement(u32_at(b, 0x3c)),
        }
    }

    pub fn valid(&self) -> bool {
        self.first & DEHYDRATED_BIT != 0
    }

    pub fn level(&self) -> u32 {
        self.first >> 20 & 0xf
    }

    /// The header's bytes, as the rehydrate sizes its copy.
    pub fn header(&self) -> u32 {
        if self.sized == 0 {
            self.level() * self.level() * 0x90 + 0x40
        } else {
            self.size
        }
    }

    /// What the centring of bit 30 moves each axis by: the middle of the
    /// box on an axis it does not hold 0 on, else nothing (0x180014700).
    pub fn centre(&self) -> [f32; 3] {
        std::array::from_fn(|k| {
            let (low, high) = (self.low[k], self.high[k]);
            if 0.0 < low || high < 0.0 {
                (low + high) * 0.5
            } else {
                0.0
            }
        })
    }

    pub fn describe(&self) -> String {
        let (l, h) = (self.low, self.high);
        let mut text = format!(
            "first dword {:#010x} ({}, level {}), header {} bytes, {} leaves, root {:#x} ({}), box ({} {} {}) - ({} {} {}), +60 {}",
            self.first,
            if self.valid() {
                "bit 24 set"
            } else {
                "BIT 24 CLEAR: the builder answers E_INVALIDARG"
            },
            self.level(),
            self.header(),
            self.leaves,
            self.root,
            node_kind(self.root),
            l[0],
            l[1],
            l[2],
            h[0],
            h[1],
            h[2],
            self.placement.describe()
        );
        if self.placement.centred() {
            let c = self.centre();
            let _ = write!(text, " (moved by {} {} {})", c[0], c[1], c[2]);
        }
        text
    }
}

/// The kind of node a node pointer names (its low 3 bits).
pub fn node_kind(pointer: u32) -> &'static str {
    match pointer & 7 {
        0..=3 => "triangle",
        4 => "box16",
        5 => "box32",
        6 => "instance",
        _ => "procedural",
    }
}

/// The word at +0x3c (+60) of a bottom level's header: the frame its nodes
/// are stored in, which the rehydrate hands to the leaf stage
/// (0x180014700). [`NO_PLACEMENT`]: none, the nodes are in the geometry's
/// own frame. Any other word: bits 24..29 index a table of 64 rotations
/// (0x18000be20), two bits an axis (x lowest, then y, then z), each a turn
/// about that axis by 0, 22.5, 45 or 67.5 degrees, made one after another
/// x, y, z; they are not permutations of the axes (90 degrees is not
/// among them), and index 0, which the builder skips, is the identity;
/// bit 30 centres each axis
/// on the box's middle where the box does not hold 0
/// ([`Dehydrated::centre`]); bit 31 scales the axes by the powers of two
/// whose float exponents are its bytes 0, 1 and 2 (x, y, z; 127 is 1.0),
/// and without it they are not scaled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Placement(pub u32);

pub const NO_PLACEMENT: u32 = 0x7f_7f7f;

impl Placement {
    pub fn none(self) -> bool {
        self.0 == NO_PLACEMENT
    }

    /// The rotation's index (0 with no placement).
    pub fn rotation(self) -> u32 {
        if self.none() { 0 } else { self.0 >> 24 & 0x3f }
    }

    pub fn centred(self) -> bool {
        !self.none() && self.0 & 1 << 30 != 0
    }

    /// The powers of two the axes are scaled by (x, y, z), with bit 31.
    pub fn exponents(self) -> Option<[i32; 3]> {
        (!self.none() && self.0 & 1 << 31 != 0)
            .then(|| [0, 8, 16].map(|s| (self.0 >> s & 0xff) as i32 - 127))
    }

    pub fn scaled(self) -> bool {
        self.exponents().is_some_and(|e| e != [0; 3])
    }

    /// The nodes are in the geometry's own frame: no placement, or one that
    /// changes nothing.
    pub fn identity(self) -> bool {
        self.rotation() == 0 && !self.centred() && !self.scaled()
    }

    pub fn describe(self) -> String {
        if self.none() {
            return "0x7f7f7f (none)".to_string();
        }
        let mut text = format!("{:#010x} (rotation {}", self.0, self.rotation());
        if let Some(e) = self.exponents() {
            let _ = write!(text, ", scale 2^{} 2^{} 2^{}", e[0], e[1], e[2]);
        }
        if self.centred() {
            text.push_str(", centred");
        }
        text.push_str(if self.identity() {
            ": in effect none)"
        } else {
            ")"
        });
        text
    }
}

/// `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC` (16 bytes):
/// where the builder stores a fact about the built structure, and which.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Postbuild {
    pub dest: u64,
    pub kind: u32,
}

pub const POSTBUILD: usize = 16;

impl Postbuild {
    pub fn parse(b: &[u8]) -> Postbuild {
        Postbuild {
            dest: u64_at(b, 0),
            kind: u32_at(b, 8),
        }
    }

    pub fn what(&self) -> &'static str {
        match self.kind {
            0 => "compacted size",
            1 => "tools visualization",
            2 => "serialization",
            3 => "current size",
            _ => "info?",
        }
    }
}

/// `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO` (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrebuildInfo {
    pub result: u64,
    pub scratch: u64,
    pub update_scratch: u64,
}

const _: () = assert!(size_of::<PrebuildInfo>() == 24);

/// `D3D12_RAYTRACING_GEOMETRY_DESC` (56 bytes): triangles or boxes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Geometry {
    Triangles {
        flags: u32,
        /// GPU address of a 3x4 row-major matrix, or 0.
        transform: u64,
        index_format: u32,
        vertex_format: u32,
        index_count: u32,
        vertex_count: u32,
        index_buffer: u64,
        vertex_buffer: u64,
        vertex_stride: u64,
    },
    Aabbs {
        flags: u32,
        count: u64,
        start: u64,
        stride: u64,
    },
    /// A kind this reader does not know (opacity micromaps and the like).
    Other { kind: u32, flags: u32 },
}

pub const GEOMETRY: usize = 56;

impl Geometry {
    pub fn parse(b: &[u8]) -> Geometry {
        let (kind, flags) = (u32_at(b, 0), u32_at(b, 4));
        match kind {
            TRIANGLES => Geometry::Triangles {
                flags,
                transform: u64_at(b, 8),
                index_format: u32_at(b, 16),
                vertex_format: u32_at(b, 20),
                index_count: u32_at(b, 24),
                vertex_count: u32_at(b, 28),
                index_buffer: u64_at(b, 32),
                vertex_buffer: u64_at(b, 40),
                vertex_stride: u64_at(b, 48),
            },
            AABBS => Geometry::Aabbs {
                flags,
                count: u64_at(b, 8),
                start: u64_at(b, 16),
                stride: u64_at(b, 24),
            },
            _ => Geometry::Other { kind, flags },
        }
    }

    /// Triangles it holds (0 for boxes).
    pub fn triangles(&self) -> u64 {
        match *self {
            Geometry::Triangles {
                index_format,
                index_count,
                vertex_count,
                ..
            } => {
                u64::from(if index_format == 0 {
                    vertex_count
                } else {
                    index_count
                }) / 3
            }
            _ => 0,
        }
    }

    /// The title memory a build reads for it: (what, address, bytes) of the
    /// vertices, indices, transform or boxes it names.
    pub fn spans(&self) -> Vec<(&'static str, u64, u64)> {
        let mut out = Vec::new();
        match *self {
            Geometry::Triangles {
                transform,
                index_format,
                index_count,
                vertex_count,
                index_buffer,
                vertex_buffer,
                vertex_stride,
                ..
            } => {
                if vertex_buffer != 0 {
                    out.push((
                        "vertices",
                        vertex_buffer,
                        vertex_stride * u64::from(vertex_count),
                    ));
                }
                let index_size = match index_format {
                    57 => 2,
                    42 => 4,
                    _ => 0,
                };
                if index_buffer != 0 && index_size != 0 {
                    out.push(("indices", index_buffer, index_size * u64::from(index_count)));
                }
                if transform != 0 {
                    out.push(("transform", transform, 48));
                }
            }
            Geometry::Aabbs {
                count,
                start,
                stride,
                ..
            } => {
                if start != 0 {
                    out.push(("boxes", start, stride * count));
                }
            }
            Geometry::Other { .. } => {}
        }
        out
    }

    /// Its `D3D12_RAYTRACING_GEOMETRY_FLAGS`.
    pub fn flags(&self) -> u32 {
        match *self {
            Geometry::Triangles { flags, .. }
            | Geometry::Aabbs { flags, .. }
            | Geometry::Other { flags, .. } => flags,
        }
    }

    /// Whether it is a list of triangles marked opaque.
    pub fn opaque_triangles(&self) -> bool {
        matches!(*self, Geometry::Triangles { flags, .. } if flags & OPAQUE != 0)
    }

    /// Boxes it holds (0 for triangles).
    pub fn aabbs(&self) -> u64 {
        match *self {
            Geometry::Aabbs { count, .. } => count,
            _ => 0,
        }
    }

    /// One line of the trace.
    pub fn describe(&self) -> String {
        match *self {
            Geometry::Triangles {
                flags,
                transform,
                index_format,
                vertex_format,
                index_count,
                vertex_count,
                index_buffer,
                vertex_buffer,
                vertex_stride,
            } => {
                let index = if index_format == 0 && index_buffer == 0 {
                    "no index".to_string()
                } else {
                    format!(
                        "index {} x{index_count} at {index_buffer:#x}",
                        format(index_format)
                    )
                };
                let transform = if transform == 0 {
                    "no transform".to_string()
                } else {
                    format!("transform at {transform:#x}")
                };
                format!(
                    "triangles {} vertex {} stride {vertex_stride} x{vertex_count} at {vertex_buffer:#x}; {index}; {transform}",
                    bits(flags, &GEOMETRY_FLAGS),
                    format(vertex_format),
                )
            }
            Geometry::Aabbs {
                flags,
                count,
                start,
                stride,
            } => format!(
                "boxes {} x{count} stride {stride} at {start:#x}",
                bits(flags, &GEOMETRY_FLAGS)
            ),
            Geometry::Other { kind, flags } => format!("geometry of type {kind} flags {flags:#x}"),
        }
    }
}

/// `D3D12_RAYTRACING_INSTANCE_DESC` (64 bytes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    pub transform: [f32; 12],
    pub id: u32,
    pub mask: u8,
    pub contribution: u32,
    pub flags: u8,
    /// The GPU address of the bottom-level structure it places.
    pub structure: u64,
}

pub const INSTANCE: usize = 64;

impl Instance {
    pub fn parse(b: &[u8]) -> Instance {
        let mut transform = [0.0; 12];
        for (i, t) in transform.iter_mut().enumerate() {
            *t = f32_at(b, i * 4);
        }
        let (a, c) = (u32_at(b, 48), u32_at(b, 52));
        Instance {
            transform,
            id: a & 0xff_ffff,
            mask: (a >> 24) as u8,
            contribution: c & 0xff_ffff,
            flags: (c >> 24) as u8,
            structure: u64_at(b, 56),
        }
    }

    pub fn describe(&self) -> String {
        let t = &self.transform;
        format!(
            "id {} mask {:#04x} hit group +{} {} structure {:#x}; position ({:.1} {:.1} {:.1})",
            self.id,
            self.mask,
            self.contribution,
            bits(u32::from(self.flags), &INSTANCE_FLAGS),
            self.structure,
            t[3],
            t[7],
            t[11]
        )
    }
}

/// DXGI format 53 (`R16_TYPELESS`): an index format the builder takes
/// beside `R16_UINT` and `R32_UINT`, as a third mode of its leaf stage
/// (bit 0x40000 of a geometry's constants, where `R16_UINT` sets 0x10000
/// and `R32_UINT` 0x20000: `FUN_18001c460`, 0x18001c460), with
/// `IndexCount / 3` triangles (0x180014270). What its indices are is not
/// in the builder's CPU code.
pub const INDEX_53: u32 = 53;
/// `R32G32_FLOAT`: two-float vertices (z read as 0 by Direct3D).
pub const TWO_FLOATS: u32 = 16;
/// Indices and vertices read at most from a geometry to test them.
pub const PEEKED_INDICES: usize = 96;
pub const PEEKED_VERTICES: usize = 8;

/// The first indices of a triangle geometry, read from title memory as
/// 16-bit and as 32-bit values: whether all of those read are below its
/// vertex count, each way. Lists of 16-bit indices pass the first and
/// (with fewer than 65,536 vertices) fail the second; 32-bit ones pass
/// both, their halves being small too. `None`: no indices, too few, or
/// not readable.
pub fn index_fit(memory: &dyn Memory, g: &Geometry) -> Option<[bool; 2]> {
    let Geometry::Triangles {
        index_format,
        index_count,
        vertex_count,
        index_buffer,
        ..
    } = *g
    else {
        return None;
    };
    if index_format == 0 || index_buffer == 0 || index_count < 2 {
        return None;
    }
    let n = (index_count as usize).min(PEEKED_INDICES) & !1;
    let b = memory.bytes(index_buffer, n * 2)?;
    let sixteen = b
        .chunks_exact(2)
        .all(|c| u32::from(u16::from_le_bytes([c[0], c[1]])) < vertex_count);
    let thirty_two = b.chunks_exact(4).all(|c| u32_at(c, 0) < vertex_count);
    Some([sixteen, thirty_two])
}

/// Whether the first vertices of a two-float geometry read as positions
/// do: finite, and 0 or between 1e-4 and 1e5 in size. Packed or quantized
/// data read as floats mostly does not. `None`: not two floats, a stride
/// with bit 15 set (not a stride the builder documents), or not readable.
pub fn plausible_two_floats(memory: &dyn Memory, g: &Geometry) -> Option<bool> {
    let Geometry::Triangles {
        vertex_format: TWO_FLOATS,
        vertex_count,
        vertex_buffer,
        vertex_stride,
        ..
    } = *g
    else {
        return None;
    };
    if vertex_buffer == 0 || vertex_count == 0 || !(8..0x8000).contains(&vertex_stride) {
        return None;
    }
    let n = (vertex_count as usize).min(PEEKED_VERTICES);
    let len = (n - 1) * vertex_stride as usize + 8;
    let b = memory.bytes(vertex_buffer, len)?;
    let sane = |x: f32| x.is_finite() && (x == 0.0 || (1e-4..=1e5).contains(&x.abs()));
    Some((0..n).all(|k| {
        let at = k * vertex_stride as usize;
        sane(f32_at(&b, at)) && sane(f32_at(&b, at + 4))
    }))
}

/// The kinds and flags of `geometries`, counted, in the order first seen:
/// `3 triangles OPAQUE, 1 triangles NONE`. The rehydrate's descriptions
/// say them, but its GPU path copies each from +8 on, past `Type` and
/// `Flags` (0x180014700): whether they are what the console builds with is
/// for a run to show.
pub fn flags_tally(geometries: &[Geometry]) -> String {
    let mut seen: Vec<(String, u32)> = Vec::new();
    for g in geometries {
        let kind = match g {
            Geometry::Triangles { .. } => "triangles",
            Geometry::Aabbs { .. } => "boxes",
            Geometry::Other { .. } => "other",
        };
        let name = format!("{kind} {}", bits(g.flags(), &GEOMETRY_FLAGS));
        match seen.iter_mut().find(|(n, _)| *n == name) {
            Some((_, n)) => *n += 1,
            None => seen.push((name, 1)),
        }
    }
    if seen.is_empty() {
        return "no geometries".into();
    }
    let parts: Vec<String> = seen.iter().map(|(name, n)| format!("{n} {name}")).collect();
    parts.join(", ")
}

/// Geometry descriptions read at most from one build (the rest are counted
/// as unread).
pub const GEOMETRIES_READ: u32 = 1 << 14;

/// The geometries of a bottom-level build, as many as could be read, and
/// whether all were.
pub fn geometries(memory: &dyn Memory, inputs: &Inputs) -> (Vec<Geometry>, bool) {
    if inputs.top() {
        return (Vec::new(), true);
    }
    let n = inputs.count.min(GEOMETRIES_READ);
    let mut out = Vec::with_capacity(n as usize);
    match inputs.layout {
        ARRAY => {
            if let Some(b) = memory.bytes(inputs.descs, n as usize * GEOMETRY) {
                out.extend(b.chunks_exact(GEOMETRY).map(Geometry::parse));
            }
        }
        ARRAY_OF_POINTERS => {
            if let Some(pointers) = memory.bytes(inputs.descs, n as usize * 8) {
                for p in pointers.chunks_exact(8) {
                    match memory.bytes(u64_at(p, 0), GEOMETRY) {
                        Some(b) => out.push(Geometry::parse(&b)),
                        None => break,
                    }
                }
            }
        }
        _ => {}
    }
    let whole = out.len() == inputs.count as usize;
    (out, whole)
}

/// `bytes` as dwords, eight to a line, each line with its byte offset.
pub fn dwords(bytes: &[u8]) -> String {
    let mut text = String::new();
    for (line, row) in bytes.chunks(32).enumerate() {
        let _ = write!(text, "\n    +{:#05x}:", line * 32);
        for word in row.chunks(4) {
            let mut w = [0u8; 4];
            w[..word.len()].copy_from_slice(word);
            let _ = write!(text, " {:08x}", u32::from_le_bytes(w));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Memory of the test's own: byte runs at addresses.
    #[derive(Default)]
    struct Fake(BTreeMap<u64, Vec<u8>>);

    impl Memory for Fake {
        fn bytes(&self, at: u64, len: usize) -> Option<Vec<u8>> {
            let (&start, run) = self.0.range(..=at).next_back()?;
            let from = (at - start) as usize;
            run.get(from..from + len).map(<[u8]>::to_vec)
        }
    }

    fn put32(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    fn put64(b: &mut Vec<u8>, v: u64) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    fn triangles(vertices: u64, indices: u64) -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, TRIANGLES);
        put32(&mut b, 1); // opaque
        put64(&mut b, 0); // no transform
        put32(&mut b, 57); // R16_UINT
        put32(&mut b, 6); // R32G32B32_FLOAT
        put32(&mut b, 300); // indices
        put32(&mut b, 120); // vertices
        put64(&mut b, indices);
        put64(&mut b, vertices);
        put64(&mut b, 12);
        assert_eq!(b.len(), GEOMETRY);
        b
    }

    fn inputs(kind: u32, flags: u32, count: u32, layout: u32, descs: u64) -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, kind);
        put32(&mut b, flags);
        put32(&mut b, count);
        put32(&mut b, layout);
        put64(&mut b, descs);
        b
    }

    #[test]
    fn a_build_description_reads_as_the_headers_lay_it_out() {
        let mut b = Vec::new();
        put64(&mut b, 0x1_0000_1000);
        b.extend(inputs(
            BOTTOM_LEVEL,
            ALLOW_COMPACTION | PREFER_FAST_TRACE,
            2,
            ARRAY,
            0x5000,
        ));
        put64(&mut b, 0);
        put64(&mut b, 0x2_0000_0000);
        assert_eq!(b.len(), BUILD_DESC);
        let d = BuildDesc::parse(&b);
        assert_eq!(d.dest, 0x1_0000_1000);
        assert_eq!(d.scratch, 0x2_0000_0000);
        assert_eq!(d.inputs.level(), "bottom");
        assert_eq!(d.inputs.count, 2);
        assert!(!d.inputs.update());
        assert_eq!(
            build_flags(d.inputs.flags),
            "ALLOW_COMPACTION|PREFER_FAST_TRACE"
        );
        assert_eq!(build_flags(0), "NONE");
        assert_eq!(build_flags(DEHYDRATE | ALLOW_COMPACTION), "ALLOW_COMPACTION|DEHYDRATE");
        assert_eq!(
            build_flags(PERFORM_UPDATE | ALLOW_UPDATE | 0x100),
            "ALLOW_UPDATE|PERFORM_UPDATE|0x100"
        );
    }

    #[test]
    fn geometries_are_read_in_an_array_and_through_pointers() {
        let mut memory = Fake::default();
        let mut array = triangles(0xa000, 0xb000);
        array.extend(triangles(0xc000, 0));
        // The second without indices: its triangles come from the vertices.
        array[GEOMETRY + 16..GEOMETRY + 20].copy_from_slice(&0u32.to_le_bytes());
        memory.0.insert(0x5000, array);
        let i = Inputs::parse(&inputs(BOTTOM_LEVEL, 0, 2, ARRAY, 0x5000));
        let (g, whole) = geometries(&memory, &i);
        assert!(whole);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].triangles(), 100);
        assert_eq!(g[1].triangles(), 40);
        let text = g[0].describe();
        assert!(text.contains("OPAQUE"), "{text}");
        assert!(
            text.contains("R32G32B32_FLOAT stride 12 x120 at 0xa000"),
            "{text}"
        );
        assert!(text.contains("index R16_UINT x300 at 0xb000"), "{text}");
        assert!(text.contains("no transform"), "{text}");
        assert_eq!(
            g[0].spans(),
            [("vertices", 0xa000, 12 * 120), ("indices", 0xb000, 600)]
        );
        assert_eq!(g[1].spans(), [("vertices", 0xc000, 12 * 120)]);

        let mut pointers = Vec::new();
        put64(&mut pointers, 0x5000 + GEOMETRY as u64);
        put64(&mut pointers, 0x5000);
        memory.0.insert(0x9000, pointers);
        let p = Inputs::parse(&inputs(BOTTOM_LEVEL, 0, 2, ARRAY_OF_POINTERS, 0x9000));
        let (g2, whole) = geometries(&memory, &p);
        assert!(whole);
        assert_eq!(g2, [g[1], g[0]]);

        // Asked for more than memory holds: what could be read, and so said.
        let short = Inputs::parse(&inputs(BOTTOM_LEVEL, 0, 3, ARRAY_OF_POINTERS, 0x9000));
        let (g3, whole) = geometries(&memory, &short);
        assert!(!whole);
        assert!(g3.is_empty());
    }

    #[test]
    fn boxes_and_unknown_kinds() {
        let mut b = Vec::new();
        put32(&mut b, AABBS);
        put32(&mut b, 2);
        put64(&mut b, 7);
        put64(&mut b, 0xd000);
        put64(&mut b, 32);
        b.resize(GEOMETRY, 0);
        let g = Geometry::parse(&b);
        assert_eq!(g.aabbs(), 7);
        assert_eq!(g.triangles(), 0);
        assert_eq!(
            g.describe(),
            "boxes NO_DUPLICATE_ANYHIT x7 stride 32 at 0xd000"
        );
        b[0] = 5;
        assert_eq!(Geometry::parse(&b), Geometry::Other { kind: 5, flags: 2 });
    }

    #[test]
    fn an_instance_splits_its_packed_fields() {
        let mut b = Vec::new();
        for i in 0..12 {
            b.extend_from_slice(&(i as f32).to_le_bytes());
        }
        put32(&mut b, 0xff00_0007); // mask 0xff, id 7
        put32(&mut b, 0x0400_0003); // FORCE_OPAQUE, hit group 3
        put64(&mut b, 0x3_0000_0100);
        assert_eq!(b.len(), INSTANCE);
        let i = Instance::parse(&b);
        assert_eq!((i.id, i.mask, i.contribution, i.flags), (7, 0xff, 3, 4));
        assert_eq!(i.structure, 0x3_0000_0100);
        assert_eq!(i.transform[3], 3.0);
        let text = i.describe();
        assert!(text.contains("FORCE_OPAQUE"), "{text}");
        assert!(text.contains("position (3.0 7.0 11.0)"), "{text}");
    }

    #[test]
    fn a_batched_entry_and_its_postbuild_request() {
        let mut b = Vec::new();
        put64(&mut b, 0x10_0000);
        b.extend(inputs(
            TOP_LEVEL,
            PERFORM_UPDATE | ALLOW_UPDATE,
            900,
            ARRAY,
            0x20_0000,
        ));
        put64(&mut b, 0x10_0000);
        put64(&mut b, 0x30_0000);
        put32(&mut b, 1);
        put32(&mut b, 0);
        put64(&mut b, 0x7ff0_0000);
        // +0x40 the records' size in bytes, +0x48 their address.
        put64(&mut b, 32);
        put64(&mut b, 0x55_0000);
        assert_eq!(b.len(), BATCHED);
        let e = Batched::parse(&b);
        assert!(e.build.inputs.top() && e.build.inputs.update());
        assert_eq!(e.build.source, e.build.dest);
        assert_eq!(
            (e.postbuild_count, e.word, e.postbuild),
            (1, 0, 0x7ff0_0000)
        );
        assert_eq!(
            e.extensions,
            Stream {
                bytes: 32,
                at: 0x55_0000
            }
        );
        assert!(!e.extensions.empty());
        assert!(Stream { bytes: 16, at: 0 }.empty());

        let mut p = Vec::new();
        put64(&mut p, 0x4000);
        put32(&mut p, 0);
        put32(&mut p, 0);
        let p = Postbuild::parse(&p);
        assert_eq!((p.dest, p.what()), (0x4000, "compacted size"));
    }

    /// A 16-byte extension record: the kind, then three dwords.
    fn record(kind: u32, a: u32, b: u32, c: u32) -> Vec<u8> {
        let mut r = Vec::new();
        for w in [kind, a, b, c] {
            put32(&mut r, w);
        }
        r
    }

    #[test]
    fn extension_records_read_as_the_builder_reads_them() {
        // Kind 0: a float at +4 and at +8, four bytes at +0xc.
        let q = Record::parse(&record(
            0,
            (-1.0f32).to_bits(),
            12.5f32.to_bits(),
            0x0403_0201,
        ));
        assert_eq!(
            q,
            Record::Thresholds {
                quad_join: -1.0,
                box16_inflation: 12.5,
                treelet: [1, 2, 3, 4]
            }
        );
        // Kind 1: the instances at +8, the k-DOP N in the byte at +0xd.
        let s = Record::parse(&record(1, 0xdead, 700, 0x0000_0900));
        assert_eq!(
            s,
            Record::Sizes {
                instances: 700,
                bytes: [0, 9, 0, 0]
            }
        );
        assert!(
            s.describe().contains("instances 700 k-DOP N 9"),
            "{}",
            s.describe()
        );
        // Kind 2: the extra instances at +4 (0xffffffff: none), two floats after.
        let x = Record::parse(&record(2, u32::MAX, 0x3e4c_cccd, 0x3f40_0000));
        assert_eq!(
            x,
            Record::Split {
                extra: u32::MAX,
                split16: 0.2,
                isolate: 0.75
            }
        );
        assert!(
            x.describe().contains("extra instances none (split16 0.2, isolate 0.75)"),
            "{}",
            x.describe()
        );
        assert_eq!(Record::parse(&record(7, 1, 2, 3)).kind(), 7);

        // The walk stops after the first record of a kind it does not know;
        // a size that is not a multiple of 16 has its last part record read.
        let mut list = record(2, 5, 0, 0);
        list.extend(record(9, 0, 0, 0));
        list.extend(record(1, 0, 40, 0));
        let mut memory = Fake::default();
        memory.0.insert(0x8000, list);
        let r = records(
            &memory,
            &Stream {
                bytes: 40,
                at: 0x8000,
            },
        )
        .unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].kind(), 9);
        assert_eq!(
            records(
                &memory,
                &Stream {
                    bytes: 0,
                    at: 0x8000
                }
            ),
            Some(vec![])
        );
        assert_eq!(
            records(
                &memory,
                &Stream {
                    bytes: 64,
                    at: 0x8000
                }
            ),
            None
        );
        let text = extensions(
            &memory,
            &Stream {
                bytes: 40,
                at: 0x8000,
            },
            &Inputs::parse(&inputs(TOP_LEVEL, 0, 10, ARRAY, 0)),
        );
        assert!(text.contains("SIZE NOT A MULTIPLE OF 16"), "{text}");
        assert!(text.contains("KIND 9"), "{text}");
        // Kind 1 lies after the unknown kind: the builder never sees it.
        assert!(text.contains("10 instances + 5 extra for splitting"), "{text}");
        assert_eq!(
            extensions(&memory, &Stream::default(), &Inputs::default()),
            "no extension records"
        );
    }

    #[test]
    fn what_the_builder_takes_from_the_records() {
        let top = |flags, count| Inputs::parse(&inputs(TOP_LEVEL, flags, count, ARRAY, 0));
        let bottom = |flags| Inputs::parse(&inputs(BOTTOM_LEVEL, flags, 4, ARRAY, 0));
        let sizes = Record::parse(&record(1, 0, 300, 0x0001_1400));
        // Kind 1's instances replace NumDescs on a top level that cannot be
        // updated and has two or more (0x18000c780) ...
        let t = taken(&top(PREFER_FAST_TRACE, 100), &[sizes]);
        assert_eq!(t.instances, Some(300));
        // ... with 35 % more instances for splitting for fast trace (0x18000c820) ...
        assert_eq!(t.extra, Some(35));
        assert_eq!(t.level, None);
        assert_eq!(t.box16_inflation, 0.0);
        // ... but not with ALLOW_UPDATE, nor with fewer than two.
        let u = taken(&top(ALLOW_UPDATE | PREFER_FAST_TRACE, 100), &[sizes]);
        assert_eq!((u.instances, u.extra), (Some(100), Some(0)));
        assert_eq!(taken(&top(0, 1), &[sizes]).instances, Some(1));
        // Kind 2's extra instances win over the 35 %, unless 0xffffffff.
        let slots = Record::parse(&record(2, 7, 0, 0));
        assert_eq!(taken(&top(PREFER_FAST_TRACE, 100), &[slots]).extra, Some(7));
        let none = Record::parse(&record(2, u32::MAX, 0, 0));
        assert_eq!(taken(&top(PREFER_FAST_TRACE, 100), &[none]).extra, Some(35));
        assert_eq!(taken(&top(0, 100), &[none]).extra, Some(0));

        // A bottom level takes the k-DOP N at +0xd, at most 15 (0x180011b80).
        assert_eq!(taken(&bottom(0), &[sizes]).level, Some(15));
        let three = Record::parse(&record(1, 0, 0, 0x0300));
        assert_eq!(taken(&bottom(0), &[three]).level, Some(3));
        assert!(
            taken(&bottom(0), &[three])
                .describe()
                .contains("k-DOP N 3 (+1296 bytes)")
        );
        assert_eq!(taken(&bottom(0), &[]).level, None);
        assert_eq!(taken(&bottom(0), &[]).instances, None);

        // Kind 0's box16 inflation threshold, its defaults by flags, and the mode bit.
        assert_eq!(taken(&bottom(0), &[]).box16_inflation, 1.005);
        assert_eq!(taken(&bottom(MINIMIZE_MEMORY), &[]).box16_inflation, 1.01);
        assert_eq!(
            taken(&bottom(MINIMIZE_MEMORY | PREFER_FAST_TRACE), &[]).box16_inflation,
            1.005
        );
        let low = Record::parse(&record(0, 0, (-2.0f32).to_bits(), 0));
        assert_eq!(taken(&bottom(PREFER_FAST_TRACE), &[low]).box16_inflation, 0.0);
        let high = Record::parse(&record(0, 0, 11.0f32.to_bits(), 0));
        assert!(taken(&bottom(MINIMIZE_MEMORY), &[high]).mode);
        assert!(!taken(&bottom(0), &[high]).mode);
        assert!(!taken(&top(MINIMIZE_MEMORY, 4), &[high]).mode);
        let ten = Record::parse(&record(0, 0, 10.0f32.to_bits(), 0));
        assert!(!taken(&bottom(MINIMIZE_MEMORY), &[ten]).mode);
        // The first record of a kind is the one taken.
        assert_eq!(taken(&bottom(0), &[ten, high]).box16_inflation, 10.0);
    }

    /// 64 bytes of shipped data: the first dword, the leaves at +0xc, a box
    /// at +0x10, the header's size switch and size at +0x2c/+0x38, the root
    /// at +0x30 and the placement word at +0x3c.
    fn shipped(
        first: u32,
        low: [f32; 3],
        high: [f32; 3],
        sized: (u32, u32),
        placement: u32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, first);
        put32(&mut b, 0);
        put32(&mut b, 0);
        put32(&mut b, 5000);
        for f in low.iter().chain(&high) {
            b.extend_from_slice(&f.to_bits().to_le_bytes());
        }
        put32(&mut b, 0); // +0x28
        put32(&mut b, sized.0);
        put32(&mut b, 0x45); // +0x30: box32 node 8
        put32(&mut b, 0);
        put32(&mut b, sized.1);
        put32(&mut b, placement);
        assert_eq!(b.len(), DEHYDRATED);
        b
    }

    #[test]
    fn shipped_data_reads_as_the_rehydrate_reads_it() {
        // Bit 24 set, level 3 in bits 20..23: a header of 3²·0x90 + 0x40.
        let d = Dehydrated::parse(&shipped(
            0x0130_0000,
            [-1.0; 3],
            [1.0; 3],
            (0, 0),
            NO_PLACEMENT,
        ));
        assert!(d.valid());
        assert_eq!(
            (d.level(), d.header(), d.leaves, d.root),
            (3, 3 * 3 * 0x90 + 0x40, 5000, 0x45)
        );
        assert_eq!(node_kind(d.root), "box32");
        assert!(d.placement.none() && d.placement.identity());
        let text = d.describe();
        assert!(text.contains("bit 24 set, level 3"), "{text}");
        assert!(text.contains("+60 0x7f7f7f (none)"), "{text}");
        // +0x2c not 0: the header's size is +0x38.
        let sized = Dehydrated::parse(&shipped(
            0x0100_0000,
            [0.0; 3],
            [0.0; 3],
            (1, 0x200),
            NO_PLACEMENT,
        ));
        assert_eq!(sized.header(), 0x200);
        // Without bit 24 the builder refuses the call.
        let bad = Dehydrated::parse(&shipped(
            0x0030_0000,
            [0.0; 3],
            [0.0; 3],
            (0, 0),
            NO_PLACEMENT,
        ));
        assert!(!bad.valid());
        assert!(
            bad.describe().contains("BIT 24 CLEAR"),
            "{}",
            bad.describe()
        );
    }

    #[test]
    fn the_placement_word_names_rotation_scale_and_centring() {
        // Rotation 5, centred, scaled by 2^1 2^0 2^-2.
        let word = 1 << 31 | 1 << 30 | 5 << 24 | 125 << 16 | 127 << 8 | 128;
        let p = Placement(word);
        assert!(!p.none() && !p.identity());
        assert_eq!(p.rotation(), 5);
        assert!(p.centred() && p.scaled());
        assert_eq!(p.exponents(), Some([1, 0, -2]));
        assert!(
            p.describe()
                .contains("rotation 5, scale 2^1 2^0 2^-2, centred"),
            "{}",
            p.describe()
        );
        // Without bit 31 the bytes are not exponents.
        assert_eq!(Placement(3 << 24 | 0x80_8080).exponents(), None);
        assert_eq!(Placement(3 << 24 | 0x80_8080).rotation(), 3);
        // Another word that changes nothing: rotation 0, exponents of 1.0.
        let plain = Placement(1 << 31 | 0x7f_7f7f);
        assert!(!plain.none() && plain.identity());
        assert!(
            plain.describe().contains("in effect none"),
            "{}",
            plain.describe()
        );
        // The centring moves an axis whose box does not hold 0, to its middle.
        let d = Dehydrated::parse(&shipped(
            0x0100_0000,
            [2.0, -1.0, -6.0],
            [4.0, 1.0, -2.0],
            (0, 0),
            1 << 30,
        ));
        assert_eq!(d.centre(), [3.0, 0.0, -4.0]);
        assert!(
            d.describe().contains("(moved by 3 0 -4)"),
            "{}",
            d.describe()
        );
    }

    #[test]
    fn indices_and_two_float_vertices_are_tested_for_what_they_hold() {
        let mut memory = Fake::default();
        // 16-bit indices below 120 vertices: they fit as 16-bit, and as
        // 32-bit pairs (b << 16 | a) they do not.
        let mut sixteen = Vec::new();
        for i in [0u16, 1, 2, 2, 1, 3, 100, 119] {
            sixteen.extend_from_slice(&i.to_le_bytes());
        }
        memory.0.insert(0xb000, sixteen);
        let mut list = triangles(0xa000, 0xb000);
        list[16..20].copy_from_slice(&INDEX_53.to_le_bytes());
        list[24..28].copy_from_slice(&8u32.to_le_bytes());
        let g = Geometry::parse(&list);
        assert_eq!(index_fit(&memory, &g), Some([true, false]));
        // 32-bit indices pass both: their halves are small too.
        let mut wide = Vec::new();
        for i in [0u32, 1, 2, 119] {
            put32(&mut wide, i);
        }
        memory.0.insert(0xb000, wide);
        assert_eq!(index_fit(&memory, &g), Some([true, true]));
        // No indices, or none readable.
        assert_eq!(
            index_fit(&memory, &Geometry::parse(&triangles(0xa000, 0))),
            None
        );
        assert_eq!(index_fit(&Fake::default(), &g), None);

        // Two floats a vertex: positions, or packed data read as floats.
        let mut b = triangles(0xc000, 0);
        b[20..24].copy_from_slice(&TWO_FLOATS.to_le_bytes());
        b[48..56].copy_from_slice(&8u64.to_le_bytes());
        let two = Geometry::parse(&b);
        let mut floats = Vec::new();
        for f in [0.0f32, 1.5, -20.25, 300.0, 0.001, -7.0, 2.0, 4.0] {
            floats.extend_from_slice(&f.to_le_bytes());
        }
        memory.0.insert(0xc000, floats);
        // (120 vertices; the first 8 read, 4 of them in these 32 bytes.)
        assert_eq!(plausible_two_floats(&memory, &two), None);
        let mut floats = Vec::new();
        for k in 0..16 {
            floats.extend_from_slice(&(k as f32 * 0.5).to_le_bytes());
        }
        memory.0.insert(0xc000, floats);
        assert_eq!(plausible_two_floats(&memory, &two), Some(true));
        let mut packed = Vec::new();
        for _ in 0..16 {
            put32(&mut packed, 0x7fc0_1234);
        }
        memory.0.insert(0xc000, packed);
        assert_eq!(plausible_two_floats(&memory, &two), Some(false));
        // Not two floats, or a stride with bit 15.
        assert_eq!(
            plausible_two_floats(&memory, &Geometry::parse(&triangles(0xc000, 0))),
            None
        );
        b[48..56].copy_from_slice(&0x8008u64.to_le_bytes());
        assert_eq!(plausible_two_floats(&memory, &Geometry::parse(&b)), None);
    }

    #[test]
    fn geometry_flags_are_counted_by_kind() {
        let opaque = Geometry::parse(&triangles(0xa000, 0xb000));
        let mut b = triangles(0xa000, 0);
        b[4..8].copy_from_slice(&0u32.to_le_bytes());
        let other = Geometry::parse(&b);
        assert!(opaque.opaque_triangles() && !other.opaque_triangles());
        assert_eq!((opaque.flags(), other.flags()), (1, 0));
        assert_eq!(
            flags_tally(&[opaque, other, opaque]),
            "2 triangles OPAQUE, 1 triangles NONE"
        );
        assert_eq!(flags_tally(&[]), "no geometries");
    }

    #[test]
    fn formats_and_dwords() {
        assert_eq!(format(6), "R32G32B32_FLOAT");
        assert_eq!(format(999), "format 999");
        let text = dwords(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 7]);
        assert_eq!(text, "\n    +0x000: 00000001 ffffffff 00000007");
    }
}
