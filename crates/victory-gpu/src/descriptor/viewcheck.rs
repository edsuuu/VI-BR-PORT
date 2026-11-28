//! Checks of the title's texture views and samplers against what the host
//! accepts, before they reach `CreateShaderResourceView`,
//! `CreateUnorderedAccessView` and `CreateSampler`.
//!
//! The console takes any view; the host removes the device (reason
//! `DXGI_ERROR_INVALID_CALL`) for one it refuses, and those calls return
//! nothing to tell. Buffer views are checked already (`buffer_view_ok`);
//! texture views and samplers went almost as the title wrote them. A view
//! here is compared with the host copy it views: its dimension, its mip,
//! array, plane and cube ranges, its format and component mapping; a
//! sampler with the limits of its fields. What can be fitted is moved to
//! the nearest valid description; what cannot is refused (the slot gets a
//! null view). Each kind of fault is logged once with the title's values,
//! and the 5-second line counts them.
//!
//! Off by default; `VICTORY_VIEW_CHECK=1` turns it on (api-viewcheck,
//! after the device removal of run barco-ab-f1). Not a translator switch.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

/// Views (and samplers) moved to a valid description.
pub(crate) static FITTED: AtomicU64 = AtomicU64::new(0);
/// Views left null as the host would refuse them.
pub(crate) static REFUSED: AtomicU64 = AtomicU64::new(0);

/// Whether `VICTORY_VIEW_CHECK` is on (off by default).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_VIEW_CHECK"))
}

/// What a check made of a description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Valid as it was.
    Fine,
    /// Moved to a valid description; the first fault found.
    Fitted(&'static str),
    /// No valid description near it: leave the slot null.
    Refused(&'static str),
}

/// The faults of one check: the first one names the verdict.
#[derive(Default)]
struct Faults {
    fitted: Option<&'static str>,
}

impl Faults {
    fn fit(&mut self, kind: &'static str) {
        self.fitted.get_or_insert(kind);
    }
    fn verdict(self) -> Verdict {
        self.fitted.map_or(Verdict::Fine, Verdict::Fitted)
    }
}

// Resource dimensions.
const TEX1D: i32 = 2;
const TEX2D: i32 = 3;
const TEX3D: i32 = 4;

/// The levels a texture has: its own count, or the whole chain if 0.
fn levels_of(r: &D3D12_RESOURCE_DESC) -> u32 {
    let own = u32::from(r.MipLevels);
    if own > 0 {
        return own;
    }
    let depth = if r.Dimension.0 == TEX3D { u64::from(r.DepthOrArraySize) } else { 1 };
    let largest = r.Width.max(u64::from(r.Height)).max(depth).max(1);
    64 - largest.leading_zeros()
}

/// The array slices a texture has (one for a 3D texture).
fn slices_of(r: &D3D12_RESOURCE_DESC) -> u32 {
    if r.Dimension.0 == TEX3D {
        1
    } else {
        u32::from(r.DepthOrArraySize).max(1)
    }
}

/// The planes a format has: two for depth-stencil and video formats.
fn planes_of(format: u32) -> u32 {
    match format {
        // Depth-stencil; NV12, P010, P016, 420_OPAQUE, NV11, P208.
        19..=22 | 44..=47 | 103..=106 | 110 | 130 => 2,
        _ => 1,
    }
}

/// A typed format a view of a typeless (or depth-only) `format` can read
/// as, if the view named one the host does not take in a view: depth
/// formats read as their colour formats, a typeless one as the first
/// typed format of its family.
fn readable(format: u32, plane: u32) -> Option<u32> {
    let stencil = plane == 1;
    Some(match format {
        // R32G8X24 and D32S8.
        19 | 20 => {
            if stencil {
                22
            } else {
                21
            }
        }
        // R32 typeless and D32.
        39 | 40 => 41,
        // R24G8 and D24S8.
        44 | 45 => {
            if stencil {
                47
            } else {
                46
            }
        }
        // R16 typeless and D16.
        53 | 55 => 56,
        // B8G8R8A8 and B8G8R8X8 typeless: their UNORM formats, which the
        // table of families keeps apart from them.
        90 => 87,
        92 => 88,
        // Other typeless formats: the first of the family after them,
        // which is a typed one ([`crate::resource::family`]).
        _ if crate::resource::family(format) == format && crate::resource::family(format + 1) == format => format + 1,
        _ => return None,
    })
}

/// Fits `format` for a view: depth and typeless formats to a readable one.
fn fit_format(format: &mut DXGI_FORMAT, plane: u32, faults: &mut Faults) {
    let f = format.0 as u32;
    if f == 0 {
        return;
    }
    if let Some(typed) = readable(f, plane) {
        format.0 = typed as i32;
        faults.fit("typeless or depth view format");
    }
}

/// The ranges of a texture view, whatever its dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Range {
    most: u32,
    levels: u32,
    first: u32,
    size: u32,
    plane: u32,
    clamp: f32,
}

