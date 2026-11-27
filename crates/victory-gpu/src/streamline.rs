//! NVIDIA Streamline (DLSS Super Resolution, DLSS Frame Generation, Reflex),
//! loaded at run time from a folder of the user's; everything off by
//! default, and with every switch off nothing here is loaded or called.
//!
//! - `VICTORY_REFLEX=on` (or `1`) or `boost`: Reflex low latency by itself.
//! - `VICTORY_FRAMEGEN=1`: DLSS Frame Generation (2x) on our present.
//! - `VICTORY_DLSS=1` (or `auto`, or a mode: `dlaa`, `quality`, `balanced`,
//!   `performance`, `ultraperformance`): DLSS Super Resolution in place of
//!   the title's FidelityFX upscaling.
//! - `VICTORY_STREAMLINE_DIR=<folder>`: where `sl.interposer.dll`, its
//!   plugins and NGX's DLLs are; else `streamline` beside our executable, else
//!   our executable's folder ([`plan::candidate_dirs`]).
//! - `VICTORY_STREAMLINE_LOG=<folder>`: Streamline's own log files there.
//!
//! No NVIDIA file is part of this repository. Anything missing (the folder,
//! a signature, a feature on this card, the device hand-over) is said in one
//! line of the log, and the run goes on exactly as without the switches.
//!
//! Streamline is used with manual hooking: nothing of D3D12 or DXGI is
//! hooked globally; only the objects handed to `slUpgradeInterface` are
//! proxies.

#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub(crate) mod abi;
pub mod plan;

pub use plan::{Dlss, Mode, Reflex};

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

/// The project id NGX is given (engine "custom", no NVIDIA application id).
const PROJECT_ID: &CStr = c"6f0d7b2e-3a41-4c1e-9b57-2d8e4f1a9c30";
const ENGINE_VERSION: &CStr = c"VIctory 0.1";

macro_rules! once {
    ($level:ident, $($arg:tt)*) => {{
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::$level!($($arg)*));
    }};
}

/// What the user asked for, read from the environment once.
#[derive(Clone, Debug)]
pub struct Asked {
    pub dlss: Dlss,
    pub framegen: bool,
    pub reflex: Option<Reflex>,
    /// `VICTORY_DLSS_PRESET`: the DLSS model (`sl::DLSSPreset`).
    pub preset: u32,
    /// `VICTORY_DLSS_JITTER` and `VICTORY_DLSS_MV`: sign or scale fixes.
    pub jitter: [f32; 2],
    pub mvec: [f32; 2],
    /// `VICTORY_DLSS_REACTIVE=0`: the reactive mask is not given to DLSS.
    pub reactive: bool,
    pub dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
}

impl Asked {
    /// The switches; an unknown word is an error.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok();
        let preset = match var("VICTORY_DLSS_PRESET") {
            Some(name) => plan::preset(&name).ok_or_else(|| {
                format!("VICTORY_DLSS_PRESET={name}: not one of default, e, f, j, k, l, m")
            })?,
            None => 0,
        };
        let folder = |name: &str| {
            var(name)
                .filter(|v| !v.trim().is_empty() && v.trim() != "0")
                .map(PathBuf::from)
        };
        Ok(Asked {
            dlss: Dlss::from_switch(var("VICTORY_DLSS").as_deref())?,
            framegen: var("VICTORY_FRAMEGEN").is_some_and(|v| v.trim() == "1"),
            reflex: Reflex::from_switch(var("VICTORY_REFLEX").as_deref())?,
            preset,
            jitter: plan::pair(var("VICTORY_DLSS_JITTER").as_deref()),
            mvec: plan::pair(var("VICTORY_DLSS_MV").as_deref()),
            reactive: var("VICTORY_DLSS_REACTIVE").is_none_or(|v| v.trim() != "0"),
            dir: folder("VICTORY_STREAMLINE_DIR"),
            log_dir: folder("VICTORY_STREAMLINE_LOG"),
        })
    }

    pub fn any(&self) -> bool {
        self.dlss.on() || self.framegen || self.reflex.is_some()
    }
}

/// What [`init`] made available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Available {
    pub dlss: bool,
    pub framegen: bool,
    pub reflex: bool,
}

