//! Dictation runtime orchestrator: wires the pure [`Session`] state machine
//! to real adapters (audio capture, STT, refinement, injection, history) on
//! a single dedicated worker thread.
//!
//! ## Design
//!
//! [`Runtime::spawn`] starts one worker thread that owns a `crossbeam`
//! `select!` loop over three channels: hotkey events, audio frames, and
//! control messages ([`RuntimeHandle::cancel`]/`toggle`/`reload`/`shutdown`).
//! Everything the loop needs — the [`Session`], the adapters, and a handful
//! of small pieces of in-flight state (the active capture handle, the
//! silence detector, the current utterance's raw text) — lives entirely on
//! that thread; nothing here is shared across threads except through the
//! channels and the `Arc<dyn EventSink>` the caller supplies. This satisfies
//! the same "Runtime owns everything" shape the design brief sketches,
//! without needing a persistent `Runtime` struct instance: the spawned
//! closure's captured state *is* that ownership.
//!
//! `Session::handle` is pure and total, so this module's job is simply: feed
//! it events, and execute the effects it returns. [`dispatch`] does the
//! former (and emits the resulting phase to the [`EventSink`]); `run_effect`
//! does the latter. Because every step here is synchronous on one thread,
//! executing an effect that itself produces a new event (e.g. finishing
//! transcription, refining, injecting) simply calls back into `dispatch`,
//! forming a straight-line call chain from `HotkeyPressed` down to `Idle`
//! for a single utterance — there is no queue or scheduler to reason about.
//!
//! ## The snippet short-circuit
//!
//! `Session` has no notion of voice snippets — it only knows whether
//! refinement is enabled. So the snippet check happens here, right after
//! `engine.finish()`: dictionary rules are applied to the raw transcript,
//! then [`match_snippet`] is tried on the result. A hit *replaces* the
//! outgoing text with the snippet body and is remembered (in
//! [`PendingUtterance::snippet_hit`]) for the rest of this utterance. If
//! refinement is enabled, `Session` still emits `Effect::Refine` (it doesn't
//! know better) — but when this module executes that effect, a snippet hit
//! makes it feed `Event::RefineDone(body)` straight back into the session
//! without ever calling `ctx.refiner`. This is the one and only bypass of
//! the refiner, and it keeps the "the refiner was never called" guarantee
//! regardless of the user's refine-enabled setting.
//!
//! ## Cancel commit points
//!
//! Because a whole utterance unwinds as one straight-line call chain (see
//! above), a naive implementation would only notice a `Cancel` *after* that
//! chain finished — including after injecting. `Session` explicitly models
//! `CancelRequested` from `Transcribing`/`Refining`/`Injecting` all going to
//! `Idle`, so this module has to actually check for a pending cancel at the
//! two points where the chain would otherwise commit to using a result it
//! blocked on: right after `engine.finish()` returns (before dispatching
//! `TranscriptReady`/`TranscriptFailed`) and right after a refine call
//! resolves — successfully, by failure, or by timeout (before dispatching
//! `RefineDone`/`RefineFailed`, i.e. before the `Inject` effect that
//! transition would produce ever runs). [`check_for_cancel`] does this: a
//! non-blocking drain of the control channel that, if it finds a `Cancel`,
//! makes the caller feed `Event::CancelRequested` instead of the pending
//! event, abandoning the transcript/refine result entirely — nothing is
//! injected. Any *other* control message found during that drain (a
//! `Reload`, a `Toggle`, a `Shutdown`) is not lost: it's queued onto
//! `WorkerCtx::pending_control` and replayed at the top of the main loop
//! once the current utterance has settled to `Idle` or back to `Recording`,
//! preserving arrival order. `engine.finish()` and an in-flight refine call
//! themselves stay blocking/uninterruptible — that's an accepted trade
//! (whisper inference in particular can't be aborted mid-call) — the
//! guarantee this establishes is narrower and sufficient: nothing is
//! *injected* once a cancel has arrived before the corresponding commit
//! point.
//!
//! ## The capture test seam
//!
//! `utter_audio::Capture` touches real audio hardware and is deliberately
//! `!Send` (it must be created, used, and dropped on one thread). Rather
//! than hardcode it, this module depends on the small [`CaptureBackend`] /
//! [`ActiveCapture`] traits; [`RealCaptureBackend`] is the thin production
//! adapter, and tests substitute a scripted fake. `CaptureBackend` itself
//! must be `Send` (it crosses into the worker thread via [`RuntimeDeps`]),
//! but `ActiveCapture` does not: the live capture handle is created *on* the
//! worker thread by a `Send` backend and never leaves it, so `Capture`'s
//! `!Send`-ness is never in tension with this design.

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{select, unbounded, Receiver, Sender};

