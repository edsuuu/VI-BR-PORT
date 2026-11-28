//! Linked programs: what device method 0x1c8 gets
//! ([`crate::native::linked_program`]). They are the code as the hardware
//! runs it, and come without a container: no signatures, no stage data. What
//! those would say is derived here, from the code and from the pipeline
//! state the title passes beside it ([`LinkedIo`]).
//!
//! **Vertex inputs.** The vertex fetch is inlined. `s[0:1]` is the address
//! of a table of vertex buffer descriptors, 16 bytes a slot (the code masks
//! `s1` to its low 8 bits); a slot's descriptor is loaded with
//! `s_load_dwordx4`, and each attribute with a buffer load indexed by
//! `v0 + s2` (the vertex index plus the draw's base vertex) at the
//! attribute's byte offset: whole dwords (`buffer_load_dwordx<n>`) for
//! 32-bit formats, `tbuffer_load_format_*` with the format otherwise. Each
//! such load is one element of the pipeline's input layout, by slot and
//! offset. [`fetch`] rewrites them away: what a load wrote becomes the entry
//! value of a VGPR of its own ([`attribute_vgpr`]), which is how a
//! container's vertex program gets its attributes, so that the host's input
//! assembler does the fetch with the title's own layout and nothing later
//! needs to know. Whatever does not fit that picture is refused.
//!
//! **The link.** Stages meet by number: the pixel stage's attribute k reads
//! the vertex program's `exp param<n>`, n the offset in
//! `SPI_PS_INPUT_CNTL_<k>`. Both stages name parameter n `TEXCOORD<n>` in
//! register n + 1, after the position in register 0, so that each stage's
//! signature follows from its own code.
//! shortcut: every parameter is declared a float. A parameter that carries
//! integers keeps its bits only where the host moves a constant float input
//! unchanged.
//!
//! **Pixel outputs.** `exp mrt<n>` is render target n. The exports are
//! specialised to the targets' formats (`SPI_SHADER_COL_FORMAT`); the one
//! form taken is two 16-bit floats per dword (`compr`, FP16_ABGR).

use std::collections::{HashMap, HashSet};

use crate::ShaderError;
use crate::gfx10::{Fields, NULL, Operand};
use crate::ir::{Program, Reg, Value};
use crate::native::{HwStage, LinkedIo, NativeProgram, PsInput, StageIo, Usage, VsOutput};

/// The first VGPR that stands for an attribute channel once [`fetch`] has
/// run: no linked program seen uses half as many registers.
const ATTRIBUTE_VGPRS: u8 = 128;

/// `SPI_PS_INPUT_CNTL`: the parameter read, and flat shading.
const CNTL_OFFSET: u32 = 0x1F;
const CNTL_FLAT: u32 = 1 << 10;

/// `SPI_SHADER_COL_FORMAT` of a target the program exports two 16-bit
/// floats per dword to.
pub(crate) const COL_FP16: u32 = 4;

/// The VGPR whose entry value is channel `channel` of input element `row`
/// after [`fetch`].
pub(crate) fn attribute_vgpr(row: u8, channel: u8) -> u8 {
    ATTRIBUTE_VGPRS + 4 * row + channel
}

/// The signature register of vertex parameter `param` (`exp param<n>`):
/// after the position's.
pub(crate) fn param_row(param: u8) -> u8 {
    param + 1
}

/// How the channels of a vertex format convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Numeric {
    Unorm,
    Snorm,
    Uint,
    Sint,
    Float,
}

/// A vertex element's DXGI format: its channels' bits, their count and how
/// they convert. Only formats of equal channels in memory order: the code
/// may fetch any run of them on its own (the title's fetches one byte of a
/// colour as `8_UNORM`).
pub(crate) fn vertex_format(format: u32) -> Option<(u8, u8, Numeric)> {
    use Numeric::*;
    Some(match format {
        2 => (32, 4, Float),
        3 => (32, 4, Uint),
        4 => (32, 4, Sint),
        6 => (32, 3, Float),
        7 => (32, 3, Uint),
        8 => (32, 3, Sint),
        10 => (16, 4, Float),
        11 => (16, 4, Unorm),
        12 => (16, 4, Uint),
        13 => (16, 4, Snorm),
        14 => (16, 4, Sint),
        16 => (32, 2, Float),
        17 => (32, 2, Uint),
        18 => (32, 2, Sint),
        28 => (8, 4, Unorm),
        30 => (8, 4, Uint),
        31 => (8, 4, Snorm),
        32 => (8, 4, Sint),
        34 => (16, 2, Float),
        35 => (16, 2, Unorm),
        36 => (16, 2, Uint),
        37 => (16, 2, Snorm),
        38 => (16, 2, Sint),
        41 => (32, 1, Float),
        42 => (32, 1, Uint),
        43 => (32, 1, Sint),
        49 => (8, 2, Unorm),
        50 => (8, 2, Uint),
        51 => (8, 2, Snorm),
        52 => (8, 2, Sint),
        54 => (16, 1, Float),
        56 => (16, 1, Unorm),
        57 => (16, 1, Uint),
        58 => (16, 1, Snorm),
        59 => (16, 1, Sint),
        61 => (8, 1, Unorm),
        62 => (8, 1, Uint),
        63 => (8, 1, Snorm),
        64 => (8, 1, Sint),
        _ => return None,
    })
}

