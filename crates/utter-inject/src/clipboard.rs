//! Thin wrapper around the system clipboard (`arboard`), which supports
//! Linux X11 and Wayland (via `wayland-data-control`), Windows, and macOS —
//! so, unlike the uinput pieces of this crate, nothing here needs a
//! non-Linux stub.

use utter_core::InjectError;

/// Best-effort read of the current clipboard text: `None` if the clipboard
/// is unavailable, empty, or holds non-text data. Used to save the previous
/// contents before a clipboard-paste injection so they can be restored.
pub(crate) fn read_text_lossy() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Overwrites the system clipboard with `text`.
pub(crate) fn set_text(text: &str) -> Result<(), InjectError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| InjectError::Backend(format!("clipboard unavailable: {e}")))?;
    clipboard
        .set_text(text)
        .map_err(|e| InjectError::Backend(format!("failed to set clipboard: {e}")))
}

/// Best-effort restore of a previously saved clipboard value.
///
/// Failures are logged, not propagated: by the time this runs the text has
/// already been injected, so failing to restore the user's prior clipboard
/// is unfortunate but must never turn a successful injection into an error.
pub(crate) fn restore_text(previous: Option<String>) {
    let Some(previous) = previous else {
        return;
    };

    if let Err(err) = set_text(&previous) {
        tracing::warn!("utter-inject: failed to restore clipboard after paste: {err}");
    }
}
