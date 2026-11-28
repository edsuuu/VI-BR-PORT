//! The console's packed triangle data, and its expansion into what the
//! host's builder takes (`VICTORY_DXR_EXPAND`, `super::bottom`).
//!
//! Most of the title's geometry descriptions name data that is not what
//! their Direct3D formats say. The batched build's `LoadTriangles` stage and
//! the unpacking's `LeafRehydration` stage of `XboxBvhBuilder.dll` read both
//! kinds the same way (the layouts the L track read in their GFX10 code,
//! `docs/agente-rtn-l.md` section 15):
//!
//! - **Index format 53** (`DXGI_FORMAT_R16_TYPELESS`, the stages' index
//!   mode 4): a 12-byte header (the number of blocks in the low 17 bits of
//!   its first dword, the number of triangle records in its second), one
//!   16-byte entry a block, one 32-bit record a triangle (its block in the
//!   top 11 bits, three 7-bit offsets below), and the 16-bit vertex indices
//!   the offsets pick from the block's base on. [`expand_indices`] makes
//!   `R32_UINT` triangles of them.
//! - **Vertex format `R32G32_FLOAT`** (16, the stages' vertex code 0):
//!   quantized positions, 24, 24 and 16 bits in two dwords, each scaled and
//!   offset by floats the title keeps in the 32 bytes before the vertices,
//!   with an axis order ([`Quantization`]). [`expand_positions`] makes
//!   `R32G32B32_FLOAT` positions of them.
//!
//! Bit 15 of `StrideInBytes` is not part of the stride (the stages take its
//! low 15 bits); with index format 53 it swaps each triangle's first and
//! third index, its winding ([`FLIP`]).
//!
//! The positions are in the geometry's own frame: the description's
//! `Transform3x4` applies after them, as in Direct3D.
//!
//! The bit layouts here are the one definition: the listening post's
//! decoder (`victory_hle::modules::bvh_builder::packed`, which tries them on
//! samples of the title's data) uses these functions.

/// The index format of the packed indices.
pub const INDEX_53: u32 = 53;
/// The vertex format of the quantized positions.
pub const QUANTIZED: u32 = 16;
/// The bit of `StrideInBytes` that flips the winding with [`INDEX_53`].
pub const FLIP: u64 = 0x8000;
/// The stride bits the stages use.
pub const STRIDE: u64 = 0x7fff;
/// The header of packed indices, and a block entry, in bytes.
pub const HEADER: u64 = 12;
pub const BLOCK: u64 = 16;
/// Bytes before the vertices that hold their [`Quantization`].
pub const QUANTIZATION: u64 = 0x20;
/// Bytes of one quantized position.
pub const POSITION: u64 = 8;

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A triangle record: its block (bits 21..31) and the three 7-bit offsets
/// of its corners (bits 0..6, 7..13, 14..20), in corner order.
pub fn record(w: u32) -> (u32, [u32; 3]) {
    (w >> 21, [w & 0x7f, (w >> 7) & 0x7f, (w >> 14) & 0x7f])
}

/// Where a block's indices begin, in 16-bit indices: bit 14 of its first
/// dword above the low 16 bits of its second.
pub fn block_base(e0: u32, e1: u32) -> u32 {
    ((e0 << 2) & 0x1_0000) | (e1 & 0xffff)
}

/// The header of packed indices: the number of blocks and of triangle
/// records (from its first 8 bytes).
pub fn header(b: &[u8]) -> Option<(u32, u32)> {
    Some((u32_le(b, 0)? & 0x1_ffff, u32_le(b, 4)?))
}

/// Where the triangle records end, from the start of the packed indices:
/// the 16-bit indices begin there.
pub fn records_end(blocks: u32, records: u32) -> u64 {
    HEADER + BLOCK * u64::from(blocks) + 4 * u64::from(records)
}

/// The three quantized fields of a vertex's two dwords: 24 bits at the top
/// of the first, 24 bits of the second's low half above the first's low
/// byte, 16 bits at the top of the second.
pub fn fields(d0: u32, d1: u32) -> [u32; 3] {
    [d0 >> 8, ((d1 & 0xffff) << 8) | (d0 & 0xff), d1 >> 16]
}

