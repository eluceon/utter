//! Hotkey chord parsing, the platform-agnostic hotkey source port, and
//! permission diagnostics.
//!
//! The platform-specific pieces (evdev monitoring, the X11 fallback) live in
//! [`crate::hotkey_evdev`] and [`crate::hotkey_x11`]; this module only holds
//! what can be reasoned about, and tested, without touching real hardware.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

/// How often the Linux backends (`hotkey_evdev`, `hotkey_x11`) wake up to
/// re-check whether they should shut down, instead of blocking forever on a
/// single read. Chosen comfortably under the ~1s shutdown bound this crate
/// targets, with margin for two checks to land inside it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// A process-wide counter bumped once per [`create_source`] call.
///
/// Detecting "my `tx`'s receiver was dropped" from inside a background
/// thread fundamentally requires attempting a real send on that exact
/// channel (`crossbeam_channel::Sender` has no side-channel-free liveness
/// peek) — so that check alone can only fire on the *next* real chord
/// event, which may never come. Re-registration (the actual hot path this
/// exists for: `save_settings` rebuilding the hotkey source) doesn't have
/// that problem: it's a construction-time event we can observe directly.
/// Each concrete [`HotkeySource`] captures its generation at construction
/// and checks it on every wake-up (see `SHUTDOWN_POLL_INTERVAL`); once a
/// *newer* source has been created, older instances recognize themselves
/// as superseded and shut down on their own, independent of `tx` activity.
static SOURCE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Bumps and returns the generation number for a freshly created hotkey
/// source. Call once per [`create_source`] invocation.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn next_generation() -> u64 {
    SOURCE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// True once `generation` is no longer the most recently created source's
/// generation, i.e. a later [`create_source`] call has superseded it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn is_stale(generation: u64) -> bool {
    SOURCE_GENERATION.load(Ordering::SeqCst) != generation
}

/// Identifies one hotkey binding among the set registered together in a
/// single [`create_source`] call, by its position in that call's `specs`
/// slice.
///
/// A newtype rather than a bare `usize` so this index can't be silently
/// confused with an unrelated integer as it travels from the hotkey port,
/// through the runtime, to whatever it ends up mapped to (e.g. a language
/// profile) — a mapping this crate deliberately knows nothing about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingId(usize);

impl BindingId {
    /// Returns the binding's position in the `specs` slice it was created
    /// from.
    pub fn index(self) -> usize {
        self.0
    }
}

impl From<usize> for BindingId {
    fn from(index: usize) -> Self {
        Self(index)
    }
}

/// A chord state transition reported by a [`HotkeySource`], identifying
/// which registered binding it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// The full chord for `binding` just became held (all of its keys are
    /// down).
    Pressed {
        /// Which binding's chord fired.
        binding: BindingId,
    },
    /// At least one key of `binding`'s previously-held chord was just
    /// released.
    Released {
        /// Which binding's chord fired.
        binding: BindingId,
    },
}

/// A background hotkey monitor. `run` takes ownership of `self` because
/// implementations block for their whole lifetime pumping OS input events;
/// callers spawn it on its own thread.
pub trait HotkeySource: Send {
    /// Runs the source's event loop, forwarding [`HotkeyEvent`]s until it
    /// shuts down. Linux implementations shut down promptly (within
    /// [`SHUTDOWN_POLL_INTERVAL`]-scale latency, not tied to any device or
    /// hotkey activity) as soon as either: `tx`'s receiving end is dropped
    /// *and* a real event is subsequently attempted, or a newer
    /// [`create_source`] call has superseded this one — the latter is what
    /// makes hotkey re-registration (e.g. `save_settings` rebuilding the
    /// source) a cheap, bounded operation rather than a thread leak.
    fn run(self: Box<Self>, tx: crossbeam_channel::Sender<HotkeyEvent>);
}

