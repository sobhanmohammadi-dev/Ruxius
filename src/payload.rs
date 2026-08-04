//! Ruxius ships as a single generic `ruxius.exe`. Turning it into a
//! distributable app doesn't involve recompiling anything: `rux build`
//! takes a copy of the *currently running* executable, appends a
//! zstd-compressed tar of a PHP install (`php/`) plus an application's
//! files (`app/`), and writes a small footer at the very end describing
//! where that payload starts. Any Ruxius executable — the freshly built
//! `ruxius.exe` or an app someone already built — can then detect whether it
//! has a payload appended to itself and, if so, run it.
//!
//! File layout of a built executable:
//!
//! ```text
//! [ ...original ruxius.exe bytes... ][ zstd(tar(php/ + app/)) ][ footer (48 bytes) ]
//! ```
//!
//! Footer (fixed 48 bytes at EOF): `sha256(payload)` (32 bytes) +
//! `payload_len` as u64 little-endian (8 bytes) + magic `b"RUXPAY01"` (8 bytes).

use crate::error::{LauncherError, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"RUXPAY01";
const FOOTER_LEN: u64 = 32 + 8 + 8;

/// Location and identity of a payload appended to an executable.
#[derive(Debug, Clone)]
pub struct PayloadInfo {
    pub offset: u64,
    pub len: u64,
    pub checksum: [u8; 32],
}

impl PayloadInfo {
    pub fn checksum_hex(&self) -> String {
        hex::encode(self.checksum)
    }
}

/// Checks whether `exe_path` has a Ruxius payload appended to it. Returns
/// `None` for a bare/unbuilt `ruxius.exe` (the packager itself).
pub fn detect(exe_path: &Path) -> Result<Option<PayloadInfo>> {
    let mut file = File::open(exe_path)?;
    let total_len = file.metadata()?.len();
    if total_len < FOOTER_LEN {
        return Ok(None);
    }

    file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    file.read_exact(&mut footer)?;

    let magic = &footer[40..48];
    if magic != MAGIC {
        return Ok(None);
    }

    let mut checksum = [0u8; 32];
    checksum.copy_from_slice(&footer[0..32]);
    let len_bytes: [u8; 8] = footer[32..40].try_into().unwrap();
    let payload_len = u64::from_le_bytes(len_bytes);

    let stub_len = total_len.saturating_sub(FOOTER_LEN).saturating_sub(payload_len);
    if payload_len == 0 || stub_len == 0 || stub_len + payload_len + FOOTER_LEN != total_len {
        // Corrupt or truncated footer; treat as "no payload" rather than crash.
        return Ok(None);
    }

    Ok(Some(PayloadInfo {
        offset: stub_len,
        len: payload_len,
        checksum,
    }))
}

/// Reads the raw (still zstd-compressed) payload bytes out of `exe_path`.
pub fn read_payload_bytes(exe_path: &Path, info: &PayloadInfo) -> Result<Vec<u8>> {
    let mut file = File::open(exe_path)?;
    file.seek(SeekFrom::Start(info.offset))?;
    let mut buf = vec![0u8; info.len as usize];
    file.read_exact(&mut buf)?;

    let mut hasher = Sha256::new();
    hasher.update(&buf);
    let actual: [u8; 32] = hasher.finalize().into();
    if actual != info.checksum {
        return Err(LauncherError::ChecksumMismatch {
            expected: hex::encode(info.checksum),
            actual: hex::encode(actual),
        });
    }

    Ok(buf)
}

/// Packs a PHP install directory and an application directory into a single
/// zstd-compressed tar archive, laid out as `php/...` and `app/...`.
pub fn pack(php_dir: &Path, app_dir: &Path) -> Result<Vec<u8>> {
    let tar_buf: Vec<u8> = Vec::new();
    let mut builder = tar::Builder::new(tar_buf);

    add_dir_contents(&mut builder, php_dir, "php")?;
    add_dir_contents(&mut builder, app_dir, "app")?;

    let tar_bytes = builder
        .into_inner()
        .map_err(|e| LauncherError::Archive(format!("failed to finalize tar: {e}")))?;

    zstd::stream::encode_all(tar_bytes.as_slice(), 19)
        .map_err(|e| LauncherError::Archive(format!("zstd compression failed: {e}")))
}

fn add_dir_contents(
    builder: &mut tar::Builder<Vec<u8>>,
    src_dir: &Path,
    prefix: &str,
) -> Result<()> {
    if !src_dir.is_dir() {
        return Err(LauncherError::Archive(format!(
            "{} is not a directory",
            src_dir.display()
        )));
    }

    let mut entries: Vec<_> = walkdir::WalkDir::new(src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .collect();
    entries.sort();

    for path in entries {
        if path == src_dir {
            continue;
        }
        let rel = path.strip_prefix(src_dir).expect("path under src_dir");
        let archive_path = Path::new(prefix).join(rel);

        if path.is_dir() {
            builder
                .append_dir(&archive_path, &path)
                .map_err(|e| LauncherError::Archive(format!("adding dir {path:?}: {e}")))?;
        } else if path.is_file() {
            let mut f = File::open(&path)
                .map_err(|e| LauncherError::Archive(format!("opening {path:?}: {e}")))?;
            builder
                .append_file(&archive_path, &mut f)
                .map_err(|e| LauncherError::Archive(format!("adding file {path:?}: {e}")))?;
        }
    }

    Ok(())
}

/// Writes a new executable at `output_path`: the "stub" bytes of
/// `base_exe` (itself, with any existing payload stripped off) followed by
/// `payload` and a fresh footer. This is how `rux build` produces a
/// distributable app without invoking a compiler.
pub fn build_output(base_exe: &Path, payload: &[u8], output_path: &Path) -> Result<()> {
    let stub_len = match detect(base_exe)? {
        Some(existing) => existing.offset,
        None => File::open(base_exe)?.metadata()?.len(),
    };

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut src = File::open(base_exe)?;
    let mut out = File::create(output_path)?;

    let mut remaining = stub_len;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let chunk = remaining.min(buf.len() as u64) as usize;
        src.read_exact(&mut buf[..chunk])?;
        out.write_all(&buf[..chunk])?;
        remaining -= chunk as u64;
    }

    out.write_all(payload)?;

    let mut hasher = Sha256::new();
    hasher.update(payload);
    let checksum: [u8; 32] = hasher.finalize().into();

    out.write_all(&checksum)?;
    out.write_all(&(payload.len() as u64).to_le_bytes())?;
    out.write_all(MAGIC)?;
    out.flush()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = out.metadata()?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(output_path, perms)?;
    }

    Ok(())
}
