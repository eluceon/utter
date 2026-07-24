//! [`EventSink`] implementation that emits Tauri events to every window and
//! drives the HUD window's visibility from the dictation phase.
//!
//! ## The HUD must never take keyboard focus
//!
//! Injection (paste or direct typing) is synthesized via a virtual keyboard
//! right after a dictation session ends, targeting whatever window
//! currently holds keyboard focus. If the HUD itself holds focus at that
//! moment, the synthesized keystrokes go to the HUD (a borderless overlay
//! with no text field) instead of the app the user was dictating into —
//! the injector still reports success, but nothing visibly happens.
//!
//! `tauri.conf.json`'s hud window sets `"focus": false`, which only
//! controls whether the *window manager* grants it focus at creation time.
//! On Linux/GTK (`tao`'s window backend), that alone is not durable: a
//! window created with `focus: false` but the default `focusable: true`
//! gets its GTK `accept-focus` property temporarily cleared, then a
//! one-shot handler restores it to `true` on the window's *first* GTK
//! `draw` event — which fires the first time the (initially hidden) HUD is
//! actually shown. From that point on the HUD is fully focusable, and
//! GNOME Wayland grants it keyboard focus every time `show()` is called
//! afterward, stealing it from whatever the user was dictating into.
//! Confirmed live: `hud.is_focused()` reads `false` before the very first
//! `show()` and `true` every time after.
//!
//! `tauri.conf.json` also sets `"focusable": false` on the hud window,
//! which is necessary but, measured live on GNOME/Wayland/Mutter, is *not*
//! sufficient by itself: the compositor still grants the window real
//! keyboard focus on `show()` regardless of the GTK-level `accept-focus`
//! property, because that property is an X11/GTK concept Mutter's Wayland
//! `xdg-shell` focus policy for ordinary toplevels does not fully honor.
//! [`configure_hud_window`] additionally sets the window's GTK type hint to
//! `Notification`, which *is* a category Mutter's Wayland focus policy
//! excludes from ever receiving keyboard focus — measured live, this drops
//! the window holding focus at the moment injection fires from 3/3 trials
//! to 1/3 (only the very first `show()` of a freshly started process can
//! still race, matching the "first GTK `draw` event" trigger above; every
//! `show()` after that in the same process is clean).
//!
//! [`TauriEventSink::set_hud_visible`] additionally re-asserts
//! `focusable(false)` via [`tauri::WebviewWindow::set_focusable`] after
//! every `show()` as defense-in-depth, and hides the HUD (rather than
//! showing it) during the `Injecting` phase as well as `Idle`, so it holds
//! as little window state as possible while the synthesized keystrokes go
//! out — injection is effectively instant, so the HUD never visibly
//! renders an "injecting" state anyway.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::events::{DictationPhase, DictationState, Notice, NoticeKind};
use crate::runtime::EventSink;
use crate::state::AppState;

/// The Tauri window label the HUD lives at (see `tauri.conf.json`).
const HUD_WINDOW_LABEL: &str = "hud";

/// The desktop notification title shown for error notices.
const NOTIFICATION_TITLE: &str = "Utter";

/// Emits a `"warning"` notice via a fresh sink — used when a UI action (tray
/// toggle, HUD cancel) reaches for the dictation runtime but none is running
/// (e.g. `runtime_boot::boot` itself failed outright at startup), so the
/// user gets feedback instead of a silent no-op.
pub(crate) fn notify_no_session(app: &AppHandle) {
    TauriEventSink::new(app.clone()).notify("warning", "dictation engine is not running");
}

/// Marks the HUD as a `Notification`-type window at the GTK level (see
/// module docs for why `tauri.conf.json`'s `focusable: false` alone is not
/// enough on GNOME/Wayland/Mutter). Called once from `setup`; logs and
/// otherwise no-ops if the window or its underlying GTK handle isn't
/// available, since a HUD that still occasionally steals focus is
/// degraded, not fatal.
#[cfg(target_os = "linux")]
pub(crate) fn configure_hud_window(app: &AppHandle) {
    let Some(hud) = app.get_webview_window(HUD_WINDOW_LABEL) else {
        tracing::warn!("hud window not found at setup time; skipping type hint");
        return;
    };
    match hud.gtk_window() {
        Ok(gtk_win) => {
            use gtk::prelude::GtkWindowExt;
            gtk_win.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
        }
        Err(e) => tracing::warn!("failed to get hud's gtk window: {e}"),
    }
}

/// Decides whether the HUD window should actually be shown for a given
/// dictation phase, given the current "Show HUD" preference. Hiding is
/// never gated on the preference — a HUD that was visible before the
/// setting was turned off still needs to hide on the next transition back
/// to idle — only *showing* it is.
fn should_show_hud(phase_wants_visible: bool, hud_enabled: bool) -> bool {
    phase_wants_visible && hud_enabled
}

