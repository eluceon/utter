//! evdev-based [`HotkeySource`]: monitors every readable `/dev/input`
//! keyboard device directly, tracking a chord's pressed-set across all of
//! them.
//!
//! This module only builds on Linux; non-Linux targets never reference it
//! (see `create_source` and `check_permissions` in `crate::hotkey`).

use std::str::FromStr;

use evdev::{Device, EventSummary, KeyCode};

use crate::hotkey::{
    is_stale, ChordTracker, HotkeyEvent, HotkeySource, HotkeySpec, KeyToken, PermissionProbe,
    SHUTDOWN_POLL_INTERVAL,
};

/// The full alphabet, used as a "does this device look like a keyboard"
/// heuristic: evdev reports `KEY` capability for all sorts of hardware
/// (mice report button codes as "keys" too, media-control devices report a
/// handful of specific keys, etc), but only an actual keyboard supports
/// every letter. Requiring the whole alphabet — rather than just one
/// arbitrary key — reliably excludes non-keyboard devices from getting a
/// reader thread.
const ALPHABET_KEYS: [KeyCode; 26] = [
    KeyCode::KEY_A,
    KeyCode::KEY_B,
    KeyCode::KEY_C,
    KeyCode::KEY_D,
    KeyCode::KEY_E,
    KeyCode::KEY_F,
    KeyCode::KEY_G,
    KeyCode::KEY_H,
    KeyCode::KEY_I,
    KeyCode::KEY_J,
    KeyCode::KEY_K,
    KeyCode::KEY_L,
    KeyCode::KEY_M,
    KeyCode::KEY_N,
    KeyCode::KEY_O,
    KeyCode::KEY_P,
    KeyCode::KEY_Q,
    KeyCode::KEY_R,
    KeyCode::KEY_S,
    KeyCode::KEY_T,
    KeyCode::KEY_U,
    KeyCode::KEY_V,
    KeyCode::KEY_W,
    KeyCode::KEY_X,
    KeyCode::KEY_Y,
    KeyCode::KEY_Z,
];

/// True if `dev` supports every letter key, i.e. it looks like an actual
/// keyboard rather than a mouse, joystick, or media-control device.
///
/// `pub(crate)` so `crate::modifier_wait`'s live evdev probe can reuse the
/// same "is this device a keyboard" heuristic rather than duplicating it.
pub(crate) fn looks_like_keyboard(dev: &Device) -> bool {
    let Some(keys) = dev.supported_keys() else {
        return false;
    };
    ALPHABET_KEYS.iter().all(|code| keys.contains(*code))
}

/// True if at least one `/dev/input/event*` device can be opened for
/// reading. `evdev::enumerate()` silently skips devices it can't open, so an
/// empty result here means none were readable — either none exist, or this
/// process lacks `input` group membership.
pub(crate) fn any_input_device_readable() -> bool {
    evdev::enumerate().next().is_some()
}

/// Probes the two permissions the Linux backend depends on: readable evdev
/// keyboard devices, and a writable `/dev/uinput`.
pub(crate) fn probe_permissions() -> PermissionProbe {
    PermissionProbe {
        input_group: any_input_device_readable(),
        uinput_writable: uinput_writable(),
    }
}

fn uinput_writable() -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

/// Resolves a [`HotkeySpec`]'s tokens to evdev key-code alternatives: one
/// group per token, where any key within a group satisfies it (e.g. either
/// physical Ctrl key satisfies a `Ctrl` token).
fn resolve_groups(spec: &HotkeySpec) -> Vec<Vec<KeyCode>> {
    spec.tokens()
        .map(|token| resolve_alternatives(*token))
        .collect()
}

