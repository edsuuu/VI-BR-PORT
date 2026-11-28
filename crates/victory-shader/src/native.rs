//! Native GFX10 programs that the console compiler stores in a `DXIL` part.
//!
//! An `XDAT` index after the program header lists blobs stored around the
//! bitcode; one or more of them hold native programs, zlib-packed (a vertex
//! shader can carry two: compiled for use with and without a geometry
//! shader). Layout as observed in shipped titles (index offsets are relative
//! to the part data):
//!
//! ```text
//! +0x18 "XDAT"  u32 entry count, u32 (0x24 seen), then per entry:
//!               u32 kind, u32 offset, u32 size
//! kind 0x0d     "XBCH" u32 version (3), u64 packed size, u64 unpacked size,
//!               zlib stream
//! unpacked      "FSSM" u32 version (0x101), u64 payload size, payload
//! payload +0x00 u64 graphics slot count (6)
//!         +0x08 u64 record offsets: LS HS ES GS VS PS (0x08..0x30),
//!               mesh 0x48, compute 0x60; unused ones are 0xDEADBEEF_DEADBEEF
//! record  +0x00 u64 code offset, +0x08 u64 code size (payload-relative),
//!         +0x14 u32 flags: bit 5 wave32 (else wave64)
//!         +0x28 u64 root layout table, +0x30 u64 used-resource table
//!               (payload-relative), +0x38 u32 and +0x3c u32 their counts
//!         +0x50 u32 bits 0-9 VGPRs used, bits 20-27 SGPRs used
//! layout  20 B: u32 root type, u32 table offset (dwords), u32 user SGPRs,
//!               u32 base register, u8 first user-data dword, u8 root slot,
//!               u16 0
//! used    20 B: u8 resource type, u8 root type, u16 0, u32 table offset
//!               (dwords), u32 lower bound, u32 stride, u8 descriptor
//!               dwords, u8 first user-data dword, u8 root slot, u8 0
//! ```
//!
//! Stage data follows the record; offsets from its end (E = record + 0x54):
//!
//! ```text
//! PS  E+0x24  32 inputs of 8 B: u8 usage, u8 signature row, u8 channel
//!             mask, u8 attribute (v_interp `attr`), u8 default, u8 second
//!             row (of a packed pair), u8 parameter slot, u8 flags (bit 0
//!             flat, 1 linear, 3 fp16, 4 fp16 packed); E+0x124 u32 count
//!     E+0x134 u32 SPI_PS_INPUT_ADDR, E+0x138 u32 DB_SHADER_CONTROL
//!     E+0x148 u32 export patch size (dwords), E+0x14c 8 patches, one per
//!             MRT: u32 code offset (dwords, -1 none), then the export code
//!             for each color format in 10 slots of 10 dwords (ZERO, 32_R,
//!             32_GR, 32_AR, FP16, UNORM16, SNORM16, UINT16, SINT16, 32_ABGR)
//! VS  E+0x24  32 inputs of 5 B: u8 usage, u8 row, u8 channel mask, u8 VGPR
//!             of channel 0 (channel k in VGPR + k), u8 highest channel;
//!             E+0xc4 u32 count
//!     E+0xc8  32 outputs of 6 B: u8 usage, u8 row, u8 parameter (`exp
//!             param<n>`), u8 row again, u8 slot, u8 flags (bit 0 fp16,
//!             bit 1 two fp16 values per dword); E+0x188 u32 count
//! CS  E+0x24  u32 COMPUTE_PGM_RSRC2, E+0x28 u32 x3 thread-group size
//! MS  E+0xc8  outputs as VS, E+0x188 count; the param byte's bit 7 marks a
//!             per-primitive output (`PSG1` row); the 3 bytes more are the
//!             second row of a packed pair, a slot, flags (bit 0 fp16, bit 1
//!             two fp16 elements packed)
//!     E+0x18c u32 SPI_VS_OUT_CONFIG, SPI_SHADER_POS_FORMAT,
//!             PA_CL_VS_OUT_CNTL, VGT_STRMOUT_CONFIG, SPI_SHADER_PGM_RSRC2_GS,
//!             VGT_GS_OUT_PRIM_TYPE, VGT_GS_INSTANCE_CNT, SPI_SHADER_IDX_FORMAT
//!     E+0x448 u16 thread-group size in lanes, u16 maximum vertices,
//!             u16 maximum primitives, u8 user SGPRs, u8 output topology
//!             (2 triangles); code follows at E+0x45c
//! record +0x4c u32 LDS bytes (`lds_byte_size`; mesh programs read it here)
//! ```
//!
//! Rows are registers of the container's signature parts (`ISG1`, `OSG1`).
//! Merged stages keep their stage data in another layout (not read).
//!
//! Other kinds seen: 0x07 program header + bitcode (or a stub when stripped),
//! 0x0a `xbop` (compiler options), 0x0e a table not decoded yet, 0x10 and
//! 0x12 16-byte blobs (0x10 is the shader hash).
//! Unknown versions are rejected so a format change shows up as an error.

use crate::ShaderError::{self, Malformed};
use crate::dxbc::{u32_at, u64_at};

const XDAT_OFFSET: usize = 0x18;
const MAX_XDAT_ENTRIES: usize = 64;
const KIND_NATIVE: u32 = 0x0d;
const XBCH_VERSION: u32 = 3;
const XBCH_HEADER: usize = 0x18;
const FSSM_VERSION: u32 = 0x101;
const FSSM_HEADER: usize = 0x10;
const GRAPHICS_SLOT_COUNT: u64 = 6;
/// Where each stage's record offset sits in the FSSM payload.
const SLOTS: [(HwStage, usize); 8] = [
    (HwStage::Ls, 0x08),
    (HwStage::Hs, 0x10),
    (HwStage::Es, 0x18),
    (HwStage::Gs, 0x20),
    (HwStage::Vs, 0x28),
    (HwStage::Ps, 0x30),
    (HwStage::Ms, 0x48),
    (HwStage::Cs, 0x60),
];
const ABSENT: u64 = 0xDEAD_BEEF_DEAD_BEEF;
const RECORD_SIZE: u64 = 0x54;
const RECORD_FLAGS: usize = 0x14;
const FLAG_WAVE32: u32 = 1 << 5;
const RECORD_REGS: usize = 0x50;
const RECORD_LAYOUT: usize = 0x28;
const RECORD_USED: usize = 0x30;
const RECORD_COUNTS: usize = 0x38;
const ENTRY_SIZE: u64 = 20;
const MAX_SEMANTICS: u32 = 32;
const PS_INPUTS: usize = 0x24;
const PS_INPUT_COUNT: usize = 0x124;
const PS_INPUT_ADDR: usize = 0x134;
const PS_DB_SHADER_CONTROL: usize = 0x138;
const PS_PATCH_SIZE: usize = 0x148;
const PS_PATCHES: usize = 0x14c;
const PATCH_SLOT_DWORDS: usize = 10;
/// The variant that exports every channel as a raw 32-bit value.
const PATCH_FULL: usize = 9;
const PATCH_BYTES: usize = 4 + 10 * PATCH_SLOT_DWORDS * 4;
const MRTS: usize = 8;
const VS_INPUTS: usize = 0x24;
const VS_INPUT_COUNT: usize = 0xc4;
const VS_OUTPUTS: usize = 0xc8;
const VS_OUTPUT_COUNT: usize = 0x188;
const CS_RSRC2: usize = 0x24;
const CS_THREADS: usize = 0x28;
const RECORD_LDS: usize = 0x4c;
const MS_CLIP_CNTL: usize = 0x194;
const MS_INFO: usize = 0x448;
const FSPX_VERSION: u32 = 1;
const FSPX_HEADER: usize = 0x10;
const FSPX_PACKET: u32 = 0;
const FSPX_CODE: u32 = 6;
const FSPX_METADATA: u32 = 7;
const PACKET_THREADS: usize = 0x0c;
const PACKET_FLAGS: usize = 0x12;
const PACKET_WAVE32: u16 = 1 << 4;
const PACKET_RSRC1: usize = 0x14;
const PACKET_RSRC2: usize = 0x18;
const FSPX_RECORDS: usize = 0x18;
const FSPX_RECORD: usize = 32;
/// The pipeline metadata entry of an `FSPX` part (a hash).
const FSPX_PIPELINE_METADATA: u32 = 1;
/// What a compute program's metadata starts with (`fspx_program`).
const FSPX_META_TAG: u32 = 0x403;
/// Bytes of a compute program's metadata its size field leaves out.
const FSPX_META_UNCOUNTED: usize = 0x14;
/// Where its record count sits.
const FSPX_META_COUNT: usize = 0x14;
/// Bytes of a compute pipeline's register packet.
const FSPX_PACKET_SIZE: usize = 0x1c;
/// The part of a container we make around a linked program.
const LINKED_MAGIC: &[u8; 4] = b"VLNK";
const LINKED_VERSION: u32 = 1;
const LINKED_TAG: u32 = 2;
const LINKED_RECORDS: usize = 0x10;
/// The low 15 bits of the dword that counts a linked program's records.
const LINKED_MARK: u32 = 0x440;
/// Dwords a linked code block has past what its metadata counts.
const LINKED_CODE_EXTRA: usize = 10;
/// Where a register image says where its last section is.
const REGS_SECTION: usize = 0x28;
/// What that offset counts from.
const REGS_SECTION_BASE: usize = 0x1c;
/// The last section's place in the images of vertex plus pixel pipelines.
const REGS_LAST: usize = 0x138;
/// Its dwords before the pixel input controls, and after them.
const REGS_LAST_HEAD: usize = 14;
const REGS_LAST_TAIL: usize = 3;
const REGS_VS_RSRC1: usize = 0x38;
const REGS_PS_RSRC1: usize = 0x4c;
const REGS_PS_INPUT_ENA: usize = 0x58;
const REGS_PS_IN_CONTROL: usize = 0x5c;
const REGS_COL_FORMAT: usize = 0x70;
const REGS_COLOR_CONTROL: usize = 0x7c;
const REGS_VS_OUT_CONFIG: usize = 0x84;
const REGS_DEPTH_CONTROL: usize = 0xb8;
const REGS_STENCIL_REF_MASK: usize = 0xc0;
const REGS_BLEND: usize = 0xd0;
const LAYOUT_TARGET_FORMATS: usize = 0x10;
const LAYOUT_SAMPLE_MASK: usize = 0x1c;
const LAYOUT_DEPTH_FORMAT: usize = 0x24;
const LAYOUT_ELEMENTS_SIZE: usize = 0x38;
const LAYOUT_ELEMENT_COUNT: usize = 0x4c;
const LAYOUT_ELEMENTS: usize = 0x50;
const MAX_INPUT_ELEMENTS: u32 = 32;
/// Unpacked blobs are a few KiB; refuse to allocate for absurd sizes.
const MAX_UNPACKED: u64 = 16 << 20;

/// Hardware shader stage a native program runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HwStage {
    Ls,
    Hs,
    Es,
    Gs,
    Vs,
    Ps,
    /// Mesh shader.
    Ms,
    Cs,
}

impl HwStage {
    /// The SGPR holding user-data dword `index`: merged stages (and mesh
    /// shaders, which run as one) have 8 system SGPRs first.
    pub fn user_sgpr(self, index: u8) -> u8 {
        match self {
            HwStage::Ls | HwStage::Hs | HwStage::Es | HwStage::Gs | HwStage::Ms => index + 8,
            HwStage::Vs | HwStage::Ps | HwStage::Cs => index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProgram {
    pub stage: HwStage,
    /// Runs as 64 lanes per wave (else 32); sizes lane masks.
    pub wave64: bool,
    /// VGPRs the program uses (v0 up to this), the bound of indexed access.
    pub vgprs: u16,
    /// Where the root signature's parameters arrive (user SGPRs).
    pub layout: Vec<RootParam>,
    /// The descriptors the program reads, by root parameter.
    pub used: Vec<UsedResource>,
    pub io: StageIo,
    /// GFX10 machine code, as stored.
    pub code: Vec<u8>,
}

impl NativeProgram {
    /// The code with each render target's export patched for its full
    /// 32-bit format, so exports carry the shader's values unconverted (as
    /// the console does at pipeline creation for that format).
    pub fn patched_code(&self) -> Vec<u8> {
        let mut code = self.code.clone();
        if let StageIo::Ps { patches, .. } = &self.io {
            for p in patches {
                let at = p.offset as usize * 4;
                for (i, word) in p.code.iter().enumerate() {
                    code[at + 4 * i..at + 4 * i + 4].copy_from_slice(&word.to_le_bytes());
                }
            }
        }
        code
    }
}

/// What a semantic is, as the stage data names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Usage {
    Generic,
    ClipDistance,
    Other(u8),
}

impl Usage {
    fn from_raw(raw: u8) -> Self {
        match raw {
            6 => Usage::Generic,
            7 => Usage::ClipDistance,
            n => Usage::Other(n),
        }
    }
}

/// A pixel shader input: what `v_interp` of attribute `attr` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsInput {
    pub usage: Usage,
    /// Register in the input signature.
    pub row: u8,
    pub mask: u8,
    pub attr: u8,
    pub flat: bool,
    pub fp16: bool,
    /// Two 16-bit values per dword ([`VsOutput::pack2`]); `row2` is the
    /// second element's register.
    pub pack2: bool,
    pub row2: u8,
}

/// A vertex attribute the fetch shader loads: channel k into VGPR `vgpr + k`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsInput {
    pub usage: Usage,
    pub row: u8,
    pub mask: u8,
    pub vgpr: u8,
}

