//! Migration of v0.1 configuration files into v0.2's language-profile schema.
//!
//! v0.1 had one implicit profile: a single hotkey, engine and refinement
//! policy shared by everything the user dictated. v0.2 replaced that with
//! [`LanguageProfile`], letting several hotkey-bound language setups coexist.
//! [`migrate_v1`] reads a v0.1 document and produces the one profile it
//! implies, carrying across every field a v0.1 config could hold — not just
//! the ones that changed shape.

use serde::de::IntoDeserializer;
use serde::{Deserialize, Serialize};

use crate::error::MigrateError;
use crate::profile::{LanguageProfile, RefinePolicy};
use crate::settings::{
    Advanced, CloudSttCfg, Dictation, Dictionary, EngineCfg, EngineKind, General, HistoryCfg,
    RefineCfg, Settings,
};
use utter_refine::Snippet;

/// The catalog id a v0.1 Russian vosk model (`vosk-model-small-ru-*`) is
/// migrated to.
const SHERPA_MODEL_RU: &str = "gigaam-v3-e2e-rnnt";

/// The catalog id a v0.1 English vosk model (`vosk-model-small-en-*`), or a
/// vosk model whose language cannot be determined, is migrated to.
const SHERPA_MODEL_EN: &str = "parakeet-tdt-110m-en";

/// Migrates a v0.1 config document into v0.2's [`Settings`], folding its
/// single implicit hotkey/engine/refine setup into one [`LanguageProfile`].
///
/// Every top-level section other than `engine` is unchanged between v0.1 and
/// v0.2, so it carries across verbatim. Vosk was removed from this build
/// (replaced by sherpa-onnx); a v0.1 config naming it is routed to the
/// sherpa-onnx model for the same language instead of failing or pointing at
/// a model id that no longer exists — `vosk-model-small-ru-*` becomes
/// `SHERPA_MODEL_RU`, `-en-*` (or anything else) becomes
/// `SHERPA_MODEL_EN`. Both are more accurate than Vosk ever was; what does
/// not carry over is live partial results, which nothing in this phase
/// provides for any engine regardless.
///
/// Returns [`MigrateError::AlreadyMigrated`] if `raw` already has a
/// `[[profiles]]` table — see `predates_profiles` for why that check reads
/// the raw document rather than a parsed [`Settings`].
pub fn migrate_v1(raw: &str) -> Result<Settings, MigrateError> {
    if !predates_profiles(raw)? {
        return Err(MigrateError::AlreadyMigrated);
    }

    let v1: V1Settings = toml::from_str(raw)?;
    let general_language = v1.general.language.clone();
    let (engine, language) = migrate_engine(&v1.engine, general_language.as_deref());
    let hotkey = v1.dictation.hotkey.clone();

    let profile = LanguageProfile {
        id: "default".to_string(),
        hotkey,
        language,
        engine: engine.clone(),
        draft: None,
        refine: RefinePolicy {
            enabled: v1.refine.enabled,
            tone: v1.refine.tone,
        },
    };

    Ok(Settings {
        general: v1.general,
        dictation: v1.dictation,
        engine,
        refine: v1.refine,
        dictionary: v1.dictionary,
        snippets: v1.snippets,
        history: v1.history,
        advanced: v1.advanced,
        profiles: vec![profile],
    })
}

/// True when `raw` is a document that predates language profiles: valid TOML
/// with no top-level `[[profiles]]` array-of-tables.
///
/// This inspects the raw [`toml::Value`], never a parsed [`Settings`]:
/// `Settings` derives `#[serde(default)]` and `Settings::default()` seeds one
/// profile, so a document with no `profiles` key and a document holding
/// exactly that one synthetic default profile deserialize to the *identical*
/// `Settings` value. Asking the parsed struct whether migration is needed
/// (e.g. `settings.profiles.is_empty()`) can therefore never tell a real
/// v0.1 config apart from a fresh v0.2 default — every real upgrade would
/// silently keep its hardcoded default profile instead of the user's actual
/// hotkey, model and refinement settings. Only the raw document still carries
/// the distinction, so that is what this checks.
fn predates_profiles(raw: &str) -> Result<bool, MigrateError> {
    let value: toml::Value = toml::from_str(raw)?;
    let has_profiles_table = matches!(value.get("profiles"), Some(toml::Value::Array(_)));
    Ok(!has_profiles_table)
}

