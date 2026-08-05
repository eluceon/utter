//! Integration tests for the dictation runtime orchestrator: drives
//! `Runtime::spawn` through scripted fakes for every adapter (STT engine,
//! refiner, injector, capture backend) and a real, temp-dir-backed
//! `HistoryRepo`, asserting on the observable state sequence, notices, and
//! injected/recorded text — never on internal implementation details.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use utter_audio::AudioFrame;
use utter_core::{
    DictationMode, InjectError, InjectionMethod, RefineError, SttEngine, SttError, TextInjector,
    TextRefiner, Tone, TranscribeOptions, Transcript,
};
use utter_desktop_lib::profiles::{ProfileDeps, ProfileLoader, ProfileRegistry};
use utter_desktop_lib::runtime::{ActiveCapture, CaptureBackend, EventSink, Runtime, RuntimeDeps};
use utter_inject::{BindingId, HotkeyEvent};
use utter_refine::{ReplaceRule, Snippet};
use utter_store::{HistoryRepo, LanguageProfile};

/// Generous but bounded: every wait in these tests uses this instead of an
/// unbounded `recv`, so a regression that stalls the worker fails the test
/// promptly rather than hanging the suite.
const WAIT: Duration = Duration::from_secs(5);

// ---- fakes ------------------------------------------------------------

/// One call the fake STT engine recorded, in the order it happened. Lets
/// tests assert ordering (e.g. "every `Feed` precedes the `Finish`") rather
/// than just call counts.
#[derive(Debug, Clone, PartialEq)]
enum CallRecord {
    Feed(Vec<i16>),
    Finish,
}

/// Speech-to-text engine that returns a fixed, scripted result from
/// `finish()` regardless of what was fed to it, records every `feed`/
/// `finish` call (in order, with `feed`'s samples) into a shared log, and
/// can optionally return a scripted partial from `feed` or block for a bit
/// inside `finish` (to widen a real-time window for a racing `cancel()`).
struct FakeSttEngine {
    result: Result<Transcript, SttError>,
    calls: Arc<Mutex<Vec<CallRecord>>>,
    partial: Option<String>,
    finish_delay: Duration,
    begin_opts: Arc<Mutex<Vec<TranscribeOptions>>>,
}

impl SttEngine for FakeSttEngine {
    fn begin(&mut self, opts: &TranscribeOptions) -> Result<(), SttError> {
        self.begin_opts.lock().expect("lock").push(opts.clone());
        Ok(())
    }

    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError> {
        self.calls
            .lock()
            .expect("lock")
            .push(CallRecord::Feed(samples.to_vec()));
        Ok(self.partial.clone())
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        if !self.finish_delay.is_zero() {
            thread::sleep(self.finish_delay);
        }
        self.calls.lock().expect("lock").push(CallRecord::Finish);
        self.result.clone()
    }
}

fn transcript(text: &str) -> Transcript {
    Transcript {
        text: text.to_string(),
        language: None,
    }
}

/// Refiner whose behavior (uppercase / fail / succeed-after-a-delay) is
/// scripted, with a shared call counter tests assert on to prove (or
/// disprove) it ran. `Delay` is used to widen a real-time window for a
/// racing `cancel()` in the cancel-during-refine test.
enum RefineBehavior {
    Uppercase,
    Fail(String),
    Delay(Duration),
}

struct FakeRefiner {
    behavior: RefineBehavior,
    calls: Arc<AtomicUsize>,
}

impl TextRefiner for FakeRefiner {
    fn refine(&self, text: &str, _tone: Tone) -> Result<String, RefineError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.behavior {
            RefineBehavior::Uppercase => Ok(text.to_uppercase()),
            RefineBehavior::Fail(msg) => Err(RefineError::Http(msg.clone())),
            RefineBehavior::Delay(d) => {
                thread::sleep(*d);
                Ok(text.to_uppercase())
            }
        }
    }
}

/// Records every string handed to `inject`, optionally failing instead.
struct FakeInjector {
    injected: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

impl TextInjector for FakeInjector {
    fn inject(&mut self, text: &str) -> Result<InjectionMethod, InjectError> {
        if self.fail {
            return Err(InjectError::Backend("injection failed".to_string()));
        }
        self.injected.lock().expect("lock").push(text.to_string());
        Ok(InjectionMethod::Type)
    }
}

/// Never touches real audio hardware. Hands back a no-op capture handle,
/// and — this is the point of it — stashes the `Sender<AudioFrame>` it was
/// given into a shared slot, so a test can fetch it after "recording" starts
/// and push scripted `AudioFrame`s through the exact same channel the real
/// worker loop reads from.
struct FakeCaptureBackend {
    tx_slot: Arc<Mutex<Option<Sender<AudioFrame>>>>,
}

impl CaptureBackend for FakeCaptureBackend {
    fn start(
        &self,
        _device: Option<&str>,
        tx: Sender<AudioFrame>,
    ) -> Result<Box<dyn ActiveCapture>, String> {
        *self.tx_slot.lock().expect("lock") = Some(tx);
        Ok(Box::new(NoopActiveCapture))
    }
}

struct NoopActiveCapture;

impl ActiveCapture for NoopActiveCapture {
    fn stop(self: Box<Self>) {}
}

/// One `emit_state` call: phase, level, and partial, in the shape tests need
/// to check all three (most tests only care about the phase; a couple check
/// the partial too).
type Emission = (String, f32, Option<String>);

/// One `notify` call: kind ("info"/"error") and message.
type Notice = (String, String);

/// Records every `emit_state` call and every `notify` call, each pushed to
/// its own channel so tests can wait on the *next* one with a bounded
/// timeout instead of polling or inferring it from an unrelated event. The
/// two channels are independent: nothing here orders an emission against a
/// notice beyond what `dispatch`/`run_effect` themselves guarantee, so a
/// test that wants a notice must wait for it explicitly via `recv_notice`
/// rather than assuming it already happened because some state was observed.
struct FakeSink {
    states_tx: Sender<Emission>,
    notices_tx: Sender<Notice>,
}

impl EventSink for FakeSink {
    fn emit_state(&self, state: &str, level: f32, partial: Option<&str>) {
        let _ = self
            .states_tx
            .send((state.to_string(), level, partial.map(str::to_string)));
    }