/// What `exp param<param>` of a vertex shader writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsOutput {
    pub usage: Usage,
    /// Register in the output signature.
    pub row: u8,
    pub param: u8,
    /// 16-bit values, one per dword (low half) unless `pack2`: then two per
    /// dword, channel c of this row in dword c / 2 (odd channels in the
    /// high half), then a second 16-bit row from the next whole dword (the
    /// next 16-bit row of the signature no entry names).
    pub fp16: bool,
    pub pack2: bool,
}

/// The export code of one render target for its full 32-bit format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPatch {
    pub mrt: u8,
    /// Where it goes in the code, in dwords.
    pub offset: u32,
    pub code: Vec<u32>,
}

/// Inputs and outputs, for the stages whose stage data is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StageIo {
    #[default]
    None,
    Vs {
        inputs: Vec<VsInput>,
        outputs: Vec<VsOutput>,
    },
    Ps {
        inputs: Vec<PsInput>,
        /// `SPI_PS_INPUT_ADDR`: which system values arrive in VGPRs.
        input_addr: u32,
        /// `DB_SHADER_CONTROL`: depth export, kill.
        db_shader_control: u32,
        patches: Vec<ExportPatch>,
    },
    Cs(ComputeIo),
    Ms(MeshIo),
    /// A linked vertex or pixel program ([`linked_program`]): no stage data,
    /// its I/O is derived from the code (`crate::linked`).
    Linked(LinkedIo),
}

/// What a linked program's I/O is derived with besides its code: pipeline
/// state the title passes beside it ([`GraphicsRegs`], [`InputLayout`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkedIo {
    /// Vertex: the pipeline's input elements. The code fetches its vertices
    /// itself; each of its loads is one of these, by slot and byte offset.
    pub inputs: Vec<InputElement>,
    /// Pixel: `SPI_PS_INPUT_ENA`, which system values arrive in VGPRs.
    pub input_ena: u32,
    /// Pixel: `SPI_SHADER_COL_FORMAT`, 4 bits per render target: what the
    /// code's exports are specialised to (4: two 16-bit floats per dword).
    pub col_format: u32,
    /// Pixel: `SPI_PS_INPUT_CNTL_<k>` of attribute k (`v_interp` `attr<k>`):
    /// the vertex program's parameter it reads in bits 0-4, and how.
    pub input_cntl: Vec<u32>,
    /// The stage's `SPI_SHADER_PGM_RSRC2`: its count of user SGPRs in bits
    /// 1-5 ([`LinkedIo::user_sgprs`]).
    pub rsrc2: u32,
}

impl LinkedIo {
    /// User SGPRs the hardware loads before the system ones (a pixel
    /// program's primitive mask comes next).
    pub fn user_sgprs(&self) -> u8 {
        (self.rsrc2 >> 1 & 0x1F) as u8
    }
}

/// One element of a pipeline's input layout, as D3D12 describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputElement {
    pub name: String,
    pub index: u32,
    /// DXGI format, as the title numbers it.
    pub format: u32,
    pub slot: u32,
    pub offset: u32,
    pub per_instance: bool,
    pub step_rate: u32,
}

/// A mesh program's outputs and limits. It runs as one hardware NGG stage:
/// lane n of the group (`threads` lanes) exports vertex n (`exp pos`,
/// `exp param`) and primitive n (`exp prim`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshIo {
    /// `exp param<n>` targets; `param` bit 7 set: a per-primitive output,
    /// `row` then a row of the primitive signature (`PSG1`).
    pub outputs: Vec<VsOutput>,
    /// `PA_CL_VS_OUT_CNTL`: which clip and cull distances `pos1+` carry.
    pub clip_cntl: u32,
    /// Thread-group size in lanes (the HLSL group rounded up to waves).
    pub threads: u32,
    pub max_vertices: u32,
    pub max_primitives: u32,
    /// 1 lines, 2 triangles.
    pub topology: u8,
    pub lds_bytes: u32,
}

/// A compute program's dispatch settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputeIo {
    /// `COMPUTE_PGM_RSRC2`: user SGPRs, which system SGPRs and VGPRs
    /// follow them, LDS size.
    pub rsrc2: u32,
    /// Thread-group size (`numthreads`).
    pub threads: [u32; 3],
}

impl ComputeIo {
    /// User SGPRs before the system ones.
    pub fn user_sgprs(&self) -> u8 {
        (self.rsrc2 >> 1 & 0x1F) as u8
    }

    /// The SGPR of each enabled thread-group id component, in order after
    /// the user SGPRs, then the thread-group info SGPR if enabled.
    pub fn system_sgprs(&self) -> ([Option<u8>; 3], Option<u8>) {
        let mut next = self.user_sgprs();
        let mut take = |bit: u32| {
            (self.rsrc2 >> bit & 1 != 0).then(|| {
                next += 1;
                next - 1
            })
        };
        let ids = [take(7), take(8), take(9)];
        (ids, take(10))
    }

    /// SGPRs the hardware initializes: the user SGPRs, the enabled system
    /// ones, then the scratch wave offset when scratch is enabled (bit 0).
    /// The ones above hold whatever the previous wave left.
    pub fn initialized_sgprs(&self) -> u8 {
        let (ids, info) = self.system_sgprs();
        let end = ids.into_iter().chain([info]).flatten().map(|s| s + 1).max();
        end.unwrap_or(0).max(self.user_sgprs()) + (self.rsrc2 & 1) as u8
    }

    /// Thread id components in v0, v1, v2 (1 to 3).
    pub fn thread_id_vgprs(&self) -> u8 {
        (self.rsrc2 >> 11 & 3) as u8 + 1
    }

    /// LDS bytes per thread group (512-byte granules).
    pub fn lds_bytes(&self) -> u32 {
        (self.rsrc2 >> 15 & 0x1FF) * 512
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootType {
    Constants,
    /// A 64-bit buffer address.
    Descriptor,
    /// The 32-bit heap address of a descriptor table range.
    Table,
    /// A sampler the root signature fixes.
    ImmediateSampler,
    /// The 32-bit address of the CBV/SRV/UAV heap (SM 6.6 dynamic resources).
    ResourceHeap,
    /// The 32-bit address of the sampler heap.
    SamplerHeap,
    Other(u32),
}

impl RootType {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => RootType::Constants,
            1 => RootType::Descriptor,
            3 => RootType::Table,
            4 => RootType::ImmediateSampler,
            5 => RootType::ResourceHeap,
            6 => RootType::SamplerHeap,
            n => RootType::Other(n),
        }
    }
}

/// One root parameter (or table range) and the user SGPRs holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootParam {
    pub root: RootType,
    /// Of the range within its table.
    pub table_offset_dwords: u32,
    /// User SGPRs from `user_data` on.
    pub sgprs: u32,
    /// Shader register of the first descriptor (`b`/`t`/`u`/`s` number).
    pub base_register: u32,
    /// First user-data dword ([`HwStage::user_sgpr`]).
    pub user_data: u8,
    /// Index of the parameter in the root signature.
    pub slot: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    Srv,
    Sampler,
    Cbv,
    Uav,
    Other(u8),
}

/// A descriptor range the program reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsedResource {
    pub kind: ResourceType,
    pub root: RootType,
    pub table_offset_dwords: u32,
    /// Shader register of the first descriptor read.
    pub lower_bound: u32,
    pub stride: u32,
    /// Size of the descriptor (4 or 8).
    pub dwords: u8,
    pub user_data: u8,
    pub slot: u8,
}

/// The native programs in a `DXIL` part's data, or in an `ACCC` part's
/// ([`fspx_program`], or a linked program we wrapped ourselves:
/// [`linked_container`]); empty for the PC layout.
pub fn native_programs(part: &[u8]) -> Result<Vec<NativeProgram>, ShaderError> {
    if part.starts_with(b"FSPX") {
        return Ok(vec![fspx_program(part)?]);
    }
    if part.starts_with(LINKED_MAGIC) {
        return Ok(vec![linked_part(part)?]);
    }
    if part.get(XDAT_OFFSET..XDAT_OFFSET + 4) != Some(b"XDAT") {
        return Ok(Vec::new());
    }
    let count = u32_at(part, XDAT_OFFSET + 4)? as usize;
    if count > MAX_XDAT_ENTRIES {
        return Err(Malformed("implausible XDAT entry count"));
    }
    let mut out = Vec::new();
    for i in 0..count {
        let at = XDAT_OFFSET + 12 + 12 * i;
        if u32_at(part, at)? != KIND_NATIVE {
            continue;
        }
        let offset = u32_at(part, at + 4)?.into();
        let size = u32_at(part, at + 8)?.into();
        let blob = bytes(part, offset, size).ok_or(Malformed("XDAT entry out of bounds"))?;
        out.extend(programs(&unpack(blob)?)?);
    }
    Ok(out)
}

/// The compute program of an `ACCC` part, which a container carries in place
/// of `DXIL` when another compiler made the native code (the AMD
/// FidelityFX upscaler's shaders). Layout as observed (offsets from "FSPX"):
///
/// ```text
/// +0x00 "FSPX" u32 version (1), u32 entry count, u32 size; entries of 16 B
///       from +0x10: u32 kind, u32 offset, u32 size, u32 0
/// kind 0  pipeline registers, 0x1c B: +0x0c u16 x3 thread-group size,
///         +0x12 u16 flags (bit 4 wave32), +0x14 COMPUTE_PGM_RSRC1,
///         +0x18 COMPUTE_PGM_RSRC2
/// kind 1  pipeline metadata (a hash); kind 6 the GFX10 code
/// kind 7  program metadata: u32 0x403, u32 size - 0x14, u32 code dwords,
///         u64 0, u32 0x440 in its low 15 bits and the count of records
///         above them; then 32-byte records from +0x18 ([`resource_records`])
/// ```
///
/// (The count was read as "u8 0x40, u8 first user-data dword, u8 count of
/// 0x41 records" until the linked programs' metadata showed what it counts:
/// every record, in all 9 blocks compared.)
pub fn fspx_program(part: &[u8]) -> Result<NativeProgram, ShaderError> {
    if u32_at(part, 4)? != FSPX_VERSION {
        return Err(Malformed("unknown FSPX version"));
    }
    let count = u32_at(part, 8)? as usize;
    if count > MAX_XDAT_ENTRIES {
        return Err(Malformed("implausible FSPX entry count"));
    }
    let entry = |kind: u32| -> Result<&[u8], ShaderError> {
        for i in 0..count {
            let at = FSPX_HEADER + 16 * i;
            if u32_at(part, at)? == kind {
                let (offset, size) = (u32_at(part, at + 4)?, u32_at(part, at + 8)?);
                return bytes(part, offset.into(), size.into())
                    .ok_or(Malformed("FSPX entry out of bounds"));
            }
        }
        Err(Malformed("FSPX entry missing"))
    };
    let packet = entry(FSPX_PACKET)?;
    let code = entry(FSPX_CODE)?;
    let meta = entry(FSPX_METADATA)?;
    if u32_at(meta, 8)? as usize * 4 != code.len() {
        return Err(Malformed("FSPX code size disagrees"));
    }
    let wave64 = u16_at(packet, PACKET_FLAGS)? & PACKET_WAVE32 == 0;
    // VGPRs in granules of 4 (wave64) or 8 (wave32).
    let granules = (u32_at(packet, PACKET_RSRC1)? & 0x3F) + 1;
    let vgprs = (granules * if wave64 { 4 } else { 8 }).min(256) as u16;
    let threads = |i: usize| u16_at(packet, PACKET_THREADS + 2 * i).map(u32::from);
    let (layout, used) = resource_records(meta.get(FSPX_RECORDS..).unwrap_or(&[]))?;
    Ok(NativeProgram {
        stage: HwStage::Cs,
        wave64,
        vgprs,
        layout,
        used,
        io: StageIo::Cs(ComputeIo {
            rsrc2: u32_at(packet, PACKET_RSRC2)?,
            threads: [threads(0)?, threads(1)?, threads(2)?],
        }),
        code: code.to_vec(),
    })
}

