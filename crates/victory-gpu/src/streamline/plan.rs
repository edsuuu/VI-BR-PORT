//! What does not need the library: the switches as read, which DLSS mode a
//! title's render and output sizes call for, the FidelityFX states and flags
//! as the host and Streamline read them, the camera constants built from
//! what an upscaling names, and where the library is looked for.

use std::path::{Path, PathBuf};

use super::abi;

/// A DLSS mode by DLSS's own name: the render size over the output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dlaa,
    Quality,
    Balanced,
    Performance,
    UltraPerformance,
}

impl Mode {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "dlaa" | "native" => Mode::Dlaa,
            "quality" => Mode::Quality,
            "balanced" => Mode::Balanced,
            "performance" => Mode::Performance,
            "ultraperformance" | "ultra-performance" | "ultra_performance" => {
                Mode::UltraPerformance
            }
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Dlaa => "dlaa",
            Mode::Quality => "quality",
            Mode::Balanced => "balanced",
            Mode::Performance => "performance",
            Mode::UltraPerformance => "ultraperformance",
        }
    }

    /// The render size over the output size (DLSS's ratios).
    pub fn scale(self) -> f32 {
        match self {
            Mode::Dlaa => 1.0,
            Mode::Quality => 2.0 / 3.0,
            Mode::Balanced => 0.58,
            Mode::Performance => 0.5,
            Mode::UltraPerformance => 1.0 / 3.0,
        }
    }

    /// The `sl::DLSSMode` of the same ratio.
    pub fn sl_mode(self) -> u32 {
        match self {
            Mode::Dlaa => abi::DLSS_DLAA,
            Mode::Quality => abi::DLSS_MAX_QUALITY,
            Mode::Balanced => abi::DLSS_BALANCED,
            Mode::Performance => abi::DLSS_MAX_PERFORMANCE,
            Mode::UltraPerformance => abi::DLSS_ULTRA_PERFORMANCE,
        }
    }
}

/// `VICTORY_DLSS`: off (unset or `0`), the mode the sizes call for (`1`,
/// `auto`), or a mode by name. A named mode also sets the title's render
/// scale on its FidelityFX path to that mode's (unless `VICTORY_FFX_SCALE`
/// says another), in the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dlss {
    Off,
    Auto,
    Mode(Mode),
}

impl Dlss {
    /// The switch's value; an unknown word is an error, not a silent "off".
    pub fn from_switch(value: Option<&str>) -> Result<Self, String> {
        let Some(value) = value.map(str::trim) else {
            return Ok(Dlss::Off);
        };
        match value.to_ascii_lowercase().as_str() {
            "" | "0" | "off" => Ok(Dlss::Off),
            "1" | "on" | "auto" => Ok(Dlss::Auto),
            other => Mode::from_name(other).map(Dlss::Mode).ok_or_else(|| {
                format!(
                    "VICTORY_DLSS={value}: not one of 0, 1, auto, dlaa, quality, balanced, performance, ultraperformance"
                )
            }),
        }
    }

    pub fn on(self) -> bool {
        self != Dlss::Off
    }

    /// The mode asked for by name, if any.
    pub fn mode(self) -> Option<Mode> {
        match self {
            Dlss::Mode(mode) => Some(mode),
            _ => None,
        }
    }
}

/// `VICTORY_REFLEX`: Reflex low latency, with boost or without.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reflex {
    On,
    Boost,
}

impl Reflex {
    /// The switch's value: `None` off (unset, `0`, `off`); an unknown word is
    /// an error.
    pub fn from_switch(value: Option<&str>) -> Result<Option<Self>, String> {
        let Some(value) = value.map(str::trim) else {
            return Ok(None);
        };
        match value.to_ascii_lowercase().as_str() {
            "" | "0" | "off" => Ok(None),
            "1" | "on" => Ok(Some(Reflex::On)),
            "boost" => Ok(Some(Reflex::Boost)),
            _ => Err(format!(
                "VICTORY_REFLEX={value}: not one of 0, on, 1, boost"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Reflex::On => "on",
            Reflex::Boost => "boost",
        }
    }

    pub fn sl_mode(self) -> u32 {
        match self {
            Reflex::On => abi::REFLEX_LOW_LATENCY,
            Reflex::Boost => abi::REFLEX_LOW_LATENCY_WITH_BOOST,
        }
    }
}

/// `VICTORY_DLSS_PRESET`: the DLSS model for every mode (`sl::DLSSPreset`).
pub fn preset(name: &str) -> Option<u32> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "" | "default" => 0,
        "e" => 5,
        "f" => 6,
        "j" => 10,
        "k" => 11,
        "l" => 12,
        "m" => 13,
        _ => return None,
    })
}