use utter_audio::{rms_level, AudioFrame, SilenceDetector};
use utter_core::{
    DictationMode, Effect, Event, Session, State, SttEngine, TextInjector, TextRefiner, Tone,
    TranscribeOptions, Transcript,
};
use utter_inject::HotkeyEvent;
use utter_refine::{apply_rules, match_snippet, ReplaceRule, Snippet};
use utter_store::{HistoryRepo, NewEntry};

/// Sink the runtime reports dictation phase changes and user-facing notices
/// to. `state` matches the `DictationPhase` names in [`crate::events`]
/// (`"idle"`, `"recording"`, `"transcribing"`, `"refining"`, `"injecting"`);
/// `kind` matches `NoticeKind` (`"info"`, `"warning"`, `"error"`).
pub trait EventSink: Send + Sync {
    fn emit_state(&self, state: &str, level: f32, partial: Option<&str>);
    fn notify(&self, kind: &str, msg: &str);
}

/// A handle to the dictation history store. Currently just the concrete
/// [`HistoryRepo`]: `HistoryRepo::add` takes `&self` (rusqlite's
/// `Connection` provides its own interior mutability), so the single-owner
/// worker thread that holds it needs no extra synchronization wrapper.
pub type HistoryHandle = HistoryRepo;

/// A live, in-progress audio capture, created by a [`CaptureBackend`].
///
/// Not `Send`: it is created by the worker thread and only ever used and
/// dropped there (mirroring `utter_audio::Capture`'s own contract), so it
/// never needs to cross a thread boundary.
pub trait ActiveCapture {
    /// Stops capture, flushing any trailing buffered audio to the channel
    /// given to [`CaptureBackend::start`] before returning.
    fn stop(self: Box<Self>);
}

/// Starts microphone capture. The seam between this module and real audio
/// hardware: production code uses [`RealCaptureBackend`], tests substitute a
/// scripted fake that never touches a real device.
pub trait CaptureBackend: Send {
    fn start(
        &self,
        device: Option<&str>,
        tx: Sender<AudioFrame>,
    ) -> Result<Box<dyn ActiveCapture>, String>;
}

/// Production [`CaptureBackend`]: starts real microphone capture via
/// [`utter_audio::Capture`].
pub struct RealCaptureBackend;

impl CaptureBackend for RealCaptureBackend {
    fn start(
        &self,
        device: Option<&str>,
        tx: Sender<AudioFrame>,
    ) -> Result<Box<dyn ActiveCapture>, String> {
        utter_audio::Capture::start(device, tx)
            .map(|capture| Box::new(RealActiveCapture(capture)) as Box<dyn ActiveCapture>)
            .map_err(|e| e.to_string())
    }
}

struct RealActiveCapture(utter_audio::Capture);

impl ActiveCapture for RealActiveCapture {
    fn stop(self: Box<Self>) {
        self.0.stop();
    }
}