    fn notify(&self, kind: &str, msg: &str) {
        let _ = self.notices_tx.send((kind.to_string(), msg.to_string()));
    }
}

/// Waits for the next emission and returns just its phase — what almost
/// every test wants.
fn recv_state(rx: &Receiver<Emission>) -> String {
    rx.recv_timeout(WAIT)
        .expect("expected a dictation-state emission within the timeout")
        .0
}

/// Waits for emissions, skipping any whose phase doesn't match `expected` —
/// needed once a test pushes audio frames, since each processed frame emits
/// its own `"recording"` (possibly repeated several times) before the next
/// real transition.
fn recv_until(rx: &Receiver<Emission>, expected: &str) {
    loop {
        let (state, _, _) = rx
            .recv_timeout(WAIT)
            .expect("expected a dictation-state emission within the timeout");
        if state == expected {
            return;
        }
    }
}

/// Waits for the next emission that carries a partial transcript, ignoring
/// any (e.g. the initial `"recording"` from `StartCapture`) that don't.
fn recv_partial(rx: &Receiver<Emission>) -> Option<String> {
    loop {
        let (_, _, partial) = rx
            .recv_timeout(WAIT)
            .expect("expected a dictation-state emission within the timeout");
        if partial.is_some() {
            return partial;
        }
    }
}

/// Waits for the next notice with the same bounded timeout `recv_state`
/// uses, instead of inferring a notice happened from some unrelated state
/// emission — `emit_state` and `notify` are ordered relative to each other
/// only by whatever `dispatch`/`run_effect` actually guarantee (see
/// `run_effect`'s effect ordering), which for some transitions puts the
/// notice *after* a state a test has already observed.
fn recv_notice(rx: &Receiver<Notice>) -> Notice {
    rx.recv_timeout(WAIT)
        .expect("expected a notify() call within the timeout")
}

fn assert_no_more_states(rx: &Receiver<Emission>) {
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "expected no further state emissions"
    );
}

/// A `ProfileLoader` that hands back a pre-built `ProfileDeps` for each profile id exactly once
/// (`.remove()`s it out of a per-id slot), panicking if the registry ever asks for the same
/// profile's deps a second time -- which only happens if a test's own profile list has a
/// duplicate id, a fixture bug this is deliberately strict about rather than silently rebuilding
/// or reusing something. `ProfileRegistry` itself only ever calls `load` once per entry and
/// caches the result forever (see its own doc comment), so this is never exercised twice for the
/// same id in practice.
struct FakeProfileLoader {
    slots: Mutex<HashMap<String, ProfileDeps>>,
}