/// One logical key: a token in a parsed hotkey chord, and also the unit
/// [`ChordMatcher`] tracks live press/release state against.
///
/// Kept free of any platform key-code type so [`HotkeySpec`] compiles and is
/// testable on every target; platform backends resolve their raw key codes
/// to this type before feeding events to [`ChordMatcher`] (see
/// `hotkey_evdev::resolve_key_codes`), so e.g. either physical Ctrl key
/// reported by evdev becomes the same `Key::Ctrl` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Ctrl,
    Alt,
    Shift,
    Super,
    /// A single letter or digit, always stored lowercase.
    Char(char),
    /// `Fn` function key, 1..=24.
    Function(u8),
    /// The space bar. Kept as its own variant rather than folded into
    /// `Char` since it is not alphanumeric and needs its own evdev/X11
    /// key-code resolution (`KEY_SPACE` / `Code::Space`).
    Space,
}

impl Key {
    fn is_modifier(self) -> bool {
        matches!(self, Key::Ctrl | Key::Alt | Key::Shift | Key::Super)
    }
}

/// A parsed hotkey chord, e.g. `ctrl+alt+d` or the modifier-only `ctrl+super`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub(crate) tokens: HashSet<Key>,
}

impl HotkeySpec {
    /// Iterates the chord's tokens in unspecified order.
    ///
    /// Only consumed by the Linux backends (`hotkey_evdev`, `hotkey_x11`);
    /// allowed dead on other targets rather than cfg-gated out, since a
    /// method this small isn't worth losing to a future platform backend
    /// forgetting it exists.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn tokens(&self) -> impl Iterator<Item = &Key> {
        self.tokens.iter()
    }

    /// True if every token in the chord is a modifier (no letter, digit, or
    /// function key). Modifier-only chords are supported by the evdev
    /// backend but not by the X11 (`global-hotkey`) fallback, which always
    /// requires a non-modifier base key.
    pub fn is_modifier_only(&self) -> bool {
        !self.tokens.is_empty() && self.tokens.iter().all(|t| t.is_modifier())
    }
}

/// An error parsing a hotkey chord specification.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HotkeyParseError {
    /// The specification had no tokens at all (e.g. `""` or `"+"`).
    #[error("empty hotkey specification")]
    Empty,
    /// A token was not a recognized modifier, letter, digit, or function key.
    #[error("unknown hotkey token: {0:?}")]
    UnknownToken(String),
    /// More than one letter, digit, or function key was given; a chord has
    /// at most one base key.
    #[error("hotkey chord may have at most one base key (letter, digit, or function key)")]
    MultipleBaseKeys,
}

/// Parses a `+`-separated hotkey chord such as `"ctrl+super"` or
/// `"ctrl+alt+d"` into a [`HotkeySpec`].
///
/// Tokens are case-insensitive. Recognized modifier names: `ctrl`/`control`,
/// `alt`, `shift`, `super`/`meta`/`win`. A single letter, single digit,
/// `f1`..`f24`, or `space` is accepted as the (at most one) base key — a
/// second one (e.g. `"a+b"`) is rejected with
/// [`HotkeyParseError::MultipleBaseKeys`]. A chord made up entirely of
/// modifiers is valid (see [`HotkeySpec::is_modifier_only`]).
pub fn parse_hotkey(s: &str) -> Result<HotkeySpec, HotkeyParseError> {
    let mut tokens = HashSet::new();
    let mut saw_any = false;
    let mut saw_base_key = false;

    for raw in s.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        saw_any = true;

        let parsed = parse_token(token)?;
        if !parsed.is_modifier() {
            if saw_base_key {
                return Err(HotkeyParseError::MultipleBaseKeys);
            }
            saw_base_key = true;
        }
        tokens.insert(parsed);
    }

    if !saw_any {
        return Err(HotkeyParseError::Empty);
    }

    Ok(HotkeySpec { tokens })
}

fn parse_token(token: &str) -> Result<Key, HotkeyParseError> {
    let lower = token.to_lowercase();

    match lower.as_str() {
        "ctrl" | "control" => return Ok(Key::Ctrl),
        "alt" => return Ok(Key::Alt),
        "shift" => return Ok(Key::Shift),
        "super" | "meta" | "win" => return Ok(Key::Super),
        "space" => return Ok(Key::Space),
        _ => {}
    }

    if let Some(rest) = lower.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=24).contains(&n) {
                return Ok(Key::Function(n));
            }
        }
    }

    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Ok(Key::Char(c));
        }
    }

    Err(HotkeyParseError::UnknownToken(token.to_string()))
}