struct Library(windows_sys::Win32::Foundation::HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this owns the reference LoadLibraryExW took.
        unsafe { windows_sys::Win32::Foundation::FreeLibrary(self.0) };
    }
}

// SAFETY: a module handle.
unsafe impl Send for Library {}
// SAFETY: as above.
unsafe impl Sync for Library {}

/// The library's exported functions.
struct Sl {
    _library: Library,
    init: abi::SlInit,
    is_feature_supported: abi::SlIsFeatureSupported,
    evaluate_feature: abi::SlEvaluateFeature,
    free_resources: abi::SlFreeResources,
    set_tag_for_frame: abi::SlSetTagForFrame,
    get_feature_requirements: abi::SlGetFeatureRequirements,
    get_feature_version: abi::SlGetFeatureVersion,
    upgrade_interface: abi::SlUpgradeInterface,
    set_constants: abi::SlSetConstants,
    get_native_interface: abi::SlGetNativeInterface,
    get_feature_function: abi::SlGetFeatureFunction,
    get_new_frame_token: abi::SlGetNewFrameToken,
    set_d3d_device: abi::SlSetD3DDevice,
}

static SL: OnceLock<Sl> = OnceLock::new();

/// What is on once the library is in: the switches and the features found.
struct Plan {
    asked: Asked,
    available: Available,
}

static PLAN: OnceLock<Plan> = OnceLock::new();
/// [`init`]'s answer, made once.
static INIT: OnceLock<Available> = OnceLock::new();

/// The per-feature functions, once the device is set.
#[derive(Default)]
struct Functions {
    dlss_optimal: Option<abi::SlDlssGetOptimalSettings>,
    dlss_set_options: Option<abi::SlDlssSetOptions>,
    dlssg_set_options: Option<abi::SlDlssgSetOptions>,
    dlssg_get_state: Option<abi::SlDlssgGetState>,
    reflex_set_options: Option<abi::SlReflexSetOptions>,
    reflex_sleep: Option<abi::SlReflexSleep>,
    reflex_get_state: Option<abi::SlReflexGetState>,
    pcl_set_marker: Option<abi::SlPclSetMarker>,
}

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();
static DLSS_READY: AtomicBool = AtomicBool::new(false);
static FG_READY: AtomicBool = AtomicBool::new(false);
static REFLEX_READY: AtomicBool = AtomicBool::new(false);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Loads Streamline and asks for what the switches name; once, before the
/// host device is made (`crate::host::host`), and before the title's
/// imports are bound when the app asks first. Fails soft: what is not there
/// is logged and left off. Nothing at all without a switch.
pub fn init() -> Available {
    *INIT.get_or_init(|| {
        let asked = match Asked::from_env() {
            Ok(asked) => asked,
            Err(why) => {
                tracing::warn!("Streamline: {why}; DLSS, frame generation and Reflex stay off");
                return Available::default();
            }
        };
        if !asked.any() {
            return Available::default();
        }
        match load_and_init(&asked) {
            Ok(available) => available,
            Err(why) => {
                tracing::warn!(
                    "Streamline: {why}; DLSS, frame generation and Reflex stay off, and everything runs as without their switches"
                );
                Available::default()
            }
        }
    })
}

/// The folder of our executable.
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

