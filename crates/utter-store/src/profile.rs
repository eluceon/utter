//! Language profiles: the unit of configuration a hotkey binding resolves to.
//!
//! Each profile binds one chord to one language and everything that follows
//! from it — which engine transcribes, which model it loads, and whether the
//! transcript is refined afterwards. Pressing a profile's hotkey selects the
//! whole set at once, so the user never picks a language and an engine
//! separately.

use serde::{Deserialize, Serialize};

use utter_core::Tone;

use crate::settings::EngineCfg;

/// One language and everything dictating in it implies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageProfile {
    /// Stable identifier, used in config, history entries and the HUD. Chosen
    /// by the user rather than generated, so it survives reordering.
    pub id: String,
    /// The chord that selects this profile, in the same syntax as any other
    /// hotkey (`ctrl+alt+super`).
    pub hotkey: String,
    /// BCP-47-style language tag passed to the engine as a transcription hint.
    pub language: String,
    /// Which engine produces the text that gets injected.
    pub engine: EngineCfg,
    /// Which engine drives the live preview, if any. Nothing reads this yet;
    /// the preview arrives in a later release.
    pub draft: Option<DraftCfg>,
    /// Whether and how this profile's transcripts are refined.
    pub refine: RefinePolicy,
}

impl Default for LanguageProfile {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            hotkey: "ctrl+super".to_string(),
            language: "en".to_string(),
            engine: EngineCfg::default(),
            draft: None,
            refine: RefinePolicy::default(),
        }
    }
}

/// The streaming model backing a profile's live preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DraftCfg {
    /// Catalog id of the streaming model, resolved through
    /// [`ModelManager::path_for`](crate::ModelManager::path_for) like every
    /// other model id — never a filesystem path.
    pub model: String,
}

/// A profile's refinement policy.
///
/// `enabled` here is only half the answer: refinement runs when **both** this
/// flag and the global [`RefineCfg::enabled`](crate::settings::RefineCfg)
/// master switch are set. See
/// [`refinement_is_on`](crate::settings::refinement_is_on), which is the one
/// place that combination is computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefinePolicy {
    pub enabled: bool,
    pub tone: Tone,
}

impl Default for RefinePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            tone: Tone::Clean,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_profile_starts_with_refinement_off() {
        // The default engine emits punctuation and casing itself, so there is
        // nothing for a refiner to fix on a fresh install.
        assert!(!RefinePolicy::default().enabled);
    }

    #[test]
    fn a_draft_config_round_trips_through_toml() {
        let draft = DraftCfg {
            model: "zipformer-ru-small".to_string(),
        };
        let text = toml::to_string(&draft).expect("serialize");
        let parsed: DraftCfg = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed, draft);
    }
}
