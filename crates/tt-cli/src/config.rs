//! Configuration loading and management.

use std::fmt;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::de::Error as _;
use serde::{Deserialize, Serialize};

/// Application configuration.
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the database file.
    pub database_path: PathBuf,
    /// Path to the markdown-backed todo store directory.
    pub todo_store_path: PathBuf,
    /// LLM classifier settings.
    #[serde(default)]
    pub classifier: ClassifierConfig,
    /// Maximum number of streams allowed in work in progress.
    #[serde(default = "default_wip_limit")]
    pub wip_limit: u32,
    /// Time window used for priority drift detection, in minutes.
    #[serde(default = "default_drift_window_min")]
    pub drift_window_min: u32,
    /// Human-input inactivity required before the timeline emits an idle fold, in minutes.
    #[serde(default = "default_idle_threshold_min")]
    pub idle_threshold_min: u32,
    /// HTTP server settings.
    #[serde(default)]
    pub serve: ServeConfig,
    /// Seconds between background ingestion scans.
    #[serde(default = "default_ingest_interval_s")]
    pub ingest_interval_s: u32,
    /// Seconds between synchronization scans.
    #[serde(default = "default_sync_interval_s")]
    pub sync_interval_s: u32,
}

/// LLM classifier configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClassifierConfig {
    /// Model used for classification.
    pub model: String,
    /// Minimum confidence required for automatic assignment.
    pub confidence_threshold: f64,
    /// Environment variable containing the provider API key.
    pub api_key_env: String,
    /// Read-only context-lookup command line; absent leaves the tool disabled.
    ///
    /// It is split with POSIX shell-word rules before the model's query is appended as one
    /// final argument.
    pub context_command: Option<String>,
    /// Operator-provided domain vocabulary, entity glossaries, or classification guidance.
    ///
    /// Trailing whitespace is ignored and the rendered value is capped to keep one
    /// classification prompt bounded.
    #[serde(default, deserialize_with = "deserialize_context_instructions")]
    pub context_instructions: Option<String>,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            model: "claude-haiku-4-5".to_string(),
            confidence_threshold: 0.8,
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            context_command: None,
            context_instructions: None,
        }
    }
}

/// HTTP server configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServeConfig {
    /// Port the local HTTP server listens on.
    pub port: u16,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self { port: 8765 }
    }
}

const MAX_CONTEXT_INSTRUCTIONS_BYTES: usize = 8 * 1024;

fn deserialize_context_instructions<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(mut context_instructions) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    context_instructions.truncate(context_instructions.trim_end().len());
    if context_instructions.len() > MAX_CONTEXT_INSTRUCTIONS_BYTES {
        return Err(D::Error::custom(format!(
            "classifier.context_instructions exceeds {MAX_CONTEXT_INSTRUCTIONS_BYTES} bytes"
        )));
    }
    Ok((!context_instructions.is_empty()).then_some(context_instructions))
}

const fn default_wip_limit() -> u32 {
    4
}

const fn default_drift_window_min() -> u32 {
    90
}

const fn default_idle_threshold_min() -> u32 {
    15
}

const fn default_ingest_interval_s() -> u32 {
    30
}

const fn default_sync_interval_s() -> u32 {
    60
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_path", &self.database_path)
            .field("todo_store_path", &self.todo_store_path)
            .field("classifier", &self.classifier)
            .field("wip_limit", &self.wip_limit)
            .field("drift_window_min", &self.drift_window_min)
            .field("idle_threshold_min", &self.idle_threshold_min)
            .field("serve", &self.serve)
            .field("ingest_interval_s", &self.ingest_interval_s)
            .field("sync_interval_s", &self.sync_interval_s)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        let data_dir = dirs_data_path().unwrap_or_else(|| PathBuf::from("."));
        Self {
            database_path: data_dir.join("tt.db"),
            todo_store_path: data_dir,
            classifier: ClassifierConfig::default(),
            wip_limit: default_wip_limit(),
            drift_window_min: default_drift_window_min(),
            idle_threshold_min: default_idle_threshold_min(),
            serve: ServeConfig::default(),
            ingest_interval_s: default_ingest_interval_s(),
            sync_interval_s: default_sync_interval_s(),
        }
    }
}

impl Config {
    /// Loads configuration from default locations.
    #[expect(
        clippy::result_large_err,
        reason = "figment::Error is large but only returned at startup"
    )]
    pub fn load() -> Result<Self, figment::Error> {
        Self::load_from(None)
    }

    /// Loads configuration, optionally from a specific file.
    #[expect(
        clippy::result_large_err,
        reason = "figment::Error is large but only returned at startup"
    )]
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

/// Returns the platform-specific config directory for time-tracker.
fn dirs_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("time-tracker"))
}