/// Creates the best available [`HotkeySource`] watching every chord in
/// `specs` at once: the evdev backend if at least one `/dev/input/event*`
/// device is readable, otherwise the X11 (`global-hotkey`) fallback.
///
/// Each [`HotkeyEvent`] the returned source produces carries a [`BindingId`]
/// equal to the firing chord's position in `specs`, so callers can register
/// several chords (e.g. one per language profile) behind a single source
/// rather than one source per chord — each evdev source opens every input
/// device on the machine, so more than one running at once would duplicate
/// that work and race for the same key presses.
///
/// Fails if any spec in `specs` is modifier-only and evdev is unavailable,
/// since the X11 fallback cannot represent a chord without a non-modifier
/// base key.
pub fn create_source(specs: &[HotkeySpec]) -> anyhow::Result<Box<dyn HotkeySource>> {
    #[cfg(target_os = "linux")]
    {
        let generation = next_generation();

        if crate::hotkey_evdev::any_input_device_readable() {
            return Ok(Box::new(crate::hotkey_evdev::EvdevHotkeySource::new(
                specs, generation,
            )));
        }

        if specs.iter().any(HotkeySpec::is_modifier_only) {
            anyhow::bail!(
                "modifier-only hotkeys require the evdev backend; the X11 fallback \
                 (global-hotkey) cannot represent a chord without a non-modifier base key"
            );
        }

        Ok(Box::new(crate::hotkey_x11::X11HotkeySource::new(
            specs, generation,
        )?))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = specs;
        anyhow::bail!("hotkey capture is not implemented on this platform yet")
    }
}

/// A snapshot of the OS-level permissions needed for evdev hotkeys and
/// uinput-based text injection to work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionReport {
    /// Whether at least one `/dev/input/event*` node is readable by the
    /// current user (a proxy for `input` group membership).
    pub input_group: bool,
    /// Whether `/dev/uinput` is writable by the current user.
    pub uinput_writable: bool,
    /// A shell snippet the user can run to fix whatever is missing.
    pub fix_command: String,
}

/// Raw probe results, kept separate from [`PermissionReport`] so the report
/// text can be built and tested as a pure function of the probe outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PermissionProbe {
    pub(crate) input_group: bool,
    pub(crate) uinput_writable: bool,
}

/// The remediation shown to the user regardless of which check failed: it is
/// always safe to (re-)apply both the group membership and the udev rule.
const FIX_COMMAND: &str = concat!(
    "sudo usermod -aG input $USER && ",
    "echo 'KERNEL==\"uinput\", MODE=\"0660\", GROUP=\"input\"' | ",
    "sudo tee /etc/udev/rules.d/60-utter-uinput.rules && ",
    "sudo udevadm control --reload-rules && sudo udevadm trigger && ",
    "echo 'log out and back in for group membership to take effect'"
);

fn build_permission_report(probe: PermissionProbe) -> PermissionReport {
    PermissionReport {
        input_group: probe.input_group,
        uinput_writable: probe.uinput_writable,
        fix_command: FIX_COMMAND.to_string(),
    }
}

/// Checks whether this process can read evdev keyboard devices and write to
/// `/dev/uinput`, the two permissions the Linux backend depends on.
pub fn check_permissions() -> PermissionReport {
    build_permission_report(probe_permissions())
}

#[cfg(target_os = "linux")]
fn probe_permissions() -> PermissionProbe {
    crate::hotkey_evdev::probe_permissions()
}

#[cfg(not(target_os = "linux"))]
fn probe_permissions() -> PermissionProbe {
    PermissionProbe {
        input_group: false,
        uinput_writable: false,
    }
}

