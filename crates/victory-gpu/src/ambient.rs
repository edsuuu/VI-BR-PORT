//! A stand-in for the indirect light the title traces rays for.
//!
//! With the ray-tracing structure builder off (`crate::pipeline::builder_runs`)
//! the title's rays find nothing. Its program that resolves the diffuse light
//! of its screen probes (`CS_ScreenProbes_TemporalResolve_*`) then leaves the
//! sky's light outdoors, nothing indoors (walls are black where no lamp or
//! sun reaches) and, in blocks of the picture's tiles, what the probes'
//! memory held. Right after that program the light is raised to an estimate
//! and bounded by a multiple of it.
//!
//! That a stand-in can be put there, and in which of the program's images,
//! is beloooo's find (VIctory-Belobug-AMD, 06031237, 4f45ab88, cd9f78ce,
//! GPL-3.0-or-later), which puts a constant there, shaded in creases from the
//! scene's depth. The estimate here is worked out from the frame instead, so
//! it follows the hour, the room and its lamps by itself: the light the
//! visible surfaces send out (the title keeps it for its own screen traces:
//! `CS_CacheScreenSpaceRadiance`, the frame before) is averaged in 64x36
//! cells, and every point gets the mean of the cells weighted by the share of
//! its surroundings they cover, as a directional value (first-order harmonics
//! at 160x90 points, taken between them by depth).
//!
//! What it assumes: what is not on screen sends out what is (with a slow
//! mean of the last seconds, so that turning into a dark corner is not black
//! at once). No occlusion but the creases'.
//!
//! `VICTORY_AMBIENT`:
//! - unset or `0`/`off`: the title's own output.
//! - a number: that share of the estimate (1: all of it).
//! - `estimate:<share>:<least>`: the same, never under the constant `<least>`.
//! - `constant:<level>`: the constant, as beloooo's fork has it.
//! - `constant:<level>:<share>`: the constant, or that share of the light
//!   around (the slow mean of what is on screen) where that is more: by day
//!   outdoors a constant fit for a night indoors leaves what the sun does
//!   not reach black (the boat of the first open-world mission).
//!
//! `VICTORY_AMBIENT_SHADE=<strength>,<radius in metres>` sets the creases.
//!
//! The air of a room is lit by the same probes: without rays they hold the
//! sky's light indoors too, a brown veil at sunset. `VICTORY_AMBIENT_AIR`
//! bounds what the air scatters by a multiple of the light around.
//!
//! `VICTORY_AMBIENT_OUTSIDE=<share>[:<least>]` keeps the constant inside rooms
//! and gives the estimate outside them ([`outside`]).

use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host};

/// Programs of the title the stand-in follows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Role {
    /// Resolves the diffuse light.
    Resolve,
    /// Keeps the light visible surfaces send out.
    Radiance,
    /// Adds the lamps' light to what the air scatters (the last program
    /// that writes that volume before it is summed up along the view).
    Air,
    /// The last program that writes the lamps' shadow-mask table before the
    /// tiled lighting reads it (`CS_DenoiseShadowMaskLDSPacked`): a check
    /// that can light every lamp as if nothing stood in its way
    /// ([`lamp_shadows_off`]).
    LampShadows,
}

/// The program's part in this, by its name in the container.
pub(crate) fn role(container: &[u8]) -> Option<Role> {
    let has = |name: &[u8]| container.windows(name.len()).any(|w| w == name);
    if has(b"CS_ScreenProbes_TemporalResolve") {
        Some(Role::Resolve)
    } else if has(b"CS_CacheScreenSpaceRadiance") {
        Some(Role::Radiance)
    } else if has(b"CS_AccumulateLocalLightInscatteringVolume") {
        Some(Role::Air)
    } else if has(b"CS_DenoiseShadowMaskLDSPacked") {
        Some(Role::LampShadows)
    } else {
        None
    }
}

/// Where the resolve writes the diffuse light (R9G9B9E5 as 32-bit integers)
/// and reads the scene's depth, and where the other writes the light of
/// visible surfaces: root parameter and place in its table, in dwords.
pub(crate) const LIGHT: (u8, u32) = (1, 176);
pub(crate) const DEPTH: (u8, u32) = (0, 464);
/// What sits at [`DEPTH`] in the resolve the prison runs
/// (`CS_ScreenProbes_TemporalResolve_ReflectiveCaustics`, 34fb51b8a0312e82) is
/// the title's view of the stencil plane (it reads its second channel; its
/// depth is the view at 688, which it tests against 0 for the sky), so the
/// passes read no depth from it: every point is taken as 1,000 m away, the
/// creases are never shaded and the estimate weighs every cell alike.
pub(crate) const STENCIL: (u8, u32) = (0, 464);
pub(crate) const TRUE_DEPTH: (u8, u32) = (0, 688);

/// The resolve's view the passes read depth from: [`TRUE_DEPTH`] with
/// `VICTORY_AMBIENT_TRUE_DEPTH=1`, else [`DEPTH`] as before (a fix, off
/// while it is tried: with it the creases' shade starts to act).
pub(crate) fn depth_at() -> (u8, u32) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("VICTORY_AMBIENT_TRUE_DEPTH").is_some_and(|v| v == "1")) {
        TRUE_DEPTH
    } else {
        DEPTH
    }
}
pub(crate) const RADIANCE: (u8, u32) = (1, 16);
/// Where the light the air scatters is.
pub(crate) const AIR: (u8, u32) = (1, 32);
/// Where the lamps' shadow-mask table is (7680x90 R32_UINT): per 8x8 block
/// of the screen and lamp a word, 0 a block in full shadow, up to 255 one
/// value for the block, above that a block of the mask atlas, all ones a
/// block no lamp has. The tiled lighting (`TL_PS_*`, 0x28b0-0x2b64) reads it
/// at table offset 968 and scales a lamp by `1 + (value - 1) * strength`.
pub(crate) const LAMP_TABLE: (u8, u32) = (1, 24);

/// `VICTORY_LAMP_SHADOWS=off`: every block of the lamps' shadow-mask table
/// that a lamp has is set to one word, 255, after the title made the table
/// (`off:<word>` sets that word instead; the title divides it by 255 or, by
/// a flag of its constants, by 15). A check, not a way to run: lamps then
/// light through walls. It tells whether a room that stays dark is dark
/// because its lamps are in shadow (it lights up) or because no lamp of the
/// tiled lighting reaches it (it does not). Unset (the default): the
/// title's table.
pub(crate) fn lamp_shadows_off() -> Option<u32> {
    static SET: OnceLock<Option<u32>> = OnceLock::new();
    *SET.get_or_init(|| {
        let set = std::env::var("VICTORY_LAMP_SHADOWS").unwrap_or_default();
        let rest = set.trim().strip_prefix("off")?;
        match rest.strip_prefix(':') {
            Some(word) => word.trim().parse().ok().filter(|&w: &u32| w <= 255),
            None => rest.is_empty().then_some(255),
        }
    })
}