/// A compute pipeline as device method 0x218 gets it, made a container of
/// one `ACCC` part ([`fspx_program`] reads it like any other): the four
/// blocks a deserialized compute pipeline names are the entries of an
/// `FSPX` part, kinds 0 (`packet`), 1 (`pipeline_meta`), 6 (`code`) and 7
/// (`meta`), in the same layout.
///
/// Everything [`fspx_program`] reads is checked first, and that code and
/// metadata are one program's (the hash of the code's leading `v_nop`s is
/// the metadata's 0x43 record, as in [`linked_program`]): a block that is
/// something else is an error, not a container.
pub fn deserialized_compute_container(
    packet: &[u8],
    pipeline_meta: &[u8],
    code: &[u8],
    meta: &[u8],
) -> Result<Vec<u8>, ShaderError> {
    if packet.len() != FSPX_PACKET_SIZE {
        return Err(Malformed("a compute packet of another size"));
    }
    if u32_at(meta, 0)? != FSPX_META_TAG {
        return Err(Malformed("unknown compute metadata"));
    }
    if u32_at(meta, 4)? as usize + FSPX_META_UNCOUNTED != meta.len() {
        return Err(Malformed("compute metadata size disagrees"));
    }
    if u32_at(meta, 8)? as usize * 4 != code.len() {
        return Err(Malformed("compute code size disagrees"));
    }
    let count = u32_at(meta, FSPX_META_COUNT)?;
    if count & 0x7FFF != LINKED_MARK {
        return Err(Malformed("unknown compute metadata"));
    }
    if FSPX_RECORDS + FSPX_RECORD * (count >> 15) as usize != meta.len() {
        return Err(Malformed("compute metadata record count disagrees"));
    }
    let hash = match meta[FSPX_RECORDS..].as_chunks::<FSPX_RECORD>().0.last() {
        Some(e) if e[0] == 0x43 => [le32(e, 4), le32(e, 8), le32(e, 12), le32(e, 16)],
        _ => return Err(Malformed("compute metadata without a hash")),
    };
    if code_hash(code)? != hash {
        return Err(Malformed("compute code and metadata of different programs"));
    }
    let entries = [
        (FSPX_PACKET, packet),
        (FSPX_PIPELINE_METADATA, pipeline_meta),
        (FSPX_CODE, code),
        (FSPX_METADATA, meta),
    ];
    let mut at = FSPX_HEADER + 16 * entries.len();
    let total = at + entries.iter().map(|(_, b)| b.len()).sum::<usize>();
    let mut part = b"FSPX".to_vec();
    for v in [FSPX_VERSION, entries.len() as u32, total as u32] {
        part.extend(v.to_le_bytes());
    }
    for (kind, block) in &entries {
        for v in [*kind, at as u32, block.len() as u32, 0] {
            part.extend(v.to_le_bytes());
        }
        at += block.len();
    }
    for (_, block) in &entries {
        part.extend(*block);
    }
    fspx_program(&part)?;
    Ok(crate::dxbc::build(&[(crate::FourCC::ACCC, &part)]))
}

/// The root parameters and used resources that the 32-byte records of a
/// program's metadata list: the records of an `FSPX` part's kind 7
/// ([`fspx_program`]) and of a linked program ([`linked_program`]) alike.
/// Each starts with a u32 tag:
///
/// ```text
/// 0x41  a resource the code reads: u8 resource type (as FSSM's used
///       entries), u8 root type, u16 0, u32 table offset (dwords), u32
///       register, u32 (see below), u8 descriptor dwords, u8 user-data
///       dword, u8 root slot, u8 0, u32 -1, u32 0
/// 0x42  a view declaration: u32 0, u8 1, u8 class (0 SRV, 1 UAV),
///       u8 resource kind (DXIL's), u8 0, u32 register
/// 0x43  the shader hash (16 B); the last record
/// ```
///
/// The u32 after the register was read as a view dimension (1 buffer, 4 2D,
/// 0 root view). In the linked programs it is 4 for a texture and looks like
/// the stride for a buffer (0x80, 0x40; 0x88, 0x44, 0x5c in the compute
/// ones).
/// shortcut: it is not taken as the stride (`stride` stays 0, as before),
/// until a program that indexes such a buffer shows which reading is right.
fn resource_records(records: &[u8]) -> Result<(Vec<RootParam>, Vec<UsedResource>), ShaderError> {
    let mut layout: Vec<RootParam> = Vec::new();
    let mut used = Vec::new();
    for e in records.as_chunks::<FSPX_RECORD>().0 {
        match e[0] {
            0x41 => {}
            0x42 => continue,
            0x43 => break,
            _ => return Err(Malformed("unknown program metadata record")),
        }
        let root = RootType::from_raw(e[5].into());
        let u = UsedResource {
            kind: match e[4] {
                0 => ResourceType::Srv,
                1 => ResourceType::Sampler,
                2 => ResourceType::Cbv,
                3 => ResourceType::Uav,
                n => ResourceType::Other(n),
            },
            root,
            table_offset_dwords: le32(e, 8),
            lower_bound: le32(e, 12),
            stride: 0,
            dwords: e[20],
            user_data: e[21],
            slot: e[22],
        };
        let sgprs = match root {
            RootType::Table => 1,
            RootType::Descriptor => 2,
            // The code holds its words; no user data.
            RootType::ImmediateSampler => 0,
            // Root constants (the linked pixel programs' `b500`, indices
            // into the sampler heap). The record does not say how many: its
            // descriptor-dwords byte is 4 where the stage's user SGPRs
            // leave room for two, and two are read.
            // shortcut: only the first dword is the parameter's here; a
            // program that reads another finds an SGPR nothing fills and is
            // refused. (`linked_part`, which knows the stage's user SGPRs,
            // widens it.)
            RootType::Constants => 1,
            _ => return Err(Malformed("a metadata resource of an unknown root type")),
        };
        if sgprs > 0 && !layout.iter().any(|p| p.slot == u.slot) {
            layout.push(RootParam {
                root,
                table_offset_dwords: 0,
                sgprs,
                base_register: u.lower_bound,
                user_data: u.user_data,
                slot: u.slot,
            });
        }
        used.push(u);
    }
    Ok((layout, used))
}

/// The 128-bit hash the console compiler stores in the operand fields of a
/// program's two leading `v_nop`s (three dwords each, the third a literal):
/// dwords 0, 2, 3 and 5, the upper halves of dwords 0 and 3 taken from the
/// dword after each around its bit 8. The metadata's 0x43 record holds the
/// same four dwords.
fn code_hash(code: &[u8]) -> Result<[u32; 4], ShaderError> {
    let w = |i: usize| u32_at(code, 4 * i);
    let split = |low: u32, next: u32| low & 0xFFFF | (next & 0xFF) << 16 | (next >> 9 & 0xFF) << 24;
    Ok([split(w(0)?, w(1)?), w(2)?, split(w(3)?, w(4)?), w(5)?])
}

/// A vertex or pixel program as device method 0x1c8 gets it: the code as
/// the hardware runs it (linked: the vertex fetch inlined, the pixel exports
/// specialised to their render targets' formats) and its metadata, without
/// a container or stage data. `rsrc1` is the stage's
/// `SPI_SHADER_PGM_RSRC1` and `wave64` its wave size, both from the
/// pipeline's registers ([`GraphicsRegs`]). Layout as observed:
///
/// ```text
/// code  two v_nop holding the program's hash, the code up to s_endpgm,
///       ten s_code_end, a trailer of 5 dwords, padding
/// meta  +0x00 u32 2, +0x04 u32 size - 0x0c, +0x08 u32 code dwords - 10,
///       +0x0c u32 0x440 in its low 15 bits and the count of records above
///       them; then 32-byte records from +0x10 ([`resource_records`])
/// ```
///
/// Everything stated is checked, and that code and metadata are one
/// program's (the hash): a block that is something else is an error, not a
/// program. The result's I/O is [`StageIo::Linked`] with nothing in it: the
/// caller adds what the pipeline says ([`LinkedIo`]).
pub fn linked_program(
    stage: HwStage,
    code: &[u8],
    meta: &[u8],
    rsrc1: u32,
    wave64: bool,
) -> Result<NativeProgram, ShaderError> {
    if !matches!(stage, HwStage::Vs | HwStage::Ps) {
        return Err(Malformed("a linked program of another stage"));
    }
    if u32_at(meta, 0)? != LINKED_TAG {
        return Err(Malformed("unknown linked metadata"));
    }
    if u32_at(meta, 4)? as usize + 0xc != meta.len() {
        return Err(Malformed("linked metadata size disagrees"));
    }
    if (u32_at(meta, 8)? as usize + LINKED_CODE_EXTRA) * 4 != code.len() {
        return Err(Malformed("linked code size disagrees"));
    }
    let count = u32_at(meta, 0xc)?;
    if count & 0x7FFF != LINKED_MARK {
        return Err(Malformed("unknown linked metadata"));
    }
    let count = (count >> 15) as usize;
    if LINKED_RECORDS + FSPX_RECORD * count != meta.len() {
        return Err(Malformed("linked metadata record count disagrees"));
    }
    let records = &meta[LINKED_RECORDS..];
    let hash = match records.as_chunks::<FSPX_RECORD>().0.last() {
        Some(e) if e[0] == 0x43 => [le32(e, 4), le32(e, 8), le32(e, 12), le32(e, 16)],
        _ => return Err(Malformed("linked metadata without a hash")),
    };
    if code_hash(code)? != hash {
        return Err(Malformed("linked code and metadata of different programs"));
    }
    let (layout, used) = resource_records(records)?;
    // VGPRs in granules of 4 (wave64) or 8 (wave32).
    let granules = (rsrc1 & 0x3F) + 1;
    Ok(NativeProgram {
        stage,
        wave64,
        vgprs: (granules * if wave64 { 4 } else { 8 }).min(256) as u16,
        layout,
        used,
        io: StageIo::Linked(LinkedIo::default()),
        code: code.to_vec(),
    })
}

/// A linked program as a container of our own making, so that it goes
/// wherever the title's containers go (the shader table, the translation
/// cache, [`native_programs`]): one `ACCC` part, native code in place of
/// `DXIL`, that holds what [`linked_program`] is given and `io`.
///
/// ```text
/// "VLNK" u32 version (1), u32 stage (0 vertex, 1 pixel), u32 wave64,
///        u32 rsrc1, u32 rsrc2, u32 SPI_PS_INPUT_ENA,
///        u32 SPI_SHADER_COL_FORMAT, u32 input control count, u32 input
///        element count, u32 code bytes, u32 metadata bytes; then the input
///        controls, the elements (as in the title's layout block,
///        [`InputLayout`]), the code, the metadata
/// ```
///
/// A program whose metadata declares a sampler no record places (a 0x42
/// record of class 3) picks it from the sampler heap, by an index in a root
/// constant: its layout gets the heap, whose base the code reads from the
/// stage's last user SGPR.
/// shortcut: that the last user-data dword is the sampler heap's base is
/// read off the code of the eight pixel programs seen that do this (they
/// load 16 bytes at `s11 + (constant << 4)` with 12 user SGPRs, one of
/// them for each of two constants), not off anything that says so.
///
/// Fails where [`linked_program`] does, and where the part would not give
/// its program back: a container is only made of a program that is one.
pub fn linked_container(
    stage: HwStage,
    code: &[u8],
    meta: &[u8],
    rsrc1: u32,
    wave64: bool,
    io: &LinkedIo,
) -> Result<Vec<u8>, ShaderError> {
    let mut part = LINKED_MAGIC.to_vec();
    let head = [
        LINKED_VERSION,
        u32::from(stage == HwStage::Ps),
        u32::from(wave64),
        rsrc1,
        io.rsrc2,
        io.input_ena,
        io.col_format,
        io.input_cntl.len() as u32,
        io.inputs.len() as u32,
        code.len() as u32,
        meta.len() as u32,
    ];
    for v in head.iter().chain(&io.input_cntl) {
        part.extend(v.to_le_bytes());
    }
    for e in &io.inputs {
        part.extend((e.name.len() as u32 + 1).to_le_bytes());
        part.extend(e.name.as_bytes());
        part.push(0);
        let fields = [e.index, e.format, e.slot, e.offset, u32::from(e.per_instance), e.step_rate];
        part.extend(fields.iter().flat_map(|v| v.to_le_bytes()));
    }
    part.extend(code);
    part.extend(meta);
    linked_part(&part)?;
    Ok(crate::dxbc::build(&[(crate::FourCC::ACCC, &part)]))
}