/// Resolves a v0.1 engine configuration into its v0.2 equivalent, plus the
/// language tag the resulting profile should carry.
///
/// Whisper and cloud pass through unchanged: neither engine was removed. A
/// vosk configuration has no v0.2 equivalent, so it is rerouted to the
/// sherpa-onnx model for the vosk model's own language, inferred from its
/// catalog naming (`vosk-model-small-<lang>-*`) — a stronger signal of what
/// the user actually dictated in than `general.language`, which the fallback
/// engine branch below uses only because it has nothing better.
fn migrate_engine(v1: &V1EngineCfg, general_language: Option<&str>) -> (EngineCfg, String) {
    let fallback_language = general_language.unwrap_or("en").to_string();

    if v1.active != "vosk" {
        let engine = EngineCfg {
            active: parse_known_engine(&v1.active),
            whisper_model: v1.whisper_model.clone(),
            sherpa_model: None,
            cloud: v1.cloud.clone(),
        };
        return (engine, fallback_language);
    }

    let vosk_model = v1.vosk_model.as_deref().unwrap_or("");
    let language = infer_vosk_language(vosk_model).unwrap_or_else(|| {
        tracing::warn!(
            "vosk_model \"{vosk_model}\" in a v0.1 config does not match a known language; \
             defaulting the migrated profile to english"
        );
        "en"
    });

    // Built by hand rather than `EngineCfg::sherpa(...)`: that constructor's
    // `..Self::default()` would reset `whisper_model` and `cloud` to their
    // defaults. `cloud` configures a third engine the user can switch to
    // independently of vosk/sherpa, so it survives regardless of which engine
    // was active; `whisper_model` is a catalog id the user may have chosen
    // and downloaded, so it is kept too in case they switch back to whisper
    // later. Only `vosk_model` has no v0.2 field to land in.
    let engine = EngineCfg {
        active: EngineKind::Sherpa,
        whisper_model: v1.whisper_model.clone(),
        sherpa_model: Some(sherpa_model_for_language(language).to_string()),
        cloud: v1.cloud.clone(),
    };
    (engine, language.to_string())
}

/// Parses a v0.1 `engine.active` string into the [`EngineKind`] this build
/// still runs, falling back to the default (and logging) on anything it does
/// not recognize other than `"vosk"`, which [`migrate_engine`] handles
/// separately. Delegates to [`EngineKind`]'s derived `Deserialize` rather
/// than hand-mapping each variant name a second time, so a new variant is
/// picked up automatically instead of silently falling back until this
/// function is remembered and updated too.
fn parse_known_engine(name: &str) -> EngineKind {
    EngineKind::deserialize(name.into_deserializer()).unwrap_or_else(
        |_: serde::de::value::Error| {
            tracing::warn!(
                "engine.active \"{name}\" in a v0.1 config is not a recognized engine; \
                 falling back to the default"
            );
            EngineKind::default()
        },
    )
}

/// Infers a BCP-47 language tag from a v0.1 vosk model's catalog id, using
/// the `vosk-model-small-<lang>-<version>` naming every model this project
/// ever shipped a preset for followed.
fn infer_vosk_language(vosk_model: &str) -> Option<&'static str> {
    if vosk_model.contains("-ru-") || vosk_model.ends_with("-ru") {
        Some("ru")
    } else if vosk_model.contains("-en-") || vosk_model.ends_with("-en") {
        Some("en")
    } else {
        None
    }
}

/// The sherpa-onnx catalog id to migrate a vosk user of `language` onto.
fn sherpa_model_for_language(language: &str) -> &'static str {
    match language {
        "ru" => SHERPA_MODEL_RU,
        _ => SHERPA_MODEL_EN,
    }
}

/// The subset of a v0.1 document this migration reads.
///
/// Every field other than `engine` kept the same shape from v0.1 to v0.2, so
/// this borrows those types directly from [`crate::settings`] and
/// [`utter_refine`]. Only `engine` differs: v0.1 named its local-model field
/// `vosk_model` where v0.2 has `sherpa_model`, and v0.1's `active` could name
/// an engine (`"vosk"`) this build's [`EngineKind`] no longer defines —
/// [`V1EngineCfg::active`] is read as a plain string for exactly that reason.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct V1Settings {
    general: General,
    dictation: Dictation,
    engine: V1EngineCfg,
    refine: RefineCfg,
    dictionary: Dictionary,
    snippets: Vec<Snippet>,
    history: HistoryCfg,
    advanced: Advanced,
}

/// A v0.1 `[engine]` table: the one section whose shape changed before v0.2.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
struct V1EngineCfg {
    active: String,
    whisper_model: String,
    vosk_model: Option<String>,
    cloud: CloudSttCfg,
}