fn resolve_alternatives(token: KeyToken) -> Vec<KeyCode> {
    let codes = match token {
        KeyToken::Ctrl => vec![KeyCode::KEY_LEFTCTRL, KeyCode::KEY_RIGHTCTRL],
        KeyToken::Alt => vec![KeyCode::KEY_LEFTALT, KeyCode::KEY_RIGHTALT],
        KeyToken::Shift => vec![KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT],
        KeyToken::Super => vec![KeyCode::KEY_LEFTMETA, KeyCode::KEY_RIGHTMETA],
        KeyToken::Char(c) => KeyCode::from_str(&format!("KEY_{}", c.to_ascii_uppercase()))
            .into_iter()
            .collect(),
        KeyToken::Function(n) => KeyCode::from_str(&format!("KEY_F{n}"))
            .into_iter()
            .collect(),
        KeyToken::Space => vec![KeyCode::KEY_SPACE],
    };

    if codes.is_empty() {
        tracing::warn!(
            "utter-inject: hotkey token {token:?} did not resolve to a known evdev key code"
        );
    }

    codes
}

/// An evdev-backed [`HotkeySource`]: spawns one reader thread per readable
/// keyboard device and merges their key events into a single chord tracker.
pub(crate) struct EvdevHotkeySource {
    groups: Vec<Vec<KeyCode>>,
    generation: u64,
}

impl EvdevHotkeySource {
    pub(crate) fn new(spec: &HotkeySpec, generation: u64) -> Self {
        Self {
            groups: resolve_groups(spec),
            generation,
        }
    }
}