/// Tracks every chord in a fixed set of [`HotkeySpec`]s against a live
/// stream of individual key state changes, reporting which binding (its
/// position in the `specs` slice the matcher was built from) transitions
/// between held and not-held.
///
/// Operates purely on the logical [`Key`] type, with no platform key-code
/// type or real input device involved, so it is testable on every target.
/// Physical-key alternatives (e.g. either physical Ctrl key satisfying a
/// `Key::Ctrl` token) are resolved to `Key` values before reaching this
/// type — see `hotkey_evdev::resolve_key_codes`, the evdev backend's only
/// caller.
///
/// Only wired up by the Linux evdev backend today; kept portable and
/// allowed dead elsewhere (rather than cfg-gated away) so its unit tests
/// keep running, and it stays ready, on every target.
#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct ChordMatcher {
    bindings: Vec<HashSet<Key>>,
    pressed: HashSet<Key>,
    fired: Vec<bool>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl ChordMatcher {
    /// Builds a matcher watching every chord in `specs`, indexed by
    /// position: the first spec becomes `BindingId(0)`, and so on.
    pub(crate) fn new(specs: &[HotkeySpec]) -> Self {
        let bindings: Vec<HashSet<Key>> = specs.iter().map(|spec| spec.tokens.clone()).collect();
        let fired = vec![false; bindings.len()];
        Self {
            bindings,
            pressed: HashSet::new(),
            fired,
        }
    }

    /// Records `key` as pressed and returns the resulting chord-level
    /// event, if any. See [`Self::update`] for how a transition is chosen.
    pub(crate) fn on_key_down(&mut self, key: Key) -> Option<HotkeyEvent> {
        self.pressed.insert(key);
        self.update()
    }

    /// Records `key` as released and returns the resulting chord-level
    /// event, if any. See [`Self::update`] for how a transition is chosen.
    pub(crate) fn on_key_up(&mut self, key: Key) -> Option<HotkeyEvent> {
        self.pressed.remove(&key);
        self.update()
    }

    /// Re-evaluates every binding against the current `pressed` set and
    /// reports the first one whose held/not-held state just changed.
    /// Autorepeat (a key reported down while already down) never reaches
    /// here as a change, so no event is ever re-fired while a chord stays
    /// pressed.
    ///
    /// At most one transition is reported per key change: two bindings
    /// completing simultaneously from a single key event is not a
    /// configuration this crate needs to support today, since each binding
    /// maps to a distinct chord chosen independently by the caller.
    fn update(&mut self) -> Option<HotkeyEvent> {
        for (index, tokens) in self.bindings.iter().enumerate() {
            let full = !tokens.is_empty() && tokens.iter().all(|k| self.pressed.contains(k));

            match (full, self.fired[index]) {
                (true, false) => {
                    self.fired[index] = true;
                    return Some(HotkeyEvent::Pressed {
                        binding: BindingId(index),
                    });
                }
                (false, true) => {
                    self.fired[index] = false;
                    return Some(HotkeyEvent::Released {
                        binding: BindingId(index),
                    });
                }
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifier_only_chord_case_insensitively() {
        let spec = parse_hotkey("Ctrl+SUPER").expect("should parse");
        assert!(spec.is_modifier_only());
        assert_eq!(spec.tokens, HashSet::from([Key::Ctrl, Key::Super]));
    }

    #[test]
    fn parses_chord_with_base_key() {
        let spec = parse_hotkey("ctrl+alt+d").expect("should parse");
        assert!(!spec.is_modifier_only());
        assert_eq!(
            spec.tokens,
            HashSet::from([Key::Ctrl, Key::Alt, Key::Char('d')])
        );
    }

    #[test]
    fn accepts_modifier_aliases() {
        let spec = parse_hotkey("control+meta").expect("should parse");
        assert_eq!(spec.tokens, HashSet::from([Key::Ctrl, Key::Super]));

        let spec = parse_hotkey("win+shift").expect("should parse");
        assert_eq!(spec.tokens, HashSet::from([Key::Super, Key::Shift]));
    }

    #[test]
    fn accepts_digit_and_function_keys() {
        let spec = parse_hotkey("ctrl+5").expect("should parse");
        assert_eq!(spec.tokens, HashSet::from([Key::Ctrl, Key::Char('5')]));

        let spec = parse_hotkey("ctrl+f1").expect("should parse");
        assert_eq!(spec.tokens, HashSet::from([Key::Ctrl, Key::Function(1)]));
    }

    #[test]
    fn parses_space_as_a_base_key() {
        let spec = parse_hotkey("ctrl+space").expect("should parse");
        assert!(!spec.is_modifier_only());
        assert_eq!(spec.tokens, HashSet::from([Key::Ctrl, Key::Space]));
    }

    #[test]
    fn rejects_unknown_token_naming_it() {
        let err = parse_hotkey("ctrl+banana").unwrap_err();
        assert_eq!(err, HotkeyParseError::UnknownToken("banana".to_string()));
    }

    #[test]
    fn rejects_out_of_range_function_key() {
        assert!(parse_hotkey("ctrl+f25").is_err());
    }

    #[test]
    fn rejects_more_than_one_base_key() {
        assert_eq!(parse_hotkey("a+b"), Err(HotkeyParseError::MultipleBaseKeys));
        assert_eq!(
            parse_hotkey("ctrl+d+f1"),
            Err(HotkeyParseError::MultipleBaseKeys)
        );
    }

    #[test]
    fn rejects_empty_specification() {
        assert_eq!(parse_hotkey(""), Err(HotkeyParseError::Empty));
        assert_eq!(parse_hotkey("+"), Err(HotkeyParseError::Empty));
    }

    #[test]
    fn permission_report_fix_command_mentions_group_and_udev_rule() {
        let report = build_permission_report(PermissionProbe {
            input_group: false,
            uinput_writable: false,
        });
        assert!(report.fix_command.contains("usermod -aG input"));
        assert!(report.fix_command.contains(r#"KERNEL=="uinput""#));
    }

    #[test]
    fn permission_report_carries_probe_values_through() {
        let report = build_permission_report(PermissionProbe {
            input_group: true,
            uinput_writable: false,
        });
        assert!(report.input_group);
        assert!(!report.uinput_writable);
    }

    #[test]
    fn chord_matcher_partial_chord_emits_nothing() {
        let specs = [parse_hotkey("ctrl+alt").expect("valid chord")];
        let mut matcher = ChordMatcher::new(&specs);
        assert_eq!(matcher.on_key_down(Key::Ctrl), None);
    }

    #[test]
    fn chord_matcher_fires_pressed_once_and_ignores_repeat() {
        let specs = [parse_hotkey("ctrl+alt").expect("valid chord")];
        let mut matcher = ChordMatcher::new(&specs);
        assert_eq!(matcher.on_key_down(Key::Ctrl), None);
        assert_eq!(
            matcher.on_key_down(Key::Alt),
            Some(HotkeyEvent::Pressed {
                binding: BindingId(0)
            })
        );
        // autorepeat: Alt reported down again while already down.
        assert_eq!(matcher.on_key_down(Key::Alt), None);
        assert_eq!(matcher.on_key_down(Key::Ctrl), None);
    }

    #[test]
    fn chord_matcher_fires_released_once_on_first_release() {
        let specs = [parse_hotkey("ctrl+alt").expect("valid chord")];
        let mut matcher = ChordMatcher::new(&specs);
        matcher.on_key_down(Key::Ctrl);
        assert_eq!(
            matcher.on_key_down(Key::Alt),
            Some(HotkeyEvent::Pressed {
                binding: BindingId(0)
            })
        );

        assert_eq!(
            matcher.on_key_up(Key::Ctrl),
            Some(HotkeyEvent::Released {
                binding: BindingId(0)
            })
        );
        // The other key releasing afterward should not re-fire.
        assert_eq!(matcher.on_key_up(Key::Alt), None);
    }

    #[test]
    fn each_binding_is_reported_by_its_own_id() {
        let specs = [
            parse_hotkey("ctrl+super").expect("valid chord"),
            parse_hotkey("ctrl+alt+super").expect("valid chord"),
        ];
        let mut matcher = ChordMatcher::new(&specs);

        assert_eq!(matcher.on_key_down(Key::Ctrl), None);
        assert_eq!(
            matcher.on_key_down(Key::Super),
            Some(HotkeyEvent::Pressed {
                binding: BindingId(0)
            })
        );
        assert_eq!(
            matcher.on_key_up(Key::Super),
            Some(HotkeyEvent::Released {
                binding: BindingId(0)
            })
        );
    }

    #[test]
    fn generation_strictly_increases_and_marks_old_ones_stale() {
        // Only asserts facts that hold regardless of other tests bumping
        // the same process-wide counter concurrently: each call to
        // `next_generation` is strictly greater than the last one *this
        // thread* observed, and a generation is always stale once a later
        // one has been minted (concurrent activity from other tests can
        // only make `first` more stale, never less).
        let first = next_generation();
        let second = next_generation();

        assert!(second > first);
        assert!(is_stale(first));
    }
}
