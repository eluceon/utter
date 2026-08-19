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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::events::{DictationPhase, DictationState, Notice, NoticeKind};
use crate::runtime::EventSink;
use crate::state::AppState;

/// The Tauri window label the HUD lives at (see `tauri.conf.json`).
const HUD_WINDOW_LABEL: &str = "hud";

/// The desktop notification title shown for notices.
const NOTIFICATION_TITLE: &str = "Utter";

/// The shortest gap between any two desktop notifications, whatever they
/// say. See [`NoticeThrottle`] for why there is a floor at all.
const MIN_NOTIFICATION_GAP: Duration = Duration::from_secs(2);

/// How long a message that has just been shown suppresses an identical one.
/// Longer than [`MIN_NOTIFICATION_GAP`] because a repeat says nothing new,
/// while a different message might.
const REPEAT_SUPPRESSION: Duration = Duration::from_secs(60);

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

/// Remembers the last desktop notification shown, so the next one can be
/// held back.
///
/// Notices are the app's only way to tell the user it has degraded, and
/// during dictation there is no window open to tell them in — so every kind
/// goes to the desktop notification service, not just errors. That makes
/// their *rate* the app's problem: the runtime reports some conditions per
/// audio frame (a speech engine that errors on every `feed` emits a warning
/// dozens of times a second), and dozens of desktop notifications a second
/// is a worse failure than the one being reported.
///
/// So a notification needs both to be [`MIN_NOTIFICATION_GAP`] after the
/// previous one and, if it says the same thing, [`REPEAT_SUPPRESSION`] after
/// it. What this deliberately does not do is queue: a notification held back
/// is dropped, not deferred, because by the time it could be shown it would
/// be describing something the user has already been told about or that is
/// no longer happening. What is dropped is by construction either a repeat or
/// one of a burst whose first member did get through, so the *condition*
/// never goes unannounced even when a particular report of it does.
///
/// The full wording of every notice is still emitted on the event bus, where
/// the settings window lists it for as long as that window is open; the ones
/// reported before any window exists to hear them are parked instead, and
/// drained on mount (see [`crate::state::PendingNotices`]).
///
/// Lives in [`AppState`], not in the sink: sinks are constructed fresh for
/// each emit, so a throttle owned by one would never see the previous one's
/// notification.
#[derive(Default)]
pub struct NoticeThrottle {
    last: Mutex<Option<(String, Instant)>>,
}

impl NoticeThrottle {
    /// Whether `msg` may be shown as a desktop notification now, recording
    /// it as shown if so.
    fn allow(&self, msg: &str) -> bool {
        // A poisoned lock here means a previous caller panicked mid-update;
        // the worst that recovering costs is one extra notification, which
        // is a better outcome than taking the dictation pipeline down.
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if !passes_throttle(last.as_ref(), msg, now) {
            return false;
        }
        *last = Some((msg.to_string(), now));
        true
    }
}

/// The decision [`NoticeThrottle::allow`] makes, without the lock or the
/// clock, so it can be tested at whatever instants the test likes.
fn passes_throttle(last: Option<&(String, Instant)>, msg: &str, now: Instant) -> bool {
    let Some((last_msg, shown_at)) = last else {
        return true;
    };
    let elapsed = now.saturating_duration_since(*shown_at);
    if elapsed < MIN_NOTIFICATION_GAP {
        return false;
    }
    elapsed >= REPEAT_SUPPRESSION || last_msg != msg
}