/// Everything [`Runtime::spawn`] needs to drive one dictation session, and
/// everything [`RuntimeHandle::reload`] can swap out for the next one.
pub struct RuntimeDeps {
    pub mode: DictationMode,
    pub refine_enabled: bool,
    /// Continuous-silence duration that auto-stops recording; `None`
    /// disables the silence timeout entirely.
    pub silence: Option<Duration>,
    pub engine: Box<dyn SttEngine>,
    pub refiner: Option<Box<dyn TextRefiner>>,
    pub injector: Box<dyn TextInjector>,
    pub rules: Vec<ReplaceRule>,
    pub snippets: Vec<Snippet>,
    pub history: Option<HistoryHandle>,
    pub capture_device: Option<String>,
    /// Audio capture backend; [`RealCaptureBackend`] in production.
    pub capture: Box<dyn CaptureBackend>,
    /// The hotkey source's event stream. Owning and re-registering the
    /// actual `HotkeySource` background thread is the app boot path's job
    /// (a later task); this module only ever consumes a `Receiver`, which
    /// keeps it trivially testable — tests drive it with a plain channel.
    pub hotkey_rx: Receiver<HotkeyEvent>,
    pub vad_sensitivity: f32,
    pub refine_timeout: Duration,
    pub tone: Tone,
    pub language: Option<String>,
    /// Recorded on each history entry (e.g. `"whisper"`, `"sherpa"`, `"cloud"`).
    pub engine_label: String,
    /// User-configured dictionary terms (proper nouns, jargon, ...), fed to
    /// the active engine as a recognition hint so it is more likely to get
    /// them right in the first place — distinct from `rules`, which rewrite
    /// the transcript after the fact. How they are delivered is
    /// engine-specific: whisper.cpp takes them as an `initial_prompt` on
    /// every `begin()`, while sherpa-onnx takes them as hotwords fixed at
    /// model load, which is why changing them rebuilds the runtime (see
    /// `runtime_boot::build_sherpa`) rather than being re-supplied per call.
    pub dictionary_terms: Vec<String>,
}

/// Messages sent from a [`RuntimeHandle`] to the worker thread.
enum ControlMsg {
    Cancel,
    Toggle,
    Reload(Box<RuntimeDeps>),
    Shutdown,
}

/// A running dictation runtime's control handle. Cheap to hold onto; every
/// method besides [`shutdown`](RuntimeHandle::shutdown) just posts a message
/// to the worker thread and returns immediately.
pub struct RuntimeHandle {
    control_tx: Sender<ControlMsg>,
    worker: Option<JoinHandle<()>>,
}

impl RuntimeHandle {
    /// Cancels the in-flight session, if any (no-op when idle). Nothing is
    /// injected for a cancelled session.
    pub fn cancel(&self) {
        let _ = self.control_tx.send(ControlMsg::Cancel);
    }

    /// Drives the session as if the hotkey had just been pressed (from
    /// idle) or as if this mode's "stop" action had just occurred (from
    /// recording); ignored while transcribing/refining/injecting. Lets a UI
    /// affordance (tray menu, HUD button) trigger dictation without a real
    /// hotkey chord.
    pub fn toggle(&self) {
        let _ = self.control_tx.send(ControlMsg::Toggle);
    }

    /// Swaps in new dependencies. If a session is currently recording, it is
    /// cancelled first (nothing injected) rather than queuing the reload —
    /// applying a new engine/refiner/injector mid-utterance would mix state
    /// from two configurations, and settings changes are rare enough that
    /// losing an in-flight recording is an acceptable, clearly-signposted
    /// trade for correctness. Idle sessions swap immediately.
    pub fn reload(&self, deps: RuntimeDeps) {
        let _ = self.control_tx.send(ControlMsg::Reload(Box::new(deps)));
    }

