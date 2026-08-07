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

    use std::time::Duration;

    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, InputEvent, KeyCode, KeyEvent};
    use utter_core::InjectError;

    /// How long to wait, once, right after registering a brand-new uinput
    /// device, before it is used to synthesize any key events. Compositors
    /// (Wayland in particular) enumerate input devices asynchronously; a
    /// device used the instant it's created can be invisible to the
    /// compositor for the first event or two. Paid once at construction,
    /// not per injection.
    const DEVICE_SETTLE_DELAY: Duration = Duration::from_millis(200);

    /// Pause after each synthesized character while typing.
    ///
    /// Without it, a long string is written to `/dev/uinput` as one
    /// uninterrupted burst: dozens of key events land in the kernel's
    /// per-reader evdev queue faster than the reading side (the
    /// compositor's input thread) can drain it, and once that queue fills
    /// the kernel silently drops the oldest queued events instead of
    /// blocking — the synthesized text arrives with a chunk missing from
    /// the middle, with no error surfaced anywhere. Spacing characters out
    /// gives the reader a chance to keep the queue drained.
    ///
    /// 1ms, combined with `tap` batching each character's down+up into one
    /// `SYN_REPORT` (see below), was measured reliable: a raw-uinput-capture
    /// harness reconstructing the typed text from this device's own event
    /// stream got byte-identical output on 28/28 consecutive runs across two
    /// 200+ character strings (mixed case, digits, punctuation). 0ms, even
    /// with the same batching, dropped characters on most runs (queue
    /// overflow again) — so 1ms is a measured floor, not a guess. This is an
    /// 8x reduction from the original 8ms fix, which predated the `tap`
    /// batching and was set without exploring lower values.
    const INTER_KEY_DELAY: Duration = Duration::from_millis(1);

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

    /// The chord that asks the focused application to paste.
    ///
    /// **Shift+Insert, deliberately not Ctrl+V.** uinput emits raw key
    /// *codes*, and the compositor translates each one through whatever
    /// keyboard layout is active at that moment. `KEY_V` is a letter key, so
    /// under a non-Latin layout it no longer means "v": on the Russian
    /// layout it is `Cyrillic_em`, and the application receives Ctrl+м —
    /// not its paste shortcut. It does not paste, and the bare character is
    /// inserted instead, so a whole dictated sentence arrives as the single
    /// letter "м".
    ///
    /// Picking a different letter key cannot fix this, because a layout that
    /// has no Latin `v` anywhere offers no key code that means paste; and an
    /// application cannot switch the user's layout on Wayland. `KEY_INSERT`
    /// sidesteps the problem entirely by carrying no character at all, so it
    /// survives translation through any layout unchanged.
    ///
    /// The cost is that Shift+Insert reads CLIPBOARD in GTK applications but
    /// PRIMARY in VTE terminals, which is why
    /// [`ClipboardPasteInjector`](crate::ClipboardPasteInjector) publishes
    /// the text to both selections.
    pub(super) const PASTE_CHORD: [KeyCode; 2] = [KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_INSERT];

    /// The key transitions a chord expands to: every key pressed in the
    /// order given, then released in the reverse order, so a modifier listed
    /// first is down before — and still down after — the key it modifies.
    ///
    /// Pure so the ordering can be tested without a uinput device; see
    /// [`VirtualKeyboard::chord`] for why the ordering matters.
    pub(super) fn chord_steps(codes: &[KeyCode]) -> Vec<(KeyCode, i32)> {
        let down = codes.iter().map(|&code| (code, 1));
        let up = codes.iter().rev().map(|&code| (code, 0));
        down.chain(up).collect()
    }

    /// The union of every key code `char_to_key` can produce, plus the keys
    /// needed for [`PASTE_CHORD`].
    fn all_supported_keys() -> AttributeSet<KeyCode> {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_LEFTCTRL);
        keys.insert(KeyCode::KEY_LEFTSHIFT);
        for code in PASTE_CHORD {
            keys.insert(code);
        }
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

            // Let the compositor pick up the new device before it's ever
            // used; see `DEVICE_SETTLE_DELAY`.
            std::thread::sleep(DEVICE_SETTLE_DELAY);

            Ok(Self { device })
        }

        /// Synthesizes the paste combo; see [`PASTE_CHORD`] for why it is
        /// Shift+Insert rather than Ctrl+V.
        pub fn paste(&mut self) -> Result<(), InjectError> {
            self.chord(&PASTE_CHORD)
        }

        /// Types `text` one character at a time. Pre-validates the whole
        /// string first; see [`validate_typeable`].
        pub fn type_text(&mut self, text: &str) -> Result<(), InjectError> {
            validate_typeable(text)?;

            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                // Unwrap-free by construction: validate_typeable already
                // confirmed every character maps to a key.
                if let Some((code, shift)) = char_to_key(c) {
                    if shift {
                        self.tap(&[KeyCode::KEY_LEFTSHIFT, code])?;
                    } else {
                        self.tap(&[code])?;
                    }
                }
                // See `INTER_KEY_DELAY`: skip the sleep after the very last
                // character, there is nothing left to race.
                if chars.peek().is_some() {
                    std::thread::sleep(INTER_KEY_DELAY);
                }
            }

            Ok(())
        }

        /// Presses `codes` in order and releases them in reverse, each key
        /// transition in its own uinput write and therefore its own
        /// `SYN_REPORT`.
        ///
        /// The ordering is the point. A real keyboard can never report a
        /// modifier and the key it modifies going down at the same instant:
        /// Shift settles into the modifier state first, and only the next
        /// input frame carries Insert. Emitting both in one frame — as this
        /// did until a `Shift+Insert` paste turned out to do nothing in
        /// Chrome — leaves a toolkit free to process the second key before
        /// it has applied the first, seeing a bare `Insert` and pasting
        /// nothing. Wine and GTK tolerate the batched form; not everything
        /// does, and hardware never produces it.
        fn chord(&mut self, codes: &[KeyCode]) -> Result<(), InjectError> {
            for (code, value) in chord_steps(codes) {
                self.emit(&[code], value)?;
                std::thread::sleep(INTER_KEY_DELAY);
            }
            Ok(())
        }

        /// Presses and releases `codes` as a single uinput write carrying
        /// both the down and up events (one syscall, one `SYN_REPORT`,
        /// instead of `chord`'s two of each). Used for `type_text`'s
        /// per-character taps, which fire far more often than `chord`'s one
        /// combo per paste: halving the syscall/`SYN_REPORT` count here
        /// measurably matters at typing volume. Proven byte-identical
        /// against `chord`'s old two-emit behavior by a raw-uinput-capture
        /// harness (see `INTER_KEY_DELAY`) across 8+ consecutive runs of a
        /// 230-character string before this became the shipped behavior.
        fn tap(&mut self, codes: &[KeyCode]) -> Result<(), InjectError> {
            let mut events: Vec<InputEvent> = Vec::with_capacity(codes.len() * 2);
            events.extend(
                codes
                    .iter()
                    .map(|code| InputEvent::from(KeyEvent::new(*code, 1))),
            );
            events.extend(
                codes
                    .iter()
                    .map(|code| InputEvent::from(KeyEvent::new(*code, 0))),
            );

            self.device
                .emit(&events)
                .map_err(|e| InjectError::Backend(format!("failed to emit uinput event: {e}")))
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

        pub fn paste(&mut self) -> Result<(), InjectError> {
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
    use super::linux_impl::{char_to_key, chord_steps, validate_typeable, PASTE_CHORD};
    use evdev::KeyCode;

    #[test]
    fn a_chord_presses_its_modifier_before_the_key_and_releases_it_after() {
        // Hardware cannot report a modifier and its key going down in the
        // same input frame, and a toolkit that processes the frame in order
        // may apply the key before the modifier — seeing a bare Insert, and
        // pasting nothing.
        assert_eq!(
            chord_steps(&PASTE_CHORD),
            vec![
                (KeyCode::KEY_LEFTSHIFT, 1),
                (KeyCode::KEY_INSERT, 1),
                (KeyCode::KEY_INSERT, 0),
                (KeyCode::KEY_LEFTSHIFT, 0),
            ]
        );
    }

    #[test]
    fn the_paste_chord_survives_a_non_latin_keyboard_layout() {
        // uinput emits key *codes*; the compositor translates them through
        // the active layout. A chord built from a character key therefore
        // means something else under a Cyrillic (or Greek, or Arabic)
        // layout — Ctrl+V becomes Ctrl+м, which no application treats as
        // paste, and the letter is typed instead of the transcript.
        for code in PASTE_CHORD {
            let types_a_character = (0u8..=127)
                .filter_map(|byte| char_to_key(byte as char))
                .any(|(mapped, _)| mapped == code);
            assert!(
                !types_a_character,
                "{code:?} is a character key, so its meaning depends on the active layout"
            );
        }
    }

    #[test]
    fn the_paste_chord_is_shift_insert() {
        assert_eq!(PASTE_CHORD, [KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_INSERT]);
    }

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
