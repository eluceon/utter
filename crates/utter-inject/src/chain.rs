//! Strategy chain over [`utter_core::TextInjector`] backends: try each in
//! order and return whichever succeeds first.

use utter_core::{InjectError, InjectionMethod, TextInjector};

/// Returns the backend try-order for a user preference string.
///
/// `"auto"` tries the full layered fallback: clipboard+paste, then direct
/// typing, then clipboard-only as the universal last resort. Any recognized
/// explicit method name is tried first. `"type"` (direct uinput typing) can
/// only map ASCII-ish characters to US-QWERTY key codes (see
/// `VirtualKeyboard::type_text` / `validate_typeable` in `uinput_kbd.rs`),
/// so a non-ASCII transcript (Cyrillic, CJK, emoji, ...) always fails that
/// backend outright; falling straight through to clipboard-only would then
/// leave the text merely copied, with nothing inserted into the focused
/// app. Clipboard-paste has no such character-set restriction and still
/// auto-inserts, so it is interposed between the preferred backend and the
/// clipboard-only last resort for every preference except clipboard-only
/// itself (already first there, and clipboard-paste is a strict superset of
/// what it can insert, so listing it twice would be pure dead weight).
/// Deduplicated when the preference already *is* one of the appended
/// backends. An unrecognized preference string falls back to the `"auto"`
/// order rather than failing outright.
pub fn injection_order(preference: &str) -> Vec<InjectionMethod> {
    use InjectionMethod::{ClipboardOnly, ClipboardPaste, Type};

    let preferred = match preference.trim().to_lowercase().as_str() {
        "auto" => return vec![ClipboardPaste, Type, ClipboardOnly],
        "clipboard_paste" | "clipboard-paste" | "clipboardpaste" => ClipboardPaste,
        "type" | "typing" => Type,
        "clipboard_only" | "clipboard-only" | "clipboardonly" => ClipboardOnly,
        _ => return vec![ClipboardPaste, Type, ClipboardOnly],
    };

    if preferred == ClipboardOnly {
        vec![ClipboardOnly]
    } else {
        let mut order = vec![preferred];
        if preferred != ClipboardPaste {
            order.push(ClipboardPaste);
        }
        order.push(ClipboardOnly);
        order
    }
}

/// Tries a sequence of [`TextInjector`] backends in order, returning the
/// method reported by the first one that succeeds.
///
/// If every backend fails, the returned [`InjectError::Backend`] message
/// lists each attempt's error so the failure is diagnosable.
pub struct ChainInjector {
    injectors: Vec<Box<dyn TextInjector>>,
}

impl ChainInjector {
    /// Builds a chain that tries `injectors` in the given order.
    pub fn new(injectors: Vec<Box<dyn TextInjector>>) -> Self {
        Self { injectors }
    }
}

impl TextInjector for ChainInjector {
    fn inject(&mut self, text: &str) -> Result<InjectionMethod, InjectError> {
        let mut attempts = Vec::with_capacity(self.injectors.len());

        for injector in &mut self.injectors {
            match injector.inject(text) {
                Ok(method) => return Ok(method),
                Err(err) => attempts.push(err.to_string()),
            }
        }

        Err(InjectError::Backend(format!(
            "all {} injector(s) failed: {}",
            attempts.len(),
            attempts.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeInjector {
        result: Result<InjectionMethod, InjectError>,
    }

    impl TextInjector for FakeInjector {
        fn inject(&mut self, _text: &str) -> Result<InjectionMethod, InjectError> {
            self.result.clone()
        }
    }

    #[test]
    fn auto_order_is_the_full_fallback_chain() {
        assert_eq!(
            injection_order("auto"),
            vec![
                InjectionMethod::ClipboardPaste,
                InjectionMethod::Type,
                InjectionMethod::ClipboardOnly,
            ]
        );
    }

    #[test]
    fn type_preference_falls_back_to_clipboard_paste_before_clipboard_only() {
        // "type" can't map non-ASCII text at all (see `uinput_kbd.rs`), so
        // it must land on clipboard-paste (which still auto-inserts)
        // instead of skipping straight to clipboard-only (copy-only).
        assert_eq!(
            injection_order("type"),
            vec![
                InjectionMethod::Type,
                InjectionMethod::ClipboardPaste,
                InjectionMethod::ClipboardOnly,
            ]
        );
    }

    #[test]
    fn clipboard_paste_preference_is_not_duplicated() {
        assert_eq!(
            injection_order("clipboard_paste"),
            vec![
                InjectionMethod::ClipboardPaste,
                InjectionMethod::ClipboardOnly
            ]
        );
    }

    #[test]
    fn clipboard_only_preference_is_deduplicated() {
        assert_eq!(
            injection_order("clipboard_only"),
            vec![InjectionMethod::ClipboardOnly]
        );
    }

    #[test]
    fn unrecognized_preference_falls_back_to_auto() {
        assert_eq!(injection_order("bogus"), injection_order("auto"));
    }

    #[test]
    fn preference_matching_is_case_and_whitespace_insensitive() {
        assert_eq!(injection_order("  Type  "), injection_order("type"));
    }

    #[test]
    fn chain_falls_back_to_second_injector_on_first_failure() {
        let mut chain = ChainInjector::new(vec![
            Box::new(FakeInjector {
                result: Err(InjectError::Backend("nope".to_string())),
            }),
            Box::new(FakeInjector {
                result: Ok(InjectionMethod::Type),
            }),
        ]);

        assert_eq!(chain.inject("hi").unwrap(), InjectionMethod::Type);
    }

    #[test]
    fn chain_first_injector_wins_without_trying_others() {
        let mut chain = ChainInjector::new(vec![
            Box::new(FakeInjector {
                result: Ok(InjectionMethod::ClipboardPaste),
            }),
            Box::new(FakeInjector {
                result: Err(InjectError::Backend("should never run".to_string())),
            }),
        ]);

        assert_eq!(chain.inject("hi").unwrap(), InjectionMethod::ClipboardPaste);
    }

    #[test]
    fn chain_reports_all_attempts_when_everything_fails() {
        let mut chain = ChainInjector::new(vec![
            Box::new(FakeInjector {
                result: Err(InjectError::Backend("first failure".to_string())),
            }),
            Box::new(FakeInjector {
                result: Err(InjectError::NoBackend("second failure".to_string())),
            }),
        ]);

        let err = chain.inject("hi").unwrap_err().to_string();
        assert!(err.contains("first failure"));
        assert!(err.contains("second failure"));
    }
}