/// The GFX10 buffer format a typed load names, as [`vertex_format`] gives a
/// DXGI one: families `8`, `16`, `8_8`, `32`, `16_16`, then from 56
/// `8_8_8_8`, `32_32`, `16_16_16_16`, `32_32_32`, `32_32_32_32`, each with
/// unorm, snorm, uscaled, sscaled, uint, sint, float as far as it has them
/// (the 32-bit ones: uint, sint, float). None for the scaled forms, which no
/// input element has, and for the families of mixed channels.
fn buffer_format(format: u8) -> Option<(u8, u8, Numeric)> {
    use Numeric::*;
    const NARROW: [Option<Numeric>; 7] =
        [Some(Unorm), Some(Snorm), None, None, Some(Uint), Some(Sint), Some(Float)];
    const WIDE: [Option<Numeric>; 3] = [Some(Uint), Some(Sint), Some(Float)];
    let (first, bits, channels) = match format {
        1..=6 => (1, 8, 1),
        7..=13 => (7, 16, 1),
        14..=19 => (14, 8, 2),
        20..=22 => (20, 32, 1),
        23..=29 => (23, 16, 2),
        56..=61 => (56, 8, 4),
        62..=64 => (62, 32, 2),
        65..=71 => (65, 16, 4),
        72..=74 => (72, 32, 3),
        75..=77 => (75, 32, 4),
        _ => return None,
    };
    let k = usize::from(format - first);
    let numeric = if bits == 32 { WIDE[k] } else { NARROW[k] };
    Some((bits, channels, numeric?))
}

fn unsupported(offset: usize, what: &'static str) -> ShaderError {
    ShaderError::Unsupported { offset, what }
}