fn load_and_init(asked: &Asked) -> Result<Available, String> {
    let dirs = plan::candidate_dirs(asked.dir.as_deref(), exe_dir().as_deref());
    let Some(dir) = plan::find_dir(&dirs, Path::is_file) else {
        let looked: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        return Err(format!(
            "no {} in {} (VICTORY_STREAMLINE_DIR names another folder; the files come from NVIDIA's Streamline SDK 2.14.16, bin\\x64)",
            plan::INTERPOSER,
            looked.join(", ")
        ));
    };
    let path = std::path::absolute(dir.join(plan::INTERPOSER)).map_err(|e| e.to_string())?;
    verify_signature(&path)?;
    let sl = load(&path)?;
    let sl = SL.get_or_init(|| sl);
    let dir_wide = wide(&dir);
    let paths = [dir_wide.as_ptr()];
    let log_wide = asked.log_dir.as_deref().map(wide);
    let mut features = Vec::new();
    if asked.dlss.on() {
        features.push(abi::FEATURE_DLSS);
    }
    if asked.framegen {
        features.push(abi::FEATURE_DLSS_G);
    }
    // Frame generation needs Reflex and the latency markers too.
    if asked.framegen || asked.reflex.is_some() {
        features.extend([abi::FEATURE_REFLEX, abi::FEATURE_PCL]);
    }
    let preferences = abi::Preferences {
        base: abi::BaseStructure::new(abi::PREFERENCES, 1),
        show_console: false,
        log_level: if asked.log_dir.is_some() {
            abi::LOG_VERBOSE
        } else {
            abi::LOG_DEFAULT
        },
        paths_to_plugins: paths.as_ptr(),
        num_paths_to_plugins: 1,
        path_to_logs_and_data: log_wide.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
        allocate_callback: std::ptr::null(),
        release_callback: std::ptr::null(),
        log_message_callback: Some(log_message),
        // No downloads (OTA): only what is in the folder runs.
        flags: abi::DISABLE_CL_STATE_TRACKING
            | abi::DISABLE_DEBUG_TEXT
            | abi::USE_MANUAL_HOOKING
            | abi::USE_DXGI_FACTORY_PROXY
            | abi::USE_FRAME_BASED_RESOURCE_TAGGING,
        features_to_load: features.as_ptr(),
        num_features_to_load: features.len() as u32,
        application_id: 0,
        engine: abi::ENGINE_CUSTOM,
        engine_version: ENGINE_VERSION.as_ptr(),
        project_id: PROJECT_ID.as_ptr(),
        render_api: abi::RENDER_API_D3D12,
    };
    // SAFETY: a filled preferences structure; the library copies what it keeps.
    let code = unsafe { (sl.init)(&preferences, abi::SDK_VERSION) };
    if code != 0 {
        return Err(format!(
            "slInit failed: {} ({code})",
            abi::result_name(code)
        ));
    }
    let adapter = default_adapter();
    let luid = adapter.as_ref().map(|a| a.1);
    let supported = |feature: u32| -> Result<(), String> {
        let Some(luid) = luid else {
            return Err("no adapter".into());
        };
        let info = abi::AdapterInfo {
            base: abi::BaseStructure::new(abi::ADAPTER_INFO, 1),
            device_luid: std::ptr::from_ref(&luid).cast(),
            device_luid_size: size_of_val(&luid) as u32,
            vk_physical_device: std::ptr::null_mut(),
        };
        // SAFETY: an adapter description that lives through the call.
        let code = unsafe { (sl.is_feature_supported)(feature, &info) };
        if code == 0 {
            Ok(())
        } else {
            Err(format!("{} ({code})", abi::result_name(code)))
        }
    };
    let dlss = asked.dlss.on()
        && report(
            "DLSS Super Resolution",
            abi::FEATURE_DLSS,
            supported(abi::FEATURE_DLSS),
            sl,
        );
    let wants_reflex = asked.framegen || asked.reflex.is_some();
    let reflex = wants_reflex
        && report(
            "Reflex",
            abi::FEATURE_REFLEX,
            supported(abi::FEATURE_REFLEX),
            sl,
        )
        && report(
            "PC latency markers (PCL)",
            abi::FEATURE_PCL,
            supported(abi::FEATURE_PCL),
            sl,
        );
    let framegen = asked.framegen
        && report(
            "DLSS Frame Generation",
            abi::FEATURE_DLSS_G,
            supported(abi::FEATURE_DLSS_G),
            sl,
        )
        && reflex;
    let available = Available {
        dlss,
        framegen,
        reflex: reflex && asked.reflex.is_some(),
    };
    tracing::info!(
        library = %path.display(),
        sdk = "2.14.16",
        adapter = adapter.as_ref().map_or("?", |a| a.0.as_str()),
        dlss,
        framegen,
        reflex = asked.reflex.filter(|_| available.reflex).map_or("off", Reflex::name),
        dlss_mode = asked.dlss.mode().map_or("by the sizes", Mode::name),
        preset = asked.preset,
        "Streamline loaded (manual hooking, no downloads)"
    );
    let _ = PLAN.set(Plan {
        asked: asked.clone(),
        available,
    });
    Ok(available)
}