impl ProfileLoader for FakeProfileLoader {
    fn load(&self, profile: &LanguageProfile) -> (ProfileDeps, Vec<(&'static str, String)>) {
        let deps = self
            .slots
            .lock()
            .expect("lock")
            .remove(&profile.id)
            .unwrap_or_else(|| panic!("no fixture registered for profile \"{}\"", profile.id));
        (deps, Vec::new())
    }
}

/// Builds a `ProfileRegistry` over `profiles`, each paired with the exact `ProfileDeps` it must
/// resolve to -- the hotkey string in each `LanguageProfile` is never consulted by the worker
/// (only `runtime_boot::parse_profile_hotkeys` reads it, an earlier step these tests bypass by
/// constructing `RuntimeDeps` directly and driving `hotkey_rx` with synthetic `BindingId`s), so
/// any placeholder works.
fn registry_with(profiles_and_deps: Vec<(LanguageProfile, ProfileDeps)>) -> ProfileRegistry {
    let mut slots = HashMap::new();
    let mut profiles = Vec::new();
    for (profile, deps) in profiles_and_deps {
        slots.insert(profile.id.clone(), deps);
        profiles.push(profile);
    }
    let loader = Box::new(FakeProfileLoader {
        slots: Mutex::new(slots),
    });
    let (registry, _notices) = ProfileRegistry::new(profiles, loader);
    registry
}

fn test_profile(id: &str) -> LanguageProfile {
    LanguageProfile {
        id: id.to_string(),
        ..LanguageProfile::default()
    }
}

/// Common `RuntimeDeps`/`ProfileDeps` fields every test wants; individual fields are overridden
/// per test before calling `build`. Always builds a `ProfileRegistry` with exactly one profile
/// (id `"default"`, binding 0) -- every existing test presses/toggles `BindingId::from(0)`, so a
/// single-profile registry keeps them all exercising the same worker-side behaviour they always
/// have. Multi-profile routing itself is covered separately (see
/// `each_hotkey_dictates_with_its_own_profile`).
struct DepsBuilder {
    mode: DictationMode,
    refine_enabled: bool,
    engine_result: Result<Transcript, SttError>,
    calls: Arc<Mutex<Vec<CallRecord>>>,
    partial: Option<String>,
    finish_delay: Duration,
    refiner: Option<(RefineBehavior, Arc<AtomicUsize>)>,
    inject_fail: bool,
    injected: Arc<Mutex<Vec<String>>>,
    rules: Vec<ReplaceRule>,
    snippets: Vec<Snippet>,
    history: Option<HistoryRepo>,
    silence: Option<Duration>,
    capture_tx_slot: Arc<Mutex<Option<Sender<AudioFrame>>>>,
    dictionary_terms: Vec<String>,
    /// The profile's language, as read into `TranscribeOptions.language` by `start_capture`.
    /// Defaults to `None` like every other fixture; the one test that cares sets a real value
    /// and asserts it reached `begin_opts`, since this is the single hop that makes a profile's
    /// language reach the engine at all.
    language: Option<String>,
    begin_opts: Arc<Mutex<Vec<TranscribeOptions>>>,
}

impl DepsBuilder {
    fn new(engine_result: Result<Transcript, SttError>) -> Self {
        Self {
            mode: DictationMode::PushToTalk,
            refine_enabled: false,
            engine_result,
            calls: Arc::new(Mutex::new(Vec::new())),
            partial: None,
            finish_delay: Duration::ZERO,
            refiner: None,
            inject_fail: false,
            injected: Arc::new(Mutex::new(Vec::new())),
            rules: Vec::new(),
            snippets: Vec::new(),
            history: None,
            silence: None,
            capture_tx_slot: Arc::new(Mutex::new(None)),
            dictionary_terms: Vec::new(),
            language: None,
            begin_opts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn build(self, hotkey_rx: Receiver<HotkeyEvent>) -> RuntimeDeps {
        let refiner: Option<Arc<dyn TextRefiner>> = self.refiner.map(|(behavior, calls)| {
            Arc::new(FakeRefiner { behavior, calls }) as Arc<dyn TextRefiner>
        });

        let profile_deps = ProfileDeps {
            engine: Box::new(FakeSttEngine {
                result: self.engine_result,
                calls: self.calls,
                partial: self.partial,
                finish_delay: self.finish_delay,
                begin_opts: self.begin_opts,
            }),
            refiner,
            refine_enabled: self.refine_enabled,
            tone: Tone::Clean,
            language: self.language,
            engine_label: "fake-engine".to_string(),
            dictionary_terms: self.dictionary_terms,
        };
        let profiles = registry_with(vec![(test_profile("default"), profile_deps)]);

        RuntimeDeps {
            mode: self.mode,
            silence: self.silence,
            profiles,
            injector: Box::new(FakeInjector {
                injected: self.injected,
                fail: self.inject_fail,
            }),
            rules: self.rules,
            snippets: self.snippets,
            history: self.history,
            capture_device: None,
            capture: Box::new(FakeCaptureBackend {
                tx_slot: self.capture_tx_slot,
            }),
            hotkey_rx,
            vad_sensitivity: 0.5,
            refine_timeout: Duration::from_secs(1),
        }
    }
}

fn fake_sink() -> (Arc<FakeSink>, Receiver<Emission>, Receiver<Notice>) {
    let (states_tx, states_rx) = unbounded();
    let (notices_tx, notices_rx) = unbounded();
    let sink = Arc::new(FakeSink {
        states_tx,
        notices_tx,
    });
    (sink, states_rx, notices_rx)
}

/// Retrieves the `Sender<AudioFrame>` a `FakeCaptureBackend` stashed once
/// capture started.
///
/// Observing the `"recording"` state emission only proves `dispatch` has
/// called `session.handle` and emitted the new phase — `emit_state` runs
/// *before* the loop over effects that actually executes `Effect::StartCapture`
/// (see `dispatch` in runtime.rs), and that execution happens on the worker
/// thread while this call runs on the test thread. So there is no
/// happens-before edge guaranteeing `ctx.capture.start()` (and therefore this
/// stash) has completed by the time a test's `recv_state` call returns —
/// only that it's *about to*. Poll with a bounded total wait instead of
/// asserting immediately.
fn capture_tx(slot: &Arc<Mutex<Option<Sender<AudioFrame>>>>) -> Sender<AudioFrame> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(tx) = slot.lock().expect("lock").clone() {
            return tx;
        }
        assert!(
            Instant::now() < deadline,
            "capture should have started and stashed its sender within {WAIT:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Builds a `ProfileDeps` whose engine always returns `text` from `finish()`, with no refiner --
/// the shape most routing tests need, where only the *identity* of the profile's output matters.
fn profile_deps_with_transcript(text: &str) -> ProfileDeps {
    ProfileDeps {
        engine: Box::new(FakeSttEngine {
            result: Ok(transcript(text)),
            calls: Arc::new(Mutex::new(Vec::new())),
            partial: None,
            finish_delay: Duration::ZERO,
            begin_opts: Arc::new(Mutex::new(Vec::new())),
        }),
        refiner: None,
        refine_enabled: false,
        tone: Tone::Clean,
        language: None,
        engine_label: "fake-engine".to_string(),
        dictionary_terms: Vec::new(),
    }
}

/// Drives one full no-refine `PushToTalk` session for `binding` (press, release, and the
/// resulting state sequence) -- the common shape `each_hotkey_dictates_with_its_own_profile` and
/// `pressing_an_unregistered_binding_starts_no_session`'s companion tests both need.
fn press_and_release(
    hotkey_tx: &Sender<HotkeyEvent>,
    states_rx: &Receiver<Emission>,
    binding: BindingId,
) {
    hotkey_tx
        .send(HotkeyEvent::Pressed { binding })
        .expect("send pressed");
    assert_eq!(recv_state(states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released { binding })
        .expect("send released");
    assert_eq!(recv_state(states_rx), "transcribing");
    assert_eq!(recv_state(states_rx), "injecting");
    assert_eq!(recv_state(states_rx), "idle");
}

// ---- tests --------------------------------------------------------------

#[test]
fn happy_path_emits_full_sequence_and_injects_refined_text() {
    let refine_calls = Arc::new(AtomicUsize::new(0));
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.refine_enabled = true;
    builder.refiner = Some((RefineBehavior::Uppercase, refine_calls.clone()));
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");

    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "refining");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(*injected.lock().expect("lock"), vec!["HELLO WORLD"]);
    assert_eq!(refine_calls.load(Ordering::SeqCst), 1);

    handle.shutdown();
}

#[test]
fn refiner_failure_injects_raw_and_notifies() {
    let refine_calls = Arc::new(AtomicUsize::new(0));
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.refine_enabled = true;
    builder.refiner = Some((
        RefineBehavior::Fail("refiner unreachable".to_string()),
        refine_calls.clone(),
    ));
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "refining");
    assert_eq!(recv_state(&states_rx), "injecting");

    // `RefineFailed` produces `[Inject(raw), NotifyInfo(..)]` (see
    // `on_refining` in session.rs): the notice is the *second* effect, run
    // only after `Inject`'s own nested dispatch has already emitted "idle".
    // So waiting for "idle" on `states_rx` proves nothing about whether the
    // notice has fired yet -- wait for it explicitly instead.
    let (kind, msg) = recv_notice(&notices_rx);
    assert_eq!(kind, "info");
    assert!(
        msg.contains("Refinement unavailable"),
        "expected a refinement-unavailable notice, got {msg:?}"
    );

    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(*injected.lock().expect("lock"), vec!["hello world"]);
    assert_eq!(refine_calls.load(Ordering::SeqCst), 1);

    handle.shutdown();
}

#[test]
fn dictionary_rule_applied_before_injection_and_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("history.sqlite3");
    let history = HistoryRepo::open(&db_path).expect("open history db");

    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("open the pod bay doors")));
    builder.rules = vec![ReplaceRule {
        heard: "pod".to_string(),
        write: "airlock".to_string(),
    }];
    builder.injected = injected.clone();
    builder.history = Some(history);
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(
        *injected.lock().expect("lock"),
        vec!["open the airlock bay doors"],
        "the dictionary rule must be applied to the raw transcript before injection"
    );