// Shader view dimensions.
const S_BUFFER: i32 = 1;
const S_1D: i32 = 2;
const S_1DA: i32 = 3;
const S_2D: i32 = 4;
const S_2DA: i32 = 5;
const S_2DMS: i32 = 6;
const S_2DMSA: i32 = 7;
const S_3D: i32 = 8;
const S_CUBE: i32 = 9;
const S_CUBEA: i32 = 10;

/// The ranges of a shader view description of a texture.
fn srv_range(d: &D3D12_SHADER_RESOURCE_VIEW_DESC) -> Range {
    let mut r = Range {
        most: 0,
        levels: 1,
        first: 0,
        size: 1,
        plane: 0,
        clamp: 0.0,
    };
    // SAFETY (each arm): the member the dimension names.
    unsafe {
        match d.ViewDimension.0 {
            S_1D => {
                let t = d.Anonymous.Texture1D;
                (r.most, r.levels, r.clamp) = (t.MostDetailedMip, t.MipLevels, t.ResourceMinLODClamp);
            }
            S_1DA => {
                let t = d.Anonymous.Texture1DArray;
                (r.most, r.levels, r.first, r.size, r.clamp) =
                    (t.MostDetailedMip, t.MipLevels, t.FirstArraySlice, t.ArraySize, t.ResourceMinLODClamp);
            }
            S_2D => {
                let t = d.Anonymous.Texture2D;
                (r.most, r.levels, r.plane, r.clamp) = (t.MostDetailedMip, t.MipLevels, t.PlaneSlice, t.ResourceMinLODClamp);
            }
            S_2DA => {
                let t = d.Anonymous.Texture2DArray;
                (r.most, r.levels, r.first, r.size, r.plane, r.clamp) = (
                    t.MostDetailedMip,
                    t.MipLevels,
                    t.FirstArraySlice,
                    t.ArraySize,
                    t.PlaneSlice,
                    t.ResourceMinLODClamp,
                );
            }
            S_2DMSA => {
                let t = d.Anonymous.Texture2DMSArray;
                (r.first, r.size) = (t.FirstArraySlice, t.ArraySize);
            }
            S_3D => {
                let t = d.Anonymous.Texture3D;
                (r.most, r.levels, r.clamp) = (t.MostDetailedMip, t.MipLevels, t.ResourceMinLODClamp);
            }
            S_CUBE => {
                let t = d.Anonymous.TextureCube;
                (r.most, r.levels, r.size, r.clamp) = (t.MostDetailedMip, t.MipLevels, 6, t.ResourceMinLODClamp);
            }
            S_CUBEA => {
                let t = d.Anonymous.TextureCubeArray;
                (r.most, r.levels, r.first, r.size, r.clamp) = (
                    t.MostDetailedMip,
                    t.MipLevels,
                    t.First2DArrayFace,
                    t.NumCubes.saturating_mul(6),
                    t.ResourceMinLODClamp,
                );
            }
            _ => {}
        }
    }
    r
}

/// Writes `r` into `d` for its dimension.
fn put_srv_range(d: &mut D3D12_SHADER_RESOURCE_VIEW_DESC, r: Range) {
    d.Anonymous = match d.ViewDimension.0 {
        S_1D => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture1D: D3D12_TEX1D_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                ResourceMinLODClamp: r.clamp,
            },
        },
        S_1DA => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture1DArray: D3D12_TEX1D_ARRAY_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                FirstArraySlice: r.first,
                ArraySize: r.size,
                ResourceMinLODClamp: r.clamp,
            },
        },
        S_2D => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                PlaneSlice: r.plane,
                ResourceMinLODClamp: r.clamp,
            },
        },
        S_2DA => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2DArray: D3D12_TEX2D_ARRAY_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                FirstArraySlice: r.first,
                ArraySize: r.size,
                PlaneSlice: r.plane,
                ResourceMinLODClamp: r.clamp,
            },
        },
        S_2DMS => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2DMS: D3D12_TEX2DMS_SRV::default(),
        },
        S_2DMSA => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2DMSArray: D3D12_TEX2DMS_ARRAY_SRV {
                FirstArraySlice: r.first,
                ArraySize: r.size,
            },
        },
        S_3D => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture3D: D3D12_TEX3D_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                ResourceMinLODClamp: r.clamp,
            },
        },
        S_CUBE => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            TextureCube: D3D12_TEXCUBE_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                ResourceMinLODClamp: r.clamp,
            },
        },
        S_CUBEA => D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            TextureCubeArray: D3D12_TEXCUBE_ARRAY_SRV {
                MostDetailedMip: r.most,
                MipLevels: r.levels,
                First2DArrayFace: r.first,
                NumCubes: r.size / 6,
                ResourceMinLODClamp: r.clamp,
            },
        },
        _ => return,
    };
}