/// Rewrites the inlined vertex fetch of a linked vertex program (see the
/// module): the channels each fetch loads become entry values of
/// [`attribute_vgpr`]s, and the loads go, with the code that only served
/// them (the table's address, the descriptor loads, the index). Returns how
/// many loads it replaced; nothing changes for any other program.
///
/// Fails when the program reads the vertex buffer table in a way that is no
/// such fetch, or fetches what the input layout does not describe: what is
/// left would otherwise read memory the host has no view of.
pub fn fetch(p: &mut Program, native: &NativeProgram) -> Result<usize, ShaderError> {
    let StageIo::Linked(io) = &native.io else {
        return Ok(0);
    };
    if native.stage != HwStage::Vs {
        return Ok(0);
    }
    let entry = |reg: Reg| p.inputs.iter().find(|i| i.0 == reg).map(|i| i.1);
    let (Some(low), Some(high)) = (entry(Reg::S(0)), entry(Reg::S(1))) else {
        // The table's address is never read: a program without vertex input.
        return Ok(0);
    };
    let scalar = |inst: &crate::ir::Inst, op: &Operand| match *op {
        Operand::Scalar { code, .. } if code != NULL => inst.read(Reg::S(code)),
        _ => None,
    };

    // The table's address: `s0` and its copies, `s1` masked to 8 bits and
    // its copies. `forming`: what only makes the address or the index.
    let mut lows = vec![low];
    let mut raw_highs = vec![high];
    let mut highs: Vec<Value> = Vec::new();
    let mut forming: Vec<(usize, usize)> = Vec::new();
    loop {
        let mut changed = false;
        for (b, block) in p.blocks.iter().enumerate() {
            for (i, inst) in block.insts.iter().enumerate() {
                let Fields::Sop {
                    dst: Some(Operand::Scalar { code: d, .. }),
                    src,
                    ..
                } = &inst.fields
                else {
                    continue;
                };
                let mut found = false;
                match (inst.name(), &src[..]) {
                    ("s_mov_b32" | "s_mov_b64", [Operand::Scalar { code: s, .. }]) if *s != NULL => {
                        for &(reg, out) in &inst.writes {
                            let Reg::S(c) = reg else { continue };
                            let Some(from) = c.checked_sub(*d).and_then(|k| inst.read(Reg::S(s + k))) else {
                                continue;
                            };
                            for set in [&mut lows, &mut raw_highs, &mut highs] {
                                if set.contains(&from) && !set.contains(&out) {
                                    set.push(out);
                                    found = true;
                                }
                            }
                        }
                    }
                    ("s_bfe_u32", [x, Operand::Literal(0x0008_0000)])
                    | ("s_and_b32", [x, Operand::Literal(0xff)] | [Operand::Literal(0xff), x]) => {
                        if let (Some(from), Some(out)) = (scalar(inst, x), inst.write(Reg::S(*d)))
                            && raw_highs.contains(&from)
                            && !highs.contains(&out)
                        {
                            highs.push(out);
                            found = true;
                        }
                    }
                    _ => {}
                }
                if found {
                    if !forming.contains(&(b, i)) {
                        forming.push((b, i));
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // The descriptor loads: each dword they write, as (load, slot, dword).
    let mut descriptors: HashMap<Value, ((usize, usize), u32, u8)> = HashMap::new();
    let mut loads: Vec<(usize, usize)> = Vec::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            let Fields::Smem {
                data,
                base: Operand::Scalar { code: base, .. },
                offset,
                soffset,
                ..
            } = inst.fields
            else {
                continue;
            };
            let through = inst.read(Reg::S(base)).is_some_and(|v| lows.contains(&v))
                && inst.read(Reg::S(base + 1)).is_some_and(|v| highs.contains(&v));
            if !through {
                continue;
            }
            let at = inst.machine.offset;
            let (Some(Operand::Scalar { code: first, dwords: 4 }), "s_load_dwordx4") = (data, inst.name()) else {
                return Err(unsupported(at, "a load from the vertex buffer table that is no descriptor's"));
            };
            let plain = matches!(soffset, Operand::Scalar { code: NULL, .. } | Operand::Const(128));
            if !plain || offset < 0 || offset % 16 != 0 {
                return Err(unsupported(at, "a vertex buffer slot the code computes"));
            }
            for k in 0..4 {
                let v = inst.write(Reg::S(first + k)).ok_or(unsupported(at, "a vertex buffer descriptor"))?;
                descriptors.insert(v, ((b, i), offset as u32 / 16, k));
            }
            loads.push((b, i));
        }
    }
    if loads.is_empty() {
        return Ok(0);
    }
    if p.vgprs > u16::from(ATTRIBUTE_VGPRS) {
        return Err(unsupported(0, "a linked vertex program with more than 128 VGPRs"));
    }

    // The vertex index: `v0 + s2`, both as on entry.
    let index_parts = (entry(Reg::V(0)), entry(Reg::S(2)));
    let mut indices: HashMap<Value, (usize, usize)> = HashMap::new();
    if let (Some(vertex), Some(base)) = index_parts {
        for (b, block) in p.blocks.iter().enumerate() {
            for (i, inst) in block.insts.iter().enumerate() {
                let Fields::Valu(v) = &inst.fields else {
                    continue;
                };
                let (Some(Operand::Vector { reg, .. }), [x, y]) = (v.dst, &v.src[..]) else {
                    continue;
                };
                if inst.name() != "v_add_nc_u32" || v.sdwa.is_some() || v.dpp.is_some() {
                    continue;
                }
                let read = |op: &Operand| match *op {
                    Operand::Vector { reg, .. } => inst.read(Reg::V(reg)),
                    ref s => scalar(inst, s),
                };
                let sum = [read(x), read(y)];
                if (sum == [Some(vertex), Some(base)] || sum == [Some(base), Some(vertex)])
                    && let Some(out) = inst.write(Reg::V(reg))
                {
                    indices.insert(out, (b, i));
                }
            }
        }
    }

    let mut next = p
        .inputs
        .iter()
        .map(|i| i.1)
        .chain(p.blocks.iter().flat_map(|b| {
            b.phis
                .iter()
                .map(|phi| phi.value)
                .chain(b.insts.iter().flat_map(|i| i.writes.iter().map(|w| w.1)))
        }))
        .map(|v| v.0 + 1)
        .max()
        .unwrap_or(0);
    let mut inputs = p.inputs.clone();
    let mut replaced: HashMap<Value, Value> = HashMap::new();
    let mut fetches: Vec<(usize, usize)> = Vec::new();
    for (b, block) in p.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            let Fields::Buffer {
                data,
                addr,
                rsrc: Operand::Scalar { code: rsrc, .. },
                soffset,
                offset,
                offen,
                idxen,
                format,
                lds,
                tfe,
                ..
            } = inst.fields
            else {
                continue;
            };
            let words: Vec<_> = (0..4)
                .map(|k| inst.read(Reg::S(rsrc + k)).and_then(|v| descriptors.get(&v)))
                .collect();
            if words.iter().all(Option::is_none) {
                continue;
            }
            let at = inst.machine.offset;
            let fail = |what| Err(unsupported(at, what));
            let slot = match words[..] {
                [Some(&(load, slot, 0)), Some(&(l1, _, 1)), Some(&(l2, _, 2)), Some(&(l3, _, 3))]
                    if [l1, l2, l3] == [load; 3] =>
                {
                    slot
                }
                _ => return fail("a vertex buffer descriptor the code puts together"),
            };
            if b != 0 {
                return fail("a vertex fetch outside the entry block");
            }
            let (Some(Operand::Vector { reg, .. }), Operand::Vector { reg: index, .. }) = (data, addr) else {
                return fail("a vertex fetch of this kind");
            };
            let plain = matches!(soffset, Operand::Scalar { code: NULL, .. } | Operand::Const(128));
            let by_vertex = idxen
                && !offen
                && plain
                && inst.read(Reg::V(index)).is_some_and(|v| indices.contains_key(&v));
            if !by_vertex || lds || tfe {
                return fail("a vertex fetch that is not indexed by the vertex");
            }
            let count: u8 = match (inst.name(), format) {
                ("buffer_load_dword", None) => 1,
                ("buffer_load_dwordx2", None) => 2,
                ("buffer_load_dwordx3", None) => 3,
                ("buffer_load_dwordx4", None) => 4,
                ("tbuffer_load_format_x", Some(_)) => 1,
                ("tbuffer_load_format_xy", Some(_)) => 2,
                ("tbuffer_load_format_xyz", Some(_)) => 3,
                ("tbuffer_load_format_xyzw", Some(_)) => 4,
                _ => return fail("a vertex fetch of this kind"),
            };
            // What each loaded channel is: whole dwords as they are, or the
            // format's channels converted its way.
            let (bits, numeric) = match format {
                None => (32, None),
                Some(f) => match buffer_format(f) {
                    // (past the format's channels the hardware loads 0 and 1)
                    Some((bits, channels, numeric)) if count <= channels => (bits, Some(numeric)),
                    Some(_) => return fail("a vertex fetch of more channels than its format has"),
                    None => return fail("a vertex fetch in a format of scaled or unequal channels"),
                },
            };
            for k in 0..count {
                let Some(out) = inst.write(Reg::V(reg + k)) else {
                    return fail("a vertex fetch of this kind");
                };
                // The element channel at this channel's bytes. A load need
                // not be an element's own: the code fetches one channel of
                // an element, or two adjacent elements at once.
                let byte = u32::from(offset) + u32::from(k) * u32::from(bits / 8);
                let covering = io.inputs.iter().enumerate().find_map(|(row, e)| {
                    let (width, channels, converts) = vertex_format(e.format)?;
                    let at = byte.checked_sub(e.offset).filter(|_| e.slot == slot)?;
                    let step = u32::from(width / 8);
                    (at < step * u32::from(channels)).then_some((row, e, at, step, width, converts))
                });
                let Some((row, element, at, step, width, converts)) = covering else {
                    return fail("a vertex fetch no input element describes");
                };
                if width != bits || at % step != 0 || numeric.is_some_and(|n| n != converts) {
                    return fail("a vertex fetch in another format than its input element's");
                }
                if element.per_instance {
                    return fail("a per-instance element fetched by the vertex");
                }
                let home = Reg::V(attribute_vgpr(row as u8, (at / step) as u8));
                let value = match inputs.iter().find(|e| e.0 == home) {
                    Some(e) => e.1,
                    None => {
                        let value = Value(next);
                        next += 1;
                        inputs.push((home, value));
                        value
                    }
                };
                replaced.insert(out, value);
            }
            fetches.push((b, i));
        }
    }
    // A descriptor is only what a fetch reads through.
    for (b, block) in p.blocks.iter().enumerate() {
        let phi = block.phis.iter().flat_map(|phi| &phi.args).any(|a| descriptors.contains_key(a));
        let read = block.insts.iter().enumerate().find(|(i, inst)| {
            !fetches.contains(&(b, *i)) && inst.reads.iter().any(|r| descriptors.contains_key(&r.1))
        });
        if phi || read.is_some() {
            let at = read.map_or(block.offset, |(_, inst)| inst.machine.offset);
            return Err(unsupported(at, "a vertex buffer descriptor read as data"));
        }
    }

    p.inputs = inputs;
    for block in &mut p.blocks {
        let renamed = |v: &mut Value| {
            if let Some(&to) = replaced.get(v) {
                *v = to;
            }
        };
        for phi in &mut block.phis {
            phi.args.iter_mut().for_each(renamed);
        }
        for inst in &mut block.insts {
            inst.reads.iter_mut().for_each(|r| renamed(&mut r.1));
        }
    }
    // The fetches and their descriptor loads go, then what only formed the
    // table's address and the index, once nothing reads it.
    let mut drop: Vec<(usize, usize)> = fetches.iter().chain(&loads).copied().collect();
    forming.extend(indices.values().copied());
    let read_by_others = |p: &Program, drop: &[(usize, usize)]| {
        let mut used = HashSet::new();
        for (b, block) in p.blocks.iter().enumerate() {
            used.extend(block.phis.iter().flat_map(|phi| phi.args.iter().copied()));
            for (i, inst) in block.insts.iter().enumerate() {
                if !drop.contains(&(b, i)) {
                    used.extend(inst.reads.iter().map(|r| r.1));
                }
            }
        }
        used
    };
    loop {
        let used = read_by_others(p, &drop);
        let before = drop.len();
        for &site in &forming {
            let inst = &p.blocks[site.0].insts[site.1];
            if !drop.contains(&site) && inst.writes.iter().all(|w| !used.contains(&w.1)) {
                drop.push(site);
            }
        }
        if drop.len() == before {
            break;
        }
    }
    for (b, block) in p.blocks.iter_mut().enumerate() {
        let mut i = 0;
        block.insts.retain(|_| {
            i += 1;
            !drop.contains(&(b, i - 1))
        });
    }
    // The table's address and the base vertex are no inputs once nothing
    // reads them, nor are the VGPRs only the fetch read: the vertex index,
    // and what the loads' registers held before (an input nobody reads
    // would still be declared: `SV_VertexID`, `SV_InstanceID`).
    let used = read_by_others(p, &[]);
    p.inputs
        .retain(|&(reg, v)| !matches!(reg, Reg::S(0..=2) | Reg::V(_)) || used.contains(&v));
    Ok(fetches.len())
}

/// Every export of `p`: its instruction's offset, target, and the channels
/// it enables.
fn exports(p: &Program) -> impl Iterator<Item = (usize, u8, u8, bool)> + '_ {
    p.blocks.iter().flat_map(|b| &b.insts).filter_map(|inst| match &inst.fields {
        Fields::Exp { target, src, compr, .. } => {
            let mask = (0..4).fold(0, |m, c| m | u8::from(src[c].is_some()) << c);
            Some((inst.machine.offset, *target, mask, *compr))
        }
        _ => None,
    })
}

