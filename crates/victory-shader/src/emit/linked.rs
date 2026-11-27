//! The signatures of a linked program (`crate::linked`), which has no
//! container to take them from: the vertex inputs are the pipeline's input
//! elements the code fetches, the vertex outputs and pixel inputs the
//! parameters the stages meet by, the pixel outputs the render targets the
//! code exports to.

use super::container::SigElement;
use crate::gfx10::Fields;
use crate::linked::{self, Numeric};
use crate::native::{HwStage, LinkedIo};
use crate::{ShaderError, ir};

/// `SPI_PS_INPUT_ENA`: the position's four channels, the front face.
const ENA_POSITION: u32 = 0xF00;
const ENA_FRONT_FACE: u32 = 1 << 12;

fn element(name: &str, index: u32, kind: u8, comp: u8, row: Option<u32>, cols: u8) -> SigElement {
    SigElement {
        name: name.into(),
        index,
        kind,
        comp,
        interp: 0,
        row,
        col: 0,
        cols,
        usage: 0,
        half: false,
    }
}

/// The position: register 0 in both stages.
fn position() -> SigElement {
    element("SV_Position", 0, 3, 9, Some(0), 4)
}

/// Parameter `param` with channels up to the highest of `mask`.
fn parameter(row: u8, mask: u8) -> SigElement {
    let cols = (8 - mask.leading_zeros()) as u8;
    element("TEXCOORD", u32::from(row) - 1, 0, 9, Some(row.into()), cols)
}

/// The input and output signatures of the linked program `p` (after the
/// fetch rewrite) of `stage`.
pub(super) fn signatures(
    stage: HwStage,
    io: &LinkedIo,
    p: &ir::Program,
) -> Result<(Vec<SigElement>, Vec<SigElement>), ShaderError> {
    if stage == HwStage::Ps {
        let mut inputs = Vec::new();
        if io.input_ena & ENA_POSITION != 0 {
            inputs.push(position());
        }
        // Attributes that read one parameter share its element.
        let mut rows: Vec<(u8, u8)> = Vec::new();
        for i in linked::ps_inputs(p, io)? {
            match rows.iter_mut().find(|r| r.0 == i.row) {
                Some(r) => r.1 |= i.mask,
                None => rows.push((i.row, i.mask)),
            }
        }
        rows.sort_unstable();
        inputs.extend(rows.into_iter().map(|(row, mask)| parameter(row, mask)));
        if io.input_ena & ENA_FRONT_FACE != 0 {
            inputs.push(element("SV_IsFrontFace", 0, 13, 5, None, 1));
        }
        let (targets, depth) = linked::ps_outputs(p, io)?;
        let mut outputs: Vec<SigElement> = targets
            .into_iter()
            .map(|n| element("SV_Target", n.into(), 16, 9, Some(n.into()), 4))
            .collect();
        if depth {
            outputs.push(element("SV_Depth", 0, 17, 9, None, 1));
        }
        return Ok((inputs, outputs));
    }
    // The input elements the code fetches: the channels of each that the
    // rewrite made entry values.
    let mut inputs = Vec::new();
    for (row, e) in io.inputs.iter().enumerate() {
        let row = row as u8;
        let fetched = |c: &u8| p.inputs.iter().any(|i| i.0 == ir::Reg::V(linked::attribute_vgpr(row, *c)));
        let Some(last) = (0..4).filter(fetched).max() else {
            continue;
        };
        // Integers as they are; everything else reaches the program as
        // floats, converted by the input assembler as the console's typed
        // load converts.
        let comp = match linked::vertex_format(e.format) {
            Some((_, _, Numeric::Uint)) => 5,
            Some((_, _, Numeric::Sint)) => 4,
            Some(_) => 9,
            // (the rewrite fetches from no such element)
            None => {
                return Err(ShaderError::Unsupported {
                    offset: 0,
                    what: "a vertex fetch no input element describes",
                });
            }
        };
        inputs.push(element(&e.name, e.index, 0, comp, Some(row.into()), last + 1));
    }
    // What `pos1` and up carry (clip and cull distances, the point size,
    // the target index) is in registers the pipeline's image is not known
    // to hold.
    let beyond = p.blocks.iter().flat_map(|b| &b.insts).find(
        |i| matches!(i.fields, Fields::Exp { target: 13..=15, .. }),
    );
    if let Some(inst) = beyond {
        return Err(ShaderError::Unsupported {
            offset: inst.machine.offset,
            what: "a clip, cull or misc vector export of a linked program",
        });
    }
    let mut outputs = vec![position()];
    let parameters = linked::vs_outputs(p);
    outputs.extend(parameters.iter().filter(|o| o.1 != 0).map(|o| parameter(o.0.row, o.1)));
    Ok((inputs, outputs))
}

#[cfg(test)]
mod tests {
    use super::super::{HostShader, signature, translate};
    use crate::ShaderError;
    use crate::dxbc::{Container, FourCC};
    use crate::native::fixture::{HASH, linked_code, linked_meta};
    use crate::native::{
        GraphicsRegs, HwStage, InputElement, InputLayout, LinkedIo, RootType, dumped, linked_container,
    };