    /// Shuts the worker thread down and waits for it to exit.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let _ = self.control_tx.send(ControlMsg::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for RuntimeHandle {
    /// Safety net for callers that drop the handle without calling
    /// `shutdown`: still asks the worker to exit and waits for it, so the
    /// thread never outlives its handle unnoticed. Idempotent with an
    /// explicit `shutdown` call (`worker.take()` guards the double join).
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

/// Namespace for [`Runtime::spawn`]. There is no persistent `Runtime`
/// instance to hold onto — see the module doc comment for why the worker
/// thread's own captured state fills that role instead.
pub struct Runtime;

impl Runtime {
    /// Spawns the worker thread and returns a handle to control it.
    pub fn spawn(deps: RuntimeDeps, sink: Arc<dyn EventSink>) -> RuntimeHandle {
        let (control_tx, control_rx) = unbounded();
        let worker = thread::Builder::new()
            .name("utter-dictation".to_string())
            .spawn(move || worker_loop(deps, sink, control_rx))
            .expect("failed to spawn the utter-dictation worker thread");

        RuntimeHandle {
            control_tx,
            worker: Some(worker),
        }
    }
}

/// The current utterance's raw transcript and whether a voice snippet
/// replaced it, kept around from the moment `engine.finish()` succeeds until
/// injection completes (successfully or not) so `run_refine` can skip the
/// refiner and `record_history` can log both the raw and final text.
struct PendingUtterance {
    raw: String,
    snippet_hit: bool,
}

/// Everything the worker thread owns for the lifetime of the runtime:
/// swappable adapters/config (from the latest [`RuntimeDeps`]) plus
/// in-flight session state that survives across `select!` iterations.
struct WorkerCtx {
    engine: Box<dyn SttEngine>,
    refiner: Option<Arc<dyn TextRefiner>>,
    injector: Box<dyn TextInjector>,
    rules: Vec<ReplaceRule>,
    snippets: Vec<Snippet>,
    history: Option<HistoryHandle>,
    capture_device: Option<String>,
    capture: Box<dyn CaptureBackend>,
    hotkey_rx: Receiver<HotkeyEvent>,
    vad_sensitivity: f32,
    silence: Option<Duration>,
    refine_timeout: Duration,
    tone: Tone,
    language: Option<String>,
    engine_label: String,
    mode: DictationMode,
    dictionary_terms: Vec<String>,

    sink: Arc<dyn EventSink>,
    // Kept alive for the runtime's whole lifetime (even with no capture
    // active) so the channel never disconnects: a disconnected receiver
    // would make every `select!` iteration see it as immediately "ready"
    // with an `Err`, spinning the worker thread at 100% CPU forever.
    audio_tx: Sender<AudioFrame>,
    audio_rx: Receiver<AudioFrame>,

    active_capture: Option<Box<dyn ActiveCapture>>,
    silence_detector: Option<SilenceDetector>,
    session_started_at: Option<Instant>,
    pending: Option<PendingUtterance>,

    /// The control channel, moved in here (rather than kept as a separate
    /// `worker_loop` local) so the cancel commit points — deep inside the
    /// `dispatch` call chain, not at the top of the loop — can drain it too.
    control_rx: Receiver<ControlMsg>,
    /// Control messages pulled out of `control_rx` by [`check_for_cancel`]
    /// that turned out not to be the `Cancel` it was looking for. Replayed,
    /// in order, at the top of the main loop before the next blocking
    /// `select!` — see the module doc comment ("Cancel commit points").
    pending_control: VecDeque<ControlMsg>,
}

impl WorkerCtx {
    fn new(
        deps: RuntimeDeps,
        sink: Arc<dyn EventSink>,
        audio_tx: Sender<AudioFrame>,
        audio_rx: Receiver<AudioFrame>,
        control_rx: Receiver<ControlMsg>,
    ) -> Self {
        let mut ctx = Self {
            engine: deps.engine,
            refiner: None,
            injector: deps.injector,
            rules: deps.rules,
            snippets: deps.snippets,
            history: deps.history,
            capture_device: deps.capture_device,
            capture: deps.capture,
            hotkey_rx: deps.hotkey_rx,
            vad_sensitivity: deps.vad_sensitivity,
            silence: deps.silence,
            refine_timeout: deps.refine_timeout,
            tone: deps.tone,
            language: deps.language,
            engine_label: deps.engine_label,
            mode: deps.mode,
            dictionary_terms: deps.dictionary_terms,
            sink,
            audio_tx,
            audio_rx,
            active_capture: None,
            silence_detector: None,
            session_started_at: None,
            pending: None,
            control_rx,
            pending_control: VecDeque::new(),
        };
        ctx.refiner = deps.refiner.map(Arc::from);
        ctx
    }