/// The name of a `sl::DLSSMode`.
pub fn sl_mode_name(mode: u32) -> &'static str {
    match mode {
        abi::DLSS_OFF => "off",
        abi::DLSS_MAX_PERFORMANCE => "performance",
        abi::DLSS_BALANCED => "balanced",
        abi::DLSS_MAX_QUALITY => "quality",
        abi::DLSS_ULTRA_PERFORMANCE => "ultra performance",
        abi::DLSS_ULTRA_QUALITY => "ultra quality",
        abi::DLSS_DLAA => "DLAA",
        _ => "?",
    }
}

/// The DLSS modes tried for a render size, largest render size first.
pub const MODES: [u32; 6] = [
    abi::DLSS_DLAA,
    abi::DLSS_ULTRA_QUALITY,
    abi::DLSS_MAX_QUALITY,
    abi::DLSS_BALANCED,
    abi::DLSS_MAX_PERFORMANCE,
    abi::DLSS_ULTRA_PERFORMANCE,
];

/// The DLSS mode for a title that renders `render` for an output of
/// `output`: `preferred` if its optimal render size (`optimal`, from the
/// library) holds `render`, else of the modes whose optimal size holds it
/// (DLSS makes its feature for that size and takes smaller ones as a
/// sub-rectangle) the one with the smallest.
pub fn pick_mode(
    render: [u32; 2],
    output: [u32; 2],
    preferred: Option<u32>,
    optimal: impl Fn(u32) -> Option<[u32; 2]>,
) -> Option<u32> {
    if render[0] == 0 || render[1] == 0 || render[0] > output[0] || render[1] > output[1] {
        return None;
    }
    let holds = |size: [u32; 2]| {
        size[0] >= render[0] && size[1] >= render[1] && size[0] <= output[0] && size[1] <= output[1]
    };
    if let Some(mode) = preferred
        && optimal(mode).is_some_and(holds)
    {
        return Some(mode);
    }
    MODES
        .iter()
        .filter_map(|&mode| optimal(mode).map(|size| (mode, size)))
        .filter(|&(_, size)| holds(size))
        .min_by_key(|&(_, size)| u64::from(size[0]) * u64::from(size[1]))
        .map(|(mode, _)| mode)
}

// FidelityFX API upscale flags (`FFX_UPSCALE_ENABLE_*`).
pub const FFX_HIGH_DYNAMIC_RANGE: u32 = 1 << 0;
pub const FFX_DISPLAY_RESOLUTION_MOTION_VECTORS: u32 = 1 << 1;
pub const FFX_MOTION_VECTORS_JITTER_CANCELLATION: u32 = 1 << 2;
pub const FFX_DEPTH_INVERTED: u32 = 1 << 3;
pub const FFX_DEPTH_INFINITE: u32 = 1 << 4;
pub const FFX_AUTO_EXPOSURE: u32 = 1 << 5;

/// A FidelityFX resource state as the host resource state it means (as the
/// PC backend reads it); `None` for generic read and indirect arguments,
/// which a texture DLSS reads cannot be in.
pub fn host_state(state: u32) -> Option<u32> {
    Some(match state {
        0x1 | 0x80 => 0,  // common, present
        0x2 => 0x8,       // unordered access
        0x4 => 0x40,      // compute read
        0x8 => 0x80,      // pixel read
        0xc => 0xc0,      // both
        0x10 => 0x800,    // copy source
        0x20 => 0x400,    // copy destination
        0x100 => 0x4,     // render target
        0x200 => 0x10,    // depth write
        _ => return None, // generic read, indirect arguments
    })
}

/// The host state DLSS is told an input is in: the one the title gave, or,
/// where that cannot be a texture's, shader resource.
pub fn input_state(ffx_state: u32) -> u32 {
    host_state(ffx_state)
        .filter(|&s| s != 0x800 && s != 0x400 && s != 0)
        .unwrap_or(0xc0)
}

/// The host state DLSS is told the output is in: unordered access.
pub fn output_state(ffx_state: u32) -> u32 {
    host_state(ffx_state).filter(|&s| s == 0x8).unwrap_or(0x8)
}