    handle.shutdown();

    let verify = HistoryRepo::open(&db_path).expect("reopen history db");
    let entries = verify.list(None, 10).expect("list history");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].raw_text, "open the pod bay doors");
    assert_eq!(entries[0].final_text, "open the airlock bay doors");
}

/// Also pins the profile's `language` reaching `TranscribeOptions` (every other fixture in this
/// file leaves it `None`, which made `start_capture` discarding it undetectable -- see
/// `start_capture`'s use of `deps.language`).
#[test]
fn dictionary_terms_are_passed_to_engine_as_initial_prompt() {
    let begin_opts = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.dictionary_terms = vec!["SQLite".to_string(), "Tauri".to_string()];
    builder.language = Some("ru".to_string());
    builder.begin_opts = begin_opts.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    let opts = begin_opts.lock().expect("lock");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].initial_prompt, Some("SQLite, Tauri".to_string()));
    assert_eq!(
        opts[0].language,
        Some("ru".to_string()),
        "the profile's own language must reach the engine's TranscribeOptions -- the single hop \
         that makes a hotkey's profile dictate in its configured language"
    );
    drop(opts);

    handle.shutdown();
}

#[test]
fn empty_dictionary_terms_produce_no_initial_prompt() {
    let begin_opts = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.begin_opts = begin_opts.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    let opts = begin_opts.lock().expect("lock");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].initial_prompt, None);
    drop(opts);

    handle.shutdown();
}

