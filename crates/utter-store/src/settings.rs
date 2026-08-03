//! Application settings: schema, defaults, and atomic TOML persistence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::de::IntoDeserializer;
use serde::{Deserialize, Serialize};

use utter_core::{DictationMode, Tone};
use utter_refine::{ReplaceRule, Snippet};

use crate::profile::{LanguageProfile, RefinePolicy};

/// The full, on-disk application settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// One entry per language the user dictates in, each binding a hotkey to
    /// an engine, a model and a refinement policy.
    pub profiles: Vec<LanguageProfile>,
}

impl Default for Settings {
    /// A fresh install gets one profile on the local sherpa-onnx engine.
    /// whisper.cpp remains selectable but is no longer what a new user
    /// starts with: the sherpa models emit punctuation and casing directly,
    /// which is what makes refinement optional rather than expected.
    fn default() -> Self {
        Self {
            general: General::default(),
            dictation: Dictation::default(),
            engine: EngineCfg::default(),
            refine: RefineCfg::default(),
            dictionary: Dictionary::default(),
            snippets: Vec::new(),
            history: HistoryCfg::default(),
            advanced: Advanced::default(),
            profiles: vec![LanguageProfile {
                id: "default".to_string(),
                hotkey: Dictation::default().hotkey,
                language: "en".to_string(),
                engine: EngineCfg::sherpa("parakeet-tdt-110m-en"),
                draft: None,
                refine: RefinePolicy::default(),
            }],
        }
    }
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
    #[serde(deserialize_with = "deserialize_active_engine")]
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

impl EngineCfg {
    /// A configuration selecting the sherpa-onnx engine with `model`, the
    /// catalog id of one of its multi-artifact models.
    pub fn sherpa(model: &str) -> Self {
        Self {
            active: EngineKind::Sherpa,
            sherpa_model: Some(model.to_string()),
            ..Self::default()
        }
    }
}

/// Whether a transcript from a profile carrying `policy` should be refined.
///
/// Refinement is gated twice on purpose. [`RefineCfg::enabled`] is a master
/// switch the tray toggles, meaning "don't touch my text right now" whichever
/// language is about to be spoken; [`RefinePolicy::enabled`] is the profile's
/// own standing preference, which differs by language because some engines
/// already emit punctuation and casing on their own. Refinement runs only
/// when both agree.
pub fn refinement_is_on(global: &RefineCfg, policy: &RefinePolicy) -> bool {
    global.enabled && policy.enabled
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

/// Deserializes `engine.active`, tolerating a name this build does not
/// recognise (e.g. a v0.1 config's `active = "vosk"`, left behind once the
/// Vosk engine was removed). The derived `Deserialize` for [`EngineKind`]
/// would fail the *whole* TOML document on an unrecognised variant — the
/// unknown-key tolerance `#[serde(default)]` gives every other field does
/// not extend to enum values. Falling back to [`EngineKind::default`] here
/// keeps a stale engine name from turning into a startup crash; the value
/// is logged so the fallback is not silent.
///
/// The fallback re-uses the derived `Deserialize` for [`EngineKind`] instead
/// of hand-writing the string-to-variant mapping a second time: a mapping
/// duplicated here would silently drift out of sync with the derive as soon
/// as a variant is added (it would compile, and just deserialize the new
/// name back to the default), whereas delegating to the derive means a new
/// variant is picked up automatically.
fn deserialize_active_engine<'de, D>(deserializer: D) -> Result<EngineKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(
        EngineKind::deserialize(raw.as_str().into_deserializer()).unwrap_or_else(
            |_: serde::de::value::Error| {
                let fallback = EngineKind::default();
                tracing::warn!(
                    "unrecognized engine.active value \"{raw}\" in settings; falling back to \"{}\"",
                    engine_kind_as_toml(fallback)
                );
                fallback
            },
        ),
    )
}

/// Renders `kind` the way it appears in a TOML config file (its
/// `#[serde(rename_all = "snake_case")]` spelling), for diagnostics aimed at
/// someone reading their `config.toml` — not `{kind:?}`'s Rust spelling,
/// which a user grepping their logs for `active = "whisper"` will not find.
fn engine_kind_as_toml(kind: EngineKind) -> String {
    toml::Value::try_from(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{kind:?}"))
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

    #[test]
    fn profiles_round_trip_through_toml() {
        let settings = Settings {
            profiles: vec![LanguageProfile {
                id: "ru".into(),
                hotkey: "ctrl+super".into(),
                language: "ru".into(),
                engine: EngineCfg::sherpa("gigaam-v3-e2e-rnnt"),
                draft: None,
                refine: RefinePolicy {
                    enabled: false,
                    tone: Tone::Clean,
                },
            }],
            ..Settings::default()
        };

        let text = toml::to_string(&settings).expect("serialize");
        let parsed: Settings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed, settings);
    }

    #[test]
    fn a_fresh_install_defaults_to_the_sherpa_engines() {
        let settings = Settings::default();
        assert_eq!(
            settings.profiles.len(),
            1,
            "one profile until the user adds more"
        );

        let profile = &settings.profiles[0];
        assert_eq!(profile.engine.active, EngineKind::Sherpa);
        assert!(
            !profile.refine.enabled,
            "the default engine already emits punctuation, so refinement starts off"
        );
    }

    #[test]
    fn refinement_needs_both_the_master_switch_and_the_profile_policy() {
        let on = RefineCfg {
            enabled: true,
            ..RefineCfg::default()
        };
        let off = RefineCfg::default();
        let wants = RefinePolicy {
            enabled: true,
            ..RefinePolicy::default()
        };
        let declines = RefinePolicy::default();

        assert!(refinement_is_on(&on, &wants));
        assert!(
            !refinement_is_on(&off, &wants),
            "the tray master switch wins"
        );
        assert!(!refinement_is_on(&on, &declines), "the profile opted out");
    }
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
    fn an_unknown_engine_name_falls_back_without_losing_other_settings() {
        // A v0.1 config naming an engine this build no longer has. The whole
        // document must still parse: the user's hotkey and dictionary are not
        // collateral damage for one stale enum value.
        let toml = r#"
[dictation]
hotkey = "ctrl+alt+super"

[engine]
active = "vosk"
vosk_model = "vosk-model-small-ru-0.22"

[dictionary]
terms = ["PostgreSQL"]
"#;

        let settings: Settings =
            toml::from_str(toml).expect("an unknown engine must not fail the file");

        assert_eq!(settings.engine.active, EngineKind::default());
        assert_eq!(settings.dictation.hotkey, "ctrl+alt+super");
        assert_eq!(settings.dictionary.terms, vec!["PostgreSQL".to_string()]);
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
