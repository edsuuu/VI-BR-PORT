//! Samplers as the host describes them, from the console's sampler
//! description (`CreateSamplerX`), which names the hardware's sampler
//! fields. (Immediate samplers in shader code come decoded with the
//! translation.)

use windows::Win32::Graphics::Direct3D12::*;

/// The fields both forms share.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fields {
    /// Point 0, bilinear 1, anisotropic point 2, anisotropic bilinear 3.
    mag: u32,
    min: u32,
    /// None 0, point 1, linear 2.
    mip: u32,
    /// Plain 0, minimum 1, maximum 2.
    mode: u32,
    /// Host address modes.
    address: [u32; 3],
    anisotropy: u32,
    /// Host comparison function.
    compare: u32,
    min_lod: f32,
    max_lod: f32,
    bias: f32,
    border: [f32; 4],
}

impl Fields {
    fn host(&self) -> D3D12_SAMPLER_DESC {
        let linear = |f: u32| u32::from(f == 1 || f == 3);
        let aniso = self.mag >= 2 || self.min >= 2;
        // The console compares when the instruction says so, with the
        // sampler's function; the host needs a comparison sampler for that.
        // A sampler given a real test (less ... greater-or-equal) is one;
        // never, always and none are plain samplers.
        const LESS: u32 = 2;
        const GREATER_EQUAL: u32 = 7;
        let compares = (LESS..=GREATER_EQUAL).contains(&self.compare);
        let reduction = match self.mode {
            1 => 2,
            2 => 3,
            _ => u32::from(compares),
        };
        let filter = if aniso {
            0x55
        } else {
            linear(self.min) << 4 | linear(self.mag) << 2 | u32::from(self.mip == 2)
        };
        let min_lod = finite_lod(self.min_lod, 0.0);
        // Without a mip filter the console clamps sampling to `min_lod`.
        let max_lod = if self.mip == 0 {
            min_lod
        } else {
            finite_lod(self.max_lod, min_lod).max(min_lod)
        };
        D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER((reduction << 7 | filter) as i32),
            AddressU: address_mode(self.address[0]),
            AddressV: address_mode(self.address[1]),
            AddressW: address_mode(self.address[2]),
            MipLODBias: if self.bias.is_nan() {
                0.0
            } else {
                self.bias
                    .clamp(D3D12_MIP_LOD_BIAS_MIN, D3D12_MIP_LOD_BIAS_MAX)
            },
            MaxAnisotropy: if aniso {
                self.anisotropy.clamp(1, 16)
            } else {
                1
            },
            ComparisonFunc: if reduction == 1 {
                D3D12_COMPARISON_FUNC(self.compare as i32)
            } else {
                D3D12_COMPARISON_FUNC_NEVER
            },
            BorderColor: self.border,
            MinLOD: min_lod,
            MaxLOD: max_lod,
        }
    }
}

/// Maps a console address mode (`1..=5`) to host D3D12, falling back to `CLAMP`.
fn address_mode(mode: u32) -> D3D12_TEXTURE_ADDRESS_MODE {
    match mode {
        1..=5 => D3D12_TEXTURE_ADDRESS_MODE(mode as i32),
        _ => D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
    }
}

/// Sanitizes an LOD bound for D3D12, substituting `fallback` for `NaN` and clamping infinities.
fn finite_lod(value: f32, fallback: f32) -> f32 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(-D3D12_FLOAT32_MAX, D3D12_FLOAT32_MAX)
    }
}

/// The console's sampler description (27 words): flags, magnification,
/// minification, mip and depth filters, filter mode, three address modes,
/// minimum and maximum LOD, LOD bias, secondary bias, anisotropy, its bias
/// and threshold, comparison, border color, then tuning words.
pub(crate) fn console(words: &[u32; 27]) -> D3D12_SAMPLER_DESC {
    let f = |i: usize| f32::from_bits(words[i]);
    Fields {
        mag: words[1],
        min: words[2],
        mip: words[3],
        mode: words[5],
        address: [words[6], words[7], words[8]],
        min_lod: f(9),
        max_lod: f(10),
        bias: f(11),
        anisotropy: words[13],
        compare: words[16],
        border: [f(17), f(18), f(19), f(20)],
    }
    .host()
}


