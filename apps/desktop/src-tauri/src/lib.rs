//! Tauri application entry point: builds the app, wires up managed state
//! and command handlers, and runs the event loop.
//!
//! This crate is the app *shell* only (Task 16): it owns settings/history
//! persistence and the model catalog through [`state::AppState`], plus the
//! commands the frontend calls. There is no dictation session runtime here
//! yet — the tray menu, HUD, and hotkey-driven session orchestration land in
//! later tasks.

mod commands;
/// Event payload shapes shared with the frontend. Public: `DictationState`
/// and `Notice` aren't constructed anywhere yet (only `model-progress` is
/// emitted by this task), but their shape is part of this crate's contract
/// with the frontend, defined once for the runtime task that will emit them.
pub mod events;
/// The dictation runtime orchestrator (worker thread, state machine wiring).
/// Public so integration tests can drive it directly; nothing in `run()`
/// constructs or starts it yet — booting it into the app is a later task.
pub mod runtime;
mod state;

use tauri::Manager;

use state::AppState;

/// Builds and runs the Tauri application.
///
/// Returns `Err` instead of panicking on failure, so `main` can report the
/// error and exit with a non-zero status rather than unwinding through a
/// panic.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), String> {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let state = AppState::new().map_err(|e| e.to_string())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::list_devices,
            commands::list_models,
            commands::download_model,
            commands::remove_model,
            commands::history_list,
            commands::history_delete,
            commands::history_clear,
            commands::set_api_key,
            commands::has_api_key,
            commands::permissions_report,
            commands::test_refine,
        ])
        .run(tauri::generate_context!())
        .map_err(|e| e.to_string())
}