/// Fits the levels `most`, `levels` (`u32::MAX`: all from `most`) to a
/// texture of `mips` levels.
fn fit_levels(most: &mut u32, levels: &mut u32, mips: u32, faults: &mut Faults) {
    if *most >= mips {
        *most = mips - 1;
        faults.fit("first level past the texture's");
    }
    if *levels == u32::MAX {
        return;
    }
    if *levels == 0 {
        *levels = u32::MAX;
        faults.fit("no levels");
    } else if *levels > mips - *most {
        *levels = mips - *most;
        faults.fit("levels past the texture's");
    }
}

/// Fits the slices `first`, `size` (`u32::MAX`: all from `first`) to a
/// texture of `slices`.
fn fit_slices(first: &mut u32, size: &mut u32, slices: u32, faults: &mut Faults) {
    if *first >= slices {
        *first = slices - 1;
        faults.fit("first slice past the texture's");
    }
    if *size == u32::MAX {
        *size = slices - *first;
    } else if *size == 0 {
        *size = slices - *first;
        faults.fit("no slices");
    } else if *size > slices - *first {
        *size = slices - *first;
        faults.fit("slices past the texture's");
    }
}

/// Fits the plane of a view in `view` format of a texture in `own`: within
/// the format's planes, and the one a plane's own format names (the depth
/// plane's formats 21 and 46 read plane 0, the stencil's 22 and 47 plane 1).
fn fit_plane(plane: &mut u32, own: u32, view: u32, faults: &mut Faults) {
    let named = match view {
        21 | 46 => Some(0),
        22 | 47 => Some(1),
        _ => None,
    };
    if let Some(named) = named
        && *plane != named
        && planes_of(own) == 2
    {
        *plane = named;
        faults.fit("plane its format does not name");
    } else if *plane >= planes_of(own) {
        *plane = 0;
        faults.fit("plane the format does not have");
    }
}

/// Fits the component mapping: four selectors of 0 to 5 and the bit the
/// host requires.
fn fit_mapping(mapping: &mut u32, faults: &mut Faults) {
    const ALWAYS: u32 = 1 << 12;
    let mut fitted = *mapping & ALWAYS != 0 && *mapping & !0x1fff == 0;
    let mut out = ALWAYS;
    for c in 0..4 {
        let mut s = (*mapping >> (3 * c)) & 7;
        if s > 5 {
            s = c;
            fitted = false;
        }
        out |= s << (3 * c);
    }
    if !fitted {
        *mapping = out;
        faults.fit("component mapping");
    }
}

