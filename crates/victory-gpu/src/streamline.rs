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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
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
    // Frame generation: its functions, Reflex's markers, and the direct
    // queue made through the device proxy (it hooks queue creation).
    let framegen_functions =
        functions.dlssg_set_options.is_some() && functions.dlssg_get_state.is_some();
    let queue = (plan.available.framegen && framegen_functions && reflex)
        .then(|| proxied_queue(sl, device))
        .flatten();
    if plan.available.framegen && queue.is_none() {
        tracing::warn!(
            "DLSS Frame Generation stays off: the device hand-over to Streamline did not complete"
        );
    }
    if plan.available.framegen && !plan.available.dlss {
        tracing::warn!(
            "DLSS Frame Generation takes the depth and motion vectors of DLSS's upscaling: without VICTORY_DLSS it never switches on"
        );
    }
    let _ = FUNCTIONS.set(functions);
    DLSS_READY.store(dlss, Release);
    FG_READY.store(queue.is_some(), Release);
    // (After the functions: a title thread that sees this finds them.)
    REFLEX_READY.store(reflex, Release);
    queue
}

/// The direct queue, made through Streamline's proxy of `device` (frame
/// generation hooks queue creation); the native queue is returned for our
/// use and the proxy kept for the swap chain.
fn proxied_queue(sl: &Sl, device: &ID3D12Device) -> Option<ID3D12CommandQueue> {
    let mut raw = device.as_raw();
    // SAFETY: an owned reference of ours; on success `raw` is a new proxy
    // that holds its own reference to the device.
    let code = unsafe { (sl.upgrade_interface)(&mut raw) };
    if code != 0 || raw.is_null() || raw == device.as_raw() {
        tracing::warn!(
            "Streamline: the device was not upgraded: {} ({code})",
            abi::result_name(code)
        );
        return None;
    }
    // SAFETY: the proxy's one reference, now ours.
    let proxy_device = unsafe { ID3D12Device::from_raw(raw) };
    let desc = D3D12_COMMAND_QUEUE_DESC {
        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
        ..Default::default()
    };
    // SAFETY: plain queue creation through the proxy.
    let proxy_queue: ID3D12CommandQueue = match unsafe { proxy_device.CreateCommandQueue(&desc) } {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!(error = %e, "Streamline: no queue from the device proxy");
            return None;
        }
    };
    let mut native = std::ptr::null_mut();
    // SAFETY: a proxy queue; the native one comes back with a reference.
    let code = unsafe { (sl.get_native_interface)(proxy_queue.as_raw(), &mut native) };
    if code != 0 || native.is_null() {
        tracing::warn!(
            "Streamline: no native queue behind the proxy: {} ({code})",
            abi::result_name(code)
        );
        return None;
    }
    // SAFETY: the reference slGetNativeInterface added.
    let native = unsafe { ID3D12CommandQueue::from_raw(native) };
    tracing::info!(
        "Streamline: the direct queue was made through the device proxy (frame generation)"
    );
    let _ = PROXIES.set(Proxies {
        _device: proxy_device,
        queue: proxy_queue,
    });
    Some(native)
}