/// A linked vertex program's parameters, as stage data would list them:
/// one entry per `exp param<n>`, in its register ([`param_row`]), with the
/// channels some export of it enables.
pub(crate) fn vs_outputs(p: &Program) -> Vec<(VsOutput, u8)> {
    let mut out: Vec<(VsOutput, u8)> = Vec::new();
    for (_, target, mask, _) in exports(p) {
        let Some(param) = target.checked_sub(32).filter(|n| *n < 32) else {
            continue;
        };
        match out.iter_mut().find(|o| o.0.param == param) {
            Some(o) => o.1 |= mask,
            None => out.push((
                VsOutput {
                    usage: Usage::Generic,
                    row: param_row(param),
                    param,
                    fp16: false,
                    pack2: false,
                },
                mask,
            )),
        }
    }
    out.sort_by_key(|o| o.0.param);
    out
}

/// The attributes `p` interpolates: (offset of the instruction, attribute,
/// channel, whether it reads one vertex's value).
fn attribute_reads(p: &Program) -> impl Iterator<Item = (usize, u8, u8, bool)> + '_ {
    p.blocks.iter().flat_map(|b| &b.insts).filter_map(|inst| {
        let attr = match &inst.fields {
            Fields::Vintrp { attr, .. } => *attr,
            Fields::Valu(v) if inst.name().starts_with("v_interp_") => *v.src.first()?,
            _ => return None,
        };
        let Operand::Attr { index, chan, .. } = attr else {
            return None;
        };
        Some((inst.machine.offset, index, chan, inst.name() == "v_interp_mov_f32"))
    })
}

