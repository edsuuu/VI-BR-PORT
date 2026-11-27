//! Pipeline states. Both descriptions start as on PC: the root signature,
//! then the shader stages (five for graphics, one for compute); streams
//! carry the rest of the graphics state as subobjects of the standard
//! types.
//!
//! Host pipelines are made on a pipeline's first draw, not at creation:
//! The title creates some 60,000 pipelines in its first 20
//! seconds and draws with a few dozen (decision 0007, step 3). Shader
//! containers are shared by content hash and translated once, on first
//! use (decision 0003).
//!
//! shortcut: only vertex plus pixel pipelines from streams and compute
//! pipelines get host pipelines; mesh, tessellation and geometry stages,
//! and the plain graphics description, keep their containers only. The 32-byte
//! packet after the header stays zero: titles only copy it inline, which
//! our null command-list limits keep from running.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::HashMap;
use std::mem::offset_of;
use std::sync::{Arc, Mutex, OnceLock};

use crate::object::{self, E_INVALIDARG, HRESULT, Iid, Object, PIPELINE, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;

const PIPELINE_IIDS: [Iid; 4] = [
    guid(
        0x765a30f3,
        0xf624,
        0x4c6f,
        [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
pub(crate) struct Bytecode {
    code: *const u8,
    size: usize,
}

#[repr(C)]
pub(crate) struct GraphicsHead {
    root: This,
    /// Vertex, pixel, domain, hull, geometry.
    stages: [Bytecode; 5],
}

#[repr(C)]
pub(crate) struct ComputeHead {
    root: This,
    stage: Bytecode,
}

/// A shader container a title passed, shared by every pipeline with the
/// same bytes.
pub(crate) struct Shader {
    pub(crate) stage: &'static str,
    /// The first 8 bytes of its BLAKE3, as logged.
    pub(crate) hash: String,
    container: Vec<u8>,
    /// Its part in the stand-in for the indirect light.
    #[cfg(windows)]
    pub(crate) ambient: Option<crate::ambient::Role>,
    /// A pixel program of the title's decoration bake ([`bakes`]).
    #[cfg(windows)]
    pub(crate) bakes: bool,
    /// A pixel program of the title's characters' G-buffer pass
    /// (`PS_DeferredPed*`): what it samples a capture can save
    /// ([`crate::capture::note_ped_read`]).
    #[cfg(all(windows, feature = "diagnostics"))]
    pub(crate) peds: bool,
    #[cfg(windows)]
    translated: OnceLock<Option<Translated>>,
}

impl std::fmt::Debug for Shader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.stage, self.hash)
    }
}

/// Every container seen, by full content hash.
static SHADERS: Mutex<Option<HashMap<[u8; 32], Arc<Shader>>>> = Mutex::new(None);

/// For the memory lines: containers kept and their bytes, translations
/// kept and the bytes of their host code. None of them is ever let go.
static KEPT: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// What the shader table and the pipeline library keep in memory:
/// containers and their bytes, translations and their bytes, the bytes of
/// the library's file (kept whole while the library lives) and what its
/// last save wrote.
#[cfg(windows)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) fn kept() -> [u64; 7] {
    use std::sync::atomic::Ordering::Relaxed;
    [
        KEPT[0].load(Relaxed),
        KEPT[1].load(Relaxed),
        KEPT[2].load(Relaxed),
        KEPT[3].load(Relaxed),
        library::BLOB.load(Relaxed),
        library::SAVED.load(Relaxed),
        library::TRIMMED_BYTES.load(Relaxed),
    ]
}

