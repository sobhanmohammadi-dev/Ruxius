//! Manages the `extension=`/`zend_extension=` lines in a PHP install's
//! `php.ini` — list what's configured (and what's merely available in
//! `ext/` but not turned on), enable, and disable, without hand-editing
//! the file.

use crate::error::{LauncherError, Result};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ExtensionStatus {
    /// The value exactly as written in php.ini, e.g. "curl" or "php_curl.dll".
    pub raw_value: String,
    /// Normalized name used for matching/display, e.g. "curl".
    pub name: String,
    pub enabled: bool,
    pub zend: bool,
}

/// True if this extension is only known because a matching DLL exists in
/// `ext/`, not because php.ini mentions it at all.
#[derive(Debug, Clone)]
pub struct AvailableExtension {
    pub name: String,
}

fn normalize(value: &str) -> String {
    let mut v = value.trim();
    if let Some(stripped) = v.strip_prefix("php_").or_else(|| v.strip_prefix("PHP_")) {
        v = stripped;
    }
    for suffix in [".dll", ".DLL", ".so", ".SO"] {
        if let Some(stripped) = v.strip_suffix(suffix) {
            v = stripped;
            break;
        }
    }
    v.to_ascii_lowercase()
}

/// Rejects anything that isn't a plausible extension name before it's
/// ever written into php.ini. `set_enabled` only ever emits a new line as
/// `extension=<ext_name>` verbatim, so without this check a crafted
/// argument containing a newline (or `;`, `[`, `]`) could inject arbitrary
/// ini directives — a real concern once this is reachable from the GUI,
/// not just a trusted shell argument.
fn is_safe_extension_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn read_lines(php_ini: &Path) -> Result<(Vec<String>, bool)> {
    let raw = std::fs::read_to_string(php_ini)
        .map_err(|e| LauncherError::Extraction(format!("reading {}: {e}", php_ini.display())))?;
    let uses_crlf = raw.contains("\r\n");
    let lines = raw.lines().map(|l| l.to_string()).collect();
    Ok((lines, uses_crlf))
}

fn write_lines(php_ini: &Path, lines: &[String], uses_crlf: bool) -> Result<()> {
    backup_original_once(php_ini);

    let sep = if uses_crlf { "\r\n" } else { "\n" };
    let mut content = lines.join(sep);
    content.push_str(sep);

    let parent = php_ini
        .parent()
        .ok_or_else(|| LauncherError::Extraction("php.ini has no parent directory".into()))?;
    let tmp_path = parent.join(".ruxius-php-ini.tmp");

    // Atomic write: write the full new content to a temp file first, then
    // rename over the original — a crash or power loss mid-write leaves
    // either the untouched original or the complete new file, never a
    // half-written php.ini that would break every extension on next launch.
    std::fs::write(&tmp_path, &content)
        .map_err(|e| LauncherError::Extraction(format!("writing {}: {e}", tmp_path.display())))?;
    std::fs::rename(&tmp_path, php_ini)
        .map_err(|e| LauncherError::Extraction(format!("replacing {}: {e}", php_ini.display())))?;
    Ok(())
}

/// Copies `php_ini` to `php_ini.orig` the first time Ruxius is about to
/// modify it — never overwritten after that, so there's always an
/// unmodified baseline to manually restore from, no matter how many times
/// extensions get toggled afterward. Best-effort: if the copy fails (e.g.
/// a read-only parent directory) the actual edit still proceeds, since a
/// missing backup shouldn't block a change the user asked for.
fn backup_original_once(php_ini: &Path) {
    let backup_path = php_ini.with_extension("ini.orig");
    if backup_path.exists() {
        return;
    }
    if let Err(e) = std::fs::copy(php_ini, &backup_path) {
        log::warn!("Couldn't create php.ini backup at {}: {e}", backup_path.display());
    }
}

/// Parses every `extension=`/`zend_extension=` line (enabled or commented
/// out) in `php_ini`.
pub fn list_configured(php_ini: &Path) -> Result<Vec<ExtensionStatus>> {
    let (lines, _) = read_lines(php_ini)?;
    let mut out = Vec::new();

    for line in &lines {
        let trimmed = line.trim();
        let (enabled, body) = match trimmed.strip_prefix(';') {
            Some(rest) => (false, rest.trim_start()),
            None => (true, trimmed),
        };

        let (zend, rest) = if let Some(v) = body.strip_prefix("zend_extension=") {
            (true, v)
        } else if let Some(v) = body.strip_prefix("extension=") {
            (false, v)
        } else {
            continue;
        };

        let raw_value = rest.trim().to_string();
        if raw_value.is_empty() {
            continue;
        }
        out.push(ExtensionStatus {
            name: normalize(&raw_value),
            raw_value,
            enabled,
            zend,
        });
    }

    Ok(out)
}