/// Whether the pass that follows a program of `role` runs.
pub(crate) fn wanted(role: Role) -> bool {
    match role {
        Role::LampShadows => lamp_shadows_off().is_some(),
        Role::Resolve | Role::Radiance | Role::Air => on(),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Set {
    Off,
    /// The share of the estimate, and the least light it gives (0: none).
    Estimate(f32, f32),
    /// The level, and the share of the light around that takes its place
    /// where that is more (0: none).
    Constant(f32, f32),
}

fn parse(set: &str) -> Set {
    let set = set.trim();
    let number = |t: &str| t.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0);
    let pair = |rest: &str| {
        let (a, b) = rest.split_once(':').unwrap_or((rest, ""));
        (number(a), number(b).unwrap_or(0.0))
    };
    if let Some(rest) = set.strip_prefix("constant:") {
        let (level, share) = pair(rest);
        return level.map_or(Set::Off, |level| Set::Constant(level, share));
    }
    // `estimate:<share>:<least>`: the estimate, never under the constant
    // `<least>` (a room the estimate finds dark keeps the night constant).
    if let Some(rest) = set.strip_prefix("estimate:") {
        let (level, least) = pair(rest);
        return level.map_or(Set::Off, |level| Set::Estimate(level, least));
    }
    number(set).map_or(Set::Off, |level| Set::Estimate(level, 0.0))
}

/// `VICTORY_AMBIENT_CELLS=full`: the light around is averaged over every
/// texel of each of the 64x36 cells of the kept radiance (10x10 at 640x360),
/// not 5x5 of them: a fluorescent tube one or two texels thin is then counted
/// in every frame, not only when a sample falls on it (which made the
/// estimate flicker as the camera turned). Unset: 5x5, as before.
fn full_cells() -> bool {
    static SET: OnceLock<bool> = OnceLock::new();
    *SET.get_or_init(|| std::env::var("VICTORY_AMBIENT_CELLS").is_ok_and(|v| v.trim() == "full"))
}

fn set() -> Set {
    static SET: OnceLock<Set> = OnceLock::new();
    *SET.get_or_init(|| parse(&std::env::var("VICTORY_AMBIENT").unwrap_or_default()))
}

/// Whether the stand-in is on: set, and the title's rays find nothing.
pub(crate) fn on() -> bool {
    set() != Set::Off && !crate::pipeline::builder_runs()
}

/// The most the air scatters, as a multiple of the light around
/// (`VICTORY_AMBIENT_AIR=<multiple>`; unset or 0: what the title has).
fn air() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT_AIR").unwrap_or_default();
        set.trim().parse().ok().filter(|v: &f32| v.is_finite() && *v > 0.0).unwrap_or(0.0)
    })
}

/// How bright (luminance of the kept radiance) a texel counts at most in the
/// light around before it is scaled down: lamps and glints count, but not
/// for everything. `VICTORY_AMBIENT_LAMPS=<luminance>`; unset: 20, as before.
/// A room lit by fluorescent tubes (the prison's common room, where the
/// title's traced light from the tubes is what is missing) has little else
/// bright on screen: a higher bound lets the tubes raise the estimate and
/// the slow mean the constant's share follows.
fn lamps() -> f32 {
    static SET: OnceLock<f32> = OnceLock::new();
    *SET.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT_LAMPS").unwrap_or_default();
        set.trim().parse().ok().filter(|v: &f32| v.is_finite() && *v > 0.0).unwrap_or(20.0)
    })
}

/// `VICTORY_AMBIENT_KEY=<mean>:<most gain>`: the constant light (of
/// `constant:`, and the least light of `estimate:`) is raised by the ratio
/// of `<mean>` to the slow mean of what visible surfaces send out, when that
/// is lower, by at most `<most gain>` (default 8). Unset: no raising.
///
/// Why: the title's exposure in the prison's halls is pinned at its upper
/// bound (EV -5.5, the same in the lit corridor, the cell and the common
/// room: `PS_CalculateExposure`, its average luminance 41 in the corridor
/// and 5.7 in the common room, capture prisao-d-alvos2), so a room that gets
/// less light than the console's traced light would give it stays that much
/// darker on screen. The corridor, right with the constant 0.1, has a mean of
/// about 0.19 in the kept radiance; the common room 0.075.
fn key() -> [f32; 2] {
    static SET: OnceLock<[f32; 2]> = OnceLock::new();
    *SET.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT_KEY").unwrap_or_default();
        let mut parts = set.split(':').take(2).map(|v| v.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0));
        match (parts.next().flatten(), parts.next().flatten()) {
            (Some(key), most) => [key, most.unwrap_or(8.0)],
            _ => [0.0, 1.0],
        }
    })
}

