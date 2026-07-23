//! uinput-backed virtual keyboard for synthesizing key events.
//!
//! Events written to a `/dev/uinput` virtual device are delivered to the
//! kernel input stack exactly as though a hardware keyboard produced them.
//! That is what lets this approach work under Wayland compositors (GNOME
//! included), where synthetic input cannot otherwise be injected into an
//! arbitrary focused window.
//!
//! This mechanism is Linux-only; [`VirtualKeyboard`] exists on every
//! platform so callers don't need `cfg` gates, but on non-Linux it is an
//! uninhabited stub whose constructor always fails with
//! [`utter_core::InjectError::NoBackend`].

#[cfg(target_os = "linux")]
mod linux_impl {
    use std::str::FromStr;

    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, InputEvent, KeyCode, KeyEvent};
    use utter_core::InjectError;

    /// Maps an ASCII character to the evdev key code that types it on a
    /// standard US QWERTY layout, plus whether Shift must be held.
    ///
    /// Returns `None` for characters this backend cannot type (non-ASCII,
    /// most control characters, etc).
    pub(super) fn char_to_key(c: char) -> Option<(KeyCode, bool)> {
        Some(match c {
            'a'..='z' => (single_char_code(c.to_ascii_uppercase())?, false),
            'A'..='Z' => (single_char_code(c)?, true),
            '1'..='9' => (single_char_code(c)?, false),
            '0' => (KeyCode::KEY_0, false),
            ' ' => (KeyCode::KEY_SPACE, false),
            '\n' => (KeyCode::KEY_ENTER, false),
            '\t' => (KeyCode::KEY_TAB, false),
            '-' => (KeyCode::KEY_MINUS, false),
            '_' => (KeyCode::KEY_MINUS, true),
            '=' => (KeyCode::KEY_EQUAL, false),
            '+' => (KeyCode::KEY_EQUAL, true),
            '[' => (KeyCode::KEY_LEFTBRACE, false),
            '{' => (KeyCode::KEY_LEFTBRACE, true),
            ']' => (KeyCode::KEY_RIGHTBRACE, false),
            '}' => (KeyCode::KEY_RIGHTBRACE, true),
            '\\' => (KeyCode::KEY_BACKSLASH, false),
            '|' => (KeyCode::KEY_BACKSLASH, true),
            ';' => (KeyCode::KEY_SEMICOLON, false),
            ':' => (KeyCode::KEY_SEMICOLON, true),
            '\'' => (KeyCode::KEY_APOSTROPHE, false),
            '"' => (KeyCode::KEY_APOSTROPHE, true),
            '`' => (KeyCode::KEY_GRAVE, false),
            '~' => (KeyCode::KEY_GRAVE, true),
            ',' => (KeyCode::KEY_COMMA, false),
            '<' => (KeyCode::KEY_COMMA, true),
            '.' => (KeyCode::KEY_DOT, false),
            '>' => (KeyCode::KEY_DOT, true),
            '/' => (KeyCode::KEY_SLASH, false),
            '?' => (KeyCode::KEY_SLASH, true),
            '!' => (KeyCode::KEY_1, true),
            '@' => (KeyCode::KEY_2, true),
            '#' => (KeyCode::KEY_3, true),
            '$' => (KeyCode::KEY_4, true),
            '%' => (KeyCode::KEY_5, true),
            '^' => (KeyCode::KEY_6, true),
            '&' => (KeyCode::KEY_7, true),
            '*' => (KeyCode::KEY_8, true),
            '(' => (KeyCode::KEY_9, true),
            ')' => (KeyCode::KEY_0, true),
            _ => return None,
        })
    }

    /// Resolves a single letter or digit to its `KEY_<CHAR>` code, e.g.
    /// `'d' -> KeyCode::KEY_D`, `'5' -> KeyCode::KEY_5`.
    fn single_char_code(c: char) -> Option<KeyCode> {
        KeyCode::from_str(&format!("KEY_{c}")).ok()
    }

    /// Validates that every character in `text` is mappable to a key code
    /// *before* any key events are emitted, so a string with one unmappable
    /// character never leaves the rest of it half-typed.
    pub(super) fn validate_typeable(text: &str) -> Result<(), InjectError> {
        match text.chars().find(|c| char_to_key(*c).is_none()) {
            Some(bad) => Err(InjectError::Backend(format!(
                "cannot type character {bad:?}: no key mapping on this layout"
            ))),
            None => Ok(()),
        }
    }

    /// The union of every key code `char_to_key` can produce, plus the keys
    /// needed for the Ctrl+V paste combo.
    fn all_supported_keys() -> AttributeSet<KeyCode> {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_LEFTCTRL);
        keys.insert(KeyCode::KEY_LEFTSHIFT);
        keys.insert(KeyCode::KEY_V);
        for byte in 0u8..=127 {
            if let Some((code, _)) = char_to_key(byte as char) {
                keys.insert(code);
            }
        }
        keys
    }

    /// A synthetic keyboard registered with the kernel's uinput subsystem.
    pub struct VirtualKeyboard {
        device: VirtualDevice,
    }

    impl VirtualKeyboard {
        /// Creates and registers a new virtual keyboard. Fails if
        /// `/dev/uinput` cannot be opened or the device cannot be created
        /// (typically a permissions problem; see
        /// [`crate::hotkey::check_permissions`]).
        pub fn new() -> Result<Self, InjectError> {
            let device = VirtualDevice::builder()
                .map_err(|e| InjectError::Backend(format!("/dev/uinput unavailable: {e}")))?
                .name("utter-virtual-keyboard")
                .with_keys(&all_supported_keys())
                .map_err(|e| InjectError::Backend(format!("failed to set uinput keymap: {e}")))?
                .build()
                .map_err(|e| {
                    InjectError::Backend(format!("failed to create uinput device: {e}"))
                })?;

            Ok(Self { device })
        }

        /// Synthesizes a Ctrl+V key combo.
        pub fn ctrl_v(&mut self) -> Result<(), InjectError> {
            self.chord(&[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_V])
        }

        /// Types `text` one character at a time. Pre-validates the whole
        /// string first; see [`validate_typeable`].
        pub fn type_text(&mut self, text: &str) -> Result<(), InjectError> {
            validate_typeable(text)?;

            for c in text.chars() {
                // Unwrap-free by construction: validate_typeable already
                // confirmed every character maps to a key.
                if let Some((code, shift)) = char_to_key(c) {
                    if shift {
                        self.chord(&[KeyCode::KEY_LEFTSHIFT, code])?;
                    } else {
                        self.chord(&[code])?;
                    }
                }
            }

            Ok(())
        }

        /// Presses then releases `codes` together, e.g. `[Ctrl, V]`.
        fn chord(&mut self, codes: &[KeyCode]) -> Result<(), InjectError> {
            self.emit(codes, 1)?;
            self.emit(codes, 0)
        }

        fn emit(&mut self, codes: &[KeyCode], value: i32) -> Result<(), InjectError> {
            let events: Vec<InputEvent> = codes
                .iter()
                .map(|code| KeyEvent::new(*code, value).into())
                .collect();

            self.device
                .emit(&events)
                .map_err(|e| InjectError::Backend(format!("failed to emit uinput event: {e}")))
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod stub_impl {
    use utter_core::InjectError;

    /// Uninhabited on non-Linux platforms: uinput is Linux-only, so `new`
    /// always fails and no instance of this type can ever exist, which lets
    /// every other method be an empty, unreachable match.
    pub enum VirtualKeyboard {}

    impl VirtualKeyboard {
        pub fn new() -> Result<Self, InjectError> {
            Err(InjectError::NoBackend(
                "uinput virtual keyboard is only available on Linux".to_string(),
            ))
        }

        pub fn ctrl_v(&mut self) -> Result<(), InjectError> {
            match *self {}
        }

        pub fn type_text(&mut self, _text: &str) -> Result<(), InjectError> {
            match *self {}
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux_impl::VirtualKeyboard;
#[cfg(not(target_os = "linux"))]
pub(crate) use stub_impl::VirtualKeyboard;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux_impl::{char_to_key, validate_typeable};
    use evdev::KeyCode;

    #[test]
    fn maps_lowercase_letter_without_shift() {
        assert_eq!(char_to_key('d'), Some((KeyCode::KEY_D, false)));
    }

    #[test]
    fn maps_uppercase_letter_with_shift() {
        assert_eq!(char_to_key('D'), Some((KeyCode::KEY_D, true)));
    }

    #[test]
    fn maps_digit() {
        assert_eq!(char_to_key('5'), Some((KeyCode::KEY_5, false)));
        assert_eq!(char_to_key('0'), Some((KeyCode::KEY_0, false)));
    }

    #[test]
    fn maps_shifted_symbol() {
        assert_eq!(char_to_key('!'), Some((KeyCode::KEY_1, true)));
        assert_eq!(char_to_key(')'), Some((KeyCode::KEY_0, true)));
    }

    #[test]
    fn maps_whitespace_and_control_keys() {
        assert_eq!(char_to_key(' '), Some((KeyCode::KEY_SPACE, false)));
        assert_eq!(char_to_key('\n'), Some((KeyCode::KEY_ENTER, false)));
        assert_eq!(char_to_key('\t'), Some((KeyCode::KEY_TAB, false)));
    }

    #[test]
    fn rejects_unmappable_character() {
        assert_eq!(char_to_key('€'), None);
        assert_eq!(char_to_key('字'), None);
    }

    #[test]
    fn validate_typeable_accepts_ascii() {
        assert!(validate_typeable("Hello, World! 123.").is_ok());
    }

    #[test]
    fn validate_typeable_rejects_before_typing() {
        let err = validate_typeable("ok€").unwrap_err();
        assert!(matches!(err, utter_core::InjectError::Backend(_)));
    }
}
