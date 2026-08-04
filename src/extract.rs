use crate::error::{LauncherError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const MANIFEST_FILE_NAME: &str = "manifest.json";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    checksum: String,
}

/// Ensures the given payload (a zstd-compressed tar containing `php/` and
/// `app/`) is extracted and up to date at `extract_root`. Skips extraction
/// entirely if a prior extraction already matches `checksum_hex`. Returns
/// the directory that contains the extracted `php/` and `app/` folders.
pub fn ensure_extracted(
    extract_root: &Path,
    compressed_payload: &[u8],
    checksum_hex: &str,
) -> Result<PathBuf> {
    let manifest_path = extract_root.join(MANIFEST_FILE_NAME);

    if extract_root.exists() {
        if let Some(existing) = read_manifest(&manifest_path) {
            if existing.checksum == checksum_hex && sanity_check(extract_root) {
                log::info!("Bundled app already up to date, skipping extraction.");
                return Ok(extract_root.to_path_buf());
            }
            log::info!("Bundled app is outdated or incomplete; re-extracting.");
        } else {
            log::info!("No previous extraction manifest found; extracting fresh.");
        }
    }

    extract_fresh(extract_root, compressed_payload)?;

    let manifest = Manifest {
        checksum: checksum_hex.to_string(),
    };
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| LauncherError::Extraction(format!("failed to serialize manifest: {e}")))?;
    fs::write(&manifest_path, json)
        .map_err(|e| LauncherError::Extraction(format!("failed to write manifest: {e}")))?;

    Ok(extract_root.to_path_buf())
}

fn read_manifest(path: &Path) -> Option<Manifest> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Wipes any partial/stale extraction and unpacks the payload atomically
/// via a temp-directory-then-rename strategy, so a crash mid-extraction
/// never leaves a half-written app behind.
fn extract_fresh(extract_root: &Path, compressed_payload: &[u8]) -> Result<()> {
    let parent = extract_root
        .parent()
        .ok_or_else(|| LauncherError::Extraction("extract root has no parent directory".into()))?;
    fs::create_dir_all(parent)?;

    let tmp_dir = tempfile::Builder::new()
        .prefix(".ruxius-extract-")
        .tempdir_in(parent)
        .map_err(|e| LauncherError::Extraction(format!("failed to create temp dir: {e}")))?;

    decompress_and_unpack(compressed_payload, tmp_dir.path())?;

    if !sanity_check(tmp_dir.path()) {
        return Err(LauncherError::Extraction(
            "extracted payload failed sanity check (missing php/php.exe or app/)".into(),
        ));
    }

    if extract_root.exists() {
        // Best-effort removal of the previous extraction. If a stray file is
        // locked by a lingering process we still proceed; the fresh files
        // will overwrite what they can and the manifest reflects reality.
        let _ = fs::remove_dir_all(extract_root);
    }

    let tmp_path = tmp_dir.keep();
    fs::rename(&tmp_path, extract_root)
        .map_err(|e| LauncherError::Extraction(format!("failed to move extraction into place: {e}")))?;

    log::info!("Extracted bundled app to {}", extract_root.display());
    Ok(())
}

fn decompress_and_unpack(compressed_payload: &[u8], dest: &Path) -> Result<()> {
    let decompressed = zstd::stream::decode_all(Cursor::new(compressed_payload))
        .map_err(|e| LauncherError::Archive(format!("zstd decompression failed: {e}")))?;

    let mut ar = tar::Archive::new(Cursor::new(decompressed));
    ar.set_preserve_permissions(true);
    ar.unpack(dest)
        .map_err(|e| LauncherError::Archive(format!("tar extraction failed: {e}")))?;

    Ok(())
}

/// Minimal sanity check that extraction produced a usable bundle: a PHP
/// binary under `php/` and an application document root under `app/`.
fn sanity_check(dir: &Path) -> bool {
    let php_dir = dir.join("php");
    let has_php = php_dir.join("php.exe").is_file() || php_dir.join("php").is_file();
    let has_app = dir.join("app").is_dir();
    has_php && has_app
}
