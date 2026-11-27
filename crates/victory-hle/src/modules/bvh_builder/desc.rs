//! The descriptions the title hands its structure builder, read from bytes.
//!
//! Everything Direct3D 12 defines on every platform is laid out as the
//! public headers say (`d3d12.h`: the build inputs and description, the
//! geometry descriptions, the instance description, the prebuild sizes and
//! the postbuild requests). Two are the console's own and not public:
//!
//! - `D3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2`, read
//!   here as [`Batched`]: a build description followed by a count and an
//!   address of postbuild requests, 80 bytes in all. **Inferred**, not
//!   confirmed: the trace dumps the bytes of the first calls in hex so that
//!   a run can say whether the fields hold what they are taken for;
//! - `D3D12XBOX_XDXR_STREAM_DESC`, which the prebuild call takes beside the
//!   inputs: not decoded at all, only dumped.
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

const BUILD_FLAGS: [(u32, &str); 6] = [
    (ALLOW_UPDATE, "ALLOW_UPDATE"),
    (ALLOW_COMPACTION, "ALLOW_COMPACTION"),
    (PREFER_FAST_TRACE, "PREFER_FAST_TRACE"),
    (PREFER_FAST_BUILD, "PREFER_FAST_BUILD"),
    (MINIMIZE_MEMORY, "MINIMIZE_MEMORY"),
    (PERFORM_UPDATE, "PERFORM_UPDATE"),
];

// D3D12_ELEMENTS_LAYOUT.
pub const ARRAY: u32 = 0;
pub const ARRAY_OF_POINTERS: u32 = 1;

// D3D12_RAYTRACING_GEOMETRY_TYPE and _FLAGS.
const TRIANGLES: u32 = 0;
const AABBS: u32 = 1;
const GEOMETRY_FLAGS: [(u32, &str); 2] = [(0x1, "OPAQUE"), (0x2, "NO_DUPLICATE_ANYHIT")];

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

/// One entry of a batched build (inferred, see the module): the build, then
/// the postbuild requests the title wants answered for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Batched {
    pub build: BuildDesc,
    pub postbuild_count: u32,
    /// The dword after the count (taken for padding).
    pub word: u32,
    pub postbuild: u64,
    /// The last 16 bytes (meaning unknown).
    pub rest: [u64; 2],
}

pub const BATCHED: usize = 80;

impl Batched {
    pub fn parse(b: &[u8]) -> Batched {
        Batched {
            build: BuildDesc::parse(&b[..BUILD_DESC]),
            postbuild_count: u32_at(b, 48),
            word: u32_at(b, 52),
            postbuild: u64_at(b, 56),
            rest: [u64_at(b, 64), u64_at(b, 72)],
        }
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
        put64(&mut b, 0);
        put64(&mut b, 0x55);
        assert_eq!(b.len(), BATCHED);
        let e = Batched::parse(&b);
        assert!(e.build.inputs.top() && e.build.inputs.update());
        assert_eq!(e.build.source, e.build.dest);
        assert_eq!(
            (e.postbuild_count, e.word, e.postbuild),
            (1, 0, 0x7ff0_0000)
        );
        assert_eq!(e.rest, [0, 0x55]);

        let mut p = Vec::new();
        put64(&mut p, 0x4000);
        put32(&mut p, 0);
        put32(&mut p, 0);
        let p = Postbuild::parse(&p);
        assert_eq!((p.dest, p.what()), (0x4000, "compacted size"));
    }

    #[test]
    fn formats_and_dwords() {
        assert_eq!(format(6), "R32G32B32_FLOAT");
        assert_eq!(format(999), "format 999");
        let text = dwords(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 7]);
        assert_eq!(text, "\n    +0x000: 00000001 ffffffff 00000007");
    }
}