#[test]
fn snippet_trigger_bypasses_refiner() {
    let refine_calls = Arc::new(AtomicUsize::new(0));
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("insert my signature")));
    builder.refine_enabled = true;
    builder.refiner = Some((RefineBehavior::Uppercase, refine_calls.clone()));
    builder.snippets = vec![Snippet {
        trigger: "insert my signature".to_string(),
        body: "John Doe, CEO".to_string(),
    }];
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "refining");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(*injected.lock().expect("lock"), vec!["John Doe, CEO"]);
    assert_eq!(
        refine_calls.load(Ordering::SeqCst),
        0,
        "the refiner must never be called for a snippet hit"
    );

    handle.shutdown();
}

#[test]
fn cancel_during_recording_injects_nothing() {
    let injected = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("should never be seen")));
    builder.calls = calls.clone();
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");

    handle.cancel();
    assert_eq!(recv_state(&states_rx), "idle");

    assert!(injected.lock().expect("lock").is_empty());
    assert!(
        calls.lock().expect("lock").is_empty(),
        "engine.feed()/finish() must never run for a cancelled recording with no audio"
    );
    assert_no_more_states(&states_rx);

    handle.shutdown();
}

#[test]
fn cancel_after_finish_before_transcript_ready_injects_nothing() {
    // Commit point 1/2 (see the module doc comment in runtime.rs): a cancel
    // that arrives while `engine.finish()` is blocking must still prevent
    // injection. Delaying `finish()` gives the test a real-time window to
    // send `cancel()` after "transcribing" is observed (which happens
    // *before* `finish()` is even called) but comfortably before `finish()`
    // returns and the runtime checks for a pending cancel.
    let injected = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.finish_delay = Duration::from_millis(250);
    builder.calls = calls.clone();
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");

    handle.cancel();

    assert_eq!(recv_state(&states_rx), "idle");
    assert!(injected.lock().expect("lock").is_empty());
    assert_no_more_states(&states_rx);

    handle.shutdown();
}

#[test]
fn cancel_during_refine_injects_nothing() {
    // Commit point 2/2: a cancel that arrives while the refine call is in
    // flight must still prevent injection, even though the refine call
    // itself completed (the refiner's own network/inference call cannot be
    // aborted mid-flight — only the resulting `Inject` is prevented). This
    // is also the only place a "cancel just before inject" could land in
    // this design: nothing async happens between a refine call resolving
    // and the `Inject` effect it would produce, so there is no separate,
    // distinguishable commit point to test beyond this one.
    let injected = Arc::new(Mutex::new(Vec::new()));
    let refine_calls = Arc::new(AtomicUsize::new(0));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.refine_enabled = true;
    builder.refiner = Some((
        RefineBehavior::Delay(Duration::from_millis(250)),
        refine_calls,
    ));
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "refining");

    handle.cancel();

    assert_eq!(recv_state(&states_rx), "idle");
    assert!(
        injected.lock().expect("lock").is_empty(),
        "nothing must be injected once a cancel arrived before the inject commit point"
    );
    assert_no_more_states(&states_rx);

    handle.shutdown();
}

#[test]
fn empty_transcript_notifies_and_injects_nothing() {
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("   ")));
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "idle");

    // Absence assertion: checked directly, not awaited -- there's nothing to
    // wait for here, only a Vec that must still be empty.
    assert!(injected.lock().expect("lock").is_empty());

    // `TranscriptReady` with empty text produces `[NotifyInfo(..)]` after the
    // transition to "idle" is already emitted (see `on_transcribing` in
    // session.rs), so the notice can genuinely still be in flight here.
    // Wait for it explicitly instead of inferring it already happened.
    let (kind, msg) = recv_notice(&notices_rx);
    assert_eq!(kind, "info");
    assert_eq!(msg, "Nothing heard");

    handle.shutdown();
}

#[test]
fn history_entry_recorded_with_raw_and_final_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("history.sqlite3");
    let history = HistoryRepo::open(&db_path).expect("open history db");

    let refine_calls = Arc::new(AtomicUsize::new(0));
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.refine_enabled = true;
    builder.refiner = Some((RefineBehavior::Uppercase, refine_calls));
    builder.injected = injected.clone();
    builder.history = Some(history);
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "refining");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(*injected.lock().expect("lock"), vec!["HELLO WORLD"]);

    handle.shutdown();

    // Re-open the same on-disk database to verify what the worker thread
    // persisted; the write is autocommit and happened-before the "idle"
    // emission above, so it is guaranteed visible here.
    let verify = HistoryRepo::open(&db_path).expect("reopen history db");
    let entries = verify.list(None, 10).expect("list history");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].raw_text, "hello world");
    assert_eq!(entries[0].final_text, "HELLO WORLD");
    assert_eq!(entries[0].engine, "fake-engine");
    assert!(entries[0].duration_ms >= 0);
}

