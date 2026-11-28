//! The node fields the code after a walk reads through the pointers a hit
//! saves, and whether a ray query of the host gives each of them.
//!
//! The title's GI walks (`CS_DeferredGather_*`) inline the console's own
//! traversal routine and, after it, the routine's accessors of the committed
//! hit (the GDK's `XDXRStandaloneTraverse*.hlsli`, read as reference): the
//! instance's index at +112 of the instance node; through the bottom level
//! the instance names at +60, the geometry index in the high 24 bits of +48
//! of the triangle node and the primitive index at +52, or +56 for the
//! second triangle of a pair (node kind 1). A query has each of those as its
//! committed state (`CommittedInstanceIndex`, `CommittedGeometryIndex`,
//! `CommittedPrimitiveIndex`), on a host structure built from the same
//! instance and geometry descriptions in the same order.
//!
//! Which field a load reads is found by evaluating its address with made-up
//! pointers, structure base and bottom-level word, and varying each: an
//! address that moves by 256 bytes when the node index moves by 4 and with
//! the structure's base is a field of an instance node; one that moves with
//! the word an instance's +60 load returned is a field of a triangle node of
//! that bottom level. The field is the address's low byte (every base is
//! 256-byte aligned and the made-up node indices are multiples of 4), tried
//! for each node kind. Anything else that the pointers reach is refused:
//! a store, a load of another field, a lane test or a branch made of them.

use std::collections::{HashMap, HashSet};

use super::{Arg, Def, Facts, Hop, Live, Role, Seen, Site, Use, compare, inline, is_load, is_store, plain, sources};
use super::super::cfg::Loops;
use crate::gfx10::{Fields, NULL, Operand};
use crate::ir::{EXEC, Reg, Value};

/// A field of the committed hit a load after the walk reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// +112 of the instance node: the instance's index.
    InstanceIndex,
    /// +60 of the instance node: the bottom level's address over 256, which
    /// the code only adds the triangle node's place to.
    Blas,
    /// +48 of the triangle node: the geometry index (high 24 bits) and the
    /// geometry's flags (low 8).
    GeometryWord,
    /// +52 of the triangle node, +56 for node kind 1: the primitive index.
    PrimitiveIndex,
}

impl Field {
    pub fn text(self) -> &'static str {
        match self {
            Field::InstanceIndex => "instance+112 (instance index)",
            Field::Blas => "instance+60 (bottom level)",
            Field::GeometryWord => "triangle+48 (geometry index)",
            Field::PrimitiveIndex => "triangle+52/+56 (primitive index)",
        }
    }
}

/// A load after the walk that reads a field of the hit's nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLoad {
    pub site: Site,
    /// Its byte offset.
    pub offset: usize,
    /// The VGPR it loads into (one dword).
    pub dst: u8,
    pub field: Field,
}

/// What a hit's node pointer points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Points {
    Instance,
    Triangle,
}

/// The made-up values an address is evaluated with.
#[derive(Clone)]
struct Env {
    /// The hit pointers (outputs of the walk) and the pointer each holds.
    hits: Vec<(Value, u32)>,
    /// The loads of an instance's +60 found so far and the word each returns.
    blas: HashMap<Site, u32>,
    /// The structure's view descriptor: the load's offset and its dwords.
    descriptor: Option<usize>,
    dwords: [u32; 8],
}

impl Env {
    fn pointer(n: u32, kind: u32) -> u32 {
        (n << 3) | kind
    }
}