/// Extensions that have a `php_<name>.dll` in `ext_dir` but aren't
/// mentioned in php.ini at all (enabled or disabled) — available to turn
/// on but not yet configured either way.
pub fn list_available_unconfigured(
    php_ini: &Path,
    ext_dir: &Path,
) -> Result<Vec<AvailableExtension>> {
    let configured = list_configured(php_ini)?;
    let configured_names: std::collections::HashSet<String> =
        configured.iter().map(|e| e.name.clone()).collect();

    let Ok(entries) = std::fs::read_dir(ext_dir) else {
        return Ok(Vec::new());
    };

    let mut available: Vec<AvailableExtension> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str())?.eq_ignore_ascii_case("dll") {
                let stem = path.file_stem()?.to_string_lossy().into_owned();
                let name = normalize(&stem);
                if !configured_names.contains(&name) {
                    return Some(AvailableExtension { name });
                }
            }
            None
        })
        .collect();

    available.sort_by(|a, b| a.name.cmp(&b.name));
    available.dedup_by(|a, b| a.name == b.name);
    Ok(available)
}

/// Outcome of an enable/disable request, for clear reporting back to the
/// user (CLI or GUI) about what actually happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleOutcome {
    Changed,
    AlreadyInThatState,
    AddedNewLine,
    NotAvailable,
}