/// The program of a [`linked_container`]'s part.
fn linked_part(part: &[u8]) -> Result<NativeProgram, ShaderError> {
    const SHORT: ShaderError = Malformed("linked part out of bounds");
    let mut rest = part.get(4..).ok_or(SHORT)?;
    let mut take = |n: usize| -> Result<&[u8], ShaderError> {
        let (taken, after) = rest.split_at_checked(n).ok_or(SHORT)?;
        rest = after;
        Ok(taken)
    };
    let head: Vec<u32> = take(44)?.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
    let [version, stage, wave64, rsrc1, rsrc2, input_ena, col_format, cntl, elements, code, meta] = head[..] else {
        return Err(SHORT);
    };
    if version != LINKED_VERSION || stage > 1 || wave64 > 1 {
        return Err(Malformed("unknown linked part"));
    }
    if cntl > MAX_SEMANTICS || elements > MAX_INPUT_ELEMENTS {
        return Err(Malformed("implausible linked part"));
    }
    let input_cntl = take(4 * cntl as usize)?.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
    let mut inputs = Vec::new();
    for _ in 0..elements {
        inputs.push(input_element(&mut take)?);
    }
    let (code, meta) = (take(code as usize)?, take(meta as usize)?);
    let stage = if stage == 1 { HwStage::Ps } else { HwStage::Vs };
    let mut program = linked_program(stage, code, meta, rsrc1, wave64 == 1)?;
    let io = LinkedIo {
        inputs,
        input_ena,
        col_format,
        input_cntl,
        rsrc2,
    };
    // (`linked_program` has checked the metadata's size)
    let records = meta[LINKED_RECORDS..].as_chunks::<FSPX_RECORD>().0;
    let placed = |register: u32| records.iter().any(|e| e[0] == 0x41 && e[4] == 1 && le32(e, 12) == register);
    if records.iter().any(|e| e[0] == 0x42 && e[9] == 3 && !placed(le32(e, 12))) {
        let last = io.user_sgprs().checked_sub(1).ok_or(Malformed("a heap sampler without user data"))?;
        if program.layout.iter().any(|p| u32::from(p.user_data) + p.sgprs > u32::from(last)) {
            return Err(Malformed("a heap sampler without room for its heap"));
        }
        program.layout.push(RootParam {
            root: RootType::SamplerHeap,
            table_offset_dwords: 0,
            sgprs: 1,
            base_register: 0,
            user_data: last,
            // No parameter of the title's root signature.
            slot: u8::MAX,
        });
    }
    // Root constants run up to the next parameter, or to the end of the
    // user SGPRs: their record does not say how many there are
    // ([`resource_records`]), and one program reads two (both of them
    // sampler indices, in s9 and s10 before the heap's base in s11).
    let starts: Vec<u8> = program.layout.iter().map(|p| p.user_data).collect();
    for p in program.layout.iter_mut().filter(|p| p.root == RootType::Constants) {
        let next = starts.iter().copied().filter(|&s| s > p.user_data).min();
        let end = next.unwrap_or(io.user_sgprs());
        if end > p.user_data {
            p.sgprs = u32::from(end - p.user_data);
        }
    }
    program.io = StageIo::Linked(io);
    Ok(program)
}

/// One input element as the title's layout block and our linked part hold
/// it, unaligned: u32 name length with its NUL, the name, u32 semantic
/// index, u32 format, u32 slot, u32 byte offset, u32 class (1 per
/// instance), u32 step rate.
fn input_element<'a>(
    take: &mut impl FnMut(usize) -> Result<&'a [u8], ShaderError>,
) -> Result<InputElement, ShaderError> {
    let len = le32(take(4)?, 0) as usize;
    let name = match take(len)?.split_last() {
        Some((0, name)) if name.iter().all(|b| b.is_ascii_graphic()) => name,
        _ => return Err(Malformed("an input element's name")),
    };
    let name = String::from_utf8_lossy(name).into_owned();
    let f = take(24)?;
    let class = le32(f, 16);
    if class > 1 {
        return Err(Malformed("an input element's class"));
    }
    Ok(InputElement {
        name,
        index: le32(f, 0),
        format: le32(f, 4),
        slot: le32(f, 8),
        offset: le32(f, 12),
        per_instance: class == 1,
        step_rate: le32(f, 20),
    })
}

/// The register image device method 0x1c8 gets with a pipeline's programs.
/// Offsets as observed in the 16 descriptions a run dumped; the ones marked
/// guess track nothing else we can see.
///
/// ```text
/// +0x28 u16 offset (from +0x1c) and u16 size of the image's last section,
///       which ends the image
/// +0x38 SPI_SHADER_PGM_RSRC1_VS, +0x3c RSRC2_VS
/// +0x4c SPI_SHADER_PGM_RSRC1_PS, +0x50 RSRC2_PS (0 without a pixel program)
/// +0x58 SPI_PS_INPUT_ENA (guess: 2, and i/j are in v0/v1)
/// +0x5c SPI_PS_IN_CONTROL (NUM_INTERP is the vertex program's parameter
///       count; bit 15 PS_W32_EN)
/// +0x70 SPI_SHADER_COL_FORMAT (4: FP16_ABGR; 0 without a pixel program)
/// +0x7c CB_COLOR_CONTROL
/// +0x84 SPI_VS_OUT_CONFIG
/// +0xb8 DB_DEPTH_CONTROL, +0xbc DB_STENCIL_CONTROL (guess: they decode as
///       such in every image: depth off and "always" where only colour is
///       drawn, depth on and written in the one without a pixel program)
/// +0xc0 DB_STENCILREFMASK, +0xc4 DB_STENCILREFMASK_BF (guess)
/// +0xd0 CB_BLEND0_CONTROL to CB_BLEND7_CONTROL
/// last  14 dwords, then SPI_PS_INPUT_CNTL_0 to _<NUM_INTERP - 1>, padded to
///       an even count, then 3 dwords
/// ```
///
/// The last section is at +0x138 in the images of vertex plus pixel
/// pipelines, which are 0x17c bytes without interpolants, 0x184 with two,
/// 0x18c with three. An image that has it elsewhere (one of 0x200 bytes was
/// seen, of a pipeline with another geometry stage in place of the vertex
/// program) has other registers before it, so none of these offsets hold:
/// it is refused.
///
/// Rasterizer state and the vertex stage's wave size are not identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsRegs {
    pub vs_rsrc1: u32,
    pub vs_rsrc2: u32,
    pub ps_rsrc1: u32,
    pub ps_rsrc2: u32,
    pub ps_input_ena: u32,
    pub ps_in_control: u32,
    pub col_format: u32,
    pub cb_color_control: u32,
    pub vs_out_config: u32,
    pub db_depth_control: u32,
    pub db_stencil_control: u32,
    /// Front, then back.
    pub db_stencil_ref_mask: [u32; 2],
    pub blend: [u32; 8],
    /// `SPI_PS_INPUT_CNTL_<k>`, one per interpolated parameter.
    pub ps_input_cntl: Vec<u32>,
}

