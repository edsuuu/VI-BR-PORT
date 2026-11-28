//! Texture mods: a title texture replaced by a file, matched by the digest of
//! the title memory it is read from; and the dump that finds them, of the
//! textures the title's interface (its Scaleform programs, `SF_Batch*`)
//! samples.
//!
//! `VICTORY_UI_TEXTURES_DUMP=<folder>`: each texture an interface draw samples
//! is written there once, as `<tiled bytes>-<digest>.bin` (the staging buffer
//! after detiling: the host texture's subresources laid out as D3D12 places
//! them) and `<tiled bytes>-<digest>.txt` (its description and each
//! subresource's offset, row pitch and rows). `scripts/texture-mod.mjs` turns
//! a `.bin` into a picture and an edited picture back.
//!
//! `VICTORY_TEXTURE_MODS=<folder>`: every `<tiled bytes>-<digest>.bin` there
//! replaces, byte for byte, the staging buffer of the texture whose title
//! memory has that length and digest, each time it is detiled. A file of
//! another length than the staging buffer is left alone (the texture's
//! host copy is laid out otherwise: another build, another level held).
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The dump's folder, when it is on.
fn dump_folder() -> Option<&'static Path> {
    static FOLDER: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOLDER
        .get_or_init(|| std::env::var_os("VICTORY_UI_TEXTURES_DUMP").filter(|v| !v.is_empty()).map(PathBuf::from))
        .as_deref()
}

/// The mods, by the length of the title memory they replace: digest and file.
fn mods() -> &'static HashMap<usize, Vec<(u128, PathBuf)>> {
    static MODS: OnceLock<HashMap<usize, Vec<(u128, PathBuf)>>> = OnceLock::new();
    MODS.get_or_init(|| {
        let mut mods: HashMap<usize, Vec<(u128, PathBuf)>> = HashMap::new();
        let Some(folder) = std::env::var_os("VICTORY_TEXTURE_MODS").filter(|v| !v.is_empty()) else {
            return mods;
        };
        let folder = PathBuf::from(folder);
        let Ok(entries) = std::fs::read_dir(&folder) else {
            tracing::warn!(folder = %folder.display(), "the texture mods' folder cannot be read");
            return mods;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some((len, digest)) = parse_name(&path) {
                mods.entry(len).or_default().push((digest, path));
            }
        }
        tracing::info!(
            folder = %folder.display(),
            textures = mods.values().map(Vec::len).sum::<usize>(),
            "texture mods"
        );
        mods
    })
}

/// `<length>-<digest in hex>.bin`.
fn parse_name(path: &Path) -> Option<(usize, u128)> {
    if path.extension()? != "bin" {
        return None;
    }
    let (len, digest) = path.file_stem()?.to_str()?.split_once('-')?;
    Some((len.parse().ok()?, u128::from_str_radix(digest, 16).ok()?))
}

fn name(len: usize, digest: u128) -> String {
    format!("{len}-{digest:032x}")
}

/// Whether detiling has anything to do here.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| dump_folder().is_some() || !mods().is_empty())
}

/// The title memory (start address) of textures an interface draw sampled.
static INTERFACE: Mutex<Option<HashSet<usize>>> = Mutex::new(None);
/// Digests already dumped.
static DUMPED: Mutex<Option<HashSet<u128>>> = Mutex::new(None);

/// A draw of the title's interface samples `resource`. With the dump on, its
/// memory is noted and the texture detiled again, so the dump sees it.
pub(crate) fn note_interface(resource: crate::object::This) {
    if dump_folder().is_none() {
        return;
    }
    let Some((at, _)) = crate::texture::staged_memory(resource) else {
        return;
    };
    let new = INTERFACE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(at as usize);
    if new {
        tracing::info!(memory = format_args!("{at:#x}"), "an interface texture noted for the dump");
        crate::texture::detile_again(resource);
    }
}

/// A texture was detiled: `tiled` is its title memory (from `memory`),
/// `staged` the whole staging buffer it was detiled into. A mod for it
/// replaces the staging buffer; an interface texture is dumped.
pub(crate) fn detiled(memory: usize, tiled: &[u8], staged: &mut [u8], describe: impl FnOnce() -> String) {
    let len = tiled.len();
    let candidates = mods().get(&len);
    let dump = dump_folder().filter(|_| {
        INTERFACE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|set| set.contains(&memory))
    });
    if candidates.is_none() && dump.is_none() {
        return;
    }
    let digest = crate::texture::digest(tiled).value();
    if let Some((_, path)) = candidates.and_then(|c| c.iter().find(|(d, _)| *d == digest)) {
        replace(path, staged);
    }
    if let Some(folder) = dump {
        let first = DUMPED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(digest);
        if first {
            let stem = name(len, digest);
            let bin = folder.join(format!("{stem}.bin"));
            let txt = folder.join(format!("{stem}.txt"));
            let written = std::fs::create_dir_all(folder)
                .and_then(|()| std::fs::write(&bin, &staged[..]))
                .and_then(|()| std::fs::write(&txt, describe()));
            match written {
                Ok(()) => tracing::info!(file = %bin.display(), "an interface texture dumped"),
                Err(e) => tracing::warn!(%e, file = %bin.display(), "an interface texture not dumped"),
            }
        }
    }
}

/// Copies a mod's bytes over the staging buffer, when the lengths agree.
fn replace(path: &Path, staged: &mut [u8]) {
    static READ: Mutex<Option<HashMap<PathBuf, Option<Vec<u8>>>>> = Mutex::new(None);
    let mut read = READ.lock().unwrap_or_else(|e| e.into_inner());
    let bytes = read.get_or_insert_with(HashMap::new).entry(path.to_path_buf()).or_insert_with(|| {
        std::fs::read(path)
            .inspect_err(|e| tracing::warn!(%e, file = %path.display(), "a texture mod cannot be read"))
            .ok()
    });
    match bytes {
        Some(b) if b.len() == staged.len() => {
            staged.copy_from_slice(b);
            if first_time(path) {
                tracing::info!(file = %path.display(), "a texture replaced by its mod");
            }
        }
        Some(b) => {
            if first_time(path) {
                tracing::warn!(file = %path.display(), mod_bytes = b.len(), staged = staged.len(), "a texture mod of another size left out");
            }
        }
        None => {}
    }
}

/// Whether `path` is logged for the first time.
fn first_time(path: &Path) -> bool {
    static LOGGED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
    LOGGED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mod_file_is_named_by_length_and_digest() {
        let digest = 0x0123_4567_89ab_cdef_0011_2233_4455_6677u128;
        let file = PathBuf::from(format!("{}.bin", name(4096, digest)));
        assert_eq!(parse_name(&file), Some((4096, digest)));
        assert_eq!(parse_name(Path::new("4096-xyz.bin")), None);
        assert_eq!(parse_name(Path::new("4096-00.txt")), None);
    }
}
