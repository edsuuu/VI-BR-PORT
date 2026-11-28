//! Device methods 0x1c8 and 0x218: pipelines the title has ready, made by
//! the console's compiler ahead of time and stored with their state, that
//! it hands over at boot to be deserialized (graphics, then compute). Each
//! description is a block of 64-bit words: the root signature, the pipeline's
//! register packet and its metadata as (address, bytes) pairs, then per
//! stage the code and the code's metadata, likewise. Graphics stages come in
//! the order pixel, vertex, geometry, export, hull, local, then one more;
//! compute has its one stage.
//!
//! The title calls both in a loop over its list of ready pipelines (title+
//! 0x2477578 and title+0x24800d5): on failure it skips that entry and goes
//! on with the next, so the pipeline is not there when a draw asks for it
//! (read off its code; what it does then is not, see the report).
//!
//! `VICTORY_PSO_DESERIALIZE` turns them on (off by default: the two methods
//! answer `E_NOTIMPL`, as they always did, [`super::unknown_pipeline`]):
//! `1` both, `graphics` or `compute` one of them. With `=check` added
//! (`VICTORY_PSO_DESERIALIZE_CHECK=1`) a pipeline whose programs do not
//! translate is refused at once, so the title does without it as before,
//! at the cost of translating every one of them at boot.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::atomic::{AtomicU64, Ordering};

use crate::object::{E_NOTIMPL, HRESULT, Iid, This};
use crate::pipeline;

/// The words of a description that are read: root signature, packet,
/// metadata, then four words for each of the seven graphics stages.
const WORDS: usize = 5 + 4 * 7;
/// Word of the first stage: the pixel program's code (graphics) or the
/// compute program's (compute).
const STAGES: usize = 5;
/// Where the vertex and the geometry program's words start.
const VERTEX: usize = STAGES + 4;
const GEOMETRY: usize = STAGES + 8;
/// The most bytes a block of such a description may have, to be one: the
/// register image and the formats and input layout (0x200 and 0xbc seen),
/// a program's code (0x4a40 seen in graphics, 0x1f9c0 in compute) and its
/// metadata (0x5b0 and 0x1798 seen).
const STATE_CAP: u64 = 0x1000;
const CODE_CAP: u64 = 0x10_0000;
const META_CAP: u64 = 0x1_0000;
/// The bytes a program's code starts with that hold its hash: two `v_nop`s
/// of three dwords (`victory_shader::native::linked_program`).
const HASH_BYTES: usize = 24;

/// Calls, pipelines handed out and descriptions refused, per method
/// (graphics, compute), for [`note`].
static COUNTS: [[AtomicU64; 3]; 2] = [const { [const { AtomicU64::new(0) }; 3] }; 2];

/// Which of the two methods `VICTORY_PSO_DESERIALIZE` turns on: graphics,
/// compute. Both always in the tests here.
fn on() -> [bool; 2] {
    #[cfg(test)]
    return [true; 2];
    #[cfg(not(test))]
    {
        static ON: std::sync::OnceLock<[bool; 2]> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            let v = std::env::var("VICTORY_PSO_DESERIALIZE").unwrap_or_default();
            let on = [matches!(v.as_str(), "1" | "graphics"), matches!(v.as_str(), "1" | "compute")];
            if on != [false; 2] {
                tracing::info!(
                    graphics = on[0],
                    compute = on[1],
                    check = check(),
                    "pipelines the title has ready are deserialized (VICTORY_PSO_DESERIALIZE)"
                );
            }
            on
        })
    }
}

/// Whether a pipeline is only handed out once its programs translate
/// (`VICTORY_PSO_DESERIALIZE_CHECK=1`; off by default: they are translated on
/// first use, as any other).
fn check() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PSO_DESERIALIZE_CHECK").is_some_and(|v| v == "1"))
}