/// A linked pixel program's inputs, as stage data would list them: one
/// entry per attribute the code reads, in the register of the vertex
/// parameter its input control names, with the channels read.
///
/// Fails for an attribute whose control the hardware treats specially (a
/// default value, a point sprite coordinate, 16-bit values: nothing in the
/// code tells them from a plain one), and for a flat one the code
/// interpolates.
pub(crate) fn ps_inputs(p: &Program, io: &LinkedIo) -> Result<Vec<PsInput>, ShaderError> {
    let mut out: Vec<PsInput> = Vec::new();
    for (at, attr, chan, vertex) in attribute_reads(p) {
        let Some(&cntl) = io.input_cntl.get(usize::from(attr)) else {
            return Err(unsupported(at, "an interpolated attribute past the pixel input controls"));
        };
        if cntl & !(CNTL_OFFSET | CNTL_FLAT) != 0 {
            return Err(unsupported(
                at,
                "an attribute whose input control asks for a default, a sprite coordinate or 16-bit values",
            ));
        }
        let flat = cntl & CNTL_FLAT != 0;
        if flat && !vertex {
            // The hardware gives such an interpolation the provoking
            // vertex's value; here it would come out interpolated.
            return Err(unsupported(at, "an interpolation of a flat-shaded attribute"));
        }
        let row = param_row((cntl & CNTL_OFFSET) as u8);
        match out.iter_mut().find(|i| i.attr == attr) {
            Some(i) => i.mask |= 1 << chan,
            None => out.push(PsInput {
                usage: Usage::Generic,
                row,
                mask: 1 << chan,
                attr,
                flat,
                fp16: false,
                pack2: false,
                row2: row,
            }),
        }
    }
    out.sort_by_key(|i| i.attr);
    Ok(out)
}