/// The field loads the hit pointers among `outputs` (role `HitNode`) reach
/// after the region, or why they cannot all be made from a query. Empty when
/// no output is a hit pointer.
pub(super) fn fields(
    facts: &Facts,
    loops: &Loops,
    region: &[bool],
    outputs: &[Live],
    descriptor: Option<usize>,
    cache: &mut Seen,
) -> Result<Vec<FieldLoad>, String> {
    let hits: Vec<(Value, u32)> = outputs
        .iter()
        .filter(|l| l.role == Some(Role::HitNode) && matches!(l.reg, Reg::V(_)))
        .map(|l| (l.value, l.miss.unwrap_or(u32::MAX)))
        .collect();
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    if hits.iter().any(|h| h.1 == u32::MAX) {
        return Err("a hit pointer whose miss value is no constant".into());
    }
    let mut env = Env {
        hits: hits.iter().enumerate().map(|(j, &(v, _))| (v, Env::pointer(16 * (j as u32 + 1), 6))).collect(),
        blas: HashMap::new(),
        descriptor,
        dwords: [0x5678_9a00, 0x0000_0042, 0x8000_0000, 0x8900_03ff, 0, 0, 0, 0],
    };
    let mut found: HashMap<Site, Field> = HashMap::new();
    let mut points: HashMap<usize, Points> = HashMap::new();
    // (rounds: a bottom level's word found lets its triangle fields be read,
    // and may reach more loads)
    for round in 0.. {
        let loads = reached(facts, loops, region, &hits, &env.blas, cache)?;
        let mut progress = false;
        for &site in &loads {
            if found.contains_key(&site) {
                continue;
            }
            let Some((field, j, at)) = classify(facts, &env, site)? else { continue };
            if *points.entry(j).or_insert(at) != at {
                return Err("a hit pointer read both as an instance and as a triangle".into());
            }
            if field == Field::Blas {
                env.blas.insert(site, 0x0012_3400);
            }
            found.insert(site, field);
            progress = true;
        }
        let left = loads.iter().find(|s| !found.contains_key(s));
        match (left, progress && round < 8) {
            (_, true) => continue,
            (None, false) => break,
            (Some(&s), false) => {
                return Err(format!("a load at {:#x} of a node field not known", facts.inst(s).machine.offset));
            }
        }
    }
    if found.values().any(|&f| f != Field::Blas && f != Field::InstanceIndex)
        && !found.values().any(|&f| f == Field::Blas)
    {
        return Err("a triangle field without the instance's bottom level".into());
    }
    let mut out: Vec<FieldLoad> = found
        .into_iter()
        .map(|(site, field)| {
            let inst = facts.inst(site);
            let dst = match inst.fields {
                Fields::Flat { dst: Some(Operand::Vector { reg, .. }), .. } => reg,
                _ => 0,
            };
            FieldLoad { site, offset: inst.machine.offset, dst, field }
        })
        .collect();
    out.sort_by_key(|f| f.offset);
    Ok(out)
}