/// Counts a call of method `m` (0 graphics, 1 compute) and its outcome,
/// and logs the counts at the 1st, 2nd, 4th ... call and every 1024th (the
/// title makes some 20,000 calls in a second of its boot).
fn note(m: usize, taken: bool) {
    let calls = COUNTS[m][0].fetch_add(1, Ordering::Relaxed) + 1;
    COUNTS[m][if taken { 1 } else { 2 }].fetch_add(1, Ordering::Relaxed);
    if calls.is_power_of_two() || calls.is_multiple_of(1024) {
        tracing::info!(
            method = if m == 0 { "graphics (0x1c8)" } else { "compute (0x218)" },
            calls,
            handed_out = COUNTS[m][1].load(Ordering::Relaxed),
            refused = COUNTS[m][2].load(Ordering::Relaxed),
            "pipelines the title has ready, deserialized (VICTORY_PSO_DESERIALIZE)"
        );
    }
}

/// Logs why a description was refused, once per reason (and at most 64).
fn refused(method: &str, why: String) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    // (A reason may name a place in a program: not without end.)
    if seen.len() < 64 && !seen.contains(&why) {
        tracing::warn!("device method {method}: no pipeline of {why} (logged once)");
        seen.push(why);
    }
}

/// Reads title memory at `at` into `to`; false if not all of it is there.
/// Through the kernel: none of a description's addresses is known to be
/// mapped.
fn read_title(at: u64, to: &mut [u8]) -> bool {
    #[cfg(windows)]
    return crate::host::read_into(at as usize as *const u8, to);
    // Off Windows no title runs: tests come here, with blocks of their own.
    #[cfg(not(windows))]
    {
        // SAFETY: as said, `to.len()` bytes the caller owns.
        unsafe { std::ptr::copy_nonoverlapping(at as usize as *const u8, to.as_mut_ptr(), to.len()) };
        true
    }
}

/// The block of `len` bytes at `at` that a description names.
fn block(what: &str, at: u64, len: u64, cap: u64) -> Result<Vec<u8>, String> {
    if at == 0 || len == 0 || len > cap {
        return Err(format!("{what} of an implausible size"));
    }
    let mut bytes = vec![0; len as usize];
    if !read_title(at, &mut bytes) {
        return Err(format!("{what} that is not readable"));
    }
    Ok(bytes)
}

/// The words of the description at `desc`.
fn words(desc: *const u8) -> Result<[u64; WORDS], String> {
    let mut bytes = [0u8; 8 * WORDS];
    if desc.is_null() || !read_title(desc as u64, &mut bytes) {
        return Err("a description that is not readable".into());
    }
    let mut w = [0u64; WORDS];
    for (w, c) in w.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *w = u64::from_le_bytes(*c);
    }
    Ok(w)
}

/// Whether the stage whose words start at `at` is absent: all four 0.
fn absent(w: &[u64], at: usize) -> bool {
    w[at..at + 4].iter().all(|&x| x == 0)
}

/// Device method 0x1c8 (`DeserializeGraphicsPipelineStateX`): a graphics
/// pipeline of linked programs, the code as the hardware runs it with the
/// registers of its state, as `(this, description, interface id, out)`.
///
/// ```text
/// 0       the root signature; only told apart by here
/// 1, 2    the register image: address, bytes (`victory_shader::GraphicsRegs`)
/// 3, 4    formats and input layout (`victory_shader::InputLayout`)
/// 5, 6    the pixel program's code, 7, 8 its metadata; all 0 without one
/// 9-12    the vertex program's, likewise
/// 13-32   the other geometry stages', 0 in what is taken
/// ```
///
/// The programs become containers of ours and the registers a graphics
/// state, and from there it is a pipeline like one from a stream
/// (`pipeline::create_linked`): translated and made on its first draw.
/// The title passes one set of programs in many descriptions; what tells
/// descriptions apart ([`linked_pipeline`]) is all that is read of one seen
/// before.
///
/// A description that is not all of this is refused as the method always
/// was: `E_NOTIMPL` with `out` null, the reason logged once.
/// shortcut: one with a geometry program (the console's primitive shader,
/// in place of a vertex program) is refused: the register image has other
/// registers first, and no such program is translated.
pub(super) extern "system" fn create_graphics(this: This, desc: *const u8, iid: *const Iid, out: *mut This) -> HRESULT {
    if !on()[0] {
        return super::unknown_pipeline::<0x1c8>(this, desc, iid, out);
    }
    let hr = match linked_pipeline(desc, iid, out) {
        Ok(hr) => hr,
        Err(why) => {
            if let Some(why) = why {
                refused("0x1c8", why);
            }
            null_out(out)
        }
    };
    note(0, hr >= 0);
    hr
}

