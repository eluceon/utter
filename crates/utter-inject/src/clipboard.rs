//! Thin wrapper around the system clipboard (`arboard`), which supports
//! Linux X11 and Wayland (via `wayland-data-control`), Windows, and macOS.
//!
//! Linux is the one platform with more than one selection to write. X11 and
//! Wayland both carry a CLIPBOARD *and* a PRIMARY selection, and which of
//! them a paste reads is decided by the receiving toolkit rather than by us —
//! so the Linux implementation publishes to both. `arboard` exposes that
//! choice through `LinuxClipboardKind`, which, as its name says, does not
//! exist on Windows or macOS; hence the split below, mirroring the one in
//! [`crate::uinput_kbd`].
//!
//! Publishing is not a store-and-forget operation. A selection has an
//! *owner*, and the owner serves its contents to whoever asks for them
//! later; the connection that published the text has to stay open for the
//! paste that follows to read anything. Hence [`Selections`], which holds one
//! connection open for as long as the injector that owns it lives, rather
//! than opening one per write.

#[cfg(target_os = "linux")]
mod platform {
    use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
    use utter_core::InjectError;

    /// The selections a paste can read from.
    ///
    /// Both are written because the paste chord is Shift+Insert (see
    /// [`crate::uinput_kbd`] for why it cannot be Ctrl+V), and GTK
    /// applications read CLIPBOARD from it while VTE terminals read PRIMARY.
    /// Writing only one would make dictation work in an editor and silently
    /// do nothing in a terminal.
    const SELECTIONS: [LinuxClipboardKind; 2] =
        [LinuxClipboardKind::Clipboard, LinuxClipboardKind::Primary];

    /// The previous contents of every selection, saved so a paste can put
    /// them back.
    pub(crate) struct SavedSelections([(LinuxClipboardKind, Option<String>); 2]);

    /// One clipboard connection, reused for every read and write.
    ///
    /// The connection is what makes a published selection readable: X11 has
    /// no clipboard *storage*, only owners that answer requests, so closing
    /// the connection that published a selection gives up ownership and the
    /// selection stops resolving to anything. CLIPBOARD hides this — the
    /// session's clipboard manager takes a copy under ICCCM the moment the
    /// owner goes away, which is exactly what a clipboard manager is for.
    /// PRIMARY has no manager and nothing to hide behind: opening a
    /// connection per write left it empty by the time the paste chord
    /// arrived, so VTE terminals — the toolkit that pastes PRIMARY — pasted
    /// whatever the user had last selected with the mouse, while every
    /// CLIPBOARD-reading application looked perfectly correct.
    ///
    /// Opened lazily, so constructing an injector still costs nothing on a
    /// machine with no display, and reopened after a failure rather than
    /// held onto: a connection to a display server that has since gone away
    /// would otherwise wedge injection for the rest of the session.
    #[derive(Default)]
    pub(crate) struct Selections {
        connection: Option<arboard::Clipboard>,
    }

    impl Selections {
        /// Creates a handle that has not yet connected to the clipboard.
        pub(crate) fn new() -> Self {
            Self { connection: None }
        }

        fn connection(&mut self) -> Result<&mut arboard::Clipboard, String> {
            match self.connection {
                Some(ref mut connection) => Ok(connection),
                None => {
                    let connection = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                    Ok(self.connection.insert(connection))
                }
            }
        }

        /// Best-effort read of every selection: an entry is `None` if that
        /// selection is unavailable, empty, or holds non-text data.
        pub(crate) fn save(&mut self) -> SavedSelections {
            SavedSelections(SELECTIONS.map(|kind| (kind, self.read_text_lossy(kind))))
        }

        fn read_text_lossy(&mut self, kind: LinuxClipboardKind) -> Option<String> {
            self.connection().ok()?.get().clipboard(kind).text().ok()
        }

