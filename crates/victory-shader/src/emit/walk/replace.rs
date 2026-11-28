//! A recognized walk taken out of the program before anything else sees it
//! (`VICTORY_RT_QUERY`): the region becomes one instruction, the node test at
//! its site, which reads the ray as it was when the region was entered and
//! writes the values the region handed to the code after it. The lowering
//! makes a host ray query of that instruction (`query.rs`).
//!
//! It happens before the structure, the resources and the wave forms are
//! worked out (`emit::emit`), so all of them see the program without its
//! walk: a console wave64 program whose walk kept it in a costly or
//! approximate form is judged anew without it (the native design's critic,
//! item 8).
//!
//! Only what the census calls fit is taken out (family A0, nothing refused),
//! and only where every value the region hands on is one a query gives: the
//! hit distance, whether something was hit, a constant, or the value the
//! register held when the region was entered. Values of the region that the
//! census finds no lane reads after it (temporaries a later walk overwrites
//! before it reads them) are left as the register was at the entry.

use std::collections::{HashMap, HashSet};

use super::recognize::{Bound, ExitKind, Family, Mask, Role, Walk, recognize};
use crate::gfx10::{Fields, Operand};
use crate::ir::{EXEC, Exit, Inst, Program, Reg, Value};
use crate::structure::Structured;

/// `RAY_FLAG_FORCE_OPAQUE`: the walks taken out run no code of the program
/// for any triangle, and accept every one in reach.
pub(crate) const FORCE_OPAQUE: u32 = 0x1;
/// `RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH`.
pub(crate) const FIRST_HIT: u32 = 0x4;
/// `RAY_FLAG_CULL_BACK_FACING_TRIANGLES`.
pub(crate) const CULL_BACK: u32 = 0x10;
/// `RAY_FLAG_CULL_FRONT_FACING_TRIANGLES`.
pub(crate) const CULL_FRONT: u32 = 0x20;
/// `RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES`: the walks step over node kind 7.
pub(crate) const SKIP_PROCEDURAL: u32 = 0x200;

/// What the lowering needs to make a host ray query of a site that stands
/// for a region. It names registers, which the site reads with their values
/// at the region's entry, and not the values themselves: the wave forms
/// rebuild a program's values (`ir::without_skips`), its registers stay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Query {
    /// The site's byte offset: the instruction that now stands for the region.
    pub offset: usize,
    /// The query's `RAY_FLAG`s.
    pub flags: u32,
    /// The instance inclusion mask.
    pub mask: u8,
    pub t_min: TMin,
    /// The ray at the region's entry.
    pub extent: Reg,
    pub origin: [Reg; 3],
    pub direction: [Reg; 3],
    /// Byte offset of the scalar load of the structure's view descriptor,
    /// which says where the host finds the structure the title meant.
    pub descriptor_load: usize,
    pub outputs: Vec<Output>,
}

/// The ray's t min: a constant (float bits), or what a register held at the
/// region's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TMin {
    Const(u32),
    Reg(Reg),
}

/// A register the region handed a value on in, now written by the query;
/// its value at the region's entry is what a miss leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Output {
    pub reg: Reg,
    pub kind: OutKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutKind {
    /// The committed hit's t; a miss leaves the entry value.
    T,
    /// This node pointer at a hit, a triangle's (kind 1 to 3) other than the
    /// miss value: the code after the walk only tells a hit from a miss and
    /// a triangle from the procedural kind with it (the census checks);
    /// a miss leaves the entry value.
    Hit(u32),
    /// The entry value.
    Entry,
    Constant(u32),
}

/// `p` with the walks of `families` that fit taken out, and the queries that
/// stand for them; `None` when none is.
pub(crate) fn replace(p: &Program, st: &Structured, families: &[Family]) -> Option<(Program, Vec<Query>)> {
    let mut q = p.clone();
    let mut own: Option<Structured> = None;
    let mut queries = Vec::new();
    let mut tried: HashSet<usize> = HashSet::new();
    loop {
        let st = own.as_ref().unwrap_or(st);
        let walks = recognize(&q, Some(st));
        let Some(w) = walks
            .into_iter()
            .find(|w| w.fits() && families.contains(&w.family) && !tried.contains(&w.site_offsets[0]))
        else {
            break;
        };
        tried.insert(w.site_offsets[0]);
        let Ok((next, query)) = collapse(&q, &w) else {
            continue;
        };
        // (each later walk is recognized in the program without the earlier ones)
        let Ok(s) = crate::structure::structure(&next) else {
            break;
        };
        q = next;
        own = Some(s);
        queries.push(query);
    }
    (!queries.is_empty()).then_some((q, queries))
}

