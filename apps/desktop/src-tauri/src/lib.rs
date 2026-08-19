//! Tauri application entry point: builds the app, wires up managed state,
//! command handlers, the system tray, and the live dictation runtime, then
//! runs the event loop.

mod commands;
/// Event payload shapes shared with the frontend.
pub mod events;
/// Maps each language profile's hotkey binding to its own lazily-built
/// engines; wired into the runtime worker in `runtime`. Public so
/// integration tests can build a `ProfileRegistry` over a fake
/// `ProfileLoader`, the same seam `runtime.rs` itself depends on.
pub mod profiles;
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
use tracing::level_filters::LevelFilter;

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

/// Turns the `advanced.log_level` setting into a maximum level. Anything
/// unrecognised (an old or hand-edited config) logs at `info` rather than
/// falling silent — the setting is a dial, and a bad value should not be
/// able to turn diagnostics off altogether.
fn max_level(setting: &str) -> LevelFilter {
    match setting.to_ascii_lowercase().as_str() {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        "off" => LevelFilter::OFF,
        _ => LevelFilter::INFO,
    }
}

/// Installs the `tracing` subscriber every crate in the workspace logs
/// through, writing to stderr. Without it `tracing`'s events go nowhere at
/// all: not to a file, not to a console, not anywhere the user or a bug
/// report can reach — which is what `advanced.log_level` has been promising
/// to control.
///
/// Failure here means a subscriber is already installed (nothing else can
/// fail), so there is nothing to report and nowhere to report it.
fn init_tracing(setting: &str) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(max_level(setting))
        .with_writer(std::io::stderr)
        .try_init();
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
            // As early as settings are available, and before `boot` — every
            // degradation it reports is logged on the way past.
            match state.settings.read() {
                Ok(settings) => init_tracing(&settings.advanced.log_level),
                Err(_) => init_tracing("info"),
            }
            app.manage(state);

            let handle = app.handle().clone();

            #[cfg(target_os = "linux")]
            sink::configure_hud_window(&handle);

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
            commands::take_pending_notices,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_level_the_settings_ui_offers() {
        assert_eq!(max_level("trace"), LevelFilter::TRACE);
        assert_eq!(max_level("debug"), LevelFilter::DEBUG);
        assert_eq!(max_level("info"), LevelFilter::INFO);
        assert_eq!(max_level("warn"), LevelFilter::WARN);
        assert_eq!(max_level("error"), LevelFilter::ERROR);
        assert_eq!(max_level("off"), LevelFilter::OFF);
    }

    #[test]
    fn an_unreadable_level_still_logs_rather_than_falling_silent() {
        assert_eq!(max_level("verbose"), LevelFilter::INFO);
        assert_eq!(max_level(""), LevelFilter::INFO);
    }

    #[test]
    fn level_names_are_not_case_sensitive() {
        assert_eq!(max_level("DEBUG"), LevelFilter::DEBUG);
    }

    /// Every command the settings UI invokes is registered in the
    /// `generate_handler!` list above.
    ///
    /// Nothing else checks this. A command can exist, be annotated, compile,
    /// and have a matching typed wrapper in `api.ts`, and still reject at
    /// runtime with "command not found" if its name never reached the list —
    /// a one-line omission with no compile-time symptom on either side, since
    /// the two halves are in different languages and neither toolchain reads
    /// the other. `take_pending_notices` is what motivated writing this down:
    /// it is the only channel a startup notice reaches a window through, it
    /// is called exactly once (on mount), and its rejection is deliberately
    /// swallowed there — so an unregistered command would restore precisely
    /// the silent dead end it was added to close, with every test green.
    #[test]
    fn every_command_the_frontend_invokes_is_registered() {
        const LIB_RS: &str = include_str!("lib.rs");
        const API_TS: &str = include_str!("../../ui/src/lib/api.ts");

        // The first `generate_handler!` in this file is the real one; this
        // test's own mention of it is below.
        let registered: Vec<&str> = LIB_RS
            .split_once("generate_handler![")
            .and_then(|(_, rest)| rest.split_once(']'))
            .expect("lib.rs must have a generate_handler! list")
            .0
            .split(',')
            .filter_map(|entry| entry.trim().strip_prefix("commands::"))
            .collect();

        let invoked: Vec<&str> = API_TS
            .match_indices("invoke('")
            .filter_map(|(at, marker)| API_TS[at + marker.len()..].split_once('\''))
            .map(|(name, _)| name)
            .collect();

        // Both halves are scraped from source, so an extraction that quietly
        // matched nothing would make every assertion below vacuously true --
        // the failure mode this whole test exists to catch, one level up.
        assert!(
            registered.len() >= 10,
            "scraped only {registered:?} from the handler list; the extraction is broken, not \
             the app"
        );
        assert!(
            invoked.len() >= 10,
            "scraped only {invoked:?} from api.ts; the extraction is broken, not the app"
        );

        for name in &invoked {
            assert!(
                registered.contains(name),
                "api.ts invokes \"{name}\", which is not in generate_handler!: every call to it \
                 rejects at runtime. Registered: {registered:?}"
            );
        }
    }
}