/// The shared container for `bytes`.
fn shader(stage: &'static str, bytes: &[u8]) -> Arc<Shader> {
    let full = blake3::hash(bytes);
    let mut shaders = SHADERS.lock().unwrap_or_else(|e| e.into_inner());
    shaders
        .get_or_insert_with(HashMap::new)
        .entry(*full.as_bytes())
        .or_insert_with(|| {
            KEPT[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            KEPT[1].fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
            Arc::new(Shader {
                stage,
                hash: full.to_hex()[..16].to_owned(),
                container: bytes.to_vec(),
                #[cfg(windows)]
                ambient: (stage == "compute").then(|| crate::ambient::role(bytes)).flatten(),
                #[cfg(windows)]
                bakes: bakes(bytes),
                #[cfg(all(windows, feature = "diagnostics"))]
                peds: stage == "pixel" && bytes.windows(14).any(|w| w == b"PS_DeferredPed"),
                #[cfg(windows)]
                translated: OnceLock::new(),
            })
        })
        .clone()
}

/// A translated shader and the console resources its code reads.
#[cfg(windows)]
pub(crate) struct Translated {
    pub(crate) host: victory_shader::emit::HostShader,
    /// A compute program's threads in a group.
    pub(crate) threads: Option<[u32; 3]>,
    pub(crate) used: Vec<victory_shader::UsedResource>,
    /// Whether the shader's code can store to buffer memory ([`stores`]);
    /// true when its code is not known.
    pub(crate) stores: bool,
    /// Whether it can store through raw addresses ([`stores_by_address`]).
    pub(crate) stores_by_address: bool,
    /// Whether it stores through a view it picks from the heap by an index,
    /// once asked (`crate::picked`).
    pub(crate) picks: std::sync::atomic::AtomicU8,
}

#[cfg(windows)]
impl Shader {
    /// The translation, made on first use; a shader that does not
    /// translate is logged once.
    pub(crate) fn translated(&self) -> Option<&Translated> {
        self.translated
            .get_or_init(|| {
                let done = translate(&self.container);
                // `VICTORY_SHADER_SAVE=<dir>`: each shader as the title gave it (`.cdxil`) and as
                // translated (`.dxil`), by the hash crash reports name it with
                if let Some(dir) = std::env::var_os("VICTORY_SHADER_SAVE") {
                    let dir = std::path::PathBuf::from(dir);
                    let _ = std::fs::create_dir_all(&dir);
                    let stem = format!("{}.{}", self.hash, self.stage);
                    let _ = std::fs::write(dir.join(format!("{stem}.cdxil")), &self.container);
                    if let Ok(t) = &done {
                        let _ = std::fs::write(dir.join(format!("{stem}.dxil")), &t.host.dxil);
                    }
                }
                if let Ok(t) = &done {
                    KEPT[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    KEPT[3].fetch_add(t.host.dxil.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                match &done {
                    Ok(t) => tracing::info!(
                        stage = self.stage,
                        hash = %self.hash,
                        user_data = t.host.user_data.len(),
                        samplers = t.host.samplers.len(),
                        "shader translated"
                    ),
                    Err(e) => tracing::warn!(
                        stage = self.stage,
                        hash = %self.hash,
                        error = %e,
                        "shader does not translate; its pipelines' draws are dropped"
                    ),
                }
                done.ok()
            })
            .as_ref()
    }
}

#[cfg(windows)]
impl Shader {
    /// The console programs of the container; `None` if they are not known.
    pub(crate) fn programs(&self) -> Option<Vec<victory_shader::NativeProgram>> {
        use victory_shader::{Container, FourCC};
        let c = Container::parse(&self.container).ok()?;
        let part = c.part(FourCC::DXIL).or(c.part(FourCC::ACCC))?;
        victory_shader::native_programs(part.data).ok()
    }
}

#[cfg(windows)]
fn translate(container: &[u8]) -> Result<Translated, String> {
    use victory_shader::{Container, FourCC, HwStage};
    // The translation depends on it (a view index past a capped heap).
    crate::descriptor::host::probe_view_limit();
    let c = Container::parse(container).map_err(|e| e.to_string())?;
    let programs = match c.part(FourCC::DXIL).or(c.part(FourCC::ACCC)) {
        Some(part) => victory_shader::native_programs(part.data).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    let host = crate::shader_cache::host_shader(container)?;
    let stores = programs.is_empty() || programs.iter().any(stores);
    let threads = programs.iter().find_map(|p| match &p.io {
        victory_shader::native::StageIo::Cs(cs) if cs.threads.iter().all(|&n| n > 0) => Some(cs.threads),
        _ => None,
    });
    let stores_by_address = programs.is_empty() || programs.iter().any(stores_by_address);
    let used = programs
        .into_iter()
        .find(|p| {
            matches!(
                p.stage,
                HwStage::Vs | HwStage::Ps | HwStage::Ms | HwStage::Cs
            )
        })
        .map(|p| p.used)
        .unwrap_or_default();
    Ok(Translated {
        host,
        threads,
        used,
        stores,
        stores_by_address,
        picks: Default::default(),
    })
}

/// The compute pipelines of the console's ray-tracing structure builder
/// (`XboxBvhBuilder.dll`; its programs are the ones with a spilled
/// user-data table) run **all or none**. Its stages feed each other
/// through memory: a stage that runs on what a dropped stage never wrote
/// computes its store offsets from garbage. The structures lie in plain
/// title memory, between the title's own objects, and a root view has no
/// size, so those stores land on the neighbours (run 98, with 14 of 35
/// stages translated: the title died within a second of the first builder
/// submission, on pointers that had become bounding-box bit patterns).
///
/// So a builder dispatch runs only while every builder pipeline the title
/// has created has a host pipeline; the first builder dispatch makes them
/// all on the spot instead of over the next frames (a stage "still being
/// made" would be a dropped stage). While one cannot be made the builder
/// is off; it is looked at again when the title creates another. A stage
/// that could not be bound stops the stages after it for the rest of that
/// frame and the next ([`builder_rests`]); it turned the builder off for the
/// rest of the run at first.
///
/// The first look makes every builder pipeline not made yet on the spot,
/// on the title's thread, so a pipeline is either made or refused there
/// and the decision holds until the title creates another. With
/// `VICTORY_BVH_RETRY=1` nothing is made or waited for at a dispatch: a
/// pipeline the workers are still making keeps the builder off for now,
/// and it is looked at again at the next builder dispatch; once all are
/// made it runs from the next frame, so no frame runs only the later of its
/// stages. One refused keeps it off as without the switch. (Unset: as
/// before, the first look decides.)
///
/// The decision is published where anything may read it without taking
/// the lock or making a pipeline ([`builder_known_off`]).
///
/// shortcut: and only with `VICTORY_BVH_BUILDER=1`, until a run has shown
/// the whole builder and the host side of title memory by address
/// (`memory.rs`) working on a card; nothing reads the structures before
/// the programs that walk them translate.
#[cfg(windows)]
struct Builder {
    pipelines: Vec<Arc<make::Pending>>,
    /// Every one of `pipelines` has a host pipeline.
    whole: bool,
    /// One could not be made, of so many (0: none).
    off: usize,
    /// No stage runs before this frame.
    rests: u64,
    /// The last decision ([`UNDECIDED`], [`RUNS`] or [`OFF`]).
    state: u8,
    /// The pipelines without a host pipeline at the last look that turned
    /// it off (their shader hashes), and whether they were only not made
    /// yet (`VICTORY_BVH_RETRY=1`).
    missing: Vec<String>,
    waiting: bool,
}

#[cfg(windows)]
static BUILDER: Mutex<Builder> = Mutex::new(Builder::new());

/// No builder dispatch has been looked at yet.
#[cfg(windows)]
const UNDECIDED: u8 = 0;
/// The builder runs: every one of its pipelines has a host pipeline.
#[cfg(windows)]
const RUNS: u8 = 1;
/// The builder is off: one of its pipelines has none (yet).
#[cfg(windows)]
const OFF: u8 = 2;

/// [`Builder::state`], published at each decision for readers that must
/// neither take the lock nor make anything (the passes that only serve
/// rays, [`skips_rt_skin`]).
#[cfg(windows)]
static BUILDER_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(UNDECIDED);

/// What a builder pipeline is at a look ([`Builder::look`]).
#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Made {
    Yes,
    Refused,
    NotYet,
}

#[cfg(windows)]
impl Builder {
    const fn new() -> Builder {
        Builder {
            pipelines: Vec::new(),
            whole: false,
            off: 0,
            rests: 0,
            state: UNDECIDED,
            missing: Vec::new(),
            waiting: false,
        }
    }

    /// At a builder dispatch in frame `frame`: whether the builder runs.
    /// `status` says what one of `pipelines` is (making it first, without
    /// [`retry`]); `hashes` names it in the log.
    fn look(
        &mut self,
        frame: u64,
        retry: bool,
        mut status: impl FnMut(&Arc<make::Pending>) -> Made,
        hashes: impl Fn(&make::Pending) -> String,
    ) -> bool {
        if self.off == self.pipelines.len() || frame < self.rests {
            return false;
        }
        if self.whole {
            return true;
        }
        let mut refused = Vec::new();
        let mut not_yet = Vec::new();
        for p in &self.pipelines {
            match status(p) {
                Made::Yes => {}
                Made::Refused => refused.push(hashes(p)),
                Made::NotYet => not_yet.push(hashes(p)),
            }
        }
        let of = self.pipelines.len();
        if !refused.is_empty() || (!not_yet.is_empty() && !retry) {
            self.off = of;
            self.state = OFF;
            self.waiting = false;
            refused.extend(not_yet);
            tracing::warn!(
                missing = refused.len(),
                of,
                which = ?refused,
                "the ray-tracing structure builder stays off: not all of its programs have a host pipeline, and its stages run all or none"
            );
            self.missing = refused;
            return false;
        }
        if !not_yet.is_empty() {
            // Looked at again at the next dispatch: said when the count changes.
            if !self.waiting || self.missing.len() != not_yet.len() {
                tracing::info!(
                    being_made = not_yet.len(),
                    of,
                    which = ?not_yet,
                    "the ray-tracing structure builder waits: some of its programs are still being made (VICTORY_BVH_RETRY=1)"
                );
            }
            self.state = OFF;
            self.waiting = true;
            self.missing = not_yet;
            return false;
        }
        self.off = 0;
        self.whole = true;
        self.state = RUNS;
        self.missing.clear();
        if std::mem::take(&mut self.waiting) {
            // Not from the middle of a frame whose first stages were dropped.
            self.rests = frame + 1;
            tracing::info!(
                pipelines = of,
                from_frame = self.rests,
                "the ray-tracing structure builder runs from the next frame: all of its programs have host pipelines now"
            );
            return false;
        }
        tracing::info!(
            pipelines = of,
            "the ray-tracing structure builder runs: all of its programs have host pipelines"
        );
        true
    }
}

#[cfg(all(test, windows))]
mod builder_tests {
    use super::*;

    /// A builder of `n` pipelines, and what each is at the next look.
    fn builder(n: usize) -> (Builder, Vec<Arc<make::Pending>>) {
        let mut b = Builder::new();
        b.pipelines = (0..n).map(|_| Arc::new(make::Pending::new(Vec::new(), None))).collect();
        let pipelines = b.pipelines.clone();
        (b, pipelines)
    }

    fn look(b: &mut Builder, pipelines: &[Arc<make::Pending>], frame: u64, retry: bool, made: &[Made]) -> bool {
        b.look(
            frame,
            retry,
            |p| made[pipelines.iter().position(|q| Arc::ptr_eq(p, q)).unwrap()],
            |_| "hash".to_owned(),
        )
    }

    /// With the retry, a pipeline the workers make late keeps the builder
    /// off only until it is there, and the builder starts at a frame's start.
    #[test]
    fn a_late_pipeline_turns_the_builder_on_at_the_next_frame() {
        use Made::*;
        let (mut b, p) = builder(3);
        assert!(!look(&mut b, &p, 10, true, &[Yes, NotYet, Yes]));
        assert_eq!((b.state, b.waiting, b.missing.len()), (OFF, true, 1));
        assert!(!look(&mut b, &p, 10, true, &[Yes, NotYet, Yes]));
        // Made in the middle of frame 10: not before frame 11.
        assert!(!look(&mut b, &p, 10, true, &[Yes, Yes, Yes]));
        assert_eq!((b.state, b.waiting), (RUNS, false));
        assert!(!look(&mut b, &p, 10, true, &[Yes, Yes, Yes]));
        assert!(look(&mut b, &p, 11, true, &[Yes, Yes, Yes]));
        assert!(look(&mut b, &p, 12, true, &[Yes, Yes, Yes]));
    }

    /// Without the retry the first look decides (what it saw was made on
    /// the spot) until the title creates another builder pipeline.
    #[test]
    fn without_the_retry_the_first_look_decides() {
        use Made::*;
        let (mut b, mut p) = builder(2);
        assert!(!look(&mut b, &p, 5, false, &[Yes, Refused]));
        assert_eq!((b.state, b.off), (OFF, 2));
        assert!(!look(&mut b, &p, 6, false, &[Yes, Yes]));
        b.pipelines.push(Arc::new(make::Pending::new(Vec::new(), None)));
        p = b.pipelines.clone();
        assert!(look(&mut b, &p, 7, false, &[Yes, Yes, Yes]));
        assert_eq!(b.state, RUNS);
        // A builder that runs from its first look runs in that frame.
        let (mut b, p) = builder(2);
        assert!(look(&mut b, &p, 3, false, &[Yes, Yes]));
    }

    /// One refused keeps it off with the retry too; so does a pipeline
    /// still being made next to one refused.
    #[test]
    fn a_refused_pipeline_keeps_the_builder_off() {
        use Made::*;
        let (mut b, p) = builder(3);
        assert!(!look(&mut b, &p, 1, true, &[Refused, NotYet, Yes]));
        assert_eq!((b.state, b.waiting, b.missing.len()), (OFF, false, 2));
        assert!(!look(&mut b, &p, 2, true, &[Refused, Yes, Yes]));
        assert_eq!(b.off, 3);
    }

    /// No builder pipeline: nothing to run, nothing decided.
    #[test]
    fn no_pipeline_no_decision() {
        let (mut b, p) = builder(0);
        assert!(!look(&mut b, &p, 1, true, &[]));
        assert_eq!(b.state, UNDECIDED);
    }
}

/// `VICTORY_BVH_RETRY=1`: a builder pipeline still being made keeps the
/// builder off only until it is made ([`Builder`]). Unset: as before.
#[cfg(windows)]
fn retry() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_RETRY").is_some_and(|v| v == "1"))
}

/// Whether the title's own ray tracing is asked for: its structure builder
/// may run (`VICTORY_BVH_BUILDER=1`).
#[cfg(windows)]
fn builder_asked() -> bool {
    static ASKED: OnceLock<bool> = OnceLock::new();
    *ASKED.get_or_init(|| std::env::var_os("VICTORY_BVH_BUILDER").is_some_and(|v| v == "1"))
}

/// Builder dispatches let through to the host since the start
/// ([`builder_report`]).
#[cfg(windows)]
static BUILDER_DISPATCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A dispatch of the builder was let through ([`builder_runs`] said yes).
#[cfg(windows)]
pub(crate) fn builder_dispatched() {
    BUILDER_DISPATCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// With ray tracing asked for: a loud line at the first call (the run's
/// first frame report) and then every 30 s, saying whether the structure
/// builder runs, so that no run is measured as one with ray tracing while
/// nothing builds the structures. Off: which of its pipelines it misses
/// (refused, or still being made), of how many. Runs: how many of its
/// dispatches went to the card a frame since the last line. Nothing is
/// made or decided here.
#[cfg(windows)]
pub(crate) fn builder_report() {
    use std::sync::atomic::Ordering;
    if !builder_asked() {
        return;
    }
    // When the last line was said, at which frame and dispatch count.
    static LAST: Mutex<Option<(std::time::Instant, u64, u64)>> = Mutex::new(None);
    let frame = crate::frame::current();
    let dispatches = BUILDER_DISPATCHES.load(Ordering::Relaxed);
    let (frames, let_through, secs) = {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let since = last.map(|(at, f, d)| (at.elapsed(), frame.saturating_sub(f), dispatches.saturating_sub(d)));
        if since.is_some_and(|(elapsed, ..)| elapsed < std::time::Duration::from_secs(30)) {
            return;
        }
        *last = Some((std::time::Instant::now(), frame, dispatches));
        since.map_or((frame, dispatches, 0), |(elapsed, f, d)| (f, d, elapsed.as_secs()))
    };
    let b = BUILDER.lock().unwrap_or_else(|e| e.into_inner());
    let of = b.pipelines.len();
    match b.state {
        RUNS => tracing::info!(
            pipelines = of,
            per_frame = format_args!("{:.1}", let_through as f64 / frames.max(1) as f64),
            frames,
            secs,
            resting = frame < b.rests,
            "RAY TRACING asked for, structure builder RUNS: all {of} of its pipelines made; builder dispatches to the card per frame since the last line"
        ),
        OFF => tracing::warn!(
            missing = b.missing.len(),
            of,
            which = ?b.missing,
            still_being_made = b.waiting,
            "RAY TRACING asked for, structure builder OFF: missing {} of {of}; no structure is built, this run does not measure ray tracing",
            b.missing.len()
        ),
        _ => {
            let missing: Vec<String> =
                b.pipelines.iter().filter(|p| make::made(p) != Some(true)).map(|p| make::hashes(p)).collect();
            tracing::warn!(
                created = of,
                not_made = missing.len(),
                which = ?missing,
                "RAY TRACING asked for, structure builder not dispatched yet: {of} of its pipelines created, {} of them without a host pipeline so far",
                missing.len()
            );
        }
    }
}

/// Whether the builder's pipelines go to the workers ahead of the others,
/// at their creation and at a look ([`make::ahead`]): with the builder asked
/// for, unless `VICTORY_BVH_AHEAD=0` (then they wait in the list of
/// pipelines started at creation, and the first look makes the rest on the
/// title's thread, as before).
#[cfg(windows)]
fn builder_ahead() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    builder_asked() && *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_AHEAD").is_none_or(|v| v != "0"))
}

/// Whether the last look at a builder dispatch turned the builder off (or
/// keeps it off while its programs are made). Nothing is made, locked or
/// decided here; false before the first look.
#[cfg(windows)]
pub(crate) fn builder_known_off() -> bool {
    BUILDER_STATE.load(std::sync::atomic::Ordering::Relaxed) == OFF
}

/// Whether `shaders` are a pipeline of the structure builder.
#[cfg(windows)]
fn of_builder(shaders: &[Arc<Shader>]) -> bool {
    use victory_shader::{Container, FourCC};
    let [shader] = shaders else { return false };
    if shader.stage != "compute" {
        return false;
    }
    let Ok(c) = Container::parse(&shader.container) else {
        return false;
    };
    c.part(FourCC::DXIL)
        .and_then(|part| victory_shader::native_programs(part.data).ok())
        .is_some_and(|programs| programs.iter().any(victory_shader::spill::is_spilled))
}

/// Whether a compute container is one of the title's `bvh_skinning`
/// programs: they pose every animated mesh (and its motion) once more, only
/// for the ray-tracing structures. With every ray a miss
/// (`VICTORY_RT_MISS=1`) and the structure builder off, nothing reads what
/// they write, and they were the largest share of the card's time in a
/// cutscene (~6 ms a dispatch, hundreds a frame).
///
/// The container holds its entry point's name, not the effect's: all 30
/// entry points named `CSComputeSkinning*` in the title's shader file are
/// `bvh_skinning`'s; the picture's own skinning is `CS_PedStandardSkinning*`,
/// `CS_PosOnlyLodSkinning*` and the cloth's `computeclothSkinning`.
///
/// Two more kinds only serve rays (after codec-d/cool_fox `make.rs`):
/// `CS_RaytracingResolve*` shades what a ray hit (some 1,000 programs) and
/// `CS_ScreenProbes_ActivateRays*` picks the screen probes that cast rays.
///
/// And the tracing itself against the structures, with the hair's geometry
/// (8 % of the card's time in the opening cutscene): the screen probes'
/// `CS_ScreenProbes_TraceRays*_BVH*` and the reflections'
/// `CS_RaytraceVBuffer*`. Their `Fallback` and `Voxels` forms march through
/// the title's voxels, not the structures, and light the picture: they run.
///
/// The `bvh_skinning_smh` program `CSGenerateHairVB` makes the hair's geometry: once taken
/// for the structures only, it also feeds the hair's shadow tiles, so it runs
/// ([`RayOnly::HairVb`]).
#[cfg(windows)]
fn for_rays(bytes: &[u8]) -> RayOnly {
    let has = |name: &[u8]| bytes.windows(name.len()).any(|w| w == name);
    if has(b"CSComputeSkinning") {
        RayOnly::Skin
    } else if has(b"CSGenerateHairVB") {
        RayOnly::HairVb
    } else if has(b"CS_RaytracingResolve") {
        RayOnly::Resolve
    } else if has(b"CS_ScreenProbes_ActivateRays") {
        RayOnly::Activate
    } else if (has(b"CS_ScreenProbes_TraceRays") && has(b"_BVH")) || (has(b"CS_RaytraceVBuffer") && !has(b"Voxels")) {
        RayOnly::Trace
    } else if has(b"CS_GenerateShadowMasks_Raytraced")
        || has(b"CS_RTShadowTrace")
        || has(b"CS_RaytraceShadows")
        || has(b"CS_TraceShadowRays")
    {
        RayOnly::ShadowMask
    } else if has(b"CS_TransmissionVolumetrics_Tiled_") {
        // (`_Tiled_<digits>` and `_Tiled_LocalLights_<digits>`; not `_Temporal_Tiled` nor
        // `_GenerateTiles`.)
        RayOnly::WaterLight
    } else {
        RayOnly::No
    }
}

/// What a compute pipeline does only for rays ([`for_rays`]).
#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RayOnly {
    No,
    Skin,
    HairVb,
    Resolve,
    Activate,
    Trace,
    ShadowMask,
    /// Not for rays: the light in the water's volume, in tiles
    /// (`water_shading:CS_TransmissionVolumetrics_Tiled` and `_Tiled_LocalLights`), which
    /// can be left out on hosts of 32-lane waves ([`skips_rt_skin`]).
    WaterLight,
}

#[cfg(all(test, windows))]
#[test]
fn ray_programs_are_known_by_name() {
    assert_eq!(for_rays(b"..\0CSComputeSkinningRigidVelocity_1c34db0a_Wrapped\0.."), RayOnly::Skin);
    assert_eq!(for_rays(b"..\0CS_PedStandardSkinning_3_00aa11bb_Wrapped\0.."), RayOnly::No);
    assert_eq!(for_rays(b"computeclothSkinning"), RayOnly::No);
    assert_eq!(for_rays(b"..\0CS_RaytracingResolve_Opaque_12345678_Wrapped\0.."), RayOnly::Resolve);
    assert_eq!(for_rays(b"..\0CS_ScreenProbes_ActivateRays_0_Wrapped\0.."), RayOnly::Activate);
    assert_eq!(for_rays(b"..\0CSGenerateHairVB_c88a8f19_Wrapped\0.."), RayOnly::HairVb);
    assert_eq!(for_rays(b"..\0CS_ScreenProbes_TraceRays_RayBinning_BVH_Mixed\0.."), RayOnly::Trace);
    assert_eq!(for_rays(b"..\0CS_ScreenProbes_TraceRays_Voxels\0.."), RayOnly::No);
    assert_eq!(for_rays(b"..\0CS_ScreenProbes_TraceRays_Fallback\0.."), RayOnly::No);
    assert_eq!(for_rays(b"..\0CS_RaytraceVBuffer_ShaderBinning_WithFar\0.."), RayOnly::Trace);
    assert_eq!(for_rays(b"..\0CS_RaytraceVBuffer_Voxels_RayBinning\0.."), RayOnly::No);
    assert_eq!(for_rays(b"..\0CS_TraceShadowRays_FullAdaptive\0.."), RayOnly::ShadowMask);
    assert_eq!(for_rays(b"..\0CS_TransmissionVolumetrics_Tiled_5dc3b259_Wrapped\0.."), RayOnly::WaterLight);
    assert_eq!(for_rays(b"..\0CS_TransmissionVolumetrics_Tiled_LocalLights_6f225b64_Wrapped\0.."), RayOnly::WaterLight);
    assert_eq!(for_rays(b"..\0CS_TransmissionVolumetrics_Temporal_Tiled_c7fd991b_Wrapped\0.."), RayOnly::No);
    assert_eq!(for_rays(b"..\0CS_TransmissionVolumetrics_GenerateTiles_1aa247ed_Wrapped\0.."), RayOnly::No);
}

/// Whether a dispatch of `this` is left out because it only serves rays,
/// and nothing they could hit is there: every ray is a miss
/// (`VICTORY_RT_MISS=1`) and the structure builder is off. Each kind runs
/// anyway with its switch: `VICTORY_RT_SKIN=1`, `VICTORY_RT_RESOLVE=1`, `VICTORY_RT_TRACE=1`,
/// `VICTORY_ACTIVATE_RAYS=1`.
///
/// The same four kinds (not the hair's geometry, the shadow masks or the
/// water's light) are left out with ray tracing asked for
/// (`VICTORY_BVH_BUILDER=1`, rays not all misses) while the builder is off
/// at its last look ([`builder_known_off`]): they would trace structures that
/// were never built. Read at every dispatch, so they run again once the
/// builder does, and before its first look they run. Their switches run
/// them anyway, and `VICTORY_RT_FOLLOW_BUILDER=0` runs them all as before.
#[cfg(windows)]
pub(crate) fn skips_rt_skin(this: This) -> bool {
    fn off(switch: &str) -> bool {
        victory_shader::emit::rt_miss()
            && std::env::var_os("VICTORY_BVH_BUILDER").is_none_or(|v| v != "1")
            && std::env::var_os(switch).is_none_or(|v| v != "1")
    }
    /// Whether the kind of `switch` follows the builder.
    fn follows(switch: &str) -> bool {
        builder_asked()
            && !victory_shader::emit::rt_miss()
            && std::env::var_os("VICTORY_RT_FOLLOW_BUILDER").is_none_or(|v| v != "0")
            && std::env::var_os(switch).is_none_or(|v| v != "1")
    }
    static FOLLOWS: [OnceLock<bool>; 4] = [const { OnceLock::new() }; 4];
    let follow = |i: usize, switch: &str| *FOLLOWS[i].get_or_init(|| follows(switch)) && builder_known_off();
    static SKIN: OnceLock<bool> = OnceLock::new();
    static HAIR_VB: OnceLock<bool> = OnceLock::new();
    static RESOLVE: OnceLock<bool> = OnceLock::new();
    static ACTIVATE: OnceLock<bool> = OnceLock::new();
    static TRACE: OnceLock<bool> = OnceLock::new();
    static SHADOW: OnceLock<bool> = OnceLock::new();
    static WATER: OnceLock<bool> = OnceLock::new();
    if this.is_null() {
        return false;
    }
    match pipeline(this).rt_only {
        RayOnly::No => false,
        RayOnly::Skin => *SKIN.get_or_init(|| off("VICTORY_RT_SKIN")) || follow(0, "VICTORY_RT_SKIN"),
        // The hair's geometry is also what the sun's shadow mask reads for the tiles with hair
        // (`CS_GenerateShadowMasks_HairOnly`): left out, those tiles kept stale, lit values over
        // the hair (blocky orange hair in the prison window, run prisao-raios; right with it, run
        // prisao-pele). It runs unless `VICTORY_RT_HAIR_VB=0`.
        RayOnly::HairVb => *HAIR_VB.get_or_init(|| std::env::var_os("VICTORY_RT_HAIR_VB").is_some_and(|v| v == "0")),
        RayOnly::Resolve => *RESOLVE.get_or_init(|| off("VICTORY_RT_RESOLVE")) || follow(1, "VICTORY_RT_RESOLVE"),
        RayOnly::Activate => *ACTIVATE.get_or_init(|| off("VICTORY_ACTIVATE_RAYS")) || follow(2, "VICTORY_ACTIVATE_RAYS"),
        RayOnly::Trace => *TRACE.get_or_init(|| off("VICTORY_RT_TRACE")) || follow(3, "VICTORY_RT_TRACE"),
        // (opt-in while it is tried: `VICTORY_SKIP_RT_SHADOW=1`)
        RayOnly::ShadowMask => *SHADOW.get_or_init(|| {
            std::env::var_os("VICTORY_SKIP_RT_SHADOW").is_some_and(|v| v == "1") && off("VICTORY_RT_SHADOW")
        }),
        // Programs of one 64-lane wave a group, which a host of 32-lane waves gets in the
        // wave-wide form only. On an RTX 5070 at the Series X profile they leave whole tiles
        // of open water dark and take about 15 ms of the card a frame (trustno1-coder/VIctory
        // 35c15608: 25 fps with them, 30 without), and that tree leaves them out. Here (RTX
        // 4060 Ti, Series S profile, the first boat ride, runs barco-s-agua0 and barco-s-agua1
        // of 27 Nov) they cost half a frame a second (23.8 fps with them, 24.3 without) and
        // without them the water near the boat is black instead of lit green, so they run;
        // `VICTORY_WATER_LIGHT=0` leaves them out on a host of 32-lane waves.
        RayOnly::WaterLight => *WATER.get_or_init(|| {
            victory_shader::emit::host_settings()[0] == 32 && std::env::var_os("VICTORY_WATER_LIGHT").is_some_and(|v| v == "0")
        }),
    }
}

/// `VICTORY_BVH_QUARANTINE=1`: what the structure builder's stages store
/// stays in the host's copies and is not copied back into title memory. A
/// check for a first run of the whole builder: a stage that stores where
/// it should not can then not reach the title's own objects (run 98), and
/// the programs that walk the structures read the host's copies anyway.
/// shortcut: not a way to run: the title also reads results back (sizes,
/// counters) and gets none, and a copy's pages are overwritten from title
/// memory when the title writes them.
#[cfg(windows)]
pub(crate) fn builder_quarantined(this: This) -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    is_builder(this) && *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_QUARANTINE").is_some_and(|v| v == "1"))
}

/// Whether the title's pipeline `this` is one of the structure builder's.
#[cfg(windows)]
pub(crate) fn is_builder(this: This) -> bool {
    !this.is_null() && pipeline(this).builder
}

/// A stage of the builder was dropped after all: none runs for the rest of
/// this frame and the next (what it fed is not there).
#[cfg(windows)]
pub(crate) fn builder_rests(why: &'static str) {
    let mut b = BUILDER.lock().unwrap_or_else(|e| e.into_inner());
    b.rests = crate::frame::current() + 2;
    static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
        tracing::warn!(why, "the ray-tracing structure builder rests for this frame and the next: its stages run all or none");
    }
}