/// What an upscaling says of the camera, and of how to read its inputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub jitter: [f32; 2],
    /// Multiplies the motion vectors into render pixels (FidelityFX).
    pub motion_scale: [f32; 2],
    pub render: [u32; 2],
    pub output: [u32; 2],
    pub near: f32,
    pub far: f32,
    pub fov_y: f32,
    pub reset: bool,
    /// The context's creation flags (`FFX_*` above).
    pub flags: u32,
}

impl Camera {
    pub fn depth_inverted(&self) -> bool {
        self.flags & FFX_DEPTH_INVERTED != 0
    }

    pub fn display_motion_vectors(&self) -> bool {
        self.flags & FFX_DISPLAY_RESOLUTION_MOTION_VECTORS != 0
    }

    /// The camera with its nearer plane as the near one. FidelityFX takes
    /// the two distances in either order, and a title whose depth is
    /// inverted may name the far plane first; Streamline's constants name
    /// them as they are (frame generation reads them).
    pub fn planes_in_order(&self) -> Camera {
        let mut camera = *self;
        if self.near > self.far && self.far > 0.0 {
            (camera.near, camera.far) = (self.far, self.near);
        }
        camera
    }

    /// The size the motion vectors cover.
    pub fn motion_size(&self) -> [u32; 2] {
        if self.display_motion_vectors() {
            self.output
        } else {
            self.render
        }
    }

    /// Streamline's motion vector scale: it multiplies by the render size
    /// itself before handing the scale to DLSS, which then gets FidelityFX's
    /// scale back.
    pub fn mvec_scale(&self, multiplier: [f32; 2]) -> [f32; 2] {
        let size = self.motion_size();
        [0, 1].map(|i| self.motion_scale[i] * multiplier[i] / size[i].max(1) as f32)
    }

    /// The common constants of a frame. The title names no camera matrices:
    /// the projection is made of the field of view, the aspect and the depth
    /// range it gives; camera motion is in the motion vectors, so the
    /// clip-to-previous-clip matrices stay the identity and the camera stands
    /// at the origin looking down +z.
    pub fn constants(
        &self,
        jitter_multiplier: [f32; 2],
        mvec_multiplier: [f32; 2],
    ) -> abi::Constants {
        let aspect = self.render[0].max(1) as f32 / self.render[1].max(1) as f32;
        let view_to_clip = projection(
            self.near,
            self.far,
            self.fov_y,
            aspect,
            self.depth_inverted(),
        );
        let clip_to_view = invert(&view_to_clip).unwrap_or(IDENTITY);
        abi::Constants {
            base: abi::BaseStructure::new(abi::CONSTANTS, 2),
            camera_view_to_clip: view_to_clip,
            clip_to_camera_view: clip_to_view,
            clip_to_lens_clip: IDENTITY,
            clip_to_prev_clip: IDENTITY,
            prev_clip_to_clip: IDENTITY,
            jitter_offset: [
                self.jitter[0] * jitter_multiplier[0],
                self.jitter[1] * jitter_multiplier[1],
            ],
            mvec_scale: self.mvec_scale(mvec_multiplier),
            camera_pinhole_offset: [0.0, 0.0],
            camera_pos: [0.0, 0.0, 0.0],
            camera_up: [0.0, 1.0, 0.0],
            camera_right: [1.0, 0.0, 0.0],
            camera_fwd: [0.0, 0.0, 1.0],
            camera_near: self.near,
            camera_far: self.far,
            camera_fov: self.fov_y,
            camera_aspect_ratio: aspect,
            motion_vectors_invalid_value: 0.0,
            depth_inverted: abi::boolean(self.depth_inverted()),
            camera_motion_included: abi::TRUE,
            motion_vectors_3d: abi::FALSE,
            reset: abi::boolean(self.reset),
            orthographic_projection: abi::FALSE,
            motion_vectors_dilated: abi::FALSE,
            motion_vectors_jittered: abi::boolean(
                self.flags & FFX_MOTION_VECTORS_JITTER_CANCELLATION != 0,
            ),
            min_relative_linear_depth_object_separation: 40.0,
        }
    }
}

