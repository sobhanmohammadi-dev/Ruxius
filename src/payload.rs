//! Ruxius ships as a single generic `ruxius.exe`. Turning it into a
//! distributable app doesn't involve recompiling anything: `rux build`
//! takes a copy of the *currently running* executable, appends a payload
//! describing a PHP install plus an application's files, and writes a
//! small footer at the very end recording where that payload starts. Any
//! Ruxius executable — the freshly built `ruxius.exe` or an app someone
//! already built — can then detect whether it has a payload appended to
//! itself and, if so, run it.
//!
//! File layout of a built executable:
//!
//! ```text
//! [ ...original ruxius.exe bytes... ][ payload (see below) ][ footer (48 bytes) ]
//! ```
//!
//! Footer (fixed 48 bytes at EOF): `sha256(payload)` (32 bytes) +
//! `payload_len` as u64 little-endian (8 bytes) + magic `b"RUXPAY01"` (8 bytes).
//!
//! Payload layout: `[php_len: u64][zstd(tar(php/))][app_len: u64][zstd(tar(app/))][meta_len: u32][meta.json]`.
//! The PHP archive is split out on its own so it can be cached across
//! builds — for a given PHP install it never changes, so `rux build` skips
//! re-walking and re-compressing it (typically the bulk of the payload)
//! unless that install's files have actually changed.

use crate::error::{LauncherError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;

const MAGIC: &[u8; 8] = b"RUXPAY01";
const FOOTER_LEN: u64 = 32 + 8 + 8;

/// Per-app window settings, chosen at `rux build` time and carried inside
/// the payload so a built app doesn't need any command-line arguments to
/// know its own title/size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildMeta {
    pub title: String,
    pub width: u32,
    pub height: u32,
}

impl Default for BuildMeta {
    fn default() -> Self {
        Self {
            title: "Ruxius App".to_string(),
            width: 1400,
            height: 900,
        }
    }
}

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

/// The php/ and app/ archives plus metadata, split back out of a payload.
pub struct UnpackedPayload<'a> {
    pub php_archive: &'a [u8],
    pub app_archive: &'a [u8],
    pub meta: BuildMeta,
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

/// Reads the raw payload bytes out of `exe_path` and verifies them against
/// the footer's checksum.
pub fn read_payload_bytes(exe_path: &Path, info: &PayloadInfo) -> Result<Vec<u8>> {
    let mut file = File::open(exe_path)?;
    file.seek(SeekFrom::Start(info.offset))?;
    let mut buf = vec![0u8; info.len as usize];
    file.read_exact(&mut buf)?;

    let actual = sha256(&buf);
    if actual != info.checksum {
        return Err(LauncherError::ChecksumMismatch {
            expected: hex::encode(info.checksum),
            actual: hex::encode(actual),
        });
    }

    Ok(buf)
}

/// Splits a raw payload back into its php archive, app archive, and
/// metadata.
pub fn unpack<'a>(payload: &'a [u8]) -> Result<UnpackedPayload<'a>> {
    let mut cursor = 0usize;
    let read_u64 = |buf: &'a [u8], at: usize| -> Result<u64> {
        let bytes: [u8; 8] = buf
            .get(at..at + 8)
            .ok_or_else(|| LauncherError::Archive("payload truncated".into()))?
            .try_into()
            .unwrap();
        Ok(u64::from_le_bytes(bytes))
    };

    let php_len = read_u64(payload, cursor)? as usize;
    cursor += 8;
    let php_archive = payload
        .get(cursor..cursor + php_len)
        .ok_or_else(|| LauncherError::Archive("payload truncated (php section)".into()))?;
    cursor += php_len;

    let app_len = read_u64(payload, cursor)? as usize;
    cursor += 8;
    let app_archive = payload
        .get(cursor..cursor + app_len)
        .ok_or_else(|| LauncherError::Archive("payload truncated (app section)".into()))?;
    cursor += app_len;

    let meta_len_bytes: [u8; 4] = payload
        .get(cursor..cursor + 4)
        .ok_or_else(|| LauncherError::Archive("payload truncated (meta length)".into()))?
        .try_into()
        .unwrap();
    cursor += 4;
    let meta_len = u32::from_le_bytes(meta_len_bytes) as usize;
    let meta_bytes = payload
        .get(cursor..cursor + meta_len)
        .ok_or_else(|| LauncherError::Archive("payload truncated (meta section)".into()))?;

    let meta: BuildMeta = serde_json::from_slice(meta_bytes)
        .map_err(|e| LauncherError::Archive(format!("invalid metadata: {e}")))?;

    Ok(UnpackedPayload {
        php_archive,
        app_archive,
        meta,
    })
}