impl GraphicsRegs {
    pub fn parse(image: &[u8]) -> Result<Self, ShaderError> {
        let at = |at: usize| u32_at(image, at);
        let section = at(REGS_SECTION)?;
        let (start, size) = (
            REGS_SECTION_BASE + (section & 0xFFFF) as usize,
            (section >> 16) as usize,
        );
        if start != REGS_LAST {
            return Err(Malformed("a register image of another layout"));
        }
        if start + size != image.len() {
            return Err(Malformed("register image size disagrees"));
        }
        let ps_in_control = at(REGS_PS_IN_CONTROL)?;
        let interps = (ps_in_control & 0x3F) as usize;
        if size != 4 * (REGS_LAST_HEAD + interps.next_multiple_of(2) + REGS_LAST_TAIL) {
            return Err(Malformed("register image interpolant count disagrees"));
        }
        let mut blend = [0; 8];
        for (i, b) in blend.iter_mut().enumerate() {
            *b = at(REGS_BLEND + 4 * i)?;
        }
        Ok(GraphicsRegs {
            vs_rsrc1: at(REGS_VS_RSRC1)?,
            vs_rsrc2: at(REGS_VS_RSRC1 + 4)?,
            ps_rsrc1: at(REGS_PS_RSRC1)?,
            ps_rsrc2: at(REGS_PS_RSRC1 + 4)?,
            ps_input_ena: at(REGS_PS_INPUT_ENA)?,
            ps_in_control,
            col_format: at(REGS_COL_FORMAT)?,
            cb_color_control: at(REGS_COLOR_CONTROL)?,
            vs_out_config: at(REGS_VS_OUT_CONFIG)?,
            db_depth_control: at(REGS_DEPTH_CONTROL)?,
            db_stencil_control: at(REGS_DEPTH_CONTROL + 4)?,
            db_stencil_ref_mask: [at(REGS_STENCIL_REF_MASK)?, at(REGS_STENCIL_REF_MASK + 4)?],
            blend,
            ps_input_cntl: (0..interps)
                .map(|k| at(REGS_LAST + 4 * (REGS_LAST_HEAD + k)))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Whether the pixel stage runs 64 lanes a wave (`PS_W32_EN` clear).
    pub fn ps_wave64(&self) -> bool {
        self.ps_in_control >> 15 & 1 == 0
    }

    /// What the pixel program's I/O is derived with.
    pub fn pixel_io(&self) -> LinkedIo {
        LinkedIo {
            inputs: Vec::new(),
            input_ena: self.ps_input_ena,
            col_format: self.col_format,
            input_cntl: self.ps_input_cntl.clone(),
            rsrc2: self.ps_rsrc2,
        }
    }

    /// What the vertex program's I/O is derived with: `layout`'s elements.
    pub fn vertex_io(&self, layout: &InputLayout) -> LinkedIo {
        LinkedIo {
            inputs: layout.elements.clone(),
            rsrc2: self.vs_rsrc2,
            ..LinkedIo::default()
        }
    }

    /// Parameters the pixel stage interpolates (`NUM_INTERP`).
    pub fn num_interp(&self) -> u32 {
        self.ps_in_control & 0x3F
    }
}

/// The block of formats and input layout device method 0x1c8 gets with a
/// pipeline's programs (0x50 bytes and the elements, padded to 4). As
/// observed; the fields marked guess are constant or nearly so in the 16
/// descriptions seen:
///
/// ```text
/// +0x00 u32 a hash or id
/// +0x10 u8 x8 render target formats (one description has five targets and
///       five bytes set)
/// +0x1c u32 -1: the sample mask (guess)
/// +0x24 u8 depth format, u8 render target count, u8 topology type (3
///       triangles, 19 the console's rectangles)
/// +0x38 u32 size of what follows +0x4c, before padding
/// +0x4c u32 element count, then per element, unaligned: u32 name length
///       with its NUL, the name, u32 semantic index, u32 format, u32 slot,
///       u32 byte offset, u32 class (1 per instance), u32 step rate
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputLayout {
    pub id: u32,
    /// DXGI formats, as the title numbers them.
    pub target_formats: [u8; 8],
    pub sample_mask: u32,
    pub depth_format: u8,
    pub targets: u8,
    /// D3D12's primitive topology type, with the console's own (19, 32).
    pub topology: u8,
    pub elements: Vec<InputElement>,
}

impl InputLayout {
    pub fn parse(block: &[u8]) -> Result<Self, ShaderError> {
        const SHORT: ShaderError = Malformed("input layout out of bounds");
        let head = block.get(..LAYOUT_ELEMENTS).ok_or(SHORT)?;
        let end = u32_at(head, LAYOUT_ELEMENTS_SIZE)? as usize + LAYOUT_ELEMENT_COUNT;
        if end < LAYOUT_ELEMENTS || end.next_multiple_of(4) != block.len() {
            return Err(Malformed("input layout size disagrees"));
        }
        let count = u32_at(head, LAYOUT_ELEMENT_COUNT)?;
        if count > MAX_INPUT_ELEMENTS {
            return Err(Malformed("implausible input element count"));
        }
        let mut rest = &block[LAYOUT_ELEMENTS..end];
        let mut take = |n: usize| -> Result<&[u8], ShaderError> {
            let (taken, after) = rest.split_at_checked(n).ok_or(SHORT)?;
            rest = after;
            Ok(taken)
        };
        let mut elements = Vec::new();
        for _ in 0..count {
            elements.push(input_element(&mut take)?);
        }
        if !rest.is_empty() {
            return Err(Malformed("input layout size disagrees"));
        }
        Ok(InputLayout {
            id: u32_at(head, 0)?,
            target_formats: head[LAYOUT_TARGET_FORMATS..LAYOUT_TARGET_FORMATS + 8]
                .try_into()
                .expect("8 bytes"),
            sample_mask: u32_at(head, LAYOUT_SAMPLE_MASK)?,
            depth_format: head[LAYOUT_DEPTH_FORMAT],
            targets: head[LAYOUT_DEPTH_FORMAT + 1],
            topology: head[LAYOUT_DEPTH_FORMAT + 2],
            elements,
        })
    }
}

fn u16_at(data: &[u8], at: usize) -> Result<u16, ShaderError> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(Malformed("FSPX data out of bounds"))
}

fn unpack(blob: &[u8]) -> Result<Vec<u8>, ShaderError> {
    if blob.get(0..4) != Some(b"XBCH") {
        return Err(Malformed("native blob lacks XBCH magic"));
    }
    if u32_at(blob, 4)? != XBCH_VERSION {
        return Err(Malformed("unknown XBCH version"));
    }
    let packed = u64_at(blob, 8)?;
    let unpacked = u64_at(blob, 16)?;
    if unpacked > MAX_UNPACKED {
        return Err(Malformed("implausible XBCH unpacked size"));
    }
    let stream =
        bytes(blob, XBCH_HEADER as u64, packed).ok_or(Malformed("XBCH stream out of bounds"))?;
    let out = inflate_zlib_exact(stream, unpacked as usize)
        .ok_or(Malformed("XBCH stream does not inflate to its stated size"))?;
    Ok(out)
}

/// Inflates a zlib stream incrementally up to `expected` bytes, rejecting
/// truncated, corrupt, under-filled, or oversized streams without eagerly
/// allocating the full declared size up front.
fn inflate_zlib_exact(source: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut state = zlib_rs::Inflate::new(true, 15);
    let mut out = Vec::new();
    let (mut in_pos, mut out_pos) = (0usize, 0usize);
    loop {
        if out_pos == expected {
            let mut extra = [0u8; 1];
            let status = state
                .decompress(
                    &source[in_pos..],
                    &mut extra,
                    zlib_rs::InflateFlush::NoFlush,
                )
                .ok()?;
            return (status == zlib_rs::Status::StreamEnd
                && state.total_out() as usize == expected)
                .then_some(out);
        }
        if out_pos == out.len() {
            let next_len = if out.is_empty() {
                expected.min(source.len().saturating_mul(4).max(4096))
            } else {
                out.len().saturating_mul(2).min(expected)
            };
            out.resize(next_len, 0);
        }
        let status = state
            .decompress(
                &source[in_pos..],
                &mut out[out_pos..],
                zlib_rs::InflateFlush::NoFlush,
            )
            .ok()?;
        let next_in = state.total_in() as usize;
        let next_out = state.total_out() as usize;
        match status {
            zlib_rs::Status::StreamEnd => {
                out.truncate(next_out);
                return (next_out == expected).then_some(out);
            }
            zlib_rs::Status::Ok if next_in != in_pos || next_out != out_pos => {
                in_pos = next_in;
                out_pos = next_out;
            }
            zlib_rs::Status::Ok | zlib_rs::Status::BufError => return None,
        }
    }
}

fn programs(fssm: &[u8]) -> Result<Vec<NativeProgram>, ShaderError> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return Err(Malformed("native blob lacks FSSM magic"));
    }
    if u32_at(fssm, 4)? != FSSM_VERSION {
        return Err(Malformed("unknown FSSM version"));
    }
    let payload = &fssm[FSSM_HEADER.min(fssm.len())..];
    if u64_at(fssm, 8)? != payload.len() as u64 {
        return Err(Malformed("FSSM payload size disagrees"));
    }
    if u64_at(payload, 0)? != GRAPHICS_SLOT_COUNT {
        return Err(Malformed("unexpected FSSM slot count"));
    }
    let mut out = Vec::new();
    for (stage, at) in SLOTS {
        let start = u64_at(payload, at)?;
        if start == ABSENT {
            continue;
        }
        let record =
            bytes(payload, start, RECORD_SIZE).ok_or(Malformed("FSSM record out of bounds"))?;
        let code = bytes(payload, u64_at(record, 0)?, u64_at(record, 8)?)
            .ok_or(Malformed("native code out of bounds"))?;
        let vgprs = (u32_at(record, RECORD_REGS)? & 0x3FF) as u16;
        if vgprs > 256 {
            return Err(Malformed("implausible VGPR count"));
        }
        let table = |at: usize, count_at: usize| -> Result<&[u8], ShaderError> {
            let len = ENTRY_SIZE * u64::from(u32_at(record, count_at)?);
            if len == 0 {
                return Ok(&[]);
            }
            bytes(payload, u64_at(record, at)?, len)
                .ok_or(Malformed("FSSM root table out of bounds"))
        };
        let layout = table(RECORD_LAYOUT, RECORD_COUNTS)?
            .as_chunks::<{ ENTRY_SIZE as usize }>()
            .0
            .iter()
            .map(|e| RootParam {
                root: RootType::from_raw(le32(e, 0)),
                table_offset_dwords: le32(e, 4),
                sgprs: le32(e, 8),
                base_register: le32(e, 12),
                user_data: e[16],
                slot: e[17],
            })
            .collect();
        let used = table(RECORD_USED, RECORD_COUNTS + 4)?
            .as_chunks::<{ ENTRY_SIZE as usize }>()
            .0
            .iter()
            .map(|e| UsedResource {
                kind: match e[0] {
                    0 => ResourceType::Srv,
                    1 => ResourceType::Sampler,
                    2 => ResourceType::Cbv,
                    3 => ResourceType::Uav,
                    n => ResourceType::Other(n),
                },
                root: RootType::from_raw(e[1].into()),
                table_offset_dwords: le32(e, 4),
                lower_bound: le32(e, 8),
                stride: le32(e, 12),
                dwords: e[16],
                user_data: e[17],
                slot: e[18],
            })
            .collect();
        // The record is in bounds, so its end is too.
        let stage_data = &payload[(start + RECORD_SIZE) as usize..];
        let io = match stage {
            HwStage::Ps => ps_io(stage_data, code.len())?,
            HwStage::Vs => vs_io(stage_data)?,
            HwStage::Cs => StageIo::Cs(ComputeIo {
                rsrc2: u32_at(stage_data, CS_RSRC2)?,
                threads: [
                    u32_at(stage_data, CS_THREADS)?,
                    u32_at(stage_data, CS_THREADS + 4)?,
                    u32_at(stage_data, CS_THREADS + 8)?,
                ],
            }),
            HwStage::Ms => ms_io(stage_data, u32_at(record, RECORD_LDS)?)?,
            _ => StageIo::None,
        };
        out.push(NativeProgram {
            stage,
            wave64: u32_at(record, RECORD_FLAGS)? & FLAG_WAVE32 == 0,
            vgprs,
            layout,
            used,
            io,
            code: code.to_vec(),
        });
    }
    Ok(out)
}

/// The entries of `size` bytes from `at`, with their count at `count_at`.
fn semantics(data: &[u8], at: usize, size: usize, count_at: usize) -> Result<&[u8], ShaderError> {
    let count = u32_at(data, count_at)?;
    if count > MAX_SEMANTICS {
        return Err(Malformed("implausible semantic count"));
    }
    data.get(at..at + size * count as usize)
        .ok_or(Malformed("stage data out of bounds"))
}

fn ps_io(data: &[u8], code_len: usize) -> Result<StageIo, ShaderError> {
    let inputs = semantics(data, PS_INPUTS, 8, PS_INPUT_COUNT)?
        .as_chunks::<8>()
        .0
        .iter()
        .map(|e| PsInput {
            usage: Usage::from_raw(e[0]),
            row: e[1],
            mask: e[2] & 0xf,
            attr: e[3],
            flat: e[7] & 1 != 0,
            fp16: e[7] & 8 != 0,
            pack2: e[7] & 0x10 != 0,
            row2: e[5],
        })
        .collect();
    let size = u32_at(data, PS_PATCH_SIZE)? as usize;
    if size > PATCH_SLOT_DWORDS {
        return Err(Malformed("implausible export patch size"));
    }
    let mut patches = Vec::new();
    for mrt in 0..MRTS {
        let at = PS_PATCHES + mrt * PATCH_BYTES;
        let offset = u32_at(data, at)?;
        if offset == u32::MAX {
            continue;
        }
        if (offset as usize + size) * 4 > code_len {
            return Err(Malformed("export patch outside the code"));
        }
        let full = at + 4 + PATCH_FULL * PATCH_SLOT_DWORDS * 4;
        let code = (0..size)
            .map(|i| u32_at(data, full + 4 * i))
            .collect::<Result<_, _>>()?;
        patches.push(ExportPatch {
            mrt: mrt as u8,
            offset,
            code,
        });
    }
    Ok(StageIo::Ps {
        inputs,
        input_addr: u32_at(data, PS_INPUT_ADDR)?,
        db_shader_control: u32_at(data, PS_DB_SHADER_CONTROL)?,
        patches,
    })
}

fn vs_outputs(data: &[u8]) -> Result<Vec<VsOutput>, ShaderError> {
    Ok(semantics(data, VS_OUTPUTS, 6, VS_OUTPUT_COUNT)?
        .as_chunks::<6>()
        .0
        .iter()
        .map(|e| VsOutput {
            usage: Usage::from_raw(e[0]),
            row: e[1],
            param: e[2],
            fp16: e[5] & 1 != 0,
            pack2: e[5] & 2 != 0,
        })
        .collect())
}

fn vs_io(data: &[u8]) -> Result<StageIo, ShaderError> {
    let inputs = semantics(data, VS_INPUTS, 5, VS_INPUT_COUNT)?
        .as_chunks::<5>()
        .0
        .iter()
        .map(|e| VsInput {
            usage: Usage::from_raw(e[0]),
            row: e[1],
            mask: e[2] & 0xf,
            vgpr: e[3],
        })
        .collect();
    let outputs = vs_outputs(data)?;
    Ok(StageIo::Vs { inputs, outputs })
}

fn ms_io(data: &[u8], lds_bytes: u32) -> Result<StageIo, ShaderError> {
    let outputs = vs_outputs(data)?;
    let info = data
        .get(MS_INFO..MS_INFO + 8)
        .ok_or(Malformed("stage data out of bounds"))?;
    let half = |at: usize| u32::from(u16::from_le_bytes([info[at], info[at + 1]]));
    Ok(StageIo::Ms(MeshIo {
        outputs,
        clip_cntl: u32_at(data, MS_CLIP_CNTL)?,
        threads: half(0),
        max_vertices: half(2),
        max_primitives: half(4),
        topology: info[7],
        lds_bytes,
    }))
}

/// For entries already sized by `as_chunks`.
fn le32(entry: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(entry[at..at + 4].try_into().unwrap())
}

/// `data[offset..offset + len]` with untrusted 64-bit bounds.
fn bytes(data: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    data.get(start..start.checked_add(usize::try_from(len).ok()?)?)
}

/// What a run with `VICTORY_PSO_DUMP` wrote of device methods 0x1c8's and
/// 0x218's descriptions, when the worktree has it (`.agente/pso-dump`): the
/// tests on the title's own blocks read them from there and pass without
/// them, since no game data is kept in the repository.
#[cfg(test)]
pub(crate) mod dumped {
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.agente/pso-dump"))
    }

    /// The block description `description` ("00001") of device method 0x1c8
    /// names at `word`.
    pub(crate) fn block(description: &str, word: u32) -> Option<Vec<u8>> {
        block_of("1c8", description, word)
    }

    /// The block a description of the method `slot` ("1c8", "218") names at
    /// `word`.
    pub(crate) fn block_of(slot: &str, description: &str, word: u32) -> Option<Vec<u8>> {
        let prefix = format!("{slot}-{description}-w{word}-");
        let entry = std::fs::read_dir(dir()).ok()?.flatten().find(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".bin")
        })?;
        std::fs::read(entry.path()).ok()
    }

    /// Every description of device method 0x1c8 dumped, in order.
    pub(crate) fn descriptions() -> Vec<String> {
        descriptions_of("1c8")
    }

    /// Every description of the method `slot` dumped, in order.
    pub(crate) fn descriptions_of(slot: &str) -> Vec<String> {
        let prefix = format!("{slot}-");
        let mut out: Vec<String> = std::fs::read_dir(dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                Some(name.strip_prefix(&prefix)?.strip_suffix(".txt")?.to_owned())
            })
            .collect();
        out.sort();
        out
    }

    /// The compute container of a description of device method 0x218
    /// (`super::deserialized_compute_container`), if all its blocks were
    /// dumped.
    pub(crate) fn compute(description: &str) -> Option<Result<Vec<u8>, crate::ShaderError>> {
        let b = |word| block_of("218", description, word);
        Some(super::deserialized_compute_container(&b(1)?, &b(3)?, &b(5)?, &b(7)?))
    }
}

/// Linked programs made of a few instructions, in the form device method
/// 0x1c8 gets them: for the tests here and of the translation.
#[cfg(test)]
pub(crate) mod fixture {
    use super::{
        FSPX_META_TAG, FSPX_META_UNCOUNTED, FSPX_RECORD, FSPX_RECORDS, LINKED_CODE_EXTRA, LINKED_MARK, LINKED_RECORDS,
        LINKED_TAG,
    };