    /// Swaps in newly-reloaded config/adapters. Runtime-owned in-flight
    /// state (`sink`, the audio channel, `active_capture`, `pending`, ...)
    /// is left untouched — by the time this runs the session is idle (see
    /// `reload`), so there is none in flight to preserve or discard.
    fn apply(&mut self, deps: RuntimeDeps) {
        self.engine = deps.engine;
        self.refiner = deps.refiner.map(Arc::from);
        self.injector = deps.injector;
        self.rules = deps.rules;
        self.snippets = deps.snippets;
        self.history = deps.history;
        self.capture_device = deps.capture_device;
        self.capture = deps.capture;
        self.hotkey_rx = deps.hotkey_rx;
        self.vad_sensitivity = deps.vad_sensitivity;
        self.silence = deps.silence;
        self.refine_timeout = deps.refine_timeout;
        self.tone = deps.tone;
        self.language = deps.language;
        self.engine_label = deps.engine_label;
        self.mode = deps.mode;
        self.dictionary_terms = deps.dictionary_terms;
    }
}

fn phase_str(state: State) -> &'static str {
    match state {
        State::Idle => "idle",
        State::Recording => "recording",
        State::Transcribing => "transcribing",
        State::Refining => "refining",
        State::Injecting => "injecting",
    }
}

fn worker_loop(deps: RuntimeDeps, sink: Arc<dyn EventSink>, control_rx: Receiver<ControlMsg>) {
    let (audio_tx, audio_rx) = unbounded::<AudioFrame>();
    let mut session = Session::new(deps.mode, deps.refine_enabled);
    let mut ctx = WorkerCtx::new(deps, sink, audio_tx, audio_rx, control_rx);

    loop {
        // Replay anything a cancel-commit-point drain pulled out of
        // `control_rx` and deferred (see `check_for_cancel`) before blocking
        // on `select!` again, so those messages are never lost and stay in
        // arrival order.
        if let Some(msg) = ctx.pending_control.pop_front() {
            if let LoopAction::Exit = handle_control(&mut session, &mut ctx, msg) {
                cleanup_and_exit(&mut ctx);
                return;
            }
            continue;
        }

        select! {
            recv(ctx.hotkey_rx) -> msg => match msg {
                // `binding` is ignored here: this runtime registers a
                // single hotkey today, so every event belongs to it.
                // Routing per binding (e.g. to a language profile) is a
                // later step's job.
                Ok(HotkeyEvent::Pressed { .. }) => {
                    dispatch(&mut session, &mut ctx, Event::HotkeyPressed)
                }
                Ok(HotkeyEvent::Released { .. }) => {
                    dispatch(&mut session, &mut ctx, Event::HotkeyReleased)
                }
                // Hotkey source gone (e.g. mid re-registration); nothing to
                // do until a `reload` supplies a fresh receiver.
                Err(_) => {}
            },
            recv(ctx.audio_rx) -> msg => {
                if let Ok(frame) = msg {
                    handle_audio_frame(&mut session, &mut ctx, frame);
                }
            },
            recv(ctx.control_rx) -> msg => match msg {
                Ok(msg) => {
                    if let LoopAction::Exit = handle_control(&mut session, &mut ctx, msg) {
                        cleanup_and_exit(&mut ctx);
                        return;
                    }
                }
                Err(_) => {
                    cleanup_and_exit(&mut ctx);
                    return;
                }
            },
        }
    }
}

/// Whether the main loop should keep going after a [`ControlMsg`].
enum LoopAction {
    Continue,
    Exit,
}

