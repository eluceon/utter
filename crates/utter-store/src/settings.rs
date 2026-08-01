//! Application settings: schema, defaults, and atomic TOML persistence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use utter_core::{DictationMode, Tone};
use utter_refine::{ReplaceRule, Snippet};

/// The full, on-disk application settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    pub dictation: Dictation,
    pub engine: EngineCfg,
    pub refine: RefineCfg,
    pub dictionary: Dictionary,
    pub snippets: Vec<Snippet>,
    pub history: HistoryCfg,
    pub advanced: Advanced,
}

/// General application preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub language: Option<String>,
    pub theme: Theme,
    pub autostart: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            language: None,
            theme: Theme::System,
            autostart: false,
        }
    }
}

/// UI theme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Dictation hotkey and recording behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dictation {
    pub mode: DictationMode,
    pub hotkey: String,
    pub silence_timeout_secs: Option<u32>,
    pub hud: bool,
}

impl Default for Dictation {
    fn default() -> Self {
        Self {
            mode: DictationMode::PushToTalk,
            hotkey: "ctrl+super".to_string(),
            silence_timeout_secs: None,
            hud: true,
        }
    }
}

/// Speech-to-text engine selection and per-engine configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineCfg {
    pub active: EngineKind,
    /// Catalog id of the whisper model, resolved to an on-disk path through
    /// [`ModelManager::path_for`](crate::ModelManager::path_for) — never a
    /// filesystem path itself.
    pub whisper_model: String,
    /// Catalog id of the sherpa-onnx model, resolved the same way as
    /// [`whisper_model`](Self::whisper_model) — never a filesystem path
    /// itself. Sherpa models install as a directory of several artifacts
    /// (encoder, decoder, joiner, tokens), which makes treating this as a
    /// path an easy mistake to reintroduce.
    pub sherpa_model: Option<String>,
    pub cloud: CloudSttCfg,
}

impl Default for EngineCfg {
    fn default() -> Self {
        Self {
            active: EngineKind::Whisper,
            whisper_model: "small".to_string(),
            sherpa_model: None,
            cloud: CloudSttCfg::default(),
        }
    }
}

/// Which speech-to-text engine is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    #[default]
    Whisper,
    Cloud,
    Sherpa,
}

/// Configuration for an OpenAI-compatible cloud speech-to-text endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudSttCfg {
    pub base_url: String,
    pub model: String,
}

impl Default for CloudSttCfg {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "whisper-1".to_string(),
        }
    }
}

/// LLM-based transcript refinement configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefineCfg {
    pub enabled: bool,
    pub tone: Tone,
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u64,
}

impl Default for RefineCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            tone: Tone::Clean,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "llama3.2".to_string(),
            timeout_secs: 10,
        }
    }
}

/// User dictionary: custom terms and "heard X, write Y" replacement rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Dictionary {
    pub terms: Vec<String>,
    pub rules: Vec<ReplaceRule>,
}

/// Dictation history preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryCfg {
    pub enabled: bool,
}

impl Default for HistoryCfg {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Advanced/expert settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    pub injection: InjectionPreference,
    pub audio_device: Option<String>,
    pub vad_sensitivity: f32,
    pub log_level: String,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            injection: InjectionPreference::Auto,
            audio_device: None,
            vad_sensitivity: 0.5,
            log_level: "info".to_string(),
        }
    }
}

/// Preferred method for injecting refined text into the focused application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectionPreference {
    #[default]
    Auto,
    ClipboardPaste,
    Type,
    ClipboardOnly,
}

/// The default per-user config file path: `<config_dir>/config.toml` under
/// the `dev.utter.utter` application identifier.
pub fn config_path() -> PathBuf {
    ProjectDirs::from("dev", "utter", "utter")
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

/// Load settings from `path`. A missing file yields `Settings::default()`;
/// an unreadable or malformed file is an error.
pub fn load(path: &Path) -> Result<Settings> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

/// Save settings to `path` atomically: serialize to a sibling `.tmp` file,
/// then rename it over `path`, creating parent directories as needed.
pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let toml_str =
        toml::to_string_pretty(settings).context("failed to serialize settings to toml")?;

    let mut tmp_path = path.as_os_str().to_owned();
    tmp_path.push(".tmp");
    let tmp_path = PathBuf::from(tmp_path);

    fs::write(&tmp_path, toml_str)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn config_path(dir: &Path) -> std::path::PathBuf {
        dir.join("config.toml")
    }

    #[test]
    fn default_settings_round_trip_through_save_and_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(dir.path());
        let settings = Settings::default();

        save(&path, &settings).expect("save should succeed");
        let loaded = load(&path).expect("load should succeed");

        assert_eq!(loaded, settings);
    }

    #[test]
    fn loading_file_with_unknown_key_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(dir.path());
        fs::write(
            &path,
            r#"
            unknown_top_level_key = "surprise"

            [general]
            unknown_nested_key = 42
            "#,
        )
        .expect("write fixture");

        let loaded = load(&path).expect("load should tolerate unknown keys");
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn loading_partial_file_fills_defaults_for_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(dir.path());
        fs::write(
            &path,
            r#"
            [dictation]
            hotkey = "ctrl+alt+space"
            "#,
        )
        .expect("write fixture");

        let loaded = load(&path).expect("load should succeed");

        assert_eq!(loaded.dictation.hotkey, "ctrl+alt+space");
        assert_eq!(loaded.dictation.mode, DictationMode::PushToTalk);
        assert_eq!(loaded.general, General::default());
        assert_eq!(loaded.engine, EngineCfg::default());
    }

    #[test]
    fn atomic_save_leaves_no_tmp_file_on_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(dir.path());
        let tmp_path = path.with_extension("toml.tmp");

        save(&path, &Settings::default()).expect("save should succeed");

        assert!(path.exists());
        assert!(!tmp_path.exists());
    }

    #[test]
    fn missing_file_loads_as_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.toml");

        let loaded = load(&path).expect("missing file should load as defaults");
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(dir.path());
        fs::write(&path, "this is not valid = = toml").expect("write fixture");

        let result = load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn defaults_match_documented_values() {
        let settings = Settings::default();

        assert_eq!(settings.dictation.hotkey, "ctrl+super");
        assert_eq!(settings.dictation.silence_timeout_secs, None);
        assert!(settings.dictation.hud);
        assert_eq!(settings.dictation.mode, DictationMode::PushToTalk);

        assert_eq!(settings.refine.tone, Tone::Clean);
        assert_eq!(settings.refine.timeout_secs, 10);
        assert!(!settings.refine.enabled);
        assert_eq!(settings.refine.base_url, "http://localhost:11434/v1");
        assert_eq!(settings.refine.model, "llama3.2");

        assert_eq!(settings.engine.whisper_model, "small");
        assert_eq!(settings.engine.active, EngineKind::Whisper);
        assert_eq!(settings.engine.cloud.base_url, "https://api.openai.com/v1");
        assert_eq!(settings.engine.cloud.model, "whisper-1");

        assert_eq!(settings.general.theme, Theme::System);
        assert_eq!(settings.general.language, None);
        assert!(!settings.general.autostart);

        assert!(settings.history.enabled);

        assert_eq!(settings.advanced.injection, InjectionPreference::Auto);
        assert_eq!(settings.advanced.audio_device, None);
        assert_eq!(settings.advanced.vad_sensitivity, 0.5);
        assert_eq!(settings.advanced.log_level, "info");
    }
}