/// The console's sampler flag bits (`D3D12XBOX_SAMPLER_FLAG_*`, word 0).
const FLAG_UNNORMALIZED: u32 = 0x1;
/// Flags the host's sampler cannot express at all: truncated coordinates
/// (0x2, 0x4), degamma override (0x8), no pre-clamp bias (0x10), no cube
/// wrap (0x20), the min-mip filter (0x100) and the anisotropy override
/// (0x200).
const FLAGS_NO_HOST_EQUIVALENT: u32 = 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x100 | 0x200;

/// Whether sampler flags are honoured where the host can: `VICTORY_SAMPLER_FLAGS=1`.
/// Default off until measured; with it on, every flag combination a title
/// creates is also logged once, so a run says which ones matter.
pub(crate) fn flags_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_SAMPLER_FLAGS").is_some_and(|v| v == "1"))
}

/// The flag word of a console sampler description.
pub(crate) fn console_flags(words: &[u32; 27]) -> u32 {
    words[0]
}

/// Logs a flag combination the first time it is seen.
pub(crate) fn note_flags(flags: u32) {
    static SEEN: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&flags) {
        seen.push(flags);
        tracing::info!(
            flags = format_args!("{flags:#x}"),
            unnormalized = flags & FLAG_UNNORMALIZED != 0,
            no_host_equivalent = format_args!("{:#x}", flags & FLAGS_NO_HOST_EQUIVALENT),
            "sampler created with console flags"
        );
    }
}