/// Checks (and fits) a shader view `d` of the texture `r`.
pub(crate) fn shader_view(r: &D3D12_RESOURCE_DESC, d: &mut D3D12_SHADER_RESOURCE_VIEW_DESC) -> Verdict {
    let dim = r.Dimension.0;
    if !matches!(dim, TEX1D | TEX2D | TEX3D) {
        return Verdict::Fine;
    }
    let view = d.ViewDimension.0;
    if view == S_BUFFER {
        return Verdict::Refused("buffer view of a texture");
    }
    if !(S_1D..=S_CUBEA).contains(&view) {
        return Verdict::Refused("unknown view dimension");
    }
    let mut faults = Faults::default();
    let mut range = srv_range(d);
    let multisampled = r.SampleDesc.Count > 1;
    let mips = levels_of(r);
    let slices = slices_of(r);
    let array_view = matches!(view, S_1DA | S_2DA | S_2DMSA | S_CUBEA);
    let cube_view = matches!(view, S_CUBE | S_CUBEA);
    // The view's dimension, as the resource's allows: the nearest one.
    let fitted = match dim {
        TEX1D => {
            if array_view || range.size > 1 && view != S_1D {
                S_1DA
            } else {
                S_1D
            }
        }
        TEX3D => S_3D,
        _ if multisampled => {
            if array_view || slices > 1 {
                S_2DMSA
            } else {
                S_2DMS
            }
        }
        _ if cube_view && slices >= 6 => {
            if view == S_CUBE && range.first == 0 {
                S_CUBE
            } else {
                S_CUBEA
            }
        }
        _ if view == S_2D || (view == S_1D || view == S_2DMS) && slices == 1 => S_2D,
        _ if view == S_3D || cube_view => S_2DA,
        _ if view == S_1D || view == S_2DMS => S_2D,
        _ if view == S_1DA || view == S_2DMSA => S_2DA,
        _ => view,
    };
    if fitted != view {
        faults.fit("view dimension the texture's does not allow");
        // A 3D view of an array: all of its slices.
        if view == S_3D {
            range.first = 0;
            range.size = u32::MAX;
        }
        if matches!(fitted, S_2DMS | S_2DMSA) {
            range.most = 0;
            range.levels = 1;
        }
        if view == S_2DMS || view == S_2DMSA {
            range.levels = u32::MAX;
        }
        d.ViewDimension.0 = fitted;
    }
    let view = fitted;
    if !matches!(view, S_2DMS | S_2DMSA) {
        fit_levels(&mut range.most, &mut range.levels, mips, &mut faults);
    }
    match view {
        S_1DA | S_2DA | S_2DMSA => fit_slices(&mut range.first, &mut range.size, slices, &mut faults),
        S_CUBE => {}
        S_CUBEA => {
            // Whole cubes within the slices.
            if range.first > slices - 6 {
                range.first = slices - 6;
                faults.fit("first cube face past the texture's");
            }
            let cubes = (slices - range.first) / 6;
            let asked = if range.size == u32::MAX { cubes } else { range.size / 6 };
            if asked == 0 || asked > cubes {
                faults.fit("cubes past the texture's");
            }
            range.size = 6 * asked.clamp(1, cubes);
        }
        _ => {}
    }
    fit_format(&mut d.Format, range.plane, &mut faults);
    if matches!(view, S_2D | S_2DA) {
        fit_plane(&mut range.plane, r.Format.0 as u32, d.Format.0 as u32, &mut faults);
    }
    if !(range.clamp >= 0.0 && range.clamp.is_finite()) {
        range.clamp = 0.0;
        faults.fit("minimum LOD clamp");
    }
    put_srv_range(d, range);
    fit_mapping(&mut d.Shader4ComponentMapping, &mut faults);
    faults.verdict()
}

// Unordered access view dimensions.
const U_BUFFER: i32 = 1;
const U_1D: i32 = 2;
const U_1DA: i32 = 3;
const U_2D: i32 = 4;
const U_2DA: i32 = 5;
const U_2DMS: i32 = 6;
const U_2DMSA: i32 = 7;
const U_3D: i32 = 8;