fn handle_control(session: &mut Session, ctx: &mut WorkerCtx, msg: ControlMsg) -> LoopAction {
    match msg {
        ControlMsg::Cancel => {
            dispatch(session, ctx, Event::CancelRequested);
            LoopAction::Continue
        }
        ControlMsg::Toggle => {
            handle_toggle(session, ctx);
            LoopAction::Continue
        }
        ControlMsg::Reload(new_deps) => {
            reload(session, ctx, *new_deps);
            LoopAction::Continue
        }
        ControlMsg::Shutdown => LoopAction::Exit,
    }
}

fn cleanup_and_exit(ctx: &mut WorkerCtx) {
    if let Some(active) = ctx.active_capture.take() {
        active.stop();
    }
}

/// Non-blocking drain of the control channel, used at the two cancel commit
/// points (see the module doc comment). Returns whether a `Cancel` was
/// found; any other message found along the way is preserved in
/// `ctx.pending_control` for the main loop to replay, in order, rather than
/// being silently dropped.
fn check_for_cancel(ctx: &mut WorkerCtx) -> bool {
    let mut cancelled = false;
    while let Ok(msg) = ctx.control_rx.try_recv() {
        if matches!(msg, ControlMsg::Cancel) {
            cancelled = true;
        } else {
            ctx.pending_control.push_back(msg);
        }
    }
    cancelled
}

/// Feeds `event` into the session, emits the resulting phase, then executes
/// every effect the transition produced, in order. Effects that themselves
/// complete synchronously and produce a further event (finishing
/// transcription, refining, injecting) call back into `dispatch` directly,
/// so a whole utterance unwinds as one straight-line call chain.
fn dispatch(session: &mut Session, ctx: &mut WorkerCtx, event: Event) {
    let effects = session.handle(event);
    ctx.sink.emit_state(phase_str(session.state()), 0.0, None);

    for effect in effects {
        run_effect(session, ctx, effect);
    }

    if session.state() == State::Idle {
        ctx.pending = None;
        ctx.session_started_at = None;
        ctx.silence_detector = None;
    }
}

fn run_effect(session: &mut Session, ctx: &mut WorkerCtx, effect: Effect) {
    match effect {
        Effect::StartCapture => start_capture(ctx),
        Effect::StopCapture => stop_capture_and_maybe_transcribe(session, ctx),
        Effect::Refine(t) => run_refine(session, ctx, t),
        Effect::Inject(text) => run_inject(session, ctx, text),
        Effect::NotifyError(msg) => ctx.sink.notify("error", &msg),
        Effect::NotifyInfo(msg) => ctx.sink.notify("info", &msg),
    }
}

fn start_capture(ctx: &mut WorkerCtx) {
    ctx.session_started_at = Some(Instant::now());
    ctx.silence_detector = ctx
        .silence
        .map(|hold| SilenceDetector::new(ctx.vad_sensitivity, hold));

    let initial_prompt = if ctx.dictionary_terms.is_empty() {
        None
    } else {
        Some(ctx.dictionary_terms.join(", "))
    };
    let opts = TranscribeOptions {
        language: ctx.language.clone(),
        initial_prompt,
    };
    // `Session` has no event for "capture failed to start" (see the module
    // doc comment): the cleanest recovery available here is to notify and
    // leave the session in `Recording`. A subsequent stop (hotkey
    // release/toggle/silence/cancel) will run `StopCapture` as normal; with
    // no audio ever fed, `engine.finish()` on an un-begun engine is expected
    // to error, which naturally routes to `TranscriptFailed` and back to
    // `Idle` — self-healing without `Session` needing to know about it.
    if let Err(e) = ctx.engine.begin(&opts) {
        ctx.sink
            .notify("error", &format!("failed to start transcription: {e}"));
        return;
    }

    match ctx
        .capture
        .start(ctx.capture_device.as_deref(), ctx.audio_tx.clone())
    {
        Ok(active) => ctx.active_capture = Some(active),
        Err(e) => ctx
            .sink
            .notify("error", &format!("failed to start audio capture: {e}")),
    }
}