/// At a dispatch of a builder pipeline: whether the builder runs (see
/// [`Builder`]).
#[cfg(windows)]
pub(crate) fn builder_runs() -> bool {
    if !builder_asked() {
        return false;
    }
    let mut b = BUILDER.lock().unwrap_or_else(|e| e.into_inner());
    if !b.whole && builder_ahead() {
        // Those not made yet go to the workers first, side by side: this
        // thread then waits for the slowest instead of making each in turn.
        for p in &b.pipelines {
            make::ahead(p);
        }
    }
    let retry = retry();
    let runs = b.look(
        crate::frame::current(),
        retry,
        |p| {
            // Without the retry, made on the spot (or waited for): never "not yet".
            let made = if retry { make::made(p) } else { Some(make::now(p)) };
            match made {
                Some(true) => Made::Yes,
                Some(false) => Made::Refused,
                None => Made::NotYet,
            }
        },
        make::hashes,
    );
    publish(&b);
    runs
}

/// Publishes the builder's decision ([`BUILDER_STATE`]), and says when it
/// changes.
#[cfg(windows)]
fn publish(b: &Builder) {
    let was = BUILDER_STATE.swap(b.state, std::sync::atomic::Ordering::Relaxed);
    if was != b.state {
        let name = |s| match s {
            RUNS => "runs",
            OFF => "off",
            _ => "not looked at",
        };
        // (What `skips_rt_skin` does with it, for the four kinds that follow.)
        let follow = !victory_shader::emit::rt_miss()
            && std::env::var_os("VICTORY_RT_FOLLOW_BUILDER").is_none_or(|v| v != "0");
        tracing::info!(
            was = name(was),
            now = name(b.state),
            ray_passes = if !follow { "run (VICTORY_RT_FOLLOW_BUILDER=0 or VICTORY_RT_MISS=1)" } else if b.state == OFF { "left out" } else { "run" },
            "the ray-tracing structure builder's decision changed (skinning, traces, resolves and ray activation for the structures follow it)"
        );
    }
}