/// How quantized positions become floats: kept by the title in the 32
/// bytes before the vertices (offsets x, y, z and the axis order at -0x20,
/// scales x, y, z at -0x10; the last dword is not read).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantization {
    pub offset: [f32; 3],
    pub order: u32,
    pub scale: [f32; 3],
}

impl Quantization {
    /// From the [`QUANTIZATION`] bytes before the vertices.
    pub fn parse(b: &[u8]) -> Option<Quantization> {
        let f = |at| u32_le(b, at).map(f32::from_bits);
        Some(Quantization {
            offset: [f(0)?, f(4)?, f(8)?],
            order: u32_le(b, 12)?,
            scale: [f(16)?, f(20)?, f(24)?],
        })
    }

    /// A vertex's position from its two dwords: each field scaled and
    /// offset by its own pair, then put in the axis order (0: the third
    /// field is x and the first z; 1: the second is x, the third y; any
    /// other: in field order).
    pub fn position(&self, d0: u32, d1: u32) -> [f32; 3] {
        let q = fields(d0, d1);
        let p: [f32; 3] = std::array::from_fn(|k| self.scale[k] * q[k] as f32 + self.offset[k]);
        match self.order {
            0 => [p[2], p[1], p[0]],
            1 => [p[1], p[2], p[0]],
            _ => p,
        }
    }
}

/// What an expansion met that is not as the stages read it; each such
/// triangle or vertex is written so that nothing is hit there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Odd {
    /// Triangles made degenerate (all three corners vertex 0): a record past
    /// the records, a block past the blocks, an index past the data or past
    /// the vertex count.
    pub degenerate: u32,
    /// Positions that are not finite, or vertices past the data (written
    /// with a NaN x: the triangles that use them are inactive).
    pub not_finite: u32,
}

/// The bytes of packed indices that `triangles` triangles read, from the
/// start: the records and blocks they name and the 16-bit indices up to the
/// furthest one they pick. `head` holds at least [`records_end`] bytes.
/// `None`: the header is not there.
pub fn indices_span(head: &[u8], triangles: u32) -> Option<u64> {
    let (blocks, records) = header(head)?;
    let first = records_end(blocks, records);
    let mut furthest = None::<u32>;
    for t in 0..triangles.min(records) {
        let Some(w) = u32_le(
            head,
            (HEADER + BLOCK * u64::from(blocks)) as usize + 4 * t as usize,
        ) else {
            break;
        };
        let (block, corners) = record(w);
        if block >= blocks {
            continue;
        }
        let at = (HEADER + BLOCK * u64::from(block)) as usize;
        let (Some(e0), Some(e1)) = (u32_le(head, at), u32_le(head, at + 4)) else {
            continue;
        };
        let base = block_base(e0, e1);
        let top = base + corners.iter().max().copied().unwrap_or(0);
        furthest = Some(furthest.map_or(top, |f| f.max(top)));
    }
    Some(first + furthest.map_or(0, |k| 2 * (u64::from(k) + 1)))
}