/// Logs whether a feature is supported, with its requirements; whether it is.
fn report(name: &str, feature: u32, supported: Result<(), String>, sl: &Sl) -> bool {
    // SAFETY: plain data; pointers null, counts zero.
    let mut requirements: abi::FeatureRequirements = unsafe { std::mem::zeroed() };
    requirements.base = abi::BaseStructure::new(abi::FEATURE_REQUIREMENTS, 2);
    // SAFETY: a structure to fill.
    let asked = unsafe { (sl.get_feature_requirements)(feature, &mut requirements) };
    let needs = (asked == 0).then(|| {
        format!(
            "OS {} (needs {}), driver {} (needs {}){}{}",
            requirements.os_version_detected,
            requirements.os_version_required,
            requirements.driver_version_detected,
            requirements.driver_version_required,
            if requirements.flags & abi::REQUIRES_HARDWARE_SCHEDULING != 0 {
                ", hardware GPU scheduling"
            } else {
                ""
            },
            if requirements.flags & abi::REQUIRES_VSYNC_OFF != 0 {
                ", v-sync off"
            } else {
                ""
            },
        )
    });
    match supported {
        Ok(()) => {
            tracing::info!(
                requirements = needs.as_deref().unwrap_or("?"),
                "Streamline: {name} is supported on this GPU"
            );
            true
        }
        Err(why) => {
            tracing::warn!(
                requirements = needs.as_deref().unwrap_or("?"),
                "Streamline: {name} is not supported here: {why}"
            );
            false
        }
    }
}

unsafe extern "C" fn log_message(kind: u32, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: a NUL-terminated message for the call.
    let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let text = text.trim_end();
    // (Every plugin in the folder that was not asked for says so.)
    let noise = text.contains("since it is was not requested by the host");
    match kind {
        abi::LOG_ERROR => tracing::error!(target: "streamline", "{text}"),
        abi::LOG_WARN if !noise => tracing::warn!(target: "streamline", "{text}"),
        _ => tracing::debug!(target: "streamline", "{text}"),
    }
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

/// The adapter a device made with no adapter named is on (the first), its
/// name and LUID (the host device is made so, `crate::host`).
fn default_adapter() -> Option<(String, windows::Win32::Foundation::LUID)> {
    // SAFETY: plain DXGI queries.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let adapter = factory.EnumAdapters1(0).ok()?;
        let desc = adapter.GetDesc1().ok()?;
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        Some((
            String::from_utf16_lossy(&desc.Description[..len]),
            desc.AdapterLuid,
        ))
    }
}

/// The Authenticode signature of `path` (NVIDIA signs its DLLs), checked
/// without going online.
fn verify_signature(path: &Path) -> Result<(), String> {
    use windows_sys::Win32::Security::WinTrust::*;
    let file = wide(path);
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: file.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: filled structures that live through both calls.
    let code = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast()) };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: as above; closes the state the first call opened.
    unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast()) };
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} is not validly signed ({:#x})",
            path.display(),
            code as u32
        ))
    }
}

fn load(path: &Path) -> Result<Sl, String> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        LoadLibraryExW,
    };
    let file = wide(path);
    // Its own folder and System32 only, never the current folder.
    // SAFETY: a NUL-terminated full path.
    let module = unsafe {
        LoadLibraryExW(
            file.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(format!(
            "{} did not load ({})",
            plan::INTERPOSER,
            std::io::Error::last_os_error()
        ));
    }
    let library = Library(module);
    let find = |name: &CStr| {
        // SAFETY: a loaded module and a NUL-terminated name.
        unsafe { GetProcAddress(module, name.as_ptr().cast()) }
            .map(|f| f as *const c_void)
            .ok_or_else(|| format!("{} has no {}", plan::INTERPOSER, name.to_string_lossy()))
    };
    macro_rules! get {
        ($name:literal, $t:ty) => {
            // SAFETY: the export of that name has the header's signature.
            unsafe { std::mem::transmute::<*const c_void, $t>(find($name)?) }
        };
    }
    Ok(Sl {
        init: get!(c"slInit", abi::SlInit),
        is_feature_supported: get!(c"slIsFeatureSupported", abi::SlIsFeatureSupported),
        evaluate_feature: get!(c"slEvaluateFeature", abi::SlEvaluateFeature),
        free_resources: get!(c"slFreeResources", abi::SlFreeResources),
        set_tag_for_frame: get!(c"slSetTagForFrame", abi::SlSetTagForFrame),
        get_feature_requirements: get!(c"slGetFeatureRequirements", abi::SlGetFeatureRequirements),
        get_feature_version: get!(c"slGetFeatureVersion", abi::SlGetFeatureVersion),
        upgrade_interface: get!(c"slUpgradeInterface", abi::SlUpgradeInterface),
        set_constants: get!(c"slSetConstants", abi::SlSetConstants),
        get_native_interface: get!(c"slGetNativeInterface", abi::SlGetNativeInterface),
        get_feature_function: get!(c"slGetFeatureFunction", abi::SlGetFeatureFunction),
        get_new_frame_token: get!(c"slGetNewFrameToken", abi::SlGetNewFrameToken),
        set_d3d_device: get!(c"slSetD3DDevice", abi::SlSetD3DDevice),
        _library: library,
    })
}

