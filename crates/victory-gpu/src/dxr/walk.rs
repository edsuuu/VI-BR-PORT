//! A walk of the **console's** structures on the CPU, for the probe to fire
//! the same rays at them as at the host's (`super::probe`) and compare ray by
//! ray: whether the host's structure answers what the title's programs
//! would find in the console's.
//!
//! The node test is the console instruction's own (`victory_shader::bvh`);
//! the rules around it are what the title's walks do, read in their code
//! (`CS_PPR_Raytrace`, the per-lane form; the ray-tracing work's notes
//! `nativo-programas.md` 2.1):
//!
//! - a structure's header holds its root node's pointer at `+48`; a pointer
//!   is a kind in its low three bits and the node at `base + (pointer >> 3)
//!   * 64`;
//! - an instance node (kind 6) holds the instance id and mask at `+48`, the
//!   hit group contribution and the instance flags at `+52`, the bottom
//!   level's root pointer at `+56` (30 bits; `0x3fffffff`: none) and the
//!   bottom level's address in 256-byte units at `+60`, the instance's index
//!   in the top level's descriptions at `+112`; the ray goes into the
//!   bottom level's own space by the 3x3 at `+76..+111` (rows of three) and
//!   the translation at `+64`, added after it: `o' = M o + T`, `d' = M d`,
//!   where a pointer at `+56` with its top bit set has no `M` (the identity);
//! - a triangle node keeps two triangles: the geometry index above the
//!   geometry flags at `+48`, the primitive index of triangle 1 at `+56` and
//!   of every other kind (0, 2, 3) at `+52`, the triangle's vertex roles in
//!   byte `k` of `+60`; of the flags byte, `0x80` marks a pair (a pointer to
//!   triangle 0 stands for triangle 1 as well, which the walk then meets in
//!   the same node) and `0x40` says triangle 1 is stored with its winding
//!   turned; neither is a Direct3D geometry flag, and the hit's flags are
//!   without them;
//! - a bottom level's header keeps its frame at `+60` (the critic's item 3);
//!   the walks do not read it, the instance node's turn already takes the
//!   ray there [inf], and the walk here only hands it on with each hit.
//!
//! The walk takes every triangle the ray meets (no culling, no judging, as
//! the probe's ray queries do) and keeps the nearest. The bottom level's own
//! space is rotated and scaled against the title's object space (the
//! critic's item 3), so only what does not depend on the space is compared:
//! the distance along the world ray (the direction goes through `M` without
//! being normalized), the barycentrics, and the ids.

use victory_shader::bvh;

/// Bytes of title memory, or the page (its address) that is not there yet.
pub(crate) trait Pages {
    fn read(&self, address: u64, out: &mut [u8]) -> Result<(), u64>;
}

/// What the walk found for one ray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Walked {
    Hit(Hit),
    Miss,
    /// A page the walk needs is not read yet.
    Missing(u64),
    /// The structure is not what the walk takes it for.
    Broken(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hit {
    pub t: f32,
    /// The weights of the input triangle's second and third vertices.
    pub barycentrics: [f32; 2],
    pub id: u32,
    pub contribution: u32,
    pub instance_flags: u32,
    pub geometry: u32,
    pub geometry_flags: u32,
    pub primitive: u32,
    /// The denominator's sign (positive: clockwise from the origin), and the
    /// triangle's roles byte (its winding bits).
    pub clockwise: bool,
    pub roles: u32,
    /// The instance node's address, and the index its `+112` keeps: the
    /// instance's place in the descriptions the top level was built from
    /// (what a program reads as `InstanceIndex`).
    pub instance: u64,
    pub index: u32,
    /// The 3x3 (rows of three) the instance node turned the ray by into the
    /// bottom level's own space; `None`: it has none (the identity).
    pub turn: Option<[f32; 9]>,
    /// The word at `+60` of the bottom level's header, which holds its
    /// frame (the critic's item 3: an index into the builder's 64 rotations,
    /// steps of 22.5 degrees about each axis, and a scale of each axis), as
    /// it is.
    pub frame_word: u32,
}

/// The mask byte of an instance, its id and the rest of its node.
struct Instance {
    id: u32,
    index: u32,
    contribution: u32,
    flags: u32,
    address: u64,
    origin: [f32; 3],
    direction: [f32; 3],
    turn: Option<[f32; 9]>,
    frame_word: u32,
}

/// Nodes walked at most a ray.
const STEPS: usize = 1 << 20;

/// The console's own bits of a triangle node's flags byte (`+48`): the
/// node holds a pair (triangle 1 is met through triangle 0's pointer), and
/// triangle 1's winding is stored turned.
const PAIR: u32 = 0x80;
const TURNED_SECOND: u32 = 0x40;

fn words<const N: usize>(pages: &dyn Pages, address: u64) -> Result<[u32; N], u64> {
    let mut bytes = vec![0u8; 4 * N];
    pages.read(address, &mut bytes)?;
    Ok(std::array::from_fn(|i| u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap_or_default())))
}