/// Returns the platform-specific data directory for time-tracker.
///
/// On Linux: `~/.local/share/time-tracker`
pub fn dirs_data_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join("time-tracker"))
}

/// Returns the platform-specific state directory for time-tracker.
///
/// On Linux: `~/.local/state/time-tracker`
pub fn dirs_state_path() -> Option<PathBuf> {
    dirs::state_dir().map(|p| p.join("time-tracker"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirs_data_path_returns_some() {
        assert!(dirs_data_path().is_some());
    }

    #[test]
    fn test_dirs_state_path_returns_some() {
        assert!(dirs_state_path().is_some());
    }

    #[test]
    fn test_dirs_data_path_ends_with_time_tracker() {
        let path = dirs_data_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "time-tracker");
    }

    #[test]
    fn test_dirs_state_path_ends_with_time_tracker() {
        let path = dirs_state_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "time-tracker");
    }

    #[test]
    fn test_default_config_uses_data_dir_for_db() {
        let config = Config::default();
        let data_dir = dirs_data_path().unwrap();
        assert_eq!(config.database_path, data_dir.join("tt.db"));
    }

    #[test]
    fn default_config_uses_data_dir_for_todo_store_when_available() {
        let config = Config::default();
        let data_dir = dirs_data_path().unwrap();

        assert_eq!(config.todo_store_path, data_dir);
    }

    #[test]
    fn default_config_sets_fifteen_minute_idle_threshold() {
        // Given: the compiled configuration defaults.
        let config = Config::default();

        // When: no TOML or environment override is supplied.

        // Then: timeline folds start after fifteen minutes without human input.
        assert_eq!(config.idle_threshold_min, 15);
    }

    #[test]
    fn load_from_uses_tt_todo_store_path_env_override() {
        const CHILD_MARKER: &str = "TT_TEST_TODO_STORE_PATH_CHILD";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let config = Config::load_from(None).unwrap();
            assert_eq!(
                config.todo_store_path,
                PathBuf::from("/tmp/tt-todo-store-env")
            );
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("config::tests::load_from_uses_tt_todo_store_path_env_override")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("TT_TODO_STORE_PATH", "/tmp/tt-todo-store-env")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn load_from_uses_tt_idle_threshold_min_env_override() {
        const CHILD_MARKER: &str = "TT_TEST_IDLE_THRESHOLD_MIN_CHILD";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let config = Config::load_from(None).unwrap();
            assert_eq!(config.idle_threshold_min, 20);
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("config::tests::load_from_uses_tt_idle_threshold_min_env_override")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("TT_IDLE_THRESHOLD_MIN", "20")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn default_classifier_context_lookup_is_disabled() {
        // Given / When: no `[classifier]` context command is supplied.
        let classifier = ClassifierConfig::default();

        // Then: context cannot alter prompts or offer tools until an operator explicitly
        // configures a read-only lookup command.
        assert!(classifier.context_command.is_none());
    }

    #[test]
    fn classifier_context_command_loads_from_the_classifier_table() {
        // Given: the explicit opt-in command shape operators use.
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[classifier]\ncontext_command = \"/usr/local/bin/lookup --scope primary\"\n",
        )
        .unwrap();

        // When
        let config = Config::load_from(Some(&config_path)).unwrap();

        // Then: the command remains a shell-word command line and is disabled only when
        // its key is absent.
        assert_eq!(
            config.classifier.context_command.as_deref(),
            Some("/usr/local/bin/lookup --scope primary")
        );
    }

    #[test]
    fn classifier_context_instructions_load_and_trim_trailing_whitespace() {
        // Given: vocabulary operators want present in every classification prompt, with
        // harmless TOML indentation and trailing whitespace.
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[classifier]\ncontext_instructions = \"\"\"\nUse shared terms consistently.   \n\"\"\"\n",
        )
        .unwrap();

        // When
        let config = Config::load_from(Some(&config_path)).unwrap();

        // Then: prompt rendering receives the authored text, not layout whitespace.
        assert_eq!(
            config.classifier.context_instructions.as_deref(),
            Some("Use shared terms consistently.")
        );
    }

    #[test]
    fn classifier_context_instructions_over_eight_kib_fail_to_load() {
        // Given: an instruction block one byte larger than the prompt budget.
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[classifier]\ncontext_instructions = \"{}\"\n",
                "x".repeat(8 * 1024 + 1)
            ),
        )
        .unwrap();

        // When
        let error = Config::load_from(Some(&config_path)).expect_err("oversized context must fail");

        // Then: the operator can identify both the key and the byte limit to correct.
        assert!(
            error
                .to_string()
                .contains("classifier.context_instructions")
        );
        assert!(error.to_string().contains("8192"));
    }
}