/// Whether a console program can store to buffer memory: it has a buffer,
/// typed-buffer, scalar-memory, flat or global store or atomic, or code
/// that is not all known (an instruction of those kinds outside our table,
/// an indirect jump, code that does not decode). Stores to textures
/// (image instructions) and to the program's own scratch memory do not
/// count: neither reaches a buffer's host copy. After upstream #413,
/// which leaves scalar-memory stores out and counts image stores.
#[cfg(windows)]
fn stores(program: &victory_shader::NativeProgram) -> bool {
    use victory_shader::gfx10::{Encoding, decode};
    let Ok(code) = decode(&program.code) else {
        return true;
    };
    code.iter().any(|i| {
        let name = i.name();
        match i.encoding {
            Encoding::Mubuf
            | Encoding::Mtbuf
            | Encoding::Smem
            | Encoding::Flat
            | Encoding::Global => name.is_none_or(|n| {
                (n.contains("store") || n.contains("atomic")) && !n.contains("scratch")
            }),
            Encoding::Sop1 => name.is_none_or(|n| matches!(n, "s_setpc_b64" | "s_swappc_b64")),
            _ => false,
        }
    })
}

/// Whether a console program can store to memory through raw addresses
/// (`crate::memory`): a flat or global store or atomic, or code that does
/// not decode. The programs that only walk ray-tracing structures do not,
/// whatever they store through views.
#[cfg(windows)]
fn stores_by_address(program: &victory_shader::NativeProgram) -> bool {
    use victory_shader::gfx10::{Encoding, decode};
    let Ok(code) = decode(&program.code) else {
        return true;
    };
    code.iter().any(|i| {
        matches!(i.encoding, Encoding::Flat | Encoding::Global)
            && i.name()
                .is_none_or(|n| (n.contains("store") || n.contains("atomic")) && !n.contains("scratch"))
    })
}