/// The description in the form the flags need (`D3D12_SAMPLER_DESC2`), when
/// a flag has a host equivalent: only unnormalized coordinates do. The host
/// accepts them with point or linear filtering, no mips, no anisotropy and
/// clamp or border addressing, so the description is cut down to that.
pub(crate) fn console2(words: &[u32; 27]) -> Option<D3D12_SAMPLER_DESC2> {
    if console_flags(words) & FLAG_UNNORMALIZED == 0 {
        return None;
    }
    let d = console(words);
    let edge = |m: D3D12_TEXTURE_ADDRESS_MODE| {
        if m == D3D12_TEXTURE_ADDRESS_MODE_BORDER {
            m
        } else {
            D3D12_TEXTURE_ADDRESS_MODE_CLAMP
        }
    };
    // Reduction bits stay; the mip bit goes; anisotropy becomes linear.
    let reduction = d.Filter.0 & 0x180;
    let xy = if d.Filter.0 & 0x40 != 0 { 0x14 } else { d.Filter.0 & 0x14 };
    Some(D3D12_SAMPLER_DESC2 {
        Filter: D3D12_FILTER(reduction | xy),
        AddressU: edge(d.AddressU),
        AddressV: edge(d.AddressV),
        AddressW: edge(d.AddressW),
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: d.ComparisonFunc,
        Anonymous: D3D12_SAMPLER_DESC2_0 {
            FloatBorderColor: d.BorderColor,
        },
        MinLOD: 0.0,
        MaxLOD: 0.0,
        Flags: D3D12_SAMPLER_FLAG_NON_NORMALIZED_COORDINATES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_descriptions_decode() {
        let mut w = [0u32; 27];
        w[1] = 1; // bilinear magnification
        w[2] = 0; // point minification
        w[3] = 2; // linear mips
        w[6..9].copy_from_slice(&[3, 3, 3]);
        w[10] = 1000.0f32.to_bits();
        let d = console(&w);
        assert_eq!(d.Filter.0, 0x5, "min point, mag and mip linear");
        assert_eq!(d.AddressW, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(d.MaxLOD, 1000.0);
        // A less-or-equal test makes a comparison sampler; never does not.
        w[16] = 4;
        assert_eq!(console(&w).Filter.0, 0x85);
        assert_eq!(console(&w).ComparisonFunc, D3D12_COMPARISON_FUNC_LESS_EQUAL);
        w[16] = 1;
        assert_eq!(console(&w).Filter.0, 0x5);
    }

    #[test]
    fn mip_none_clamps_to_min_lod_and_inverted_lod_range_clamps() {
        let mut w = [0u32; 27];
        w[3] = 0; // mip none
        w[9] = 3.0f32.to_bits();
        w[10] = 8.0f32.to_bits();
        let d = console(&w);
        assert_eq!((d.MinLOD, d.MaxLOD), (3.0, 3.0));

        w[9] = f32::NAN.to_bits();
        w[10] = 8.0f32.to_bits();
        let d = console(&w);
        assert_eq!((d.MinLOD, d.MaxLOD), (0.0, 0.0));

        w[9] = f32::INFINITY.to_bits();
        w[10] = 8.0f32.to_bits();
        let d = console(&w);
        assert_eq!((d.MinLOD, d.MaxLOD), (D3D12_FLOAT32_MAX, D3D12_FLOAT32_MAX));

        w[3] = 2; // linear mips
        w[9] = 4.0f32.to_bits();
        w[10] = 1.0f32.to_bits();
        let d = console(&w);
        assert_eq!((d.MinLOD, d.MaxLOD), (4.0, 4.0));

        w[9] = f32::NAN.to_bits();
        w[10] = f32::INFINITY.to_bits();
        let d = console(&w);
        assert_eq!((d.MinLOD, d.MaxLOD), (0.0, D3D12_FLOAT32_MAX));

        w[9] = 2.5f32.to_bits();
        w[10] = f32::NAN.to_bits();
        let d = console(&w);
        assert_eq!((d.MinLOD, d.MaxLOD), (2.5, 2.5));

        w[9] = f32::NEG_INFINITY.to_bits();
        w[10] = f32::NEG_INFINITY.to_bits();
        let d = console(&w);
        assert_eq!(
            (d.MinLOD, d.MaxLOD),
            (-D3D12_FLOAT32_MAX, -D3D12_FLOAT32_MAX)
        );
    }

    #[test]
    fn bias_address_and_comparison_stay_in_host_bounds() {
        let mut w = [0u32; 27];
        w[6..9].copy_from_slice(&[0, 6, 3]);
        w[11] = 24.0f32.to_bits();
        w[16] = 99;
        let d = console(&w);
        assert_eq!(d.MipLODBias, D3D12_MIP_LOD_BIAS_MAX);
        assert_eq!(
            [d.AddressU, d.AddressV, d.AddressW],
            [D3D12_TEXTURE_ADDRESS_MODE_CLAMP; 3]
        );
        assert_eq!(d.ComparisonFunc, D3D12_COMPARISON_FUNC_NEVER);

        w[11] = (-24.0f32).to_bits();
        w[5] = 1; // minimum reduction
        w[16] = 4; // less-or-equal, ignored when reduction != 1
        let d = console(&w);
        assert_eq!(d.MipLODBias, D3D12_MIP_LOD_BIAS_MIN);
        assert_eq!(d.ComparisonFunc, D3D12_COMPARISON_FUNC_NEVER);

        w[11] = f32::NAN.to_bits();
        w[5] = 0; // plain mode, reduction == 1 from compare == 4
        w[6..9].copy_from_slice(&[1, 4, 5]);
        w[16] = 4;
        let d = console(&w);
        assert_eq!(d.MipLODBias, 0.0);
        assert_eq!(
            [d.AddressU, d.AddressV, d.AddressW],
            [
                D3D12_TEXTURE_ADDRESS_MODE_WRAP,
                D3D12_TEXTURE_ADDRESS_MODE_BORDER,
                D3D12_TEXTURE_ADDRESS_MODE_MIRROR_ONCE,
            ]
        );
        assert_eq!(d.ComparisonFunc, D3D12_COMPARISON_FUNC_LESS_EQUAL);
    }
}

#[cfg(test)]
mod flag_tests {
    use super::*;

    #[test]
    fn unnormalized_flag_cuts_the_description_down_to_what_the_host_accepts() {
        let mut w = [0u32; 27];
        assert!(console2(&w).is_none());
        w[0] = 0x8; // degamma only: nothing the host can do
        assert!(console2(&w).is_none());
        w[0] = 0x1;
        w[1] = 3; // anisotropic bilinear
        w[2] = 3;
        w[3] = 2; // linear mips
        w[6..9].copy_from_slice(&[1, 2, 3]); // wrap, mirror, clamp
        w[10] = 12.0f32.to_bits();
        w[13] = 8;
        let d = console2(&w).expect("flag 0x1 has a host form");
        assert_eq!(d.Filter.0, 0x14, "linear minification and magnification, no mips");
        assert_eq!((d.MinLOD, d.MaxLOD, d.MaxAnisotropy), (0.0, 0.0, 1));
        assert_eq!(d.AddressU, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(d.AddressV, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(d.Flags, D3D12_SAMPLER_FLAG_NON_NORMALIZED_COORDINATES);
    }
}