/// The loads whose address the hit pointers (and the words of the loads in
/// `blas`) reach after the region, following them through every VALU and
/// SALU instruction, the joins after the region, and the old value a guarded
/// write keeps (as [`Facts::after`] does). An error for any other use: a
/// store, a branch, the lanes, a load reading them otherwise than as its
/// address. A compare of a hit pointer itself with its miss value is a test
/// for a hit, which a query's marks answer the same.
fn reached(
    facts: &Facts,
    loops: &Loops,
    region: &[bool],
    hits: &[(Value, u32)],
    blas: &HashMap<Site, u32>,
    cache: &mut Seen,
) -> Result<Vec<Site>, String> {
    let mut loads: HashSet<Site> = HashSet::new();
    let mut seen = HashSet::new();
    // (the value, the miss value when it is a hit pointer itself, the last hop)
    let mut work: Vec<(Value, Option<u32>, Option<Hop>)> = hits.iter().map(|&(v, m)| (v, Some(m), None)).collect();
    for &site in blas.keys() {
        for &(_, w) in &facts.inst(site).writes {
            work.push((w, None, None));
        }
    }
    let mut steps = 0;
    while let Some((v, raw, hop)) = work.pop() {
        if !seen.insert((v, raw, hop)) {
            continue;
        }
        for &u in facts.uses.get(&v).map_or(&[][..], |u| &u[..]) {
            steps += 1;
            if steps > 20_000 {
                return Err("too many uses to follow".into());
            }
            match u {
                Use::Phi(b, k, a) => {
                    if !region[b] {
                        let hop = (!loops.is_header(b)).then_some(Hop::Edge(b, a));
                        work.push((facts.p.blocks[b].phis[k].value, raw, hop));
                    }
                }
                Use::Inst(b, i) => {
                    if region[b] {
                        continue;
                    }
                    if let Some(hop) = hop
                        && !facts.can_hold(hop, (b, i), loops, cache)
                    {
                        continue;
                    }
                    let inst = facts.inst((b, i));
                    let name = inst.name();
                    let at = inst.machine.offset;
                    // (the old value a guarded write keeps: passed on in its result)
                    let operands = crate::ir::operand_reads(facts.p, inst);
                    let old: Vec<Reg> = inst
                        .reads
                        .iter()
                        .filter(|&&(r, x)| x == v && matches!(r, Reg::V(_)) && !operands.contains(&r))
                        .map(|&(r, _)| r)
                        .collect();
                    if !old.is_empty() && inst.reads.iter().filter(|r| r.1 == v).count() == old.len() {
                        let hop = facts.guard((b, i)).map(|g| Hop::Kept(b, g));
                        work.extend(old.iter().filter_map(|&r| inst.write(r)).map(|w| (w, None, hop)));
                        continue;
                    }
                    if is_store(inst) {
                        return Err(format!("a node pointer reaches the store at {at:#x}"));
                    }
                    if is_load(inst) {
                        if !address_reads(inst).iter().any(|&r| inst.read(r) == Some(v)) {
                            return Err(format!("a node pointer reaches the load at {at:#x} otherwise than as its address"));
                        }
                        loads.insert((b, i));
                        continue;
                    }
                    if (name.starts_with("v_cmp") || name.starts_with("s_cmp"))
                        && let Some(miss) = raw
                        && tests_for_a_hit(inst, v, miss)
                    {
                        continue;
                    }
                    if !matches!(inst.fields, Fields::Valu(_) | Fields::Sop { .. }) || !plain(inst) {
                        return Err(format!("a node pointer reaches {name} at {at:#x}"));
                    }
                    for &(r, w) in &inst.writes {
                        if matches!(r, Reg::S(c) if c == EXEC || c == EXEC + 1) {
                            return Err(format!("a node pointer reaches the lanes at {at:#x}"));
                        }
                        // (a copy of a hit pointer is one still)
                        let copy = raw.filter(|_| matches!(name, "v_mov_b32" | "s_mov_b32"));
                        work.push((w, copy, None));
                    }
                }
            }
        }
    }
    // (a branch on what they made: an SCC or VCC a branch reads)
    for (b, block) in facts.p.blocks.iter().enumerate() {
        if region[b] {
            continue;
        }
        for inst in &block.insts {
            if matches!(inst.fields, Fields::Sopp { .. })
                && inst.reads.iter().any(|&(_, x)| seen.iter().any(|s| s.0 == x))
            {
                return Err(format!("a branch at {:#x} on a value made of a node pointer", inst.machine.offset));
            }
        }
    }
    let mut out: Vec<Site> = loads.into_iter().collect();
    out.sort_unstable();
    Ok(out)
}

/// Whether compare `inst` asks of `v` only whether it is `miss`.
fn tests_for_a_hit(inst: &crate::ir::Inst, v: Value, miss: u32) -> bool {
    let src = sources(inst);
    if src.len() < 2 || !plain(inst) || inst.reads.iter().filter(|r| r.1 == v).count() != 1 {
        return false;
    }
    let k = match (super::arg(inst, &src[0]), super::arg(inst, &src[1])) {
        (Arg::Const(k), Arg::Value(w)) | (Arg::Value(w), Arg::Const(k)) if w == v => k,
        _ => return false,
    };
    let name = inst.name();
    let rest = name.strip_prefix("v_cmpx_").or_else(|| name.strip_prefix("v_cmp_"));
    matches!(rest, Some("eq_u32" | "ne_u32" | "lg_u32" | "eq_i32" | "ne_i32" | "lg_i32")) && k == miss
}