/// Words after the key's two numbers: `inside` raises only texels the stencil
/// plane marks inside a room (an exterior night, and what a window shows,
/// keep their light); `air` keeps the bound on what the air scatters where
/// it was without the key (the slow mean it follows includes the added
/// light), so the fog of a hall does not rise with its walls; `geo` divides
/// the key by the slow log-average of the cells' luminance instead of their
/// mean (a few bright windows and tubes pull the mean of a dark hall above a
/// plain corridor's: run prisao-s-amb-t10).
///
/// `room` (implies `inside`): the key is measured on the cells the stencil
/// plane marks inside a room only, and what is not per texel (the air's hold
/// of `air`, the `tint`) acts as much as the screen is inside (the slow share
/// of such cells). A scene with nothing inside then gets nothing of the key:
/// the opening's night street (run intro-s-cand: gain 8 measured on the
/// street, the fog held to an eighth everywhere) keeps its fog, while the
/// prison's rooms (about 98% inside, agente-interior.md 14.3) are raised as
/// before. Without the word: the whole screen's measure, as before.
fn key_flags() -> KeyWords {
    static SET: OnceLock<KeyWords> = OnceLock::new();
    *SET.get_or_init(|| key_words(&std::env::var("VICTORY_AMBIENT_KEY").unwrap_or_default()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct KeyWords {
    inside: bool,
    air: bool,
    geo: bool,
    room: bool,
}

fn key_words(set: &str) -> KeyWords {
    let words: Vec<&str> = set.split(':').skip(2).map(str::trim).collect();
    let room = words.contains(&"room");
    KeyWords {
        inside: room || words.contains(&"inside"),
        air: words.contains(&"air"),
        geo: words.contains(&"geo"),
        room,
    }
}

/// The key's gain for a measured light: the ratio of the key to it, from 1
/// up to `most` (no key: 1). The passes' `gain_of` does the same.
fn key_gain(key: f32, most: f32, measured: f32) -> f32 {
    if key <= 0.0 {
        return 1.0;
    }
    (key / measured.max(1e-4)).clamp(1.0, most.max(1.0))
}

/// What of a gain acts on what is not per texel (`room`): as much as the
/// screen is inside. The passes' `gain_whole` does the same.
fn whole_gain(gain: f32, share: f32) -> f32 {
    1.0 + (gain - 1.0) * share.clamp(0.0, 1.0)
}

/// `VICTORY_AMBIENT_OUTSIDE=<share>[:<least>]`, with `VICTORY_AMBIENT=constant:...`:
/// texels the stencil plane does not mark inside a room take the estimate
/// (that share of it, never under the constant `<least>`, what the title's
/// program left kept up to four times it) instead of the constant, and the
/// bound on what the air scatters (`VICTORY_AMBIENT_AIR`) acts as much as the
/// screen is inside. The constant's share of the light around
/// (`constant:<level>:<share>`) stays a floor outside too. Unset or 0: the
/// constant and the air's bound everywhere, as before.
///
/// Why: the constant is written over what the title's program left, so at
/// night outdoors it takes away the sky's light the probes hold (the
/// blue-green of the opening on the 26 Nov morning, run intro-video) and puts
/// there a flat level five to fifty times what the street's surfaces send out
/// (slow mean of the kept radiance 0.002-0.02 in run intro-s-grosso, against
/// 0.1): an even brown on every face. And the air's bound, made for the
/// prison's halls, holds the fog of a night street to 0.8 of that mean, so the
/// halo of its lamps and of the police lights goes. Inside (the prison's
/// rooms, about 98% of their screen) nothing changes.
fn outside() -> [f32; 2] {
    static SET: OnceLock<[f32; 2]> = OnceLock::new();
    *SET.get_or_init(|| parse_outside(&std::env::var("VICTORY_AMBIENT_OUTSIDE").unwrap_or_default()))
}

fn parse_outside(set: &str) -> [f32; 2] {
    let number = |t: &str| t.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0);
    let (share, least) = set.split_once(':').unwrap_or((set, ""));
    match number(share) {
        Some(share) => [share, number(least).unwrap_or(0.0)],
        None => [0.0, 0.0],
    }
}

/// The word `tint`: the air's colour goes 60% of the way (times how much the
/// key raises the room: none at a gain of 1, all from 2) to that of the light
/// around. Its own switch word, since it changes colours, not levels.
fn key_tint() -> bool {
    static SET: OnceLock<bool> = OnceLock::new();
    *SET.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT_KEY").unwrap_or_default();
        set.split(':').skip(2).any(|w| w.trim() == "tint")
    })
}

fn key_geo() -> bool {
    key_flags().geo
}

fn key_inside() -> bool {
    key_flags().inside
}

fn key_air_hold() -> bool {
    key_flags().air
}

fn key_room() -> bool {
    key_flags().room
}

/// Strength and radius of the creases' shade.
fn shade() -> [f32; 2] {
    static SET: OnceLock<[f32; 2]> = OnceLock::new();
    *SET.get_or_init(|| {
        let mut shade = [1.0, 0.6];
        let set = std::env::var("VICTORY_AMBIENT_SHADE").unwrap_or_default();
        for (to, v) in shade.iter_mut().zip(set.split(',')) {
            if let Ok(v) = v.trim().parse() {
                *to = v;
            }
        }
        shade
    })
}

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=25, b0), DescriptorTable(SRV(t0)), DescriptorTable(UAV(u0)), UAV(u1), UAV(u2), DescriptorTable(SRV(t1))"
#define CX 64
#define CY 36
#define CELLS (CX * CY)
// After the cells' places: the slow log-average of their luminance.
#define GEO (2 * CELLS + 1)
// Then the slow log-average of each of their colours (the hue of the bulk).
#define GEO_RGB (2 * CELLS + 2)
// Then, of the cells inside a room (the stencil plane's bit 3 at their
// centre): the slow log-average of their luminance (x, and its log in y), the
// slow share of the screen they are (z); and their slow mean luminance (x of
// the next).
#define INSIDE (2 * CELLS + 3)
#define INSIDE_MEAN (2 * CELLS + 4)
#define PX 160
#define PY 90
cbuffer Set : register(b0) {
    float level; float constant; float strength; float radius;
    float near; float tan_y; uint width; uint height;
    float follow; float most; float air; float around;
    float lamps; float full; float least; float key;
    float most_gain; float inside_only; float air_hold; float geo_key;
    float air_tint; float room;
    // (VICTORY_AMBIENT_OUTSIDE) The estimate's share and least light outside
    // rooms with the constant; whether the stencil plane is bound.
    float outside_level; float outside_least; float has_stencil;
};
Texture2D<float> depth : register(t0);
// The scene's stencil plane (the title's view of plane 1): bit 3 "inside a
// room" in its second channel.
Texture2D<uint2> stencil : register(t1);
// Cells: the light they send out (then, at CELLS, the slow mean), from
// CELLS + 1 on where they are.
RWStructuredBuffer<float4> cells : register(u1);
// Three a point: red, green and blue, each a mean and a direction.
RWStructuredBuffer<float4> points : register(u2);
static const float3 LUMA = float3(0.2126, 0.7152, 0.0722);

// How much the constant light is raised so that a scene whose surfaces send
// out less than `key` on the mean (the slow mean of the kept radiance) gets
// closer to it: the ratio, from 1 up to `most_gain` (no key: 1). The light
// itself raises the mean, so the gain settles below the ratio it starts at.
float gain_of(float measured) {
    return clamp(key / max(measured, 1e-4), 1.0, max(most_gain, 1.0));
}
float gain_to_key(float4 mean_now) {
    if (key <= 0.0 || mean_now.a <= 0.0) return 1.0;
    // (`room`: measured on the cells inside a room only; none yet: no gain.)
    if (room > 0.0) {
        float4 in_room = cells[INSIDE];
        if (in_room.a <= 0.0) return 1.0;
        return gain_of(geo_key > 0.0 ? in_room.x : cells[INSIDE_MEAN].x);
    }
    // (`geo`: the log-average instead of the mean.)
    float measured = dot(mean_now.rgb, LUMA);
    if (geo_key > 0.0) {
        float4 geo = cells[GEO];
        if (geo.a <= 0.0) return 1.0;
        measured = geo.x;
    }
    return gain_of(measured);
}

