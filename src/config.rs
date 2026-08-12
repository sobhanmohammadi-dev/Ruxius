use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CONFIG_FILE_NAME: &str = "config.json";

/// Named PHP installs registered with `rux php add <name> <path>`, so
/// `rux build <app> <name> <output>` can refer to them by name instead of
/// a full path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub php_versions: BTreeMap<String, PathBuf>,
}

impl AppConfig {
    /// The default (non-portable) config location, under the app data dir.
    pub fn config_path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONFIG_FILE_NAME)
    }

    /// Where a *portable* `config.json` would live: right next to the
    /// currently running executable. If someone drops (or a previous save
    /// already wrote) a config file there, Ruxius prefers it over
    /// `%LOCALAPPDATA%` — handy for carrying `ruxius.exe` plus its PHP
    /// registry around on a USB stick or between machines without an
    /// install step.
    fn portable_config_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        Some(dir.join(CONFIG_FILE_NAME))
    }

    /// Picks the config file to actually use: the portable one next to the
    /// executable if it already exists, otherwise the default location
    /// under the app data dir.
    fn resolve_path(data_dir: &Path) -> PathBuf {
        if let Some(portable) = Self::portable_config_path() {
            if portable.is_file() {
                return portable;
            }
        }
        Self::config_path(data_dir)
    }

    /// Loads the config from disk, falling back to defaults if no config
    /// file exists yet or it can't be parsed.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::resolve_path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Saves to whichever location `load` would read from: the portable
    /// config next to the executable if one already exists there,
    /// otherwise the default `%LOCALAPPDATA%` location.
    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        let path = Self::resolve_path(data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Resolves a `php` argument that may be either a name registered via
    /// `rux php add` or a literal filesystem path.
    pub fn resolve_php_reference(&self, reference: &str) -> PathBuf {
        match self.php_versions.get(reference) {
            Some(path) => path.clone(),
            None => PathBuf::from(reference),
        }
    }

    /// Where `.pack` files (see `pack.rs`) live: next to a portable config
    /// if one is in use, so an archived install travels with the tool the
    /// same way the registry does — otherwise under the app data dir.
    pub fn packs_dir(data_dir: &Path) -> PathBuf {
        if let Some(portable) = Self::portable_config_path() {
            if portable.is_file() {
                if let Some(dir) = portable.parent() {
                    return dir.join("packs");
                }
            }
        }
        data_dir.join("packs")
    }
}