fn handle_audio_frame(session: &mut Session, ctx: &mut WorkerCtx, frame: AudioFrame) {
    if session.state() != State::Recording {
        // Stray frame after capture already stopped (e.g. one last buffered
        // callback firing before the stream handle was dropped); discard.
        return;
    }

    let level = rms_level(&frame.samples);
    let partial = match ctx.engine.feed(&frame.samples) {
        Ok(partial) => partial,
        Err(e) => {
            ctx.sink
                .notify("warning", &format!("speech engine error: {e}"));
            None
        }
    };
    ctx.sink.emit_state("recording", level, partial.as_deref());

    let silence_fired = ctx
        .silence_detector
        .as_mut()
        .is_some_and(|detector| detector.observe(level, Instant::now()));
    if silence_fired {
        dispatch(session, ctx, Event::SilenceTimeout);
    }
}

/// Executes `Effect::StopCapture`: stops the active capture (flushing
/// trailing audio into the channel), drains whatever is now sitting there,
/// and — only if the session actually landed in `Transcribing` (as opposed
/// to `Idle`, e.g. a cancel raced the same effect) — feeds the trailing
/// audio to the engine and runs it to completion.
fn stop_capture_and_maybe_transcribe(session: &mut Session, ctx: &mut WorkerCtx) {
    if let Some(active) = ctx.active_capture.take() {
        active.stop();
    }
    ctx.silence_detector = None;

    let mut trailing = Vec::new();
    while let Ok(frame) = ctx.audio_rx.try_recv() {
        trailing.push(frame);
    }

    if session.state() != State::Transcribing {
        // Cancelled (or superseded by a reload): discard trailing audio,
        // never call finish(), nothing gets injected.
        return;
    }

    for frame in &trailing {
        if let Err(e) = ctx.engine.feed(&frame.samples) {
            ctx.sink.notify(
                "warning",
                &format!("speech engine error while flushing: {e}"),
            );
        }
    }

    let result = ctx.engine.finish();

    // Commit point 1/2 (see the module doc comment): a `Cancel` that arrived
    // any time up to and including while `finish()` was blocking must win
    // over the transcript it produced — feed `CancelRequested` instead and
    // never dispatch the pending result at all, regardless of whether it
    // was a success or a failure.
    if check_for_cancel(ctx) {
        dispatch(session, ctx, Event::CancelRequested);
        return;
    }

    match result {
        Ok(t) => {
            let ruled = apply_rules(&t.text, &ctx.rules);
            let (final_text, snippet_hit) = match match_snippet(&ruled, &ctx.snippets) {
                Some(snippet) => (snippet.body.clone(), true),
                None => (ruled, false),
            };
            ctx.pending = Some(PendingUtterance {
                raw: t.text.clone(),
                snippet_hit,
            });
            dispatch(
                session,
                ctx,
                Event::TranscriptReady(Transcript {
                    text: final_text,
                    language: t.language,
                }),
            );
        }
        Err(e) => dispatch(session, ctx, Event::TranscriptFailed(e.to_string())),
    }
}

fn run_refine(session: &mut Session, ctx: &mut WorkerCtx, t: Transcript) {
    let snippet_hit = ctx.pending.as_ref().is_some_and(|p| p.snippet_hit);

    let event = if snippet_hit {
        // The one and only refiner bypass: see the module doc comment.
        Event::RefineDone(t.text)
    } else {
        match &ctx.refiner {
            Some(refiner) => {
                match refine_with_timeout(
                    refiner.clone(),
                    t.text.clone(),
                    ctx.tone,
                    ctx.refine_timeout,
                ) {
                    Ok(text) => Event::RefineDone(text),
                    Err(reason) => Event::RefineFailed {
                        raw: t.text,
                        reason,
                    },
                }
            }
            None => Event::RefineFailed {
                raw: t.text,
                reason: "no refiner configured".to_string(),
            },
        }
    };

    // Commit point 2/2 (see the module doc comment): a `Cancel` queued
    // while the refine call (or, for a snippet hit, the essentially
    // instantaneous synchronous bypass above) was in flight must win over
    // injecting `event`'s text — abandon it entirely rather than
    // dispatching it, so the `Inject` effect it would produce never runs.
    if check_for_cancel(ctx) {
        dispatch(session, ctx, Event::CancelRequested);
        return;
    }

    dispatch(session, ctx, event);
}

