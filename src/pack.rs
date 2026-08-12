//! `rux php archive` snapshots a registered PHP install into a `<name>.pack`
//! file: the exact same zstd-compressed tar bytes `rux build` would produce
//! for that PHP directory, saved once so future builds can read them
//! straight off disk instead of re-walking, re-hashing, and re-compressing
//! the install every time.
//!
//! This is a deliberate tradeoff from the automatic fingerprint cache
//! (`payload::cached_or_build_archive`): a `.pack` is trusted as-is once it
//! exists — it's not re-validated against the live PHP directory on every
//! build. If you update a PHP install, re-run `rux php archive` to refresh
//! its `.pack`. In exchange, a build using a `.pack` skips filesystem work
//! for PHP entirely: no directory walk, no per-file `stat`, no re-read, no
//! re-compression — just one file read plus a checksum check.
//!
//! File layout: `[magic: 8 bytes "RUXPACK1"][sha256(archive): 32 bytes]
//! [archive_len: u64 LE][archive bytes (zstd-compressed tar, same format
//! `payload.rs` uses internally for the PHP side of a payload)]`.

use crate::error::{LauncherError, Result};
use crate::payload;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const PACK_MAGIC: &[u8; 8] = b"RUXPACK1";
const PACK_HEADER_LEN: usize = 8 + 32 + 8;

/// Builds a fresh archive of `php_dir` (walk + parallel read + multithreaded
/// zstd compress, same as a normal build would do) and writes it to
/// `<packs_dir>/<name>.pack`, replacing any existing pack for that name.
/// Returns the path written.
pub fn archive(name: &str, php_dir: &Path, packs_dir: &Path) -> Result<PathBuf> {
    if !php_dir.is_dir() {
        return Err(LauncherError::Archive(format!(
            "{} is not a directory",
            php_dir.display()
        )));
    }

    let entries = payload::walk_sorted(php_dir);
    let archive_bytes = payload::tar_zstd_entries(&entries, &[])?;
    let checksum = payload::sha256(&archive_bytes);

    std::fs::create_dir_all(packs_dir)?;
    let pack_path = packs_dir.join(format!("{name}.pack"));

    // Atomic write: build the full file in a temp path first, then rename
    // into place, so a crash or power loss mid-write can never leave a
    // half-written `.pack` that a later build would read as corrupt (or
    // worse, silently truncated in a way that *looks* complete).
    let tmp_path = packs_dir.join(format!(".{name}.pack.tmp"));
    {
        let mut f = File::create(&tmp_path)?;
        f.write_all(PACK_MAGIC)?;
        f.write_all(&checksum)?;
        f.write_all(&(archive_bytes.len() as u64).to_le_bytes())?;
        f.write_all(&archive_bytes)?;
        f.flush()?;
    }
    std::fs::rename(&tmp_path, &pack_path)?;

    Ok(pack_path)
}

/// Reads and verifies `<packs_dir>/<name>.pack`, returning the compressed
/// archive bytes ready to hand to `payload::pack` as
/// `PhpArchiveSource::Prebuilt`. Returns `Ok(None)` if no pack exists for
/// that name (not an error — callers fall back to the normal directory
/// path). Returns `Err` if a pack exists but is corrupt or tampered with:
/// callers should surface that clearly rather than silently falling back,
/// since a corrupt `.pack` silently ignored could mask real data loss.
pub fn read(name: &str, packs_dir: &Path) -> Result<Option<Vec<u8>>> {
    let pack_path = packs_dir.join(format!("{name}.pack"));
    let mut file = match File::open(&pack_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(LauncherError::Io(e)),
    };

    let mut header = [0u8; PACK_HEADER_LEN];
    file.read_exact(&mut header).map_err(|e| {
        LauncherError::Archive(format!(
            "'{}' is too short to be a valid .pack file: {e}",
            pack_path.display()
        ))
    })?;

    if &header[0..8] != PACK_MAGIC {
        return Err(LauncherError::Archive(format!(
            "'{}' doesn't look like a Ruxius .pack file (bad magic)",
            pack_path.display()
        )));
    }
    let mut expected_checksum = [0u8; 32];
    expected_checksum.copy_from_slice(&header[8..40]);
    let len_bytes: [u8; 8] = header[40..48].try_into().unwrap();
    let archive_len = u64::from_le_bytes(len_bytes) as usize;

    let mut archive_bytes = vec![0u8; archive_len];
    file.read_exact(&mut archive_bytes).map_err(|e| {
        LauncherError::Archive(format!(
            "'{}' is truncated or corrupt: {e}",
            pack_path.display()
        ))
    })?;

    let actual_checksum = payload::sha256(&archive_bytes);
    if actual_checksum != expected_checksum {
        return Err(LauncherError::ChecksumMismatch {
            expected: hex::encode(expected_checksum),
            actual: hex::encode(actual_checksum),
        });
    }

    Ok(Some(archive_bytes))
}

/// Lists every `.pack` file's name (without the extension) present in
/// `packs_dir`, sorted. Used by `rux php list`/`rux doctor` to show which
/// registered versions have an archived pack available.
pub fn list_names(packs_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(packs_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("pack") {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}
