//! Thin wrapper around the system clipboard (`arboard`), which supports
//! Linux X11 and Wayland (via `wayland-data-control`), Windows, and macOS —
//! so, unlike the uinput pieces of this crate, nothing here needs a
//! non-Linux stub.

use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
use utter_core::InjectError;

/// The two X11/Wayland selections a paste can read from.
///
/// Both are written before a paste because the paste chord is Shift+Insert
/// (see [`crate::uinput_kbd`]), and which selection that reads is decided by
/// the receiving toolkit, not by us: GTK applications take CLIPBOARD, VTE
/// terminals take PRIMARY. Writing only one would make dictation work in an
/// editor and silently do nothing in a terminal.
const SELECTIONS: [LinuxClipboardKind; 2] =
    [LinuxClipboardKind::Clipboard, LinuxClipboardKind::Primary];

/// One selection's previous contents, saved so a paste can put them back.
pub(crate) struct SavedSelections([(LinuxClipboardKind, Option<String>); 2]);

/// Best-effort read of both selections: an entry is `None` if that selection
/// is unavailable, empty, or holds non-text data.
pub(crate) fn save() -> SavedSelections {
    SavedSelections(SELECTIONS.map(|kind| (kind, read_text_lossy(kind))))
}

fn read_text_lossy(kind: LinuxClipboardKind) -> Option<String> {
    arboard::Clipboard::new()
        .ok()?
        .get()
        .clipboard(kind)
        .text()
        .ok()
}

/// Publishes `text` to every selection a paste might read from.
///
/// Fails only if *no* selection could be written: a compositor that supports
/// CLIPBOARD but not PRIMARY (or the reverse) must still be able to paste,
/// so a partial success is a success. The error names every selection that
/// was tried, since "the clipboard is unavailable" is otherwise the same
/// message whether one backend or all of them failed.
pub(crate) fn set_text(text: &str) -> Result<(), InjectError> {
    let mut failures = Vec::new();
    for kind in SELECTIONS {
        if let Err(e) = set_one(kind, text) {
            failures.push(format!("{kind:?}: {e}"));
        }
    }

    if failures.len() == SELECTIONS.len() {
        return Err(InjectError::Backend(format!(
            "no clipboard selection could be written ({})",
            failures.join("; ")
        )));
    }
    Ok(())
}

fn set_one(kind: LinuxClipboardKind, text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set()
        .clipboard(kind)
        .text(text)
        .map_err(|e| e.to_string())
}

/// Best-effort restore of previously saved selections.
///
/// Failures are logged, not propagated: by the time this runs the text has
/// already been injected, so failing to restore the user's prior clipboard
/// is unfortunate but must never turn a successful injection into an error.
pub(crate) fn restore(previous: SavedSelections) {
    for (kind, value) in previous.0 {
        let Some(value) = value else {
            continue;
        };
        if let Err(err) = set_one(kind, &value) {
            tracing::warn!("utter-inject: failed to restore {kind:?} after paste: {err}");
        }
    }
}
