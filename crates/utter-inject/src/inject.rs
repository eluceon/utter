//! Layered text injection backends: clipboard+paste, direct typing, and
//! clipboard-only as the universal last resort.
//!
//! Each backend implements [`utter_core::TextInjector`]. Constructing
//! [`ClipboardPasteInjector`] or [`TypeInjector`] can fail with
//! [`InjectError::NoBackend`] where the platform lacks a uinput virtual
//! keyboard (i.e. anywhere but Linux); callers should compose only the
//! backends that constructed successfully, typically via
//! [`crate::chain::ChainInjector`].

use std::time::Duration;

use utter_core::{InjectError, InjectionMethod, TextInjector};

use crate::clipboard;
use crate::modifier_wait;
use crate::uinput_kbd::VirtualKeyboard;

/// How long to wait after setting the clipboard before synthesizing Ctrl+V.
/// On Wayland the clipboard offer has to be registered with the compositor
/// before a paste can pick it up; without this delay a paste synthesized
/// immediately after `set_text` can race that registration and land on
/// whatever was previously on the clipboard (or nothing at all).
const CLIPBOARD_SET_TO_PASTE_DELAY: Duration = Duration::from_millis(80);

/// How long to wait after synthesizing Ctrl+V before restoring the user's
/// previous clipboard contents. The target application reads the clipboard
/// asynchronously in response to the paste keystroke, so restoring too soon
/// would race it and paste our own restored (old) value instead.
const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(150);

/// Injects text by placing it on the clipboard and synthesizing Ctrl+V.
///
/// The previous clipboard contents are saved before the paste and restored
/// afterward on a best-effort basis: a failed restore is logged (see
/// [`crate::clipboard::restore_text`]) but never turns a successful
/// injection into an error.
pub struct ClipboardPasteInjector {
    keyboard: VirtualKeyboard,
}

impl ClipboardPasteInjector {
    /// Creates a new injector, failing if no uinput virtual keyboard backend
    /// is available on this platform.
    pub fn new() -> Result<Self, InjectError> {
        Ok(Self {
            keyboard: VirtualKeyboard::new()?,
        })
    }
}

impl TextInjector for ClipboardPasteInjector {
    fn inject(&mut self, text: &str) -> Result<InjectionMethod, InjectError> {
        let previous = clipboard::read_text_lossy();
        clipboard::set_text(text)?;
        std::thread::sleep(CLIPBOARD_SET_TO_PASTE_DELAY);

        // Push-to-talk releases the hotkey right before this runs; wait
        // (bounded) for any physical modifier still down to clear so the
        // compositor sees a clean Ctrl+V rather than some other chord (see
        // `crate::modifier_wait`).
        modifier_wait::wait_for_modifiers_released();

        let paste_result = self.keyboard.ctrl_v();
        std::thread::sleep(CLIPBOARD_RESTORE_DELAY);
        clipboard::restore_text(previous);

        paste_result?;
        Ok(InjectionMethod::ClipboardPaste)
    }
}

/// Injects text by synthesizing individual key presses, without touching
/// the clipboard.
///
/// Limited to characters the virtual keyboard can map to a key code on a
/// standard US QWERTY layout (see [`crate::uinput_kbd`]); any unmappable
/// character fails the whole call *before* anything is typed, so a
/// [`crate::chain::ChainInjector`] can cleanly fall back to another backend
/// instead of typing a partial string.
pub struct TypeInjector {
    keyboard: VirtualKeyboard,
}

impl TypeInjector {
    /// Creates a new injector, failing if no uinput virtual keyboard backend
    /// is available on this platform.
    pub fn new() -> Result<Self, InjectError> {
        Ok(Self {
            keyboard: VirtualKeyboard::new()?,
        })
    }
}

impl TextInjector for TypeInjector {
    fn inject(&mut self, text: &str) -> Result<InjectionMethod, InjectError> {
        // Same push-to-talk modifier-release race as `ClipboardPasteInjector`
        // (see `crate::modifier_wait`): `type_text` synthesizes Shift+letter
        // chords through the same virtual keyboard, so a physical Super/Ctrl
        // still down would just as readily turn one into an intercepted
        // shortcut instead of a typed character. Bounded (at most ~1s), so
        // it can never hang injection indefinitely even on an unmappable
        // `text` that `type_text` will go on to reject.
        modifier_wait::wait_for_modifiers_released();
        self.keyboard.type_text(text)?;
        Ok(InjectionMethod::Type)
    }
}

/// Injects text by placing it on the clipboard only, leaving the user to
/// paste it manually. The universal last resort: it has no hardware
/// dependency beyond a working clipboard, so it never needs to be gated
/// behind an availability check.
#[derive(Debug, Default)]
pub struct ClipboardOnlyInjector;

impl ClipboardOnlyInjector {
    /// Creates a new injector. Never fails: clipboard access is only
    /// attempted (and can only fail) at `inject` time.
    pub fn new() -> Self {
        Self
    }
}

impl TextInjector for ClipboardOnlyInjector {
    fn inject(&mut self, text: &str) -> Result<InjectionMethod, InjectError> {
        clipboard::set_text(text)?;
        Ok(InjectionMethod::ClipboardOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_only_injector_reports_its_method_on_success() {
        // arboard needs a running display/compositor session; skip cleanly
        // if this environment doesn't have one instead of failing the suite.
        let mut injector = ClipboardOnlyInjector::new();
        match injector.inject("utter-inject test payload") {
            Ok(method) => assert_eq!(method, InjectionMethod::ClipboardOnly),
            Err(InjectError::Backend(msg)) => {
                eprintln!("skipping: no clipboard available in this environment: {msg}");
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    /// Manual, hardware-touching verification: requires a real focused text
    /// editor window and the process to have both `input`-group and
    /// `/dev/uinput` permissions (see `crate::hotkey::check_permissions`).
    /// Run with: `cargo test -p utter-inject -- --ignored injects_into_focused_window`
    ///
    /// Linux-only: `ClipboardPasteInjector` is uninhabited on other
    /// platforms (no uinput backend), which would make this test body
    /// unreachable there.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn injects_into_focused_window() {
        let mut injector = ClipboardPasteInjector::new().expect("uinput backend available");
        let method = injector
            .inject("utter-inject manual test: clipboard-paste\n")
            .expect("injection should succeed with a focused text field");
        assert_eq!(method, InjectionMethod::ClipboardPaste);
    }
}