/// The render targets a linked pixel program exports to, and whether it
/// exports depth.
///
/// Fails for exports whose meaning depends on registers the pipeline's
/// image is not known to hold (`DB_SHADER_CONTROL`: stencil and coverage),
/// and for colour exports in another form than FP16 pairs.
pub(crate) fn ps_outputs(p: &Program, io: &LinkedIo) -> Result<(Vec<u8>, bool), ShaderError> {
    let mut targets: Vec<u8> = Vec::new();
    let mut depth = false;
    for (at, target, mask, compr) in exports(p) {
        match target {
            0..=7 => match io.col_format >> (4 * target) & 0xF {
                // A target the registers switch off: the export is dropped.
                0 => {}
                COL_FP16 if compr => {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
                _ => return Err(unsupported(at, "a colour export of a linked program that is not FP16 pairs")),
            },
            8 if mask == 1 && !compr => depth = true,
            8 => return Err(unsupported(at, "a stencil or coverage export of a linked program")),
            9 => {}
            _ => return Err(unsupported(at, "an export target")),
        }
    }
    targets.sort_unstable();
    Ok((targets, depth))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir;
    use crate::native::InputElement;

    const S_ENDPGM: u32 = 0xBF81_0000;
    /// `s_mov_b64 s[10:11], s[0:1]`; `s_bfe_u32 s11, s11, 0x80000`;
    /// `s_load_dwordx4 s[8:11], s[10:11], 0`; `v_add_nc_u32 v0, s2, v0`.
    const TABLE: [u32; 7] = [
        0xBE8A_0400,
        0x938B_FF0B,
        0x0008_0000,
        0xF408_0205,
        0xFA00_0000,
        0x4A00_0002,
        0xBF8C_C07F,
    ];
    /// `buffer_load_dwordx3 v[1:3], v0, s[8:11], 0 idxen` (offset 0).
    const POSITION: [u32; 2] = [0xE03C_2000, 0x8002_0100];
    /// `tbuffer_load_format_xyzw v[4:7], v0, s[8:11], 0 idxen` as
    /// 8_8_8_8_UNORM at offset 12.
    const COLOR: [u32; 2] = [0xE9C3_200C, 0x8002_0400];
    /// `exp pos0, v1, v2, v3, v4 done`; `exp param0, v4, v5, v6, v7`.
    const EXPORTS: [u32; 4] = [0xF800_08CF, 0x0403_0201, 0xF800_020F, 0x0706_0504];

    fn element(name: &str, format: u32, offset: u32) -> InputElement {
        InputElement {
            name: name.into(),
            index: 0,
            format,
            slot: 0,
            offset,
            per_instance: false,
            step_rate: 0,
        }
    }

    fn vertex(words: &[u32], inputs: Vec<InputElement>) -> (NativeProgram, Program) {
        let native = NativeProgram {
            stage: HwStage::Vs,
            wave64: false,
            vgprs: 48,
            layout: vec![],
            used: vec![],
            io: StageIo::Linked(LinkedIo {
                inputs,
                ..LinkedIo::default()
            }),
            code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        };
        let p = ir::build(&native.code, false, 48).unwrap();
        (native, p)
    }

    fn code(parts: &[&[u32]]) -> Vec<u32> {
        let mut words: Vec<u32> = parts.concat();
        words.push(S_ENDPGM);
        words
    }

    #[test]
    fn fetches_become_entry_values_of_attribute_registers() {
        let words = code(&[&TABLE, &POSITION, &COLOR, &EXPORTS]);
        let layout = vec![element("POSITION", 6, 0), element("COLOR", 28, 12)];
        let (native, mut p) = vertex(&words, layout);
        assert_eq!(fetch(&mut p, &native), Ok(2));
        let names: Vec<&str> = p.blocks.iter().flat_map(|b| &b.insts).map(|i| i.name()).collect();
        assert_eq!(names, ["exp", "exp", "s_endpgm"]);
        // The exports read the attributes' registers as on entry: position
        // x, y, z and the colour's four channels.
        let input = |v: Value| p.inputs.iter().find(|i| i.1 == v).map(|i| i.0);
        let read: Vec<_> = p.blocks[0].insts[1]
            .reads
            .iter()
            .filter_map(|r| input(r.1))
            .filter(|r| matches!(r, Reg::V(_)))
            .collect();
        assert_eq!(read, (0..4).map(|c| Reg::V(attribute_vgpr(1, c))).collect::<Vec<_>>());
        let pos = &p.blocks[0].insts[0];
        assert_eq!(pos.read(Reg::V(1)).and_then(input), Some(Reg::V(attribute_vgpr(0, 0))));
        assert_eq!(pos.read(Reg::V(3)).and_then(input), Some(Reg::V(attribute_vgpr(0, 2))));
        // The table's address and the base vertex are read no more.
        assert!(!p.inputs.iter().any(|i| matches!(i.0, Reg::S(0..=2))));
    }

    #[test]
    fn fetches_the_layout_does_not_describe_are_refused() {
        let words = code(&[&TABLE, &POSITION, &COLOR, &EXPORTS]);
        let refused = |layout: Vec<InputElement>| {
            let (native, mut p) = vertex(&words, layout);
            match fetch(&mut p, &native) {
                Err(ShaderError::Unsupported { what, .. }) => what,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(refused(vec![element("POSITION", 6, 0)]), "a vertex fetch no input element describes");
        // Two channels where three dwords are loaded; 16-bit channels
        // loaded as dwords; a typed load of channels that convert otherwise;
        // a format the table does not have.
        let color = || element("COLOR", 28, 12);
        assert_eq!(
            refused(vec![element("POSITION", 16, 0), color()]),
            "a vertex fetch no input element describes"
        );
        assert_eq!(
            refused(vec![element("POSITION", 10, 0), color()]),
            "a vertex fetch in another format than its input element's"
        );
        assert_eq!(
            refused(vec![element("POSITION", 6, 0), element("COLOR", 30, 12)]),
            "a vertex fetch in another format than its input element's"
        );
        assert_eq!(
            refused(vec![element("POSITION", 6, 0), element("COLOR", 24, 12)]),
            "a vertex fetch no input element describes"
        );
        let mut instance = color();
        instance.per_instance = true;
        assert_eq!(
            refused(vec![element("POSITION", 6, 0), instance]),
            "a per-instance element fetched by the vertex"
        );
    }

    #[test]
    fn a_fetch_may_take_one_channel_or_two_elements() {
        // `tbuffer_load_format_x v1, v0, s[8:11], 0 idxen offset:3` as
        // 8_UNORM: the colour's last byte. `buffer_load_dwordx4 v[4:7], v0,
        // s[8:11], 0 idxen offset:4`: two elements of two floats.
        let alpha = [0xE808_2003, 0x8002_0100];
        let both = [0xE038_2004, 0x8002_0400];
        let words = code(&[&TABLE, &alpha, &both, &EXPORTS]);
        let layout = vec![element("COLOR", 28, 0), element("TEXCOORD", 16, 4), element("POSITION", 16, 12)];
        let (native, mut p) = vertex(&words, layout);
        assert_eq!(fetch(&mut p, &native), Ok(2));
        let input = |v: Value| p.inputs.iter().find(|i| i.1 == v).map(|i| i.0);
        let pos = &p.blocks[0].insts[0];
        assert_eq!(pos.read(Reg::V(1)).and_then(input), Some(Reg::V(attribute_vgpr(0, 3))));
        let read: Vec<_> = p.blocks[0].insts[1]
            .reads
            .iter()
            .filter_map(|r| input(r.1))
            .filter(|r| matches!(r, Reg::V(_)))
            .collect();
        let expected = [(1, 0), (1, 1), (2, 0), (2, 1)].map(|(row, c)| Reg::V(attribute_vgpr(row, c)));
        assert_eq!(read, expected);
        // A byte in the middle of a float is no channel.
        let (native, mut p) = vertex(&words, vec![element("COLOR", 16, 0), element("TEXCOORD", 16, 4)]);
        assert_eq!(
            fetch(&mut p, &native),
            Err(unsupported(0x1c, "a vertex fetch in another format than its input element's"))
        );
    }

    #[test]
    fn a_fetch_by_another_index_or_a_descriptor_read_as_data_is_refused() {
        // The index is v0 alone: the add is left out.
        let words = code(&[&TABLE[..5], &POSITION, &EXPORTS]);
        let (native, mut p) = vertex(&words, vec![element("POSITION", 6, 0)]);
        assert_eq!(
            fetch(&mut p, &native),
            Err(unsupported(0x14, "a vertex fetch that is not indexed by the vertex"))
        );
        // `s_mov_b32 s20, s8`: a descriptor dword copied.
        let words = code(&[&TABLE, &POSITION, &[0xBE94_0308], &EXPORTS]);
        let (native, mut p) = vertex(&words, vec![element("POSITION", 6, 0)]);
        assert_eq!(
            fetch(&mut p, &native),
            Err(unsupported(0x24, "a vertex buffer descriptor read as data"))
        );
    }

    #[test]
    fn other_programs_are_left_alone() {
        let words = code(&[&TABLE, &POSITION, &EXPORTS]);
        let (mut native, mut p) = vertex(&words, vec![element("POSITION", 6, 0)]);
        native.io = StageIo::Vs {
            inputs: vec![],
            outputs: vec![],
        };
        let before = p.clone();
        assert_eq!(fetch(&mut p, &native), Ok(0));
        assert_eq!(p, before);
    }

    #[test]
    fn parameters_and_attributes_meet_by_number() {
        let words = code(&[&TABLE, &POSITION, &COLOR, &EXPORTS]);
        let (_, p) = vertex(&words, vec![]);
        let outputs = vs_outputs(&p);
        assert_eq!(outputs.len(), 1);
        assert_eq!((outputs[0].0.param, outputs[0].0.row, outputs[0].1), (0, 1, 0xF));

        // v_interp_p1_f32 v2, v0, attr1.x; v_interp_p2_f32 v2, v1, attr1.x;
        // v_interp_mov_f32 v3, p0, attr0.y; exp mrt0 v2, v2, v3, v3 compr
        // done vm.
        let pixel = [0xC808_0400, 0xC809_0401, 0xC80E_0102, 0xF800_1C0F, 0x8080_0302, S_ENDPGM];
        let code: Vec<u8> = pixel.iter().flat_map(|w| w.to_le_bytes()).collect();
        let p = ir::build(&code, true, 16).unwrap();
        let io = |input_cntl: &[u32], col_format| LinkedIo {
            input_cntl: input_cntl.to_vec(),
            col_format,
            ..LinkedIo::default()
        };
        // Attribute 1 reads parameter 2; attribute 0 is flat and read so.
        let inputs = ps_inputs(&p, &io(&[0x400, 2], 4)).unwrap();
        let seen: Vec<_> = inputs.iter().map(|i| (i.attr, i.row, i.mask, i.flat)).collect();
        assert_eq!(seen, [(0, 1, 2, true), (1, 3, 1, false)]);
        assert_eq!(ps_outputs(&p, &io(&[], 4)), Ok((vec![0], false)));
        // A target the registers switch off is no output.
        assert_eq!(ps_outputs(&p, &io(&[], 0)), Ok((vec![], false)));

        let what = |r: Result<Vec<PsInput>, ShaderError>| match r {
            Err(ShaderError::Unsupported { what, .. }) => what,
            other => panic!("{other:?}"),
        };
        assert_eq!(what(ps_inputs(&p, &io(&[0x400], 4))), "an interpolated attribute past the pixel input controls");
        assert_eq!(what(ps_inputs(&p, &io(&[0x400, 0x401], 4))), "an interpolation of a flat-shaded attribute");
        assert!(what(ps_inputs(&p, &io(&[0x400, 0x8_0001], 4))).contains("16-bit"));
        // Exports specialised to another format are not FP16 pairs.
        assert!(ps_outputs(&p, &io(&[], 9)).is_err());
    }
}