fn node(base: u64, pointer: u32) -> u64 {
    base.wrapping_add(u64::from(pointer >> 3) << 6)
}

/// The nearest hit of the ray `origin + t direction`, `t_min <= t < t_max`,
/// in the console's top level at `top`, through instances whose mask meets
/// `mask`.
pub(crate) fn walk(pages: &dyn Pages, top: u64, origin: [f32; 3], direction: [f32; 3], t_min: f32, t_max: f32, mask: u32) -> Walked {
    match walk_inner(pages, top, origin, direction, t_min, t_max, mask) {
        Ok(Some(hit)) => Walked::Hit(hit),
        Ok(None) => Walked::Miss,
        Err(Err(page)) => Walked::Missing(page),
        Err(Ok(why)) => Walked::Broken(why),
    }
}

type Failed = Result<&'static str, u64>;

fn walk_inner(
    pages: &dyn Pages,
    top: u64,
    origin: [f32; 3],
    direction: [f32; 3],
    t_min: f32,
    t_max: f32,
    mask: u32,
) -> Result<Option<Hit>, Failed> {
    let [root] = words::<1>(pages, top + 48).map_err(Err)?;
    // (base, pointer, the instance whose bottom level it is in)
    let mut stack: Vec<(u64, u32, Option<std::rc::Rc<Instance>>)> = vec![(top, root, None)];
    let mut best: Option<Hit> = None;
    let mut steps = 0;
    while let Some((base, pointer, instance)) = stack.pop() {
        steps += 1;
        if steps > STEPS {
            return Err(Ok("too many nodes for one ray"));
        }
        let at = node(base, pointer);
        let (o, d) = instance.as_ref().map_or((origin, direction), |i| (i.origin, i.direction));
        let far = best.map_or(t_max, |b| b.t);
        match pointer & 7 {
            kind @ (4 | 5) => {
                let w = words::<28>(pages, at).map_err(Err)?;
                let ray = bvh::Ray::new(o, d, far);
                for child in bvh::intersect(&w, kind, &ray, false) {
                    if child != bvh::NONE {
                        stack.push((base, child, instance.clone()));
                    }
                }
            }
            k @ 0..=3 => {
                let Some(inst) = instance.as_ref() else {
                    return Err(Ok("a triangle in the top level"));
                };
                let w = words::<28>(pages, at).map_err(Err)?;
                if k == 0 && w[12] & PAIR != 0 {
                    stack.push((base, pointer | 1, instance.clone()));
                }
                let ray = bvh::Ray::new(o, d, far);
                let [num, den, i, j] = bvh::triangle(&bvh::triangle_vertices(&w, k), &ray);
                if num == f32::INFINITY {
                    continue;
                }
                let t = num / den;
                if !(t >= t_min && t < far) {
                    continue;
                }
                best = Some(Hit {
                    t,
                    barycentrics: [i / den, j / den],
                    id: inst.id,
                    contribution: inst.contribution,
                    instance_flags: inst.flags,
                    geometry: w[12] >> 8,
                    geometry_flags: w[12] & 0xff & !(PAIR | TURNED_SECOND),
                    primitive: w[13 + usize::from(k == 1)],
                    clockwise: den > 0.0,
                    roles: w[15] >> (8 * k) & 0xff,
                    instance: inst.address,
                    index: inst.index,
                    turn: inst.turn,
                    frame_word: inst.frame_word,
                });
            }
            6 => {
                if instance.is_some() {
                    return Err(Ok("an instance inside a bottom level"));
                }
                let w = words::<32>(pages, at).map_err(Err)?;
                if (w[12] >> 24) & mask == 0 {
                    continue;
                }
                let blas_root = w[14] & 0x3fff_ffff;
                if blas_root == 0x3fff_ffff {
                    continue;
                }
                let f = |k: usize| f32::from_bits(w[k]);
                let translation = [f(16), f(17), f(18)];
                let turn: Option<[f32; 9]> = ((w[14] as i32) >= 0).then(|| std::array::from_fn(|k| f(19 + k)));
                let apply = |v: [f32; 3]| -> [f32; 3] {
                    let Some(m) = turn else { return v };
                    std::array::from_fn(|r| m[3 * r] * v[0] + m[3 * r + 1] * v[1] + m[3 * r + 2] * v[2])
                };
                let mut o2 = apply(o);
                for (c, t) in o2.iter_mut().zip(translation) {
                    *c += t;
                }
                let base = u64::from(w[15]) << 8;
                let [frame_word] = words::<1>(pages, base + 60).map_err(Err)?;
                let inst = Instance {
                    id: w[12] & 0xff_ffff,
                    index: w[28],
                    contribution: w[13] & 0xff_ffff,
                    flags: w[13] >> 24,
                    address: at,
                    origin: o2,
                    direction: apply(d),
                    turn,
                    frame_word,
                };
                stack.push((base, blas_root, Some(std::rc::Rc::new(inst))));
            }
            // Procedural boxes: the program's own code judges them.
            _ => {}
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Memory of the test's own, in pages of 4 KiB that may be held back.
    #[derive(Default)]
    struct Fake {
        bytes: BTreeMap<u64, u8>,
        absent: Option<u64>,
    }

    impl Fake {
        fn put(&mut self, at: u64, words: &[u32]) {
            for (i, w) in words.iter().enumerate() {
                for (k, b) in w.to_le_bytes().into_iter().enumerate() {
                    self.bytes.insert(at + 4 * i as u64 + k as u64, b);
                }
            }
        }
    }

    impl Pages for Fake {
        fn read(&self, address: u64, out: &mut [u8]) -> Result<(), u64> {
            for (i, o) in out.iter_mut().enumerate() {
                let a = address + i as u64;
                if self.absent == Some(a & !0xfff) {
                    return Err(a & !0xfff);
                }
                *o = self.bytes.get(&a).copied().unwrap_or(0);
            }
            Ok(())
        }
    }

    fn bits(v: &[f32]) -> Vec<u32> {
        v.iter().map(|f| f.to_bits()).collect()
    }

    const TOP: u64 = 0x10_0000;
    const BLAS: u64 = 0x20_0000;

    /// A top level of one 32-bit box node over two instance nodes of the
    /// same bottom level: one where it is, one turned (x and y swapped,
    /// scaled by 2) and moved, so the walk works in two spaces. The bottom
    /// level: one triangle node with two triangles in the plane z = 5 of its
    /// own space.
    fn scene() -> Fake {
        let mut m = Fake::default();
        // Top header: the root pointer (a box32 at node 1).
        m.put(TOP + 48, &[1 << 3 | 5]);
        let wide = [-100.0, -100.0, -100.0, 100.0, 100.0, 100.0];
        let off = [f32::NAN; 6];
        let mut root = vec![2 << 3 | 6, 4 << 3 | 6, bvh::NONE, bvh::NONE];
        root.extend(bits(&wide));
        root.extend(bits(&wide));
        root.extend(bits(&off));
        root.extend(bits(&off));
        m.put(TOP + 64, &root);
        // Instance 0 (node 2): id 7, mask 0x01, contribution 3, no turn.
        let mut i0 = vec![0u32; 28];
        i0[12] = 7 | 0x01 << 24;
        i0[13] = 3;
        i0[14] = 1 << 31 | (2 << 3 | 5);
        i0[15] = (BLAS >> 8) as u32;
        // Translation (0, 0, 0); its index 4 at +112.
        i0.extend([4, 0, 0, 0]);
        m.put(TOP + 2 * 64, &i0);
        // Instance 1 (node 4): id 9, mask 0x02, contribution 5; turned:
        // x' = 2 y, y' = 2 x, z' = 2 z - 10 (so z' = 5 at z = 7.5).
        let mut i1 = vec![0u32; 28];
        i1[12] = 9 | 0x02 << 24;
        i1[13] = 5 | 0x01 << 24;
        i1[14] = 2 << 3 | 5;
        i1[15] = (BLAS >> 8) as u32;
        i1[16..19].copy_from_slice(&bits(&[0.0, 0.0, -10.0]));
        i1[19..28].copy_from_slice(&bits(&[0.0, 2.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0]));
        m.put(TOP + 4 * 64, &i1);
        // The bottom level's triangle node (node 1): vertices 0-3, the
        // geometry 2 with flags 1 at +48, primitives 11 and 12.
        let v = [[0.0, 0.0, 5.0], [4.0, 0.0, 5.0], [0.0, 4.0, 5.0], [4.0, 4.0, 5.0]];
        let mut t = vec![0u32; 16];
        for (n, p) in v.iter().enumerate() {
            t[3 * n..3 * n + 3].copy_from_slice(&bits(p));
        }
        t[12] = 2 << 8 | 1;
        t[13] = 11;
        t[14] = 12;
        // Triangle 0: roles I = 1, J = 2; triangle 1 (node vertices 1, 3, 2): the same.
        t[15] = (1 | 2 << 2) | (1 | 2 << 2) << 8;
        m.put(BLAS + 64, &t);
        // Its header's frame word.
        m.put(BLAS + 60, &[0x0a7f_7f7f]);
        // Its root (node 2): a box node naming triangles 0 and 1 of node 1.
        let around = [-1.0, -1.0, 4.0, 5.0, 5.0, 6.0];
        let mut b = vec![1 << 3, 1 << 3 | 1, bvh::NONE, bvh::NONE];
        b.extend(bits(&around));
        b.extend(bits(&around));
        b.extend(bits(&off));
        b.extend(bits(&off));
        m.put(BLAS + 2 * 64, &b);
        m
    }

    #[test]
    fn the_walk_takes_the_nearest_triangle_through_the_instances_spaces() {
        let m = scene();
        // Down +z at (1, 1): instance 0's triangle 0 at z = 5, t = 5; the
        // turned one is at z = 7.5, further.
        let Walked::Hit(h) = walk(&m, TOP, [1.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0xff) else { panic!() };
        assert_eq!((h.t, h.id, h.contribution, h.geometry, h.geometry_flags, h.primitive), (5.0, 7, 3, 2, 1, 11));
        assert!((h.barycentrics[0] - 0.25).abs() < 1e-6 && (h.barycentrics[1] - 0.25).abs() < 1e-6);
        assert_eq!((h.instance, h.index), (TOP + 2 * 64, 4));
        assert_eq!((h.turn, h.frame_word), (None, 0x0a7f_7f7f));
        // Mask 2 only: the turned instance, met at z = 7.5 (t = 7.5 along the
        // world direction although its space is scaled by 2). At (0.5, 0.5)
        // its space has (1, 1): triangle 0 again.
        let Walked::Hit(h) = walk(&m, TOP, [0.5, 0.5, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0x02) else { panic!() };
        assert_eq!((h.id, h.contribution, h.instance_flags, h.primitive), (9, 5, 1, 11));
        assert!((h.t - 7.5).abs() < 1e-5, "{}", h.t);
        assert_eq!(h.turn, Some([0.0, 2.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0]));
        // At (1.75, 0.5) world, the turned space has (1, 3.5): x + y > 4 is
        // triangle 1 (primitive 12).
        let Walked::Hit(h) = walk(&m, TOP, [1.75, 0.5, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0x02) else { panic!() };
        assert_eq!(h.primitive, 12);
        // Too short to reach either, or beside both.
        assert_eq!(walk(&m, TOP, [1.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0.0, 4.0, 0xff), Walked::Miss);
        assert_eq!(walk(&m, TOP, [50.0, 50.0, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0xff), Walked::Miss);
        // Starting past the first: the second.
        let Walked::Hit(h) = walk(&m, TOP, [1.0, 1.0, 0.0], [0.0, 0.0, 1.0], 6.0, 100.0, 0xff) else { panic!() };
        assert_eq!(h.id, 9);
    }

    #[test]
    fn kinds_two_and_three_take_the_first_primitive_index() {
        let mut m = scene();
        // The node's fifth vertex of kinds 2 and 3 is its words 12-14 read as
        // floats: (4, 2, 5), so the geometry word, primitive 2.0's bits at +52
        // and 5.0's at +56.
        m.put(BLAS + 64 + 48, &bits(&[4.0, 2.0, 5.0]));
        // Every triangle's roles I = 1, J = 2.
        m.put(BLAS + 64 + 60, &[0x0909_0909]);
        let around = [-1.0, -1.0, 4.0, 5.0, 5.0, 6.0];
        let off = [f32::NAN; 6];
        let mut b = vec![1 << 3 | 2, 1 << 3 | 3, bvh::NONE, bvh::NONE];
        b.extend(bits(&around));
        b.extend(bits(&around));
        b.extend(bits(&off));
        b.extend(bits(&off));
        m.put(BLAS + 2 * 64, &b);
        // Kind 2 (node vertices 2, 3, 4) at (3, 3.5); kind 3 (2, 4, 0) at (1, 2).
        for at in [[3.0, 3.5, 0.0], [1.0, 2.0, 0.0]] {
            let Walked::Hit(h) = walk(&m, TOP, at, [0.0, 0.0, 1.0], 0.0, 100.0, 0x01) else { panic!("{at:?}") };
            assert_eq!((h.t, h.primitive), (5.0, 2f32.to_bits()), "{at:?}");
        }
    }

    #[test]
    fn a_pointer_to_triangle_zero_of_a_pair_meets_both() {
        let mut m = scene();
        // The box names triangle 0 only.
        let around = [-1.0, -1.0, 4.0, 5.0, 5.0, 6.0];
        let off = [f32::NAN; 6];
        let mut b = vec![1 << 3, bvh::NONE, bvh::NONE, bvh::NONE];
        b.extend(bits(&around));
        b.extend(bits(&around));
        b.extend(bits(&off));
        b.extend(bits(&off));
        m.put(BLAS + 2 * 64, &b);
        // Triangle 1's side (x + y > 4): nothing without the pair bit.
        let ray = |m: &Fake| walk(m, TOP, [3.0, 3.0, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0x01);
        assert_eq!(ray(&m), Walked::Miss);
        // A pair whose second is stored turned: triangle 1 is met, and the
        // hit's flags are the geometry's alone.
        m.put(BLAS + 64 + 48, &[2 << 8 | PAIR | TURNED_SECOND | 1]);
        let Walked::Hit(h) = ray(&m) else { panic!() };
        assert_eq!((h.t, h.geometry, h.geometry_flags, h.primitive), (5.0, 2, 1, 12));
        // Triangle 0's side still finds triangle 0.
        let Walked::Hit(h) = walk(&m, TOP, [1.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0x01) else { panic!() };
        assert_eq!(h.primitive, 11);
    }

    #[test]
    fn a_page_not_read_yet_is_named() {
        let mut m = scene();
        m.absent = Some(BLAS);
        assert_eq!(walk(&m, TOP, [1.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0xff), Walked::Missing(BLAS));
        // A ray that meets no instance never needs it.
        assert_eq!(walk(&m, TOP, [500.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0xff), Walked::Miss);
    }
}