    fn validate(shader: &HostShader) {
        #[cfg(windows)]
        if let Ok(mut v) = super::super::validator::Validator::load(
            super::super::validator::Validator::default_path().as_deref(),
        ) {
            v.validate(&mut shader.dxil.clone()).unwrap();
        }
        let _ = shader;
    }

    /// The rows of one of a translation's signatures: name, semantic index,
    /// register, columns.
    fn rows(shader: &HostShader, part: FourCC) -> Vec<(String, u32, Option<u32>, u8)> {
        let c = Container::parse(&shader.dxil).unwrap();
        let rows = signature(c.part(part).unwrap().data).unwrap();
        rows.into_iter().map(|e| (e.name, e.index, e.row, e.cols)).collect()
    }

    fn row(name: &str, index: u32, register: u32, cols: u8) -> (String, u32, Option<u32>, u8) {
        (name.into(), index, Some(register), cols)
    }

    /// A vertex program that fetches a position (three floats) and a colour
    /// (four normalised bytes) and passes on two of the colour's channels,
    /// and a pixel program that interpolates them and exports them as FP16
    /// pairs: each stage's signatures follow from its own code, and meet.
    #[test]
    fn a_linked_pair_translates_with_signatures_that_meet() {
        let vertex = [
            0xBE8A_0400, // s_mov_b64 s[10:11], s[0:1]
            0x938B_FF0B,
            0x0008_0000, // s_bfe_u32 s11, s11, 0x80000
            0xF408_0205,
            0xFA00_0000, // s_load_dwordx4 s[8:11], s[10:11], 0
            0x4A00_0002, // v_add_nc_u32 v0, s2, v0
            0xE03C_2000,
            0x8002_0100, // buffer_load_dwordx3 v[1:3], v0, s[8:11], 0 idxen
            0xE9C3_200C,
            0x8002_0400, // tbuffer_load_format_xyzw v[4:7], ... offset:12 8_8_8_8_UNORM
            0xF800_08CF,
            0x0303_0201, // exp pos0, v1, v2, v3, v3 done
            0xF800_0203,
            0x0000_0504, // exp param0, v4, v5, off, off
        ];
        let element = |name: &str, format, offset| InputElement {
            name: name.into(),
            index: 0,
            format,
            slot: 0,
            offset,
            per_instance: false,
            step_rate: 0,
        };
        let code = linked_code(HASH, &vertex);
        let meta = linked_meta(&code, &[], HASH);
        let io = LinkedIo {
            inputs: vec![element("POSITION", 6, 0), element("COLOR", 28, 12)],
            rsrc2: 0x14,
            ..LinkedIo::default()
        };
        let vs = translate(&linked_container(HwStage::Vs, &code, &meta, 5, false, &io).unwrap()).unwrap();
        validate(&vs);
        assert_eq!(vs.stage, HwStage::Vs);
        // (of the colour, the two channels the program reads)
        assert_eq!(rows(&vs, FourCC::ISG1), [row("POSITION", 0, 0, 3), row("COLOR", 0, 1, 2)]);
        assert_eq!(rows(&vs, FourCC::OSG1), [row("SV_Position", 0, 0, 4), row("TEXCOORD", 0, 1, 2)]);
        // The fetch is the input assembler's: no buffer is read, no user data.
        assert!(vs.user_data.is_empty() && vs.root_srvs.is_empty());

        let pixel = [
            0xBEFC_030A, // s_mov_b32 m0, s10
            0xC808_0000, // v_interp_p1_f32 v2, v0, attr0.x
            0xC809_0001, // v_interp_p2_f32 v2, v1, attr0.x
            0xC80C_0100, // v_interp_p1_f32 v3, v0, attr0.y
            0xC80D_0101, // v_interp_p2_f32 v3, v1, attr0.y
            0xF800_1C0F,
            0x8080_0302, // exp mrt0, v2, v2, v3, v3 done compr vm
        ];
        let code = linked_code(HASH, &pixel);
        let meta = linked_meta(&code, &[], HASH);
        let io = |input_cntl: &[u32], col_format| LinkedIo {
            inputs: Vec::new(),
            input_ena: 2,
            col_format,
            input_cntl: input_cntl.to_vec(),
            rsrc2: 0x14,
        };
        let pixel =
            |io: &LinkedIo| translate(&linked_container(HwStage::Ps, &code, &meta, 3, true, io).unwrap());
        let ps = pixel(&io(&[0], 4)).unwrap();
        validate(&ps);
        assert_eq!(ps.stage, HwStage::Ps);
        assert_eq!(rows(&ps, FourCC::ISG1), [row("TEXCOORD", 0, 1, 2)]);
        assert_eq!(rows(&ps, FourCC::OSG1), [row("SV_Target", 0, 0, 4)]);
        // Attribute 0 may read another parameter: its input control says.
        let second = pixel(&io(&[1], 4)).unwrap();
        validate(&second);
        assert_eq!(rows(&second, FourCC::ISG1), [row("TEXCOORD", 1, 2, 2)]);
        // A target the registers switch off has no output.
        let off = pixel(&io(&[0], 0)).unwrap();
        validate(&off);
        assert!(rows(&off, FourCC::OSG1).is_empty());

        // What cannot be derived is refused by name.
        let what = |r: Result<HostShader, ShaderError>| match r {
            Err(ShaderError::Unsupported { what, .. }) => what,
            other => panic!("{other:?}"),
        };
        assert_eq!(what(pixel(&io(&[], 4))), "an interpolated attribute past the pixel input controls");
        assert_eq!(what(pixel(&io(&[0x400], 4))), "an interpolation of a flat-shaded attribute");
        assert_eq!(
            what(pixel(&io(&[0], 9))),
            "a colour export of a linked program that is not FP16 pairs"
        );
    }

