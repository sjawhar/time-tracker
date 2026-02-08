//! Configuration loading and management.

use std::fmt;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

/// Application configuration.
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the database file.
    pub database_path: PathBuf,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_path", &self.database_path)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        let data_dir = dirs_config_path().unwrap_or_else(|| PathBuf::from("."));
        Self {
            database_path: data_dir.join("tt.db"),
        }
    }
}

impl Config {
    /// Loads configuration from default locations.
    #[allow(clippy::result_large_err)]
    pub fn load() -> Result<Self, figment::Error> {
        Self::load_from(None)
    }

    /// Loads configuration, optionally from a specific file.
    #[allow(clippy::result_large_err)]
    pub fn load_from(config_path: Option<&Path>) -> Result<Self, figment::Error> {
        let mut figment = Figment::from(Serialized::defaults(Self::default()));

        // Load from default config location
        if let Some(config_dir) = dirs_config_path() {
            figment = figment.merge(Toml::file(config_dir.join("config.toml")));
        }

        // Load from specified config file
        if let Some(path) = config_path {
            figment = figment.merge(Toml::file(path));
        }

        // Load from environment variables (TT_*)
        figment = figment.merge(Env::prefixed("TT_"));

        figment.extract()
    }
}

/// Returns the platform-specific config directory for tt.
fn dirs_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("tt"))
}