/// Decompresses one of the split archives (php or app) into `dest`.
pub fn unpack_archive_into(archive: &[u8], dest: &Path) -> Result<()> {
    let decompressed = zstd::stream::decode_all(Cursor::new(archive))
        .map_err(|e| LauncherError::Archive(format!("zstd decompression failed: {e}")))?;

    let mut ar = tar::Archive::new(Cursor::new(decompressed));
    ar.set_preserve_permissions(true);
    ar.unpack(dest)
        .map_err(|e| LauncherError::Archive(format!("tar extraction failed: {e}")))?;

    Ok(())
}

/// Packs a PHP install directory and an application directory into a
/// payload, embedding `meta`. The PHP archive is cached under `cache_dir`
/// keyed by a fingerprint of each directory's contents, so rebuilding the
/// same app skips re-walking and re-compressing whichever side (PHP,
/// app, or both) hasn't actually changed since the last build. Both are
/// checked and, if needed, rebuilt concurrently on separate threads, and
/// each is compressed using all available CPU cores.
pub fn pack(php_dir: &Path, app_dir: &Path, meta: &BuildMeta, cache_dir: &Path) -> Result<Vec<u8>> {
    let (php_result, app_result) = thread::scope(|scope| {
        let php_handle = scope.spawn(|| cached_or_build_archive("php", php_dir, cache_dir));
        let app_handle = scope.spawn(|| cached_or_build_archive("app", app_dir, cache_dir));
        (php_handle.join(), app_handle.join())
    });

    let php_archive =
        php_result.map_err(|_| LauncherError::Archive("PHP packing thread panicked".into()))??;
    let app_archive =
        app_result.map_err(|_| LauncherError::Archive("app packing thread panicked".into()))??;

    let meta_bytes = serde_json::to_vec(meta)
        .map_err(|e| LauncherError::Archive(format!("failed to serialize metadata: {e}")))?;

    let mut out = Vec::with_capacity(php_archive.len() + app_archive.len() + meta_bytes.len() + 24);
    out.extend_from_slice(&(php_archive.len() as u64).to_le_bytes());
    out.extend_from_slice(&php_archive);
    out.extend_from_slice(&(app_archive.len() as u64).to_le_bytes());
    out.extend_from_slice(&app_archive);
    out.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&meta_bytes);
    Ok(out)
}

/// Returns a cached compressed archive of `dir` if one exists for its
/// current fingerprint, otherwise builds it fresh and caches the result.
/// `kind` ("php" or "app") just namespaces the cache file so the two don't
/// collide.
/// One entry from a directory walk, kept around so both fingerprinting and
/// archiving can share a single traversal instead of walking twice.
struct DirEntry {
    abs_path: PathBuf,
    rel_path: PathBuf,
    is_dir: bool,
}

/// Walks `dir` once and returns every entry sorted by relative path (the
/// deterministic order both the fingerprint and the tar archive need).
/// Both `cached_or_build_archive`'s fingerprint check and, on a cache miss,
/// the archive build itself run off this single list rather than each
/// re-walking the directory tree independently.
fn walk_sorted(dir: &Path) -> Vec<DirEntry> {
    let mut entries: Vec<DirEntry> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path() != dir)
        .map(|e| {
            let abs_path = e.path().to_path_buf();
            let rel_path = abs_path
                .strip_prefix(dir)
                .unwrap_or(&abs_path)
                .to_path_buf();
            DirEntry {
                abs_path,
                rel_path,
                is_dir: e.file_type().is_dir(),
            }
        })
        .collect();
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    entries
}

/// Splits `items` into chunks (one per available CPU core, capped at
/// `items.len()`) and processes each chunk on its own thread, preserving
/// order in the returned `Vec`. Falls back to a plain sequential map for
/// small inputs or single-core machines, where spawning threads would cost
/// more than it saves.
pub fn parallel_map<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    if workers <= 1 || items.len() < 32 {
        return items.iter().map(|item| f(item)).collect();
    }

    let chunk_size = items.len().div_ceil(workers);
    thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(|| chunk.iter().map(|item| f(item)).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

fn cached_or_build_archive(kind: &str, dir: &Path, cache_dir: &Path) -> Result<Vec<u8>> {
    if !dir.is_dir() {
        return Err(LauncherError::Archive(format!(
            "{} is not a directory",
            dir.display()
        )));
    }

    let entries = walk_sorted(dir);
    let fingerprint = fingerprint_entries(&entries);
    let cache_path = cache_dir.join(format!("{kind}-{fingerprint}.tar.zst"));

    if let Ok(cached) = std::fs::read(&cache_path) {
        log::info!("Reusing cached {kind} archive ({fingerprint})");
        return Ok(cached);
    }

    log::info!("No cached {kind} archive for {fingerprint}; packing fresh.");
    let archive = tar_zstd_entries(&entries)?;

    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        log::warn!("Couldn't create {kind} archive cache dir: {e}");
    } else if let Err(e) = std::fs::write(&cache_path, &archive) {
        log::warn!("Couldn't write {kind} archive cache: {e}");
    }

    Ok(archive)
}

