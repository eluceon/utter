//! Tauri command handlers. Every command returns `Result<_, String>` (never
//! panics) so a failure surfaces as a rejected promise in the frontend
//! rather than crashing the app.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

use utter_core::TextRefiner;
use utter_inject::PermissionReport;
use utter_refine::{LlmConfig, LlmRefiner};
use utter_store::{HistoryEntry, ModelInfo, Settings};

use crate::events::ModelProgress;
use crate::state::AppState;
use crate::{keyring_password, KEYRING_SERVICE, REFINE_KEY_SERVICE, STT_KEY_SERVICE};

/// Maximum number of history entries returned by [`history_list`].
const HISTORY_LIST_LIMIT: u32 = 500;

/// How often `download_model`'s progress callback is allowed to emit a
/// `model-progress` event, at minimum — throttled to avoid flooding the
/// frontend with an event per 64 KiB chunk on a fast connection.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(500);
/// ... or after this many percentage points of additional progress,
/// whichever comes first, so a slow connection still reports promptly.
const PROGRESS_MIN_PERCENT_STEP: u64 = 1;

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    // A poisoned lock only indicates some other reader/writer panicked while
    // holding it; the settings value itself is still intact, so recover it
    // rather than propagating a panic through a command that can't return
    // an error per its (binding) signature.
    state
        .settings
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<AppState>,
    settings: Settings,
) -> Result<(), String> {
    persist_and_apply(&app, &state, settings)
}

/// Saves `settings` to disk, rebuilds the live dictation runtime from them
/// (hotkey source, STT engine, refiner, injector chain — see
/// `runtime_boot::rebuild`), and updates the in-memory copy.
///
/// The one path every settings change goes through, whether it came from the
/// settings UI (`save_settings`) or the tray's "Refinement" checkbox.
///
/// Holds the write lock across persist + apply + in-memory update so two
/// concurrent callers are fully serialized instead of interleaving their
/// disk write / apply / in-memory steps (which could otherwise leave the
/// on-disk file and in-memory state with different winners). This runs on
/// tauri's threadpool rather than an async/UI thread, and `utter_store::save`
/// is a small TOML write, so holding a `std::sync::RwLock` write guard across
/// it (and the runtime rebuild) is fine.
pub(crate) fn persist_and_apply(
    app: &AppHandle,
    state: &AppState,
    settings: Settings,
) -> Result<(), String> {
    let mut guard = state
        .settings
        .write()
        .map_err(|_| "settings lock poisoned".to_string())?;

    utter_store::save(&utter_store::config_path(), &settings)
        .map_err(|e| format!("failed to save settings: {e}"))?;

    crate::runtime_boot::rebuild(app, state, &settings)?;

    *guard = settings;

    Ok(())
}

/// Cancels the in-flight dictation session, if any (a no-op while idle).
/// Backs the HUD's "click anywhere to cancel" affordance.
#[tauri::command]
pub fn cancel_dictation(state: State<AppState>) -> Result<(), String> {
    let guard = state
        .session_ctl
        .lock()
        .map_err(|_| "session control lock poisoned".to_string())?;
    if let Some(handle) = guard.as_ref() {
        handle.cancel();
    }
    Ok(())
}

#[tauri::command]
pub fn list_devices() -> Vec<String> {
    utter_audio::list_input_devices()
}

#[tauri::command]
pub fn list_models(state: State<AppState>) -> Vec<ModelInfo> {
    state.models.catalog()
}

#[tauri::command]
pub async fn download_model(app: AppHandle, id: String) -> Result<(), String> {
    let models = {
        let state = app.state::<AppState>();
        state.models.clone()
    };

    let progress_app = app.clone();
    let progress_id = id.clone();
    let download_id = id.clone();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut last_emitted_done = 0u64;
        let mut last_emit_at = Instant::now();

        models.download(&download_id, &mut |done, total| {
            let now = Instant::now();
            if should_emit_progress(
                done,
                total,
                last_emitted_done,
                now.duration_since(last_emit_at),
                PROGRESS_MIN_INTERVAL,
                PROGRESS_MIN_PERCENT_STEP,
            ) {
                last_emitted_done = done;
                last_emit_at = now;
                let _ = progress_app.emit(
                    "model-progress",
                    ModelProgress {
                        id: progress_id.clone(),
                        done,
                        total,
                    },
                );
            }
        })
    })
    .await
    .map_err(|e| format!("download task failed to run: {e}"))?;

    result
        .map(|_path| ())
        .map_err(|e| format!("failed to download model '{id}': {e}"))
}

/// Decides whether a `model-progress` event should be emitted now.
///
/// Always emits at the very start (`done == 0`) and at completion
/// (`total > 0 && done >= total`); otherwise throttles to at most once per
/// `min_interval` or once per `min_percent_step` additional percentage
/// points, whichever comes first.
fn should_emit_progress(
    done: u64,
    total: u64,
    last_emitted_done: u64,
    elapsed_since_last: Duration,
    min_interval: Duration,
    min_percent_step: u64,
) -> bool {
    if done == 0 || (total > 0 && done >= total) {
        return true;
    }
    if elapsed_since_last >= min_interval {
        return true;
    }
    // `checked_div` returns `None` when `total == 0` (unknown content
    // length), in which case only the time-based threshold above applies.
    if let (Some(last_percent), Some(current_percent)) = (
        last_emitted_done.saturating_mul(100).checked_div(total),
        done.saturating_mul(100).checked_div(total),
    ) {
        if current_percent >= last_percent + min_percent_step {
            return true;
        }
    }
    false
}