    pub(crate) const HASH: [u32; 4] = [0xD21D_A4DF, 0x1234_5678, 0x91FE_D799, 0x9ABC_DEF0];

    pub(crate) fn words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A linked code block: the two `v_nop`s holding `hash`, `body`,
    /// `s_endpgm`, and the ten dwords the metadata does not count.
    pub(crate) fn linked_code(hash: [u32; 4], body: &[u32]) -> Vec<u8> {
        let nop = |h: u32, literal: u32| {
            [0xD580_0000 | h & 0xFFFF, 0x03FE_0100 | h >> 16 & 0xFF | (h >> 24) << 9, literal]
        };
        let mut code = nop(hash[0], hash[1]).to_vec();
        code.extend(nop(hash[2], hash[3]));
        code.extend(body);
        code.push(0xBF81_0000);
        code.extend([0xBF9F_0000; LINKED_CODE_EXTRA]);
        words(&code)
    }

    /// A resource record: an SRV of `dwords` at `offset` dwords of the table
    /// in user-data dword 8 (root slot 0).
    pub(crate) fn table_record(register: u32, offset: u32, dwords: u8) -> [u8; FSPX_RECORD] {
        let mut e = [0; FSPX_RECORD];
        e[0] = 0x41;
        e[5] = 3;
        e[8..12].copy_from_slice(&offset.to_le_bytes());
        e[12..16].copy_from_slice(&register.to_le_bytes());
        e[20..23].copy_from_slice(&[dwords, 8, 0]);
        e
    }

    /// Linked metadata for `code` with `records` and the hash record.
    pub(crate) fn linked_meta(code: &[u8], records: &[[u8; FSPX_RECORD]], hash: [u32; 4]) -> Vec<u8> {
        let size = LINKED_RECORDS + FSPX_RECORD * (records.len() + 1);
        let mut meta = words(&[
            LINKED_TAG,
            size as u32 - 0xc,
            (code.len() / 4 - LINKED_CODE_EXTRA) as u32,
            LINKED_MARK | (records.len() as u32 + 1) << 15,
        ]);
        for r in records {
            meta.extend_from_slice(r);
        }
        meta.extend(words(&[0x43, hash[0], hash[1], hash[2], hash[3], 0, 0, 0]));
        meta
    }

    /// Compute metadata for `code` (every dword of it counted) with
    /// `records` and the hash record, as device method 0x218 gets it.
    pub(crate) fn compute_meta(code: &[u8], records: &[[u8; FSPX_RECORD]], hash: [u32; 4]) -> Vec<u8> {
        let size = FSPX_RECORDS + FSPX_RECORD * (records.len() + 1);
        let mut meta = words(&[
            FSPX_META_TAG,
            (size - FSPX_META_UNCOUNTED) as u32,
            (code.len() / 4) as u32,
            0,
            0,
            LINKED_MARK | (records.len() as u32 + 1) << 15,
        ]);
        for r in records {
            meta.extend_from_slice(r);
        }
        meta.extend(words(&[0x43, hash[0], hash[1], hash[2], hash[3], 0, 0, 0]));
        meta
    }

    /// A compute pipeline's register packet: the thread-group size, the wave
    /// size bit, `COMPUTE_PGM_RSRC1` and `COMPUTE_PGM_RSRC2`.
    pub(crate) fn compute_packet(threads: [u16; 3], wave32: bool, rsrc1: u32, rsrc2: u32) -> Vec<u8> {
        let [x, y, z] = threads.map(u32::from);
        words(&[0, 0, 0, x | y << 16, z | u32::from(wave32) << 20, rsrc1, rsrc2])
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{HASH, compute_meta, compute_packet, linked_code, linked_meta, table_record, words};
    use super::*;

    #[test]
    fn deserialized_compute_blocks_make_a_container_of_their_program() {
        // s_mov_b32 s9, 0 before the end; 64 threads of one wave of 32.
        let code = linked_code(HASH, &[0xBE89_0380]);
        let meta = compute_meta(&code, &[table_record(13, 0, 4)], HASH);
        // RSRC1: 2 granules of VGPRs; RSRC2: 2 user SGPRs, group id x, 1 id VGPR.
        let packet = compute_packet([64, 1, 1], true, 0x01, 0x84);
        let pipeline_meta = words(&[0x1234_5678, 3, 0, 0, 0]);
        let container = deserialized_compute_container(&packet, &pipeline_meta, &code, &meta).unwrap();
        let c = crate::Container::parse(&container).unwrap();
        let [p] = &native_programs(c.part(crate::FourCC::ACCC).unwrap().data).unwrap()[..] else {
            panic!("one program");
        };
        assert_eq!((p.stage, p.wave64, p.vgprs), (HwStage::Cs, false, 16));
        assert_eq!(p.code, code);
        assert_eq!(p.io, StageIo::Cs(ComputeIo { rsrc2: 0x84, threads: [64, 1, 1] }));
        let used: Vec<_> = p.used.iter().map(|u| (u.lower_bound, u.table_offset_dwords, u.dwords)).collect();
        assert_eq!(used, [(13, 0, 4)]);
        // Without the wave size bit: waves of 64, granules of 4 VGPRs.
        let wave64 = compute_packet([8, 8, 1], false, 0x01, 0x84);
        let container = deserialized_compute_container(&wave64, &pipeline_meta, &code, &meta).unwrap();
        let c = crate::Container::parse(&container).unwrap();
        let p = fspx_program(c.part(crate::FourCC::ACCC).unwrap().data).unwrap();
        assert_eq!((p.wave64, p.vgprs), (true, 8));
        assert_eq!(p.io, StageIo::Cs(ComputeIo { rsrc2: 0x84, threads: [8, 8, 1] }));

        let refused = |packet: &[u8], code: &[u8], meta: &[u8]| {
            deserialized_compute_container(packet, &pipeline_meta, code, meta).is_err()
        };
        assert!(refused(&packet[..0x18], &code, &meta), "packet size");
        let other = linked_code([HASH[0] ^ 0x0100_0000, HASH[1], HASH[2], HASH[3]], &[0xBE89_0380]);
        assert!(refused(&packet, &other, &meta), "another program's code");
        assert!(refused(&packet, &code[..code.len() - 4], &meta), "code size");
        assert!(refused(&packet, &code, &meta[..meta.len() - FSPX_RECORD]), "metadata size");
        for (at, what) in [(0, "tag"), (4, "size"), (8, "code dwords"), (0x14, "mark"), (0x16, "count")] {
            let mut bad = meta.clone();
            bad[at] ^= 1;
            assert!(refused(&packet, &code, &bad), "{what}");
        }
        let mut unhashed = meta.clone();
        let last = unhashed.len() - FSPX_RECORD;
        unhashed[last] = 0x42;
        assert!(refused(&packet, &code, &unhashed), "no hash");
        // Graphics metadata is not compute metadata.
        let linked = linked_meta(&code, &[table_record(13, 0, 4)], HASH);
        assert!(refused(&packet, &code, &linked), "linked metadata");
    }

    /// Every compute description dumped (`.agente/pso-dump`) makes a
    /// container whose program has the packet's group size and wave size.
    #[test]
    fn dumped_compute_descriptions_make_containers() {
        for d in dumped::descriptions_of("218") {
            let Some(container) = dumped::compute(&d) else {
                continue;
            };
            let container = container.unwrap_or_else(|e| panic!("{d}: {e}"));
            let packet = dumped::block_of("218", &d, 1).unwrap();
            let c = crate::Container::parse(&container).unwrap();
            let p = fspx_program(c.part(crate::FourCC::ACCC).unwrap().data).unwrap();
            let StageIo::Cs(cs) = &p.io else { panic!("{d}: not compute") };
            let threads: Vec<u32> = (0..3).map(|i| u32::from(u16_at(&packet, 0xc + 2 * i).unwrap())).collect();
            assert_eq!(cs.threads[..], threads[..], "{d}");
            assert_eq!(p.wave64, u32_at(&packet, 0x10).unwrap() & 0x10_0000 == 0, "{d}");
            assert!(cs.threads.iter().product::<u32>() > 0, "{d}");
        }
    }

    #[test]
    fn linked_programs_are_checked_against_their_metadata() {
        // s_mov_b32 s9, 0 before the end.
        let code = linked_code(HASH, &[0xBE89_0380]);
        let records = [table_record(13, 0, 4), table_record(0, 0x10, 8)];
        let meta = linked_meta(&code, &records, HASH);
        let p = linked_program(HwStage::Ps, &code, &meta, 3, true).unwrap();
        assert_eq!((p.stage, p.wave64, p.vgprs), (HwStage::Ps, true, 16));
        assert_eq!(p.io, StageIo::Linked(LinkedIo::default()));
        assert_eq!(p.code, code);
        assert_eq!(
            p.layout,
            [RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 13,
                user_data: 8,
                slot: 0
            }]
        );
        let used: Vec<_> = p.used.iter().map(|u| (u.lower_bound, u.table_offset_dwords, u.dwords)).collect();
        assert_eq!(used, [(13, 0, 4), (0, 0x10, 8)]);
        // Granules of 8 VGPRs in 32-lane waves.
        assert_eq!(linked_program(HwStage::Vs, &code, &meta, 5, false).unwrap().vgprs, 48);

        let refused = |code: &[u8], meta: &[u8]| linked_program(HwStage::Vs, code, meta, 5, false).is_err();
        assert!(linked_program(HwStage::Cs, &code, &meta, 5, false).is_err(), "stage");
        let other = linked_code([HASH[0] ^ 0x0100_0000, HASH[1], HASH[2], HASH[3]], &[0xBE89_0380]);
        assert!(refused(&other, &meta), "another program's code");
        assert!(refused(&code[..code.len() - 4], &meta), "code size");
        assert!(refused(&code, &meta[..meta.len() - FSPX_RECORD]), "metadata size");
        for (at, what) in [(0, "tag"), (4, "size"), (8, "code dwords"), (0xc, "mark"), (0xe, "count")] {
            let mut bad = meta.clone();
            bad[at] ^= 1;
            assert!(refused(&code, &bad), "{what}");
        }
        // The hash record must be the last one.
        let mut unhashed = meta.clone();
        let last = unhashed.len() - FSPX_RECORD;
        unhashed[last] = 0x42;
        assert!(refused(&code, &unhashed), "no hash");
    }

    #[test]
    fn linked_containers_give_their_program_back() {
        let code = linked_code(HASH, &[0xBE89_0380]);
        let meta = linked_meta(&code, &[table_record(13, 0, 4)], HASH);
        let io = LinkedIo {
            inputs: vec![InputElement {
                name: "COLOR".into(),
                index: 1,
                format: 30,
                slot: 2,
                offset: 8,
                per_instance: true,
                step_rate: 1,
            }],
            input_ena: 2,
            col_format: 4,
            input_cntl: vec![0, 0x401],
            rsrc2: 0x14,
        };
        // A sampler the metadata declares and no record places comes from
        // the sampler heap, whose base is the last of the 12 user SGPRs.
        let mut constant = table_record(500, 0, 4);
        constant[4..6].copy_from_slice(&[2, 0]);
        constant[21..23].copy_from_slice(&[9, 1]);
        let mut sampler = [0u8; FSPX_RECORD];
        sampler[0] = 0x42;
        sampler[8..11].copy_from_slice(&[2, 3, 14]);
        let picked = linked_meta(&code, &[table_record(2, 0, 8), constant, sampler], HASH);
        let heap = LinkedIo {
            rsrc2: 0x18,
            ..io.clone()
        };
        let container = linked_container(HwStage::Ps, &code, &picked, 3, true, &heap).unwrap();
        let c = crate::Container::parse(&container).unwrap();
        let p = &native_programs(c.part(crate::FourCC::ACCC).unwrap().data).unwrap()[0];
        let layout: Vec<_> = p.layout.iter().map(|p| (p.root, p.user_data, p.sgprs)).collect();
        // (the constants run up to it)
        assert_eq!(
            layout,
            [(RootType::Table, 8, 1), (RootType::Constants, 9, 2), (RootType::SamplerHeap, 11, 1)]
        );
        // With ten user SGPRs the constant would lie where the heap's base is.
        assert!(linked_container(HwStage::Ps, &code, &picked, 3, true, &io).is_err());
        // Nor is a container made of blocks that are not one program.
        assert!(linked_container(HwStage::Ps, &code[4..], &meta, 3, true, &io).is_err());

        for (stage, wave64) in [(HwStage::Vs, false), (HwStage::Ps, true)] {
            let container = linked_container(stage, &code, &meta, 3, wave64, &io).unwrap();
            let c = crate::Container::parse(&container).unwrap();
            let part = c.part(crate::FourCC::ACCC).unwrap();
            let mut expected = linked_program(stage, &code, &meta, 3, wave64).unwrap();
            expected.io = StageIo::Linked(io.clone());
            assert_eq!(native_programs(part.data).unwrap(), [expected]);
            // What the part holds is checked as the blocks were.
            let mut other = part.data.to_vec();
            let hash = other.len() - FSPX_RECORD + 4;
            other[hash] ^= 1;
            assert!(native_programs(&other).is_err(), "hash");
            assert!(native_programs(&part.data[..part.data.len() - 4]).is_err(), "size");
        }
    }

