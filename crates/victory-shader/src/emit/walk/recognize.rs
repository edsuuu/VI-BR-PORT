//! The recognizer: where a program walks the console's ray-tracing
//! structure in software, what goes into the walk and what comes out of it,
//! and whether the walk could be the host's ray query instead.
//!
//! It only reads the program (the census of milestone M3a, `translate
//! --walk-report`). The title has two molds of walk (the native design's
//! `nativo-programas.md`): mold A, one lane one walk, in a loop around one
//! `image_bvh64_intersect_ray` with the stack in LDS; and mold B, the top of
//! the structure walked by the whole wave with lane votes, the BLAS by each
//! lane, in a pair of loops around two `image_bvh_intersect_ray`. The region a
//! query would stand for is the loop around the site (A) or the outer loop of
//! the pair (B).
//!
//! What is read here, and how far it is to be trusted:
//! - the loops, sites, anchors and the code inside the region are counted;
//! - the live inputs and outputs are the SSA values that cross the region's
//!   edge; their roles come from following copies, selects and phis inside
//!   the region back to the site's operands and results;
//! - the exit kind ("first hit" or "closest") comes from the select that
//!   takes a triangle's `t` as the new extent: the writes under the same
//!   condition are what a hit saves, and a constant among them that the
//!   loop's own exit test ends the walk on means the walk stops at that hit.
//!   Where any link of that chain is missing the kind is unknown, and said so.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use super::cfg::{Loop, Loops};

mod fields;
pub use fields::{Field, FieldLoad};
use crate::gfx10::{Fields, NULL, Operand};
use crate::ir::{EXEC, Inst, Program, Reg, Value};
use crate::structure::{Atom, Bool, LaneExit, Structured};

/// An instruction: block and index.
pub type Site = (usize, usize);

/// How the walk is laid out (see the module notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mold {
    A,
    B,
    Other,
}

/// The mold and what the program's own code does inside the walk: nothing
/// (`0`), reads of the title's geometry table only (`F`, flags), texture
/// reads (`A`, alpha). As `nativo-programas.md` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    A0,
    AF,
    AA,
    B0,
    BF,
    BA,
    Other,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::A0 => "A0",
            Family::AF => "AF",
            Family::AA => "AA",
            Family::B0 => "B0",
            Family::BF => "BF",
            Family::BA => "BA",
            Family::Other => "other",
        }
    }

    /// The family named `name` (`a0`, `B0`...).
    pub fn parse(name: &str) -> Option<Family> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "a0" => Family::A0,
            "af" => Family::AF,
            "aa" => Family::AA,
            "b0" => Family::B0,
            "bf" => Family::BF,
            "ba" => Family::BA,
            _ => return None,
        })
    }
}

/// Whether the walk ends at the first triangle it accepts or keeps the
/// closest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    First,
    Closest,
    Unknown,
}

/// The fixed marks of the title's walks (`nativo-programas.md`, section 4).
#[derive(Debug, Clone, Default)]
pub struct Anchors {
    /// Every site's descriptor is the constant one: dword 1 0x80000000 (boxes
    /// sorted), dword 3 0x890003ff (a structure, barycentrics returned).
    pub descriptor: bool,
    /// The node kind taken from the pointer (`& 7`, `& 6`).
    pub kind: bool,
    /// The children sorted (`v_min3/med3/max3_u32`).
    pub sort: bool,
    /// A pop from the LDS stack (`ds_wrxchg_rtn_b32`).
    pub pop: bool,
    /// The parent fetch through the header's +52 and +56.
    pub parent: bool,
    /// The iteration cap (`s_cmpk_*_u32 ..., 0xffff`).
    pub cap: Option<u16>,
    /// Lane votes and a stack in VGPR lanes (mold B's top).
    pub votes: bool,
}

/// What a value crossing the region's edge stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// The world-space ray at the region's entry, axis 0-2.
    Origin(u8),
    Direction(u8),
    Inverse(u8),
    /// The ray's extent (t max) at the entry.
    Extent,
    /// The node pointer the walk starts from (the structure's root).
    Root,
    /// A dword of the structure's view descriptor, or the base address made
    /// of it.
    Structure,
    /// The lanes that enter (EXEC).
    Lanes,
    // Outputs:
    /// The extent once the walk is over: the accepted triangle's t, or the
    /// entry's extent on a miss.
    T,
    BaryI,
    BaryJ,
    /// A node pointer the walk saves when it accepts a triangle (0 or the
    /// entry value on a miss).
    HitNode,
    /// A node pointer the walk holds at its end without saving it at a hit
    /// (the current node or instance).
    NodePointer,
    /// A constant the walk sets (its state).
    State,
    /// The ray as the walk left it: in object space if it ended inside an
    /// instance.
    Ray,
    /// A value computed from an instance's transform (object space).
    ObjectSpace,
    /// The DXR hit kind of the accepted triangle: 0xfe for its front face,
    /// 0xff for its back, the instance's winding taken into account.
    HitKind,
    /// The same value on every path through the region.
    Constant(u32),
    /// The value the register held at the entry, whatever the walk did.
    Entry,
    Other(String),
}

impl Role {
    fn text(&self) -> String {
        const AXIS: [char; 3] = ['x', 'y', 'z'];
        match self {
            Role::Origin(a) => format!("origin.{}", AXIS[*a as usize]),
            Role::Direction(a) => format!("direction.{}", AXIS[*a as usize]),
            Role::Inverse(a) => format!("inverse.{}", AXIS[*a as usize]),
            Role::Extent => "extent".into(),
            Role::Root => "root".into(),
            Role::Structure => "structure".into(),
            Role::Lanes => "lanes".into(),
            Role::T => "t".into(),
            Role::BaryI => "bary.i".into(),
            Role::BaryJ => "bary.j".into(),
            Role::HitNode => "hit-node".into(),
            Role::NodePointer => "node-pointer".into(),
            Role::State => "state".into(),
            Role::Ray => "ray".into(),
            Role::ObjectSpace => "object-space".into(),
            Role::HitKind => "hit-kind".into(),
            Role::Constant(k) => format!("const {k:#x}"),
            Role::Entry => "entry".into(),
            Role::Other(s) => s.clone(),
        }
    }

    /// Whether a query's committed state (or the region's entry) gives this
    /// output.
    fn has_accessor(&self) -> bool {
        matches!(
            self,
            Role::T | Role::BaryI | Role::BaryJ | Role::HitNode | Role::HitKind | Role::Constant(_) | Role::Lanes | Role::Entry
        )
    }
}

/// A value that crosses the region's edge.
#[derive(Debug, Clone)]
pub struct Live {
    pub reg: Reg,
    pub value: Value,
    pub role: Option<Role>,
    /// An output's value on a miss when that is a constant: what the
    /// register held at the region's entry.
    pub miss: Option<u32>,
}

/// A bound of the ray: a constant (float bits), or a value of the program
/// from before the region and the register the walk reads it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Const(u32),
    Value(Reg, Value),
}

/// The instance inclusion mask the walk tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mask {
    /// One constant byte (`InstanceInclusionMask`).
    Constant(u8),
    /// From a value of the program.
    Value(Value),
    /// None found, or several.
    Unknown(String),
}

/// A walk found in a program, and what the recognizer read of it.
#[derive(Debug, Clone)]
pub struct Walk {
    pub header: usize,
    pub header_offset: usize,
    /// The lowest and highest instruction offsets inside the region.
    pub span: (usize, usize),
    pub blocks: usize,
    pub insts: usize,
    pub sites: Vec<Site>,
    pub site_offsets: Vec<usize>,
    /// The 64-bit form of the node test.
    pub wide: bool,
    pub mold: Mold,
    pub family: Family,
    pub anchors: Anchors,
    /// Texture reads inside the region (other than the node test).
    pub tex: usize,
    /// Indexed buffer reads inside the region (the title's geometry table).
    pub geo: usize,
    /// Edges out of the region (block inside, block outside).
    pub exits: Vec<(usize, usize)>,
    pub inputs: Vec<Live>,
    pub outputs: Vec<Live>,
    pub exit: ExitKind,
    /// How the exit kind was read, or why it could not be.
    pub exit_why: String,
    /// The ray's t min: a constant (float bits) or a value from before the
    /// region; `None` when none is found.
    pub t_min: Option<Bound>,
    /// The constant the walk multiplies an instance's facing by: the ray's
    /// face culling (0 none, 1 back faces, -1 front faces [inf]); `None` when
    /// no single one is found.
    pub cull: Option<i32>,
    /// Per block of the program: inside the region.
    pub body: Vec<bool>,
    pub mask: Mask,
    /// The structure's view descriptor: the scalar load that reads it
    /// (its offset), when the base the walk uses comes from one.
    pub descriptor_load: Option<usize>,
    /// Uses of the outputs after the region, by output register.
    pub after: Vec<(Reg, After)>,
    /// What keeps the region from being replaced by a query, if anything.
    pub refusals: Vec<String>,
    /// The loads after the region of the hit's node fields, which a query
    /// gives (`fields.rs`).
    pub fields: Vec<FieldLoad>,
    /// What a query has to give beyond the walks of milestone M3a (hit kind,
    /// barycentrics, node fields, a cap in a register): the walk fits only
    /// the replacement that gives them ([`Walk::fits_extended`]).
    pub extended: Vec<&'static str>,
}

/// What the code after the region does with an output.
#[derive(Debug, Clone, Default)]
pub struct After {
    pub compares: usize,
    /// Memory reads at addresses made from the output: the constant part of
    /// the address, from the node's start where it is a node pointer.
    pub loads: Vec<i64>,
    pub stores: usize,
    pub uses: usize,
}

impl Walk {
    /// Whether the census finds the walk fit for milestone M3a's replacement:
    /// family A0 and nothing refused, nothing [`Walk::extended`] needed.
    pub fn fits(&self) -> bool {
        self.family == Family::A0 && self.refusals.is_empty() && self.extended.is_empty()
    }

    /// Whether the walk fits the replacement that also gives the hit kind,
    /// the barycentrics and the hit's node fields (`VICTORY_RT_QUERY=a0f`).
    pub fn fits_extended(&self) -> bool {
        self.family == Family::A0 && self.refusals.is_empty()
    }

    /// The verdict of the report lines.
    fn verdict(&self) -> String {
        if self.fits() {
            "fits A0".to_owned()
        } else if self.fits_extended() {
            format!("fits A0 with {}", self.extended.join(", "))
        } else if self.refusals.is_empty() {
            format!("{}, not in M3a", self.family.name())
        } else {
            format!("refused: {}", self.refusals.join("; "))
        }
    }

    /// One tab-separated line: label, walk number, family, mold, exit kind,
    /// object-space outputs, verdict.
    pub fn summary(&self, label: &str, n: usize) -> String {
        let objects = self.outputs.iter().filter(|l| matches!(l.role, Some(Role::ObjectSpace | Role::Ray))).count();
        let exit = match self.exit {
            ExitKind::First => "first-hit",
            ExitKind::Closest => "closest",
            ExitKind::Unknown => "unknown",
        };
        let verdict = self.verdict();
        format!(
            "WALKTSV\t{label}\t{n}\t{}\t{:?}\t{exit}\t{objects}\t{}\t{verdict}",
            self.family.name(),
            self.mold,
            self.site_offsets.len()
        )
    }