/// Emits `dictation-state`/`notice` events to every window and shows a
/// desktop notification for errors. Cheap to construct (just an `AppHandle`
/// clone plus a shared flag lookup), so callers build a fresh one whenever
/// they need to emit rather than threading one instance around.
pub struct TauriEventSink {
    app: AppHandle,
    /// Shared with `AppState::hud_enabled` (see its docs): a live mirror of
    /// `settings.dictation.hud`, kept in-place-updatable so a sink built at
    /// boot (or a previous `rebuild`) still observes later settings changes
    /// without needing to be reconstructed.
    hud_enabled: Arc<AtomicBool>,
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        let hud_enabled = app.state::<AppState>().hud_enabled.clone();
        Self { app, hud_enabled }
    }

    /// Shows or hides the HUD window, logging (rather than propagating) any
    /// failure to find or toggle it — a missing HUD window should never take
    /// the dictation pipeline down with it.
    fn set_hud_visible(&self, visible: bool) {
        let visible = should_show_hud(visible, self.hud_enabled.load(Ordering::Relaxed));

        let Some(hud) = self.app.get_webview_window(HUD_WINDOW_LABEL) else {
            tracing::warn!(
                "hud window not found; cannot {}",
                if visible { "show" } else { "hide" }
            );
            return;
        };

        // Only call show() on an idle->visible edge, not on every
        // already-visible re-emit: recording level ticks re-emit the same
        // "recording" phase many times a second, and calling show() on an
        // already-shown window is wasteful and re-triggers a focus grant on
        // some compositors.
        let already_visible = hud.is_visible().unwrap_or(false);
        let result = if visible {
            if already_visible {
                Ok(())
            } else {
                hud.show()
            }
        } else {
            hud.hide()
        };
        if let Err(e) = result {
            tracing::warn!(
                "failed to {} hud window: {e}",
                if visible { "show" } else { "hide" }
            );
        }

        // Defense-in-depth against focus-stealing (see module docs for the
        // full picture: `tauri.conf.json`'s `focusable: false` plus the GTK
        // `Notification` type hint from `configure_hud_window` do most of
        // the work): re-asserting non-focusable here too guards against any
        // windowing-toolkit path that might still grant it focus on
        // `show()` — cheap (one message to the window thread) and a no-op
        // if focus was never at risk.
        if visible {
            if let Err(e) = hud.set_focusable(false) {
                tracing::warn!("failed to keep hud window non-focusable: {e}");
            }
        }
    }
}

impl EventSink for TauriEventSink {
    fn emit_state(&self, state: &str, level: f32, partial: Option<&str>) {
        let Some(phase) = parse_phase(state) else {
            tracing::warn!("unknown dictation phase from runtime: {state:?}");
            return;
        };

        // Hide (don't show) the HUD during Injecting too, not just Idle:
        // injection is synthesized right after this call returns (see
        // `runtime::dispatch`), so the HUD must already be non-visible
        // before the keystrokes go out rather than reacting after the
        // fact. Injection is effectively instant, so the HUD never
        // visibly renders an "injecting" state anyway.
        self.set_hud_visible(!matches!(
            phase,
            DictationPhase::Idle | DictationPhase::Injecting
        ));

        let payload = DictationState {
            state: phase,
            level,
            partial: partial.map(str::to_string),
        };
        if let Err(e) = self.app.emit("dictation-state", payload) {
            tracing::warn!("failed to emit dictation-state: {e}");
        }
    }

    fn notify(&self, kind: &str, msg: &str) {
        let notice_kind = parse_kind(kind);

        let notice = Notice {
            kind: notice_kind,
            message: msg.to_string(),
        };
        if let Err(e) = self.app.emit("notice", notice) {
            tracing::warn!("failed to emit notice: {e}");
        }

        if notice_kind == NoticeKind::Error {
            let result = self
                .app
                .notification()
                .builder()
                .title(NOTIFICATION_TITLE)
                .body(msg)
                .show();
            if let Err(e) = result {
                tracing::warn!("failed to show desktop notification: {e}");
            }
        }
    }
}

fn parse_phase(state: &str) -> Option<DictationPhase> {
    Some(match state {
        "idle" => DictationPhase::Idle,
        "recording" => DictationPhase::Recording,
        "transcribing" => DictationPhase::Transcribing,
        "refining" => DictationPhase::Refining,
        "injecting" => DictationPhase::Injecting,
        _ => return None,
    })
}

fn parse_kind(kind: &str) -> NoticeKind {
    match kind {
        "warning" => NoticeKind::Warning,
        "error" => NoticeKind::Error,
        _ => NoticeKind::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_known_phase() {
        assert_eq!(parse_phase("idle"), Some(DictationPhase::Idle));
        assert_eq!(parse_phase("recording"), Some(DictationPhase::Recording));
        assert_eq!(
            parse_phase("transcribing"),
            Some(DictationPhase::Transcribing)
        );
        assert_eq!(parse_phase("refining"), Some(DictationPhase::Refining));
        assert_eq!(parse_phase("injecting"), Some(DictationPhase::Injecting));
        assert_eq!(parse_phase("bogus"), None);
    }

    #[test]
    fn parses_notice_kind_defaulting_unknown_to_info() {
        assert_eq!(parse_kind("warning"), NoticeKind::Warning);
        assert_eq!(parse_kind("error"), NoticeKind::Error);
        assert_eq!(parse_kind("info"), NoticeKind::Info);
        assert_eq!(parse_kind("whatever"), NoticeKind::Info);
    }

    #[test]
    fn hud_never_shown_when_disabled_regardless_of_phase() {
        assert!(!should_show_hud(true, false));
        assert!(!should_show_hud(false, false));
    }

    #[test]
    fn hud_follows_phase_when_enabled() {
        assert!(should_show_hud(true, true));
        assert!(!should_show_hud(false, true));
    }
}