    /// A register image of a vertex plus pixel pipeline with `cntl` as its
    /// pixel input controls.
    fn register_image(cntl: &[u32]) -> Vec<u8> {
        let size = 4 * (REGS_LAST_HEAD + cntl.len().next_multiple_of(2) + REGS_LAST_TAIL);
        let mut image = vec![0u8; REGS_LAST + size];
        let mut put = |at: usize, v: u32| image[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(REGS_SECTION, ((REGS_LAST - REGS_SECTION_BASE) | size << 16) as u32);
        put(REGS_VS_RSRC1, 0x082C_0345);
        put(REGS_PS_RSRC1, 0x022C_C343);
        put(REGS_PS_RSRC1 + 4, 0x14);
        put(REGS_PS_INPUT_ENA, 2);
        put(REGS_PS_IN_CONTROL, cntl.len() as u32);
        put(REGS_COL_FORMAT, 4);
        put(REGS_DEPTH_CONTROL, 0x0070_07F0);
        put(REGS_BLEND + 4, 0xE504_0504);
        for (k, &c) in cntl.iter().enumerate() {
            put(REGS_LAST + 4 * (REGS_LAST_HEAD + k), c);
        }
        image
    }

    #[test]
    fn register_images_give_the_stage_registers_and_input_controls() {
        for cntl in [&[][..], &[0, 1], &[0, 1, 0x402]] {
            let image = register_image(cntl);
            let r = GraphicsRegs::parse(&image).unwrap();
            assert_eq!(r.ps_input_cntl, cntl);
            assert_eq!((r.vs_rsrc1, r.ps_rsrc1, r.ps_rsrc2), (0x082C_0345, 0x022C_C343, 0x14));
            assert_eq!((r.ps_input_ena, r.col_format, r.db_depth_control), (2, 4, 0x0070_07F0));
            assert_eq!(r.blend[..2], [0, 0xE504_0504]);
            assert!(r.ps_wave64());
            assert_eq!(r.num_interp() as usize, cntl.len());
        }
        // 0x17c bytes without interpolants, 0x184 with two, 0x18c with three.
        let sizes = [0, 2, 3].map(|n| register_image(&vec![0; n]).len());
        assert_eq!(sizes, [0x17c, 0x184, 0x18c]);

        let image = register_image(&[0, 1]);
        let mut wide = image.clone();
        wide[REGS_PS_IN_CONTROL + 1] = 0x80;
        assert!(!GraphicsRegs::parse(&wide).unwrap().ps_wave64());
        let mut more = image.clone();
        more[REGS_PS_IN_CONTROL] = 5;
        assert!(GraphicsRegs::parse(&more).is_err(), "interpolant count");
        let mut elsewhere = image.clone();
        elsewhere[REGS_SECTION] += 4;
        assert!(GraphicsRegs::parse(&elsewhere).is_err(), "another layout");
        assert!(GraphicsRegs::parse(&image[..image.len() - 4]).is_err(), "size");
        assert!(GraphicsRegs::parse(&[0; 16]).is_err());
    }

    /// A formats and input layout block with `elements` as (name, semantic
    /// index, format, byte offset), one render target and triangles.
    fn layout_block(elements: &[(&str, u32, u32, u32)]) -> Vec<u8> {
        let mut block = vec![0u8; LAYOUT_ELEMENTS];
        block[LAYOUT_TARGET_FORMATS] = 28;
        block[LAYOUT_SAMPLE_MASK..][..4].copy_from_slice(&[0xFF; 4]);
        block[LAYOUT_DEPTH_FORMAT..][..3].copy_from_slice(&[55, 1, 3]);
        block[LAYOUT_ELEMENT_COUNT] = elements.len() as u8;
        for &(name, index, format, offset) in elements {
            block.extend((name.len() as u32 + 1).to_le_bytes());
            block.extend(name.as_bytes());
            block.push(0);
            block.extend(words(&[index, format, 0, offset, 0, 0]));
        }
        let size = (block.len() - LAYOUT_ELEMENT_COUNT) as u32;
        block[LAYOUT_ELEMENTS_SIZE..][..4].copy_from_slice(&size.to_le_bytes());
        block.resize(block.len().next_multiple_of(4), 0);
        block
    }

    #[test]
    fn layout_blocks_give_formats_and_unaligned_elements() {
        let block = layout_block(&[("POSITION", 0, 16, 0), ("COLOR", 1, 30, 8)]);
        // 37 and 34 bytes of elements: one byte of padding.
        assert_eq!(block.len(), LAYOUT_ELEMENTS + 72);
        let l = InputLayout::parse(&block).unwrap();
        assert_eq!((l.target_formats[0], l.depth_format, l.targets, l.topology), (28, 55, 1, 3));
        assert_eq!(l.sample_mask, u32::MAX);
        let element = |name: &str, index, format, offset| InputElement {
            name: name.into(),
            index,
            format,
            slot: 0,
            offset,
            per_instance: false,
            step_rate: 0,
        };
        assert_eq!(l.elements, [element("POSITION", 0, 16, 0), element("COLOR", 1, 30, 8)]);
        assert!(InputLayout::parse(&layout_block(&[])).unwrap().elements.is_empty());

        assert!(InputLayout::parse(&block[..block.len() - 4]).is_err(), "size");
        let mut count = block.clone();
        count[LAYOUT_ELEMENT_COUNT] = 3;
        assert!(InputLayout::parse(&count).is_err(), "count");
        let mut name = block.clone();
        name[LAYOUT_ELEMENTS] = 4;
        assert!(InputLayout::parse(&name).is_err(), "name length");
        assert!(InputLayout::parse(&[0; 8]).is_err());
    }

    /// Every description a run dumped (`.agente/pso-dump`): its blocks are
    /// what the parsers say they are, or it is of the one shape they refuse.
    #[test]
    fn dumped_descriptions_parse() {
        for d in dumped::descriptions() {
            let (Some(image), Some(block)) = (dumped::block(&d, 1), dumped::block(&d, 3)) else {
                continue;
            };
            let layout = InputLayout::parse(&block).unwrap_or_else(|e| panic!("{d}: layout: {e}"));
            // One format byte per render target (one description has five).
            let set = layout.target_formats.iter().take_while(|&&f| f != 0).count();
            assert_eq!(set, usize::from(layout.targets), "{d}: {:?}", layout.target_formats);
            assert!(layout.target_formats[set..].iter().all(|&f| f == 0), "{d}");
            let vs = dumped::block(&d, 9).zip(dumped::block(&d, 11));
            let Some((vs_code, vs_meta)) = vs else {
                // No vertex program: another geometry stage's registers come
                // first in the image.
                assert!(GraphicsRegs::parse(&image).is_err(), "{d}");
                continue;
            };
            let regs = GraphicsRegs::parse(&image).unwrap_or_else(|e| panic!("{d}: registers: {e}"));
            let vs = linked_program(HwStage::Vs, &vs_code, &vs_meta, regs.vs_rsrc1, false)
                .unwrap_or_else(|e| panic!("{d}: vertex program: {e}"));
            assert!(matches!(vs.io, StageIo::Linked(_)));
            match dumped::block(&d, 5).zip(dumped::block(&d, 7)) {
                Some((code, meta)) => {
                    let ps = linked_program(HwStage::Ps, &code, &meta, regs.ps_rsrc1, regs.ps_wave64())
                        .unwrap_or_else(|e| panic!("{d}: pixel program: {e}"));
                    assert_eq!(ps.stage, HwStage::Ps);
                    // The blocks are not interchangeable: the hash tells.
                    assert!(linked_program(HwStage::Ps, &code, &vs_meta, regs.ps_rsrc1, true).is_err());
                    // Each interpolant reads the parameter of its own
                    // number; some are flat (bit 10), nothing else is set.
                    let own = regs.ps_input_cntl.iter().enumerate().all(|(k, &c)| c & !0x400 == k as u32);
                    assert!(own, "{d}: {:x?}", regs.ps_input_cntl);
                }
                None => {
                    assert_eq!((regs.ps_rsrc1, regs.col_format, regs.num_interp()), (0, 0, 0), "{d}");
                    assert_eq!(layout.targets, 0, "{d}");
                }
            }
        }
    }

    /// The first description's layout: the three elements its vertex
    /// program loads.
    #[test]
    fn dumped_layout_has_the_fetched_elements() {
        let Some(block) = dumped::block("00001", 3) else {
            return;
        };
        let l = InputLayout::parse(&block).unwrap();
        let elements: Vec<_> = l.elements.iter().map(|e| (e.name.as_str(), e.format, e.slot, e.offset)).collect();
        assert_eq!(elements, [("POSITION", 6, 0, 0), ("COLOR", 28, 0, 12), ("TEXCOORD", 16, 0, 16)]);
        assert_eq!((l.targets, l.target_formats[0], l.topology), (1, 28, 3));
    }

    const VS_CODE: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    const PS_CODE: [u8; 4] = [9, 10, 11, 12];
    const VGPRS: u32 = 28;

    fn slot(stage: HwStage) -> usize {
        SLOTS.iter().find(|s| s.0 == stage).unwrap().1
    }

    /// Stage data: no semantics, and every export patch absent.
    const STAGE_DATA: usize = PS_PATCHES + MRTS * PATCH_BYTES;

    fn empty_stage_data() -> Vec<u8> {
        let mut data = vec![0; STAGE_DATA];
        for mrt in 0..MRTS {
            let at = PS_PATCHES + mrt * PATCH_BYTES;
            data[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        data
    }

    /// Where a one-program blob's stage data starts.
    fn stage_data_at(blob: &[u8], stage: HwStage) -> usize {
        FSSM_HEADER
            + u64_at(blob, FSSM_HEADER + slot(stage)).unwrap() as usize
            + RECORD_SIZE as usize
    }

    /// FSSM blob with `programs` as (stage, code) pairs.
    fn fssm(programs: &[(HwStage, &[u8])]) -> Vec<u8> {
        let mut payload = GRAPHICS_SLOT_COUNT.to_le_bytes().to_vec();
        payload.resize(slot(HwStage::Cs) + 8, 0);
        for (_, at) in SLOTS {
            payload[at..at + 8].copy_from_slice(&ABSENT.to_le_bytes());
        }
        for &(stage, code) in programs {
            let slot = slot(stage);
            let record = payload.len() as u64;
            payload[slot..slot + 8].copy_from_slice(&record.to_le_bytes());
            let code_at = record + RECORD_SIZE + STAGE_DATA as u64;
            payload.extend_from_slice(&code_at.to_le_bytes());
            payload.extend_from_slice(&(code.len() as u64).to_le_bytes());
            let flags = if stage == HwStage::Ps { FLAG_WAVE32 } else { 0 };
            payload.extend_from_slice(&[0; 4]);
            payload.extend_from_slice(&flags.to_le_bytes());
            payload.resize(record as usize + RECORD_REGS, 0);
            payload.extend_from_slice(&(VGPRS | 31 << 20).to_le_bytes());
            payload.extend_from_slice(&empty_stage_data());
            payload.extend_from_slice(code);
        }
        let mut out = b"FSSM".to_vec();
        out.extend_from_slice(&FSSM_VERSION.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    fn xbch(unpacked: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; zlib_rs::compress_bound(unpacked.len())];
        let (stream, rc) =
            zlib_rs::compress_slice(&mut buf, unpacked, zlib_rs::DeflateConfig::new(6));
        assert_eq!(rc, zlib_rs::ReturnCode::Ok);
        let mut out = b"XBCH".to_vec();
        out.extend_from_slice(&XBCH_VERSION.to_le_bytes());
        out.extend_from_slice(&(stream.len() as u64).to_le_bytes());
        out.extend_from_slice(&(unpacked.len() as u64).to_le_bytes());
        out.extend_from_slice(stream);
        out
    }

    /// `DXIL` part data: program header, XDAT index, then the blobs.
    fn part(blobs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut out = vec![0u8; XDAT_OFFSET];
        out.extend_from_slice(b"XDAT");
        out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x24u32.to_le_bytes());
        let mut offset = out.len() + 12 * blobs.len();
        for (kind, blob) in blobs {
            for v in [*kind, offset as u32, blob.len() as u32] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            offset += blob.len();
        }
        for (_, blob) in blobs {
            out.extend_from_slice(blob);
        }
        out
    }

    #[test]
    fn finds_graphics_programs() {
        let native = xbch(&fssm(&[(HwStage::Vs, &VS_CODE), (HwStage::Ps, &PS_CODE)]));
        let data = part(&[(0x0a, b"xbop"), (KIND_NATIVE, &native)]);
        let found = native_programs(&data).unwrap();
        assert_eq!(
            found,
            [
                NativeProgram {
                    stage: HwStage::Vs,
                    wave64: true,
                    vgprs: 28,
                    layout: vec![],
                    used: vec![],
                    io: StageIo::Vs {
                        inputs: vec![],
                        outputs: vec![],
                    },
                    code: VS_CODE.to_vec()
                },
                NativeProgram {
                    stage: HwStage::Ps,
                    wave64: false,
                    vgprs: 28,
                    layout: vec![],
                    used: vec![],
                    io: StageIo::Ps {
                        inputs: vec![],
                        input_addr: 0,
                        db_shader_control: 0,
                        patches: vec![],
                    },
                    code: PS_CODE.to_vec()
                },
            ]
        );
    }

    #[test]
    fn reads_root_tables() {
        let mut blob = fssm(&[(HwStage::Ps, &PS_CODE)]);
        let record = FSSM_HEADER + u64_at(&blob, FSSM_HEADER + slot(HwStage::Ps)).unwrap() as usize;
        let tables = (blob.len() - FSSM_HEADER) as u64;
        // A table range in s9 (root slot 1), and an SRV read from it.
        for v in [3u32, 2, 1, 5] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        blob.extend_from_slice(&[9, 1, 0, 0, 0, 3, 0, 0]);
        for v in [2u32, 5, 4] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        blob.extend_from_slice(&[8, 9, 1, 0]);
        let payload = (blob.len() - FSSM_HEADER) as u64;
        blob[8..16].copy_from_slice(&payload.to_le_bytes());
        blob[record + RECORD_LAYOUT..][..8].copy_from_slice(&tables.to_le_bytes());
        blob[record + RECORD_USED..][..8].copy_from_slice(&(tables + ENTRY_SIZE).to_le_bytes());
        blob[record + RECORD_COUNTS..][..8].copy_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);

        let p = &native_programs(&part(&[(KIND_NATIVE, &xbch(&blob))])).unwrap()[0];
        assert_eq!(
            p.layout,
            [RootParam {
                root: RootType::Table,
                table_offset_dwords: 2,
                sgprs: 1,
                base_register: 5,
                user_data: 9,
                slot: 1
            }]
        );
        assert_eq!(
            p.used,
            [UsedResource {
                kind: ResourceType::Srv,
                root: RootType::Table,
                table_offset_dwords: 2,
                lower_bound: 5,
                stride: 4,
                dwords: 8,
                user_data: 9,
                slot: 1
            }]
        );

        blob[record + RECORD_COUNTS] = 9;
        assert!(native_programs(&part(&[(KIND_NATIVE, &xbch(&blob))])).is_err());
    }

    #[test]
    fn reads_pixel_io_and_applies_the_full_export_patch() {
        let code = [0xBF80_0000u32; 4]; // s_nop
        let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut blob = fssm(&[(HwStage::Ps, &bytes)]);
        let e = stage_data_at(&blob, HwStage::Ps);
        // TEXCOORD in row 1 (xy), read as attr0, flat.
        blob[e + PS_INPUTS..][..8].copy_from_slice(&[6, 1, 3, 0, 0, 0, 1, 1]);
        blob[e + PS_INPUT_COUNT..][..4].copy_from_slice(&1u32.to_le_bytes());
        blob[e + PS_INPUT_ADDR..][..4].copy_from_slice(&2u32.to_le_bytes());
        blob[e + PS_PATCH_SIZE..][..4].copy_from_slice(&2u32.to_le_bytes());
        // MRT1 at dword 1: the 32_ABGR variant is `exp mrt1 v0-v3 done`.
        let patch = e + PS_PATCHES + PATCH_BYTES;
        blob[patch..][..4].copy_from_slice(&1u32.to_le_bytes());
        let full = patch + 4 + PATCH_FULL * PATCH_SLOT_DWORDS * 4;
        blob[full..][..8].copy_from_slice(&[0x1f, 0x18, 0, 0xf8, 0, 1, 2, 3]);

        let p = &native_programs(&part(&[(KIND_NATIVE, &xbch(&blob))])).unwrap()[0];
        let StageIo::Ps {
            inputs,
            input_addr,
            patches,
            ..
        } = &p.io
        else {
            panic!("{:?}", p.io)
        };
        let input = PsInput {
            usage: Usage::Generic,
            row: 1,
            mask: 3,
            attr: 0,
            flat: true,
            fp16: false,
            pack2: false,
            row2: 0,
        };
        assert_eq!((&inputs[..], *input_addr), (&[input][..], 2));
        assert_eq!(patches.len(), 1);
        assert_eq!((patches[0].mrt, patches[0].offset), (1, 1));
        let patched = p.patched_code();
        assert_eq!(patched[..4], p.code[..4]);
        assert_eq!(patched[4..12], [0x1f, 0x18, 0, 0xf8, 0, 1, 2, 3]);
        assert_eq!(patched[12..], p.code[12..]);

        blob[patch..][..4].copy_from_slice(&3u32.to_le_bytes());
        assert!(native_programs(&part(&[(KIND_NATIVE, &xbch(&blob))])).is_err());
    }

    #[test]
    fn finds_mesh_and_compute_programs() {
        for stage in [HwStage::Ms, HwStage::Cs] {
            let data = part(&[(KIND_NATIVE, &xbch(&fssm(&[(stage, &VS_CODE)])))]);
            let found = native_programs(&data).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].stage, stage);
        }
    }