/// Runs `refiner.refine` on a detached thread and races it against
/// `timeout`. A plain (non-scoped) thread is deliberate: `std::thread::scope`
/// would block this call until the spawned thread actually finishes, which
/// defeats the purpose of a timeout if the refiner call itself hangs far
/// longer than `timeout`. A detached thread lets the worker move on the
/// instant the timeout elapses; the abandoned call finishes in the
/// background and its result (sent into a channel nobody is receiving from
/// anymore) is silently dropped.
///
/// Caveat: this only bounds how long *this call* waits, not how long the
/// spawned thread lives. It relies on `refiner.refine` itself eventually
/// returning (e.g. via its own HTTP client timeout) — a `TextRefiner` impl
/// with no internal timeout of its own, racing against a network call that
/// simply hangs forever, would leak one thread per such call.
fn refine_with_timeout(
    refiner: Arc<dyn TextRefiner>,
    text: String,
    tone: Tone,
    timeout: Duration,
) -> Result<String, String> {
    let (tx, rx) = crossbeam_channel::bounded(1);

    thread::spawn(move || {
        let result = refiner.refine(&text, tone).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Err("refine request timed out".to_string()))
}

fn run_inject(session: &mut Session, ctx: &mut WorkerCtx, text: String) {
    match ctx.injector.inject(&text) {
        Ok(method) => {
            record_history(ctx, &text);
            dispatch(session, ctx, Event::InjectDone(method));
        }
        Err(e) => dispatch(session, ctx, Event::InjectFailed(e.to_string())),
    }
}

fn record_history(ctx: &mut WorkerCtx, final_text: &str) {
    let (Some(history), Some(pending)) = (&ctx.history, &ctx.pending) else {
        return;
    };

    let duration_ms = ctx
        .session_started_at
        .map(|started| started.elapsed().as_millis() as i64)
        .unwrap_or(0);

    let entry = NewEntry {
        duration_ms,
        engine: ctx.engine_label.clone(),
        raw_text: pending.raw.clone(),
        final_text: final_text.to_string(),
        app: None,
    };

    if let Err(e) = history.add(entry) {
        ctx.sink
            .notify("warning", &format!("failed to save history entry: {e}"));
    }
}

/// Drives the session as if a physical hotkey chord had just fired,
/// translating today's mode into the right half of the press/release pair:
/// idle starts recording; recording stops it (a second press for `Toggle`, a
/// release for `PushToTalk`); any busier state is ignored; there is nothing
/// sensible for a single button to do mid-pipeline.
fn handle_toggle(session: &mut Session, ctx: &mut WorkerCtx) {
    match session.state() {
        State::Idle => dispatch(session, ctx, Event::HotkeyPressed),
        State::Recording => {
            let event = match ctx.mode {
                DictationMode::Toggle => Event::HotkeyPressed,
                DictationMode::PushToTalk => Event::HotkeyReleased,
            };
            dispatch(session, ctx, event);
        }
        State::Transcribing | State::Refining | State::Injecting => {}
    }
}

/// Applies reloaded dependencies. If a session is recording, it is
/// cancelled first (see [`RuntimeHandle::reload`]'s doc comment for why);
/// by the time `ctx.apply` runs, the session is always idle.
fn reload(session: &mut Session, ctx: &mut WorkerCtx, new_deps: RuntimeDeps) {
    if session.state() == State::Recording {
        dispatch(session, ctx, Event::CancelRequested);
    }
    *session = Session::new(new_deps.mode, new_deps.refine_enabled);
    ctx.apply(new_deps);
}