        /// Publishes `text` to every selection a paste might read from, and
        /// keeps serving it until the next write or until `self` is dropped.
        ///
        /// Fails only if *no* selection could be written: a compositor that
        /// supports CLIPBOARD but not PRIMARY (or the reverse) must still be
        /// able to paste, so a partial success is a success. The error names
        /// every selection that was tried, since "the clipboard is
        /// unavailable" is otherwise the same message whether one backend or
        /// all of them failed.
        pub(crate) fn set_text(&mut self, text: &str) -> Result<(), InjectError> {
            let mut failures = Vec::new();
            for kind in SELECTIONS {
                if let Err(e) = self.set_one(kind, text) {
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

        fn set_one(&mut self, kind: LinuxClipboardKind, text: &str) -> Result<(), String> {
            let result = self
                .connection()?
                .set()
                .clipboard(kind)
                .text(text)
                .map_err(|e| e.to_string());
            if result.is_err() {
                self.connection = None;
            }
            result
        }

        /// Best-effort restore of previously saved selections.
        ///
        /// Failures are logged, not propagated: by the time this runs the
        /// text has already been injected, so failing to restore the user's
        /// prior clipboard is unfortunate but must never turn a successful
        /// injection into an error.
        pub(crate) fn restore(&mut self, previous: SavedSelections) {
            for (kind, value) in previous.0 {
                let Some(value) = value else {
                    continue;
                };
                if let Err(err) = self.set_one(kind, &value) {
                    tracing::warn!("utter-inject: failed to restore {kind:?} after paste: {err}");
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use utter_core::InjectError;

    /// The previous clipboard contents, saved so a paste can put them back.
    /// Windows and macOS have a single clipboard, so unlike Linux there is
    /// nothing to choose between.
    pub(crate) struct SavedSelections(Option<String>);

    /// One clipboard connection, reused for every read and write. See the
    /// Linux implementation for why the connection is kept rather than
    /// opened per call; here it is simply the cheaper of the two.
    #[derive(Default)]
    pub(crate) struct Selections {
        connection: Option<arboard::Clipboard>,
    }

    impl Selections {
        /// Creates a handle that has not yet connected to the clipboard.
        pub(crate) fn new() -> Self {
            Self { connection: None }
        }

        fn connection(&mut self) -> Result<&mut arboard::Clipboard, String> {
            match self.connection {
                Some(ref mut connection) => Ok(connection),
                None => {
                    let connection = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                    Ok(self.connection.insert(connection))
                }
            }
        }

        /// Best-effort read of the clipboard: `None` if it is unavailable,
        /// empty, or holds non-text data.
        pub(crate) fn save(&mut self) -> SavedSelections {
            SavedSelections(self.connection().ok().and_then(|c| c.get_text().ok()))
        }

        /// Overwrites the clipboard with `text`.
        pub(crate) fn set_text(&mut self, text: &str) -> Result<(), InjectError> {
            self.set_one(text)
                .map_err(|e| InjectError::Backend(format!("clipboard unavailable: {e}")))
        }

        fn set_one(&mut self, text: &str) -> Result<(), String> {
            let result = self.connection()?.set_text(text).map_err(|e| e.to_string());
            if result.is_err() {
                self.connection = None;
            }
            result
        }

        /// Best-effort restore; see the Linux implementation for why a
        /// failure here is logged rather than propagated.
        pub(crate) fn restore(&mut self, previous: SavedSelections) {
            let Some(value) = previous.0 else {
                return;
            };
            if let Err(err) = self.set_one(&value) {
                tracing::warn!("utter-inject: failed to restore the clipboard after paste: {err}");
            }
        }
    }
}

pub(crate) use platform::Selections;

/// A process has exactly one set of selections, and more than one test in
/// this crate writes them; without this they overwrite each other's payload
/// and fail on the other test's text.
#[cfg(test)]
pub(crate) static SELECTION_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Selections;
    use arboard::{GetExtLinux, LinuxClipboardKind};

    /// A paste reads a selection *after* the call that published it has
    /// returned, so publishing it is only half the job: the value has to
    /// still be served once `set_text` is done. On X11 a selection is served
    /// by whoever owns it, and ownership ends when the owning connection
    /// goes away -- CLIPBOARD survives that because the session's clipboard
    /// manager keeps a copy, PRIMARY has no manager and simply vanishes.
    ///
    /// Reads back through a second `arboard` client on purpose: that is the
    /// position every paste target is in, and the only one from which the
    /// difference is visible.
    #[test]
    fn primary_selection_is_still_served_after_set_text_returns() {
        let _guard = super::SELECTION_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut selections = Selections::new();
        let text = format!("utter-inject primary probe {}", std::process::id());
        if selections.set_text(&text).is_err() {
            eprintln!("skipping: no clipboard available in this environment");
            return;
        }

        let mut reader = arboard::Clipboard::new().expect("a clipboard that could be written to");
        let got = reader
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .ok();
        assert_eq!(
            got.as_deref(),
            Some(text.as_str()),
            "PRIMARY stopped being served once set_text returned; a VTE terminal \
             pastes that selection and would get whatever it held before"
        );
    }
}
