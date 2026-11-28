//! The VI-BR-PORT mark, drawn from code: a retro sun striped across its lower
//! half over the night sky of the launcher's theme, in a rounded square. The
//! repository holds no image files (docs/asset-policy.md), so the executable's
//! icon (build.rs), the window's and the tray's are all computed here.
//! The colours are the theme's (scripts/launcher/tokens.json: night, violet,
//! sunTop, sunMid, sunLow, accent). No dependencies: build.rs includes this file.

/// The mark at `n` x `n` pixels, RGBA, rows top to bottom, each pixel the mean
/// of 4 x 4 samples (smooth edges at 16 pixels as at 256).
pub fn rgba(n: u32) -> Vec<u8> {
    const S: u32 = 4;
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let mut sum = [0.0f32; 4];
            for sy in 0..S {
                for sx in 0..S {
                    let u = (x as f32 + (sx as f32 + 0.5) / S as f32) / n as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / S as f32) / n as f32;
                    let [r, g, b, a] = sample(u, v);
                    // Premultiplied while summing, so the edges do not darken.
                    sum[0] += r * a;
                    sum[1] += g * a;
                    sum[2] += b * a;
                    sum[3] += a;
                }
            }
            let count = (S * S) as f32;
            let a = sum[3] / count;
            let un = |c: f32| if a > 0.0 { (c / count / a).clamp(0.0, 1.0) } else { 0.0 };
            out.extend_from_slice(&[
                (un(sum[0]) * 255.0).round() as u8,
                (un(sum[1]) * 255.0).round() as u8,
                (un(sum[2]) * 255.0).round() as u8,
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// The colour at (u, v), both 0..1 from the top left; alpha 0 outside the square.
fn sample(u: f32, v: f32) -> [f32; 4] {
    // The rounded square, corners of a fifth of the side.
    let r = 0.2;
    let (cx, cy) = (u.clamp(r, 1.0 - r), v.clamp(r, 1.0 - r));
    if (u - cx).hypot(v - cy) > r {
        return [0.0; 4];
    }
    // The sky: night at the top, violet low, a magenta glow at the horizon.
    let mut c = mix(rgb(0x120824), rgb(0x3A1368), v / 0.78);
    let horizon = 0.78;
    let glow = (1.0 - (v - horizon).abs() / 0.16).max(0.0);
    c = mix(c, rgb(0x8E1F86), glow * 0.55);
    // The sun: a disc resting on the horizon, gold at its top, coral low.
    let (sx, sy, sr) = (0.5, 0.52, 0.30);
    let d = (u - sx).hypot(v - sy);
    if d <= sr && v <= horizon {
        let t = (v - (sy - sr)) / (2.0 * sr);
        let sun = if t < 0.5 { mix(rgb(0xFFE08A), rgb(0xFFB04A), t * 2.0) } else { mix(rgb(0xFFB04A), rgb(0xFF6A5A), (t - 0.5) * 2.0) };
        // Bands cut across its lower half, thicker toward the horizon.
        let cut = (v > sy + 0.02) && {
            let k = ((v - (sy + 0.02)) / 0.055).floor();
            let within = (v - (sy + 0.02)) - k * 0.055;
            within < 0.012 + k * 0.006
        };
        if !cut {
            c = sun;
        }
    }
    // The sea below the horizon, with the sun's reflection as accent lines.
    if v > horizon {
        c = mix(rgb(0x24123F), rgb(0x120824), (v - horizon) / 0.22);
        let line = ((v - horizon) / 0.045).fract() < 0.35;
        let width = 0.22 - (v - horizon) * 0.6;
        if line && (u - sx).abs() < width {
            c = mix(c, rgb(0xFF3D8B), 0.85);
        }
    }
    [c[0], c[1], c[2], 1.0]
}
