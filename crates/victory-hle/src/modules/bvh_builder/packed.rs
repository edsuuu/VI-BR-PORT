//! The console's packed triangle data, as its builder's stages read it.
//!
//! Two kinds of geometry description name data that is not what their
//! Direct3D formats say. Both are read the same way by the batched build's
//! `LoadTriangles` stage and the unpacking's `LeafRehydration` stage of
//! `XboxBvhBuilder.dll` (their GFX10 code; the CPU code only passes the
//! description's fields on, `0x18001c460`):
//!
//! - **Index format 53** (`DXGI_FORMAT_R16_TYPELESS`, the stages' index
//!   mode 4): two levels of indices ([`Indices53`]). A 12-byte header (the
//!   number of blocks in its low 17 bits, then the number of triangle
//!   records), 16-byte block entries, one 32-bit record a triangle (a block
//!   in its top 11 bits, three 7-bit offsets below), and the 16-bit vertex
//!   indices the offsets pick from the block's base on.
//! - **Vertex format `R32G32_FLOAT`** (16; the stages' vertex code 0, a
//!   stride of 8): quantized positions ([`Quantization`]), 24, 24 and 16
//!   bits in two dwords, each scaled and offset by floats the title keeps
//!   in the 32 bytes before the vertices (the stages read them at the
//!   vertex buffer's address less 0x20 and 0x10), with an axis order.
//!
//! Bit 15 of `StrideInBytes` is not part of the stride (the stages take its
//! low 15 bits): with index format 53 it swaps a triangle's first and third
//! index, its winding ([`FLIP`]).
//!
//! Positions decode to the geometry's own frame, before the frame of the
//! structure's nodes (`desc::Placement`) and before its `Transform3x4`.

use super::desc::Memory;
use victory_gpu::dxr::packed as layout;

/// The constants and bit layouts are the host's expansion's
/// (`victory_gpu::dxr::packed`, `VICTORY_DXR_EXPAND`): one definition for
/// what the samples here try and what the host builds from.
pub use layout::{BLOCK, FLIP, HEADER, INDEX_53, QUANTIZED, STRIDE, block_base, fields, record};

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or_default())
}

/// Packed indices at a title address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Indices53 {
    pub at: u64,
    pub blocks: u32,
    pub records: u32,
}

impl Indices53 {
    pub fn read(memory: &dyn Memory, at: u64) -> Option<Indices53> {
        let (blocks, records) = layout::header(&memory.bytes(at, 8)?)?;
        Some(Indices53 {
            at,
            blocks,
            records,
        })
    }

    /// The address of triangle `t`'s record.
    pub fn record_at(&self, t: u32) -> u64 {
        self.at + HEADER + BLOCK * u64::from(self.blocks) + 4 * u64::from(t)
    }

    /// The address of the 16-bit index `k` (from the block bases on).
    pub fn index_at(&self, k: u32) -> u64 {
        self.record_at(self.records) + 2 * u64::from(k)
    }

    /// Triangle `t`'s three vertex indices, the first and third swapped
    /// with `flip` ([`FLIP`]). `None`: not readable.
    pub fn triangle(&self, memory: &dyn Memory, t: u32, flip: bool) -> Option<[u32; 3]> {
        let w = u32_le(&memory.bytes(self.record_at(t), 4)?, 0);
        let (block, corners) = record(w);
        let e = memory.bytes(self.at + HEADER + BLOCK * u64::from(block), 8)?;
        let base = block_base(u32_le(&e, 0), u32_le(&e, 4));
        let mut out = [0u32; 3];
        for (o, l) in out.iter_mut().zip(corners) {
            let b = memory.bytes(self.index_at(base + l), 2)?;
            *o = u32::from(u16::from_le_bytes([b[0], b[1]]));
        }
        if flip {
            out.swap(0, 2);
        }
        Some(out)
    }
}

/// How quantized positions become floats: kept by the title in the 32
/// bytes before the vertices (offsets x, y, z and the axis order at -0x20,
/// scales x, y, z at -0x10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantization {
    pub offset: [f32; 3],
    pub order: u32,
    pub scale: [f32; 3],
}