#[test]
fn reload_swaps_deps_between_sessions() {
    let injected_a = Arc::new(Mutex::new(Vec::new()));
    let injected_b = Arc::new(Mutex::new(Vec::new()));
    let begin_opts_b = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder_a = DepsBuilder::new(Ok(transcript("hello world")));
    builder_a.injected = injected_a.clone();
    let deps_a = builder_a.build(hotkey_rx);

    let handle = Runtime::spawn(deps_a, sink);

    // Still idle (no session started yet): the new deps, including a fresh
    // hotkey channel and updated dictionary terms, apply immediately rather
    // than being queued.
    let (hotkey_tx_b, hotkey_rx_b) = unbounded();
    let mut builder_b = DepsBuilder::new(Ok(transcript("second session")));
    builder_b.injected = injected_b.clone();
    builder_b.dictionary_terms = vec!["SQLite".to_string(), "Tauri".to_string()];
    builder_b.begin_opts = begin_opts_b.clone();
    let deps_b = builder_b.build(hotkey_rx_b);
    handle.reload(deps_b);

    hotkey_tx_b
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx_b
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert!(injected_a.lock().expect("lock").is_empty());
    assert_eq!(*injected_b.lock().expect("lock"), vec!["second session"]);

    // The reloaded deps' dictionary terms must reach the STT engine as an
    // `initial_prompt`, proving `WorkerCtx::apply` copies `profiles`
    // just like every other field (not just at `WorkerCtx::new`).
    let opts_b = begin_opts_b.lock().expect("lock");
    assert_eq!(opts_b.len(), 1);
    assert_eq!(opts_b[0].initial_prompt, Some("SQLite, Tauri".to_string()));
    drop(opts_b);

    // The old hotkey channel's receiver was dropped by `reload`; sending on
    // it now simply fails rather than resurrecting the old session.
    let _ = hotkey_tx.send(HotkeyEvent::Pressed {
        binding: BindingId::from(0),
    });

    handle.shutdown();
}

#[test]
fn toggle_drives_a_full_session_in_toggle_mode() {
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (_hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("toggled")));
    builder.mode = DictationMode::Toggle;
    builder.injected = injected.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    handle.toggle();
    assert_eq!(recv_state(&states_rx), "recording");

    handle.toggle();
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(*injected.lock().expect("lock"), vec!["toggled"]);

    handle.shutdown();
}

#[test]
fn audio_frames_are_fed_to_engine_and_partial_reaches_sink() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tx_slot = Arc::new(Mutex::new(None));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.calls = calls.clone();
    builder.partial = Some("partial text".to_string());
    builder.capture_tx_slot = tx_slot.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");

    let tx = capture_tx(&tx_slot);
    tx.send(AudioFrame {
        samples: vec![100; 50],
    })
    .expect("send frame");

    // The frame's rms/partial reaches the sink as a "recording" emission
    // carrying the scripted partial.
    let partial = recv_partial(&states_rx);
    assert_eq!(partial.as_deref(), Some("partial text"));

    // ... and the frame's samples actually reached `engine.feed`.
    let calls = calls.lock().expect("lock");
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, CallRecord::Feed(samples) if samples.len() == 50)),
        "expected engine.feed to have been called with the pushed frame, got {calls:?}"
    );
    drop(calls);

    handle.shutdown();
}

#[test]
fn trailing_frames_are_fed_before_finish_is_called() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tx_slot = Arc::new(Mutex::new(None));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.calls = calls.clone();
    builder.capture_tx_slot = tx_slot.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");

    let tx = capture_tx(&tx_slot);
    // Push frames and release the hotkey back-to-back, without waiting for
    // either frame to be individually processed first: whichever the
    // `select!` loop happens to pick up first (an audio frame via the
    // normal recording-path feed, or the hotkey release triggering
    // `StopCapture`'s trailing-frame drain), every sample must still reach
    // `engine.feed` strictly before `engine.finish()` is called.
    tx.send(AudioFrame {
        samples: vec![1, 2, 3],
    })
    .expect("send frame 1");
    tx.send(AudioFrame {
        samples: vec![4, 5, 6],
    })
    .expect("send frame 2");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");

    // Depending on which the `select!` loop happens to service first, zero,
    // one, or two of the pushed frames may be processed via the normal
    // recording-path feed (each emitting its own extra "recording") before
    // `Released` is picked up — `recv_until` skips those rather than
    // asserting an exact count, since the ordering guarantee under test is
    // about `engine.feed`/`engine.finish()` call order, not the state
    // channel's exact cardinality.
    recv_until(&states_rx, "transcribing");
    recv_until(&states_rx, "injecting");
    recv_until(&states_rx, "idle");

    let calls = calls.lock().expect("lock");
    let finish_index = calls
        .iter()
        .position(|c| *c == CallRecord::Finish)
        .expect("finish() should have been called exactly once");
    let feeds_before_finish: Vec<&CallRecord> = calls[..finish_index].iter().collect();
    assert_eq!(
        feeds_before_finish.len(),
        2,
        "both frames should have been fed before finish(), got {calls:?}"
    );
    assert!(feeds_before_finish
        .iter()
        .any(|c| matches!(c, CallRecord::Feed(s) if s == &vec![1i16, 2, 3])));
    assert!(feeds_before_finish
        .iter()
        .any(|c| matches!(c, CallRecord::Feed(s) if s == &vec![4i16, 5, 6])));
    drop(calls);

    handle.shutdown();
}