    /// The walk as report lines.
    pub fn lines(&self, label: &str, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        let reg = |r: Reg| match r {
            Reg::S(c) if c == EXEC => "exec".to_owned(),
            Reg::S(c) if c == EXEC + 1 => "exec_hi".to_owned(),
            Reg::S(106) => "vcc".to_owned(),
            Reg::S(c) => format!("s{c}"),
            Reg::V(c) => format!("v{c}"),
            Reg::Scc => "scc".to_owned(),
        };
        let sites: Vec<String> = self.site_offsets.iter().map(|o| format!("{o:#x}")).collect();
        out.push(format!(
            "WALK {label} #{n} family={} mold={:?} form={} sites={} region={:#x}..{:#x} header={:#x} blocks={} insts={} exits={}",
            self.family.name(),
            self.mold,
            if self.wide { "bvh64" } else { "bvh32" },
            sites.join(","),
            self.span.0,
            self.span.1,
            self.header_offset,
            self.blocks,
            self.insts,
            self.exits.len()
        ));
        let a = &self.anchors;
        out.push(format!(
            "  anchors: descriptor={} kind={} sort={} pop={} parent={} cap={} votes={}; in-walk tex={} geo={}",
            a.descriptor,
            a.kind,
            a.sort,
            a.pop,
            a.parent,
            a.cap.map_or("none".to_owned(), |c| format!("{c:#x}")),
            a.votes,
            self.tex,
            self.geo
        ));
        let lives = |list: &[Live]| {
            let mut named = Vec::new();
            let mut rest = Vec::new();
            for l in list {
                match &l.role {
                    Some(r) => named.push(format!("{}={}", reg(l.reg), r.text())),
                    None => rest.push(reg(l.reg)),
                }
            }
            rest.sort();
            rest.dedup();
            let mut s = named.join(" ");
            if !rest.is_empty() {
                let _ = write!(s, " (+{} unnamed: {})", rest.len(), rest.join(" "));
            }
            s
        };
        out.push(format!("  inputs: {}", lives(&self.inputs)));
        out.push(format!("  outputs: {}", lives(&self.outputs)));
        let t_min = match self.t_min {
            Some(Bound::Const(k)) => format!("{}", f32::from_bits(k)),
            Some(Bound::Value(r, _)) => format!("the value in {}", reg(r)),
            None => "not found".to_owned(),
        };
        let mask = match &self.mask {
            Mask::Constant(m) => format!("{m:#04x}"),
            Mask::Value(_) => "a value".to_owned(),
            Mask::Unknown(why) => format!("unknown ({why})"),
        };
        out.push(format!(
            "  exit: {} ({}); t-min {t_min}; instance mask {mask}; cull {}; descriptor load {}",
            match self.exit {
                ExitKind::First => "first-hit",
                ExitKind::Closest => "closest",
                ExitKind::Unknown => "unknown",
            },
            self.exit_why,
            self.cull.map_or("not found".to_owned(), |c| c.to_string()),
            self.descriptor_load.map_or("not found".to_owned(), |o| format!("{o:#x}"))
        ));
        let objects: Vec<String> = self
            .outputs
            .iter()
            .filter(|l| matches!(l.role, Some(Role::ObjectSpace | Role::Ray)))
            .map(|l| reg(l.reg))
            .collect();
        out.push(format!(
            "  object-space outputs: {}",
            if objects.is_empty() { "none".to_owned() } else { objects.join(" ") }
        ));
        let misses: Vec<String> = self
            .outputs
            .iter()
            .filter_map(|l| l.miss.map(|m| format!("{}={m:#x}", reg(l.reg))))
            .collect();
        if !misses.is_empty() {
            out.push(format!("  on a miss: {}", misses.join(" ")));
        }
        if !self.after.is_empty() {
            let mut s = String::new();
            for (r, a) in &self.after {
                let loads: Vec<String> = a.loads.iter().map(|o| format!("+{o}")).collect();
                let _ = write!(
                    s,
                    " {}: {} compares, {} loads [{}], {} stores, {} uses;",
                    reg(*r),
                    a.compares,
                    a.loads.len(),
                    loads.join(" "),
                    a.stores,
                    a.uses
                );
            }
            out.push(format!("  after:{s}"));
        }
        if !self.fields.is_empty() {
            let f: Vec<String> = self.fields.iter().map(|f| format!("{:#x}={}", f.offset, f.field.text())).collect();
            out.push(format!("  node fields after: {}", f.join(" ")));
        }
        let verdict = match self.verdict() {
            _ if self.refusals.is_empty() && !self.fits_extended() => {
                format!("family {}: not in M3a", self.family.name())
            }
            v => v,
        };
        out.push(format!("  verdict: {verdict}"));
        out
    }
}

/// Where a value is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Def {
    Input,
    Phi(usize, usize),
    Inst(usize, usize),
}

/// Where a value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    Inst(usize, usize),
    /// Block, phi, and the predecessor index of the argument.
    Phi(usize, usize, usize),
}

/// An operand as the recognizer reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arg {
    Value(Value),
    Const(u32),
    None,
}

/// The value of inline constant operand `c` (128-192: 0-64, 193-208: -1 to
/// -16, 240-248: floats).
fn inline(c: u8) -> Option<u32> {
    Some(match c {
        128..=192 => u32::from(c - 128),
        193..=208 => (-(i32::from(c) - 192)) as u32,
        240 => 0.5f32.to_bits(),
        241 => (-0.5f32).to_bits(),
        242 => 1.0f32.to_bits(),
        243 => (-1.0f32).to_bits(),
        244 => 2.0f32.to_bits(),
        245 => (-2.0f32).to_bits(),
        246 => 4.0f32.to_bits(),
        247 => (-4.0f32).to_bits(),
        248 => 0x3e22_f983,
        _ => return None,
    })
}

fn arg(inst: &Inst, op: &Operand) -> Arg {
    match *op {
        Operand::Vector { reg, .. } => inst.read(Reg::V(reg)).map_or(Arg::None, Arg::Value),
        Operand::Scalar { code, .. } if code == NULL => Arg::Const(0),
        Operand::Scalar { code, .. } => inst.read(Reg::S(code)).map_or(Arg::None, Arg::Value),
        Operand::Const(c) => inline(c).map_or(Arg::None, Arg::Const),
        Operand::Literal(x) => Arg::Const(x),
        Operand::Attr { .. } => Arg::None,
    }
}

/// The sources of a VALU or SALU instruction.
fn sources(inst: &Inst) -> &[Operand] {
    match &inst.fields {
        Fields::Valu(v) => &v.src,
        Fields::Sop { src, .. } => src,
        _ => &[],
    }
}

/// Whether the VALU instruction has no modifier that changes its operands
/// or result.
fn plain(inst: &Inst) -> bool {
    match &inst.fields {
        Fields::Valu(v) => v.neg == 0 && v.abs == 0 && !v.clamp && v.omod == 0 && v.sdwa.is_none() && v.dpp.is_none(),
        Fields::Sop { .. } => true,
        _ => false,
    }
}

/// The single VGPR or SGPR a VALU or SALU instruction writes, if it writes one dword.
fn dst_reg(inst: &Inst) -> Option<Reg> {
    match &inst.fields {
        Fields::Valu(v) => match v.dst {
            Some(Operand::Vector { reg, .. }) => Some(Reg::V(reg)),
            Some(Operand::Scalar { code, .. }) => Some(Reg::S(code)),
            _ => None,
        },
        Fields::Sop { dst: Some(Operand::Scalar { code, .. }), .. } => Some(Reg::S(*code)),
        _ => None,
    }
}

/// The constant part of a memory access's address: the instruction's own
/// offset. (The address registers' constant terms are added by the caller.)
fn memory_offset(inst: &Inst) -> Option<i64> {
    match &inst.fields {
        Fields::Flat { offset, .. } => Some(i64::from(*offset)),
        Fields::Smem { offset, .. } => Some(i64::from(*offset)),
        _ => None,
    }
}

fn is_load(inst: &Inst) -> bool {
    let n = inst.name();
    (n.starts_with("global_load") || n.starts_with("flat_load") || n.starts_with("s_load")) && !n.contains("lds")
}

fn is_store(inst: &Inst) -> bool {
    let n = inst.name();
    n.starts_with("global_store")
        || n.starts_with("flat_store")
        || n.starts_with("buffer_store")
        || n.starts_with("tbuffer_store")
        || n.starts_with("image_store")
        || n.starts_with("ds_write")
        || n.starts_with("global_atomic")
        || n.starts_with("buffer_atomic")
        || n.starts_with("s_store")
}

/// A compare's result for operands `a` and `b`, by the compare's name
/// (`v_cmp_ne_u32`, `v_cmpx_lt_i32`...); `None` for kinds it does not know.
fn compare(name: &str, a: u32, b: u32) -> Option<bool> {
    let rest = name.strip_prefix("v_cmpx_").or_else(|| name.strip_prefix("v_cmp_"))?;
    let (op, ty) = rest.split_once('_')?;
    let ord = match ty {
        "u32" => a.cmp(&b),
        "i32" => (a as i32).cmp(&(b as i32)),
        _ => return None,
    };
    use std::cmp::Ordering::*;
    Some(match op {
        "eq" => ord == Equal,
        "ne" | "lg" => ord != Equal,
        "lt" => ord == Less,
        "le" => ord != Greater,
        "gt" => ord == Greater,
        "ge" => ord != Less,
        _ => return None,
    })
}

/// A per-lane condition evaluated with some atoms known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    False,
    True,
    Depends,
}

fn eval(st: &Structured, b: Bool, known: &dyn Fn(Atom) -> Option<bool>, memo: &mut HashMap<Bool, Tri>) -> Tri {
    if b == Bool::TRUE {
        return Tri::True;
    }
    if b == Bool::FALSE {
        return Tri::False;
    }
    if let Some(&t) = memo.get(&b) {
        return t;
    }
    let Some((atom, lo, hi)) = st.bools.node(b) else {
        return Tri::Depends;
    };
    let t = match known(atom) {
        Some(true) => eval(st, hi, known, memo),
        Some(false) => eval(st, lo, known, memo),
        None => {
            let (l, h) = (eval(st, lo, known, memo), eval(st, hi, known, memo));
            if l == h { l } else { Tri::Depends }
        }
    };
    memo.insert(b, t);
    t
}

/// Whether `a` is the negation of `b` (canonical diagrams: the same shape
/// with the constants swapped).
fn negation(st: &Structured, a: Bool, b: Bool, memo: &mut HashMap<(Bool, Bool), bool>) -> bool {
    if (a == Bool::TRUE && b == Bool::FALSE) || (a == Bool::FALSE && b == Bool::TRUE) {
        return true;
    }
    if let Some(&r) = memo.get(&(a, b)) {
        return r;
    }
    let r = match (st.bools.node(a), st.bools.node(b)) {
        (Some((x, alo, ahi)), Some((y, blo, bhi))) if x == y => {
            negation(st, alo, blo, memo) && negation(st, ahi, bhi, memo)
        }
        _ => false,
    };
    memo.insert((a, b), r);
    r
}

/// Where the values of a program are defined and read.
struct Facts<'a> {
    p: &'a Program,
    st: Option<&'a Structured>,
    defs: HashMap<Value, Def>,
    uses: HashMap<Value, Vec<Use>>,
    /// The reads of each instruction that are operands: without the old
    /// value of a VGPR it only writes, where every lane that reaches it writes
    /// (`ir::operand_reads`; with a guard, or without a structure, the old
    /// value is kept in some lanes and counts).
    reads: HashMap<Site, Vec<(Reg, Value)>>,
}

/// Where a value followed forward last went on in only some lanes: through
/// the edge from predecessor number `.1` into the join `.0` (a block without
/// back edges in), or, kept by a guarded write in block `.0`, in the lanes
/// the write's guard `.1` leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Hop {
    Edge(usize, usize),
    Kept(usize, Bool),
}