/// Checks (and fits) an unordered access view `d` of the texture `r`.
pub(crate) fn unordered_view(r: &D3D12_RESOURCE_DESC, d: &mut D3D12_UNORDERED_ACCESS_VIEW_DESC) -> Verdict {
    let dim = r.Dimension.0;
    if !matches!(dim, TEX1D | TEX2D | TEX3D) {
        return Verdict::Fine;
    }
    let view = d.ViewDimension.0;
    if view == U_BUFFER {
        return Verdict::Refused("buffer view of a texture");
    }
    if matches!(view, U_2DMS | U_2DMSA) {
        // Multisampled unordered access needs a capability this host
        // may lack: not fitted.
        return Verdict::Refused("multisampled unordered view");
    }
    if !(U_1D..=U_3D).contains(&view) {
        return Verdict::Refused("unknown view dimension");
    }
    let mut faults = Faults::default();
    let mips = levels_of(r);
    let slices = slices_of(r);
    // (mip, first, size, plane) whatever the dimension.
    // SAFETY (each arm): the member the dimension names.
    let (mut mip, mut first, mut size, mut plane) = unsafe {
        match view {
            U_1D => (d.Anonymous.Texture1D.MipSlice, 0, 1, 0),
            U_1DA => {
                let t = d.Anonymous.Texture1DArray;
                (t.MipSlice, t.FirstArraySlice, t.ArraySize, 0)
            }
            U_2D => {
                let t = d.Anonymous.Texture2D;
                (t.MipSlice, 0, 1, t.PlaneSlice)
            }
            U_2DA => {
                let t = d.Anonymous.Texture2DArray;
                (t.MipSlice, t.FirstArraySlice, t.ArraySize, t.PlaneSlice)
            }
            _ => {
                let t = d.Anonymous.Texture3D;
                (t.MipSlice, t.FirstWSlice, t.WSize, 0)
            }
        }
    };
    let array_view = matches!(view, U_1DA | U_2DA);
    let fitted = match dim {
        TEX1D if array_view || view == U_2DA => U_1DA,
        TEX1D => U_1D,
        TEX3D => U_3D,
        _ if view == U_1D => U_2D,
        _ if view == U_1DA || view == U_3D => U_2DA,
        _ => view,
    };
    if fitted != view {
        faults.fit("view dimension the texture's does not allow");
        // Into or out of a 3D view: all of its slices.
        if view == U_3D || fitted == U_3D {
            (first, size) = (0, u32::MAX);
        }
        d.ViewDimension.0 = fitted;
    }
    if mip >= mips {
        mip = mips - 1;
        faults.fit("mip past the texture's");
    }
    match fitted {
        U_1DA | U_2DA => fit_slices(&mut first, &mut size, slices, &mut faults),
        U_3D => {
            let depth = (u32::from(r.DepthOrArraySize) >> mip).max(1);
            if first >= depth {
                first = depth - 1;
                faults.fit("first depth slice past the level's");
            }
            if size != u32::MAX && (size == 0 || size > depth - first) {
                size = depth - first;
                faults.fit("depth slices past the level's");
            }
        }
        _ => {}
    }
    fit_format(&mut d.Format, plane, &mut faults);
    if matches!(fitted, U_2D | U_2DA) {
        fit_plane(&mut plane, r.Format.0 as u32, d.Format.0 as u32, &mut faults);
    }
    d.Anonymous = match fitted {
        U_1D => D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture1D: D3D12_TEX1D_UAV { MipSlice: mip },
        },
        U_1DA => D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture1DArray: D3D12_TEX1D_ARRAY_UAV {
                MipSlice: mip,
                FirstArraySlice: first,
                ArraySize: size,
            },
        },
        U_2D => D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_UAV {
                MipSlice: mip,
                PlaneSlice: plane,
            },
        },
        U_2DA => D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture2DArray: D3D12_TEX2D_ARRAY_UAV {
                MipSlice: mip,
                FirstArraySlice: first,
                ArraySize: size,
                PlaneSlice: plane,
            },
        },
        _ => D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture3D: D3D12_TEX3D_UAV {
                MipSlice: mip,
                FirstWSlice: first,
                WSize: size,
            },
        },
    };
    // sRGB formats take no unordered access.
    if let Some(&(_, linear)) = crate::resource::SRGB.iter().find(|&&(s, _)| s == d.Format.0 as u32) {
        d.Format.0 = linear as i32;
        faults.fit("sRGB unordered view");
    }
    faults.verdict()
}

/// Checks (and fits) a sampler: a filter of known bits, address modes 1 to
/// 5, anisotropy 1 to 16, a comparison 1 to 8 for comparison filters, LOD
/// bias within the host's range, no NaN, and `MinLOD <= MaxLOD`. A
/// sampler can always be fitted.
pub(crate) fn sampler(d: &mut D3D12_SAMPLER_DESC) -> Verdict {
    let mut faults = Faults::default();
    let f = d.Filter.0 as u32;
    let (mip, mag, min) = (f & 1, (f >> 2) & 1, (f >> 4) & 1);
    let anisotropic = f & 0x40 != 0;
    let reduction = (f >> 7) & 3;
    let base = if anisotropic { 0x55 } else { min << 4 | mag << 2 | mip };
    let filter = reduction << 7 | base;
    if filter != f {
        d.Filter.0 = filter as i32;
        faults.fit("sampler filter");
    }
    for mode in [&mut d.AddressU, &mut d.AddressV, &mut d.AddressW] {
        if !(1..=5).contains(&mode.0) {
            mode.0 = 3;
            faults.fit("sampler address mode");
        }
    }
    if !(1..=16).contains(&d.MaxAnisotropy) {
        d.MaxAnisotropy = d.MaxAnisotropy.clamp(1, 16);
        faults.fit("sampler anisotropy");
    }
    let comparison = reduction == 1;
    let func = d.ComparisonFunc.0;
    if !(0..=8).contains(&func) || (comparison && func == 0) {
        d.ComparisonFunc.0 = if comparison { 4 } else { 1 };
        faults.fit("sampler comparison");
    }
    // The host's LOD bias range: [-16, 15.99].
    let bias = d.MipLODBias;
    if bias.is_nan() || !(-16.0..=15.99).contains(&bias) {
        d.MipLODBias = if bias.is_nan() { 0.0 } else { bias.clamp(-16.0, 15.99) };
        faults.fit("sampler LOD bias");
    }
    if d.MinLOD.is_nan() {
        d.MinLOD = 0.0;
        faults.fit("sampler LOD NaN");
    }
    if d.MaxLOD.is_nan() {
        d.MaxLOD = f32::MAX;
        faults.fit("sampler LOD NaN");
    }
    if d.MinLOD > d.MaxLOD {
        d.MaxLOD = d.MinLOD;
        faults.fit("sampler minimum LOD above the maximum");
    }
    for c in &mut d.BorderColor {
        if c.is_nan() {
            *c = 0.0;
            faults.fit("sampler border colour NaN");
        }
    }
    faults.verdict()
}

