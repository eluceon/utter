//! Waits for the user's physical modifier keys (Ctrl/Alt/Shift/Super) to be
//! released before a backend synthesizes a paste chord.
//!
//! Push-to-talk holds the hotkey (e.g. `ctrl+super`) down while recording
//! and releases it to stop; injection follows within about a second. If a
//! physical Ctrl or Super key is still (or again) held when the virtual
//! keyboard synthesizes the paste chord — the release of one chord key already fires
//! [`crate::HotkeyEvent::Released`] while the other may still be
//! physically down, and a fast re-press can also land inside the window —
//! the compositor sees an unrelated chord (e.g. Ctrl+Super+V) that most
//! apps don't bind to paste. The paste then silently does nothing even
//! though the injector reports success. Waiting for a clean modifier state
//! first avoids that race.

use std::time::{Duration, Instant};

/// Upper bound on how long to wait for modifiers to clear before giving up
/// and synthesizing the paste anyway: better a possible race than an
/// indefinite hang if a modifier is stuck down for some unrelated reason.
pub(crate) const RELEASE_TIMEOUT: Duration = Duration::from_millis(1000);

/// How often to re-sample modifier state while waiting.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Blocks the calling thread, calling `probe` every `poll_interval`, until
/// it reports nothing held (returns `false`) or `timeout` has elapsed —
/// whichever comes first. Returns `true` if it observed a clear state,
/// `false` if it gave up at the timeout with `probe` still returning `true`.
///
/// Pulled out of the live evdev polling below so the retry/timeout logic
/// itself is unit-testable without any device or real waiting.
pub(crate) fn wait_for_clear_with(
    timeout: Duration,
    poll_interval: Duration,
    mut probe: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !probe() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll_interval);
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{wait_for_clear_with, POLL_INTERVAL, RELEASE_TIMEOUT};
    use evdev::{AttributeSet, Device, KeyCode};

    /// Every physical modifier key that could turn a synthesized paste chord
    /// into a chord the focused app doesn't recognize as paste (see module
    /// docs).
    const MODIFIER_KEYS: [KeyCode; 8] = [
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_RIGHTCTRL,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_RIGHTALT,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_RIGHTSHIFT,
        KeyCode::KEY_LEFTMETA,
        KeyCode::KEY_RIGHTMETA,
    ];

    fn any_modifier_down(keys: &AttributeSet<KeyCode>) -> bool {
        MODIFIER_KEYS.iter().any(|code| keys.contains(*code))
    }

    /// Waits (bounded by [`RELEASE_TIMEOUT`]) until none of the readable
    /// keyboard devices report a modifier key held.
    ///
    /// Best-effort: a device that fails to query its key state is treated
    /// as "not held" rather than aborting the whole wait, and no readable
    /// keyboard at all is treated as "nothing to wait on" (proceed
    /// immediately) — both match this function's role as a defensive
    /// pre-paste check, not a hard permission requirement.
    pub(crate) fn wait_for_modifiers_released() -> bool {
        let devices: Vec<Device> = evdev::enumerate()
            .map(|(_, dev)| dev)
            .filter(crate::hotkey_evdev::looks_like_keyboard)
            .collect();

        if devices.is_empty() {
            return true;
        }

        wait_for_clear_with(RELEASE_TIMEOUT, POLL_INTERVAL, || {
            devices.iter().any(|dev| match dev.get_key_state() {
                Ok(keys) => any_modifier_down(&keys),
                Err(_) => false,
            })
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod stub_impl {
    /// No evdev backend exists off Linux; `ClipboardPasteInjector::inject`
    /// can never actually run there either (see `crate::uinput_kbd`'s
    /// uninhabited stub), so this only needs to make the call site
    /// `cfg`-free, not do anything meaningful.
    pub(crate) fn wait_for_modifiers_released() -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux_impl::wait_for_modifiers_released;
#[cfg(not(target_os = "linux"))]
pub(crate) use stub_impl::wait_for_modifiers_released;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn returns_true_immediately_when_already_clear() {
        assert!(wait_for_clear_with(
            Duration::from_millis(50),
            Duration::from_millis(5),
            || false
        ));
    }

    #[test]
    fn returns_true_once_probe_clears_before_timeout() {
        let calls = Cell::new(0u32);
        let cleared =
            wait_for_clear_with(Duration::from_millis(200), Duration::from_millis(5), || {
                let n = calls.get();
                calls.set(n + 1);
                n < 2 // held for the first two polls, clear from the third on
            });
        assert!(cleared);
        assert!(calls.get() >= 3);
    }

    #[test]
    fn returns_false_when_still_held_at_the_timeout() {
        assert!(!wait_for_clear_with(
            Duration::from_millis(30),
            Duration::from_millis(5),
            || true
        ));
    }
}