impl HotkeySource for EvdevHotkeySource {
    fn run(self: Box<Self>, tx: crossbeam_channel::Sender<HotkeyEvent>) {
        let devices: Vec<Device> = evdev::enumerate()
            .map(|(_, dev)| dev)
            .filter(looks_like_keyboard)
            .collect();

        if devices.is_empty() {
            tracing::warn!("utter-inject: no readable evdev keyboard devices found; hotkey capture is inactive");
            return;
        }

        let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<(KeyCode, bool)>();
        let generation = self.generation;

        for mut device in devices {
            let raw_tx = raw_tx.clone();
            if let Err(err) = device.set_nonblocking(true) {
                tracing::warn!("utter-inject: failed to set evdev device non-blocking: {err}");
                continue;
            }

            std::thread::spawn(move || loop {
                if is_stale(generation) {
                    return;
                }

                match device.fetch_events() {
                    Ok(events) => {
                        for event in events {
                            if let EventSummary::Key(_, code, value) = event.destructure() {
                                // 0 = up, 1 = down, 2 = autorepeat; only
                                // forward real transitions.
                                if (value == 0 || value == 1)
                                    && raw_tx.send((code, value == 1)).is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        // No data right now: bounded wake-up (rather than a
                        // blocking read) is what lets this thread notice
                        // staleness or a `raw_tx` disconnect promptly even
                        // when this specific device is otherwise idle.
                        std::thread::sleep(SHUTDOWN_POLL_INTERVAL);
                    }
                    Err(err) => {
                        tracing::warn!("utter-inject: evdev device read error: {err}");
                        return;
                    }
                }
            });
        }
        drop(raw_tx);

        let mut tracker = ChordTracker::new(self.groups);
        loop {
            if is_stale(generation) {
                return;
            }

            match raw_rx.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
                Ok((code, is_down)) => {
                    if let Some(event) = tracker.on_key_change(code, is_down) {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::parse_hotkey;

    #[test]
    fn resolves_modifier_tokens_to_both_physical_keys() {
        let spec = parse_hotkey("ctrl+super").unwrap();
        let groups = resolve_groups(&spec);
        assert_eq!(groups.len(), 2);
        assert!(groups
            .iter()
            .any(|g| g.contains(&KeyCode::KEY_LEFTCTRL) && g.contains(&KeyCode::KEY_RIGHTCTRL)));
        assert!(groups
            .iter()
            .any(|g| g.contains(&KeyCode::KEY_LEFTMETA) && g.contains(&KeyCode::KEY_RIGHTMETA)));
    }

    #[test]
    fn resolves_base_key_and_function_key() {
        let spec = parse_hotkey("ctrl+alt+d").unwrap();
        let groups = resolve_groups(&spec);
        assert!(groups.iter().any(|g| g == &vec![KeyCode::KEY_D]));

        let spec = parse_hotkey("f1").unwrap();
        let groups = resolve_groups(&spec);
        assert_eq!(groups, vec![vec![KeyCode::KEY_F1]]);
    }

    #[test]
    fn resolves_space_base_key() {
        let spec = parse_hotkey("ctrl+space").unwrap();
        let groups = resolve_groups(&spec);
        assert!(groups.iter().any(|g| g == &vec![KeyCode::KEY_SPACE]));
    }

    /// Manual, hardware-touching verification: press-and-release the given
    /// chord on a physical keyboard while this test runs. Requires `input`
    /// group membership (readable `/dev/input/event*`).
    /// Run with: `cargo test -p utter-inject -- --ignored records_hotkey_chord`
    #[test]
    #[ignore]
    fn records_hotkey_chord() {
        let spec = parse_hotkey("ctrl+alt+u").expect("valid spec");
        let source = EvdevHotkeySource::new(&spec, crate::hotkey::next_generation());
        let (tx, rx) = crossbeam_channel::unbounded();

        std::thread::spawn(move || Box::new(source).run(tx));

        eprintln!("press and release ctrl+alt+u within 10 seconds...");
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("expected a hotkey event from a physical chord press");
        assert_eq!(event, HotkeyEvent::Pressed);
    }

    /// Reads this process's live thread count from `/proc/self/status`
    /// (Linux-only, matching this module).
    fn thread_count() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("Threads:"))
                    .and_then(|n| n.trim().parse().ok())
            })
            .expect("/proc/self/status should report a parseable Threads: line on Linux")
    }

    /// Manual, hardware-touching verification of the fix in this changeset:
    /// re-registering (simulated here by constructing a second source with a
    /// newer generation and dropping the first's receiver, exactly what
    /// `save_settings` rebuilding the hotkey source will do) must make the
    /// first source's reader/merge threads exit within ~1s, with no key
    /// press required. `#[ignore]`d because it needs a readable
    /// `/dev/input/event*` keyboard; asserts on this process's own thread
    /// count (via `/proc/self/status`) rather than just printing, so it's a
    /// real (if Linux-specific and timing-sensitive) pass/fail check.
    #[test]
    #[ignore]
    fn stale_source_shuts_down_without_a_key_press() {
        let spec = parse_hotkey("ctrl+alt+u").expect("valid spec");

        let first = EvdevHotkeySource::new(&spec, crate::hotkey::next_generation());
        let (tx1, rx1) = crossbeam_channel::unbounded();
        std::thread::spawn(move || Box::new(first).run(tx1));

        // Let `first`'s per-device reader threads spin up, then snapshot:
        // this is "baseline plus exactly one source's threads."
        std::thread::sleep(std::time::Duration::from_millis(300));
        let with_first_only = thread_count();

        // Simulate `save_settings` rebuilding the source: a newer
        // generation supersedes `first`, and its receiver is dropped — but
        // the chord itself is never pressed.
        let second = EvdevHotkeySource::new(&spec, crate::hotkey::next_generation());
        let (tx2, _rx2) = crossbeam_channel::unbounded();
        std::thread::spawn(move || Box::new(second).run(tx2));
        drop(rx1);

        // Comfortably past `SHUTDOWN_POLL_INTERVAL` (5x+ margin): `first`'s
        // threads should have noticed they're stale and exited by now, and
        // `second`'s threads (over the same device set) should have fully
        // spun up. If `first` had instead leaked, this process would now be
        // running *two* sources' worth of threads — noticeably more than
        // `with_first_only`, not roughly the same.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let after = thread_count();

        assert!(
            after <= with_first_only,
            "expected `first`'s reader/merge threads to have exited, leaving \
             roughly `second`'s thread count alone (same device set) instead \
             of both sources' threads added together: \
             with_first_only={with_first_only} after={after}"
        );
    }
}
