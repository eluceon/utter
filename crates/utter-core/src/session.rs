//! Dictation session state machine.
//!
//! `Session::handle` is a pure, synchronous function: it consumes an `Event`,
//! updates internal state, and returns the `Effect`s the runtime must perform
//! (audio capture, STT, refinement, injection, notifications). No I/O, threads,
//! or timers happen here — the runtime executes the returned effects and feeds
//! results back in as further events.

use crate::types::{InjectionMethod, Transcript};

/// How the user's hotkey drives recording start/stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationMode {
    /// Recording is active only while the hotkey is held down.
    PushToTalk,
    /// One press starts recording, the next press stops it.
    Toggle,
}

/// The dictation session's current phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording,
    Transcribing,
    Refining,
    Injecting,
}

/// Inputs to the session state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    HotkeyPressed,
    HotkeyReleased,
    CancelRequested,
    SilenceTimeout,
    TranscriptReady(Transcript),
    TranscriptFailed(String),
    RefineDone(String),
    RefineFailed { raw: String, reason: String },
    InjectDone(InjectionMethod),
    InjectFailed(String),
}

/// Side effects the runtime must perform in response to a transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    StartCapture,
    StopCapture,
    Refine(Transcript),
    Inject(String),
    NotifyError(String),
    NotifyInfo(String),
}

/// A single dictation session: hotkey press through injected text.
pub struct Session {
    mode: DictationMode,
    refine_enabled: bool,
    state: State,
}

impl Session {
    pub fn new(mode: DictationMode, refine_enabled: bool) -> Self {
        Self {
            mode,
            refine_enabled,
            state: State::Idle,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Advance the state machine. Pure and total: every (state, event) pair
    /// is handled, unhandled combinations are no-ops that return no effects.
    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        match self.state {
            State::Idle => self.on_idle(event),
            State::Recording => self.on_recording(event),
            State::Transcribing => self.on_transcribing(event),
            State::Refining => self.on_refining(event),
            State::Injecting => self.on_injecting(event),
        }
    }

    fn on_idle(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::HotkeyPressed => {
                self.state = State::Recording;
                vec![Effect::StartCapture]
            }
            _ => vec![],
        }
    }