/// The registers a memory instruction's address is made of.
fn address_reads(inst: &crate::ir::Inst) -> Vec<Reg> {
    let mut out = Vec::new();
    if let Fields::Flat { addr, saddr, .. } = &inst.fields {
        if let Some(Operand::Vector { reg, dwords }) = *addr {
            out.extend((0..dwords).map(|k| Reg::V(reg + k)));
        }
        if let Some(Operand::Scalar { code, dwords }) = *saddr
            && code != NULL
        {
            out.extend((0..dwords).map(|k| Reg::S(code + k)));
        }
    }
    out
}

/// The field load `site` reads, the hit pointer (its number) it reads it
/// through, and what that pointer points at; `None` when its address cannot
/// be evaluated yet (a bottom level's word not found).
fn classify(facts: &Facts, env: &Env, site: Site) -> Result<Option<(Field, usize, Points)>, String> {
    let inst = facts.inst(site);
    let at = inst.machine.offset;
    if inst.name() != "global_load_dword" {
        return Err(format!("{} at {at:#x} reads a node field", inst.name()));
    }
    let Some(a0) = address(facts, env, site) else { return Ok(None) };
    let moved = |e: &Env| address(facts, e, site).map(|a| a.wrapping_sub(a0));
    // Which pointer the address follows.
    let mut by = None;
    for j in 0..env.hits.len() {
        let mut e = env.clone();
        e.hits[j].1 += Env::pointer(4, 0);
        match moved(&e) {
            Some(0) => {}
            Some(256) if by.is_none() => by = Some(j),
            Some(_) => return Err(format!("the load at {at:#x}: an address not of 64-byte nodes")),
            None => return Ok(None),
        }
    }
    let Some(j) = by else {
        return Err(format!("the load at {at:#x}: an address no hit pointer moves"));
    };
    let mut e = env.clone();
    e.dwords[0] = e.dwords[0].wrapping_add(0x100);
    let by_base = moved(&e).ok_or("an address not evaluated")?;
    let by_blas = env.blas.keys().any(|&s| {
        let mut e = env.clone();
        e.blas.insert(s, env.blas[&s] + 1);
        moved(&e) == Some(256)
    });
    match (by_base, by_blas) {
        (b, false) if b != 0 && b % 256 == 0 => {
            let field = match a0 & 0xff {
                60 => Field::Blas,
                112 => Field::InstanceIndex,
                off => return Err(format!("the load at {at:#x}: +{off} of an instance")),
            };
            Ok(Some((field, j, Points::Instance)))
        }
        (0, true) => {
            let mut offs = [0u64; 4];
            for (kind, off) in offs.iter_mut().enumerate() {
                let mut e = env.clone();
                e.hits[j].1 = (e.hits[j].1 & !7) | kind as u32;
                *off = address(facts, &e, site).ok_or("an address not evaluated")? & 0xff;
            }
            let field = match offs {
                [48, 48, 48, 48] => Field::GeometryWord,
                [52, 56, 52, 52] => Field::PrimitiveIndex,
                o => return Err(format!("the load at {at:#x}: +{o:?} of a triangle by kind")),
            };
            Ok(Some((field, j, Points::Triangle)))
        }
        _ => Err(format!("the load at {at:#x}: an address of neither the structure nor a bottom level")),
    }
}

/// The address the load at `site` reads under `env`.
fn address(facts: &Facts, env: &Env, site: Site) -> Option<u64> {
    let inst = facts.inst(site);
    let Fields::Flat { addr: Some(Operand::Vector { reg, dwords }), saddr, offset, .. } = inst.fields else {
        return None;
    };
    let mut memo = HashMap::new();
    let mut word = |r: Reg| inst.read(r).and_then(|v| eval(facts, env, v, 0, &mut memo));
    let base = match saddr {
        Some(Operand::Scalar { code, .. }) if code != NULL => {
            let lo = u64::from(word(Reg::S(code))?);
            let hi = u64::from(word(Reg::S(code + 1))?);
            (hi << 32 | lo).wrapping_add(u64::from(word(Reg::V(reg))?))
        }
        _ if dwords == 2 => u64::from(word(Reg::V(reg + 1))?) << 32 | u64::from(word(Reg::V(reg))?),
        _ => return None,
    };
    Some(base.wrapping_add(i64::from(offset) as u64))
}

