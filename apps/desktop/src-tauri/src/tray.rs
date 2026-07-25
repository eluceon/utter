//! System tray icon and menu: toggle dictation, flip refinement on/off,
//! open the settings window, and quit (shutting the dictation runtime down
//! cleanly first).
//!
//! Tray icon state variants (a distinct "recording" icon) are not
//! implemented: there is no second icon asset to swap in via `set_icon`, and
//! tauri's tray tooltip — the other lightweight option — is documented as
//! unsupported on Linux, this project's only target platform today. A
//! single static icon is used for every dictation phase; the HUD window is
//! the actual state indicator (see `sink.rs`, `ui/src/hud/Hud.svelte`).

use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::state::AppState;

const MENU_TOGGLE: &str = "toggle-dictation";
const MENU_REFINE: &str = "toggle-refinement";
const MENU_SETTINGS: &str = "open-settings";
const MENU_QUIT: &str = "quit";

/// Builds the tray icon and its menu, wiring every item to its handler.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let refine_enabled = {
        let state = app.state::<AppState>();
        state
            .settings
            .read()
            .map(|s| s.refine.enabled)
            .unwrap_or(false)
    };

    let toggle = MenuItem::with_id(app, MENU_TOGGLE, "Toggle dictation", true, None::<&str>)?;
    let refine = CheckMenuItem::with_id(
        app,
        MENU_REFINE,
        "Refinement",
        true,
        refine_enabled,
        None::<&str>,
    )?;
    let settings_item = MenuItem::with_id(app, MENU_SETTINGS, "Open settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&toggle, &refine, &settings_item, &quit])?;

    let refine_for_handler = refine.clone();
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| handle_menu_event(app, event, &refine_for_handler));

    match app.default_window_icon().cloned() {
        Some(icon) => builder = builder.icon(icon),
        None => tracing::warn!("no default window icon configured; tray icon may not render"),
    }

    builder.build(app)?;

    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: MenuEvent, refine_item: &CheckMenuItem<tauri::Wry>) {
    match event.id().as_ref() {
        MENU_TOGGLE => toggle_dictation(app),
        MENU_REFINE => toggle_refinement(app, refine_item),
        MENU_SETTINGS => open_settings(app),
        MENU_QUIT => quit(app),
        _ => {}
    }
}

fn toggle_dictation(app: &AppHandle) {
    let state = app.state::<AppState>();
    let guard = state
        .session_ctl
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(handle) => handle.toggle(),
        None => {
            drop(guard);
            crate::sink::notify_no_session(app);
        }
    }
}

/// Flips `settings.refine.enabled`, persists it through the same save path
/// `save_settings` uses, and syncs the checkbox's visual state to the
/// authoritative (just-persisted) value rather than trusting the platform to
/// have already toggled it itself.
fn toggle_refinement(app: &AppHandle, refine_item: &CheckMenuItem<tauri::Wry>) {
    let state = app.state::<AppState>();

    let mut settings = {
        let guard = state
            .settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    };
    settings.refine.enabled = !settings.refine.enabled;
    let enabled = settings.refine.enabled;

    if let Err(e) = crate::commands::persist_and_apply(app, &state, settings) {
        tracing::warn!("failed to toggle refinement: {e}");
        return;
    }

    if let Err(e) = refine_item.set_checked(enabled) {
        tracing::warn!("failed to update refinement menu checkbox: {e}");
    }
}

fn open_settings(app: &AppHandle) {
    match app.get_webview_window("main") {
        Some(window) => {
            if let Err(e) = window.show() {
                tracing::warn!("failed to show main window: {e}");
            }
            if let Err(e) = window.set_focus() {
                tracing::warn!("failed to focus main window: {e}");
            }
        }
        None => tracing::warn!("main window not found; cannot open settings"),
    }
}

fn quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    crate::runtime_boot::shutdown(&state);
    app.exit(0);
}