/// A cheap fingerprint of a directory's contents (relative path, size, and
/// modified time of every file), used to decide whether a cached archive
/// is still valid — much faster than hashing file contents, and good
/// enough since we only need to detect "this directory changed".
/// The `stat` call for each file is the actual cost here (a syscall per
/// file), so those run in parallel; the final hash fold is cheap enough to
/// stay single-threaded (and needs to, since `Sha256` is a sequential
/// state machine).
fn fingerprint_entries(entries: &[DirEntry]) -> String {
    let files: Vec<&DirEntry> = entries.iter().filter(|e| !e.is_dir).collect();

    let stats: Vec<Option<(u64, u64)>> = parallel_map(&files, |entry| {
        let meta = std::fs::metadata(&entry.abs_path).ok()?;
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Some((meta.len(), mtime_secs))
    });

    let mut hasher = Sha256::new();
    for (entry, stat) in files.iter().zip(stats.into_iter()) {
        hasher.update(entry.rel_path.to_string_lossy().as_bytes());
        if let Some((len, mtime)) = stat {
            hasher.update(len.to_le_bytes());
            hasher.update(mtime.to_le_bytes());
        }
    }

    hex::encode(hasher.finalize())[..24].to_string()
}

/// Builds a zstd-compressed tar from an already-walked entry list. File
/// contents are read in parallel across threads (the actual I/O-bound
/// work); the tar itself is still assembled on a single thread afterwards,
/// since `tar::Builder` writes to one sequential stream — but by then
/// every read has already completed, so that step is just memory copies.
fn tar_zstd_entries(entries: &[DirEntry]) -> Result<Vec<u8>> {
    let files: Vec<&DirEntry> = entries.iter().filter(|e| !e.is_dir).collect();
    let contents: Vec<std::io::Result<Vec<u8>>> =
        parallel_map(&files, |entry| std::fs::read(&entry.abs_path));

    let tar_buf: Vec<u8> = Vec::new();
    let mut builder = tar::Builder::new(tar_buf);
    let mut contents = contents.into_iter();

    for entry in entries {
        if entry.is_dir {
            builder
                .append_dir(&entry.rel_path, &entry.abs_path)
                .map_err(|e| {
                    LauncherError::Archive(format!("adding dir {:?}: {e}", entry.abs_path))
                })?;
        } else {
            let data = contents
                .next()
                .expect("contents has one entry per file, in the same order")
                .map_err(|e| {
                    LauncherError::Archive(format!("reading {:?}: {e}", entry.abs_path))
                })?;

            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder
                .append_data(&mut header, &entry.rel_path, data.as_slice())
                .map_err(|e| {
                    LauncherError::Archive(format!("adding file {:?}: {e}", entry.abs_path))
                })?;
        }
    }

    let tar_bytes = builder
        .into_inner()
        .map_err(|e| LauncherError::Archive(format!("failed to finalize tar: {e}")))?;

    compress_zstd_mt(&tar_bytes, 19)
}

/// Compresses `data` with zstd at `level`, using multiple threads when more
/// than one CPU is available. On the sizes involved here (a PHP install can
/// be tens of megabytes), letting zstd split the work across cores cuts
/// compression time significantly compared to a single thread.
fn compress_zstd_mt(data: &[u8], level: i32) -> Result<Vec<u8>> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), level)
        .map_err(|e| LauncherError::Archive(format!("zstd encoder init failed: {e}")))?;

    let workers = thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    if workers > 1 {
        // Best-effort: if the underlying zstd build doesn't support this for
        // some reason, we just silently fall back to single-threaded.
        let _ = encoder.multithread(workers);
    }

    encoder
        .write_all(data)
        .map_err(|e| LauncherError::Archive(format!("zstd compression failed: {e}")))?;
    encoder
        .finish()
        .map_err(|e| LauncherError::Archive(format!("zstd compression failed: {e}")))
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

    let checksum = sha256(payload);
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

/// The checksum an already-built `output_path` would need to match for
/// `payload` to be considered "already up to date" — used by `rux build`
/// to skip rewriting an output file whose content wouldn't actually change.
pub fn matches_existing_output(output_path: &Path, payload: &[u8]) -> bool {
    let Ok(info) = detect(output_path) else {
        return false;
    };
    match info {
        Some(info) => info.checksum == sha256(payload),
        None => false,
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}