#[tauri::command]
pub fn remove_model(state: State<AppState>, id: String) -> Result<(), String> {
    state
        .models
        .remove(&id)
        .map_err(|e| format!("failed to remove model '{id}': {e}"))
}

#[tauri::command]
pub fn history_list(
    state: State<AppState>,
    query: Option<String>,
) -> Result<Vec<HistoryEntry>, String> {
    let history = state
        .history
        .lock()
        .map_err(|_| "history lock poisoned".to_string())?;
    history
        .list(query.as_deref(), HISTORY_LIST_LIMIT)
        .map_err(|e| format!("failed to list history: {e}"))
}

#[tauri::command]
pub fn history_delete(state: State<AppState>, id: i64) -> Result<(), String> {
    let history = state
        .history
        .lock()
        .map_err(|_| "history lock poisoned".to_string())?;
    history
        .delete(id)
        .map_err(|e| format!("failed to delete history entry {id}: {e}"))
}

#[tauri::command]
pub fn history_clear(state: State<AppState>) -> Result<(), String> {
    let history = state
        .history
        .lock()
        .map_err(|_| "history lock poisoned".to_string())?;
    history
        .clear()
        .map_err(|e| format!("failed to clear history: {e}"))
}

/// Validates that `service` is one of the two api-key identities the app
/// knows about, mapping any other value to a rejecting error string.
fn validate_key_service(service: &str) -> Result<(), String> {
    match service {
        STT_KEY_SERVICE | REFINE_KEY_SERVICE => Ok(()),
        other => Err(format!(
            "unknown api key service '{other}': expected '{STT_KEY_SERVICE}' or '{REFINE_KEY_SERVICE}'"
        )),
    }
}

#[tauri::command]
pub fn set_api_key(service: String, key: String) -> Result<(), String> {
    validate_key_service(&service)?;
    let entry = keyring::Entry::new(KEYRING_SERVICE, &service)
        .map_err(|e| format!("failed to open keyring entry for '{service}': {e}"))?;
    entry
        .set_password(&key)
        .map_err(|e| format!("failed to store api key for '{service}': {e}"))
}

#[tauri::command]
pub fn has_api_key(service: String) -> bool {
    if validate_key_service(&service).is_err() {
        return false;
    }
    keyring_password(&service).is_some()
}

#[tauri::command]
pub fn permissions_report() -> PermissionReport {
    utter_inject::check_permissions()
}

#[tauri::command]
pub fn test_refine(state: State<AppState>, sample: String) -> Result<String, String> {
    let settings = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned".to_string())?
        .clone();

    let api_key = keyring_password(REFINE_KEY_SERVICE);

    let refiner = LlmRefiner::new(
        LlmConfig {
            base_url: settings.refine.base_url,
            api_key,
            model: settings.refine.model,
            timeout: Duration::from_secs(settings.refine.timeout_secs),
        },
        settings.dictionary.terms,
    );

    TextRefiner::refine(&refiner, &sample, settings.refine.tone)
        .map_err(|e| format!("refine failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_always_emits_at_start() {
        assert!(should_emit_progress(
            0,
            1000,
            0,
            Duration::from_secs(0),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
    }

    #[test]
    fn progress_always_emits_at_completion() {
        assert!(should_emit_progress(
            1000,
            1000,
            10,
            Duration::from_millis(1),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
    }

    #[test]
    fn progress_suppressed_within_interval_and_percent_step() {
        // 1 more byte of a 1000-byte download barely moves the percentage,
        // and no time has passed: should not emit.
        assert!(!should_emit_progress(
            11,
            1000,
            10,
            Duration::from_millis(1),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
    }

    #[test]
    fn progress_emits_after_min_interval_elapses() {
        assert!(should_emit_progress(
            11,
            1000,
            10,
            Duration::from_millis(600),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
    }

    #[test]
    fn progress_emits_after_percent_step_reached() {
        // 10 -> 20 out of 1000 total is a 1 percentage-point jump, with no
        // time elapsed: the percent-step threshold alone should trigger it.
        assert!(should_emit_progress(
            20,
            1000,
            10,
            Duration::from_millis(1),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
    }

    #[test]
    fn progress_with_unknown_total_only_uses_time_threshold() {
        // total == 0 means the server didn't send Content-Length: percent
        // math is meaningless, so only the time threshold should apply.
        assert!(!should_emit_progress(
            5_000_000,
            0,
            0,
            Duration::from_millis(1),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
        assert!(should_emit_progress(
            5_000_000,
            0,
            0,
            Duration::from_millis(600),
            PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_PERCENT_STEP
        ));
    }

    #[test]
    fn rejects_unknown_key_service() {
        assert!(validate_key_service("whisper").is_err());
        assert!(validate_key_service("").is_err());
    }

    #[test]
    fn accepts_known_key_services() {
        assert!(validate_key_service("stt").is_ok());
        assert!(validate_key_service("refine").is_ok());
    }

    #[test]
    fn has_api_key_rejects_unknown_service_without_touching_keyring() {
        assert!(!has_api_key("nonsense".to_string()));
    }
}