// The gain for what is not per texel (the air's hold, its tint): with
// `room`, as much of it as the screen is inside (none on a night street).
float gain_whole(float4 mean_now) {
    float g = gain_to_key(mean_now);
    if (room > 0.0) {
        float4 in_room = cells[INSIDE];
        g = lerp(1.0, g, in_room.a > 0.0 ? saturate(in_room.z) : 0.0);
    }
    return g;
}

// The same at a texel of the screen: with inside_only, 1 where the stencil
// plane does not mark the texel inside a room (an exterior night keeps its
// light; what a window shows of the outside too).
float gain_at(float4 mean_now, int2 p) {
    if (inside_only > 0.0 && (stencil.Load(int3(p, 0)).y & 8u) == 0u) return 1.0;
    return gain_to_key(mean_now);
}


// The point seen at a texel, in metres from the eye (x right, y up, z away).
float3 seen(int2 p, float2 size) {
    float d = depth.Load(int3(clamp(p, int2(0, 0), int2(size) - 1), 0));
    float z = min(near / max(d, 1e-7), 1000.0);
    float2 ndc = (float2(p) + 0.5) / size * 2.0 - 1.0;
    return float3(ndc.x * tan_y * size.x / size.y * z, -ndc.y * tan_y * z, z);
}

#ifdef KEEP
RWTexture2D<float3> radiance : register(u0);
[RootSignature(RS)]
[numthreads(8, 4, 1)]
void keep(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    radiance.GetDimensions(w, h);
    float3 sum = 0;
    // 5x5 samples of the cell, or (full) every texel of it.
    uint2 n = full > 0.0 ? max(uint2(w / CX, h / CY), uint2(1, 1)) : uint2(5, 5);
    for (uint y = 0; y < n.y; y++) for (uint x = 0; x < n.x; x++) {
        uint2 p = uint2((float2(at.xy) + (float2(x, y) + 0.5) / float2(n)) * float2(w, h) / float2(CX, CY));
        float3 c = max(radiance[p], 0.0);
        c = any(isnan(c)) || any(isinf(c)) ? 0.0 : c;
        // Lamps and glints count, but not for everything.
        sum += c / max(1.0, dot(c, LUMA) / lamps);
    }
    cells[at.y * CX + at.x] = float4(sum / float(n.x * n.y), 1.0);
}
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void mean() {
    // One read of each cell for the three means (the mean, and the
    // log-averages of the luminance and of each colour).
    float3 sum = 0;
    float logs = 0;
    float3 logs_rgb = 0;
    for (uint i = 0; i < CELLS; i++) {
        float3 c = cells[i].rgb;
        sum += c;
        logs += log(dot(c, LUMA) + 1e-3);
        logs_rgb += log(c + 1e-3);
    }
    float4 was = cells[CELLS];
    float3 now = sum / CELLS;
    cells[CELLS] = float4(was.a > 0.0 ? lerp(was.rgb, now, follow) : now, 1.0);
    // The log-average (geometric mean) of the cells' luminance: it follows
    // the bulk of the picture, not a few windows and tubes (as the title's
    // own meter, a histogram, does), followed slowly in the log.
    float4 geo_was = cells[GEO];
    float geo_now = logs / CELLS;
    float geo_log = geo_was.a > 0.0 ? lerp(geo_was.y, geo_now, follow) : geo_now;
    cells[GEO] = float4(max(exp(geo_log) - 1e-3, 0.0), geo_log, 0.0, 1.0);
    float4 rgb_was = cells[GEO_RGB];
    float3 rgb_now = logs_rgb / CELLS;
    cells[GEO_RGB] = float4(rgb_was.a > 0.0 ? lerp(rgb_was.rgb, rgb_now, follow) : rgb_now, 1.0);
}
#endif

#ifdef PLACE
[RootSignature(RS)]
[numthreads(8, 4, 1)]
void place(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    float3 p = seen(int2((float2(at.xy) + 0.5) * size / float2(CX, CY)), size);
    cells[CELLS + 1 + at.y * CX + at.x] = float4(p, p.z * p.z);
}
// (`room`) The cells inside a room, by the stencil plane at their centre:
// their slow log-average and mean luminance, and the slow share of the
// screen they are. With none inside the measures keep what they were (the
// next room starts from them) and the share goes to 0.
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void inside_share() {
    float n = 0, logs = 0, sum = 0;
    for (uint i = 0; i < CELLS; i++) {
        int2 p = int2((float2(i % CX, i / CX) + 0.5) * float2(width, height) / float2(CX, CY));
        if ((stencil.Load(int3(p, 0)).y & 8u) == 0u) continue;
        float l = dot(cells[i].rgb, LUMA);
        n += 1;
        logs += log(l + 1e-3);
        sum += l;
    }
    float4 was = cells[INSIDE];
    float4 mean_was = cells[INSIDE_MEAN];
    float share_now = n / float(CELLS);
    float share = was.a > 0.0 ? lerp(was.z, share_now, follow) : share_now;
    float geo_log = was.y;
    float mean_luma = mean_was.x;
    if (n > 0) {
        geo_log = was.a > 0.0 ? lerp(was.y, logs / n, follow) : logs / n;
        mean_luma = was.a > 0.0 ? lerp(mean_was.x, sum / n, follow) : sum / n;
    }
    // (Valid once a room has been seen: before that the key gives nothing.)
    float seen_one = was.a > 0.0 || n > 0 ? 1.0 : 0.0;
    cells[INSIDE] = float4(max(exp(geo_log) - 1e-3, 0.0), geo_log, share, seen_one);
    // (y: the share has been measured, though no room was seen.)
    cells[INSIDE_MEAN] = float4(mean_luma, 1.0, 0.0, seen_one);
}
#endif

#ifdef GATHER
[RootSignature(RS)]
[numthreads(8, 2, 1)]
void gather(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    float3 p = seen(int2((float2(at.xy) + 0.5) * size / float2(PX, PY)), size);
    float r2 = 0.25 + 0.0025 * p.z * p.z;
    float whole = 0;
    float3 toward = 0, light = 0, red = 0, green = 0, blue = 0;
    for (uint i = 0; i < CELLS; i++) {
        float4 c = cells[CELLS + 1 + i];
        float3 L = cells[i].rgb;
        float3 d = c.xyz - p;
        float dd = dot(d, d);
        float w = c.w / (dd + r2);
        float3 dir = d / sqrt(dd + 0.25 * r2);
        whole += w;
        toward += w * dir;
        light += w * L;
        red += w * L.r * dir;
        green += w * L.g * dir;
        blue += w * L.b * dir;
    }
    whole = max(whole, 1e-6);
    light /= whole;
    toward /= whole;
    uint at0 = (at.y * PX + at.x) * 3;
    points[at0] = float4(red / whole - light.r * toward, light.r);
    points[at0 + 1] = float4(green / whole - light.g * toward, light.g);
    points[at0 + 2] = float4(blue / whole - light.b * toward, light.b);
}
#endif