#[repr(C)]
struct Pipeline {
    object: Object,
    packet: [u32; 8],
    /// Each used stage's container.
    shaders: Vec<Arc<Shader>>,
    /// Whether a list has bound it yet (its shaders are logged then).
    bound: std::sync::atomic::AtomicBool,
    /// What its host pipeline is made from, and the host pipeline once made
    /// (by a worker: the title may release the pipeline meanwhile).
    #[cfg(windows)]
    host: Arc<make::Pending>,
    /// One of the ray-tracing structure builder's ([`Builder`]).
    #[cfg(windows)]
    builder: bool,
    /// One of the title's passes that only serve rays
    /// ([`for_rays`]).
    #[cfg(windows)]
    rt_only: RayOnly,
}
const _: () = assert!(offset_of!(Pipeline, packet) == 0x10);

/// Stage names, in description order.
const GRAPHICS_STAGES: [&str; 5] = ["vertex", "pixel", "domain", "hull", "geometry"];

fn pipeline<'a>(this: This) -> &'a Pipeline {
    // SAFETY: the title passes one of our pipelines.
    unsafe { &*this.cast::<Pipeline>() }
}

/// List method: logs each pipeline's shaders the first time a list binds
/// it, so a run lists the shaders its frames need first.
pub(crate) fn bound(this: This) {
    if this.is_null() {
        return;
    }
    let p = pipeline(this);
    if p.bound.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let frame = crate::frame::current();
    for shader in &p.shaders {
        tracing::info!(pipeline = ?this, frame, stage = shader.stage, size = shader.container.len(), hash = %shader.hash, "first bind");
    }
}