/// The host device is made: Streamline is given it (`slSetD3DDevice`) and
/// the feature functions are fetched. With frame generation the direct
/// queue is made through Streamline's device proxy: the native queue behind
/// it, to use for everything, if it was made here.
pub(crate) fn device_made(device: &ID3D12Device) -> Option<ID3D12CommandQueue> {
    let plan = PLAN.get()?;
    let sl = SL.get()?;
    // SAFETY: the native device.
    let code = unsafe { (sl.set_d3d_device)(device.as_raw()) };
    if code != 0 {
        tracing::warn!(
            "Streamline: slSetD3DDevice failed: {} ({code}); DLSS, frame generation and Reflex stay off",
            abi::result_name(code)
        );
        return None;
    }
    let function = |feature: u32, name: &CStr| -> Option<*mut c_void> {
        let mut f = std::ptr::null_mut();
        // SAFETY: a NUL-terminated name and an out pointer.
        let code = unsafe { (sl.get_feature_function)(feature, name.as_ptr(), &mut f) };
        if code != 0 || f.is_null() {
            tracing::warn!(
                "Streamline: no {} ({}, {code})",
                name.to_string_lossy(),
                abi::result_name(code)
            );
            return None;
        }
        Some(f)
    };
    macro_rules! f {
        ($feature:expr, $name:literal, $t:ty) => {
            // SAFETY: the function of that name has the header's signature.
            function($feature, $name).map(|f| unsafe { std::mem::transmute::<*mut c_void, $t>(f) })
        };
    }
    let mut functions = Functions::default();
    if plan.available.dlss {
        functions.dlss_optimal = f!(
            abi::FEATURE_DLSS,
            c"slDLSSGetOptimalSettings",
            abi::SlDlssGetOptimalSettings
        );
        functions.dlss_set_options = f!(
            abi::FEATURE_DLSS,
            c"slDLSSSetOptions",
            abi::SlDlssSetOptions
        );
        log_version(sl, abi::FEATURE_DLSS, "DLSS Super Resolution");
    }
    if plan.available.framegen {
        functions.dlssg_set_options = f!(
            abi::FEATURE_DLSS_G,
            c"slDLSSGSetOptions",
            abi::SlDlssgSetOptions
        );
        functions.dlssg_get_state = f!(
            abi::FEATURE_DLSS_G,
            c"slDLSSGGetState",
            abi::SlDlssgGetState
        );
        log_version(sl, abi::FEATURE_DLSS_G, "DLSS Frame Generation");
    }
    if plan.available.framegen || plan.available.reflex {
        functions.reflex_set_options = f!(
            abi::FEATURE_REFLEX,
            c"slReflexSetOptions",
            abi::SlReflexSetOptions
        );
        functions.reflex_sleep = f!(abi::FEATURE_REFLEX, c"slReflexSleep", abi::SlReflexSleep);
        functions.reflex_get_state = f!(
            abi::FEATURE_REFLEX,
            c"slReflexGetState",
            abi::SlReflexGetState
        );
        functions.pcl_set_marker = f!(abi::FEATURE_PCL, c"slPCLSetMarker", abi::SlPclSetMarker);
        log_version(sl, abi::FEATURE_REFLEX, "Reflex");
    }
    let dlss = functions.dlss_optimal.is_some() && functions.dlss_set_options.is_some();
    // Reflex: low latency, which frame generation needs as well, or the mode
    // `VICTORY_REFLEX` names; the markers go whenever either is on.
    let mut reflex = false;
    if let (Some(set_options), Some(_)) = (functions.reflex_set_options, functions.pcl_set_marker) {
        let mode = plan
            .asked
            .reflex
            .map_or(abi::REFLEX_LOW_LATENCY, Reflex::sl_mode);
        let options = abi::ReflexOptions {
            base: abi::BaseStructure::new(abi::REFLEX_OPTIONS, 1),
            mode,
            frame_limit_us: 0,
            use_markers_to_optimize: false,
            virtual_key: 0,
            id_thread: 0,
        };
        // SAFETY: filled options; nothing else calls the library yet.
        let code = unsafe { set_options(&options) };
        reflex = code == 0;
        tracing::info!(
            code,
            result = abi::result_name(code),
            mode = plan
                .asked
                .reflex
                .map_or("on (for frame generation)", Reflex::name),
            sleep = plan.available.reflex && functions.reflex_sleep.is_some(),
            "Streamline: Reflex options set; latency markers at the frame's begin, first submission and present"
        );
    } else if plan.available.framegen || plan.available.reflex {
        tracing::warn!(
            "Streamline: a Reflex or latency-marker function is missing; Reflex stays off"
        );
    }
    let _ = FUNCTIONS.set(functions);
    DLSS_READY.store(dlss, Release);
    // (After the functions: a title thread that sees this finds them.)
    REFLEX_READY.store(reflex, Release);
    None
}