/// `E_NOTIMPL`, with `out` null.
fn null_out(out: *mut This) -> HRESULT {
    if !out.is_null() {
        // SAFETY: the title's out pointer (a local of its own).
        unsafe { *out = std::ptr::null() };
    }
    E_NOTIMPL
}

/// The pipeline of the description at `desc`: `Ok` with what the title
/// gets, `Err` for a description that is refused, with the reason unless it
/// was refused before (`out` is not written then).
///
/// A description is told from the others by its first word, the sizes of
/// its blocks, the bytes of its register image and of its formats and input
/// layout, and the hash each program's code starts with: read on every call
/// (some 900 bytes), and looked up before anything else is read.
fn linked_pipeline(desc: *const u8, iid: *const Iid, out: *mut This) -> Result<HRESULT, Option<String>> {
    let w = words(desc)?;
    if !w[GEOMETRY..].iter().all(|&x| x == 0) {
        return Err(Some(if absent(&w, VERTEX) {
            "a geometry program in place of a vertex program".into()
        } else {
            "another geometry stage's program".into()
        }));
    }
    if w[VERTEX] == 0 {
        return Err(Some("no vertex program".into()));
    }
    let pixel = w[STAGES] != 0;
    if !pixel && !absent(&w, STAGES) {
        return Err(Some("part of a pixel program".into()));
    }
    let image = block("a register image", w[1], w[2], STATE_CAP)?;
    let layout = block("formats and input layout", w[3], w[4], STATE_CAP)?;
    let mut hashes = [[0u8; HASH_BYTES]; 2];
    for (hash, at) in hashes.iter_mut().zip([VERTEX, STAGES]) {
        if at == STAGES && !pixel {
            continue;
        }
        if w[at + 1] < HASH_BYTES as u64 || !read_title(w[at], hash) {
            return Err(Some("program code that is not readable".into()));
        }
    }
    let mut key = blake3::Hasher::new();
    // (The sizes first: they say where each of the blocks after them ends.)
    for k in [0, 2, 4, 6, 8, 10, 12] {
        key.update(&w[k].to_le_bytes());
    }
    key.update(&image).update(&layout).update(hashes.as_flattened());
    let key = *key.finalize().as_bytes();
    if let Some(hr) = pipeline::deserialized_again(&key, iid, out) {
        return if hr == E_NOTIMPL { Err(None) } else { Ok(hr) };
    }
    linked_new(key, &w, &image, &layout, iid, out).map_err(|why| {
        pipeline::refuse_deserialized(key);
        Some(why)
    })
}