/// The host pipeline of a title pipeline; `None` if one of its shaders does
/// not translate, the host refuses it, or it is still being made (its first
/// use starts that, see [`make`]).
#[cfg(windows)]
pub(crate) fn host_pipeline<'a>(this: This) -> Option<&'a host::HostPipeline> {
    if this.is_null() {
        return None;
    }
    make::ready(&pipeline(this).host)
}

/// Whether one of the containers of the title pipeline `this` is one `f`
/// picks (false for none bound). Known at the call, before any host
/// pipeline is made.
#[cfg(windows)]
pub(crate) fn shaders_any(this: This, f: impl Fn(&Shader) -> bool) -> bool {
    !this.is_null() && pipeline(this).shaders.iter().any(|s| f(s))
}

/// Host pipelines queued and not made yet.
#[cfg(windows)]
pub(crate) fn making() -> u64 {
    make::waiting()
}

/// Draws and dispatches dropped since the last [`take_drops`], by reason
/// (diagnostics).
#[cfg(all(windows, feature = "diagnostics"))]
static DROPS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

/// Shader hashes as printed in the pipeline diagnostics.
#[cfg(all(windows, feature = "diagnostics"))]
fn parse_skip_hashes(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .filter_map(|hash| {
            if hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Some(hash.to_ascii_lowercase())
            } else {
                tracing::warn!(
                    hash,
                    "VICTORY_SKIP_PIPELINES expects 16-digit shader hashes"
                );
                None
            }
        })
        .collect()
}

#[cfg(all(windows, feature = "diagnostics"))]
fn skipped_shader(p: &Pipeline) -> Option<&str> {
    skipped_in(&p.shaders)
}

/// Whether one of `shaders` is in `VICTORY_SKIP_PIPELINES`: its pipelines are not made at
/// all (a few hang the driver's compiler and would hold the make queue).
/// The list is a diagnostics switch, as the dropping of their draws is: a plain build makes them.
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn creates_skipped(shaders: &[Arc<Shader>]) -> bool {
    skipped_in(shaders).is_some()
}

#[cfg(not(all(windows, feature = "diagnostics")))]
pub(crate) fn creates_skipped(_: &[Arc<Shader>]) -> bool {
    false
}