/// The value `v` holds under `env`, for the integer arithmetic an address
/// is made with; lane masks hold the lane's own bit. `None` for anything
/// else.
fn eval(facts: &Facts, env: &Env, v: Value, depth: u32, memo: &mut HashMap<Value, Option<u32>>) -> Option<u32> {
    if let Some(&(_, p)) = env.hits.iter().find(|h| h.0 == v) {
        return Some(p);
    }
    if depth > 48 {
        return None;
    }
    if let Some(&r) = memo.get(&v) {
        return r;
    }
    memo.insert(v, None);
    let r = match facts.defs.get(&v)? {
        Def::Input => None,
        &Def::Phi(b, k) => {
            let mut all = None;
            for &a in &facts.p.blocks[b].phis[k].args {
                let x = eval(facts, env, a, depth + 1, memo);
                if x.is_none() || all.is_some_and(|y| Some(y) != x) {
                    all = None;
                    break;
                }
                all = x;
            }
            all
        }
        &Def::Inst(b, i) => op(facts, env, (b, i), v, depth, memo),
    };
    memo.insert(v, r);
    r
}

/// What the instruction at `site` writes as `v`.
fn op(facts: &Facts, env: &Env, site: Site, v: Value, depth: u32, memo: &mut HashMap<Value, Option<u32>>) -> Option<u32> {
    let inst = facts.inst(site);
    let reg = inst.writes.iter().find(|w| w.1 == v)?.0;
    if is_load(inst) {
        if let Some(&w) = env.blas.get(&site) {
            return Some(w);
        }
        if Some(inst.machine.offset) == env.descriptor
            && let Fields::Smem { data: Some(Operand::Scalar { code, .. }), .. } = inst.fields
            && let Reg::S(c) = reg
        {
            return env.dwords.get(usize::from(c.checked_sub(code)?)).copied();
        }
        return None;
    }
    if !plain(inst) {
        return None;
    }
    let src = sources(inst);
    let scc = match inst.name() {
        "s_cselect_b32" => Some(eval(facts, env, inst.read(Reg::Scc)?, depth + 1, memo)?),
        _ => None,
    };
    let mut word = |n: usize, k: u8| -> Option<u32> {
        let o = src.get(n)?;
        match *o {
            Operand::Vector { reg, .. } => eval(facts, env, inst.read(Reg::V(reg + k))?, depth + 1, memo),
            Operand::Scalar { code, .. } if code == NULL => Some(0),
            // (a lane that runs has its own bit of EXEC)
            Operand::Scalar { code, .. } if code == EXEC => Some(u32::from(k == 0)),
            Operand::Scalar { code, .. } => eval(facts, env, inst.read(Reg::S(code + k))?, depth + 1, memo),
            Operand::Const(c) => {
                let x = inline(c)?;
                Some(if k == 0 { x } else if (x as i32) < 0 && c < 240 { u32::MAX } else { 0 })
            }
            Operand::Literal(x) => Some(if k == 0 { x } else { 0 }),
            Operand::Attr { .. } => None,
        }
    };
    let wide = |word: &mut dyn FnMut(usize, u8) -> Option<u32>, n: usize| -> Option<u64> {
        Some(u64::from(word(n, 1)?) << 32 | u64::from(word(n, 0)?))
    };
    // Which dword of the result `reg` is, and whether it is the carry.
    let (dst, carry) = match &inst.fields {
        Fields::Valu(x) => (x.dst, x.carry),
        Fields::Sop { dst, .. } => (*dst, None),
        _ => return None,
    };
    let part = |o: Option<Operand>| match (o, reg) {
        (Some(Operand::Vector { reg: d, .. }), Reg::V(r)) => r.checked_sub(d),
        (Some(Operand::Scalar { code, .. }), Reg::S(r)) => r.checked_sub(code),
        _ => None,
    };
    let half = |x: u64, k: u8| if k == 0 { x as u32 } else { (x >> 32) as u32 };
    let name = inst.name();
    if reg == Reg::Scc {
        // (an SCC compare)
        let rest = name.strip_prefix("s_cmp_")?;
        let (a, b) = (word(0, 0)?, word(1, 0)?);
        return compare(&format!("v_cmp_{rest}"), a, b).map(u32::from);
    }
    if let Some(k) = part(carry) {
        let sum = match name {
            "v_add_co_u32" => u64::from(word(0, 0)?) + u64::from(word(1, 0)?),
            "v_add_co_ci_u32" => u64::from(word(0, 0)?) + u64::from(word(1, 0)?) + u64::from(word(2, 0)? & 1),
            _ => return None,
        };
        return Some(if k == 0 { (sum >> 32) as u32 } else { 0 });
    }
    let k = part(dst)?;
    if name.starts_with("v_cmp_") {
        let (a, b) = (word(0, 0)?, word(1, 0)?);
        return Some(if k == 0 { u32::from(compare(name, a, b)?) } else { 0 });
    }
    let x = match name {
        "v_mov_b32" | "s_mov_b32" => word(0, 0)?,
        "s_mov_b64" => word(0, k)?,
        "v_lshrrev_b32" => word(1, 0)? >> (word(0, 0)? & 31),
        "v_lshlrev_b32" => word(1, 0)? << (word(0, 0)? & 31),
        "v_lshrrev_b64" => half(wide(&mut word, 1)? >> (word(0, 0)? & 63), k),
        "v_lshlrev_b64" => half(wide(&mut word, 1)? << (word(0, 0)? & 63), k),
        "s_lshl_b32" => word(0, 0)? << (word(1, 0)? & 31),
        "s_lshr_b32" => word(0, 0)? >> (word(1, 0)? & 31),
        "s_lshl_b64" => half(wide(&mut word, 0)? << (word(1, 0)? & 63), k),
        "s_lshr_b64" => half(wide(&mut word, 0)? >> (word(1, 0)? & 63), k),
        "v_add_nc_u32" | "v_add_u32" | "s_add_u32" | "s_add_i32" => word(0, 0)?.wrapping_add(word(1, 0)?),
        "v_sub_nc_u32" | "s_sub_u32" | "s_sub_i32" => word(0, 0)?.wrapping_sub(word(1, 0)?),
        "v_add_co_u32" => word(0, 0)?.wrapping_add(word(1, 0)?),
        "v_add_co_ci_u32" => word(0, 0)?.wrapping_add(word(1, 0)?).wrapping_add(word(2, 0)? & 1),
        "v_or_b32" | "s_or_b32" => word(0, 0)? | word(1, 0)?,
        "v_and_b32" | "s_and_b32" => word(0, 0)? & word(1, 0)?,
        "v_xor_b32" | "s_xor_b32" => word(0, 0)? ^ word(1, 0)?,
        "s_or_b64" => word(0, k)? | word(1, k)?,
        "s_and_b64" => word(0, k)? & word(1, k)?,
        "v_or3_b32" => word(0, 0)? | word(1, 0)? | word(2, 0)?,
        "v_lshl_add_u32" => (word(0, 0)? << (word(1, 0)? & 31)).wrapping_add(word(2, 0)?),
        "v_lshl_or_b32" => (word(0, 0)? << (word(1, 0)? & 31)) | word(2, 0)?,
        "v_add_lshl_u32" => word(0, 0)?.wrapping_add(word(1, 0)?) << (word(2, 0)? & 31),
        "v_and_or_b32" => (word(0, 0)? & word(1, 0)?) | word(2, 0)?,
        "v_bfe_u32" => {
            let (x, off, w) = (word(0, 0)?, word(1, 0)? & 31, word(2, 0)? & 31);
            if w == 0 { 0 } else { (x >> off) & (u32::MAX >> (32 - w)) }
        }
        "v_cndmask_b32" => {
            if word(2, 0)? & 1 != 0 {
                word(1, 0)?
            } else {
                word(0, 0)?
            }
        }
        "s_cselect_b32" => {
            if scc? != 0 {
                word(0, 0)?
            } else {
                word(1, 0)?
            }
        }
        _ => return None,
    };
    Some(x)
}