impl Default for V1EngineCfg {
    fn default() -> Self {
        Self {
            active: "whisper".to_string(),
            whisper_model: "small".to_string(),
            vosk_model: None,
            cloud: CloudSttCfg::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_v1_config_becomes_one_profile() {
        let v1 = include_str!("../tests/golden/v1_whisper.toml");
        let migrated = migrate_v1(v1).expect("migration must succeed");

        assert_eq!(migrated.profiles.len(), 1);
        let profile = &migrated.profiles[0];
        assert_eq!(
            profile.hotkey, "ctrl+super",
            "the user's hotkey must survive"
        );
        assert_eq!(profile.engine.whisper_model, "large-v3-turbo-q5_0");
        assert!(profile.refine.enabled, "the refine policy must survive");
    }

    #[test]
    fn a_vosk_user_is_moved_to_the_sherpa_model_for_their_language() {
        let v1 = include_str!("../tests/golden/v1_vosk.toml");
        let migrated = migrate_v1(v1).expect("migration must succeed");

        // Vosk is gone. Route by the language of the vosk model they had:
        // `vosk-model-small-ru-*` -> Russian sherpa model.
        assert_eq!(
            migrated.profiles[0].engine.sherpa_model.as_deref(),
            Some("gigaam-v3-e2e-rnnt")
        );
        assert_eq!(migrated.profiles[0].language, "ru");
    }

    #[test]
    fn a_vosk_users_cloud_and_whisper_model_settings_survive_migration() {
        // Neither `[engine.cloud]` nor `whisper_model` is specific to vosk:
        // both configure engines the user can still switch to independently
        // of whichever engine was active in v0.1, so a vosk migration must
        // not reset them to `CloudSttCfg::default()` /
        // `EngineCfg::default().whisper_model` the way building the migrated
        // engine from `EngineCfg::sherpa(...)`'s `..Self::default()` would.
        // `v1_vosk.toml` deliberately sets both away from their defaults, so
        // this test cannot pass by accident the way it would if the fixture
        // happened to already hold the default values.
        let v1 = include_str!("../tests/golden/v1_vosk.toml");
        let migrated = migrate_v1(v1).expect("migration must succeed");

        let engine = &migrated.profiles[0].engine;
        assert_eq!(engine.whisper_model, "large-v3-turbo-q5_0");
        assert_eq!(engine.cloud.base_url, "https://vosk-user.example.com/v1");
        assert_eq!(engine.cloud.model, "whisper-1-custom");
        assert_ne!(engine.cloud, CloudSttCfg::default());
    }

    #[test]
    fn migrating_v1_whisper_produces_the_full_documented_v2_settings() {
        // Cross-checks every field, not only the ones the two tests above
        // assert on: general, dictation, dictionary, snippets, history and
        // advanced must all survive, and the top-level `engine`/`refine` (what
        // the runtime still reads today) must mirror the new profile.
        let v1 = include_str!("../tests/golden/v1_whisper.toml");
        let v2 = include_str!("../tests/golden/v2_whisper.toml");

        let migrated = migrate_v1(v1).expect("migration must succeed");
        let expected: Settings = toml::from_str(v2).expect("golden v2 file must parse");

        assert_eq!(migrated, expected);
    }

    #[test]
    fn migrating_v1_vosk_produces_the_full_documented_v2_settings() {
        let v1 = include_str!("../tests/golden/v1_vosk.toml");
        let v2 = include_str!("../tests/golden/v2_vosk.toml");

        let migrated = migrate_v1(v1).expect("migration must succeed");
        let expected: Settings = toml::from_str(v2).expect("golden v2 file must parse");

        assert_eq!(migrated, expected);
    }

    #[test]
    fn an_english_vosk_model_is_recognized_too() {
        let migrated = migrate_v1(
            r#"
            [engine]
            active = "vosk"
            vosk_model = "vosk-model-small-en-us-0.15"
            "#,
        )
        .expect("migration must succeed");

        assert_eq!(
            migrated.profiles[0].engine.sherpa_model.as_deref(),
            Some("parakeet-tdt-110m-en")
        );
        assert_eq!(migrated.profiles[0].language, "en");
    }

    #[test]
    fn an_unrecognized_vosk_model_name_falls_back_to_english_rather_than_failing() {
        let migrated = migrate_v1(
            r#"
            [engine]
            active = "vosk"
            vosk_model = "some-custom-build"
            "#,
        )
        .expect("an unrecognized vosk model name must not fail the migration");

        assert_eq!(
            migrated.profiles[0].engine.sherpa_model.as_deref(),
            Some("parakeet-tdt-110m-en")
        );
    }

    #[test]
    fn a_document_with_no_profiles_table_is_recognised_as_needing_migration() {
        assert!(
            predates_profiles("").expect("an empty document is valid toml"),
            "a document with no [[profiles]] table at all must be recognised as v0.1"
        );
    }

    #[test]
    fn a_document_holding_exactly_the_default_profile_is_not_mistaken_for_a_v1_document() {
        // Pin the trap this migration exists to avoid: `Settings::default()`
        // now seeds one profile and `Settings` derives `#[serde(default)]`,
        // so a missing `[[profiles]]` table and a document holding exactly
        // that one synthetic profile deserialize to the identical `Settings`
        // value. Detection must work on the raw document, or this case would
        // be indistinguishable from a genuine v0.1 config with no profiles.
        let v2 = toml::to_string(&Settings::default()).expect("serialize");

        assert!(
            !predates_profiles(&v2).expect("valid toml"),
            "a document with a real [[profiles]] table must not be re-migrated"
        );
    }

    #[test]
    fn migrating_an_already_migrated_document_is_rejected() {
        let v2 = toml::to_string(&Settings::default()).expect("serialize");

        let err = migrate_v1(&v2).expect_err("a document with profiles must not be re-migrated");

        assert!(matches!(err, MigrateError::AlreadyMigrated));
    }

    #[test]
    fn invalid_toml_is_a_parse_error() {
        let err = migrate_v1("this is not = = valid toml").expect_err("must not succeed");

        assert!(matches!(err, MigrateError::Parse(_)));
    }
}