/// A pipeline of a description of device method 0x1c8 not seen before: of
/// its words `w`, register image and formats and input layout. `Err` names
/// what of it is not what the parsers know, or has no equal on the host.
/// shortcut: the vertex program is taken to run in waves of 32 lanes (its
/// VGPR count only fits granules of 8); where the registers say is not
/// known. The pixel program's size is `SPI_PS_IN_CONTROL`'s.
fn linked_new(
    key: [u8; 32],
    w: &[u64],
    image: &[u8],
    layout: &[u8],
    iid: *const Iid,
    out: *mut This,
) -> Result<HRESULT, String> {
    use victory_shader::native::linked_container;
    use victory_shader::{GraphicsRegs, HwStage, InputLayout};
    let regs = GraphicsRegs::parse(image).map_err(|e| format!("a register image: {e}"))?;
    let layout = InputLayout::parse(layout).map_err(|e| format!("formats and input layout: {e}"))?;
    let code = |at: usize| block("program code", w[at], w[at + 1], CODE_CAP);
    let meta = |at: usize| block("program metadata", w[at + 2], w[at + 3], META_CAP);
    let io = regs.vertex_io(&layout);
    let vertex = linked_container(HwStage::Vs, &code(VERTEX)?, &meta(VERTEX)?, regs.vs_rsrc1, false, &io)
        .map_err(|e| format!("a vertex program: {e}"))?;
    let pixel = if w[STAGES] != 0 {
        let (rsrc1, wave64, io) = (regs.ps_rsrc1, regs.ps_wave64(), regs.pixel_io());
        let pixel = linked_container(HwStage::Ps, &code(STAGES)?, &meta(STAGES)?, rsrc1, wave64, &io)
            .map_err(|e| format!("a pixel program: {e}"))?;
        Some(pixel)
    } else {
        None
    };
    if check() {
        pipeline::translates("vertex", &vertex)?;
        pixel.as_deref().map(|p| pipeline::translates("pixel", p)).transpose()?;
    }
    #[cfg(windows)]
    let graphics = pipeline::host::GraphicsState::linked(&regs, &layout).map_err(|e| format!("state with {e}"))?;
    Ok(pipeline::create_linked(
        key,
        &vertex,
        pixel.as_deref(),
        #[cfg(windows)]
        graphics,
        iid,
        out,
    ))
}

/// Device method 0x218 (`DeserializeComputePipelineStateX`): a compute
/// pipeline, as `(this, description, interface id, out)`.
///
/// ```text
/// 0       the root signature
/// 1, 2    the register packet (0x1c bytes: group size, wave size,
///         COMPUTE_PGM_RSRC1 and 2)
/// 3, 4    the pipeline's metadata (0x14 bytes, a hash first)
/// 5-8     the program's code and its metadata
/// ```
///
/// They are the four blocks of the `FSPX` part a container of another
/// compiler carries (`victory_shader::native::fspx_program`), so the
/// program becomes one such container of ours and from there it is a
/// pipeline like one of `CreateComputePipelineState`. Descriptions are told
/// apart by their first word, packet, both metadata blocks and the code's
/// hash: what is read of one seen before.
///
/// shortcut: a program of the ray-tracing structure builder (one with a
/// spilled user-data table) is refused: the builder runs all its pipelines
/// or none (`pipeline::of_builder`), and only containers are counted there.
pub(super) extern "system" fn create_compute(this: This, desc: *const u8, iid: *const Iid, out: *mut This) -> HRESULT {
    if !on()[1] {
        return super::unknown_pipeline::<0x218>(this, desc, iid, out);
    }
    let hr = match compute_pipeline(desc, iid, out) {
        Ok(hr) => hr,
        Err(why) => {
            if let Some(why) = why {
                refused("0x218", why);
            }
            null_out(out)
        }
    };
    note(1, hr >= 0);
    hr
}

/// As [`linked_pipeline`], for device method 0x218.
fn compute_pipeline(desc: *const u8, iid: *const Iid, out: *mut This) -> Result<HRESULT, Option<String>> {
    // (The words past the compute stage's are not the title's.)
    let w = words(desc)?;
    let packet = block("a register packet", w[1], w[2], STATE_CAP)?;
    let pipeline_meta = block("pipeline metadata", w[3], w[4], STATE_CAP)?;
    let meta = block("program metadata", w[STAGES + 2], w[STAGES + 3], META_CAP)?;
    let mut hash = [0u8; HASH_BYTES];
    if w[STAGES + 1] < HASH_BYTES as u64 || !read_title(w[STAGES], &mut hash) {
        return Err(Some("program code that is not readable".into()));
    }
    let mut key = blake3::Hasher::new();
    key.update(b"compute");
    for k in [0, 2, 4, STAGES + 1, STAGES + 3] {
        key.update(&w[k].to_le_bytes());
    }
    key.update(&packet).update(&pipeline_meta).update(&meta).update(&hash);
    let key = *key.finalize().as_bytes();
    if let Some(hr) = pipeline::deserialized_again(&key, iid, out) {
        return if hr == E_NOTIMPL { Err(None) } else { Ok(hr) };
    }
    compute_new(key, &w, &packet, &pipeline_meta, &meta, iid, out).map_err(|why| {
        pipeline::refuse_deserialized(key);
        Some(why)
    })
}