pub const IDENTITY: abi::Float4x4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// A left-handed perspective projection in Streamline's layout (row vectors,
/// rows as stored): depth 0 at `near` and 1 at `far`, or reversed; a far
/// plane past 1e30 (or not finite) is at infinity. Degenerate inputs give
/// the identity.
pub fn projection(near: f32, far: f32, fov_y: f32, aspect: f32, inverted: bool) -> abi::Float4x4 {
    if !(near > 0.0 && fov_y > 0.0 && fov_y < std::f32::consts::PI && aspect > 0.0) {
        return IDENTITY;
    }
    let y = 1.0 / (fov_y * 0.5).tan();
    let x = y / aspect;
    let infinite = !far.is_finite() || far > 1e30 || far <= near;
    let (a, b) = match (infinite, inverted) {
        (false, false) => (far / (far - near), -near * far / (far - near)),
        (false, true) => (near / (near - far), far * near / (far - near)),
        (true, false) => (1.0, -near),
        (true, true) => (0.0, near),
    };
    [
        [x, 0.0, 0.0, 0.0],
        [0.0, y, 0.0, 0.0],
        [0.0, 0.0, a, 1.0],
        [0.0, 0.0, b, 0.0],
    ]
}

/// A 4x4 inverse by Gauss-Jordan elimination with partial pivoting; `None`
/// if singular.
pub fn invert(m: &abi::Float4x4) -> Option<abi::Float4x4> {
    let mut a: [[f64; 8]; 4] = std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            if c < 4 {
                f64::from(m[r][c])
            } else if c - 4 == r {
                1.0
            } else {
                0.0
            }
        })
    });
    for col in 0..4 {
        let pivot = (col..4).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 || !a[pivot][col].is_finite() {
            return None;
        }
        a.swap(col, pivot);
        let p = a[col][col];
        a[col].iter_mut().for_each(|v| *v /= p);
        for r in 0..4 {
            if r != col {
                let f = a[r][col];
                let pivot_row = a[col];
                a[r].iter_mut()
                    .zip(pivot_row)
                    .for_each(|(v, p)| *v -= f * p);
            }
        }
    }
    Some(std::array::from_fn(|r| {
        std::array::from_fn(|c| a[r][c + 4] as f32)
    }))
}

/// `"x,y"` as two multipliers (`VICTORY_DLSS_JITTER`, `VICTORY_DLSS_MV`):
/// what a run may have to flip; unset or malformed is `[1, 1]`.
pub fn pair(value: Option<&str>) -> [f32; 2] {
    let Some(value) = value else {
        return [1.0, 1.0];
    };
    let mut parts = value.split(',').map(|p| p.trim().parse::<f32>());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(x)), Some(Ok(y)), None) if x.is_finite() && y.is_finite() => [x, y],
        _ => [1.0, 1.0],
    }
}

/// The library's file name.
pub const INTERPOSER: &str = "sl.interposer.dll";

/// The folders the library is looked for in, in order: the one
/// `VICTORY_STREAMLINE_DIR` names, then `streamline` beside our executable,
/// then our executable's own folder. No NVIDIA file is part of this
/// repository: the user puts them there.
pub fn candidate_dirs(named: Option<&Path>, exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = named.map(Path::to_path_buf).into_iter().collect();
    if let Some(exe) = exe_dir {
        dirs.push(exe.join("streamline"));
        dirs.push(exe.to_path_buf());
    }
    dirs
}

/// The first of `dirs` that holds the library, as `has` says.
pub fn find_dir(dirs: &[PathBuf], has: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    dirs.iter().find(|d| has(&d.join(INTERPOSER))).cloned()
}