#ifdef AIR
RWTexture3D<float3> scattered : register(u0);
// No more than a multiple of the light around: of the room as the last
// seconds showed it, and of the surface seen in this direction (towards a
// window the air outside may be as bright as the sky is).
[RootSignature(RS)]
[numthreads(8, 8, 2)]
void clear_air(uint3 at : SV_DispatchThreadID) {
    uint w, h, d;
    scattered.GetDimensions(w, h, d);
    if (at.x >= w || at.y >= h || at.z >= d) return;
    uint2 q = min(uint2((float2(at.xy) + 0.5) / float2(w, h) * float2(PX, PY)), uint2(PX - 1, PY - 1));
    uint at0 = (q.y * PX + q.x) * 3;
    float3 seen_there = float3(points[at0].w, points[at0 + 1].w, points[at0 + 2].w);
    float far = float(at.z) / float(d);
    float cap = air * lerp(dot(cells[CELLS].rgb, LUMA), dot(seen_there, LUMA), far);
    // The light the key adds to surfaces does not raise what the air may
    // scatter: the bound stays where it was before (the slow mean includes
    // the added light, so it is taken back out by the gain).
    // (`room`: as much as the screen is inside.)
    if (air_hold > 0.0) cap /= gain_whole(cells[CELLS]);
    float3 c = scattered[at];
    float has = dot(c, LUMA);
    // (VICTORY_AMBIENT_OUTSIDE) As much as the screen is inside: none on a
    // night street, whose lamps light its fog.
    float bound_share = 1.0;
    if (outside_level > 0.0 && cells[INSIDE_MEAN].y > 0.0) bound_share = saturate(cells[INSIDE].z);
    if (has > cap) c *= lerp(1.0, cap / has, bound_share);
    // (`tint`) Where the key raises a dark room, the air's colour goes part
    // of the way to that of the light its surfaces send out: without rays the
    // probes that light a hall's air hold the sky's light (a brown veil at
    // sunset), not its lamps'. As much as the key acts: none at a gain of 1.
    float4 mean_now = cells[CELLS];
    float mean_luma = dot(mean_now.rgb, LUMA);
    if (air_tint > 0.0 && mean_now.a > 0.0 && mean_luma > 1e-5) {
        float t = air_tint * saturate(gain_whole(mean_now) - 1.0);
        // The hue of the bulk of the picture (log-average of each colour),
        // not of its bright windows.
        float4 bulk = cells[GEO_RGB];
        float3 rgb = bulk.a > 0.0 ? max(exp(bulk.rgb) - 1e-3, 1e-5) : mean_now.rgb;
        float3 hue = rgb / max(dot(rgb, LUMA), 1e-5);
        c = lerp(c, dot(c, LUMA) * hue, t);
    }
    scattered[at] = c;
}
#endif

#ifdef UNSHADOW
RWTexture2D<uint> table : register(u0);
// Every block a lamp has, lit by it as if nothing stood in its way (the
// word comes in the first constant's bits).
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void unshadow(uint3 at : SV_DispatchThreadID) {
    if (at.x >= width || at.y >= height) return;
    if (table[at.xy] != 0xffffffffu) table[at.xy] = asuint(level);
}
#endif