/// What [`Facts::can_hold`] found, by hop and instruction.
type Seen = HashMap<(Hop, Site), bool>;

/// A value followed back through copies, selects and phis inside a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Root {
    /// Defined outside the region.
    Outside(Value),
    Const(u32),
    /// Computed inside the region by an instruction that is not a copy.
    Inside(Site),
}

impl<'a> Facts<'a> {
    fn new(p: &'a Program, st: Option<&'a Structured>) -> Self {
        let mut defs = HashMap::new();
        let mut uses: HashMap<Value, Vec<Use>> = HashMap::new();
        let mut reads = HashMap::new();
        for &(_, v) in &p.inputs {
            defs.insert(v, Def::Input);
        }
        for (b, block) in p.blocks.iter().enumerate() {
            for (k, phi) in block.phis.iter().enumerate() {
                defs.insert(phi.value, Def::Phi(b, k));
                for (a, &v) in phi.args.iter().enumerate() {
                    uses.entry(v).or_default().push(Use::Phi(b, k, a));
                }
            }
            for (i, inst) in block.insts.iter().enumerate() {
                for &(_, v) in &inst.writes {
                    defs.insert(v, Def::Inst(b, i));
                }
                let guarded = st.is_none_or(|st| st.guards.get(b).and_then(|g| g.get(i)).copied().flatten().is_some());
                let operands = crate::ir::operand_reads(p, inst);
                let real: Vec<(Reg, Value)> = inst
                    .reads
                    .iter()
                    .copied()
                    .filter(|&(r, _)| {
                        guarded || !matches!(r, Reg::V(_)) || inst.write(r).is_none() || operands.contains(&r)
                    })
                    .collect();
                for &(_, v) in &real {
                    uses.entry(v).or_default().push(Use::Inst(b, i));
                }
                reads.insert((b, i), real);
            }
        }
        Facts { p, st, defs, uses, reads }
    }

    /// The operand reads of the instruction at `site` ([`Facts::reads`]).
    fn reads(&self, site: Site) -> &[(Reg, Value)] {
        self.reads.get(&site).map_or(&[][..], |r| &r[..])
    }