/// Whether the latency markers run (Reflex or frame generation, with
/// Streamline's functions found): the title's side then says where its
/// frames begin, submit and present.
#[inline]
pub(crate) fn reflex_marking() -> bool {
    REFLEX_READY.load(Relaxed)
}

/// Reflex's view of the title's frames: the newest begun (its index, the
/// title's frame token) and whether its simulation ended (its first render
/// submission), and the frames the title presented that our present has
/// not shown yet, oldest first (our present comes later, on the line's
/// thread, when the title may be in its next frame).
struct Frames {
    newest: Option<u32>,
    ended: bool,
    presents: std::collections::VecDeque<u32>,
}

static FRAMES: Mutex<Frames> = Mutex::new(Frames {
    newest: None,
    ended: true,
    presents: std::collections::VecDeque::new(),
});

/// One frame token made and in use at a time: the programming guide calls
/// `slGetNewFrameToken` not thread safe, and the headers do not say how
/// many tokens it keeps, so a token is made from the frame's index where it
/// is used and read before another is made.
static TOKENS: Mutex<()> = Mutex::new(());

/// Runs `f` with the frame token of `index`, none other made meanwhile.
fn with_token(index: u32, f: impl FnOnce(*mut abi::FrameToken)) {
    let Some(sl) = SL.get() else { return };
    let _one = lock(&TOKENS);
    let mut token = std::ptr::null_mut();
    // SAFETY: an out pointer and an index.
    let code = unsafe { (sl.get_new_frame_token)(&mut token, &index) };
    if code == 0 && !token.is_null() {
        f(token);
    }
}

/// `markers` of frame `index`, in order.
fn mark(index: u32, markers: &[u32]) {
    let Some(marker) = FUNCTIONS.get().and_then(|f| f.pcl_set_marker) else {
        return;
    };
    with_token(index, |token| {
        for &m in markers {
            // SAFETY: a marker for a token of ours.
            unsafe { marker(m, token) };
        }
    });
}

/// Since the last Reflex line: frames begun, sleeps, the microseconds slept
/// and the longest sleep, presents bound to a frame and those without one.
static REFLEX_COUNTS: [std::sync::atomic::AtomicU64; 6] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 6];
const R_BEGUN: usize = 0;
const R_SLEEPS: usize = 1;
const R_SLEPT_US: usize = 2;
const R_SLEPT_LONGEST: usize = 3;
const R_PRESENTS: usize = 4;
const R_UNBOUND: usize = 5;
/// A sleep took more than 2 s: none is asked for any more (the markers go
/// on). The sleep has no bound in the header and runs on the title's thread.
static SLEEP_OFF: AtomicBool = AtomicBool::new(false);
const SLEEP_TOO_LONG_US: u64 = 2_000_000;