#[test]
fn silence_timeout_stops_recording_without_hotkey_release() {
    let tx_slot = Arc::new(Mutex::new(None));
    let (sink, states_rx, _notices_rx) = fake_sink();

    let (_hotkey_tx, hotkey_rx) = unbounded();
    let mut builder = DepsBuilder::new(Ok(transcript("hello world")));
    builder.silence = Some(Duration::from_millis(30));
    builder.capture_tx_slot = tx_slot.clone();
    let deps = builder.build(hotkey_rx);

    let handle = Runtime::spawn(deps, sink);

    // Drive recording via `toggle` rather than a hotkey channel, since no
    // release is ever going to be sent in this test.
    handle.toggle();
    assert_eq!(recv_state(&states_rx), "recording");

    let tx = capture_tx(&tx_slot);
    // All-zero samples are silence (rms 0.0). Spaced 10ms apart in real
    // time so the 30ms silence hold genuinely elapses; comfortably more
    // frames than needed, for margin.
    for _ in 0..10 {
        tx.send(AudioFrame {
            samples: vec![0i16; 10],
        })
        .expect("send silent frame");
        thread::sleep(Duration::from_millis(10));
    }

    // No hotkey release, no manual cancel: only the silence timeout can
    // have driven this transition.
    recv_until(&states_rx, "transcribing");
    recv_until(&states_rx, "injecting");
    recv_until(&states_rx, "idle");

    handle.shutdown();
}

/// Pins the whole point of Task 16: which profile a press resolves to must actually be driven by
/// its `BindingId`, not just "whichever engine happened to load first". The two profiles are
/// deliberately given *different* output text (Russian vs. English) rather than the same text --
/// a routing bug that always used binding 0's profile, or that shared one `Session`/engine across
/// bindings, would still pass a test whose profiles produced identical text. Both directions are
/// asserted (press 1 then 0, not just 0 then 1) so a bug that only gets the *first* press right
/// (e.g. one that latches onto whichever profile started the worker) cannot slip through.
#[test]
fn each_hotkey_dictates_with_its_own_profile() {
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();
    let (hotkey_tx, hotkey_rx) = unbounded();

    let profiles = registry_with(vec![
        (test_profile("ru"), profile_deps_with_transcript("привет")),
        (test_profile("en"), profile_deps_with_transcript("hello")),
    ]);

    let deps = RuntimeDeps {
        mode: DictationMode::PushToTalk,
        silence: None,
        profiles,
        injector: Box::new(FakeInjector {
            injected: injected.clone(),
            fail: false,
        }),
        rules: Vec::new(),
        snippets: Vec::new(),
        history: None,
        capture_device: None,
        capture: Box::new(FakeCaptureBackend {
            tx_slot: Arc::new(Mutex::new(None)),
        }),
        hotkey_rx,
        vad_sensitivity: 0.5,
        refine_timeout: Duration::from_secs(1),
    };

    let handle = Runtime::spawn(deps, sink);

    press_and_release(&hotkey_tx, &states_rx, BindingId::from(1));
    assert_eq!(
        injected.lock().expect("lock").last(),
        Some(&"hello".to_string()),
        "binding 1 must dictate with the \"en\" profile, loaded lazily on this first press"
    );

    press_and_release(&hotkey_tx, &states_rx, BindingId::from(0));
    assert_eq!(
        injected.lock().expect("lock").last(),
        Some(&"привет".to_string()),
        "binding 0 must dictate with the \"ru\" profile, not stay latched on binding 1's"
    );

    handle.shutdown();
}

/// Pins the `Session::new`-at-press-time fix the task brief calls out: `refine_enabled` is a
/// per-profile value now, so a session started by one binding must not carry over whatever the
/// *previous* binding's flag was. Binding 0's profile has refinement off, binding 1's has it on
/// (with a real, distinguishable refiner); pressing 0 then 1 proves the flag is read fresh at
/// each press rather than fixed for the worker's whole lifetime.
#[test]
fn each_profile_applies_its_own_refine_enabled_flag_at_press_time() {
    let injected = Arc::new(Mutex::new(Vec::new()));
    let refine_calls = Arc::new(AtomicUsize::new(0));
    let (sink, states_rx, _notices_rx) = fake_sink();
    let (hotkey_tx, hotkey_rx) = unbounded();

    let off_deps = profile_deps_with_transcript("plain");

    let mut on_deps = profile_deps_with_transcript("fancy");
    on_deps.refine_enabled = true;
    on_deps.refiner = Some(Arc::new(FakeRefiner {
        behavior: RefineBehavior::Uppercase,
        calls: refine_calls.clone(),
    }));

    let profiles = registry_with(vec![
        (test_profile("off"), off_deps),
        (test_profile("on"), on_deps),
    ]);

    let deps = RuntimeDeps {
        mode: DictationMode::PushToTalk,
        silence: None,
        profiles,
        injector: Box::new(FakeInjector {
            injected: injected.clone(),
            fail: false,
        }),
        rules: Vec::new(),
        snippets: Vec::new(),
        history: None,
        capture_device: None,
        capture: Box::new(FakeCaptureBackend {
            tx_slot: Arc::new(Mutex::new(None)),
        }),
        hotkey_rx,
        vad_sensitivity: 0.5,
        refine_timeout: Duration::from_secs(1),
    };

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(0),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");
    assert_eq!(
        injected.lock().expect("lock").last(),
        Some(&"plain".to_string())
    );
    assert_eq!(
        refine_calls.load(Ordering::SeqCst),
        0,
        "binding 0's profile has refinement off"
    );

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(1),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");
    hotkey_tx
        .send(HotkeyEvent::Released {
            binding: BindingId::from(1),
        })
        .expect("send released");
    assert_eq!(recv_state(&states_rx), "transcribing");
    assert_eq!(recv_state(&states_rx), "refining");
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");
    assert_eq!(
        injected.lock().expect("lock").last(),
        Some(&"FANCY".to_string())
    );
    assert_eq!(
        refine_calls.load(Ordering::SeqCst),
        1,
        "binding 1's profile has refinement on and must actually run it"
    );

    handle.shutdown();
}