/// Counts `verdict` and logs its kind the first time it is seen, with the
/// resource and the view as the title gave it (`before`) and as it went to
/// the host (`after`). `true`: make the view (it is valid or fitted).
pub(crate) fn note<T>(what: &'static str, verdict: Verdict, r: Option<&D3D12_RESOURCE_DESC>, before: &T, after: &T) -> bool {
    static SEEN: Mutex<BTreeSet<(&'static str, &'static str)>> = Mutex::new(BTreeSet::new());
    let (kind, made) = match verdict {
        Verdict::Fine => return true,
        Verdict::Fitted(kind) => {
            FITTED.fetch_add(1, Ordering::Relaxed);
            (kind, true)
        }
        Verdict::Refused(kind) => {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            (kind, false)
        }
    };
    if SEEN.lock().unwrap_or_else(|e| e.into_inner()).insert((what, kind)) {
        let resource = r.map(|r| {
            format!(
                "dimension {}, {}x{}x{}, {} levels, format {}, {} samples, flags {:#x}",
                r.Dimension.0,
                r.Width,
                r.Height,
                r.DepthOrArraySize,
                r.MipLevels,
                r.Format.0,
                r.SampleDesc.Count,
                r.Flags.0
            )
        });
        let words = |d: &T| -> Vec<u32> {
            let n = size_of::<T>() / 4;
            // SAFETY: a plain description of `n` words.
            unsafe { std::slice::from_raw_parts(std::ptr::from_ref(d).cast::<u32>(), n) }.to_vec()
        };
        tracing::warn!(
            what,
            kind,
            fitted = made,
            resource = resource.as_deref().unwrap_or("-"),
            title = ?words(before),
            host = ?words(after),
            "view check (VICTORY_VIEW_CHECK): a {what} the host would refuse, {}",
            if made { "fitted" } else { "left null" }
        );
    }
    made
}