#ifdef RAISE
RWTexture2D<uint> light : register(u0);
float3 unpack(uint v) {
    return float3(v & 511, (v >> 9) & 511, (v >> 18) & 511) * exp2(float(v >> 27) - 24.0);
}
uint pack(float3 c) {
    c = clamp(c, 0.0, 65408.0);
    float top = max(c.r, max(c.g, c.b));
    int e = clamp(int(floor(log2(max(top, 1e-9)))) + 16, 0, 31);
    float scale = exp2(float(24 - e));
    if (top * scale >= 511.5) { e = min(e + 1, 31); scale = exp2(float(24 - e)); }
    uint3 m = min(uint3(c * scale + 0.5), 511u);
    return m.r | (m.g << 9) | (m.b << 18) | (uint(e) << 27);
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void raise(uint3 at : SV_DispatchThreadID) {
    float2 size = float2(width, height);
    if (at.x >= width || at.y >= height) return;
    int2 p = int2(at.xy);
    float3 c = seen(p, size);
    float3 l = seen(p + int2(-1, 0), size), r = seen(p + int2(1, 0), size);
    float3 u = seen(p + int2(0, -1), size), d = seen(p + int2(0, 1), size);
    float3 dx = abs(l.z - c.z) < abs(r.z - c.z) ? c - l : r - c;
    float3 dy = abs(u.z - c.z) < abs(d.z - c.z) ? c - u : d - c;
    float3 n = normalize(cross(dy, dx) + float3(0, 0, -1e-9));
    if (dot(n, c) > 0.0) n = -n;
    // How open it is (as in beloooo's cd9f78ce).
    float open = 1.0;
    float px = min(radius * size.y / (2.0 * tan_y * c.z), 128.0);
    float noise = frac(52.9829189 * frac(dot(float2(p), float2(0.06711056, 0.00583715))));
    if (strength > 0.0 && px >= 1.5 && c.z < 400.0) {
        float sum = 0.0;
        const int N = 12;
        for (int i = 0; i < N; i++) {
            float a = i * 2.399963 + noise * 6.2831853;
            float2 o = float2(cos(a), sin(a)) * (sqrt((i + 0.5) / N) * px);
            float3 v = seen(p + int2(round(o)), size) - c;
            float vv = dot(v, v);
            float over = dot(v, n) - 0.004 * c.z - 0.005;
            sum += max(over, 0.0) / sqrt(vv + 1e-6) * saturate(1.0 - vv / (radius * radius));
        }
        open = saturate(1.0 - strength * 2.0 * sum / N);
    }
    float3 found;
    // (VICTORY_AMBIENT_OUTSIDE) Outside rooms the estimate, not the constant.
    bool outdoors = constant > 0.0 && outside_level > 0.0 && has_stencil > 0.0 &&
        (stencil.Load(int3(p, 0)).y & 8u) == 0u;
    float share_of_estimate = outdoors ? outside_level : level;
    float least_light = outdoors ? outside_least : least;
    if (constant > 0.0 && !outdoors) {
        found = float3(1.0, 0.98, 0.94) * constant;
        // A share of the light around, where that is more than the constant.
        float4 mean_now = cells[CELLS];
        float3 part = around * mean_now.rgb;
        if (around > 0.0 && mean_now.a > 0.0 && dot(part, LUMA) > dot(found, LUMA)) found = part;
        found *= open * gain_at(mean_now, p);
        light[p] = pack(found);
        return;
    }
    // Between the four points around, those at its own depth first; which
    // four is dithered a little.
    float2 g = (float2(p) + 0.5) / size * float2(PX, PY) - 0.5 + (noise - 0.5) * 0.5;
    int2 g0 = int2(floor(g));
    float2 f = g - float2(g0);
    float4 sr = 0, sg = 0, sb = 0;
    float sum = 0;
    for (int k = 0; k < 4; k++) {
        int2 q = clamp(g0 + int2(k & 1, k >> 1), int2(0, 0), int2(PX, PY) - 1);
        float z = seen(int2((float2(q) + 0.5) * size / float2(PX, PY)), size).z;
        float2 b = float2(k & 1 ? f.x : 1 - f.x, k >> 1 ? f.y : 1 - f.y);
        float w = (b.x * b.y + 1e-3) / (0.02 + abs(z - c.z) / c.z);
        uint at0 = (q.y * PX + q.x) * 3;
        sr += w * points[at0];
        sg += w * points[at0 + 1];
        sb += w * points[at0 + 2];
        sum += w;
    }
    float3 all = float3(sr.w, sg.w, sb.w) / sum;
    found = all + 1.2 * float3(dot(sr.xyz, n), dot(sg.xyz, n), dot(sb.xyz, n)) / sum;
    found = clamp(found, 0.2 * all, 2.5 * all);
    found = lerp(found, cells[CELLS].rgb, 0.2) * share_of_estimate * open;
    // Never under the least light asked for (estimate:<share>:<least>).
    float3 floor_light = float3(1.0, 0.98, 0.94) * least_light * open * gain_at(cells[CELLS], p);
    // (VICTORY_AMBIENT_OUTSIDE) Outside rooms the constant's share of the
    // light around (`constant:<level>:<share>`) is a floor too: by day what
    // the sun does not reach on the boat keeps what the constant gave it.
    if (outdoors && around > 0.0 && cells[CELLS].a > 0.0) {
        float3 part = around * cells[CELLS].rgb * open;
        if (dot(part, LUMA) > dot(floor_light, LUMA)) floor_light = part;
    }
    if (dot(found, LUMA) < dot(floor_light, LUMA)) found = floor_light;
    float3 was = unpack(light[p]);
    // What the program left counts as far as it can be true: the sky's light
    // outdoors, not the blocks of other memory.
    float top = max(dot(found, LUMA) * most, 1e-6);
    float has = dot(was, LUMA);
    was *= min(1.0, top / max(has, 1e-6));
    light[p] = pack(max(was, found));
}
#endif
"#;

/// Bytes of the cells' buffer and of the points'.
const CELLS: u64 = 64 * 36;
const CELL_BYTES: u64 = (2 * CELLS + 5) * 16;
/// The passes' root constants, in words (the `Set` buffer).
const WORDS: usize = 25;
const POINT_BYTES: u64 = 160 * 90 * 48;

struct Pass {
    root: ID3D12RootSignature,
    keep: ID3D12PipelineState,
    mean: ID3D12PipelineState,
    place: ID3D12PipelineState,
    gather: ID3D12PipelineState,
    raise: ID3D12PipelineState,
    air: ID3D12PipelineState,
    unshadow: ID3D12PipelineState,
    inside: ID3D12PipelineState,
    cells: ID3D12Resource,
    points: ID3D12Resource,
    /// Where the slow mean is copied for the log line ([`report_mean`]).
    readback: ID3D12Resource,
}
// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

fn make(device: &ID3D12Device, buffer: impl Fn(u64) -> Option<ID3D12Resource>) -> windows::core::Result<Pass> {
    let mut root: Option<ID3D12RootSignature> = None;
    let mut pipeline = |part: &str, entry| -> windows::core::Result<ID3D12PipelineState> {
        let code = crate::host::compile(&format!("#define {part}\n{SOURCE}"), entry, s!("cs_5_1"))?;
        // SAFETY: the root signature is inside the shader; plain creations.
        unsafe {
            if root.is_none() {
                root = Some(device.CreateRootSignature(0, &code)?);
            }
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            };
            device.CreateComputePipelineState(&desc)
        }
    };
    let keep = pipeline("KEEP", s!("keep"))?;
    let mean = pipeline("KEEP", s!("mean"))?;
    let place = pipeline("PLACE", s!("place"))?;
    let gather = pipeline("GATHER", s!("gather"))?;
    let raise = pipeline("RAISE", s!("raise"))?;
    let air = pipeline("AIR", s!("clear_air"))?;
    let unshadow = pipeline("UNSHADOW", s!("unshadow"))?;
    let inside = pipeline("PLACE", s!("inside_share"))?;
    let made = |bytes| buffer(bytes).ok_or_else(windows::core::Error::empty);
    let readback = {
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: 256,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut readback: Option<ID3D12Resource> = None;
        // SAFETY: plain creation of a small read-back buffer.
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )?;
        }
        readback.ok_or_else(windows::core::Error::empty)?
    };
    Ok(Pass {
        readback,
        root: root.ok_or_else(windows::core::Error::empty)?,
        keep,
        mean,
        place,
        gather,
        raise,
        air,
        unshadow,
        inside,
        cells: made(CELL_BYTES)?,
        points: made(POINT_BYTES)?,
    })
}

fn pass() -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| {
        let host = host()?;
        make(&host.device, |bytes| host.uav_buffer(bytes))
            .inspect_err(|e| tracing::warn!(error = %e, "no stand-in for the indirect light"))
            .ok()
    })
    .as_ref()
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER { Type: D3D12_RESOURCE_BARRIER_TYPE_UAV, ..Default::default() };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Whether the light of a frame has been kept: the estimate has something
/// to begin with.
static KEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the passes read and write, as views in the heap the list has set.
pub(crate) struct Views {
    /// The image the title's program just wrote.
    pub(crate) image: D3D12_GPU_DESCRIPTOR_HANDLE,
    /// The scene's depth, for [`Role::Resolve`].
    pub(crate) depth: D3D12_GPU_DESCRIPTOR_HANDLE,
    /// The scene's stencil plane, for [`Role::Resolve`] when the program
    /// has one ([`STENCIL`]).
    pub(crate) stencil: Option<D3D12_GPU_DESCRIPTOR_HANDLE>,
    /// The size of the image, and how many slices it has.
    pub(crate) size: [u32; 2],
    pub(crate) slices: u32,
}