/// Enables (`enabled = true`) or disables (`enabled = false`) the named
/// extension in `php_ini`. If it's not mentioned in php.ini at all but a
/// matching DLL exists in `ext_dir`, enabling it appends a fresh
/// `extension=<name>` line rather than failing.
pub fn set_enabled(
    php_ini: &Path,
    ext_dir: Option<&Path>,
    ext_name: &str,
    enabled: bool,
) -> Result<ToggleOutcome> {
    if !is_safe_extension_name(ext_name) {
        return Err(LauncherError::Extraction(format!(
            "'{ext_name}' isn't a valid extension name (letters, digits, '_', '-', '.' only)"
        )));
    }

    let target = normalize(ext_name);
    let (mut lines, uses_crlf) = read_lines(php_ini)?;

    for line in lines.iter_mut() {
        let trimmed = line.trim();
        let (currently_enabled, body) = match trimmed.strip_prefix(';') {
            Some(rest) => (false, rest.trim_start()),
            None => (true, trimmed),
        };
        let value_part = body
            .strip_prefix("zend_extension=")
            .or_else(|| body.strip_prefix("extension="));

        let Some(value) = value_part else { continue };
        if normalize(value) != target {
            continue;
        }

        if currently_enabled == enabled {
            return Ok(ToggleOutcome::AlreadyInThatState);
        }

        *line = if enabled {
            // Uncomment: drop one leading ';' (and one following space, if any).
            let without_semi = line.trim_start();
            let without_semi = without_semi.strip_prefix(';').unwrap_or(without_semi);
            without_semi.strip_prefix(' ').unwrap_or(without_semi).to_string()
        } else {
            format!(";{line}")
        };

        write_lines(php_ini, &lines, uses_crlf)?;
        return Ok(ToggleOutcome::Changed);
    }

    // Not mentioned in php.ini at all.
    if !enabled {
        // Disabling something that was never configured — nothing to do.
        return Ok(ToggleOutcome::AlreadyInThatState);
    }

    if let Some(ext_dir) = ext_dir {
        let dll_exists = std::fs::read_dir(ext_dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .file_stem()
                    .map(|s| normalize(&s.to_string_lossy()) == target)
                    .unwrap_or(false)
            })
        });
        if !dll_exists {
            return Ok(ToggleOutcome::NotAvailable);
        }
    }

    lines.push(format!("extension={ext_name}"));
    write_lines(php_ini, &lines, uses_crlf)?;
    Ok(ToggleOutcome::AddedNewLine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ini(dir: &Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("php.ini");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn normalize_strips_prefix_suffix_and_case() {
        assert_eq!(normalize("curl"), "curl");
        assert_eq!(normalize("php_curl"), "curl");
        assert_eq!(normalize("php_curl.dll"), "curl");
        assert_eq!(normalize("PHP_CURL.DLL"), "curl");
        assert_eq!(normalize("  curl  "), "curl");
        assert_eq!(normalize("mbstring.so"), "mbstring");
    }

    #[test]
    fn safe_extension_names_accepted() {
        assert!(is_safe_extension_name("curl"));
        assert!(is_safe_extension_name("php_curl.dll"));
        assert!(is_safe_extension_name("some-ext_1.2"));
    }

    #[test]
    fn unsafe_extension_names_rejected() {
        assert!(!is_safe_extension_name(""));
        assert!(!is_safe_extension_name("curl\nzend_extension=evil"));
        assert!(!is_safe_extension_name("curl;evil=1"));
        assert!(!is_safe_extension_name("curl][section]"));
        assert!(!is_safe_extension_name(&"a".repeat(200)));
    }

    #[test]
    fn set_enabled_rejects_unsafe_names_before_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "extension=curl\n");
        let result = set_enabled(&ini, None, "curl\nevil=1", true);
        assert!(result.is_err());
        // The file must be untouched — an injection attempt should never
        // even reach the read/modify/write path.
        assert_eq!(std::fs::read_to_string(&ini).unwrap(), "extension=curl\n");
    }

    #[test]
    fn list_configured_parses_enabled_disabled_and_zend_lines() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(
            dir.path(),
            "; a comment\nextension=curl\n;extension=gd\nzend_extension=opcache\n",
        );

        let configured = list_configured(&ini).unwrap();
        assert_eq!(configured.len(), 3);

        let curl = configured.iter().find(|e| e.name == "curl").unwrap();
        assert!(curl.enabled);
        assert!(!curl.zend);

        let gd = configured.iter().find(|e| e.name == "gd").unwrap();
        assert!(!gd.enabled);

        let opcache = configured.iter().find(|e| e.name == "opcache").unwrap();
        assert!(opcache.enabled);
        assert!(opcache.zend);
    }

    #[test]
    fn set_enabled_disables_an_enabled_line() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "extension=curl\n");

        let outcome = set_enabled(&ini, None, "curl", false).unwrap();
        assert_eq!(outcome, ToggleOutcome::Changed);

        let content = std::fs::read_to_string(&ini).unwrap();
        assert!(content.contains(";extension=curl"));
    }

    #[test]
    fn set_enabled_enables_a_disabled_line() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), ";extension=curl\n");

        let outcome = set_enabled(&ini, None, "curl", true).unwrap();
        assert_eq!(outcome, ToggleOutcome::Changed);

        let content = std::fs::read_to_string(&ini).unwrap();
        assert!(content.lines().any(|l| l == "extension=curl"));
    }

    #[test]
    fn set_enabled_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "extension=curl\n");

        let outcome = set_enabled(&ini, None, "curl", true).unwrap();
        assert_eq!(outcome, ToggleOutcome::AlreadyInThatState);
    }

    #[test]
    fn set_enabled_adds_a_new_line_when_dll_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "");
        let ext_dir = dir.path().join("ext");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("php_curl.dll"), b"").unwrap();

        let outcome = set_enabled(&ini, Some(&ext_dir), "curl", true).unwrap();
        assert_eq!(outcome, ToggleOutcome::AddedNewLine);

        let content = std::fs::read_to_string(&ini).unwrap();
        assert!(content.contains("extension=curl"));
    }

    #[test]
    fn set_enabled_refuses_to_add_a_line_with_no_matching_dll() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "");
        let ext_dir = dir.path().join("ext");
        std::fs::create_dir_all(&ext_dir).unwrap();
        // No php_curl.dll in here.

        let outcome = set_enabled(&ini, Some(&ext_dir), "curl", true).unwrap();
        assert_eq!(outcome, ToggleOutcome::NotAvailable);
        assert_eq!(std::fs::read_to_string(&ini).unwrap(), "");
    }

    #[test]
    fn list_available_unconfigured_excludes_already_configured_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "extension=curl\n");
        let ext_dir = dir.path().join("ext");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("php_curl.dll"), b"").unwrap();
        std::fs::write(ext_dir.join("php_gd.dll"), b"").unwrap();

        let available = list_available_unconfigured(&ini, &ext_dir).unwrap();
        let names: Vec<&str> = available.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["gd"]); // curl is already configured, so excluded
    }

    #[test]
    fn backup_is_created_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "extension=curl\n");
        let backup = ini.with_extension("ini.orig");

        set_enabled(&ini, None, "curl", false).unwrap();
        assert!(backup.is_file());
        let first_backup_content = std::fs::read_to_string(&backup).unwrap();
        assert_eq!(first_backup_content, "extension=curl\n");

        // A second edit must not touch the backup again, even though the
        // live file has changed since.
        set_enabled(&ini, None, "curl", true).unwrap();
        let second_backup_content = std::fs::read_to_string(&backup).unwrap();
        assert_eq!(second_backup_content, first_backup_content);
    }

    #[test]
    fn write_lines_preserves_crlf_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let ini = write_ini(dir.path(), "extension=curl\r\nextension=gd\r\n");

        set_enabled(&ini, None, "curl", false).unwrap();

        let content = std::fs::read_to_string(&ini).unwrap();
        assert!(content.contains("\r\n"), "should keep CRLF line endings");
        assert!(!content.replace("\r\n", "").contains('\n'), "should not introduce bare LF");
    }
}