/// The counts for the 5-second line; `None` when the check is off.
pub(crate) fn counts() -> Option<(u64, u64)> {
    on().then(|| (FITTED.load(Ordering::Relaxed), REFUSED.load(Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

    /// The 10 words of a 40-byte view description.
    fn words<T>(desc: &T) -> [u32; 10] {
        assert_eq!(size_of::<T>(), 40);
        // SAFETY: a 40-byte plain description.
        unsafe { std::ptr::from_ref(desc).cast::<[u32; 10]>().read_unaligned() }
    }

    fn texture(dim: i32, w: u64, h: u32, depth: u16, mips: u16, format: i32) -> D3D12_RESOURCE_DESC {
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION(dim),
            Width: w,
            Height: h,
            DepthOrArraySize: depth,
            MipLevels: mips,
            Format: DXGI_FORMAT(format),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        }
    }

    fn srv(dim: i32, format: i32, w: [u32; 6]) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
        let mut words = [0u32; 10];
        words[0] = format as u32;
        words[1] = dim as u32;
        words[2] = 5768;
        words[4..10].copy_from_slice(&w);
        // SAFETY: 40 bytes in the host's layout.
        unsafe { std::ptr::from_ref(&words).cast::<D3D12_SHADER_RESOURCE_VIEW_DESC>().read_unaligned() }
    }

    #[test]
    fn the_boat_view_is_fine() {
        // The view in flight at the f1 removal: BC7 32x32, 53 slices, 4 levels.
        let r = texture(TEX2D, 32, 32, 53, 4, 98);
        let mut d = srv(S_2DA, 98, [0, 4, 0, 53, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fine);
        assert_eq!(words(&d), words(&srv(S_2DA, 98, [0, 4, 0, 53, 0, 0])));
    }

    #[test]
    fn slices_and_levels_are_fitted() {
        let r = texture(TEX2D, 32, 32, 53, 4, 98);
        let mut d = srv(S_2DA, 98, [2, 6, 50, 10, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("levels past the texture's"));
        let t = unsafe { d.Anonymous.Texture2DArray };
        assert_eq!((t.MostDetailedMip, t.MipLevels, t.FirstArraySlice, t.ArraySize), (2, 2, 50, 3));
        // A 2D view of an array stays 2D (slice 0): such views are valid.
        let mut d = srv(S_2D, 98, [1, u32::MAX, 0, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fine);
        let mut d = srv(S_2D, 98, [9, u32::MAX, 0, 0, 0, 0]);
        let r1 = texture(TEX2D, 32, 32, 1, 4, 98);
        assert_eq!(shader_view(&r1, &mut d), Verdict::Fitted("first level past the texture's"));
        assert_eq!(unsafe { d.Anonymous.Texture2D }.MostDetailedMip, 3);
    }

    #[test]
    fn cubes_need_six_slices() {
        let r = texture(TEX2D, 64, 64, 4, 1, 28);
        let mut d = srv(S_CUBE, 28, [0, 1, 0, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("view dimension the texture's does not allow"));
        assert_eq!(d.ViewDimension.0, S_2DA);
        let t = unsafe { d.Anonymous.Texture2DArray };
        assert_eq!((t.FirstArraySlice, t.ArraySize), (0, 4));
        let r = texture(TEX2D, 64, 64, 14, 1, 28);
        let mut d = srv(S_CUBEA, 28, [0, 1, 4, 3, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("cubes past the texture's"));
        let t = unsafe { d.Anonymous.TextureCubeArray };
        assert_eq!((t.First2DArrayFace, t.NumCubes), (4, 1));
        let mut d = srv(S_CUBE, 28, [0, 1, 0, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fine);
    }

    #[test]
    fn dimensions_follow_the_resource() {
        let r = texture(TEX3D, 16, 16, 8, 3, 28);
        let mut d = srv(S_2DA, 28, [0, 1, 0, 8, 0, 0]);
        assert!(matches!(shader_view(&r, &mut d), Verdict::Fitted(_)));
        assert_eq!(d.ViewDimension.0, S_3D);
        let r = texture(TEX2D, 16, 16, 8, 3, 28);
        let mut d = srv(S_3D, 28, [0, 1, 0, 0, 0, 0]);
        assert!(matches!(shader_view(&r, &mut d), Verdict::Fitted(_)));
        assert_eq!(d.ViewDimension.0, S_2DA);
        assert_eq!(unsafe { d.Anonymous.Texture2DArray }.ArraySize, 8);
        let mut d = srv(S_BUFFER, 28, [0; 6]);
        assert!(matches!(shader_view(&r, &mut d), Verdict::Refused(_)));
        let mut d = srv(11, 28, [0; 6]);
        assert!(matches!(shader_view(&r, &mut d), Verdict::Refused(_)));
    }

    #[test]
    fn planes_formats_and_mapping() {
        // R24G8 typeless: two planes; a view in D24S8 reads as R24_UNORM_X8.
        let r = texture(TEX2D, 16, 16, 1, 1, 44);
        let mut d = srv(S_2D, 45, [0, 1, 0, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("typeless or depth view format"));
        assert_eq!(d.Format.0, 46);
        let mut d = srv(S_2D, 47, [0, 1, 1, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fine);
        let mut d = srv(S_2D, 47, [0, 1, 0, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("plane its format does not name"));
        assert_eq!(unsafe { d.Anonymous.Texture2D }.PlaneSlice, 1);
        let r = texture(TEX2D, 16, 16, 1, 1, 28);
        let mut d = srv(S_2D, 28, [0, 1, 1, 0, 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("plane the format does not have"));
        let mut d = srv(S_2D, 27, [0, 1, 0, 0, 0, 0]);
        shader_view(&r, &mut d);
        assert_eq!(d.Format.0, 28);
        let mut d = srv(S_2D, 28, [0, 1, 0, 0, 0, 0]);
        d.Shader4ComponentMapping = 7;
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("component mapping"));
        assert_eq!(d.Shader4ComponentMapping, 1 << 12);
        let mut d = srv(S_2D, 28, [0, 1, 0, f32::NAN.to_bits(), 0, 0]);
        assert_eq!(shader_view(&r, &mut d), Verdict::Fitted("minimum LOD clamp"));
    }

    #[test]
    fn whole_chain_when_levels_are_zero() {
        assert_eq!(levels_of(&texture(TEX2D, 32, 32, 1, 0, 28)), 6);
        assert_eq!(levels_of(&texture(TEX3D, 4, 4, 64, 0, 28)), 7);
    }

    #[test]
    fn unordered_views() {
        let r = texture(TEX3D, 16, 16, 8, 3, 28);
        let mut d = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT(28),
            ViewDimension: D3D12_UAV_DIMENSION(U_3D),
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture3D: D3D12_TEX3D_UAV {
                    MipSlice: 2,
                    FirstWSlice: 1,
                    WSize: 4,
                },
            },
        };
        assert_eq!(unordered_view(&r, &mut d), Verdict::Fitted("depth slices past the level's"));
        let t = unsafe { d.Anonymous.Texture3D };
        assert_eq!((t.FirstWSlice, t.WSize), (1, 1));
        d.Anonymous.Texture3D = D3D12_TEX3D_UAV {
            MipSlice: 0,
            FirstWSlice: 0,
            WSize: u32::MAX,
        };
        assert_eq!(unordered_view(&r, &mut d), Verdict::Fine);
        let r = texture(TEX2D, 16, 16, 4, 3, 29);
        let mut d = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT(29),
            ViewDimension: D3D12_UAV_DIMENSION(U_2DA),
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2DArray: D3D12_TEX2D_ARRAY_UAV {
                    MipSlice: 5,
                    FirstArraySlice: 2,
                    ArraySize: 4,
                    PlaneSlice: 0,
                },
            },
        };
        assert_eq!(unordered_view(&r, &mut d), Verdict::Fitted("mip past the texture's"));
        let t = unsafe { d.Anonymous.Texture2DArray };
        assert_eq!((t.MipSlice, t.FirstArraySlice, t.ArraySize), (2, 2, 2));
        assert_eq!(d.Format.0, 28);
        d.ViewDimension.0 = U_2DMS;
        assert!(matches!(unordered_view(&r, &mut d), Verdict::Refused(_)));
    }

    #[test]
    fn samplers() {
        let good = D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER(0x95),
            AddressU: D3D12_TEXTURE_ADDRESS_MODE(1),
            AddressV: D3D12_TEXTURE_ADDRESS_MODE(3),
            AddressW: D3D12_TEXTURE_ADDRESS_MODE(5),
            MipLODBias: -1.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D12_COMPARISON_FUNC(4),
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        let mut d = good;
        assert_eq!(sampler(&mut d), Verdict::Fine);
        let mut d = D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER(0x1234),
            AddressU: D3D12_TEXTURE_ADDRESS_MODE(0),
            MaxAnisotropy: 64,
            ComparisonFunc: D3D12_COMPARISON_FUNC(0),
            MipLODBias: 100.0,
            MinLOD: 5.0,
            MaxLOD: 2.0,
            BorderColor: [f32::NAN, 0.0, 0.0, 1.0],
            ..good
        };
        assert_eq!(sampler(&mut d), Verdict::Fitted("sampler filter"));
        // 0x1234: reduction 0, linear minification and magnification,
        // point mip; the bits above 8 go.
        assert_eq!(d.Filter.0, 0x14);
        assert_eq!(d.AddressU.0, 3);
        assert_eq!(d.MaxAnisotropy, 16);
        // No comparison (0) is valid without a comparison filter.
        assert_eq!(d.ComparisonFunc.0, 0);
        assert_eq!(d.MipLODBias, 15.99);
        assert_eq!(d.MaxLOD, 5.0);
        assert_eq!(d.BorderColor[0], 0.0);
        // A comparison filter with no comparison.
        let mut d = D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER(0x80),
            ComparisonFunc: D3D12_COMPARISON_FUNC(0),
            ..good
        };
        assert_eq!(sampler(&mut d), Verdict::Fitted("sampler comparison"));
        assert_eq!(d.ComparisonFunc.0, 4);
        // Any filter fits to a valid one.
        for f in 0..0x400 {
            let mut d = D3D12_SAMPLER_DESC { Filter: D3D12_FILTER(f), ..good };
            sampler(&mut d);
            let g = d.Filter.0 as u32;
            assert!(matches!(g & 0x7f, 0 | 1 | 4 | 5 | 0x10 | 0x11 | 0x14 | 0x15 | 0x55) && g < 0x200, "{f:#x}");
        }
    }
}
