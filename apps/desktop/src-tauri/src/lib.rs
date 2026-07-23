//! Tauri application entry point: builds the app, wires up managed state,
//! command handlers, the system tray, and the live dictation runtime, then
//! runs the event loop.

mod commands;
/// Event payload shapes shared with the frontend.
pub mod events;
/// The dictation runtime orchestrator (worker thread, state machine wiring).
/// Public so integration tests can drive it directly.
pub mod runtime;
/// Builds [`runtime::RuntimeDeps`] from persisted settings and owns the
/// dictation runtime's boot/reload/shutdown lifecycle.
mod runtime_boot;
/// [`runtime::EventSink`] implementation that emits Tauri events.
mod sink;
mod state;
/// System tray icon and menu.
mod tray;

use tauri::{Manager, RunEvent, WindowEvent};

use state::AppState;

/// The service name under which all Utter secrets are stored in the OS
/// keyring; the per-secret identity is the keyring *username*, one of
/// [`STT_KEY_SERVICE`] / [`REFINE_KEY_SERVICE`].
pub(crate) const KEYRING_SERVICE: &str = "utter";
pub(crate) const STT_KEY_SERVICE: &str = "stt";
pub(crate) const REFINE_KEY_SERVICE: &str = "refine";

/// Looks up a secret from the OS keyring under [`KEYRING_SERVICE`]. `None`
/// (rather than an error) whenever the entry doesn't exist or the keyring
/// backend itself is unavailable — every caller treats a missing key as
/// "not configured yet", not a hard failure.
pub(crate) fn keyring_password(user: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, user)
        .and_then(|entry| entry.get_password())
        .ok()
}

/// Builds and runs the Tauri application.
///
/// Returns `Err` instead of panicking on failure, so `main` can report the
/// error and exit with a non-zero status rather than unwinding through a
/// panic.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), String> {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let state = AppState::new().map_err(|e| e.to_string())?;
            app.manage(state);

            let handle = app.handle().clone();

            // Boot degrades, it doesn't fail: a missing model, no hotkey
            // permissions, or an unconfigured refiner all still leave a
            // running runtime with a notice queued (see
            // `runtime_boot::boot`'s doc comment). Only log-and-continue on
            // a genuinely unexpected failure here, rather than aborting
            // startup — the settings/tray/history UI is still useful with
            // no live session, and the next `save_settings` can recover it.
            if let Err(e) = runtime_boot::boot(&handle) {
                tracing::error!("failed to boot the dictation runtime: {e}");
            }

            tray::build(&handle)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing either window hides it to the tray rather than
            // quitting the app; the only way to fully exit is the tray's
            // "Quit" item, which shuts the runtime down explicitly. The HUD
            // has no decorations/close button so it is never *user*-closable
            // this way, but guarding it too is cheap and keeps both windows
            // symmetric against any programmatic or platform-triggered close
            // request.
            let label = window.label();
            if label == "main" || label == "hud" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Err(e) = window.hide() {
                        tracing::warn!("failed to hide {label} window on close: {e}");
                    }
                }
            }
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
            commands::cancel_dictation,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| e.to_string())?;

    // Run (rather than the `.run(context)` shorthand) so `ExitRequested` can
    // shut the dictation runtime's worker thread down explicitly before the
    // process exits — some platforms' event loops end the process without
    // unwinding the stack, which would otherwise skip `RuntimeHandle`'s
    // `Drop` safety net.
    app.run(|app_handle, event| {
        if let RunEvent::ExitRequested { .. } = event {
            let state = app_handle.state::<AppState>();
            runtime_boot::shutdown(&state);
        }
    });

    Ok(())
}
