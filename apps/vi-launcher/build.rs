//! The executable's icon: the VI-BR-PORT mark (src/mark.rs) computed at build
//! time and linked in as a Windows resource. The repository holds no image or
//! resource file (docs/asset-policy.md): both are written to OUT_DIR here.
//! The resource is a .res file, which the MSVC linker takes as an input, so no
//! resource compiler and no new crate are needed.

#[path = "src/mark.rs"]
mod mark;

use std::path::PathBuf;

/// The icon's sizes, in pixels.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/mark.rs");
    println!("cargo:rerun-if-changed=build.rs");
    // update.rs reads VI_CHANNEL (a dev build names itself so).
    println!("cargo:rerun-if-env-changed=VI_CHANNEL");
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|v| v == "windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc");
    if !windows_msvc {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let res = out.join("vi-launcher-icon.res");
    std::fs::write(&res, resources()).expect("writing the icon resource");
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// A .res file: an empty first entry, one RT_ICON per size, one RT_GROUP_ICON.
fn resources() -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let mut out = Vec::new();
    entry(&mut out, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // icons
    group.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
    for (i, &n) in SIZES.iter().enumerate() {
        let id = i as u16 + 1;
        let image = dib(n, &mark::rgba(n));
        entry(&mut out, RT_ICON, id, 0x1010, &image);
        let side = if n >= 256 { 0 } else { n as u8 };
        group.extend_from_slice(&[side, side, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes()); // planes
        group.extend_from_slice(&32u16.to_le_bytes()); // bits
        group.extend_from_slice(&(image.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    out
}

/// One resource entry: its header (type and name as ordinals, US English),
/// the data, padded to four bytes.
fn entry(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    let ordinal = |out: &mut Vec<u8>, v: u16| {
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    };
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    ordinal(out, kind);
    ordinal(out, name);
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&(if kind == 0 { 0u16 } else { 0x0409 }).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// An icon image as a device-independent bitmap: the header (twice the height:
/// colour then mask), 32-bit BGRA rows bottom up, and an all-clear AND mask
/// (the alpha channel says what shows).
fn dib(n: u32, rgba: &[u8]) -> Vec<u8> {
    let mask_row = n.div_ceil(32) * 4;
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(n as i32).to_le_bytes());
    out.extend_from_slice(&(2 * n as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // no compression
    out.extend_from_slice(&(n * n * 4 + mask_row * n).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    for y in (0..n).rev() {
        for x in 0..n {
            let p = ((y * n + x) * 4) as usize;
            out.extend_from_slice(&[rgba[p + 2], rgba[p + 1], rgba[p], rgba[p + 3]]);
        }
    }
    out.extend(std::iter::repeat_n(0u8, (mask_row * n) as usize));
    out
}