    fn on_recording(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::HotkeyReleased => match self.mode {
                DictationMode::PushToTalk => {
                    self.state = State::Transcribing;
                    vec![Effect::StopCapture]
                }
                DictationMode::Toggle => vec![],
            },
            Event::HotkeyPressed => match self.mode {
                DictationMode::Toggle => {
                    self.state = State::Transcribing;
                    vec![Effect::StopCapture]
                }
                DictationMode::PushToTalk => vec![],
            },
            Event::SilenceTimeout => {
                self.state = State::Transcribing;
                vec![Effect::StopCapture]
            }
            Event::CancelRequested => {
                self.state = State::Idle;
                vec![Effect::StopCapture]
            }
            _ => vec![],
        }
    }

    fn on_transcribing(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::TranscriptReady(t) => {
                if t.text.trim().is_empty() {
                    self.state = State::Idle;
                    vec![Effect::NotifyInfo("Nothing heard".to_string())]
                } else if self.refine_enabled {
                    self.state = State::Refining;
                    vec![Effect::Refine(t)]
                } else {
                    self.state = State::Injecting;
                    vec![Effect::Inject(t.text)]
                }
            }
            Event::TranscriptFailed(e) => {
                self.state = State::Idle;
                vec![Effect::NotifyError(e)]
            }
            Event::CancelRequested => {
                self.state = State::Idle;
                vec![]
            }
            _ => vec![],
        }
    }

    fn on_refining(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::RefineDone(text) => {
                self.state = State::Injecting;
                vec![Effect::Inject(text)]
            }
            Event::RefineFailed { raw, .. } => {
                self.state = State::Injecting;
                vec![
                    Effect::Inject(raw),
                    Effect::NotifyInfo(
                        "Refinement unavailable — inserted raw transcript".to_string(),
                    ),
                ]
            }
            Event::CancelRequested => {
                self.state = State::Idle;
                vec![]
            }
            _ => vec![],
        }
    }

    fn on_injecting(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::InjectDone(_) => {
                self.state = State::Idle;
                vec![]
            }
            Event::InjectFailed(e) => {
                self.state = State::Idle;
                vec![Effect::NotifyError(e)]
            }
            Event::CancelRequested => {
                self.state = State::Idle;
                vec![]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(text: &str) -> Transcript {
        Transcript {
            text: text.to_string(),
            language: None,
        }
    }

    // Idle | HotkeyPressed -> Recording, StartCapture
    #[test]
    fn idle_hotkey_pressed_starts_recording() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        let fx = s.handle(Event::HotkeyPressed);
        assert_eq!(s.state(), State::Recording);
        assert_eq!(fx, vec![Effect::StartCapture]);
    }

    // Recording (PTT) | HotkeyReleased -> Transcribing, StopCapture
    #[test]
    fn ptt_release_stops_capture_and_transcribes() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        let fx = s.handle(Event::HotkeyReleased);
        assert_eq!(s.state(), State::Transcribing);
        assert_eq!(fx, vec![Effect::StopCapture]);
    }

    // Recording (Toggle) | HotkeyReleased -> Recording, no effects
    #[test]
    fn toggle_release_stays_recording() {
        let mut s = Session::new(DictationMode::Toggle, true);
        s.handle(Event::HotkeyPressed);
        let fx = s.handle(Event::HotkeyReleased);
        assert_eq!(s.state(), State::Recording);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    // Recording (Toggle) | HotkeyPressed -> Transcribing, StopCapture
    #[test]
    fn toggle_second_press_stops_capture_and_transcribes() {
        let mut s = Session::new(DictationMode::Toggle, true);
        s.handle(Event::HotkeyPressed);
        let fx = s.handle(Event::HotkeyPressed);
        assert_eq!(s.state(), State::Transcribing);
        assert_eq!(fx, vec![Effect::StopCapture]);
    }

    // Recording | SilenceTimeout -> Transcribing, StopCapture
    #[test]
    fn recording_silence_timeout_transcribes() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        let fx = s.handle(Event::SilenceTimeout);
        assert_eq!(s.state(), State::Transcribing);
        assert_eq!(fx, vec![Effect::StopCapture]);
    }

    // Recording | CancelRequested -> Idle, StopCapture
    #[test]
    fn recording_cancel_stops_capture_and_returns_idle() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        let fx = s.handle(Event::CancelRequested);
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, vec![Effect::StopCapture]);
    }

    // Transcribing | TranscriptReady(t), refine on, t nonempty -> Refining, Refine(t)
    #[test]
    fn transcribing_ready_with_refine_on_goes_refining() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        let t = transcript("hello world");
        let fx = s.handle(Event::TranscriptReady(t.clone()));
        assert_eq!(s.state(), State::Refining);
        assert_eq!(fx, vec![Effect::Refine(t)]);
    }

    // Transcribing | TranscriptReady(t), refine off -> Injecting, Inject(t.text)
    #[test]
    fn transcribing_ready_with_refine_off_goes_injecting() {
        let mut s = Session::new(DictationMode::PushToTalk, false);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        let t = transcript("hello world");
        let fx = s.handle(Event::TranscriptReady(t));
        assert_eq!(s.state(), State::Injecting);
        assert_eq!(fx, vec![Effect::Inject("hello world".to_string())]);
    }

    // Transcribing | TranscriptReady(empty text) -> Idle, NotifyInfo("Nothing heard")
    #[test]
    fn transcribing_ready_with_empty_text_returns_idle() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        let fx = s.handle(Event::TranscriptReady(transcript("")));
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, vec![Effect::NotifyInfo("Nothing heard".to_string())]);
    }

    // "empty text" means empty after trim() — whitespace-only counts as empty.
    #[test]
    fn transcribing_ready_with_whitespace_only_text_returns_idle() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        let fx = s.handle(Event::TranscriptReady(transcript("   \n\t  ")));
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, vec![Effect::NotifyInfo("Nothing heard".to_string())]);
    }

    // Transcribing | TranscriptFailed(e) -> Idle, NotifyError(e)
    #[test]
    fn transcribing_failed_returns_idle_with_error() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        let fx = s.handle(Event::TranscriptFailed("stt crashed".to_string()));
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, vec![Effect::NotifyError("stt crashed".to_string())]);
    }

    // Refining | RefineDone(text) -> Injecting, Inject(text)
    #[test]
    fn refining_done_goes_injecting() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        let fx = s.handle(Event::RefineDone("Hello, world.".to_string()));
        assert_eq!(s.state(), State::Injecting);
        assert_eq!(fx, vec![Effect::Inject("Hello, world.".to_string())]);
    }

    // Refining | RefineFailed{raw,..} -> Injecting, [Inject(raw), NotifyInfo(...)]
    #[test]
    fn refining_failed_injects_raw_and_notifies() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        let fx = s.handle(Event::RefineFailed {
            raw: "hello world".to_string(),
            reason: "timeout".to_string(),
        });
        assert_eq!(s.state(), State::Injecting);
        assert_eq!(
            fx,
            vec![
                Effect::Inject("hello world".to_string()),
                Effect::NotifyInfo("Refinement unavailable — inserted raw transcript".to_string()),
            ]
        );
    }

    // Injecting | InjectDone(_) -> Idle, no effects
    #[test]
    fn injecting_done_returns_idle() {
        let mut s = Session::new(DictationMode::PushToTalk, false);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        let fx = s.handle(Event::InjectDone(InjectionMethod::ClipboardPaste));
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    // Injecting | InjectFailed(e) -> Idle, NotifyError(e)
    #[test]
    fn injecting_failed_returns_idle_with_error() {
        let mut s = Session::new(DictationMode::PushToTalk, false);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        let fx = s.handle(Event::InjectFailed("no focused window".to_string()));
        assert_eq!(s.state(), State::Idle);
        assert_eq!(
            fx,
            vec![Effect::NotifyError("no focused window".to_string())]
        );
    }

    // any non-Idle | CancelRequested -> Idle; StopCapture only if Recording.
    #[test]
    fn cancel_from_transcribing_returns_idle_with_no_effects() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        let fx = s.handle(Event::CancelRequested);
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    #[test]
    fn cancel_from_refining_returns_idle_with_no_effects() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        let fx = s.handle(Event::CancelRequested);
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    #[test]
    fn cancel_from_injecting_returns_idle_with_no_effects() {
        let mut s = Session::new(DictationMode::PushToTalk, false);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        let fx = s.handle(Event::CancelRequested);
        assert_eq!(s.state(), State::Idle);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    // Idle | anything else -> Idle, ignored, no panic
    #[test]
    fn idle_ignores_unrelated_events() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        for event in [
            Event::HotkeyReleased,
            Event::CancelRequested,
            Event::SilenceTimeout,
            Event::TranscriptFailed("x".to_string()),
            Event::RefineDone("x".to_string()),
            Event::InjectDone(InjectionMethod::Type),
            Event::InjectFailed("x".to_string()),
        ] {
            let fx = s.handle(event);
            assert_eq!(s.state(), State::Idle);
            assert_eq!(fx, Vec::<Effect>::new());
        }
    }

    // HotkeyPressed while Transcribing/Refining/Injecting is ignored: a new
    // session cannot start until Idle.
    #[test]
    fn hotkey_pressed_ignored_while_transcribing() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        assert_eq!(s.state(), State::Transcribing);
        let fx = s.handle(Event::HotkeyPressed);
        assert_eq!(s.state(), State::Transcribing);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    #[test]
    fn hotkey_pressed_ignored_while_refining() {
        let mut s = Session::new(DictationMode::PushToTalk, true);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        assert_eq!(s.state(), State::Refining);
        let fx = s.handle(Event::HotkeyPressed);
        assert_eq!(s.state(), State::Refining);
        assert_eq!(fx, Vec::<Effect>::new());
    }

    #[test]
    fn hotkey_pressed_ignored_while_injecting() {
        let mut s = Session::new(DictationMode::PushToTalk, false);
        s.handle(Event::HotkeyPressed);
        s.handle(Event::HotkeyReleased);
        s.handle(Event::TranscriptReady(transcript("hello world")));
        assert_eq!(s.state(), State::Injecting);
        let fx = s.handle(Event::HotkeyPressed);
        assert_eq!(s.state(), State::Injecting);
        assert_eq!(fx, Vec::<Effect>::new());
    }
}