#[cfg(all(windows, feature = "diagnostics"))]
fn skipped_in(shaders: &[Arc<Shader>]) -> Option<&str> {
    static HASHES: OnceLock<Vec<String>> = OnceLock::new();
    let hashes = HASHES.get_or_init(|| {
        let value = std::env::var("VICTORY_SKIP_PIPELINES").unwrap_or_default();
        parse_skip_hashes(&value)
    });
    if hashes.is_empty() {
        return None;
    }
    shaders
        .iter()
        .find(|shader| hashes.contains(&shader.hash))
        .map(|shader| shader.hash.as_str())
}

#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn is_skipped(this: This) -> bool {
    !this.is_null() && skipped_shader(pipeline(this)).is_some()
}

/// Counts a draw or dispatch of `this` intentionally skipped or dropped
/// for want of a host pipeline (diagnostics).
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn note_drop(this: This) {
    let why = if this.is_null() {
        Some("no pipeline set".to_owned())
    } else {
        let p = pipeline(this);
        skipped_shader(p)
            .map(|hash| format!("skipped shader {hash}"))
            .or_else(|| make::missing(&p.host))
    };
    if let Some(why) = why {
        note_why(why);
    }
}

/// Counts a draw or dispatch dropped for `why` (diagnostics).
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn note_why(why: String) {
    crate::capture::op(0, || format!("DROPPED {why}"));
    let mut drops = DROPS.lock().unwrap_or_else(|e| e.into_inner());
    *drops.get_or_insert_with(HashMap::new).entry(why).or_default() += 1;
}

/// The drops counted since the last call, most first (diagnostics).
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn take_drops() -> Vec<(String, u64)> {
    let drops = DROPS.lock().unwrap_or_else(|e| e.into_inner()).take();
    let mut drops: Vec<(String, u64)> = drops.into_iter().flatten().collect();
    // Frame reports only show the first 40 reasons. Keep ties ordered so
    // a HashMap iteration order change cannot change which reasons appear.
    drops.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    drops
}

/// The stage's container, if it has one.
fn copy(stage: &'static str, code: &Bytecode) -> Option<Arc<Shader>> {
    if code.code.is_null() || code.size == 0 {
        return None;
    }
    // SAFETY: the title passes `size` bytes of shader code.
    Some(shader(stage, unsafe {
        std::slice::from_raw_parts(code.code, code.size)
    }))
}

fn hand_out(
    shaders: Vec<Arc<Shader>>,
    #[cfg(windows)] graphics: Option<Box<host::GraphicsState>>,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    tracing::trace!(?shaders, "pipeline state");
    #[cfg(windows)]
    let host = Arc::new(make::Pending::new(shaders.clone(), graphics));
    #[cfg(windows)]
    known::note(host.key(), &shaders, host.graphics.as_deref());
    #[cfg(windows)]
    make::early(&host);
    #[cfg(windows)]
    let builder = of_builder(&shaders);
    #[cfg(windows)]
    if builder {
        // Another stage: the group is looked at again at its next dispatch.
        let mut b = BUILDER.lock().unwrap_or_else(|e| e.into_inner());
        b.pipelines.push(host.clone());
        b.whole = false;
        drop(b);
        // The title creates them long before its first build, which needs
        // every one of them in the same frame.
        if builder_ahead() {
            make::ahead(&host);
        }
    }
    let pipeline = Box::new(Pipeline {
        object: Object::new(&FUNCTIONS),
        packet: [0; 8],
        #[cfg(windows)]
        host,
        #[cfg(windows)]
        builder,
        #[cfg(windows)]
        rt_only: shaders.iter().filter(|s| s.stage == "compute").map(|s| for_rays(&s.container)).find(|k| *k != RayOnly::No).unwrap_or(RayOnly::No),
        shaders,
        bound: std::sync::atomic::AtomicBool::new(false),
    });
    object::hand_out(PIPELINE, pipeline, iid, &PIPELINE_IIDS, out)
}

/// Device method.
pub(crate) extern "system" fn create_graphics(
    _: This,
    desc: *const GraphicsHead,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    let stages = GRAPHICS_STAGES.iter().zip(&desc.stages);
    let shaders = stages.filter_map(|(s, c)| copy(s, c)).collect();
    hand_out(
        shaders,
        #[cfg(windows)]
        None,
        iid,
        out,
    )
}

/// Device method.
pub(crate) extern "system" fn create_compute(
    _: This,
    desc: *const ComputeHead,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    hand_out(
        copy("compute", &desc.stage).into_iter().collect(),
        #[cfg(windows)]
        None,
        iid,
        out,
    )
}

/// Device method 0x1f0: the plain compute description plus console
/// extensions `(desc, extension count, extensions, iid, out)`.
/// shortcut: the extensions (console shader settings) are ignored, as is
/// the rest of the description.
pub(crate) extern "system" fn create_compute_x(
    this: This,
    desc: *const ComputeHead,
    _extensions: u32,
    _extended: *const u8,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    create_compute(this, desc, iid, out)
}

/// A pipeline described as a stream: its size and the subobjects, each a
/// type then its data, padded to 8 bytes.
#[repr(C)]
pub(crate) struct StreamDesc {
    size: usize,
    stream: *const u8,
}

/// A subobject type's size (type included), and the stage it is if it is a
/// shader. The console numbers amplification and mesh shaders 23 and 24
/// (PC: 24 and 25); its own types from 25 on are not sized yet.
fn subobject(kind: u32) -> Option<(usize, Option<&'static str>)> {
    let stage = |name| Some((24, Some(name)));
    match kind {
        1 => stage("vertex"),
        2 => stage("pixel"),
        3 => stage("domain"),
        4 => stage("hull"),
        5 => stage("geometry"),
        6 => stage("compute"),
        23 => stage("amplification"),
        24 => stage("mesh"),
        0 => Some((16, None)),
        12 | 19 => Some((24, None)),
        7 | 15 => Some((40, None)),
        8 => Some((336, None)),
        10 => Some((48, None)),
        11 => Some((56, None)),
        17 => Some((16, None)),
        21 => Some((64, None)),
        22 => Some((32, None)),
        9 | 13 | 14 | 16 | 18 | 20 => Some((8, None)),
        _ => None,
    }
}

/// Device method: a pipeline from a stream; the shaders are kept, and the
/// graphics state for the host pipeline.
pub(crate) extern "system" fn create_stream(
    _: This,
    desc: *const StreamDesc,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    if desc.stream.is_null() {
        return E_INVALIDARG;
    }
    let mut shaders = Vec::new();
    #[cfg(windows)]
    let mut graphics = host::GraphicsState::default();
    let mut at = 0;
    while at + 8 <= desc.size {
        // SAFETY: inside the stream the title passes.
        let kind = unsafe { desc.stream.add(at).cast::<u32>().read_unaligned() };
        let Some((size, stage)) = subobject(kind) else {
            tracing::warn!(
                kind,
                at,
                "pipeline subobject of unknown size; the rest is ignored"
            );
            break;
        };
        if let Some(stage) = stage {
            // SAFETY: a shader subobject has its bytecode at +8.
            let code = unsafe { &*desc.stream.add(at + 8).cast::<Bytecode>() };
            shaders.extend(copy(stage, code));
        }
        #[cfg(windows)]
        // SAFETY: a whole subobject of this kind lies at `at`.
        unsafe {
            graphics.read(kind, desc.stream.add(at))
        };
        at += size;
    }
    #[cfg(windows)]
    host::adjust_light_volume(&shaders, &mut graphics);
    hand_out(
        shaders,
        #[cfg(windows)]
        Some(Box::new(graphics)),
        iid,
        out,
    )
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = object::is(iid, &PIPELINE_IIDS).then_some(this);
    object::answer(PIPELINE, iid, found, out)
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Pipeline>(this)
}