/// After the title's program of that role, in `list` with the program's view
/// heap set. Replaces the list's compute root signature and pipeline; false
/// if it recorded nothing.
pub(crate) fn after(list: &HostList, role: Role, views: &Views) -> bool {
    use std::sync::atomic::Ordering;
    let Some(pass) = pass() else {
        return false;
    };
    if role == Role::LampShadows {
        return unshadow(list, pass, views);
    }
    let (level, constant, around) = match set() {
        Set::Off => return false,
        Set::Estimate(level, least) => (level, 0.0f32, least),
        Set::Constant(level, around) => (0.0, level, around),
    };
    if role != Role::Radiance && constant == 0.0 && !KEPT.load(Ordering::Relaxed) {
        return false;
    }
    // The air's bound works with the constant too: the light around (the
    // cells) is kept in both forms, and the points seen are gathered for it
    // below. Without the bound the air held the sky's light indoors: orange
    // clouds that burst in the prison's cells and a brown veil in its halls.
    if role == Role::Air && air() == 0.0 {
        return false;
    }
    static SAID: Mutex<u32> = Mutex::new(0);
    {
        let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
        if *said < 4 {
            *said += 1;
            let [outside_share, outside_least] = outside();
            tracing::info!(
                ?role,
                size = ?views.size,
                level,
                constant,
                outside_share,
                outside_least,
                stencil = views.stencil.is_some(),
                "a stand-in for the indirect light"
            );
        }
    }
    list.keep(&pass.cells);
    list.keep(&pass.points);
    let [strength, radius] = shade();
    let floats = [level, constant, strength, radius, 0.1, 0.45];
    let mut words = [0u32; WORDS];
    for (to, v) in words.iter_mut().zip(floats) {
        *to = v.to_bits();
    }
    words[6] = views.size[0];
    words[7] = views.size[1];
    words[8] = 0.04f32.to_bits();
    words[9] = 4.0f32.to_bits();
    words[10] = air().to_bits();
    words[11] = around.to_bits();
    words[12] = lamps().to_bits();
    words[13] = if full_cells() { 1.0f32 } else { 0.0 }.to_bits();
    // The estimate's least light rides in the share's place (the constant
    // form's, which the estimate does not use).
    words[14] = if constant == 0.0 { around } else { 0.0 }.to_bits();
    let [key, most_gain] = key();
    words[15] = key.to_bits();
    words[16] = most_gain.to_bits();
    words[17] = if key_inside() && views.stencil.is_some() { 1.0f32 } else { 0.0 }.to_bits();
    words[18] = if key_air_hold() { 1.0f32 } else { 0.0 }.to_bits();
    words[19] = if key_geo() { 1.0f32 } else { 0.0 }.to_bits();
    words[20] = if key_tint() { 0.6f32 } else { 0.0 }.to_bits();
    // (`room` is read by every pass, the air's too, which has no stencil
    // plane: it reads what the resolve measured.)
    words[21] = if key > 0.0 && key_room() { 1.0f32 } else { 0.0 }.to_bits();
    // The estimate outside rooms with the constant (VICTORY_AMBIENT_OUTSIDE):
    // it needs the stencil plane, the points gathered and the rooms' share.
    let [outside_level, outside_least] = if constant > 0.0 { outside() } else { [0.0, 0.0] };
    let outdoors = outside_level > 0.0 && views.stencil.is_some();
    words[22] = outside_level.to_bits();
    words[23] = outside_least.to_bits();
    words[24] = if views.stencil.is_some() { 1.0f32 } else { 0.0 }.to_bits();
    let measure_rooms = ((key > 0.0 && key_room()) || outdoors) && views.stencil.is_some();
    let l = &list.list;
    // SAFETY: an open list with the heap of the views set; the title's
    // program has just used both views, so their textures are in the states
    // these views need; buffers the list keeps.
    unsafe {
        uav_barrier(l);
        l.SetComputeRootSignature(&pass.root);
        l.SetComputeRoot32BitConstants(0, WORDS as u32, words.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(1, views.depth);
        l.SetComputeRootDescriptorTable(2, views.image);
        l.SetComputeRootUnorderedAccessView(3, pass.cells.GetGPUVirtualAddress());
        l.SetComputeRootUnorderedAccessView(4, pass.points.GetGPUVirtualAddress());
        l.SetComputeRootDescriptorTable(5, views.stencil.unwrap_or(views.depth));
        let run = |pso: &ID3D12PipelineState, x, y, z| {
            l.SetPipelineState(pso);
            l.Dispatch(x, y, z);
            uav_barrier(l);
        };
        match role {
            Role::Radiance => {
                run(&pass.keep, 8, 9, 1);
                run(&pass.mean, 1, 1, 1);
                KEPT.store(true, Ordering::Relaxed);
                report_mean(list, pass);
            }
            Role::Air => run(&pass.air, views.size[0].div_ceil(8), views.size[1].div_ceil(8), views.slices.div_ceil(2)),
            // (Recorded by `unshadow`, above.)
            Role::LampShadows => {}
            Role::Resolve => {
                // (With the constant, only for the air's bound and the
                // estimate outside rooms.)
                if constant == 0.0 || air() > 0.0 || outdoors {
                    run(&pass.place, 8, 9, 1);
                    run(&pass.gather, 20, 45, 1);
                }
                if measure_rooms {
                    run(&pass.inside, 1, 1, 1);
                }
                run(&pass.raise, views.size[0].div_ceil(8), views.size[1].div_ceil(8), 1);
            }
        }
    }
    true
}

/// Every 5 s: a log line with the slow mean of the light around (as the last
/// copy left it, 5 s old) and the gain the key gives for it, then a new copy
/// of the mean (recorded in `list`, after the pass that made it).
fn report_mean(list: &HostList, pass: &Pass) {
    use std::sync::atomic::Ordering::Relaxed;
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    static COPIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed().as_secs_f32() < 5.0) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    if COPIED.load(Relaxed) {
        let mut data = std::ptr::null_mut();
        // SAFETY: the copy was recorded 5 s ago and has run; 16 bytes mapped.
        unsafe {
            if pass.readback.Map(0, None, Some(&mut data)).is_ok() {
                let m = std::slice::from_raw_parts(data.cast::<f32>(), 16).to_vec();
                pass.readback.Unmap(0, None);
                let luma = 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
                let [key, most] = key();
                let geo = m[4];
                // (`room`: the cells inside a room, and their share.)
                let (room_geo, room_share, room_mean, room_seen) = (m[8], m[10], m[12], m[11] > 0.0);
                let measured = match (key_room(), key_geo()) {
                    (true, true) => room_geo,
                    (true, false) => room_mean,
                    (false, true) => geo,
                    (false, false) => luma,
                };
                let valid = if key_room() { room_seen } else { m[3] > 0.0 };
                let gain = if valid { key_gain(key, most, measured) } else { 1.0 };
                let air_gain = if key_room() { whole_gain(gain, if room_seen { room_share } else { 0.0 }) } else { gain };
                tracing::info!(
                    mean = ?[m[0], m[1], m[2]],
                    luma,
                    geo,
                    key,
                    gain,
                    air_gain,
                    room_share,
                    room_geo,
                    room_mean,
                    inside_only = key_inside(),
                    air_hold = key_air_hold(),
                    by_geo = key_geo(),
                    by_room = key_room(),
                    "the indirect light's stand-in: slow mean of the light around and the key's gain"
                );
            }
        }
    }
    list.keep(&pass.readback);
    let l = &list.list;
    // SAFETY: an open list; the mean pass just wrote the cells (promoted to
    // unordered access in this list), the read-back buffer stays a copy
    // destination.
    unsafe {
        l.ResourceBarrier(&[crate::host::transition(
            &pass.cells,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        )]);
        l.CopyBufferRegion(&pass.readback, 0, &pass.cells, CELLS * 16, 16);
        l.CopyBufferRegion(&pass.readback, 16, &pass.cells, (2 * CELLS + 1) * 16, 16);
        // (The rooms' measures and share, and their mean.)
        l.CopyBufferRegion(&pass.readback, 32, &pass.cells, (2 * CELLS + 3) * 16, 32);
        l.ResourceBarrier(&[crate::host::transition(
            &pass.cells,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )]);
    }
    COPIED.store(true, Relaxed);
}

