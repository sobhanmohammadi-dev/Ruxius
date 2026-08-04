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
    pub fn config_path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONFIG_FILE_NAME)
    }

    /// Loads the config from disk, falling back to defaults if no config
    /// file exists yet or it can't be parsed.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::config_path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::config_path(data_dir);
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
}