/// The title waited for its frame origin and frame `frame` begins (its
/// token): with `VICTORY_REFLEX` Reflex's sleep, then the simulation's
/// start. On the title's thread, after our own clock's wait, with no lock
/// of ours held. A frame before it that never submitted ends here first.
///
/// shortcut: the sleep is at the origin wait, where the title begins its
/// frame; the title may read its input later in the frame (the other
/// fork's runs saw that), and then the sleep paces a frame whose input is
/// already read. Moving it to the title's input reading is the next step
/// if the latency report says so.
pub(crate) fn frame_begins(frame: u64) {
    if !reflex_marking() {
        return;
    }
    let index = frame as u32;
    let owed = {
        let mut frames = lock(&FRAMES);
        let owed = frames.newest.filter(|_| !frames.ended);
        frames.ended = true;
        owed
    };
    if let Some(owed) = owed {
        mark(owed, &[abi::SIMULATION_END, abi::RENDER_SUBMIT_START]);
    }
    REFLEX_COUNTS[R_BEGUN].fetch_add(1, Relaxed);
    let sleep = FUNCTIONS.get().and_then(|f| f.reflex_sleep);
    let asked = PLAN.get().is_some_and(|p| p.available.reflex);
    if let Some(sleep) = sleep.filter(|_| asked && !SLEEP_OFF.load(Relaxed)) {
        let mut made = std::ptr::null_mut();
        with_token(index, |token| made = token);
        // The token is read without the lock: held across the sleep, it
        // would hold up the markers of the frame before.
        let began = std::time::Instant::now();
        // SAFETY: a token of ours.
        unsafe { sleep(made) };
        let took = began.elapsed().as_micros() as u64;
        REFLEX_COUNTS[R_SLEEPS].fetch_add(1, Relaxed);
        REFLEX_COUNTS[R_SLEPT_US].fetch_add(took, Relaxed);
        REFLEX_COUNTS[R_SLEPT_LONGEST].fetch_max(took, Relaxed);
        if took > SLEEP_TOO_LONG_US && !SLEEP_OFF.swap(true, Relaxed) {
            tracing::warn!(
                ms = took / 1000,
                "Reflex: one slReflexSleep took more than 2 s: no further sleep is asked for in this run (the markers go on)"
            );
        }
    }
    mark(index, &[abi::SIMULATION_START]);
    let mut frames = lock(&FRAMES);
    frames.newest = Some(index);
    frames.ended = false;
}

/// The title submits to a queue that draws: the newest frame's first such
/// submission ends its simulation and begins its submissions.
pub(crate) fn title_submits() {
    if !reflex_marking() {
        return;
    }
    let index = {
        let mut frames = lock(&FRAMES);
        if frames.ended {
            return;
        }
        frames.ended = true;
        frames.newest
    };
    if let Some(index) = index {
        mark(index, &[abi::SIMULATION_END, abi::RENDER_SUBMIT_START]);
    }
}

/// The title presents: the newest frame is the one shown (bound here, in
/// the title's order); what it still owes is sent first.
pub(crate) fn title_presents() {
    if !reflex_marking() {
        return;
    }
    let mut frames = lock(&FRAMES);
    let Some(index) = frames.newest else { return };
    if !frames.ended {
        frames.ended = true;
        drop(frames);
        mark(index, &[abi::SIMULATION_END, abi::RENDER_SUBMIT_START]);
        frames = lock(&FRAMES);
    }
    // A present shown twice, or presents our side never shows: kept short.
    if frames.presents.back() != Some(&index) {
        frames.presents.push_back(index);
    }
    while frames.presents.len() > 8 {
        frames.presents.pop_front();
    }
}

/// Our present of the oldest frame the title presented begins: its
/// submissions end and its present starts. The frame's index, if it has one.
fn reflex_present_begins() -> Option<u32> {
    if !reflex_marking() {
        return None;
    }
    let index = lock(&FRAMES).presents.pop_front();
    match index {
        Some(index) => {
            REFLEX_COUNTS[R_PRESENTS].fetch_add(1, Relaxed);
            mark(index, &[abi::RENDER_SUBMIT_END, abi::PRESENT_START]);
        }
        None => {
            REFLEX_COUNTS[R_UNBOUND].fetch_add(1, Relaxed);
        }
    }
    index
}

