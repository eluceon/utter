//! X11 [`HotkeySource`] fallback, used when no evdev device is readable.
//!
//! Backed by the `global-hotkey` crate's X11 (`x11rb`) platform
//! implementation. Its [`HotKey`] always requires a non-modifier base
//! [`Code`]; there is no way to register a hotkey made only of modifiers
//! (e.g. `ctrl+super`) through this path. `create_source` checks
//! [`HotkeySpec::is_modifier_only`] and refuses to build this source for
//! such a spec, returning a clear error instead.
//!
//! This module only builds on Linux; non-Linux targets never reference it.

use std::collections::HashMap;
use std::str::FromStr;

use crossbeam_channel::RecvTimeoutError;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::hotkey::{
    is_stale, BindingId, HotkeyEvent, HotkeySource, HotkeySpec, Key, SHUTDOWN_POLL_INTERVAL,
};

/// An X11-backed [`HotkeySource`] watching every non-modifier-only chord in
/// a `specs` slice at once, indexed by position (`hotkeys[i]` is
/// `BindingId(i)`).
pub(crate) struct X11HotkeySource {
    hotkeys: Vec<HotKey>,
    generation: u64,
}

impl X11HotkeySource {
    pub(crate) fn new(specs: &[HotkeySpec], generation: u64) -> anyhow::Result<Self> {
        let hotkeys = specs
            .iter()
            .map(to_global_hotkey)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            hotkeys,
            generation,
        })
    }
}

impl HotkeySource for X11HotkeySource {
    fn run(self: Box<Self>, tx: crossbeam_channel::Sender<HotkeyEvent>) {
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(err) => {
                tracing::error!("utter-inject: failed to start the X11 hotkey manager: {err}");
                return;
            }
        };

        let mut bindings = HashMap::with_capacity(self.hotkeys.len());
        for (index, hotkey) in self.hotkeys.iter().enumerate() {
            if let Err(err) = manager.register(*hotkey) {
                tracing::error!("utter-inject: failed to register X11 hotkey: {err}");
                // Roll back whichever earlier hotkeys in this batch already
                // registered, rather than leaving them behind in the X11
                // server with nothing left in this process to ever
                // unregister them.
                for registered in &self.hotkeys[..index] {
                    let _ = manager.unregister(*registered);
                }
                return;
            }
            bindings.insert(hotkey.id(), BindingId::from(index));
        }

        // `GlobalHotKeyEvent::receiver()` is a process-wide channel shared
        // by every registered hotkey, so blocking `recv()` could sit idle
        // indefinitely if this particular chord (or any hotkey at all)
        // isn't pressed again. `recv_timeout` gives this loop a bounded
        // wake-up to notice staleness (see `crate::hotkey::is_stale`)
        // without depending on hotkey activity.
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            if is_stale(self.generation) {
                break;
            }

            match receiver.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
                Ok(event) => {
                    let Some(&binding) = bindings.get(&event.id()) else {
                        continue;
                    };

                    let mapped = match event.state() {
                        HotKeyState::Pressed => HotkeyEvent::Pressed { binding },
                        HotKeyState::Released => HotkeyEvent::Released { binding },
                    };

                    if tx.send(mapped).is_err() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // Best-effort: unregistered stale hotkeys lingering in the X11
        // server are harmless (their events just get dropped above) but
        // there's no reason to leave them registered once we're shutting
        // down.
        for hotkey in &self.hotkeys {
            let _ = manager.unregister(*hotkey);
        }
    }
}

fn to_global_hotkey(spec: &HotkeySpec) -> anyhow::Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut key = None;

    for token in spec.tokens() {
        match token {
            Key::Ctrl => mods |= Modifiers::CONTROL,
            Key::Alt => mods |= Modifiers::ALT,
            Key::Shift => mods |= Modifiers::SHIFT,
            Key::Super => mods |= Modifiers::SUPER,
            Key::Char(c) if c.is_ascii_digit() => key = Some(code_for(&format!("Digit{c}"))?),
            Key::Char(c) => key = Some(code_for(&format!("Key{}", c.to_ascii_uppercase()))?),
            Key::Function(n) => key = Some(code_for(&format!("F{n}"))?),
            Key::Space => key = Some(code_for("Space")?),
        }
    }

    let key = key.ok_or_else(|| {
        anyhow::anyhow!(
            "modifier-only hotkeys are not supported on the X11 fallback path; \
             a non-modifier base key is required"
        )
    })?;

    Ok(HotKey::new(Some(mods), key))
}

fn code_for(name: &str) -> anyhow::Result<Code> {
    Code::from_str(name).map_err(|_| anyhow::anyhow!("unsupported X11 key code: {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::parse_hotkey;

    #[test]
    fn converts_chord_with_base_key() {
        let spec = parse_hotkey("ctrl+alt+d").unwrap();
        let hotkey = to_global_hotkey(&spec).expect("should convert");
        assert_eq!(hotkey.key, Code::KeyD);
        assert!(hotkey.mods.contains(Modifiers::CONTROL));
        assert!(hotkey.mods.contains(Modifiers::ALT));
    }

    #[test]
    fn rejects_modifier_only_chord() {
        let spec = parse_hotkey("ctrl+super").unwrap();
        assert!(to_global_hotkey(&spec).is_err());
    }

    #[test]
    fn converts_digit_and_function_keys() {
        let spec = parse_hotkey("ctrl+5").unwrap();
        assert_eq!(to_global_hotkey(&spec).unwrap().key, Code::Digit5);

        let spec = parse_hotkey("ctrl+f1").unwrap();
        assert_eq!(to_global_hotkey(&spec).unwrap().key, Code::F1);
    }

    #[test]
    fn converts_space_base_key() {
        let spec = parse_hotkey("ctrl+space").unwrap();
        let hotkey = to_global_hotkey(&spec).expect("should convert");
        assert_eq!(hotkey.key, Code::Space);
        assert!(hotkey.mods.contains(Modifiers::CONTROL));
    }
}