/// A pipeline of a description of device method 0x218 not seen before.
fn compute_new(
    key: [u8; 32],
    w: &[u64],
    packet: &[u8],
    pipeline_meta: &[u8],
    meta: &[u8],
    iid: *const Iid,
    out: *mut This,
) -> Result<HRESULT, String> {
    use victory_shader::native::{deserialized_compute_container, fspx_program};
    let code = block("program code", w[STAGES], w[STAGES + 1], CODE_CAP)?;
    let container = deserialized_compute_container(packet, pipeline_meta, &code, meta)
        .map_err(|e| format!("a compute program: {e}"))?;
    // (`deserialized_compute_container` has read the part back once)
    let c = victory_shader::Container::parse(&container).map_err(|e| e.to_string())?;
    let part = c.part(victory_shader::FourCC::ACCC).ok_or("no part")?;
    let program = fspx_program(part.data).map_err(|e| e.to_string())?;
    if victory_shader::spill::is_spilled(&program) {
        return Err("a program of the structure builder".into());
    }
    if check() {
        pipeline::translates("compute", &container)?;
    }
    Ok(pipeline::create_deserialized_compute(key, &container, iid, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::device;
    use crate::object::{self, S_OK, slot};

    type Create = extern "system" fn(This, *const u8, *const Iid, *mut This) -> HRESULT;

    /// Calls method `at` with the description `w`; what it answered and
    /// what it left in `out` (which holds something of the title's stack
    /// until it is written).
    fn make(at: usize, w: &[u64; WORDS]) -> (HRESULT, This) {
        let mut p = device();
        let hr = slot::<Create>(device(), at)(device(), w.as_ptr().cast(), &object::IUNKNOWN, &mut p);
        (hr, p)
    }

    fn release(p: This) {
        assert_eq!(slot::<extern "system" fn(This) -> u32>(p, 0x10)(p), 0);
    }

    /// A linked program of no instructions, as device method 0x1c8 gets
    /// one (`victory_shader::native::linked_program`): its code, with the
    /// hash (0, `tag`, 0, `!tag`) in the two leading `v_nop`s, `s_endpgm`
    /// and the ten dwords after it, and its metadata, the hash record alone.
    fn linked_program(tag: u32) -> (Vec<u8>, Vec<u8>) {
        let mut code = vec![0xD580_0000, 0x03FE_0100, tag, 0xD580_0000, 0x03FE_0100, !tag, 0xBF81_0000];
        code.extend([0xBF9F_0000; 10]);
        let meta = [2, 0x24, 7, 0x440 | 1 << 15, 0x43, 0, tag, 0, !tag, 0, 0, 0];
        (bytes(&code), bytes(&meta))
    }

    fn bytes(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A description whose blocks are `blocks`, each at the word it names.
    fn description(blocks: &[(usize, &[u8])]) -> [u64; WORDS] {
        let mut w = [0; WORDS];
        w[0] = 0x1c8;
        for &(k, block) in blocks {
            w[k] = block.as_ptr() as u64;
            w[k + 1] = block.len() as u64;
        }
        w
    }

    #[test]
    fn linked_descriptions_give_pipelines_and_one_seen_before_shares_its_first() {
        // The register image of a pipeline without interpolants (its last
        // section, of 17 dwords, at +0x138), and formats without an input
        // element (4 bytes after +0x4c: their count), for triangles.
        let mut image = vec![0u8; 0x17c];
        image[0x28..0x2c].copy_from_slice(&0x0044_011c_u32.to_le_bytes());
        let mut layout = vec![0u8; 0x50];
        (layout[0x26], layout[0x38]) = (3, 4);
        let (code, meta) = linked_program(0x01c8_0001);
        let of = |image: &[u8], layout: &[u8], code: &[u8], meta: &[u8]| {
            description(&[(1, image), (3, layout), (VERTEX, code), (VERTEX + 2, meta)])
        };

        let (hr, p) = make(0x1c8, &of(&image, &layout, &code, &meta));
        assert_eq!(hr, S_OK);
        assert!(!p.is_null() && p != device());
        // The same description from other memory: an object of its own, of
        // the same containers and the same host pipeline to be.
        let copy = (image.clone(), layout.clone(), code.clone(), meta.clone());
        let (hr, q) = make(0x1c8, &of(&copy.0, &copy.1, &copy.2, &copy.3));
        assert_eq!(hr, S_OK);
        assert_ne!(p, q);
        assert!(pipeline::shares(p, q));
        // Other state is another description, of the same programs.
        let mut points = layout.clone();
        points[0x26] = 1;
        let (hr, r) = make(0x1c8, &of(&image, &points, &code, &meta));
        assert_eq!(hr, S_OK);
        #[cfg(windows)]
        assert!(!pipeline::shares(p, r));
        for p in [p, q, r] {
            release(p);
        }

        // Refused, with `out` null: code and metadata of different programs
        // (the second time from what is remembered of the first), state the
        // parsers do not know, a geometry program, a further stage, no
        // description.
        let (other, _) = linked_program(0x01c8_0002);
        let mismatched = of(&image, &layout, &other, &meta);
        let unknown = of(&image[4..], &layout, &code, &meta);
        let geometry = description(&[(1, &image), (3, &layout), (GEOMETRY, &code), (GEOMETRY + 2, &meta)]);
        let mut further = of(&image, &layout, &code, &meta);
        further[WORDS - 4] = code.as_ptr() as u64;
        for w in [&mismatched, &mismatched, &unknown, &geometry, &further] {
            assert_eq!(make(0x1c8, w), (E_NOTIMPL, std::ptr::null()));
        }
        let mut p = device();
        let hr = slot::<Create>(device(), 0x1c8)(device(), std::ptr::null(), &object::IUNKNOWN, &mut p);
        assert_eq!((hr, p), (E_NOTIMPL, std::ptr::null()));
    }

    /// A compute program as device method 0x218 gets it: code of
    /// `s_endpgm` after the hash (0, `tag`, 0, `!tag`), its metadata with
    /// the hash record alone, a packet of 64 threads in a wave of 32 with
    /// `rsrc2`, and the pipeline's metadata.
    fn compute_program(tag: u32, rsrc2: u32) -> [Vec<u8>; 4] {
        let code = bytes(&[0xD580_0000, 0x03FE_0100, tag, 0xD580_0000, 0x03FE_0100, !tag, 0xBF81_0000, 0xBF9F_0000]);
        let meta = bytes(&[0x403, 0x24, 8, 0, 0, 0x440 | 1 << 15, 0x43, 0, tag, 0, !tag, 0, 0, 0]);
        let packet = bytes(&[0, 0, 0, 64 | 1 << 16, 1 | 1 << 20, 0x01, rsrc2]);
        [packet, bytes(&[tag, 3, 0, 0, 0]), code, meta]
    }

    #[test]
    fn compute_descriptions_give_pipelines_and_one_seen_before_shares_its_first() {
        let of = |[packet, pipeline_meta, code, meta]: &[Vec<u8>; 4]| {
            description(&[(1, packet), (3, pipeline_meta), (STAGES, code), (STAGES + 2, meta)])
        };
        let program = compute_program(0x0218_0001, 0x84);
        let (hr, p) = make(0x218, &of(&program));
        assert_eq!(hr, S_OK);
        assert!(!p.is_null() && p != device());
        let (hr, q) = make(0x218, &of(&program.clone()));
        assert_eq!(hr, S_OK);
        assert_ne!(p, q);
        assert!(pipeline::shares(p, q));
        // Another packet is another pipeline.
        let (hr, r) = make(0x218, &of(&compute_program(0x0218_0001, 0x86)));
        assert_eq!(hr, S_OK);
        assert!(!pipeline::shares(p, r));
        for p in [p, q, r] {
            release(p);
        }

        // Refused, with `out` null: code of another program, a packet of
        // another size, no description.
        let mut mismatched = compute_program(0x0218_0002, 0x84);
        mismatched[2] = compute_program(0x0218_0003, 0x84)[2].clone();
        let mut short = compute_program(0x0218_0004, 0x84);
        short[0].truncate(0x18);
        for w in [&of(&mismatched), &of(&mismatched), &of(&short)] {
            assert_eq!(make(0x218, w), (E_NOTIMPL, std::ptr::null()));
        }
        let mut p = device();
        let hr = slot::<Create>(device(), 0x218)(device(), std::ptr::null(), &object::IUNKNOWN, &mut p);
        assert_eq!((hr, p), (E_NOTIMPL, std::ptr::null()));
    }

    /// What a run with `VICTORY_PSO_DUMP` wrote of the descriptions of the
    /// method `slot` ("1c8", "218"), when the worktree has it
    /// (`.agente/pso-dump`; no game data is kept in the repository): each
    /// description's name, and its blocks by the word that names them.
    fn dumped(slot: &str) -> Vec<(String, Vec<(usize, Vec<u8>)>)> {
        let dir = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.agente/pso-dump"));
        let mut all: Vec<(String, Vec<(usize, Vec<u8>)>)> = Vec::new();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let prefix = format!("{slot}-");
        for name in names {
            // "<slot>-<description>-w<word>-<address>.bin"
            let part = name.strip_prefix(&prefix).and_then(|n| n.strip_suffix(".bin"));
            let mut parts = part.into_iter().flat_map(|n| n.split('-'));
            let (Some(d), Some(word)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(word) = word.strip_prefix('w').and_then(|w| w.parse().ok()) else {
                continue;
            };
            // (The compute dumps read on past the description, into the next one.)
            if slot == "218" && word > STAGES + 2 {
                continue;
            }
            let Ok(block) = std::fs::read(dir.join(&name)) else {
                continue;
            };
            match all.last_mut() {
                Some(last) if last.0 == d => last.1.push((word, block)),
                _ => all.push((d.to_owned(), vec![(word, block)])),
            }
        }
        all
    }

    /// Every description dumped gets a pipeline, but the one of a geometry
    /// program; and with a host GPU each pipeline's programs translate and
    /// the host takes them with the state read from the registers, but for
    /// one pixel program that reads the fixed-point position, which the
    /// translation has not, and the compute programs that do not translate
    /// yet (`victory_shader`'s test of them names them).
    #[test]
    fn dumped_descriptions_give_pipelines() {
        let mut without = Vec::new();
        for slot in ["1c8", "218"] {
            for (d, blocks) in dumped(slot) {
                let refs: Vec<(usize, &[u8])> = blocks.iter().map(|(k, b)| (*k, &b[..])).collect();
                let w = description(&refs);
                let (hr, p) = make(if slot == "1c8" { 0x1c8 } else { 0x218 }, &w);
                if slot == "1c8" && w[VERTEX] == 0 {
                    assert_eq!((hr, p), (E_NOTIMPL, std::ptr::null()), "{d}");
                    continue;
                }
                assert_eq!(hr, S_OK, "{slot} {d}");
                #[cfg(windows)]
                if crate::host::host().is_some()
                    && let Err(why) = pipeline::made_now(p)
                {
                    without.push(format!("{slot} {d}: {why}"));
                }
                release(p);
            }
        }
        eprintln!("no host pipeline: {without:#?}");
        assert!(without.len() <= 6, "no host pipeline: {without:#?}");
    }
}