/// IUnknown 3, object 4, device child 1, pipeline 3.
static FUNCTIONS: Table<24> = {
    let mut f = object::d3d12_object(object::missing_table::<PIPELINE, 24>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    Table(f)
};

#[cfg(windows)]
pub(crate) mod host;
#[cfg(windows)]
pub mod known;
#[cfg(windows)]
mod library;
#[cfg(windows)]
mod make;

/// Lets the pipeline library's mapped file go from memory when no pipeline
/// has loaded for a while (`library::trim`); called by the host's sweep.
#[cfg(windows)]
pub(crate) fn trim_library() {
    library::trim();
}

/// The pipeline library's part of the report when the host device is
/// removed (`library::removal_note`).
#[cfg(windows)]
pub(crate) fn library_removal_note() -> String {
    library::removal_note()
}

/// Keeps compiled host pipelines, and the list of title pipelines to start
/// early, in `dir` (a folder of the cache) across runs.
#[cfg(windows)]
pub fn use_pipeline_cache(dir: std::path::PathBuf) {
    library::use_pipeline_cache(dir);
}

/// Saves the pipeline cache now if anything was added (at the end of a run).
#[cfg(windows)]
pub fn save_pipeline_cache() {
    library::save(true);
}

#[cfg(test)]
mod tests {
    #[cfg(all(windows, feature = "diagnostics"))]
    #[test]
    fn skip_hashes_accepts_full_hashes_only() {
        assert!(super::parse_skip_hashes(" , ").is_empty());
        assert_eq!(
            super::parse_skip_hashes(" 0123456789ABCDEF, 0123456789abcde, xyz, 0011223344556677,"),
            ["0123456789abcdef", "0011223344556677"]
        );
    }

    use super::*;
    use crate::object::{S_OK, slot};

    fn shaders(p: This) -> Vec<(&'static str, Vec<u8>)> {
        pipeline(p)
            .shaders
            .iter()
            .map(|s| (s.stage, s.container.clone()))
            .collect()
    }

    #[test]
    fn pipelines_keep_their_shaders() {
        let (vs, ps) = ([1u8, 2, 3], [4u8; 5]);
        let none = || Bytecode {
            code: std::ptr::null(),
            size: 0,
        };
        let desc = GraphicsHead {
            root: std::ptr::null(),
            stages: [
                Bytecode {
                    code: vs.as_ptr(),
                    size: vs.len(),
                },
                Bytecode {
                    code: ps.as_ptr(),
                    size: ps.len(),
                },
                none(),
                none(),
                none(),
            ],
        };
        let mut p = std::ptr::null();
        assert_eq!(
            create_graphics(std::ptr::null(), &desc, &PIPELINE_IIDS[0], &mut p),
            S_OK
        );
        assert_eq!(
            shaders(p),
            [("vertex", vs.to_vec()), ("pixel", ps.to_vec())]
        );
        // A second pipeline with the same pixel shader shares its container.
        let mut q = std::ptr::null();
        create_graphics(std::ptr::null(), &desc, &PIPELINE_IIDS[0], &mut q);
        assert!(Arc::ptr_eq(
            &pipeline(p).shaders[1],
            &pipeline(q).shaders[1]
        ));
        assert_eq!(slot::<extern "system" fn(This) -> u32>(p, 0x10)(p), 0);
    }

    /// The stream layout the title builds (title+0x86f090): root
    /// signature, five shader slots (the first a mesh shader), blend, sample
    /// mask, rasterizer, depth-stencil 1, sample description, input layout,
    /// topology, render target and depth formats: 0x2b0 bytes.
    #[test]
    fn streams_yield_their_shaders() {
        let (ms, ps) = ([9u8; 7], [4u8; 5]);
        let mut stream = vec![0u8; 0x2b0];
        let mut put = |at: usize, v: u64| stream[at..at + 8].copy_from_slice(&v.to_le_bytes());
        for (at, kind) in [
            (0x10, 24),
            (0x28, 2),
            (0x40, 3),
            (0x58, 4),
            (0x70, 5),
            (0x88, 8),
            (0x1d8, 9),
            (0x1e0, 10),
            (0x210, 21),
            (0x250, 17),
            (0x260, 12),
            (0x278, 14),
            (0x280, 15),
            (0x2a8, 16),
        ] {
            put(at, kind);
        }
        put(0x18, ms.as_ptr() as u64);
        put(0x20, ms.len() as u64);
        put(0x30, ps.as_ptr() as u64);
        put(0x38, ps.len() as u64);
        // Topology type 3 (triangles), one render target of format 28.
        put(0x278, 14 | 3 << 32);
        put(0x280, 15 | 28 << 32);
        put(0x2a0, 1 << 32);
        let desc = StreamDesc {
            size: stream.len(),
            stream: stream.as_ptr(),
        };
        let mut p = std::ptr::null();
        let hr = create_stream(std::ptr::null(), &desc, &PIPELINE_IIDS[0], &mut p);
        assert_eq!(hr, S_OK);
        assert_eq!(shaders(p), [("mesh", ms.to_vec()), ("pixel", ps.to_vec())]);
        #[cfg(windows)]
        {
            let g = pipeline(p).host.graphics.as_deref().unwrap();
            assert_eq!(g.topology.0, 3);
            assert_eq!(
                (g.targets.NumRenderTargets, g.targets.RTFormats[0].0),
                (1, 28)
            );
        }
    }
}

/// Whether a container is a pixel program of the title's decoration bake,
/// by its name inside (`PS_DecorationBlend_*`, `PS_BakeDecorations_*`: 51
/// of them). A list of four hashes ([`crate::draw`]'s) missed the one the
/// cutscene's faces are baked with (`PS_DecorationBlend_0011`,
/// fcee612943c008e0): its sources were not on the card and the face came
/// out black.
#[cfg(windows)]
fn bakes(bytes: &[u8]) -> bool {
    [&b"PS_DecorationBlend_"[..], b"PS_BakeDecorations_"]
        .iter()
        .any(|name| bytes.windows(name.len()).any(|w| w == *name))
}

#[cfg(all(test, windows))]
#[test]
fn bake_programs_are_known_by_name() {
    assert!(bakes(b"..\0PS_DecorationBlend_0011_ff00f5de_Wrapped\0.."));
    assert!(bakes(b"PS_BakeDecorations_0100_583f45fe_Wrapped"));
    assert!(!bakes(b"PS_PedSkin_0001"));
}