/// Pins the `session.state() == State::Idle` guard in `handle_hotkey_pressed` (see its module doc
/// comment): a binding is only ever (re)selected while the session is `Idle`. `Toggle` mode's
/// second press -- the one that stops recording -- need not come from the same binding that
/// started it (a user could, in principle, hit a different profile's chord mid-utterance); the
/// guard is what makes that press stop the in-flight session instead of being treated as a fresh
/// selection. Without it, `start_session_for` would run again, reconstructing `Session` back to
/// `Idle` and making `on_idle(HotkeyPressed)` start a *new* recording rather than stop the old
/// one -- so `Toggle`-mode dictation could never be stopped by its own hotkey, and the
/// overwritten `ctx.active_capture` would drop the first press's capture handle without
/// `stop()`, leaking its audio stream. Two profiles with deliberately different transcripts:
/// press binding 0 to start, then press binding 1 while `Recording`. The correct outcome is a
/// single `"transcribing"` emission (the *same* session stopping) and an injected transcript
/// from binding 0's profile -- binding 1 must never be consulted.
#[test]
fn second_binding_pressed_mid_recording_in_toggle_mode_stops_binding_zeros_session() {
    let injected = Arc::new(Mutex::new(Vec::new()));
    let (sink, states_rx, _notices_rx) = fake_sink();
    let (hotkey_tx, hotkey_rx) = unbounded();

    let profiles = registry_with(vec![
        (test_profile("zero"), profile_deps_with_transcript("first")),
        (test_profile("one"), profile_deps_with_transcript("second")),
    ]);

    let deps = RuntimeDeps {
        mode: DictationMode::Toggle,
        silence: None,
        profiles,
        injector: Box::new(FakeInjector {
            injected: injected.clone(),
            fail: false,
        }),
        rules: Vec::new(),
        snippets: Vec::new(),
        history: None,
        capture_device: None,
        capture: Box::new(FakeCaptureBackend {
            tx_slot: Arc::new(Mutex::new(None)),
        }),
        hotkey_rx,
        vad_sensitivity: 0.5,
        refine_timeout: Duration::from_secs(1),
    };

    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(0),
        })
        .expect("send pressed");
    assert_eq!(recv_state(&states_rx), "recording");

    // Binding 1, pressed while binding 0's session is `Recording` -- not `Idle` -- must not be
    // consulted at all (see `handle_hotkey_pressed`'s doc comment): this press just stops the
    // in-flight session.
    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(1),
        })
        .expect("send pressed");
    assert_eq!(
        recv_state(&states_rx),
        "transcribing",
        "the second press must stop binding 0's session, not rebuild it and start recording \
         again with binding 1's profile"
    );
    assert_eq!(recv_state(&states_rx), "injecting");
    assert_eq!(recv_state(&states_rx), "idle");

    assert_eq!(
        injected.lock().expect("lock").last(),
        Some(&"first".to_string()),
        "the utterance must still transcribe with binding 0's profile -- binding 1 was pressed \
         mid-recording and must never be consulted"
    );

    assert_no_more_states(&states_rx);

    handle.shutdown();
}

/// `ProfileRegistry::deps_for` returning `None` means only one thing: no binding with that id
/// exists (see its doc comment). In production this is unreachable -- `create_source` is only
/// ever handed specs for bindings the registry also has entries for -- but the worker still
/// checks explicitly (`handle_hotkey_pressed`) rather than assuming, and this pins that a press
/// for an unknown id is dropped silently: no session starts, nothing crashes.
#[test]
fn pressing_an_unregistered_binding_starts_no_session() {
    let (sink, states_rx, _notices_rx) = fake_sink();
    let (hotkey_tx, hotkey_rx) = unbounded();

    let builder = DepsBuilder::new(Ok(transcript("hello world")));
    let deps = builder.build(hotkey_rx); // single-profile registry: only binding 0 exists.
    let handle = Runtime::spawn(deps, sink);

    hotkey_tx
        .send(HotkeyEvent::Pressed {
            binding: BindingId::from(1),
        })
        .expect("send pressed");
    assert_no_more_states(&states_rx);

    handle.shutdown();
}
