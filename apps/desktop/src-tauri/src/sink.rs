//! [`EventSink`] implementation that emits Tauri events to every window and
//! drives the HUD window's visibility from the dictation phase.

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

        let result = if visible { hud.show() } else { hud.hide() };
        if let Err(e) = result {
            tracing::warn!(
                "failed to {} hud window: {e}",
                if visible { "show" } else { "hide" }
            );
        }
    }
}

impl EventSink for TauriEventSink {
    fn emit_state(&self, state: &str, level: f32, partial: Option<&str>) {
        let Some(phase) = parse_phase(state) else {
            tracing::warn!("unknown dictation phase from runtime: {state:?}");
            return;
        };

        self.set_hud_visible(phase != DictationPhase::Idle);

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