impl Quantization {
    pub fn read(memory: &dyn Memory, vertices: u64) -> Option<Quantization> {
        let b = memory.bytes(vertices.checked_sub(layout::QUANTIZATION)?, 0x20)?;
        let layout::Quantization {
            offset,
            order,
            scale,
        } = layout::Quantization::parse(&b)?;
        Some(Quantization {
            offset,
            order,
            scale,
        })
    }

    /// A vertex's position from its two dwords: each field scaled and
    /// offset by its own pair, then put in the axis order (0: the third
    /// field is x and the first z; 1: the second is x, the third y; any
    /// other: in field order).
    pub fn position(&self, d0: u32, d1: u32) -> [f32; 3] {
        let &Quantization {
            offset,
            order,
            scale,
        } = self;
        layout::Quantization {
            offset,
            order,
            scale,
        }
        .position(d0, d1)
    }

    /// Vertex `i` of a buffer at `vertices` with `stride` (its bit 15 and
    /// above dropped, as the stages do).
    pub fn vertex(
        &self,
        memory: &dyn Memory,
        vertices: u64,
        stride: u64,
        i: u32,
    ) -> Option<[f32; 3]> {
        let b = memory.bytes(vertices + (stride & STRIDE) * u64::from(i), 8)?;
        Some(self.position(u32_le(&b, 0), u32_le(&b, 4)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Fake(BTreeMap<u64, Vec<u8>>);

    impl Memory for Fake {
        fn bytes(&self, at: u64, len: usize) -> Option<Vec<u8>> {
            let (&start, run) = self.0.range(..=at).next_back()?;
            let from = (at - start) as usize;
            run.get(from..from + len).map(<[u8]>::to_vec)
        }
    }

    fn put(b: &mut Vec<u8>, w: u32) {
        b.extend_from_slice(&w.to_le_bytes());
    }

    /// Packed indices of two blocks and three triangles, laid out as the
    /// stages read them.
    fn packed() -> Vec<u8> {
        let mut b = Vec::new();
        // The header: 2 blocks (bit 17 and above are not the count), 3
        // records, a third dword the stages do not read here.
        put(&mut b, 2 | 1 << 20);
        put(&mut b, 3);
        put(&mut b, 0xdead_beef);
        // Block 0 begins at index 0; block 1 at 0x10002: bit 14 of its
        // first dword is bit 16 of its base.
        for (e0, e1) in [(0, 0), (1 << 14, 0xabcd_0002)] {
            put(&mut b, e0);
            put(&mut b, e1);
            put(&mut b, 0);
            put(&mut b, 0);
        }
        // Triangles: block 0 offsets 0, 1, 2; block 0 offsets 5, 3, 127;
        // block 1 offsets 0, 1, 2.
        let rec = |block: u32, l: [u32; 3]| block << 21 | l[2] << 14 | l[1] << 7 | l[0];
        put(&mut b, rec(0, [0, 1, 2]));
        put(&mut b, rec(0, [5, 3, 127]));
        put(&mut b, rec(1, [0, 1, 2]));
        // The 16-bit indices: 0x10002 + 3 of them for block 1.
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
    fn a_record_splits_into_its_block_and_three_offsets() {
        assert_eq!(
            record(0xffe0_0000 | 3 << 14 | 2 << 7 | 1),
            (0x7ff, [1, 2, 3])
        );
        assert_eq!(record(0x7f | 0x7f << 7 | 0x7f << 14), (0, [127, 127, 127]));
        assert_eq!(block_base(1 << 14, 0xffff_1234), 0x1_1234);
        assert_eq!(block_base(!(1 << 14), 0), 0);
    }

    #[test]
    fn packed_indices_decode_as_the_stages_read_them() {
        let mut memory = Fake::default();
        memory.0.insert(0x10_0000, packed());
        let p = Indices53::read(&memory, 0x10_0000).unwrap();
        assert_eq!((p.blocks, p.records), (2, 3));
        assert_eq!(p.record_at(0), 0x10_0000 + 12 + 32);
        assert_eq!(p.triangle(&memory, 0, false), Some([1000, 1001, 1002]));
        assert_eq!(p.triangle(&memory, 1, false), Some([1005, 1003, 1127]));
        assert_eq!(p.triangle(&memory, 2, false), Some([7, 8, 9]));
        // The stride's bit 15 swaps the first and third corner.
        assert_eq!(p.triangle(&memory, 2, true), Some([9, 8, 7]));
        // Past what memory holds: not readable.
        assert_eq!(p.triangle(&memory, 1 << 20, false), None);
    }

    #[test]
    fn quantized_positions_decode_with_their_scale_offset_and_order() {
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
        put(&mut b, d0);
        put(&mut b, d1);
        put(&mut b, 0x0000_0100);
        put(&mut b, 0x0003_0003);
        let mut memory = Fake::default();
        memory.0.insert(0x2000 - 0x20, b);
        let q = Quantization::read(&memory, 0x2000).unwrap();
        assert_eq!(q.order, 2);
        let p = [
            0.5 * 0x12_3456 as f32 + 10.0,
            0.25 * 0xab_cdef as f32 + 20.0,
            2.0 * 0x8001 as f32 + 30.0,
        ];
        // Order 2 (any but 0 and 1): the fields in their order.
        assert_eq!(q.vertex(&memory, 0x2000, 8, 0), Some(p));
        // The second vertex: fields 1, 0x300, 3 (stride 8, bit 15 dropped).
        let one = [0.5 + 10.0, 0.25 * 0x300 as f32 + 20.0, 2.0 * 3.0 + 30.0];
        assert_eq!(q.vertex(&memory, 0x2000, 8 | FLIP, 1), Some(one));
        // Order 0: the third field is x, the first z; order 1: the second x.
        let zero = Quantization { order: 0, ..q };
        assert_eq!(zero.position(d0, d1), [p[2], p[1], p[0]]);
        let first = Quantization { order: 1, ..q };
        assert_eq!(first.position(d0, d1), [p[1], p[2], p[0]]);
        // Nothing before address 0x20.
        assert_eq!(Quantization::read(&memory, 0x10), None);
    }

    /// What the samples here decode through title memory is what the
    /// host's expansion makes of the same bytes (`victory_gpu::dxr::packed`,
    /// `VICTORY_DXR_EXPAND`): the same triangles, flipped or not, and the
    /// same positions, in every axis order.
    #[test]
    fn the_samples_and_the_hosts_expansion_read_the_same() {
        let data = packed();
        let mut memory = Fake::default();
        memory.0.insert(0x10_0000, data.clone());
        let p = Indices53::read(&memory, 0x10_0000).unwrap();
        assert_eq!(layout::indices_span(&data, 3), Some(data.len() as u64));
        let mut out = vec![0u32; 9];
        for flip in [false, true] {
            let odd = layout::expand_indices(&data, 3, flip, 2000, &mut out);
            assert_eq!(odd, layout::Odd::default());
            for t in 0..3u32 {
                let k = 3 * t as usize;
                assert_eq!(
                    p.triangle(&memory, t, flip).map(|v| v.to_vec()),
                    Some(out[k..k + 3].to_vec())
                );
            }
        }
        for order in [0u32, 1, 2] {
            let mut b = Vec::new();
            for w in [1.5f32, -2.0, 7.0] {
                put(&mut b, w.to_bits());
            }
            put(&mut b, order);
            for w in [0.125f32, 0.5, 3.0] {
                put(&mut b, w.to_bits());
            }
            put(&mut b, 0);
            for (d0, d1) in [
                (0x1234_56ef, 0x8001_abcd),
                (0xffff_ffff, 0xffff_ffff),
                (0, 0),
            ] {
                put(&mut b, d0);
                put(&mut b, d1);
            }
            memory.0.insert(0x40_0000 - 0x20, b.clone());
            let q = Quantization::read(&memory, 0x40_0000).unwrap();
            let mut expanded = vec![0f32; 9];
            let odd = layout::expand_positions(&b, 8 | FLIP, 3, &mut expanded);
            assert_eq!(odd, layout::Odd::default());
            for i in 0..3u32 {
                let k = 3 * i as usize;
                let v = q.vertex(&memory, 0x40_0000, 8 | FLIP, i).unwrap();
                assert_eq!(v.to_vec(), expanded[k..k + 3].to_vec());
            }
        }
    }
}
