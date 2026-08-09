use crate::error::{LauncherError, Result};
use crate::payload::{self, BuildMeta};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_FILE_NAME: &str = "manifest.json";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    checksum: String,
}

/// Ensures the given payload (php + app archives, see `payload.rs`) is
/// extracted and up to date at `extract_root`. Skips extraction entirely
/// if a prior extraction already matches `checksum_hex`. Returns the
/// directory containing the extracted `php/`/`app/` folders and the app's
/// window metadata.
pub fn ensure_extracted(
    extract_root: &Path,
    raw_payload: &[u8],
    checksum_hex: &str,
) -> Result<(PathBuf, BuildMeta)> {
    let unpacked = payload::unpack(raw_payload)?;
    let manifest_path = extract_root.join(MANIFEST_FILE_NAME);

    if extract_root.exists() {
        if let Some(existing) = read_manifest(&manifest_path) {
            if existing.checksum == checksum_hex && sanity_check(extract_root) {
                log::info!("Bundled app already up to date, skipping extraction.");
                return Ok((extract_root.to_path_buf(), unpacked.meta));
            }
            log::info!("Bundled app is outdated or incomplete; re-extracting.");
        } else {
            log::info!("No previous extraction manifest found; extracting fresh.");
        }
    }

    extract_fresh(extract_root, unpacked.php_archive, unpacked.app_archive)?;

    let manifest = Manifest {
        checksum: checksum_hex.to_string(),
    };
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| LauncherError::Extraction(format!("failed to serialize manifest: {e}")))?;
    fs::write(&manifest_path, json)
        .map_err(|e| LauncherError::Extraction(format!("failed to write manifest: {e}")))?;

    Ok((extract_root.to_path_buf(), unpacked.meta))
}

fn read_manifest(path: &Path) -> Option<Manifest> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Wipes any partial/stale extraction and unpacks both archives atomically
/// via a temp-directory-then-rename strategy, so a crash mid-extraction
/// never leaves a half-written app behind.
fn extract_fresh(extract_root: &Path, php_archive: &[u8], app_archive: &[u8]) -> Result<()> {
    let parent = extract_root
        .parent()
        .ok_or_else(|| LauncherError::Extraction("extract root has no parent directory".into()))?;
    fs::create_dir_all(parent)?;

    let tmp_dir = tempfile::Builder::new()
        .prefix(".ruxius-extract-")
        .tempdir_in(parent)
        .map_err(|e| LauncherError::Extraction(format!("failed to create temp dir: {e}")))?;

    let php_dest = tmp_dir.path().join("php");
    let app_dest = tmp_dir.path().join("app");
    fs::create_dir_all(&php_dest)?;
    fs::create_dir_all(&app_dest)?;

    // The two archives are independent, so decompress-and-unpack both at
    // once instead of one after the other.
    let (php_result, app_result) = std::thread::scope(|scope| {
        let php_handle = scope.spawn(|| payload::unpack_archive_into(php_archive, &php_dest));
        let app_handle = scope.spawn(|| payload::unpack_archive_into(app_archive, &app_dest));
        (php_handle.join(), app_handle.join())
    });
    php_result.map_err(|_| LauncherError::Extraction("php extraction thread panicked".into()))??;
    app_result.map_err(|_| LauncherError::Extraction("app extraction thread panicked".into()))??;

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

/// Minimal sanity check that extraction produced a usable bundle: a PHP
/// binary under `php/` and an application document root under `app/`.
fn sanity_check(dir: &Path) -> bool {
    let php_dir = dir.join("php");
    let has_php = php_dir.join("php.exe").is_file() || php_dir.join("php").is_file();
    let has_app = dir.join("app").is_dir();
    has_php && has_app
}