/// The value register `r` holds when the region of `w` is entered: its phi
/// at the header, or the value from before the region that the region reads.
fn entry(p: &Program, w: &Walk, defs: &HashMap<Value, usize>, r: Reg) -> Option<Value> {
    if let Some(phi) = p.blocks[w.header].phis.iter().find(|ph| ph.reg == r) {
        return Some(phi.value);
    }
    for (b, block) in p.blocks.iter().enumerate() {
        if !w.body[b] {
            continue;
        }
        for inst in &block.insts {
            if let Some(v) = inst.read(r)
                && defs.get(&v).is_none_or(|&d| !w.body[d])
            {
                return Some(v);
            }
        }
    }
    None
}

/// `p` with the region of `w` replaced by its site, and the query.
pub(crate) fn collapse(p: &Program, w: &Walk) -> Result<(Program, Query), &'static str> {
    let &[(exiting, x)] = &w.exits[..] else {
        return Err("a region with more than one edge out");
    };
    let &[site] = &w.sites[..] else {
        return Err("a region of more than one site");
    };
    let h = w.header;
    let inst = &p.blocks[site.0].insts[site.1];
    let mut defs: HashMap<Value, usize> = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for phi in &block.phis {
            defs.insert(phi.value, b);
        }
        for i in &block.insts {
            for &(_, v) in &i.writes {
                defs.insert(v, b);
            }
        }
    }
    let at_entry = |r: Reg| entry(p, w, &defs, r);

    // The ray, as the site's operands hold it when the region is entered.
    let Fields::Image { addr, rsrc, .. } = &inst.fields else {
        return Err("a site that is no image instruction");
    };
    let mut regs = Vec::new();
    for a in addr {
        if let Operand::Vector { reg, dwords } = *a {
            regs.extend((0..dwords).map(|k| reg + k));
        }
    }
    let skip = if inst.name() == "image_bvh64_intersect_ray" { 2 } else { 1 };
    if regs.len() != skip + 10 {
        return Err("a site's operands");
    }
    let ray: Vec<Value> = regs[skip..skip + 7]
        .iter()
        .map(|&r| at_entry(Reg::V(r)))
        .collect::<Option<_>>()
        .ok_or("a ray operand made inside the region")?;

    // The descriptor's constant dwords (made inside the region): made again
    // in front of the site, so that resource tracking sees the site's
    // descriptor as it was.
    let Operand::Scalar { code, dwords: 4 } = *rsrc else {
        return Err("a site's descriptor");
    };
    let mut made: Vec<Inst> = Vec::new();
    let mut reads: Vec<(Reg, Value)> = Vec::new();
    for k in 0..4 {
        let r = Reg::S(code + k);
        let v = inst.read(r).ok_or("a site's descriptor dword")?;
        reads.push((r, v));
        let Some(&b) = defs.get(&v) else { continue };
        if !w.body[b] {
            continue;
        }
        let d = p.blocks[b]
            .insts
            .iter()
            .find(|i| i.write(r) == Some(v))
            .ok_or("a descriptor dword's write")?;
        let pure = matches!(d.name(), "s_mov_b32" | "s_brev_b32" | "s_movk_i32")
            && d.reads.iter().all(|&(_, x)| defs.get(&x).is_none_or(|&db| !w.body[db]))
            && d.writes.len() == 1;
        if !pure {
            return Err("a descriptor dword that is no constant");
        }
        if !made.contains(d) {
            made.push(d.clone());
        }
    }

    // Fresh values, for what the site now writes.
    let mut fresh = p
        .blocks
        .iter()
        .flat_map(|b| b.phis.iter().map(|ph| ph.value).chain(b.insts.iter().flat_map(|i| i.writes.iter().map(|w| w.1))))
        .chain(p.inputs.iter().map(|i| i.1))
        .map(|v| v.0)
        .max()
        .unwrap_or(0)
        + 1;
    let mut new_value = || {
        let v = Value(fresh);
        fresh += 1;
        v
    };

    // What the region hands on: each output a fresh value the site writes,
    // which the code after the region reads in place of the region's.
    let mut outputs = Vec::new();
    let mut swap: HashMap<Value, Value> = HashMap::new();
    let mut inputs = p.inputs.clone();
    for live in &w.outputs {
        let kind = match (&live.reg, &live.role) {
            // (the lanes that leave the walk are the lanes that entered it)
            (Reg::S(c), _) if *c == EXEC || *c == EXEC + 1 => {
                let e = at_entry(live.reg).ok_or("the lanes at the region's entry")?;
                swap.insert(live.value, e);
                continue;
            }
            (Reg::V(_), Some(Role::T)) => OutKind::T,
            (Reg::V(_), Some(Role::HitNode)) => {
                let miss = live.miss.ok_or("a hit node whose miss value is no constant")?;
                OutKind::Hit(if miss == 1 { 2 } else { 1 })
            }
            (Reg::V(_), Some(Role::Entry)) => OutKind::Entry,
            (Reg::V(_), Some(Role::Constant(k))) => OutKind::Constant(*k),
            _ => return Err("an output a query does not give"),
        };
        if outputs.iter().any(|o: &(Output, Value, Value)| o.0.reg == live.reg) {
            return Err("two outputs in one register");
        }
        let entry = match at_entry(live.reg) {
            Some(e) => e,
            None if matches!(kind, OutKind::Constant(_)) => {
                // (never read: a guarded write keeps it in lanes that do not run, which
                // read it no more than they did)
                let v = new_value();
                inputs.push((live.reg, v));
                v
            }
            None => return Err("an output register without a value at the entry"),
        };
        let value = new_value();
        swap.insert(live.value, value);
        outputs.push((Output { reg: live.reg, kind }, value, entry));
    }

    // The query.
    let mut flags = FORCE_OPAQUE | SKIP_PROCEDURAL;
    match w.exit {
        ExitKind::First => flags |= FIRST_HIT,
        ExitKind::Closest => {}
        ExitKind::Unknown => return Err("an exit kind not known"),
    }
    // The walk culls where the triangle's denominator times the instance's
    // facing times this factor is negative; the node test's denominator is
    // positive for a triangle whose vertices run clockwise from the ray's
    // origin (`intersect.rs`), DXR's front face, so 1 culls back faces.
    match w.cull {
        Some(0) => {}
        Some(1) => flags |= CULL_BACK,
        Some(-1) => flags |= CULL_FRONT,
        _ => return Err("face culling not known"),
    }
    let Mask::Constant(mask) = w.mask else {
        return Err("an instance mask that is no constant");
    };
    let (t_min, t_min_read) = match w.t_min {
        Some(Bound::Const(k)) => (TMin::Const(k), None),
        Some(Bound::Value(r, v)) => (TMin::Reg(r), Some((r, v))),
        None => return Err("t min not known"),
    };
    let query = Query {
        offset: inst.machine.offset,
        flags,
        mask,
        t_min,
        extent: Reg::V(regs[skip]),
        origin: [1, 2, 3].map(|k| Reg::V(regs[skip + k])),
        direction: [4, 5, 6].map(|k| Reg::V(regs[skip + k])),
        descriptor_load: w.descriptor_load.ok_or("the structure's descriptor not found")?,
        outputs: outputs.iter().map(|o| o.0).collect(),
    };

    // The site that stands for the region: it reads the ray, the descriptor,
    // the lanes and the old values of what it writes, and writes the outputs.
    let add = |r: Reg, v: Value, reads: &mut Vec<(Reg, Value)>| {
        if !reads.iter().any(|x| x.0 == r) {
            reads.push((r, v));
        }
    };
    let lanes = 1 + u8::from(p.wave64);
    for k in 0..lanes {
        let r = Reg::S(EXEC + k);
        add(r, at_entry(r).ok_or("the lanes at the region's entry")?, &mut reads);
    }
    for (n, &r) in regs[skip..skip + 7].iter().enumerate() {
        add(Reg::V(r), ray[n], &mut reads);
    }
    for (o, _, entry) in &outputs {
        if reads.iter().any(|x| x.0 == o.reg && x.1 != *entry) {
            return Err("an output in a register the site reads otherwise");
        }
        add(o.reg, *entry, &mut reads);
    }
    if let Some((r, v)) = t_min_read {
        if reads.iter().any(|x| x.0 == r && x.1 != v) {
            return Err("t min in a register the site reads otherwise");
        }
        add(r, v, &mut reads);
    }
    let stand = Inst {
        machine: inst.machine.clone(),
        fields: inst.fields.clone(),
        reads,
        writes: outputs.iter().map(|o| (o.0.reg, o.1)).collect(),
    };

    // The region's other values, which the census found no lane reads after
    // it: the register as it was at the entry where it had a value there, a
    // value from before the program otherwise.
    let header_writes: HashSet<Value> = p.blocks[h].insts.iter().flat_map(|i| i.writes.iter().map(|w| w.1)).collect();
    let mut gone: HashSet<Value> = p
        .blocks
        .iter()
        .enumerate()
        .filter(|(b, _)| w.body[*b] && *b != h)
        .flat_map(|(_, b)| b.phis.iter().map(|ph| ph.value).chain(b.insts.iter().flat_map(|i| i.writes.iter().map(|w| w.1))))
        .collect();
    gone.extend(header_writes);
    for m in &made {
        for &(_, v) in &m.writes {
            gone.remove(&v);
        }
    }
    let mut dead: HashMap<Reg, Value> = HashMap::new();
    let mut stand_in = |r: Reg, v: Value, swap: &HashMap<Value, Value>, inputs: &mut Vec<(Reg, Value)>| -> Value {
        if let Some(&e) = swap.get(&v) {
            return e;
        }
        if !gone.contains(&v) {
            return v;
        }
        if let Some(e) = at_entry(r) {
            return e;
        }
        *dead.entry(r).or_insert_with(|| {
            let n = new_value();
            inputs.push((r, n));
            n
        })
    };

    // The blocks: the header keeps its phis (their arguments from outside),
    // holds the descriptor's constants and the site, and goes to the exit;
    // the rest of the region goes.
    let n = p.blocks.len();
    let mut map = vec![usize::MAX; n];
    let mut kept = 0;
    for (b, m) in map.iter_mut().enumerate() {
        if !w.body[b] || b == h {
            *m = kept;
            kept += 1;
        }
    }
    let mut blocks = Vec::with_capacity(kept);
    for (b, block) in p.blocks.iter().enumerate() {
        if map[b] == usize::MAX {
            continue;
        }
        let mut block = block.clone();
        if b == h {
            let outside: Vec<usize> = (0..block.preds.len()).filter(|&k| !w.body[block.preds[k]]).collect();
            for phi in &mut block.phis {
                phi.args = outside.iter().map(|&k| phi.args[k]).collect();
            }
            block.preds = outside.iter().map(|&k| block.preds[k]).collect();
            block.insts = made.clone();
            block.insts.push(stand.clone());
            block.exit = Exit::Jump(x);
        } else {
            for q in &mut block.preds {
                if *q == exiting {
                    *q = h;
                }
            }
            for phi in &mut block.phis {
                let r = phi.reg;
                for a in &mut phi.args {
                    *a = stand_in(r, *a, &swap, &mut inputs);
                }
            }
            for i in &mut block.insts {
                for read in &mut i.reads {
                    read.1 = stand_in(read.0, read.1, &swap, &mut inputs);
                }
            }
        }
        if block.preds.iter().any(|&q| map[q] == usize::MAX) {
            return Err("a block outside the region entered from inside");
        }
        block.preds = block.preds.iter().map(|&q| map[q]).collect();
        block.exit = match block.exit {
            Exit::Jump(t) => Exit::Jump(map[t]),
            Exit::Branch { taken, not_taken } => Exit::Branch { taken: map[taken], not_taken: map[not_taken] },
            e => e,
        };
        blocks.push(block);
    }
    if blocks.iter().any(|b| b.exit.successors().contains(&usize::MAX)) {
        return Err("an edge into the region's body");
    }
    let program = Program { wave64: p.wave64, vgprs: p.vgprs, blocks, inputs };
    Ok((program, query))
}