/// Streamline's proxies kept for frame generation: the device that made the
/// direct queue and the queue the swap chain is made on.
struct Proxies {
    _device: ID3D12Device,
    queue: ID3D12CommandQueue,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Proxies {}
// SAFETY: as above.
unsafe impl Sync for Proxies {}

static PROXIES: OnceLock<Proxies> = OnceLock::new();
/// Our present goes through Streamline's swap chain.
static SWAP_CHAIN_PROXIED: AtomicBool = AtomicBool::new(false);

/// Whether frame generation is ready to take our swap chain (the back
/// buffer is then made at the scene plane's size, `plan::framegen_back_buffer`).
pub(crate) fn framegen_ready() -> bool {
    FG_READY.load(Acquire)
}

/// For frame generation: Streamline's proxy of `factory` and the queue
/// proxy to make the swap chain with; `None` to make it as before.
pub(crate) fn swap_chain_factory(
    factory: &IDXGIFactory2,
) -> Option<(IDXGIFactory2, ID3D12CommandQueue)> {
    if !framegen_ready() {
        return None;
    }
    let sl = SL.get()?;
    let queue = PROXIES.get()?.queue.clone();
    let mut raw = factory.as_raw();
    // SAFETY: as for the device: a new proxy with its own reference.
    let code = unsafe { (sl.upgrade_interface)(&mut raw) };
    if code != 0 || raw.is_null() || raw == factory.as_raw() {
        tracing::warn!(
            "Streamline: the DXGI factory was not upgraded: {} ({code})",
            abi::result_name(code)
        );
        FG_READY.store(false, Release);
        return None;
    }
    // SAFETY: the proxy's reference, now ours.
    Some((unsafe { IDXGIFactory2::from_raw(raw) }, queue))
}

/// The swap chain was made through the proxy (or not, after all: then
/// frame generation stays off).
pub(crate) fn swap_chain_made(proxied: bool, error: Option<&windows::core::Error>) {
    SWAP_CHAIN_PROXIED.store(proxied, Release);
    if proxied {
        tracing::info!(
            "Streamline: our swap chain is Streamline's proxy; DLSS Frame Generation can run on its presents"
        );
    } else {
        FG_READY.store(false, Release);
        tracing::warn!(
            error = error.map(|e| e.to_string()).unwrap_or_default(),
            "Streamline: the swap chain could not be made through the proxy; made as before, frame generation off"
        );
    }
}

fn framegen_presenting() -> bool {
    FG_READY.load(Acquire) && SWAP_CHAIN_PROXIED.load(Acquire)
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
static REFLEX_COUNTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
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

/// The planes a present shows (Series S: the scene and the interface over
/// it), the host states they are in, and the back buffer's size.
pub(crate) struct Planes<'a> {
    pub base: (&'a ID3D12Resource, u32),
    pub overlay: Option<(&'a ID3D12Resource, u32)>,
    pub size: [u32; 2],
}

/// The viewport frame generation wants.
const VIEWPORT_FG: u32 = 0;

/// A frame index of frame generation's own for a present no title frame
/// is bound to (its markers all go at the present): far from the title's.
static OWN_INDEX: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x8000_0000);

/// Right before our swap chain's Present: the frame's latency markers and,
/// with frame generation, its constants and tags, and whether it runs on
/// this present (it does when an upscaling was submitted since the last).
pub(crate) fn present_begins(planes: Planes<'_>) -> Presenting {
    let index = reflex_present_begins();
    if !framegen_presenting() {
        return Presenting { index };
    }
    // Frame generation wants every marker of the frame it presents: a
    // present without a title frame gets a frame of its own.
    let index = index.or_else(|| {
        let own = OWN_INDEX.fetch_add(1, Relaxed);
        mark(
            own,
            &[
                abi::SIMULATION_START,
                abi::SIMULATION_END,
                abi::RENDER_SUBMIT_START,
                abi::RENDER_SUBMIT_END,
                abi::PRESENT_START,
            ],
        );
        Some(own)
    });
    if let Some(index) = index {
        framegen_before(index, &planes);
    }
    Presenting { index }
}

/// Right after our swap chain's Present returned.
pub(crate) fn present_ends(presenting: Presenting) {
    if reflex_marking() {
        reflex_present_ends(presenting.index);
    }
    if framegen_presenting() {
        framegen_after();
    }
}

/// An upscaling's depth and motion vectors for frame generation.
struct Noted {
    id: u64,
    depth: std::sync::Arc<crate::resource::HostTexture>,
    motion: std::sync::Arc<crate::resource::HostTexture>,
    camera: plan::Camera,
}

/// Noted on host lists not submitted yet; the latest submitted.
static PENDING: Mutex<Vec<Noted>> = Mutex::new(Vec::new());
static SUBMITTED: Mutex<Option<Noted>> = Mutex::new(None);
/// The upscaling the last present tagged.
static PRESENTED_ID: AtomicU64 = AtomicU64::new(0);

/// Frame generation's note of an upscaling's depth and motion vectors on
/// the host list that recorded it ([`submitted`] makes them the latest).
fn framegen_note(host: &crate::host::HostList, up: &Upscale, id: u64) {
    if !framegen_ready() {
        return;
    }
    let (Some(depth), Some(motion)) = (
        crate::resource::host_copy(up.resources[DEPTH].0.cast()),
        crate::resource::host_copy(up.resources[MOTION].0.cast()),
    ) else {
        return;
    };
    let mut pending = lock(&PENDING);
    pending.retain(|n| id.saturating_sub(n.id) < 16);
    pending.push(Noted {
        id,
        depth,
        motion,
        camera: up.camera,
    });
    drop(pending);
    host.mark_upscale(id);
}

/// A host list that recorded upscaling `id` is submitted (in submission
/// order, before the present that follows it).
pub(crate) fn submitted(id: u64) {
    let noted = {
        let mut pending = lock(&PENDING);
        let at = pending.iter().position(|n| n.id == id);
        at.map(|i| pending.swap_remove(i))
    };
    if let Some(noted) = noted {
        let mut latest = lock(&SUBMITTED);
        if latest.as_ref().is_none_or(|l| l.id < noted.id) {
            *latest = Some(noted);
        }
    }
}

/// Frame generation's switch as last set, and its counters since the last
/// frame-rate line: presents with it on and off, frames it reported shown;
/// its last status; frames shown since the caption last asked.
static FG_ON_NOW: AtomicBool = AtomicBool::new(false);
static FG_COUNTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
const FG_PRESENTS_ON: usize = 0;
const FG_PRESENTS_OFF: usize = 1;
const FG_SHOWN: usize = 2;
const FG_SHOWN_CAPTION: usize = 3;
static FG_STATUS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Before the present of frame `index`: constants and tags of the latest
/// upscaling submitted, and frame generation on (off without a fresh one:
/// menus, loads, films).
fn framegen_before(index: u32, planes: &Planes<'_>) {
    let (Some(sl), Some(functions), Some(plan)) = (SL.get(), FUNCTIONS.get(), PLAN.get()) else {
        return;
    };
    let Some(set_options) = functions.dlssg_set_options else {
        return;
    };
    let fresh = {
        let latest = lock(&SUBMITTED);
        latest
            .as_ref()
            .filter(|n| n.id > PRESENTED_ID.load(Relaxed))
            .map(|n| (n.id, n.depth.clone(), n.motion.clone(), n.camera))
    };
    let viewport = abi::ViewportHandle::new(VIEWPORT_FG);
    // (Read in the closure; sizes of a texture its description gives.)
    let size_of = |t: &ID3D12Resource| {
        // SAFETY: a live texture.
        let d = unsafe { t.GetDesc() };
        [d.Width as u32, d.Height]
    };
    let mut tagged = false;
    with_token(index, |token| {
        if let Some((id, depth, motion, camera)) = &fresh {
            PRESENTED_ID.store(*id, Relaxed);
            // (Frame generation reads the camera's planes: the nearer is
            // the near one, whichever way the title names them.)
            let camera = camera.planes_in_order();
            let constants = camera.constants(plan.asked.jitter, plan.asked.mvec);
            // SAFETY: constants for this frame and viewport.
            let code = unsafe { (sl.set_constants)(&constants, token, &viewport) };
            if code != 0 {
                once!(
                    warn,
                    "frame generation: slSetConstants: {} ({code})",
                    abi::result_name(code)
                );
            }
            let mut resources = vec![
                (
                    abi::Resource::texture(depth.texture.as_raw(), depth.state.load(Relaxed)),
                    abi::BUFFER_DEPTH,
                    camera.render,
                ),
                (
                    abi::Resource::texture(motion.texture.as_raw(), motion.state.load(Relaxed)),
                    abi::BUFFER_MOTION_VECTORS,
                    camera.motion_size(),
                ),
            ];
            // Two planes: the scene is the colour without the interface
            // (the back buffer is made at its size and format), the
            // interface plane its colour and alpha (premultiplied, as our
            // compose blends it) where it is the back buffer's size too.
            if let Some((over, over_state)) = planes.overlay {
                if size_of(planes.base.0) == planes.size {
                    resources.push((
                        abi::Resource::texture(planes.base.0.as_raw(), planes.base.1),
                        abi::BUFFER_HUDLESS_COLOR,
                        planes.size,
                    ));
                }
                if size_of(over) == planes.size {
                    resources.push((
                        abi::Resource::texture(over.as_raw(), over_state),
                        abi::BUFFER_UI_COLOR_AND_ALPHA,
                        planes.size,
                    ));
                }
            }
            let tags: Vec<abi::ResourceTag> = resources
                .iter_mut()
                .map(|(r, kind, size)| {
                    abi::ResourceTag::new(
                        r,
                        *kind,
                        abi::VALID_UNTIL_PRESENT,
                        abi::Extent::sized(*size),
                    )
                })
                .collect();
            // SAFETY: tags of live textures in the states they are in at the
            // present (the line sends the present after those submissions).
            let code = unsafe {
                (sl.set_tag_for_frame)(
                    token,
                    &viewport,
                    tags.as_ptr(),
                    tags.len() as u32,
                    std::ptr::null_mut(),
                )
            };
            if code != 0 {
                once!(
                    warn,
                    "frame generation: slSetTagForFrame: {} ({code})",
                    abi::result_name(code)
                );
            }
            once!(
                info,
                render = ?camera.render,
                output = ?camera.output,
                back_buffer = ?planes.size,
                interface_plane = planes.overlay.is_some(),
                tags = tags.len(),
                depth_state = format_args!("{:#x}", resources[0].0.state),
                motion_state = format_args!("{:#x}", resources[1].0.state),
                near = constants.camera_near,
                far = constants.camera_far,
                depth_inverted = camera.depth_inverted(),
                jitter = ?constants.jitter_offset,
                mvec_scale = ?constants.mvec_scale,
                "DLSS Frame Generation: first frame with depth and motion vectors tagged (2 tags: no colour without the interface; 3: with it; 4: and the interface)"
            );
            tagged = code == 0;
        } else if FG_ON_NOW.load(Relaxed) {
            // No upscaling this frame: what was tagged may no longer hold
            // what it did.
            let tags = [
                abi::BUFFER_DEPTH,
                abi::BUFFER_MOTION_VECTORS,
                abi::BUFFER_HUDLESS_COLOR,
                abi::BUFFER_UI_COLOR_AND_ALPHA,
            ]
            .map(|kind| {
                abi::ResourceTag::new(
                    std::ptr::null_mut(),
                    kind,
                    abi::VALID_UNTIL_PRESENT,
                    abi::Extent::default(),
                )
            });
            // SAFETY: null tags.
            unsafe {
                (sl.set_tag_for_frame)(
                    token,
                    &viewport,
                    tags.as_ptr(),
                    tags.len() as u32,
                    std::ptr::null_mut(),
                )
            };
        }
    });
    if tagged != FG_ON_NOW.load(Relaxed) {
        let mut options = abi::DlssgOptions::new(if tagged {
            abi::DLSSG_ON
        } else {
            abi::DLSSG_OFF
        });
        options.flags = abi::DLSSG_RETAIN_RESOURCES_WHEN_OFF;
        options.num_back_buffers = 2;
        options.color_width = planes.size[0];
        options.color_height = planes.size[1];
        options.on_error_callback = Some(present_error);
        // SAFETY: filled options for viewport 0, on the presenting thread.
        let code = unsafe { set_options(&viewport, &options) };
        tracing::info!(
            index,
            on = tagged,
            code,
            result = abi::result_name(code),
            "DLSS Frame Generation switched"
        );
        if code == 0 || code == 39 {
            FG_ON_NOW.store(tagged, Relaxed);
        }
        static FIRST: AtomicBool = AtomicBool::new(true);
        if tagged
            && code == 0
            && FIRST.swap(false, Relaxed)
            && let Some(host) = crate::host::host()
        {
            host.log_video_memory("right before the first present with frame generation on");
        }
    }
    FG_COUNTS[if FG_ON_NOW.load(Relaxed) {
        FG_PRESENTS_ON
    } else {
        FG_PRESENTS_OFF
    }]
    .fetch_add(1, Relaxed);
}

unsafe extern "C" fn present_error(error: *const i32) {
    // SAFETY: the error for the call.
    let code = unsafe { error.as_ref() }.copied().unwrap_or(0);
    once!(
        warn,
        hresult = format_args!("{:#x}", code as u32),
        "DLSS Frame Generation: a present failed"
    );
}

/// After the present: what frame generation reports.
fn framegen_after() {
    if !FG_ON_NOW.load(Relaxed) {
        FG_COUNTS[FG_SHOWN].fetch_add(1, Relaxed);
        FG_COUNTS[FG_SHOWN_CAPTION].fetch_add(1, Relaxed);
        return;
    }
    let Some(get_state) = FUNCTIONS.get().and_then(|f| f.dlssg_get_state) else {
        return;
    };
    let mut state = abi::DlssgState::default();
    let viewport = abi::ViewportHandle::new(VIEWPORT_FG);
    // SAFETY: a structure to fill, no options (no estimate asked).
    let code = unsafe { get_state(&viewport, &mut state, std::ptr::null()) };
    if code != 0 {
        return;
    }
    let shown = u64::from(state.num_frames_actually_presented);
    FG_COUNTS[FG_SHOWN].fetch_add(shown, Relaxed);
    FG_COUNTS[FG_SHOWN_CAPTION].fetch_add(shown, Relaxed);
    if FG_STATUS.swap(state.status, Relaxed) != state.status {
        tracing::warn!(
            status = format_args!("{:#x}", state.status),
            vram_mb = state.estimated_vram_usage_in_bytes >> 20,
            "DLSS Frame Generation status changed (0 ok, 1 output too small, 2 Reflex not seen, 4 HDR format, 8 constants invalid, 16 back buffer index not asked)"
        );
    }
}

/// Our swap chain's buffers are about to be made again: frame generation is
/// off until the next present with an upscaling (Streamline asks so).
pub(crate) fn resizing() {
    if !framegen_presenting() || !FG_ON_NOW.load(Relaxed) {
        return;
    }
    let Some(set_options) = FUNCTIONS.get().and_then(|f| f.dlssg_set_options) else {
        return;
    };
    let options = abi::DlssgOptions::new(abi::DLSSG_OFF);
    let viewport = abi::ViewportHandle::new(VIEWPORT_FG);
    // SAFETY: filled options.
    unsafe { set_options(&viewport, &options) };
    FG_ON_NOW.store(false, Relaxed);
}

/// With frame generation on our swap chain: the frames shown since the last
/// call (the generated ones too), for the caption; `None` without it.
pub(crate) fn shown_since() -> Option<u64> {
    framegen_presenting().then(|| FG_COUNTS[FG_SHOWN_CAPTION].swap(0, Relaxed))
}

fn framegen_line(frames: f64, fps: f64) -> Option<String> {
    if !framegen_presenting() {
        return None;
    }
    let [on, off, shown] =
        [FG_PRESENTS_ON, FG_PRESENTS_OFF, FG_SHOWN].map(|i| FG_COUNTS[i].swap(0, Relaxed));
    Some(format!(
        "DLSS Frame Generation: {fps:.1} fps rendered, {:.1} fps shown; {on} presents with it on, {off} off, {shown} frames shown ({:.2} per rendered frame; status {:#x})",
        fps * shown as f64 / frames.max(1.0),
        shown as f64 / frames.max(1.0),
        FG_STATUS.load(Relaxed)
    ))
}

/// The viewport of the upscaling; frame generation's is 0.
const VIEWPORT_DLSS: u32 = 1;

/// Whether the title's FidelityFX calls are to be served (DLSS on and
/// Streamline loaded): `victory_hle` then puts its own in front of them.
pub fn serving() -> bool {
    PLAN.get().is_some_and(|p| p.available.dlss)
}

/// One upscaling the title asked for: a FidelityFX `ffxDispatch` of an
/// upscale, in its own terms (`victory_hle::modules::fidelityfx` reads it).
#[derive(Clone, Copy, Debug)]
pub struct Upscale {
    /// The FidelityFX context.
    pub context: usize,
    /// The title's command list.
    pub list: *const crate::object::Object,
    /// Colour, depth, motion vectors, exposure, reactive mask, transparency
    /// and composition mask, output: the title's resource (null if none) and
    /// the FidelityFX state it is in.
    pub resources: [(*const crate::object::Object, u32); 7],
    pub camera: plan::Camera,
    pub pre_exposure: f32,
}

// SAFETY: pointers to title objects that live until the list has run (the
// title's part, as on the console); nothing else is shared.
unsafe impl Send for Upscale {}

pub const COLOUR: usize = 0;
pub const DEPTH: usize = 1;
pub const MOTION: usize = 2;
pub const EXPOSURE: usize = 3;
pub const REACTIVE: usize = 4;
pub const OUTPUT: usize = 6;

/// What became of an upscaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// DLSS does it: the title's own must not run.
    Dlss,
    /// Not taken: the title's own runs. Why, for the log.
    No(&'static str),
}

// Since the last frame-rate line: upscalings recorded by DLSS, failed (the
// output keeps what it had), left to the title's FidelityFX.
static EVALUATED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static FORWARDED: AtomicU64 = AtomicU64::new(0);
static EVALUATED_ALL: AtomicU64 = AtomicU64::new(0);
/// Upscalings noted so far (their ids, the DLSS frame index).
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Library calls that are not thread safe, and what the upscaling keeps
/// between frames, one caller at a time.
struct Calls {
    /// Optimal render sizes by output size and mode.
    optimal: Vec<([u32; 2], u32, Option<[u32; 2]>)>,
    /// The mode and sizes the last options named.
    last: Option<(u32, [u32; 2], [u32; 2])>,
}

static CALLS: Mutex<Calls> = Mutex::new(Calls {
    optimal: Vec::new(),
    last: None,
});

/// The upscaling `up` asked for on the title's thread: DLSS records it at
/// this place of the title's list instead; the title's own runs otherwise.
pub fn upscale(up: &Upscale) -> Taken {
    if !serving() {
        return Taken::No("Streamline's DLSS is not loaded");
    }
    if !DLSS_READY.load(Acquire) {
        return Taken::No("Streamline has no device yet, or DLSS's functions were not found");
    }
    if [COLOUR, DEPTH, MOTION, OUTPUT]
        .iter()
        .any(|&i| up.resources[i].0.is_null())
    {
        return Taken::No("a required texture (colour, depth, motion vectors, output) is missing");
    }
    if up.camera.render.contains(&0) || up.camera.output.contains(&0) {
        return Taken::No("an empty render or output size");
    }
    if let Err(why) = validate(up) {
        return Taken::No(why);
    }
    let id = NEXT_ID.fetch_add(1, Relaxed) + 1;
    let up = *up;
    let asked = crate::list::external(
        up.list.cast(),
        Box::new(move |host| {
            framegen_note(host, &up, id);
            evaluate(host, &up, id)
        }),
    );
    if !asked {
        return Taken::No("the title's list records nowhere (no host list)");
    }
    Taken::Dlss
}

/// The title's own upscaling ran.
pub fn forwarded() {
    FORWARDED.fetch_add(1, Relaxed);
}

/// What an upscaling's textures must be for DLSS, checked on the title's
/// thread (the title's own runs if not): host textures, in states they can
/// be in.
fn validate(up: &Upscale) -> Result<(), &'static str> {
    for (i, &(resource, state)) in up.resources.iter().enumerate() {
        if resource.is_null() || i == 5 {
            continue;
        }
        let resource = resource.cast();
        if !crate::resource::is_resource(resource) {
            return Err("a texture of the upscaling is no resource of ours");
        }
        let Some(copy) = crate::resource::host_copy(resource) else {
            return Err("a texture of the upscaling has no host copy (a buffer?)");
        };
        let wanted = if i == OUTPUT {
            plan::output_state(state)
        } else {
            plan::input_state(state)
        };
        if crate::host::legal_state(&copy.texture, wanted) != wanted {
            return Err(if i == OUTPUT {
                "the output cannot be written by a shader (no unordered access)"
            } else {
                "an input cannot be read by a shader"
            });
        }
    }
    Ok(())
}

/// Colour, depth, motion vectors, exposure, reactive mask, (transparency),
/// output: the host textures DLSS is given and the states they are in.
type Textures = [Option<(ID3D12Resource, u32)>; 7];

/// DLSS in place of the title's upscaling, recorded into `host` (where the
/// title's list is now). Whether it recorded anything.
fn evaluate(host: &crate::host::HostList, up: &Upscale, id: u64) -> bool {
    let Some(plan) = PLAN.get() else { return false };
    let camera = up.camera;
    let fail = |why: &str| {
        FAILED.fetch_add(1, Relaxed);
        tracing::warn!(
            id,
            "DLSS: the upscaling was not recorded: {why} (the output keeps what it had)"
        );
        false
    };
    // The inputs as a compute shader's would be (aliased views, uploads of
    // title-written textures, depth twins), then in the states DLSS is told.
    let used = |i: usize| {
        !up.resources[i].0.is_null()
            && (i != REACTIVE || plan.asked.reactive)
            && (i != EXPOSURE || camera.flags & plan::FFX_AUTO_EXPOSURE == 0)
    };
    let mut textures: Textures = Default::default();
    for i in [COLOUR, DEPTH, MOTION, EXPOSURE, REACTIVE, OUTPUT] {
        if !used(i) {
            continue;
        }
        let (resource, state) = up.resources[i];
        let resource = resource.cast();
        if i != OUTPUT && crate::external::prepare_input(host, resource).is_none() {
            return fail("an input could not be prepared");
        }
        let wanted = if i == OUTPUT {
            plan::output_state(state)
        } else {
            plan::input_state(state)
        };
        let Some((texture, legal)) = crate::external::host_texture(host, resource, wanted) else {
            return fail("a texture has no host copy");
        };
        if legal != wanted {
            return fail("a texture cannot be in the state DLSS needs");
        }
        textures[i] = Some((texture, legal));
    }
    let recorded = match record(host.list.as_raw(), &textures, &camera, up.pre_exposure, id) {
        Ok(recorded) => recorded,
        Err(why) => return fail(&why),
    };
    crate::external::output_written(up.resources[OUTPUT].0.cast());
    EVALUATED.fetch_add(1, Relaxed);
    let all = EVALUATED_ALL.fetch_add(1, Relaxed) + 1;
    if all == 1 || recorded.changed {
        let formats: Vec<i32> = textures
            .iter()
            .flatten()
            // SAFETY: a live texture.
            .map(|(t, _)| unsafe { t.GetDesc() }.Format.0)
            .collect();
        tracing::info!(
            render = ?camera.render,
            output = ?camera.output,
            mode = plan::sl_mode_name(recorded.mode),
            asked = plan.asked.dlss.mode().map_or("by the sizes", Mode::name),
            preset = plan.asked.preset,
            optimal = ?recorded.optimal,
            hdr = camera.flags & plan::FFX_HIGH_DYNAMIC_RANGE != 0,
            depth_inverted = camera.depth_inverted(),
            auto_exposure = textures[EXPOSURE].is_none(),
            reactive = textures[REACTIVE].is_some(),
            display_motion_vectors = camera.display_motion_vectors(),
            jitter = ?recorded.jitter,
            mvec_scale = ?recorded.mvec_scale,
            near = camera.near,
            far = camera.far,
            fov = camera.fov_y,
            ?formats,
            evaluations = all,
            "DLSS: upscaling recorded in place of the title's FidelityFX"
        );
    }
    true
}

/// What [`record`] recorded.
struct Recorded {
    mode: u32,
    optimal: Option<[u32; 2]>,
    /// Another mode or size than the last time.
    changed: bool,
    jitter: [f32; 2],
    mvec_scale: [f32; 2],
}

/// The DLSS evaluation of `textures` recorded into the open command list
/// `list`: the mode whose render size holds the title's, the options, the
/// frame's constants (frame `id` of the upscaling's viewport), the textures
/// as local tags. The textures are left in the states given (Streamline
/// moves them to what it needs and back).
fn record(
    list: *mut c_void,
    textures: &Textures,
    camera: &plan::Camera,
    pre_exposure: f32,
    id: u64,
) -> Result<Recorded, String> {
    let (Some(plan), Some(sl), Some(functions)) = (PLAN.get(), SL.get(), FUNCTIONS.get()) else {
        return Err("Streamline is not ready".into());
    };
    let (Some(optimal_of), Some(set_options)) =
        (functions.dlss_optimal, functions.dlss_set_options)
    else {
        return Err("no DLSS functions".into());
    };
    let mut calls = lock(&CALLS);
    let output = camera.output;
    let mut sizes = Vec::new();
    for &mode in &plan::MODES {
        let known = calls
            .optimal
            .iter()
            .find(|(o, m, _)| *o == output && *m == mode)
            .map(|k| k.2);
        let size = known.unwrap_or_else(|| {
            let options = abi::DlssOptions::new(mode, output, plan.asked.preset);
            let mut settings = abi::DlssOptimalSettings::default();
            // SAFETY: filled options and a structure to fill.
            let code = unsafe { optimal_of(&options, &mut settings) };
            let size = (code == 0 && settings.optimal_render_width > 0).then_some([
                settings.optimal_render_width,
                settings.optimal_render_height,
            ]);
            calls.optimal.push((output, mode, size));
            size
        });
        sizes.push((mode, size));
    }
    let optimal_of_mode = |m: u32| sizes.iter().find(|s| s.0 == m).and_then(|s| s.1);
    let preferred = plan.asked.dlss.mode().map(Mode::sl_mode);
    let mode =
        plan::pick_mode(camera.render, output, preferred, optimal_of_mode).ok_or_else(|| {
            format!(
                "no DLSS mode takes a {:?} render for a {output:?} output",
                camera.render
            )
        })?;
    let mut options = abi::DlssOptions::new(mode, output, plan.asked.preset);
    options.pre_exposure = if pre_exposure > 0.0 {
        pre_exposure
    } else {
        1.0
    };
    options.color_buffers_hdr = abi::boolean(camera.flags & plan::FFX_HIGH_DYNAMIC_RANGE != 0);
    options.use_auto_exposure = abi::boolean(textures[EXPOSURE].is_none());
    let viewport = abi::ViewportHandle::new(VIEWPORT_DLSS);
    // SAFETY: filled options for our viewport.
    let code = unsafe { set_options(&viewport, &options) };
    if code != 0 {
        return Err(format!(
            "slDLSSSetOptions: {} ({code})",
            abi::result_name(code)
        ));
    }
    let changed = calls.last != Some((mode, camera.render, output));
    calls.last = Some((mode, camera.render, output));
    let constants = camera.constants(plan.asked.jitter, plan.asked.mvec);
    let kinds = [
        (COLOUR, abi::BUFFER_SCALING_INPUT_COLOR, camera.render),
        (DEPTH, abi::BUFFER_DEPTH, camera.render),
        (MOTION, abi::BUFFER_MOTION_VECTORS, camera.motion_size()),
        (EXPOSURE, abi::BUFFER_EXPOSURE, [1, 1]),
        (REACTIVE, abi::BUFFER_BIAS_CURRENT_COLOR_HINT, camera.render),
        (OUTPUT, abi::BUFFER_SCALING_OUTPUT_COLOR, output),
    ];
    let mut resources: Vec<(abi::Resource, u32, [u32; 2])> = kinds
        .iter()
        .filter_map(|&(i, kind, size)| {
            let (t, state) = textures[i].as_ref()?;
            Some((abi::Resource::texture(t.as_raw(), *state), kind, size))
        })
        .collect();
    let mut tags: Vec<abi::ResourceTag> = resources
        .iter_mut()
        .map(|(r, kind, size)| {
            abi::ResourceTag::new(
                r,
                *kind,
                abi::VALID_UNTIL_EVALUATE,
                abi::Extent::sized(*size),
            )
        })
        .collect();
    let mut result = Ok(());
    // The constants and the evaluation name one frame: one token for both,
    // none other made meanwhile.
    with_token(id as u32, |token| {
        // SAFETY: constants for this frame and viewport.
        let code = unsafe { (sl.set_constants)(&constants, token, &viewport) };
        if code != 0 {
            result = Err(format!(
                "slSetConstants: {} ({code})",
                abi::result_name(code)
            ));
            return;
        }
        let mut inputs: Vec<*const abi::BaseStructure> = vec![std::ptr::from_ref(&viewport.base)];
        inputs.extend(tags.iter_mut().map(|t| std::ptr::from_ref(&t.base)));
        // SAFETY: an open command list; inputs that live through the call.
        let code = unsafe {
            (sl.evaluate_feature)(
                abi::FEATURE_DLSS,
                token,
                inputs.as_ptr(),
                inputs.len() as u32,
                list,
            )
        };
        if code != 0 {
            result = Err(format!(
                "slEvaluateFeature: {} ({code})",
                abi::result_name(code)
            ));
        }
    });
    result?;
    Ok(Recorded {
        mode,
        optimal: optimal_of_mode(mode),
        changed,
        jitter: constants.jitter_offset,
        mvec_scale: constants.mvec_scale,
    })
}

/// The title's FidelityFX upscaler is destroyed: what DLSS made for it goes
/// once submitted work has run.
pub fn upscaler_destroyed() {
    let (Some(sl), true) = (SL.get(), DLSS_READY.load(Acquire)) else {
        return;
    };
    crate::external::finish_submitted();
    let viewport = abi::ViewportHandle::new(VIEWPORT_DLSS);
    let mut calls = lock(&CALLS);
    // SAFETY: our viewport; nothing that names it is in flight.
    let code = unsafe { (sl.free_resources)(abi::FEATURE_DLSS, &viewport) };
    calls.last = None;
    tracing::info!(
        code,
        result = abi::result_name(code),
        "DLSS: the title's upscaler was destroyed; DLSS let go of its resources"
    );
}

/// For the frame-rate line (`frames` presented since the last): what DLSS
/// did. `None` with Streamline's DLSS not loaded.
fn dlss_line(frames: f64) -> Option<String> {
    if !serving() {
        return None;
    }
    let per_frame = |c: &AtomicU64| c.swap(0, Relaxed) as f64 / frames.max(1.0);
    let (done, failed, forwarded) = (
        per_frame(&EVALUATED),
        per_frame(&FAILED),
        per_frame(&FORWARDED),
    );
    Some(format!(
        "DLSS per frame: {done:.2} upscalings by DLSS, {failed:.2} failed, {forwarded:.2} left to the title's FidelityFX"
    ))
}

/// The lines of the frame-rate line's second (`frames` presented since the
/// last), with Streamline's features on.
pub(crate) fn line(frames: f64, fps: f64) {
    if let Some(line) = dlss_line(frames) {
        tracing::info!("{line}");
    }
    if let Some(line) = framegen_line(frames, fps) {
        tracing::info!("{line}");
    }
}