/// [`Role::LampShadows`]: every block of the lamps' shadow-mask table that
/// a lamp has set to [`lamp_shadows_off`]'s word.
fn unshadow(list: &HostList, pass: &Pass, views: &Views) -> bool {
    let Some(word) = lamp_shadows_off() else {
        return false;
    };
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::info!(size = ?views.size, word, "the lamps' shadow-mask table is lit throughout (VICTORY_LAMP_SHADOWS)");
    }
    list.keep(&pass.cells);
    list.keep(&pass.points);
    let mut words = [0u32; WORDS];
    words[0] = word;
    words[6] = views.size[0];
    words[7] = views.size[1];
    let l = &list.list;
    // SAFETY: an open list with the heap of the view set; the title's
    // program has just written the table through it, so it is in the state
    // the view needs. The pass reads no depth: the table's own view stands
    // in for that table, as it does for the radiance's.
    unsafe {
        uav_barrier(l);
        l.SetComputeRootSignature(&pass.root);
        l.SetComputeRoot32BitConstants(0, WORDS as u32, words.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(1, views.depth);
        l.SetComputeRootDescriptorTable(2, views.image);
        l.SetComputeRootUnorderedAccessView(3, pass.cells.GetGPUVirtualAddress());
        l.SetComputeRootUnorderedAccessView(4, pass.points.GetGPUVirtualAddress());
        l.SetComputeRootDescriptorTable(5, views.stencil.unwrap_or(views.depth));
        l.SetPipelineState(&pass.unshadow);
        l.Dispatch(views.size[0].div_ceil(8), views.size[1].div_ceil(8), 1);
        uav_barrier(l);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings() {
        assert_eq!(parse(""), Set::Off);
        assert_eq!(parse("off"), Set::Off);
        assert_eq!(parse("0"), Set::Off);
        assert_eq!(parse("0.8"), Set::Estimate(0.8, 0.0));
        assert_eq!(parse("estimate:0.8:0.1"), Set::Estimate(0.8, 0.1));
        assert_eq!(parse("estimate:x:0.1"), Set::Off);
        assert_eq!(parse("constant:0.3"), Set::Constant(0.3, 0.0));
        assert_eq!(parse("constant:0.1:0.5"), Set::Constant(0.1, 0.5));
        assert_eq!(parse("constant:0.1:x"), Set::Constant(0.1, 0.0));
        assert_eq!(parse("constant:nan"), Set::Off);
    }

    #[test]
    fn key_words_and_gains() {
        assert_eq!(key_words("0.2:8"), KeyWords::default());
        let prison = key_words("0.2:8:inside:air:geo");
        assert!(prison.inside && prison.air && prison.geo && !prison.room);
        // `room` implies `inside`.
        let room = key_words("0.2:8:room:air:geo");
        assert!(room.room && room.inside && room.air && room.geo);
        assert_eq!(key_gain(0.0, 8.0, 0.01), 1.0);
        assert_eq!(key_gain(0.2, 8.0, 0.4), 1.0);
        assert_eq!(key_gain(0.2, 8.0, 0.1), 2.0);
        assert_eq!(key_gain(0.2, 8.0, 0.0002), 8.0);
        // Nothing inside (the opening's street): none of the key on the air.
        assert_eq!(whole_gain(8.0, 0.0), 1.0);
        // A room filling the screen: all of it, as without `room`.
        assert_eq!(whole_gain(8.0, 1.0), 8.0);
        assert_eq!(whole_gain(3.0, 0.5), 2.0);
    }

    #[test]
    fn outside_settings() {
        assert_eq!(parse_outside(""), [0.0, 0.0]);
        assert_eq!(parse_outside("0"), [0.0, 0.0]);
        assert_eq!(parse_outside("1"), [1.0, 0.0]);
        assert_eq!(parse_outside("1:0.03"), [1.0, 0.03]);
        assert_eq!(parse_outside("x:0.03"), [0.0, 0.0]);
    }

    #[test]
    fn roles_by_name() {
        assert_eq!(role(b"..CS_ScreenProbes_TemporalResolve_a911b49c_Wrapped.."), Some(Role::Resolve));
        assert_eq!(role(b"..CS_CacheScreenSpaceRadiance_eed2fc35.."), Some(Role::Radiance));
        assert_eq!(role(b"CS_AccumulateLocalLightInscatteringVolume_7e2f5eef"), Some(Role::Air));
        assert_eq!(role(b"..CS_DenoiseShadowMaskLDSPacked_113e2add_Wrapped.."), Some(Role::LampShadows));
        assert_eq!(role(b"CS_TemporalResolve"), None);
    }

    /// The passes compile with the system's compiler and the host takes them.
    #[test]
    fn the_passes_build() {
        let Some(host) = host() else {
            return;
        };
        make(&host.device, |bytes| host.uav_buffer(bytes)).expect("the passes");
    }
}