/// Our present of frame `index` returned.
fn reflex_present_ends(index: Option<u32>) {
    if let Some(index) = index {
        mark(index, &[abi::PRESENT_END]);
    }
    if let Some(line) = reflex_line() {
        tracing::info!("{line}");
    }
}

/// About every ten seconds (on the presenting thread): Reflex's own state
/// and what was done since the last line.
fn reflex_line() -> Option<String> {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    {
        let mut last = lock(&LAST);
        let now = std::time::Instant::now();
        match *last {
            Some(at) if now.duration_since(at) < std::time::Duration::from_secs(10) => return None,
            None => {
                *last = Some(now);
                return None;
            }
            _ => *last = Some(now),
        }
    }
    let [begun, sleeps, slept, longest, presents, unbound] =
        [0, 1, 2, 3, 4, 5].map(|i| REFLEX_COUNTS[i].swap(0, Relaxed));
    let state = FUNCTIONS.get().and_then(|f| f.reflex_get_state).map(|get| {
        let mut state = abi::ReflexState::new();
        // SAFETY: a state to fill.
        let code = unsafe { get(&mut *state) };
        (code, state)
    });
    let reported = match &state {
        Some((0, state)) => {
            let (low, available) = (
                state.low_latency_available != 0,
                state.latency_report_available != 0,
            );
            let valid: Vec<&abi::ReflexReport> = state
                .frame_report
                .iter()
                .filter(|r| {
                    r.frame_id != 0
                        && r.sim_start_time != 0
                        && r.gpu_render_end_time > r.sim_start_time
                })
                .collect();
            let n = valid.len().max(1) as f64;
            let total = valid
                .iter()
                .map(|r| (r.gpu_render_end_time - r.sim_start_time) as f64)
                .sum::<f64>()
                / n;
            let gpu = valid
                .iter()
                .map(|r| f64::from(r.gpu_frame_time_us))
                .sum::<f64>()
                / n;
            format!(
                "low latency {}, latency report {}: over {} frames from simulation start to the end of GPU rendering {total:.0} on average (the header names no unit), GPU frame time {:.2} ms on average",
                if low { "available" } else { "NOT available" },
                if available {
                    "available"
                } else {
                    "not available"
                },
                valid.len(),
                gpu / 1000.0
            )
        }
        Some((code, _)) => format!("slReflexGetState: {} ({code})", abi::result_name(*code)),
        None => "no slReflexGetState".into(),
    };
    Some(format!(
        "Reflex state: {reported}; since the last line {begun} frames begun, {sleeps} sleeps of {:.2} ms on average (the longest {:.2} ms){}, {presents} presents bound to a frame, {unbound} without one",
        slept as f64 / 1000.0 / sleeps.max(1) as f64,
        longest as f64 / 1000.0,
        if SLEEP_OFF.load(Relaxed) {
            ", NO sleep is asked for any more"
        } else {
            ""
        }
    ))
}

fn log_version(sl: &Sl, feature: u32, name: &str) {
    let mut version = abi::FeatureVersion {
        base: abi::BaseStructure::new(abi::FEATURE_VERSION, 1),
        version_sl: abi::Version::default(),
        version_ngx: abi::Version::default(),
    };
    // SAFETY: a structure to fill.
    let code = unsafe { (sl.get_feature_version)(feature, &mut version) };
    if code == 0 {
        tracing::info!(streamline = %version.version_sl, ngx = %version.version_ngx, "Streamline: {name} loaded");
    } else {
        tracing::warn!(
            "Streamline: {name} has no version: {} ({code})",
            abi::result_name(code)
        );
    }
}

/// What [`present_begins`] began, for [`present_ends`].
pub(crate) struct Presenting {
    /// The Reflex index of the frame shown, if it has one.
    index: Option<u32>,
}

/// Right before our swap chain's Present: the frame's latency markers.
pub(crate) fn present_begins() -> Presenting {
    Presenting {
        index: reflex_present_begins(),
    }
}

/// Right after our swap chain's Present returned.
pub(crate) fn present_ends(presenting: Presenting) {
    if reflex_marking() {
        reflex_present_ends(presenting.index);
    }
}