/// Emits `dictation-state`/`notice` events to every window and puts notices
/// in front of the user as desktop notifications. Cheap to construct (just an
/// `AppHandle` clone plus two shared-state lookups), so callers build a fresh
/// one whenever they need to emit rather than threading one instance around.
pub struct TauriEventSink {
    app: AppHandle,
    /// Shared with `AppState::hud_enabled` (see its docs): a live mirror of
    /// `settings.dictation.hud`, kept in-place-updatable so a sink built at
    /// boot (or a previous `rebuild`) still observes later settings changes
    /// without needing to be reconstructed.
    hud_enabled: Arc<AtomicBool>,
    /// Shared with `AppState::notice_throttle`, for the same reason
    /// `hud_enabled` is shared: one rate limit for the whole app, not one
    /// per short-lived sink (which would be no rate limit at all).
    notice_throttle: Arc<NoticeThrottle>,
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        let state = app.state::<AppState>();
        let hud_enabled = state.hud_enabled.clone();
        let notice_throttle = state.notice_throttle.clone();
        Self {
            app,
            hud_enabled,
            notice_throttle,
        }
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

        match notice_kind {
            NoticeKind::Error => tracing::error!("{msg}"),
            NoticeKind::Warning => tracing::warn!("{msg}"),
            NoticeKind::Info => tracing::info!("{msg}"),
        }

        let notice = Notice {
            kind: notice_kind,
            message: msg.to_string(),
        };
        if let Err(e) = self.app.emit("notice", notice) {
            tracing::warn!("failed to emit notice: {e}");
        }

        // Every kind, not just errors: the settings window is closed for
        // most of the app's life, and a notice nobody is on screen to read
        // is the bug this replaces (see `NoticeThrottle` for the rate).
        if self.notice_throttle.allow(msg) {
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

/// Maps `EventSink::notify`'s `kind` string onto the wire enum. `pub(crate)`
/// because [`crate::state::PendingNotices`] parks the same payload shape this
/// builds, and a notice must not change severity depending on which of the
/// two channels carried it.
pub(crate) fn parse_kind(kind: &str) -> NoticeKind {
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

    /// The tests below are all written in terms of the two constants, which
    /// pins the shape of the rule but not its scale: a gap of a millisecond
    /// would satisfy every one of them and still let a condition reported
    /// per audio frame out as a notification per audio frame, which is the
    /// whole thing the throttle exists to prevent. So the scale is pinned
    /// here, where changing either constant has to be deliberate.
    #[test]
    fn the_windows_are_wide_enough_to_be_a_rate_limit() {
        assert!(MIN_NOTIFICATION_GAP >= Duration::from_secs(1));
        assert!(REPEAT_SUPPRESSION > MIN_NOTIFICATION_GAP);
    }

    /// A notice with nothing shown before it always gets through — the
    /// common case by far, since most notices are reported once per run.
    #[test]
    fn first_notification_is_never_held_back() {
        assert!(passes_throttle(None, "engine missing", Instant::now()));
    }

    #[test]
    fn identical_message_is_suppressed_until_the_repeat_window_passes() {
        let shown_at = Instant::now();
        let last = ("speech engine error: closed".to_string(), shown_at);

        assert!(!passes_throttle(
            Some(&last),
            "speech engine error: closed",
            shown_at + REPEAT_SUPPRESSION - Duration::from_millis(1)
        ));
        assert!(passes_throttle(
            Some(&last),
            "speech engine error: closed",
            shown_at + REPEAT_SUPPRESSION
        ));
    }

    /// The rate ceiling, and the reason there is one: a runtime that reports
    /// a *different* message per audio frame must not turn into a desktop
    /// notification per audio frame either.
    #[test]
    fn different_message_still_waits_out_the_minimum_gap() {
        let shown_at = Instant::now();
        let last = ("engine missing".to_string(), shown_at);

        assert!(!passes_throttle(
            Some(&last),
            "history is full",
            shown_at + MIN_NOTIFICATION_GAP - Duration::from_millis(1)
        ));
        assert!(passes_throttle(
            Some(&last),
            "history is full",
            shown_at + MIN_NOTIFICATION_GAP
        ));
    }

    /// `allow` records what it let through, or the next call would compare
    /// against a stale message and let a repeat straight past.
    #[test]
    fn allow_records_the_message_it_let_through() {
        let throttle = NoticeThrottle::default();
        assert!(throttle.allow("live preview unavailable"));
        assert!(!throttle.allow("live preview unavailable"));
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