    /// Description `d` of a run's dump as containers of ours: its vertex
    /// program, and its pixel program if it has one.
    fn containers(d: &str) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        let regs = GraphicsRegs::parse(&dumped::block(d, 1)?).ok()?;
        let layout = InputLayout::parse(&dumped::block(d, 3)?).ok()?;
        let (code, meta) = (dumped::block(d, 9)?, dumped::block(d, 11)?);
        let vs = linked_container(HwStage::Vs, &code, &meta, regs.vs_rsrc1, false, &regs.vertex_io(&layout));
        let ps = dumped::block(d, 5).zip(dumped::block(d, 7)).map(|(code, meta)| {
            let io = regs.pixel_io();
            linked_container(HwStage::Ps, &code, &meta, regs.ps_rsrc1, regs.ps_wave64(), &io)
                .unwrap_or_else(|e| panic!("{d}: {e}"))
        });
        Some((vs.unwrap_or_else(|e| panic!("{d}: {e}")), ps))
    }

    /// The first description a run dumped (`.agente/pso-dump`): both of its
    /// programs translate, and the validator takes them.
    #[test]
    fn the_first_dumped_description_translates_and_validates() {
        let Some((vs, Some(ps))) = containers("00001") else {
            return;
        };
        let vs = translate(&vs).unwrap();
        validate(&vs);
        // Its layout's three elements, all fetched; two parameters.
        assert_eq!(
            rows(&vs, FourCC::ISG1),
            [row("POSITION", 0, 0, 3), row("COLOR", 0, 1, 4), row("TEXCOORD", 0, 2, 2)]
        );
        assert_eq!(
            rows(&vs, FourCC::OSG1),
            [row("SV_Position", 0, 0, 4), row("TEXCOORD", 0, 1, 4), row("TEXCOORD", 1, 2, 2)]
        );
        let ps = translate(&ps).unwrap();
        validate(&ps);
        assert_eq!(rows(&ps, FourCC::ISG1), [row("TEXCOORD", 0, 1, 4), row("TEXCOORD", 1, 2, 2)]);
        assert_eq!(rows(&ps, FourCC::OSG1), [row("SV_Target", 0, 0, 4)]);
        // Both read the title's one descriptor table, in user-data dword 8.
        for shader in [&vs, &ps] {
            let user: Vec<_> = shader.user_data.iter().map(|u| (u.index, u.slot, u.root)).collect();
            assert_eq!(user, [(8, 0, RootType::Table)]);
        }
    }

    /// Every description dumped: each program translates into DXIL the
    /// validator takes, or is refused as something not supported (never as
    /// malformed), and what a pixel program reads its vertex program writes.
    #[test]
    fn dumped_descriptions_translate_or_are_refused_by_name() {
        let mut refused = Vec::new();
        let mut translated = 0;
        for d in dumped::descriptions() {
            let Some((vs, ps)) = containers(&d) else {
                continue;
            };
            let mut done = |stage: &str, container: &[u8]| match translate(container) {
                Ok(shader) => {
                    validate(&shader);
                    translated += 1;
                    Some(shader)
                }
                Err(ShaderError::Unsupported { what, .. }) => {
                    refused.push(format!("{d} {stage}: {what}"));
                    None
                }
                Err(e) => panic!("{d} {stage}: {e}"),
            };
            let vs = done("vertex", &vs);
            let ps = ps.and_then(|ps| done("pixel", &ps));
            if let (Some(vs), Some(ps)) = (vs, ps) {
                let written = rows(&vs, FourCC::OSG1);
                // (system values without a register are the rasterizer's)
                for (name, index, register, cols) in rows(&ps, FourCC::ISG1) {
                    if register.is_none() {
                        continue;
                    }
                    let from = written.iter().find(|w| w.0 == name && w.1 == index);
                    assert!(
                        from.is_some_and(|w| w.2 == register && w.3 >= cols),
                        "{d}: {name}{index} in {register:?}, {cols} wide, of {written:?}"
                    );
                }
            }
        }
        // Of the 29 programs of the 15 vertex plus pixel descriptions seen,
        // one is refused: a pixel program that reads the fixed-point
        // position (`SPI_PS_INPUT_ENA` bit 15), which the lowering has not.
        assert!(refused.len() <= 1, "{translated} translated, refused: {refused:#?}");
    }
}