/// The `triangles` triangles of the packed indices in `data` (from their
/// start, as far as [`indices_span`] says or more) as three `u32` vertex
/// indices each into `out`, the first and third swapped with `flip`
/// ([`FLIP`]). A triangle that reads past `data`, names a block past the
/// blocks, or picks a vertex at or past `vertices` is made degenerate.
pub fn expand_indices(
    data: &[u8],
    triangles: u32,
    flip: bool,
    vertices: u32,
    out: &mut [u32],
) -> Odd {
    let mut odd = Odd::default();
    let (blocks, records) = header(data).unwrap_or((0, 0));
    let records_at = (HEADER + BLOCK * u64::from(blocks)) as usize;
    let indices_at = records_end(blocks, records) as usize;
    let index = |k: u32| -> Option<u32> {
        let at = indices_at + 2 * k as usize;
        Some(u32::from(u16::from_le_bytes(
            data.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let triangle = |t: u32| -> Option<[u32; 3]> {
        if t >= records {
            return None;
        }
        let (block, corners) = record(u32_le(data, records_at + 4 * t as usize)?);
        if block >= blocks {
            return None;
        }
        let at = (HEADER + BLOCK * u64::from(block)) as usize;
        let base = block_base(u32_le(data, at)?, u32_le(data, at + 4)?);
        let mut v = [0u32; 3];
        for (o, l) in v.iter_mut().zip(corners) {
            *o = index(base + l)?;
            if *o >= vertices {
                return None;
            }
        }
        if flip {
            v.swap(0, 2);
        }
        Some(v)
    };
    for (t, o) in (0..triangles).zip(out.chunks_exact_mut(3)) {
        match triangle(t) {
            Some(v) => o.copy_from_slice(&v),
            None => {
                o.fill(0);
                odd.degenerate += 1;
            }
        }
    }
    odd
}

/// The bytes `count` quantized positions `stride` apart span, with the
/// [`QUANTIZATION`] before them: what [`expand_positions`] reads, from the
/// vertices' address less [`QUANTIZATION`].
pub fn positions_span(stride: u64, count: u32) -> u64 {
    match count {
        0 => QUANTIZATION,
        n => QUANTIZATION + (stride & STRIDE) * u64::from(n - 1) + POSITION,
    }
}

/// The `count` quantized positions `stride` apart (its bit 15 and above
/// dropped, as the stages do) of `data` (from the [`QUANTIZATION`] before
/// the vertices on) as three `f32`s each into `out`. A position that is not
/// finite, or past `data`, gets a NaN x (Direct3D: the triangles that use
/// it are inactive).
pub fn expand_positions(data: &[u8], stride: u64, count: u32, out: &mut [f32]) -> Odd {
    let mut odd = Odd::default();
    let q = Quantization::parse(data);
    let stride = (stride & STRIDE) as usize;
    let base = QUANTIZATION as usize;
    for (i, o) in (0..count as usize).zip(out.chunks_exact_mut(3)) {
        let at = base + stride * i;
        let p = match (q, u32_le(data, at), u32_le(data, at + 4)) {
            (Some(q), Some(d0), Some(d1)) => Some(q.position(d0, d1)),
            _ => None,
        };
        match p {
            Some(p) if p.iter().all(|c| c.is_finite()) => o.copy_from_slice(&p),
            _ => {
                o.copy_from_slice(&[f32::NAN, 0.0, 0.0]);
                odd.not_finite += 1;
            }
        }
    }
    odd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(b: &mut Vec<u8>, w: u32) {
        b.extend_from_slice(&w.to_le_bytes());
    }

    /// Packed indices of two blocks and three triangles, laid out as the
    /// stages read them (the listening post's test has the same).
    fn packed() -> Vec<u8> {
        let mut b = Vec::new();
        // 2 blocks (bit 17 and above are not the count), 3 records, a third
        // dword the stages do not read.
        put(&mut b, 2 | 1 << 20);
        put(&mut b, 3);
        put(&mut b, 0xdead_beef);
        // Block 0 begins at index 0; block 1 at 0x10002.
        for (e0, e1) in [(0, 0), (1 << 14, 0xabcd_0002)] {
            put(&mut b, e0);
            put(&mut b, e1);
            put(&mut b, 0);
            put(&mut b, 0);
        }
        let rec = |block: u32, l: [u32; 3]| block << 21 | l[2] << 14 | l[1] << 7 | l[0];
        put(&mut b, rec(0, [0, 1, 2]));
        put(&mut b, rec(0, [5, 3, 127]));
        put(&mut b, rec(1, [0, 1, 2]));
        let mut indices = vec![0u16; 0x1_0005];
        for (k, v) in indices.iter_mut().enumerate().take(128) {
            *v = 1000 + k as u16;
        }
        indices[0x1_0002..].copy_from_slice(&[7, 8, 9]);
        for v in indices {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    #[test]
    fn packed_indices_expand_to_three_u32s_a_triangle() {
        let data = packed();
        assert_eq!(header(&data), Some((2, 3)));
        // The records end after the header, two blocks and three records;
        // the furthest index is block 1's third one, 0x10004.
        let end = records_end(2, 3);
        assert_eq!(end, 12 + 32 + 12);
        assert_eq!(indices_span(&data, 3), Some(end + 2 * 0x1_0005));
        assert_eq!(indices_span(&data, 3), Some(data.len() as u64));
        // Two triangles only read block 0, up to its index 127.
        assert_eq!(indices_span(&data, 2), Some(end + 2 * 128));
        let mut out = vec![!0u32; 9];
        assert_eq!(
            expand_indices(&data, 3, false, 2000, &mut out),
            Odd::default()
        );
        assert_eq!(out, [1000, 1001, 1002, 1005, 1003, 1127, 7, 8, 9]);
        // The stride's bit 15: the first and third corners swapped.
        expand_indices(&data, 3, true, 2000, &mut out);
        assert_eq!(out, [1002, 1001, 1000, 1127, 1003, 1005, 9, 8, 7]);
    }

    #[test]
    fn what_the_stages_would_not_read_makes_degenerate_triangles() {
        let data = packed();
        let mut out = vec![!0u32; 12];
        // A vertex count of 1010: the second triangle picks vertex 1127.
        let odd = expand_indices(&data, 4, false, 1010, &mut out);
        // The fourth has no record.
        assert_eq!(odd.degenerate, 2);
        assert_eq!(out, [1000, 1001, 1002, 0, 0, 0, 7, 8, 9, 0, 0, 0]);
        // Cut short: the third triangle's indices are past the data.
        let odd = expand_indices(&data[..data.len() - 2], 3, false, 2000, &mut out[..9]);
        assert_eq!(odd.degenerate, 1);
        assert_eq!(&out[6..9], &[0, 0, 0]);
        // A record naming block 5 of 2.
        let mut bad = data.clone();
        let at = (HEADER + 2 * BLOCK) as usize;
        bad[at..at + 4].copy_from_slice(&(5u32 << 21).to_le_bytes());
        assert_eq!(
            expand_indices(&bad, 1, false, 2000, &mut out[..3]).degenerate,
            1
        );
        // Nothing there at all.
        assert_eq!(
            expand_indices(&[], 2, false, 10, &mut out[..6]).degenerate,
            2
        );
        assert_eq!(indices_span(&[0; 4], 1), None);
    }

    #[test]
    fn quantized_positions_expand_with_their_scale_offset_and_order() {
        // Fields 0x123456 (24 bits), 0xabcdef (24 bits), 0x8001 (16 bits).
        let (d0, d1) = (0x1234_56ef, 0x8001_abcd);
        assert_eq!(fields(d0, d1), [0x12_3456, 0xab_cdef, 0x8001]);
        let mut b = Vec::new();
        for w in [10.0f32, 20.0, 30.0] {
            put(&mut b, w.to_bits());
        }
        put(&mut b, 2);
        for w in [0.5f32, 0.25, 2.0] {
            put(&mut b, w.to_bits());
        }
        put(&mut b, 0);
        // Two vertices 12 bytes apart (stride 12 with bit 15: 8 bytes read).
        put(&mut b, d0);
        put(&mut b, d1);
        put(&mut b, 0x5555_5555);
        put(&mut b, 0x0000_0100);
        put(&mut b, 0x0003_0003);
        let stride = 12 | FLIP;
        assert_eq!(positions_span(stride, 2), b.len() as u64);
        let p = [
            0.5 * 0x12_3456 as f32 + 10.0,
            0.25 * 0xab_cdef as f32 + 20.0,
            2.0 * 0x8001 as f32 + 30.0,
        ];
        let one = [0.5 + 10.0, 0.25 * 0x300 as f32 + 20.0, 2.0 * 3.0 + 30.0];
        let mut out = vec![0f32; 6];
        // Order 2 (any but 0 and 1): the fields in their order.
        assert_eq!(expand_positions(&b, stride, 2, &mut out), Odd::default());
        assert_eq!(out, [p, one].concat());
        // Order 0: the third field is x, the first z; order 1: the second x.
        let q = Quantization::parse(&b).unwrap();
        assert_eq!(
            Quantization { order: 0, ..q }.position(d0, d1),
            [p[2], p[1], p[0]]
        );
        assert_eq!(
            Quantization { order: 1, ..q }.position(d0, d1),
            [p[1], p[2], p[0]]
        );
        // A vertex past the data, or a scale that is not finite: a NaN x.
        let odd = expand_positions(&b[..b.len() - 1], stride, 2, &mut out);
        assert_eq!(odd.not_finite, 1);
        assert!(out[3].is_nan() && out[..3] == p);
        let mut inf = b.clone();
        inf[16..20].copy_from_slice(&f32::INFINITY.to_bits().to_le_bytes());
        assert_eq!(expand_positions(&inf, stride, 2, &mut out).not_finite, 2);
        assert_eq!(positions_span(8, 0), QUANTIZATION);
    }
}