    #[test]
    fn reads_mesh_outputs_and_limits() {
        let mut blob = fssm(&[(HwStage::Ms, &VS_CODE)]);
        let e = stage_data_at(&blob, HwStage::Ms);
        // param0 is row 1 per vertex, param2 row 0 of the primitive signature.
        blob[e + VS_OUTPUTS..][..12].copy_from_slice(&[6, 1, 0, 0, 0, 0, 6, 0, 0x82, 0, 0, 0]);
        blob[e + VS_OUTPUT_COUNT..][..4].copy_from_slice(&2u32.to_le_bytes());
        blob[e + MS_CLIP_CNTL..][..4].copy_from_slice(&0x0040_000Fu32.to_le_bytes());
        // 128 lanes, 128 vertices, 64 primitives, 13 user SGPRs, triangles.
        blob[e + MS_INFO..][..8].copy_from_slice(&[0x80, 0, 0x80, 0, 0x40, 0, 13, 2]);
        let record = e - RECORD_SIZE as usize;
        blob[record + RECORD_LDS..][..4].copy_from_slice(&16u32.to_le_bytes());
        let p = &native_programs(&part(&[(KIND_NATIVE, &xbch(&blob))])).unwrap()[0];
        let out = |row, param| VsOutput {
            usage: Usage::Generic,
            row,
            param,
            fp16: false,
            pack2: false,
        };
        assert_eq!(
            p.io,
            StageIo::Ms(MeshIo {
                outputs: vec![out(1, 0), out(0, 0x82)],
                clip_cntl: 0x0040_000F,
                threads: 128,
                max_vertices: 128,
                max_primitives: 64,
                topology: 2,
                lds_bytes: 16,
            })
        );
    }

    #[test]
    fn mesh_stage_io_ignores_unused_vs_input_count() {
        let mut blob = fssm(&[(HwStage::Ms, &VS_CODE)]);
        let e = stage_data_at(&blob, HwStage::Ms);
        // Mesh stage data has no vertex inputs; E+0xc4 is unused by MS.
        blob[e + VS_INPUT_COUNT..][..4].copy_from_slice(&u32::MAX.to_le_bytes());
        let p = &native_programs(&part(&[(KIND_NATIVE, &xbch(&blob))])).unwrap()[0];
        assert!(matches!(p.io, StageIo::Ms(_)));
    }

    #[test]
    fn compute_dispatch_settings() {
        // 5 user SGPRs, group id x and y, thread ids in v0-v1, 1 KiB LDS.
        let cs = ComputeIo {
            rsrc2: 0x1098A,
            threads: [16, 16, 1],
        };
        assert_eq!(cs.user_sgprs(), 5);
        assert_eq!(cs.system_sgprs(), ([Some(5), Some(6), None], None));
        assert_eq!((cs.thread_id_vgprs(), cs.lds_bytes()), (2, 1024));
        // 6 user SGPRs, all three group ids, v0-v2, no LDS.
        let cs = ComputeIo {
            rsrc2: 0x138C,
            threads: [8, 8, 1],
        };
        assert_eq!(cs.system_sgprs(), ([Some(6), Some(7), Some(8)], None));
        assert_eq!((cs.thread_id_vgprs(), cs.lds_bytes()), (3, 0));
    }

    #[test]
    fn initialized_sgprs_covers_user_system_and_scratch() {
        // 5 user SGPRs, group ids x and y (s5, s6), no scratch: s0..=s6.
        let cs = ComputeIo {
            rsrc2: 0x1098A,
            threads: [16, 16, 1],
        };
        assert_eq!(cs.initialized_sgprs(), 7);
        // All three group ids after 6 user SGPRs, plus the info SGPR: s0..=s9.
        let cs = ComputeIo {
            rsrc2: 0x78C,
            threads: [8, 8, 1],
        };
        assert_eq!(cs.initialized_sgprs(), 10);
        // The same with scratch enabled adds the wave offset: s0..=s10.
        let cs = ComputeIo {
            rsrc2: 0x78D,
            threads: [8, 8, 1],
        };
        assert_eq!(cs.initialized_sgprs(), 11);
    }

    #[test]
    fn reads_every_native_blob() {
        let es = xbch(&fssm(&[(HwStage::Es, &VS_CODE)]));
        let vs = xbch(&fssm(&[(HwStage::Vs, &VS_CODE)]));
        let data = part(&[(0x12, &[0; 16]), (KIND_NATIVE, &es), (KIND_NATIVE, &vs)]);
        let stages: Vec<_> = native_programs(&data)
            .unwrap()
            .iter()
            .map(|p| p.stage)
            .collect();
        assert_eq!(stages, [HwStage::Es, HwStage::Vs]);
    }

    #[test]
    fn pc_layout_has_none() {
        assert!(native_programs(&[0u8; 64]).unwrap().is_empty());
        assert!(
            native_programs(&part(&[(0x0a, b"xbop")]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_corrupt_blobs() {
        let good = xbch(&fssm(&[(HwStage::Ls, &VS_CODE)]));
        let corrupt = |at: usize, byte: u8| {
            let mut blob = good.clone();
            blob[at] = byte;
            native_programs(&part(&[(KIND_NATIVE, &blob)]))
        };
        assert!(corrupt(4, 9).is_err(), "unknown version");
        assert!(corrupt(16, 0xFF).is_err(), "stated size");
        assert!(corrupt(XBCH_HEADER + 2, 0xFF).is_err(), "zlib stream");
        assert!(native_programs(&part(&[(KIND_NATIVE, &good[..20])])).is_err());

        let mut bad_record = fssm(&[(HwStage::Ls, &VS_CODE)]);
        let at = FSSM_HEADER + slot(HwStage::Ls);
        bad_record[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(native_programs(&part(&[(KIND_NATIVE, &xbch(&bad_record))])).is_err());
    }

    #[test]
    fn rejects_truncated_oversized_and_bad_checksum_xbch_streams() {
        let plain = fssm(&[(HwStage::Vs, &VS_CODE)]);
        let good = xbch(&plain);
        assert_eq!(unpack(&good).unwrap(), plain);

        // Truncated zlib stream (drop last byte, update packed length).
        let mut truncated = good[..good.len() - 1].to_vec();
        let new_packed = (truncated.len() - XBCH_HEADER) as u64;
        truncated[8..16].copy_from_slice(&new_packed.to_le_bytes());
        assert!(unpack(&truncated).is_err());

        // Stated unpacked size smaller than the inflated output.
        let mut oversized = good.clone();
        oversized[16..24].copy_from_slice(&((plain.len() - 1) as u64).to_le_bytes());
        assert!(unpack(&oversized).is_err());

        // Stated unpacked size larger than the inflated output.
        let mut underfilled = good.clone();
        underfilled[16..24].copy_from_slice(&((plain.len() + 1) as u64).to_le_bytes());
        assert!(unpack(&underfilled).is_err());

        // Corrupt Adler-32 trailer.
        let mut bad_adler = good.clone();
        *bad_adler.last_mut().unwrap() ^= 1;
        assert!(unpack(&bad_adler).is_err());
    }
}