/// The back buffer frame generation presents into for planes of these
/// sizes, bottom first: the first plane's (the scene, which is then exactly
/// the colour without the interface the guide asks for, at the back
/// buffer's size), where our compose makes one as large as the largest
/// plane (Series S: a 3840x2160 interface over a 1920x1080 scene).
pub fn framegen_back_buffer(planes: &[[u32; 2]]) -> Option<[u32; 2]> {
    planes.first().copied().filter(|s| s[0] > 0 && s[1] > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-ins for DLSS's optimal render sizes for a 1920x1080 output:
    /// its ratios applied (not read from the library; ultra quality has none).
    fn optimal_1080p(mode: u32) -> Option<[u32; 2]> {
        Some(match mode {
            abi::DLSS_DLAA => [1920, 1080],
            abi::DLSS_MAX_QUALITY => [1280, 720],
            abi::DLSS_BALANCED => [1114, 626],
            abi::DLSS_MAX_PERFORMANCE => [960, 540],
            abi::DLSS_ULTRA_PERFORMANCE => [640, 360],
            _ => return None,
        })
    }

    #[test]
    fn series_s_render_sizes_pick_their_mode() {
        let out = [1920, 1080];
        // The Series S FidelityFX path: 1280x720 for 1920x1080 is quality.
        assert_eq!(
            pick_mode([1280, 720], out, None, optimal_1080p),
            Some(abi::DLSS_MAX_QUALITY)
        );
        // VICTORY_FFX_SCALE=0.5: 960x540, performance.
        assert_eq!(
            pick_mode([960, 540], out, None, optimal_1080p),
            Some(abi::DLSS_MAX_PERFORMANCE)
        );
        // A size between modes takes the next larger one.
        assert_eq!(
            pick_mode([1100, 620], out, None, optimal_1080p),
            Some(abi::DLSS_BALANCED)
        );
        // A named mode is kept while its size holds the render, else the sizes decide.
        assert_eq!(
            pick_mode([960, 540], out, Some(abi::DLSS_MAX_QUALITY), optimal_1080p),
            Some(abi::DLSS_MAX_QUALITY)
        );
        assert_eq!(
            pick_mode(
                [1280, 720],
                out,
                Some(abi::DLSS_MAX_PERFORMANCE),
                optimal_1080p
            ),
            Some(abi::DLSS_MAX_QUALITY)
        );
        // Nothing for a render larger than the output, or empty.
        assert_eq!(pick_mode([1921, 1080], out, None, optimal_1080p), None);
        assert_eq!(pick_mode([0, 1080], out, None, optimal_1080p), None);
        assert_eq!(pick_mode([1280, 720], out, None, |_| None), None);
    }

    #[test]
    fn switches_read_as_documented() {
        assert_eq!(Dlss::from_switch(None), Ok(Dlss::Off));
        assert_eq!(Dlss::from_switch(Some("0")), Ok(Dlss::Off));
        assert_eq!(Dlss::from_switch(Some("1")), Ok(Dlss::Auto));
        assert_eq!(
            Dlss::from_switch(Some(" Performance ")),
            Ok(Dlss::Mode(Mode::Performance))
        );
        assert!(Dlss::from_switch(Some("fast")).is_err());
        assert_eq!(Dlss::Mode(Mode::Balanced).mode(), Some(Mode::Balanced));
        assert!(!Dlss::Off.on() && Dlss::Auto.on());
        assert_eq!(Reflex::from_switch(None), Ok(None));
        assert_eq!(Reflex::from_switch(Some("off")), Ok(None));
        assert_eq!(Reflex::from_switch(Some("1")), Ok(Some(Reflex::On)));
        assert_eq!(Reflex::from_switch(Some("BOOST")), Ok(Some(Reflex::Boost)));
        assert!(Reflex::from_switch(Some("max")).is_err());
        assert_eq!(Reflex::Boost.sl_mode(), abi::REFLEX_LOW_LATENCY_WITH_BOOST);
        assert_eq!(
            Mode::from_name("ultra-performance"),
            Some(Mode::UltraPerformance)
        );
        for mode in [
            Mode::Dlaa,
            Mode::Quality,
            Mode::Balanced,
            Mode::Performance,
            Mode::UltraPerformance,
        ] {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::Performance.scale(), 0.5);
        assert_eq!(preset("K"), Some(11));
        assert_eq!(preset("default"), Some(0));
        assert_eq!(preset("a"), None);
        assert_eq!(sl_mode_name(abi::DLSS_DLAA), "DLAA");
    }

    #[test]
    fn the_library_is_looked_for_in_order() {
        let named = Path::new(r"D:\nv");
        let exe = Path::new(r"C:\v");
        let dirs = candidate_dirs(Some(named), Some(exe));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from(r"D:\nv"),
                PathBuf::from(r"C:\v\streamline"),
                PathBuf::from(r"C:\v")
            ]
        );
        assert_eq!(candidate_dirs(None, Some(exe)).len(), 2);
        let found = find_dir(&dirs, |p| p.starts_with(r"C:\v\streamline"));
        assert_eq!(found, Some(PathBuf::from(r"C:\v\streamline")));
        assert_eq!(find_dir(&dirs, |_| false), None);
    }

    #[test]
    fn frame_generation_presents_at_the_scene_size() {
        assert_eq!(
            framegen_back_buffer(&[[1920, 1080], [3840, 2160]]),
            Some([1920, 1080])
        );
        assert_eq!(framegen_back_buffer(&[]), None);
        assert_eq!(framegen_back_buffer(&[[0, 1080]]), None);
    }

    #[test]
    fn states_read_as_the_pc_backend_reads_them() {
        assert_eq!(host_state(0xc), Some(0xc0));
        assert_eq!(host_state(0x2), Some(0x8));
        assert_eq!(host_state(0x200), Some(0x10));
        assert_eq!(host_state(0x14), None);
        assert_eq!(input_state(0x14), 0xc0);
        assert_eq!(input_state(0x10), 0xc0);
        assert_eq!(input_state(0x4), 0x40);
        assert_eq!(input_state(0x200), 0x10);
        assert_eq!(output_state(0x2), 0x8);
        assert_eq!(output_state(0x100), 0x8);
    }

    fn camera(flags: u32) -> Camera {
        Camera {
            jitter: [0.25, -0.5],
            motion_scale: [1280.0, 720.0],
            render: [1280, 720],
            output: [1920, 1080],
            near: 0.1,
            far: 10_000.0,
            fov_y: 1.0,
            reset: true,
            flags,
        }
    }

    #[test]
    fn constants_follow_the_upscaling() {
        let c = camera(FFX_HIGH_DYNAMIC_RANGE | FFX_DEPTH_INVERTED | FFX_AUTO_EXPOSURE)
            .constants([1.0, 1.0], [1.0, 1.0]);
        assert_eq!(c.mvec_scale, [1.0, 1.0]);
        assert_eq!(c.jitter_offset, [0.25, -0.5]);
        assert_eq!(
            (c.depth_inverted, c.reset, c.camera_motion_included),
            (abi::TRUE, abi::TRUE, abi::TRUE)
        );
        assert_eq!(c.motion_vectors_jittered, abi::FALSE);
        assert!((c.camera_aspect_ratio - 1280.0 / 720.0).abs() < 1e-6);
        let flipped = camera(0).constants([-1.0, 1.0], [1.0, -2.0]);
        assert_eq!(flipped.jitter_offset, [-0.25, -0.5]);
        assert_eq!(flipped.mvec_scale, [1.0, -2.0]);
        assert_eq!(flipped.depth_inverted, abi::FALSE);
        let display = camera(FFX_DISPLAY_RESOLUTION_MOTION_VECTORS);
        assert_eq!(display.motion_size(), [1920, 1080]);
        assert_eq!(
            display.mvec_scale([1.0, 1.0]),
            [1280.0 / 1920.0, 720.0 / 1080.0]
        );
    }

    /// Points at the near and far planes land at the depth the convention
    /// says; the inverse maps clip space back.
    #[test]
    fn projections_place_near_and_far() {
        let depth = |m: &abi::Float4x4, z: f32| (z * m[2][2] + m[3][2]) / (z * m[2][3] + m[3][3]);
        let standard = projection(0.5, 100.0, 1.2, 16.0 / 9.0, false);
        assert!(depth(&standard, 0.5).abs() < 1e-5);
        assert!((depth(&standard, 100.0) - 1.0).abs() < 1e-5);
        let reversed = projection(0.5, 100.0, 1.2, 16.0 / 9.0, true);
        assert!((depth(&reversed, 0.5) - 1.0).abs() < 1e-5);
        assert!(depth(&reversed, 100.0).abs() < 1e-5);
        let infinite = projection(0.5, f32::MAX, 1.2, 1.0, true);
        assert!((depth(&infinite, 0.5) - 1.0).abs() < 1e-5);
        assert_eq!(projection(0.0, 100.0, 1.2, 1.0, false), IDENTITY);
        let inverse = invert(&standard).unwrap();
        for r in 0..4 {
            for c in 0..4 {
                let v: f32 = (0..4).map(|k| standard[r][k] * inverse[k][c]).sum();
                let want = if r == c { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-4, "{r} {c} {v}");
            }
        }
        assert_eq!(invert(&[[0.0; 4]; 4]), None);
    }

    #[test]
    fn planes_named_far_first_are_put_in_order() {
        let mut named = camera(FFX_DEPTH_INVERTED);
        (named.near, named.far) = (10_000.0, 0.15);
        let ordered = named.planes_in_order();
        assert_eq!((ordered.near, ordered.far), (0.15, 10_000.0));
        assert_eq!(ordered.planes_in_order(), ordered);
        let mut endless = camera(FFX_DEPTH_INVERTED);
        (endless.near, endless.far) = (0.15, 0.0);
        assert_eq!(endless.planes_in_order(), endless);
    }

    #[test]
    fn multiplier_pairs() {
        assert_eq!(pair(None), [1.0, 1.0]);
        assert_eq!(pair(Some("-1, 1")), [-1.0, 1.0]);
        assert_eq!(pair(Some("1")), [1.0, 1.0]);
        assert_eq!(pair(Some("1,2,3")), [1.0, 1.0]);
        assert_eq!(pair(Some("x,1")), [1.0, 1.0]);
    }
}