    fn inst(&self, (b, i): Site) -> &'a Inst {
        &self.p.blocks[b].insts[i]
    }

    fn block_of(&self, v: Value) -> Option<usize> {
        match self.defs.get(&v)? {
            Def::Input => None,
            Def::Phi(b, _) | Def::Inst(b, _) => Some(*b),
        }
    }

    fn inside(&self, v: Value, region: &[bool]) -> bool {
        self.block_of(v).is_some_and(|b| region[b])
    }

    /// The constant `v` holds, through moves.
    fn konst(&self, v: Value) -> Option<u32> {
        let mut v = v;
        for _ in 0..8 {
            let Def::Inst(b, i) = *self.defs.get(&v)? else {
                return None;
            };
            let inst = self.inst((b, i));
            let src = sources(inst);
            match inst.name() {
                "s_mov_b32" | "v_mov_b32" if plain(inst) => match arg(inst, src.first()?) {
                    Arg::Const(k) => return Some(k),
                    Arg::Value(w) => v = w,
                    Arg::None => return None,
                },
                "s_movk_i32" => match &inst.fields {
                    Fields::Sop { imm: Some(imm), .. } => return Some(*imm as i16 as i32 as u32),
                    _ => return None,
                },
                "s_brev_b32" => match arg(inst, src.first()?) {
                    Arg::Const(k) => return Some(k.reverse_bits()),
                    _ => return None,
                },
                _ => return None,
            }
        }
        None
    }

    /// The guard of a vector instruction (`None`: reaching it is enough, or no
    /// structure).
    fn guard(&self, (b, i): Site) -> Option<Bool> {
        self.st.and_then(|st| st.guards.get(b)?.get(i).copied().flatten())
    }

    /// `v` followed back inside `region` through phis, moves and selects (and
    /// the old value a guarded write keeps): the values it can be, and every
    /// value passed on the way.
    fn roots(&self, v: Value, region: &[bool]) -> (HashSet<Root>, HashSet<Value>) {
        let mut roots = HashSet::new();
        let mut seen = HashSet::new();
        let mut work = vec![v];
        while let Some(v) = work.pop() {
            if !seen.insert(v) || seen.len() > 4096 {
                continue;
            }
            match self.defs.get(&v) {
                None | Some(Def::Input) => {
                    roots.insert(Root::Outside(v));
                }
                Some(&Def::Phi(b, k)) => {
                    if region[b] {
                        work.extend(self.p.blocks[b].phis[k].args.iter().copied());
                    } else {
                        roots.insert(Root::Outside(v));
                    }
                }
                Some(&Def::Inst(b, i)) => {
                    if !region[b] {
                        roots.insert(Root::Outside(v));
                        continue;
                    }
                    let inst = self.inst((b, i));
                    let src = sources(inst);
                    let push = |a: Arg, roots: &mut HashSet<Root>, work: &mut Vec<Value>| match a {
                        Arg::Value(w) => work.push(w),
                        Arg::Const(k) => {
                            roots.insert(Root::Const(k));
                        }
                        Arg::None => {
                            roots.insert(Root::Inside((b, i)));
                        }
                    };
                    let copy = match inst.name() {
                        "v_mov_b32" | "s_mov_b32" if plain(inst) && !src.is_empty() => {
                            push(arg(inst, &src[0]), &mut roots, &mut work);
                            true
                        }
                        "v_cndmask_b32" | "s_cselect_b32" if plain(inst) && src.len() >= 2 => {
                            push(arg(inst, &src[0]), &mut roots, &mut work);
                            push(arg(inst, &src[1]), &mut roots, &mut work);
                            true
                        }
                        _ => false,
                    };
                    if !copy {
                        roots.insert(Root::Inside((b, i)));
                    }
                    // a guarded write keeps the old value in the lanes it leaves out
                    if let Some(Reg::V(r)) = dst_reg(inst)
                        && (self.st.is_none() || self.guard((b, i)).is_some())
                        && let Some(old) = inst.read(Reg::V(r))
                        && !src.iter().any(|s| matches!(s, Operand::Vector { reg, .. } if *reg == r))
                    {
                        work.push(old);
                    }
                }
            }
        }
        (roots, seen)
    }

    /// Whether `v` is computed inside `region` from an instance's transform
    /// (a load at +64..+111 of a node), within a few steps.
    fn made_of_transform(&self, v: Value, region: &[bool], loads48: &HashMap<Site, i64>) -> bool {
        let mut seen = HashSet::new();
        let mut work = vec![(v, 0)];
        while let Some((v, depth)) = work.pop() {
            if depth > 12 || !seen.insert(v) {
                continue;
            }
            let Some(&Def::Inst(b, i)) = self.defs.get(&v) else {
                if let Some(&Def::Phi(b, k)) = self.defs.get(&v)
                    && region[b]
                {
                    work.extend(self.p.blocks[b].phis[k].args.iter().map(|&a| (a, depth + 1)));
                }
                continue;
            };
            if !region[b] {
                continue;
            }
            if let Some(&off) = loads48.get(&(b, i))
                && (64..112).contains(&off)
            {
                return true;
            }
            let inst = self.inst((b, i));
            if is_load(inst) {
                continue;
            }
            for &(r, w) in self.reads((b, i)) {
                if !matches!(r, Reg::S(c) if c == EXEC || c == EXEC + 1) {
                    work.push((w, depth + 1));
                }
            }
        }
        false
    }

    /// Whether `v`, a value an accept saves, is the DXR hit kind as the
    /// console's traversal makes it (the GDK's `DetermineHitKind`): 0xfe
    /// where the triangle's denominator is positive (a select of 0xfe and
    /// 0xff on a compare of the reciprocal with 0), flipped by one bit, the
    /// instance's counter-clockwise flag (`v_xor_b32` with a `v_bfe_u32` of
    /// width 1).
    fn hit_kind(&self, v: Value) -> bool {
        let def = |v: Value| match self.defs.get(&v) {
            Some(&Def::Inst(b, i)) => Some(self.inst((b, i))),
            _ => None,
        };
        let Some(x) = def(v).filter(|x| x.name() == "v_xor_b32" && plain(x)) else {
            return false;
        };
        let src = sources(x);
        if src.len() != 2 {
            return false;
        }
        let side = |n: usize| match arg(x, &src[n]) {
            Arg::Value(w) => def(w),
            _ => None,
        };
        let select = |s: &Inst| {
            let ss = sources(s);
            if s.name() != "v_cndmask_b32" || !plain(s) || ss.len() != 3 {
                return false;
            }
            let k = |n: usize| match arg(s, &ss[n]) {
                Arg::Const(k) => Some(k),
                Arg::Value(w) => self.konst(w),
                Arg::None => None,
            };
            let pair = (k(0), k(1));
            let Arg::Value(m) = arg(s, &ss[2]) else { return false };
            let Some(c) = def(m) else { return false };
            let cs = sources(c);
            if cs.len() < 2 || !plain(c) {
                return false;
            }
            let rcp = |n: usize| matches!(arg(c, &cs[n]), Arg::Value(w) if def(w).is_some_and(|r| r.name() == "v_rcp_f32"));
            let zero = |n: usize| arg(c, &cs[n]) == Arg::Const(0);
            // (whether the mask is true where the reciprocal is positive)
            let positive = match (c.name(), zero(0) && rcp(1), rcp(0) && zero(1)) {
                ("v_cmp_lt_f32", true, _) | ("v_cmp_gt_f32", _, true) => true,
                ("v_cmp_gt_f32", true, _) | ("v_cmp_lt_f32", _, true) => false,
                _ => return false,
            };
            pair == if positive { (Some(0xff), Some(0xfe)) } else { (Some(0xfe), Some(0xff)) }
        };
        let bit = |s: &Inst| {
            let ss = sources(s);
            s.name() == "v_bfe_u32" && ss.len() == 3 && arg(s, &ss[2]) == Arg::Const(1)
        };
        match (side(0), side(1)) {
            (Some(a), Some(b)) => (select(a) && bit(b)) || (select(b) && bit(a)),
            _ => false,
        }
    }

    /// The constant term of the address of memory access `site`: its own
    /// offset plus the small constants or-ed or added into its address
    /// registers inside `region` (a node pointer's field offset).
    fn address_term(&self, site: Site) -> i64 {
        let inst = self.inst(site);
        let mut term = memory_offset(inst).unwrap_or(0);
        let addr = match &inst.fields {
            Fields::Flat { addr: Some(Operand::Vector { reg, .. }), .. } => inst.read(Reg::V(*reg)),
            Fields::Smem { base: Operand::Scalar { code, .. }, .. } => inst.read(Reg::S(*code)),
            _ => None,
        };
        let Some(mut v) = addr else {
            return term;
        };
        for _ in 0..4 {
            let Some(&Def::Inst(b, i)) = self.defs.get(&v) else {
                break;
            };
            let d = self.inst((b, i));
            let src = sources(d);
            match d.name() {
                "v_or_b32" | "s_or_b32" | "v_add_nc_u32" | "v_add_u32" | "v_add_co_u32" | "s_add_u32" | "s_add_i32"
                    if src.len() >= 2 =>
                {
                    match (arg(d, &src[0]), arg(d, &src[1])) {
                        (Arg::Const(k), Arg::Value(w)) | (Arg::Value(w), Arg::Const(k)) if k < 0x100 => {
                            term += i64::from(k);
                            v = w;
                        }
                        _ => break,
                    }
                }
                "v_mov_b32" | "s_mov_b32" if !src.is_empty() => match arg(d, &src[0]) {
                    Arg::Value(w) => v = w,
                    _ => break,
                },
                _ => break,
            }
        }
        term
    }

    /// What the code outside `region` does with `v`.
    ///
    /// A value is followed through the phis of the joins after the region,
    /// and a read in a block no lane holding it can reach is no use: a walk
    /// after this one loads a node's fields into a register only for the
    /// lanes at a node of some kinds, the lanes at a box keep what the
    /// register held, and the join after the load hands both on, but the code
    /// that reads those fields runs only for the kinds that loaded them
    /// ([`Facts::observable`]). Without that, every temporary of a later walk
    /// would count as a use of this one's values.
    fn after(&self, v: Value, region: &[bool], loops: &Loops, cache: &mut Seen) -> After {
        let mut out = After::default();
        let mut seen = HashSet::new();
        // (the value, and where it last went on in only some lanes)
        let mut work: Vec<(Value, Option<Hop>)> = vec![(v, None)];
        let mut steps = 0;
        while let Some((v, hop)) = work.pop() {
            if !seen.insert((v, hop)) {
                continue;
            }
            for &u in self.uses.get(&v).map_or(&[][..], |u| &u[..]) {
                steps += 1;
                if steps > 4000 {
                    return out;
                }
                match u {
                    Use::Phi(b, k, a) => {
                        if !region[b] {
                            let hop = (!loops.is_header(b)).then_some(Hop::Edge(b, a));
                            work.push((self.p.blocks[b].phis[k].value, hop));
                        }
                    }
                    Use::Inst(b, i) => {
                        if region[b] {
                            continue;
                        }
                        if let Some(hop) = hop
                            && !self.can_hold(hop, (b, i), loops, cache)
                        {
                            continue;
                        }
                        let inst = self.inst((b, i));
                        let name = inst.name();
                        // (the old value a guarded write keeps in the lanes it leaves out:
                        // passed on in its result, not read)
                        let operands = crate::ir::operand_reads(self.p, inst);
                        let old: Vec<Reg> = inst
                            .reads
                            .iter()
                            .filter(|&&(r, x)| x == v && matches!(r, Reg::V(_)) && !operands.contains(&r))
                            .map(|&(r, _)| r)
                            .collect();
                        if !old.is_empty() && inst.reads.iter().filter(|r| r.1 == v).count() == old.len() {
                            let hop = self.guard((b, i)).map(|g| Hop::Kept(b, g));
                            work.extend(old.iter().filter_map(|&r| inst.write(r)).map(|w| (w, hop)));
                            continue;
                        }
                        out.uses += 1;
                        if name.starts_with("v_cmp") || name.starts_with("s_cmp") {
                            out.compares += 1;
                        } else if is_store(inst) {
                            out.stores += 1;
                        } else if is_load(inst) {
                            out.loads.push(self.address_term((b, i)));
                        } else if matches!(inst.fields, Fields::Valu(_) | Fields::Sop { .. }) {
                            for &(r, w) in &inst.writes {
                                if !matches!(r, Reg::Scc) && !matches!(r, Reg::S(c) if c == EXEC || c == EXEC + 1) {
                                    work.push((w, None));
                                }
                            }
                        }
                    }
                }
            }
        }
        out.loads.sort_unstable();
        out.loads.dedup();
        out
    }

    /// Whether the code outside `region` only asks of `v`, a node pointer a
    /// hit saves, whether there was a hit and whether it was a triangle: it
    /// compares `v` for (in)equality with `miss`, its value on a miss, or
    /// takes its bit 2 (`& 4`: 0 for every triangle, set for the procedural
    /// kind 7), through phis, and nothing else. Any triangle's pointer other
    /// than `miss` then gives the program the same results. Uses are those
    /// [`Facts::after`] counts.
    fn only_tested(&self, v: Value, miss: u32, region: &[bool], loops: &Loops, cache: &mut Seen) -> bool {
        let mut seen = HashSet::new();
        let mut work: Vec<(Value, Option<Hop>)> = vec![(v, None)];
        while let Some((x, hop)) = work.pop() {
            if seen.len() > 512 {
                return false;
            }
            if !seen.insert((x, hop)) {
                continue;
            }
            for &u in self.uses.get(&x).map_or(&[][..], |u| &u[..]) {
                match u {
                    Use::Phi(b, k, a) => {
                        if !region[b] {
                            let hop = (!loops.is_header(b)).then_some(Hop::Edge(b, a));
                            work.push((self.p.blocks[b].phis[k].value, hop));
                        }
                    }
                    Use::Inst(b, i) => {
                        if region[b] {
                            continue;
                        }
                        if let Some(hop) = hop
                            && !self.can_hold(hop, (b, i), loops, cache)
                        {
                            continue;
                        }
                        let inst = self.inst((b, i));
                        let name = inst.name();
                        let src = sources(inst);
                        if src.len() < 2 || !plain(inst) || inst.reads.iter().filter(|r| r.1 == x).count() != 1 {
                            return false;
                        }
                        let k = match (arg(inst, &src[0]), arg(inst, &src[1])) {
                            (Arg::Const(k), Arg::Value(w)) | (Arg::Value(w), Arg::Const(k)) if w == x => k,
                            _ => return false,
                        };
                        let rest = name.strip_prefix("v_cmpx_").or_else(|| name.strip_prefix("v_cmp_"));
                        let tested = match rest {
                            Some("eq_u32" | "ne_u32" | "lg_u32" | "eq_i32" | "ne_i32" | "lg_i32") => k == miss,
                            _ => name == "v_and_b32" && k == 4,
                        };
                        if !tested {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Whether a lane that runs the instruction at `at` can hold a value that
    /// went on in only some lanes at `hop`: whether a lane can both have come
    /// into a join through that edge (or have been left out by that guarded
    /// write) and run `at`. Only answered "no" where the join or the write
    /// dominates `at` within the same loop iteration (the same innermost
    /// loop), so that the lane's conditions there and at `at` are over the
    /// same values; "yes" wherever anything is not known.
    fn can_hold(&self, hop: Hop, at: Site, loops: &Loops, cache: &mut Seen) -> bool {
        if let Some(&r) = cache.get(&(hop, at)) {
            return r;
        }
        let r = self.can_hold_anew(hop, at, loops);
        cache.insert((hop, at), r);
        r
    }

    fn can_hold_anew(&self, hop: Hop, at: Site, loops: &Loops) -> bool {
        let Some(st) = self.st else { return true };
        let y = at.0;
        let (by, mut all) = match hop {
            Hop::Edge(j, a) => {
                let Some(&from) = self.p.blocks[j].preds.get(a) else { return true };
                let Some(&at_from) = st.reach.get(from) else { return true };
                let mut all = vec![(at_from, true)];
                match st.exits.get(from) {
                    Some(&LaneExit::Branch { when, taken, not_taken }) => {
                        if taken == j && not_taken != j {
                            all.push((when, true));
                        } else if not_taken == j && taken != j {
                            all.push((when, false));
                        }
                    }
                    Some(LaneExit::Jump(_)) => {}
                    _ => return true,
                }
                (j, all)
            }
            Hop::Kept(w, g) => {
                let Some(&at_w) = st.reach.get(w) else { return true };
                (w, vec![(at_w, true), (g, false)])
            }
        };
        if !loops.dominates(by, y) || loops.innermost(by) != loops.innermost(y) {
            return true;
        }
        let Some(&at_y) = st.reach.get(y) else { return true };
        all.push((at_y, true));
        if let Some(g) = self.guard(at) {
            all.push((g, true));
        }
        self.meets(st, &all)
    }

    /// Whether some lane can meet every `(condition, holds)` of `all` at once,
    /// with the compares of one value against constants decided together
    /// (`ptr & 6 == 4` and `ptr & 7 < 4` are never both true, where the
    /// diagrams take the two compares for unrelated atoms). `false` only when
    /// no value of those compares' operands meets them all; other atoms are
    /// left free, and a search too large to finish answers `true`.
    fn meets(&self, st: &Structured, all: &[(Bool, bool)]) -> bool {
        if !st.bools.meet(all) {
            return false;
        }
        // The compares of a value (through ANDs with constants) with a constant.
        struct Cmp<'n> {
            atom: Atom,
            base: Value,
            mask: u32,
            name: &'n str,
            k: u32,
            k_first: bool,
        }
        let mut cmps: Vec<Cmp> = Vec::new();
        for &(b, _) in all {
            for atom in st.bools.support(b) {
                let Atom::Lane(v) = atom else { continue };
                if cmps.iter().any(|c| c.atom == atom) {
                    continue;
                }
                let Some(&Def::Inst(cb, ci)) = self.defs.get(&v) else { continue };
                let c = self.inst((cb, ci));
                let name = c.name();
                if !(name.starts_with("v_cmp_") || name.starts_with("v_cmpx_"))
                    || !(name.ends_with("_u32") || name.ends_with("_i32"))
                    || !plain(c)
                {
                    continue;
                }
                let src = sources(c);
                if src.len() < 2 {
                    continue;
                }
                let (k, x, k_first) = match (arg(c, &src[0]), arg(c, &src[1])) {
                    (Arg::Const(k), Arg::Value(x)) => (k, x, true),
                    (Arg::Value(x), Arg::Const(k)) => (k, x, false),
                    _ => continue,
                };
                let (base, mask) = self.masked(x);
                cmps.push(Cmp { atom, base, mask, name, k, k_first });
            }
        }
        if cmps.is_empty() {
            return true;
        }
        // Each base's candidate values: every low byte where a mask keeps only
        // low bits, and each constant and its neighbours.
        let mut bases: Vec<Value> = cmps.iter().map(|c| c.base).collect();
        bases.sort_unstable_by_key(|v| v.0);
        bases.dedup();
        let mut candidates: Vec<Vec<u32>> = Vec::new();
        let mut total: usize = 1;
        for &base in &bases {
            let mut c: Vec<u32> = vec![0, u32::MAX];
            if cmps.iter().any(|x| x.base == base && x.mask <= 0xff) {
                c.extend(0..=0xff);
            }
            for x in cmps.iter().filter(|x| x.base == base) {
                c.extend([x.k.wrapping_sub(1), x.k, x.k.wrapping_add(1)]);
            }
            c.sort_unstable();
            c.dedup();
            total = total.saturating_mul(c.len());
            candidates.push(c);
        }
        if total > 1 << 16 {
            return true;
        }
        let mut pick = vec![0usize; bases.len()];
        loop {
            let value = |base: Value| {
                let n = bases.iter().position(|&b| b == base).expect("a base");
                candidates[n][pick[n]]
            };
            let known = |atom: Atom| -> Option<bool> {
                let c = cmps.iter().find(|c| c.atom == atom)?;
                let x = value(c.base) & c.mask;
                if c.k_first { compare(c.name, c.k, x) } else { compare(c.name, x, c.k) }
            };
            let possible = all.iter().all(|&(b, holds)| {
                let mut memo = HashMap::new();
                !matches!(
                    (eval(st, b, &known, &mut memo), holds),
                    (Tri::True, false) | (Tri::False, true)
                )
            });
            if possible {
                return true;
            }
            // (the next assignment)
            let mut n = 0;
            loop {
                if n == pick.len() {
                    return false;
                }
                pick[n] += 1;
                if pick[n] < candidates[n].len() {
                    break;
                }
                pick[n] = 0;
                n += 1;
            }
        }
    }

    /// `x` without the ANDs with constants and the moves that made it: the
    /// value they were applied to and the bits they keep.
    fn masked(&self, mut x: Value) -> (Value, u32) {
        let mut mask = u32::MAX;
        for _ in 0..6 {
            let Some(&Def::Inst(b, i)) = self.defs.get(&x) else { break };
            let d = self.inst((b, i));
            let src = sources(d);
            match d.name() {
                "v_and_b32" if plain(d) && src.len() >= 2 => match (arg(d, &src[0]), arg(d, &src[1])) {
                    (Arg::Const(k), Arg::Value(w)) | (Arg::Value(w), Arg::Const(k)) => {
                        mask &= k;
                        x = w;
                    }
                    _ => break,
                },
                "v_mov_b32" if plain(d) && !src.is_empty() => match arg(d, &src[0]) {
                    Arg::Value(w) => x = w,
                    _ => break,
                },
                _ => break,
            }
        }
        (x, mask)
    }
}

/// `set` in order: what is read out of a set must not depend on how it was
/// hashed, or two runs of the census tell the same walk differently.
fn sorted<T: Ord>(set: HashSet<T>) -> Vec<T> {
    let mut v: Vec<T> = set.into_iter().collect();
    v.sort_unstable();
    v
}

/// The node tests of `p`.
fn sites(p: &Program) -> Vec<Site> {
    let mut out = Vec::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            if inst.name().starts_with("image_bvh") {
                out.push((b, i));
            }
        }
    }
    out
}

/// The walks of `p` (`st`: its structure, which the exit kind needs). Empty
/// for a program without node tests.
pub fn recognize(p: &Program, st: Option<&Structured>) -> Vec<Walk> {
    let all = sites(p);
    if all.is_empty() {
        return Vec::new();
    }
    let facts = Facts::new(p, st);
    let loops = Loops::new(p);
    let count = |l: &Loop| all.iter().filter(|s| l.contains(s.0)).count();
    // Each site's region: the innermost loop around it (mold A, the 64-bit
    // form), or the innermost one that holds a second site (mold B's pair).
    let mut regions: Vec<(Option<usize>, Vec<Site>)> = Vec::new();
    for &s in &all {
        let wide = facts.inst(s).name() == "image_bvh64_intersect_ray";
        let around = loops.around(s.0);
        let l = if wide { around.first().copied() } else { around.iter().copied().find(|l| count(l) >= 2) };
        let key = l.map(|l| l.header);
        match regions.iter_mut().find(|r| r.0 == key && key.is_some()) {
            Some(r) => r.1.push(s),
            None => regions.push((key, vec![s])),
        }
    }
    let mut out = Vec::new();
    for (header, sites) in regions {
        let l = header.and_then(|h| loops.loops.iter().find(|l| l.header == h));
        out.push(match l {
            Some(l) => walk(&facts, &loops, l, sites),
            None => lone(&facts, sites),
        });
    }
    out
}

/// A site outside any loop.
fn lone(facts: &Facts, sites: Vec<Site>) -> Walk {
    let s = sites[0];
    let offset = facts.inst(s).machine.offset;
    Walk {
        header: s.0,
        header_offset: offset,
        span: (offset, offset),
        blocks: 0,
        insts: 0,
        site_offsets: sites.iter().map(|&s| facts.inst(s).machine.offset).collect(),
        wide: facts.inst(s).name() == "image_bvh64_intersect_ray",
        sites,
        mold: Mold::Other,
        family: Family::Other,
        anchors: Anchors::default(),
        tex: 0,
        geo: 0,
        exits: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        exit: ExitKind::Unknown,
        exit_why: "no loop".into(),
        t_min: None,
        cull: None,
        body: Vec::new(),
        mask: Mask::Unknown("no loop".into()),
        descriptor_load: None,
        after: Vec::new(),
        refusals: vec!["a node test outside any loop".into()],
        fields: Vec::new(),
        extended: Vec::new(),
    }
}

/// What a triangle accept saves: the condition and the writes under it.
#[derive(Debug, Default)]
struct Accept {
    /// Register and what the write saves at an accept.
    saves: Vec<(u8, Saved)>,
    /// The registers an accept sets to a constant: the constant, and the one
    /// a reject sets (when one select gives both). The walk's state is among
    /// them.
    states: Vec<(u8, u32, Option<u32>)>,
    why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Saved {
    Const(u32),
    T,
    BaryI,
    BaryJ,
    NodePointer,
    Value(Value),
}

/// The triangle values of a site: `t`, the barycentrics, the compare
/// against t min.
#[derive(Debug, Default)]
struct Triangle {
    t: HashSet<Value>,
    i: HashSet<Value>,
    j: HashSet<Value>,
    /// The bound and the register the compare reads it from.
    t_min: Option<(Arg, Option<Reg>)>,
}

fn triangle(facts: &Facts, site: Site, region: &[bool]) -> Triangle {
    let inst = facts.inst(site);
    let Fields::Image { data: Operand::Vector { reg: d, .. }, .. } = inst.fields else {
        return Triangle::default();
    };
    let result = |k: u8| inst.write(Reg::V(d + k));
    let mut out = Triangle::default();
    let (Some(num), Some(den)) = (result(0), result(1)) else {
        return out;
    };
    let users = |v: Value| -> Vec<Site> {
        facts
            .uses
            .get(&v)
            .into_iter()
            .flatten()
            .filter_map(|u| match *u {
                Use::Inst(b, i) if region[b] => Some((b, i)),
                _ => None,
            })
            .collect()
    };
    for r in users(den) {
        let ri = facts.inst(r);
        if ri.name() != "v_rcp_f32" || !plain(ri) {
            continue;
        }
        let Some(Reg::V(rd)) = dst_reg(ri) else { continue };
        let Some(rcp) = ri.write(Reg::V(rd)) else { continue };
        for m in users(rcp) {
            let mi = facts.inst(m);
            if !matches!(mi.name(), "v_mul_legacy_f32" | "v_mul_f32") || !plain(mi) {
                continue;
            }
            let src = sources(mi);
            if src.len() < 2 {
                continue;
            }
            let other = match (arg(mi, &src[0]), arg(mi, &src[1])) {
                (Arg::Value(a), Arg::Value(b)) if a == rcp => b,
                (Arg::Value(a), Arg::Value(b)) if b == rcp => a,
                _ => continue,
            };
            let Some(Reg::V(md)) = dst_reg(mi) else { continue };
            let Some(prod) = mi.write(Reg::V(md)) else { continue };
            if other == num {
                out.t.insert(prod);
            } else if Some(other) == result(2) {
                out.i.insert(prod);
            } else if Some(other) == result(3) {
                out.j.insert(prod);
            }
        }
    }
    // t min: a compare that rejects t below a bound
    for &t in &out.t.clone() {
        for c in users(t) {
            let ci = facts.inst(c);
            let src = sources(ci);
            if src.len() < 2 || !ci.name().starts_with("v_cmp") || !ci.name().ends_with("_f32") || !plain(ci) {
                continue;
            }
            let (a, b) = (arg(ci, &src[0]), arg(ci, &src[1]));
            let k = match ci.name() {
                // t < bound, or bound > t
                "v_cmp_lt_f32" | "v_cmp_nge_f32" if a == Arg::Value(t) => 1,
                "v_cmp_gt_f32" | "v_cmp_nle_f32" if b == Arg::Value(t) => 0,
                _ => continue,
            };
            let reg = match src[k] {
                Operand::Vector { reg, .. } => Some(Reg::V(reg)),
                Operand::Scalar { code, .. } if code != NULL => Some(Reg::S(code)),
                _ => None,
            };
            out.t_min = Some((if k == 1 { b } else { a }, reg));
        }
    }
    out
}

/// The node pointer of a site: the 32-bit pointer whose kind the walk tests
/// (`ptr & 7`), which the 64-bit form adds the structure's base to.
fn pointer(facts: &Facts, site: Site, region: &[bool]) -> Option<Value> {
    let inst = facts.inst(site);
    let Fields::Image { addr, .. } = &inst.fields else {
        return None;
    };
    let first = match addr.first()? {
        Operand::Vector { reg, .. } => inst.read(Reg::V(*reg))?,
        _ => return None,
    };
    let tested = |v: Value| {
        facts.uses.get(&v).into_iter().flatten().any(|u| match *u {
            Use::Inst(b, i) if region[b] => {
                let ui = facts.inst((b, i));
                ui.name() == "v_and_b32"
                    && sources(ui).iter().any(|s| matches!(arg(ui, s), Arg::Const(6 | 7 | 14)))
            }
            _ => false,
        })
    };
    if tested(first) {
        return Some(first);
    }
    let Some(&Def::Inst(b, i)) = facts.defs.get(&first) else {
        return None;
    };
    let d = facts.inst((b, i));
    sources(d).iter().filter_map(|s| match arg(d, s) {
        Arg::Value(v) => Some(v),
        _ => None,
    }).find(|&v| tested(v))
}

fn accept(facts: &Facts, tri: &Triangle, region: &[bool], extent: &HashSet<Value>, pointers: &HashSet<Value>) -> Option<Accept> {
    let st = facts.st?;
    // The write that takes t as the new extent, and its condition.
    #[derive(Clone, Copy)]
    enum Cond {
        /// A select on `mask` (true: the accept takes src1), under `guard`.
        Select { guard: Option<Bool>, mask: Bool, pos: bool },
        /// A write under `guard`.
        Guarded(Bool),
        /// The writes of a block the accepted lanes enter.
        Block(usize),
    }
    let mut cond = None;
    let mut why = String::new();
    'find: for t in sorted(tri.t.clone()) {
        for u in facts.uses.get(&t).into_iter().flatten() {
            let Use::Inst(b, i) = *u else { continue };
            if !region[b] {
                continue;
            }
            let w = facts.inst((b, i));
            let Some(Reg::V(r)) = dst_reg(w) else { continue };
            let Some(result) = w.write(Reg::V(r)) else { continue };
            if !extent.contains(&result) {
                continue;
            }
            let src = sources(w);
            match w.name() {
                "v_cndmask_b32" if plain(w) && src.len() == 3 => {
                    let Arg::Value(m) = arg(w, &src[2]) else { continue };
                    let Some(&mask) = st.masks.get(&m) else {
                        why = "the extent's select is on an unknown mask".into();
                        continue;
                    };
                    let pos = arg(w, &src[1]) == Arg::Value(t);
                    cond = Some(Cond::Select { guard: facts.guard((b, i)), mask, pos });
                    break 'find;
                }
                "v_mov_b32" if plain(w) => {
                    cond = Some(match facts.guard((b, i)) {
                        Some(g) => Cond::Guarded(g),
                        None => Cond::Block(b),
                    });
                    break 'find;
                }
                _ => {}
            }
        }
    }
    let Some(cond) = cond else {
        return Some(Accept { why: if why.is_empty() { "no write takes t as the extent".into() } else { why }, ..Accept::default() });
    };
    let classify = |a: Arg| -> Saved {
        match a {
            Arg::Const(k) => Saved::Const(k),
            Arg::None => Saved::Value(Value(u32::MAX)),
            Arg::Value(v) => {
                let (roots, seen) = facts.roots(v, region);
                if tri.t.contains(&v) {
                    Saved::T
                } else if tri.i.contains(&v) {
                    Saved::BaryI
                } else if tri.j.contains(&v) {
                    Saved::BaryJ
                } else if seen.iter().any(|s| pointers.contains(s))
                    || roots.iter().any(|r| matches!(r, Root::Outside(o) if pointers.contains(o)))
                {
                    Saved::NodePointer
                } else {
                    Saved::Value(v)
                }
            }
        }
    };
    let mut out = Accept::default();
    let mut neg = HashMap::new();
    // (the reject's constant of a select between two)
    let mut rejects: HashMap<u8, u32> = HashMap::new();
    for (b, block) in facts.p.blocks.iter().enumerate() {
        if !region[b] {
            continue;
        }
        for (i, w) in block.insts.iter().enumerate() {
            let Some(Reg::V(r)) = dst_reg(w) else { continue };
            if !matches!(w.fields, Fields::Valu(_)) {
                continue;
            }
            let src = sources(w);
            let guard = facts.guard((b, i));
            let cnd = w.name() == "v_cndmask_b32" && plain(w) && src.len() == 3;
            let saved = match cond {
                Cond::Select { guard: g, mask, pos } if cnd && guard == g => {
                    let Arg::Value(m) = arg(w, &src[2]) else { continue };
                    let Some(&wm) = st.masks.get(&m) else { continue };
                    let (acc, rej) = if wm == mask {
                        if pos { (1, 0) } else { (0, 1) }
                    } else if negation(st, wm, mask, &mut neg) {
                        if pos { (0, 1) } else { (1, 0) }
                    } else {
                        continue;
                    };
                    let (a, j) = (arg(w, &src[acc]), arg(w, &src[rej]));
                    match (a, j) {
                        (Arg::Const(_), Arg::Const(kr)) => {
                            rejects.insert(r, kr);
                        }
                        _ => {
                            rejects.remove(&r);
                        }
                    }
                    classify(a)
                }
                Cond::Guarded(g) if guard == Some(g) => {
                    let v = w.write(Reg::V(r)).map_or(Arg::None, Arg::Value);
                    match w.name() {
                        "v_mov_b32" if plain(w) => classify(arg(w, &src[0])),
                        _ => classify(v),
                    }
                }
                Cond::Block(x) if x == b && guard.is_none() => {
                    let v = w.write(Reg::V(r)).map_or(Arg::None, Arg::Value);
                    match w.name() {
                        "v_mov_b32" if plain(w) => classify(arg(w, &src[0])),
                        _ => classify(v),
                    }
                }
                _ => continue,
            };
            if !matches!(cond, Cond::Select { .. }) {
                rejects.remove(&r);
            }
            // (in code order: the last write is what an accept leaves)
            out.saves.retain(|s| s.0 != r);
            out.saves.push((r, saved));
        }
    }
    for (r, saved) in &out.saves {
        if let Saved::Const(k) = *saved {
            out.states.push((*r, k, rejects.get(r).copied()));
        }
    }
    out.why = match cond {
        Cond::Select { .. } => "a select takes t".into(),
        Cond::Guarded(_) => "a guarded write takes t".into(),
        Cond::Block(_) => "a block of accepted lanes takes t".into(),
    };
    Some(out)
}

fn walk(facts: &Facts, loops: &Loops, l: &Loop, sites: Vec<Site>) -> Walk {
    let p = facts.p;
    let region = &l.body;
    let mut span = (usize::MAX, 0);
    let mut insts = 0;
    let mut anchors = Anchors::default();
    let (mut tex, mut geo) = (0, 0);
    let (mut and7, mut and6) = (false, false);
    let (mut min3, mut med3, mut max3) = (false, false, false);
    let (mut c52, mut c56) = (false, false);
    let mut writelane = false;
    let mut pops = 0;
    for (b, block) in p.blocks.iter().enumerate() {
        if !region[b] {
            continue;
        }
        for inst in &block.insts {
            insts += 1;
            let o = inst.machine.offset;
            span = (span.0.min(o), span.1.max(o));
            let name = inst.name();
            let src = sources(inst);
            let consts: Vec<u32> = src.iter().filter_map(|s| match arg(inst, s) {
                Arg::Const(k) => Some(k),
                _ => None,
            }).collect();
            match name {
                "v_and_b32" if consts.contains(&7) => and7 = true,
                "v_and_b32" if consts.contains(&6) || consts.contains(&14) => and6 = true,
                "v_min3_u32" => min3 = true,
                "v_med3_u32" => med3 = true,
                "v_max3_u32" => max3 = true,
                "ds_wrxchg_rtn_b32" => pops += 1,
                "v_writelane_b32" | "v_readlane_b32" | "s_bcnt1_i32_b32" | "s_bcnt1_i32_b64" => writelane = true,
                _ => {}
            }
            if matches!(inst.fields, Fields::Valu(_) | Fields::Sop { .. }) {
                c52 |= consts.contains(&52);
                c56 |= consts.contains(&56);
            }
            if let Some(off) = memory_offset(inst) {
                c52 |= off == 52;
                c56 |= off == 56;
            }
            if name.starts_with("s_cmpk_") && name.ends_with("_u32")
                && let Fields::Sop { imm: Some(imm @ (0xffff | 0xfffe)), .. } = inst.fields
            {
                anchors.cap = Some(imm);
            }
            match &inst.fields {
                Fields::Image { .. } if !name.starts_with("image_bvh") => tex += 1,
                Fields::Buffer { idxen: true, .. } if !is_store(inst) => geo += 1,
                _ => {}
            }
        }
    }
    anchors.kind = and7 && and6;
    anchors.sort = min3 && med3 && max3;
    anchors.pop = pops > 0;
    anchors.parent = c52 && c56;
    anchors.votes = writelane;
    let wide = sites.iter().all(|&s| facts.inst(s).name() == "image_bvh64_intersect_ray");
    let narrow = sites.iter().all(|&s| facts.inst(s).name() == "image_bvh_intersect_ray");
    anchors.descriptor = sites.iter().all(|&s| {
        let inst = facts.inst(s);
        let Fields::Image { rsrc: Operand::Scalar { code, dwords: 4.. }, .. } = inst.fields else {
            return false;
        };
        let word = |k: u8| inst.read(Reg::S(code + k)).and_then(|v| facts.konst(v));
        word(1) == Some(0x8000_0000) && word(3) == Some(0x8900_03ff)
    });
    let mold = if wide && sites.len() == 1 && anchors.pop && !anchors.votes {
        Mold::A
    } else if narrow && sites.len() == 2 && anchors.votes {
        Mold::B
    } else {
        Mold::Other
    };
    let family = match (mold, tex > 0, geo > 0) {
        (Mold::A, false, false) => Family::A0,
        (Mold::A, false, true) => Family::AF,
        (Mold::A, true, _) => Family::AA,
        (Mold::B, false, false) => Family::B0,
        (Mold::B, false, true) => Family::BF,
        (Mold::B, true, _) => Family::BA,
        _ => Family::Other,
    };

    // The values that cross the region's edge.
    let mut inputs: Vec<Live> = Vec::new();
    let mut outputs: Vec<Live> = Vec::new();
    let mut seen_in = HashSet::new();
    let mut seen_out = HashSet::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for phi in &block.phis {
            for &v in &phi.args {
                let inside = facts.inside(v, region);
                if region[b] && !inside && seen_in.insert(v) {
                    inputs.push(Live { reg: phi.reg, value: v, role: None, miss: None });
                }
                if !region[b] && inside && seen_out.insert(v) {
                    outputs.push(Live { reg: phi.reg, value: v, role: None, miss: None });
                }
            }
        }
        for i in 0..block.insts.len() {
            for &(reg, v) in facts.reads((b, i)) {
                let inside = facts.inside(v, region);
                if region[b] && !inside && seen_in.insert(v) {
                    inputs.push(Live { reg, value: v, role: None, miss: None });
                }
                if !region[b] && inside && seen_out.insert(v) {
                    outputs.push(Live { reg, value: v, role: None, miss: None });
                }
            }
        }
    }

    // The node loads at +48 of a node (an instance's ID and mask, then
    // contribution and flags, BLAS root and address) and at the transform:
    // the constant term of every load's address inside.
    let mut loads48: HashMap<Site, i64> = HashMap::new();
    for (b, block) in p.blocks.iter().enumerate() {
        if !region[b] {
            continue;
        }
        for (i, inst) in block.insts.iter().enumerate() {
            if is_load(inst) {
                loads48.insert((b, i), facts.address_term((b, i)));
            }
        }
    }

    // The site's operands: which inputs are the ray, and the pointer.
    let mut roles: HashMap<Value, Role> = HashMap::new();
    let mut pointers: HashSet<Value> = HashSet::new();
    let mut extent: HashMap<Site, HashSet<Value>> = HashMap::new();
    let mut ray_regs: HashSet<u8> = HashSet::new();
    let mut ray_versions: HashSet<Value> = HashSet::new();
    let first = sites[0];
    for &s in &sites {
        let inst = facts.inst(s);
        let Fields::Image { addr, .. } = &inst.fields else { continue };
        let mut regs = Vec::new();
        for a in addr {
            if let Operand::Vector { reg, dwords } = *a {
                regs.extend((0..dwords).map(|k| reg + k));
            }
        }
        let skip = if inst.name() == "image_bvh64_intersect_ray" { 2 } else { 1 };
        if regs.len() != skip + 10 {
            continue;
        }
        for (k, &r) in regs.iter().enumerate().skip(skip) {
            let Some(v) = inst.read(Reg::V(r)) else { continue };
            let role = match k - skip {
                0 => Role::Extent,
                n @ 1..=3 => Role::Origin((n - 1) as u8),
                n @ 4..=6 => Role::Direction((n - 4) as u8),
                n => Role::Inverse((n - 7) as u8),
            };
            let (roots, seen) = facts.roots(v, region);
            if role == Role::Extent {
                extent.entry(s).or_default().extend(seen.iter().copied());
            }
            if matches!(role, Role::Origin(_) | Role::Direction(_)) {
                ray_regs.insert(r);
                ray_versions.extend(seen.iter().copied());
            }
            // the first site's world-space ray (mold B: the top's)
            if s == first {
                for root in sorted(roots) {
                    if let Root::Outside(o) = root {
                        roles.entry(o).or_insert(role.clone());
                    }
                }
            }
        }
        if let Some(ptr) = pointer(facts, s, region) {
            let (roots, seen) = facts.roots(ptr, region);
            pointers.extend(seen);
            if s == first {
                for root in sorted(roots) {
                    if let Root::Outside(o) = root {
                        roles.entry(o).or_insert(Role::Root);
                    }
                }
            }
        }
    }
    // The base address and the descriptor it is made of: the scalar loads of
    // four dwords outside the region whose dwords the region reads, through
    // the masks that take the address apart.
    let mut descriptor_load = None;
    for live in &inputs {
        let Reg::S(_) = live.reg else { continue };
        let mut v = live.value;
        for _ in 0..4 {
            match facts.defs.get(&v) {
                Some(&Def::Inst(b, i)) => {
                    let d = facts.inst((b, i));
                    if d.name() == "s_load_dwordx4" || d.name() == "s_load_dwordx8" {
                        descriptor_load.get_or_insert(d.machine.offset);
                        roles.entry(live.value).or_insert(Role::Structure);
                        break;
                    }
                    let next = sources(d).iter().find_map(|s| match arg(d, s) {
                        Arg::Value(w) => Some(w),
                        _ => None,
                    });
                    match (d.name(), next) {
                        ("s_and_b32" | "s_or_b32" | "s_mov_b32" | "s_lshr_b64" | "s_lshl_b64" | "s_bfe_u32" | "s_lshr_b32", Some(w)) => {
                            v = w
                        }
                        _ => break,
                    }
                }
                _ => break,
            }
        }
    }
    for live in &mut inputs {
        if matches!(live.reg, Reg::S(c) if c == EXEC) {
            live.role = Some(Role::Lanes);
        } else if let Some(r) = roles.get(&live.value) {
            live.role = Some(r.clone());
        }
    }

    // The triangle test, the accept, the exit kind.
    let mut tri = Triangle::default();
    let mut acc = None;
    for &s in &sites {
        let t = triangle(facts, s, region);
        if t.t.is_empty() {
            continue;
        }
        let a = accept(facts, &t, region, extent.get(&s).unwrap_or(&HashSet::new()), &pointers);
        let found = a.as_ref().is_some_and(|a| !a.saves.is_empty());
        tri = t;
        acc = a;
        if found {
            break;
        }
    }
    let t_min = match tri.t_min {
        Some((Arg::Const(k), _)) => Some(Bound::Const(k)),
        Some((Arg::Value(v), Some(r))) if !facts.inside(v, region) => Some(Bound::Value(r, v)),
        _ => None,
    };
    let (exit, exit_why) = exit_kind(facts, l, acc.as_ref(), tri.t.is_empty());

    // The outputs' roles.
    let saves: HashMap<u8, Saved> = acc.as_ref().map(|a| a.saves.iter().cloned().collect()).unwrap_or_default();
    let mut register_cap: HashSet<Value> = HashSet::new();
    for live in &mut outputs {
        let (roots, seen) = facts.roots(live.value, region);
        let uncapped = not_capped(facts, live.value, region);
        let inside: Vec<Site> = sorted(roots.clone()).iter().filter_map(|r| match r {
            Root::Inside(s) => Some(*s),
            _ => None,
        }).collect();
        let consts: HashSet<u32> = roots.iter().filter_map(|r| match r {
            Root::Const(k) => Some(*k),
            _ => None,
        }).collect();
        let only_outside = inside.is_empty() && consts.is_empty();
        live.role = Some(match live.reg {
            Reg::S(c) if c == EXEC || c == EXEC + 1 => Role::Lanes,
            Reg::V(r) if saves.contains_key(&r) => match &saves[&r] {
                Saved::T => Role::T,
                Saved::BaryI => Role::BaryI,
                Saved::BaryJ => Role::BaryJ,
                Saved::NodePointer => Role::HitNode,
                Saved::Const(_) => Role::State,
                Saved::Value(v) if facts.hit_kind(*v) => Role::HitKind,
                Saved::Value(v) if facts.made_of_transform(*v, region, &loads48) => Role::ObjectSpace,
                Saved::Value(_) => Role::Other("saved at a hit".into()),
            },
            Reg::V(r) if ray_regs.contains(&r) && seen.iter().any(|v| ray_versions.contains(v)) => Role::Ray,
            _ if only_outside && roots.len() == 1 => Role::Entry,
            _ if only_outside => Role::Other("one of several entry values".into()),
            _ if inside.is_empty() && consts.len() == 1 && roots.len() == 1 => Role::Constant(*consts.iter().next().expect("one")),
            _ if seen.iter().any(|v| pointers.contains(v)) => Role::NodePointer,
            _ if inside.iter().any(|&s| {
                let w = facts.inst(s);
                w.writes.iter().any(|&(_, v)| facts.made_of_transform(v, region, &loads48))
            }) => Role::ObjectSpace,
            _ if uncapped.is_some() => {
                let (k, register) = uncapped.unwrap_or((0, false));
                if register {
                    register_cap.insert(live.value);
                }
                Role::Constant(k)
            }
            _ if inside.is_empty() => Role::State,
            _ => {
                let names: Vec<&str> = inside.iter().map(|&s| facts.inst(s).name()).collect();
                Role::Other(format!("computed ({})", names.first().copied().unwrap_or("?")))
            }
        });
    }

    // The instance mask: what the walk tests an instance's first dword with.
    let mask = instance_mask(facts, region, &loads48);
    let cull = cull_factor(facts, region);

    // The code after the region; an output nothing after it reads (only the
    // old value of a guarded write keeps it, or a phi no one reads) is no
    // output.
    let mut after = Vec::new();
    let mut seen = HashMap::new();
    outputs.retain(|live| {
        if matches!(live.reg, Reg::S(c) if c == EXEC || c == EXEC + 1) || live.reg == Reg::Scc {
            return true;
        }
        let a = facts.after(live.value, region, loops, &mut seen);
        if a.uses == 0 {
            return false;
        }
        after.push((live.reg, a));
        true
    });
    after.sort_by_key(|(r, _)| *r);

    // Each output's value on a miss: the constant its register held at the
    // region's entry, the same along every edge into the header.
    for live in &mut outputs {
        let Some(phi) = p.blocks[l.header].phis.iter().find(|ph| ph.reg == live.reg) else { continue };
        let mut miss = None;
        for (k, &a) in phi.args.iter().enumerate() {
            if l.contains(p.blocks[l.header].preds[k]) {
                continue;
            }
            match facts.konst(a) {
                Some(c) if miss.is_none_or(|m| m == c) => miss = Some(c),
                _ => {
                    miss = None;
                    break;
                }
            }
        }
        live.miss = miss;
    }

    // What keeps the region from being a query.
    let mut refusals = Vec::new();
    // (what the hit's node fields read after the walk, made from a query, lift)
    let mut by_fields = Vec::new();
    for live in outputs.iter().filter(|l| l.role == Some(Role::HitNode)) {
        match live.miss {
            None => refusals.push(format!("a hit node ({:?}) whose miss value is no constant", live.reg)),
            Some(m) if !facts.only_tested(live.value, m, region, loops, &mut seen) => {
                by_fields.push(format!("a hit node ({:?}) read otherwise than tested for a hit", live.reg))
            }
            Some(_) => {}
        }
    }
    if mold == Mold::Other {
        refusals.push(format!("mold not recognized ({} sites, {} pops, votes {})", sites.len(), pops, anchors.votes));
    }
    let missing: Vec<&str> = [
        ("descriptor", anchors.descriptor),
        ("kind", anchors.kind),
        ("sort", anchors.sort),
        ("pop", anchors.pop),
        ("parent", anchors.parent),
    ]
    .iter()
    .filter(|a| !a.1)
    .map(|a| a.0)
    .collect();
    if !missing.is_empty() {
        refusals.push(format!("anchors missing: {}", missing.join(",")));
    }
    let targets: HashSet<usize> = l.exits.iter().map(|e| e.1).collect();
    if targets.len() != 1 {
        refusals.push(format!("{} exit targets", targets.len()));
    }
    if exit == ExitKind::Unknown {
        refusals.push(format!("exit kind unknown: {exit_why}"));
    }
    if cull.is_none() {
        refusals.push("face culling not found".into());
    }
    if t_min.is_none() {
        refusals.push("t min not found".into());
    }
    match mask {
        Mask::Unknown(_) => refusals.push("instance mask not found".into()),
        Mask::Value(_) => refusals.push("an instance mask that is no constant".into()),
        Mask::Constant(_) => {}
    }
    if descriptor_load.is_none() {
        refusals.push("the structure's descriptor load not found".into());
    }
    for live in &outputs {
        match &live.role {
            Some(r) if r.has_accessor() => {}
            Some(Role::Ray | Role::ObjectSpace) => refusals.push(format!("output {:?} in object space", live.reg)),
            Some(r) => refusals.push(format!("output {:?} ({})", live.reg, r.text())),
            None => refusals.push(format!("output {:?} without a role", live.reg)),
        }
    }
    for (r, a) in &after {
        if a.stores > 0 && outputs.iter().any(|l| l.reg == *r && matches!(l.role, Some(Role::HitNode | Role::NodePointer))) {
            refusals.push(format!("a node pointer ({r:?}) reaches a store"));
        }
        let hit = outputs.iter().any(|l| l.reg == *r && l.role == Some(Role::HitNode));
        let node = outputs.iter().any(|l| l.reg == *r && l.role == Some(Role::NodePointer));
        if !a.loads.is_empty() && (hit || node) {
            let offs: Vec<String> = a.loads.iter().map(|o| format!("+{o}")).collect();
            let why = format!("node fields read after the walk through {r:?}: {}", offs.join(" "));
            if node { refusals.push(why) } else { by_fields.push(why) }
        }
    }
    // The hit's node fields, where the code after reads them: what a query
    // gives in their place (`fields.rs`), or why not.
    let mut fields = Vec::new();
    let mut extended = Vec::new();
    if !by_fields.is_empty() {
        match fields::fields(facts, loops, region, &outputs, descriptor_load, &mut seen) {
            Ok(f) if !f.is_empty() => {
                fields = f;
                extended.push("node fields");
            }
            Ok(_) => refusals.extend(by_fields),
            Err(why) => {
                refusals.extend(by_fields);
                refusals.push(format!("node fields: {why}"));
            }
        }
    }
    if outputs.iter().any(|l| l.role == Some(Role::HitKind)) {
        extended.push("hit kind");
    }
    if outputs.iter().any(|l| matches!(l.role, Some(Role::BaryI | Role::BaryJ))) {
        extended.push("barycentrics");
    }
    if outputs.iter().any(|l| register_cap.contains(&l.value)) {
        extended.push("a cap in a register");
    }
    refusals.sort();
    refusals.dedup();

    let mut exits = l.exits.clone();
    exits.sort_unstable();
    Walk {
        header: l.header,
        header_offset: p.blocks[l.header].offset,
        span,
        blocks: l.size,
        insts,
        site_offsets: sites.iter().map(|&s| facts.inst(s).machine.offset).collect(),
        sites,
        wide,
        mold,
        family,
        anchors,
        tex,
        geo,
        exits,
        inputs,
        outputs,
        exit,
        exit_why,
        t_min,
        cull,
        body: region.clone(),
        mask,
        descriptor_load,
        after,
        refusals,
        fields,
        extended,
    }
}

/// Whether the walk ends at its first accepted triangle: the state an accept
/// sets, tried on the loop's own exit test.
fn exit_kind(facts: &Facts, l: &Loop, acc: Option<&Accept>, no_triangle: bool) -> (ExitKind, String) {
    if no_triangle {
        return (ExitKind::Unknown, "no triangle t found".into());
    }
    let Some(st) = facts.st else {
        return (ExitKind::Unknown, "no structure".into());
    };
    let Some(acc) = acc else {
        return (ExitKind::Unknown, "no accept".into());
    };
    if acc.saves.is_empty() {
        return (ExitKind::Unknown, acc.why.clone());
    }
    if acc.states.is_empty() {
        return (ExitKind::Closest, format!("{}; an accept sets no constant", acc.why));
    }
    // Whether a lane leaves the loop with register `reg` at `k`: over every
    // edge out, the lane's condition for taking it, with the compares of that
    // register against constants known. (`touched`: some compare was.)
    let leaves = |reg: u8, k: u32, touched: &mut bool| -> Tri {
        let mut any = Tri::False;
        for &(b, x) in &l.exits {
            let LaneExit::Branch { when, taken, not_taken } = st.exits[b] else {
                // (a jump or an end out of the loop: taken by every lane there)
                any = Tri::Depends;
                continue;
            };
            let on = if taken == x {
                true
            } else if not_taken == x {
                false
            } else {
                any = Tri::Depends;
                continue;
            };
            let hit = std::cell::Cell::new(false);
            let known = |a: Atom| -> Option<bool> {
                let Atom::Lane(v) = a else { return None };
                let Some(&Def::Inst(cb, ci)) = facts.defs.get(&v) else { return None };
                let c = facts.inst((cb, ci));
                let src = sources(c);
                if src.len() < 2 {
                    return None;
                }
                let is_state = |s: &Operand| matches!(s, Operand::Vector { reg: r, .. } if *r == reg);
                let r = match (arg(c, &src[0]), arg(c, &src[1])) {
                    (Arg::Const(x), Arg::Value(_)) if is_state(&src[1]) => compare(c.name(), x, k),
                    (Arg::Value(_), Arg::Const(x)) if is_state(&src[0]) => compare(c.name(), k, x),
                    _ => None,
                };
                if r.is_some() {
                    hit.set(true);
                }
                r
            };
            let mut memo = HashMap::new();
            let t = eval(st, when, &known, &mut memo);
            *touched |= hit.get();
            let t = match (t, on) {
                (Tri::True, false) => Tri::False,
                (Tri::False, false) => Tri::True,
                (t, _) => t,
            };
            any = match (any, t) {
                (Tri::True, _) | (_, Tri::True) => Tri::True,
                (Tri::False, Tri::False) => Tri::False,
                _ => Tri::Depends,
            };
        }
        any
    };
    let mut said = acc.why.clone();
    let mut kind = None;
    let mut all_equal = true;
    let mut any_touched = false;
    for &(reg, k_acc, k_rej) in &acc.states {
        let mut touched = false;
        let on_accept = leaves(reg, k_acc, &mut touched);
        let on_reject = k_rej.map(|k| leaves(reg, k, &mut touched));
        any_touched |= touched;
        let _ = write!(
            said,
            "; v{reg} = {k_acc}{} at an accept: leaves {on_accept:?}{}",
            k_rej.map_or(String::new(), |k| format!(" ({k} at a reject)")),
            on_reject.map_or(String::new(), |t| format!(", at a reject {t:?}"))
        );
        if !touched {
            continue;
        }
        if on_accept == Tri::True && on_reject != Some(Tri::True) {
            kind = Some(ExitKind::First);
        }
        if k_rej != Some(k_acc) && on_reject != Some(on_accept) {
            all_equal = false;
        }
    }
    match kind {
        Some(k) => (k, said),
        None if !any_touched => (ExitKind::Closest, format!("{said}; no exit test reads what an accept sets [inf]")),
        None if all_equal => (ExitKind::Closest, said),
        None => (ExitKind::Unknown, said),
    }
}

/// The instance inclusion mask the region tests an instance's first dword
/// (+48: ID, and the mask in the top byte) against.
fn instance_mask(facts: &Facts, region: &[bool], loads: &HashMap<Site, i64>) -> Mask {
    // The values that are an instance's dword +48 (a load at +48, its first dword).
    let mut first: HashSet<Value> = HashSet::new();
    for (&(b, i), &off) in loads {
        if off != 48 {
            continue;
        }
        let inst = facts.inst((b, i));
        let dst = match &inst.fields {
            Fields::Flat { dst: Some(d), .. } => Some(*d),
            Fields::Smem { data: Some(d), .. } => Some(*d),
            _ => None,
        };
        match dst {
            Some(Operand::Vector { reg, .. }) => first.extend(inst.write(Reg::V(reg))),
            Some(Operand::Scalar { code, .. }) => first.extend(inst.write(Reg::S(code))),
            _ => {}
        }
    }
    // (and the phis and moves inside that pass it on)
    let mut work: Vec<Value> = first.iter().copied().collect();
    while let Some(v) = work.pop() {
        for u in facts.uses.get(&v).into_iter().flatten() {
            let next = match *u {
                Use::Phi(b, k, _) if region[b] => Some(facts.p.blocks[b].phis[k].value),
                Use::Inst(b, i) if region[b] => {
                    let inst = facts.inst((b, i));
                    match (inst.name(), dst_reg(inst)) {
                        ("v_mov_b32" | "s_mov_b32", Some(r)) if plain(inst) => inst.write(r),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(n) = next
                && first.len() < 256
                && first.insert(n)
            {
                work.push(n);
            }
        }
    }
    let mut found: Vec<Mask> = Vec::new();
    for &v in &first {
        for u in facts.uses.get(&v).into_iter().flatten() {
            let Use::Inst(b, i) = *u else { continue };
            if !region[b] {
                continue;
            }
            let inst = facts.inst((b, i));
            let src = sources(inst);
            if src.len() < 2 {
                continue;
            }
            let (a, c) = (arg(inst, &src[0]), arg(inst, &src[1]));
            let k = match (a, c) {
                (Arg::Const(k), Arg::Value(w)) | (Arg::Value(w), Arg::Const(k)) if w == v => Some(k),
                _ => None,
            };
            let other = match (a, c) {
                (Arg::Value(x), Arg::Value(y)) if x == v => Some(y),
                (Arg::Value(x), Arg::Value(y)) if y == v => Some(x),
                _ => None,
            };
            let m = match (inst.name(), k) {
                ("v_and_b32" | "s_and_b32", Some(k)) if k & 0x00ff_ffff == 0 && k != 0 => Some(Mask::Constant((k >> 24) as u8)),
                ("v_lshrrev_b32", _) if matches!(a, Arg::Const(24..=31)) => match a {
                    Arg::Const(s) => Some(Mask::Constant((0xffu32 << (s - 24)) as u8)),
                    _ => None,
                },
                ("s_lshr_b32", _) if matches!(c, Arg::Const(24..=31)) => match c {
                    Arg::Const(s) => Some(Mask::Constant((0xffu32 << (s - 24)) as u8)),
                    _ => None,
                },
                // (dword > 2^n - 1: some bit from n up, the mask's bits from n - 24 up)
                ("v_cmp_lt_u32" | "v_cmp_gt_u32", Some(k)) if k.wrapping_add(1).is_power_of_two() && k >= 0x00ff_ffff => {
                    let below = (inst.name() == "v_cmp_lt_u32") == matches!(a, Arg::Const(_));
                    let n = (k.wrapping_add(1)).trailing_zeros();
                    (below && (24..32).contains(&n)).then(|| Mask::Constant((0xffu32 << (n - 24)) as u8))
                }
                ("v_and_b32" | "s_and_b32", None) => other.map(Mask::Value),
                _ => None,
            };
            if let Some(m) = m
                && !found.contains(&m)
            {
                found.push(m);
            }
        }
    }
    match found.len() {
        1 => found.pop().expect("one"),
        0 => Mask::Unknown("no test of an instance's +48".into()),
        n => {
            let mut said: Vec<String> = found.iter().map(|m| format!("{m:?}")).collect();
            said.sort();
            Mask::Unknown(format!("{n} tests: {}", said.join(", ")))
        }
    }
}

/// The ray's face culling as the walk compiled it: the constant that the
/// instance's facing (`TRIANGLE_CULL_DISABLE` and `FRONT_COUNTERCLOCKWISE`
/// from its flags, as -1, 0 or 1) is multiplied by before the triangle's
/// denominator's sign is (`v_mul_i32_i24 x, k, y`, k one of -1, 0, 1).
/// `None` when there is not exactly one such multiply.
fn cull_factor(facts: &Facts, region: &[bool]) -> Option<i32> {
    let mut found = Vec::new();
    for (b, block) in facts.p.blocks.iter().enumerate() {
        if !region[b] {
            continue;
        }
        for inst in &block.insts {
            if inst.name() != "v_mul_i32_i24" || !plain(inst) {
                continue;
            }
            let src = sources(inst);
            if src.len() < 2 {
                continue;
            }
            let k = match (arg(inst, &src[0]), arg(inst, &src[1])) {
                (Arg::Const(k), Arg::Value(_)) | (Arg::Value(_), Arg::Const(k)) => k as i32,
                _ => continue,
            };
            if (-1..=1).contains(&k) {
                found.push(k);
            }
        }
    }
    found.dedup();
    match found[..] {
        [k] => Some(k),
        _ => None,
    }
}

/// What `v` holds when the walk ends before its iteration cap: for a value
/// made only of constants, lane tests and the cap's own test (`s_cmpk_*_u32
/// x, 0xfffe` or `0xffff`), such as the flag some walks raise when they stop
/// at the cap. `None` for any other value, and for one the cap's test does
/// not reach.
fn not_capped(facts: &Facts, v: Value, region: &[bool]) -> Option<(u32, bool)> {
    let mut touched = 0;
    let k = uncapped(facts, v, region, 0, &mut touched)?;
    (touched != 0).then_some((k, touched & 2 != 0))
}

/// [`not_capped`]'s evaluation; lane masks are the lane's own bit (0 or 1).
fn uncapped(facts: &Facts, v: Value, region: &[bool], depth: u32, touched: &mut u8) -> Option<u32> {
    if depth > 12 {
        return None;
    }
    let (b, i) = match facts.defs.get(&v) {
        Some(&Def::Inst(b, i)) => (b, i),
        // (a join of paths that all give the same)
        Some(&Def::Phi(b, k)) if region[b] => {
            let mut all = None;
            for &a in &facts.p.blocks[b].phis[k].args {
                let x = uncapped(facts, a, region, depth + 1, touched)?;
                if all.is_some_and(|y| y != x) {
                    return None;
                }
                all = Some(x);
            }
            return all;
        }
        _ => return None,
    };
    if !region[b] {
        return None;
    }
    let inst = facts.inst((b, i));
    let src = sources(inst);
    let val = |op: &Operand, touched: &mut u8| -> Option<u32> {
        match *op {
            // (a lane that runs has its own bit of EXEC)
            Operand::Scalar { code, .. } if code == EXEC => Some(1),
            _ => match arg(inst, op) {
                Arg::Const(k) => Some(k),
                Arg::Value(w) => uncapped(facts, w, region, depth + 1, touched),
                Arg::None => None,
            },
        }
    };
    let name = inst.name();
    match name {
        n if n.starts_with("s_cmpk_") && n.ends_with("_u32") => {
            let Fields::Sop { imm: Some(0xfffe | 0xffff), .. } = inst.fields else {
                return None;
            };
            *touched |= 1;
            // (the count is small: below the cap)
            Some(u32::from(n.contains("_lt_") || n.contains("_le_")))
        }
        // A cap the program holds in a register (a bound from before the
        // region, which the count of the walk's rounds is tested against):
        // the count is below it. (`touched` bit 2: a host query never stops
        // at such a cap, which the title may have set low on purpose.)
        n if n.starts_with("s_cmp_") && n.ends_with("_u32") && src.len() >= 2 => {
            let counts = |o: &Operand| {
                let Arg::Value(w) = arg(inst, o) else { return false };
                let Some(&Def::Inst(cb, ci)) = facts.defs.get(&w) else { return false };
                let c = facts.inst((cb, ci));
                region[cb]
                    && matches!(c.name(), "s_add_i32" | "s_add_u32")
                    && sources(c).iter().any(|s| arg(c, s) == Arg::Const(1))
            };
            let bound = |o: &Operand| matches!(arg(inst, o), Arg::Value(w) if !facts.inside(w, region));
            let (x, y) = if counts(&src[0]) && bound(&src[1]) {
                (1, u32::MAX)
            } else if bound(&src[0]) && counts(&src[1]) {
                (u32::MAX, 1)
            } else {
                return None;
            };
            *touched |= 2;
            compare(&format!("v_cmp_{}", &n["s_cmp_".len()..]), x, y).map(u32::from)
        }
        "s_cselect_b32" | "s_cselect_b64" if src.len() >= 2 => {
            let scc = inst.read(Reg::Scc)?;
            let c = uncapped(facts, scc, region, depth + 1, touched)?;
            val(&src[if c != 0 { 0 } else { 1 }], touched)
        }
        "v_and_b32" | "s_and_b32" | "s_and_b64" if src.len() >= 2 => Some(val(&src[0], touched)? & val(&src[1], touched)?),
        "v_mov_b32" | "s_mov_b32" | "s_mov_b64" if !src.is_empty() && plain(inst) => val(&src[0], touched),
        "v_cndmask_b32" if src.len() == 3 && plain(inst) => {
            let m = val(&src[2], touched)?;
            val(&src[if m & 1 != 0 { 1 } else { 0 }], touched)
        }
        n if n.starts_with("v_cmp_") && src.len() >= 2 => {
            let (x, y) = (val(&src[0], touched)?, val(&src[1], touched)?);
            compare(n, x, y).map(u32::from)
        }
        _ => None,
    }
}
