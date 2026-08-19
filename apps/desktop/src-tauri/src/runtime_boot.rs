//! Boots the dictation [`Runtime`] from persisted [`Settings`]: builds
//! [`RuntimeDeps`] (STT engine, refiner, injector chain, hotkey source,
//! history) and starts (or gracefully skips) the hotkey monitor thread.
//!
//! ## Degrade, don't fail
//!
//! Every piece of configuration that can plausibly be wrong or unavailable
//! (no whisper model downloaded yet, no hotkey permissions, an unconfigured
//! refiner, a build without the `sherpa` feature) degrades to a stand-in
//! that boots the runtime anyway and reports a notice, rather than aborting
//! boot.
//! [`boot`] only ever returns `Err` for genuinely unexpected failures (e.g.
//! the platform data directory can't be resolved at all).
//!
//! Every choice that doesn't need real hardware or filesystem access is a
//! small, separately testable pure function; the impure pieces (constructing
//! engines/injectors, spawning threads) are thin wrappers around them.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver};
use tauri::{AppHandle, Manager};

use utter_core::{
    InjectError, InjectionMethod, SttEngine, SttError, TextInjector, TextRefiner,
    TranscribeOptions, Transcript,
};
use utter_inject::{
    create_source, injection_order, parse_hotkey, ChainInjector, ClipboardOnlyInjector,
    ClipboardPasteInjector, HotkeyEvent, HotkeySpec, TypeInjector,
};
use utter_refine::{LlmConfig, LlmRefiner};
use utter_store::settings::{CloudSttCfg, EngineCfg, EngineKind, InjectionPreference, RefineCfg};
#[cfg(feature = "sherpa")]
use utter_store::IntegrityError;
use utter_store::{DraftCfg, LanguageProfile, ModelManager, Settings};
use utter_stt::{CloudEngine, CloudSttConfig, WhisperEngine};

use crate::profiles::{ProfileRegistry, RealProfileLoader};
use crate::runtime::{EventSink, HistoryHandle, RealCaptureBackend, Runtime, RuntimeDeps};
use crate::sink::TauriEventSink;
use crate::state::{AppState, PendingNotices};
use crate::{keyring_password, REFINE_KEY_SERVICE, STT_KEY_SERVICE};

/// Boots the dictation runtime from the current in-memory settings and
/// stores its control handle in `AppState::session_ctl`.
///
/// Called once at app startup. Any degradation (missing model, no hotkey
/// permissions, a v0.1 config that failed to migrate, ...) is reported
/// through a freshly built [`TauriEventSink`] once the runtime is up; only a
/// genuinely unexpected failure (e.g. the settings lock is poisoned, or the
/// history database can't be opened) short-circuits with `Err`, leaving
/// `session_ctl` at `None`.
pub fn boot(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();

    let settings = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned".to_string())?
        .clone();

    let history = open_history(&settings)?;
    let (deps, mut notices) = build_deps(&settings, &state.models, history);
    // A config that failed to migrate on load (see `AppState::new`) is a
    // one-time startup condition, not a per-boot degradation like the ones
    // `build_deps` reports, so it is queued here rather than threaded
    // through `build_deps`'s settings-derived checks.
    if let Some(msg) = state.startup_notice.clone() {
        notices.push(("warning", msg));
    }

    let sink = Arc::new(TauriEventSink::new(app.clone()));
    let handle = Runtime::spawn(deps, sink.clone());

    *state
        .session_ctl
        .lock()
        .map_err(|_| "session control lock poisoned".to_string())? = Some(handle);

    report_boot_notices(sink.as_ref(), &state.pending_notices, notices);

    Ok(())
}

/// Reports every notice boot collected on both channels available to it: the
/// live one (`sink`), and `parked`, which is the only one that still works
/// this early.
///
/// [`boot`] runs synchronously inside Tauri's `setup` — before the webview is
/// loaded, long before any window subscribes to `notice` — and Tauri's `emit`
/// has no replay, so on its own every `notice` fired here lands on zero
/// listeners forever. The desktop notification is not a backstop either: it
/// is deliberately rate limited (see [`crate::sink::NoticeThrottle`]) and
/// this loop has no delay in it, so a startup with two conditions to explain
/// — a transcription model that is not downloaded and a preview that is
/// unavailable, which arrive together — would show the first and drop the
/// second. Parking a copy is what lets the settings window list all of them
/// once it exists (see [`PendingNotices`] and the `take_pending_notices`
/// command).
///
/// [`rebuild`] deliberately does not park: it runs from `save_settings` or
/// the tray, with a window already open and listening, so a parked copy would
/// be replayed at some later mount as if it were news.
fn report_boot_notices(sink: &dyn EventSink, parked: &PendingNotices, notices: Vec<QueuedNotice>) {
    for (kind, msg) in notices {
        // Parked first: the parked copy is the one that survives, so it must
        // not depend on the live emit having got anywhere.
        parked.push(kind, &msg);
        sink.notify(kind, &msg);
    }
}

/// Rebuilds the dictation runtime from `settings`: reloads the running
/// worker if one exists, or spawns a fresh one if `boot` never got one going
/// (e.g. it failed outright at startup). Used by `save_settings` and the
/// tray's "Refinement" checkbox — the one path every settings change goes
/// through to reach the live runtime.
pub fn rebuild(app: &AppHandle, state: &AppState, settings: &Settings) -> Result<(), String> {
    let history = open_history(settings)?;
    let (deps, notices) = build_deps(settings, &state.models, history);
    let sink = Arc::new(TauriEventSink::new(app.clone()));

    {
        let mut guard = state
            .session_ctl
            .lock()
            .map_err(|_| "session control lock poisoned".to_string())?;

        match guard.as_ref() {
            Some(handle) => handle.reload(deps),
            None => *guard = Some(Runtime::spawn(deps, sink.clone())),
        }
    }

    for (kind, msg) in notices {
        sink.notify(kind, &msg);
    }

    Ok(())
}

/// Shuts the running dictation runtime down, if any, and waits for its
/// worker thread to exit. Called on app quit so the process never leaves a
/// zombie worker thread behind.
pub fn shutdown(state: &AppState) {
    let handle = match state.session_ctl.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(handle) = handle {
        handle.shutdown();
    }
}

/// Opens the runtime's own history connection, separate from
/// `AppState::history` (which the history-browsing commands keep open for
/// the app's whole lifetime regardless of this setting). `None` when
/// history recording is disabled.
fn open_history(settings: &Settings) -> Result<Option<HistoryHandle>, String> {
    if !settings.history.enabled {
        return Ok(None);
    }
    let path = crate::state::history_db_path().map_err(|e| e.to_string())?;
    HistoryHandle::open(&path)
        .map(Some)
        .map_err(|e| format!("failed to open history database: {e}"))
}

/// One queued user-facing notice: `kind` matches [`crate::runtime::EventSink::notify`]'s
/// convention (`"info"`, `"warning"`, `"error"`).
pub(crate) type QueuedNotice = (&'static str, String);

/// Builds [`RuntimeDeps`] from `settings`, plus any degradation notices to
/// surface once the runtime is up.
///
/// `models` is an `Arc` (not a borrow, unlike `build_engine`'s) because the [`ProfileRegistry`]
/// built here lazily loads engines for the runtime worker's whole lifetime, long after this
/// function returns — it needs an owned handle, not one borrowed from this call's stack frame.
fn build_deps(
    settings: &Settings,
    models: &Arc<ModelManager>,
    history: Option<HistoryHandle>,
) -> (RuntimeDeps, Vec<QueuedNotice>) {
    let mut notices = Vec::new();

    let (specs, kept_profiles, hotkey_notices) = parse_profile_hotkeys(settings.profiles.clone());
    notices.extend(hotkey_notices);

    let (hotkey_rx, hotkey_notice) = spawn_hotkey_sources(&specs);
    if let Some(msg) = hotkey_notice {
        notices.push(("warning", msg));
    }

    // Every call to `build_deps` -- at boot, and again on every settings save via
    // `rebuild` -- constructs a brand new `ProfileRegistry`, discarding whatever engines a
    // previous one had already lazily loaded. That looks expensive, but it is parity with
    // today: this function already rebuilt the single engine on every `rebuild` before profiles
    // existed (toggling the tray's refinement checkbox has always reloaded the whisper/sherpa
    // model too), and laziness bounds the new cost -- after a recreate, only the default
    // profile (index 0) reloads eagerly, and every other profile just re-pays its own load on
    // its next press, exactly as a single-profile setup does today, even for a bilingual user.
    // The narrower path -- keeping engines whose inputs (dictionary terms, engine config)
    // didn't change and rebuilding only refiners/flags -- is a real improvement and is
    // deferred, not rejected.
    let loader = Box::new(RealProfileLoader::new(
        models.clone(),
        settings.refine.clone(),
        settings.dictionary.terms.clone(),
    ));
    let (profiles, profile_notices) = ProfileRegistry::new(kept_profiles, loader);
    notices.extend(profile_notices);

    let injector = build_injector(settings.advanced.injection);

    let deps = RuntimeDeps {
        mode: settings.dictation.mode,
        silence: settings
            .dictation
            .silence_timeout_secs
            .map(|secs| Duration::from_secs(u64::from(secs))),
        profiles,
        injector,
        rules: settings.dictionary.rules.clone(),
        snippets: settings.snippets.clone(),
        history,
        capture_device: settings.advanced.audio_device.clone(),
        capture: Box::new(RealCaptureBackend),
        hotkey_rx,
        vad_sensitivity: settings.advanced.vad_sensitivity,
        refine_timeout: Duration::from_secs(settings.refine.timeout_secs),
    };

    (deps, notices)
}

/// Parses each profile's hotkey chord, keeping the profile only if it parses, and returns the
/// specs and surviving profiles in lockstep: `specs[i]` is `kept[i]`'s chord, so the
/// [`utter_inject::BindingId`] `create_source(&specs)` reports for index `i` always lines up
/// with `kept[i]`'s position in the [`ProfileRegistry`] built from `kept` (see its own doc
/// comment on the same invariant).
///
/// Building both lists from a single pass over `profiles` -- rather than something like
/// `profiles.iter().filter_map(|p| parse_hotkey(&p.hotkey).ok()).collect()` for the specs alone
/// -- is what keeps that alignment from drifting the moment any profile's hotkey fails to
/// parse: dropping a spec without also dropping its profile (or vice versa) would silently
/// shift every id after it, and the symptom is the user dictating in the wrong language with a
/// green test suite.
///
/// A profile whose hotkey is unparseable (nothing validates [`LanguageProfile::hotkey`] at
/// settings load) is dropped from both lists and reported as a `"warning"` notice naming it --
/// otherwise it would be a profile the registry can resolve but no chord could ever select,
/// silently dead (item 2 of the same amendment). See [`ProfileRegistry::new`]'s doc comment for
/// why the registry itself cannot catch this.
fn parse_profile_hotkeys(
    profiles: Vec<LanguageProfile>,
) -> (Vec<HotkeySpec>, Vec<LanguageProfile>, Vec<QueuedNotice>) {
    let mut specs = Vec::new();
    let mut kept = Vec::new();
    let mut notices = Vec::new();

    for profile in profiles {
        match parse_hotkey(&profile.hotkey) {
            Ok(spec) => {
                specs.push(spec);
                kept.push(profile);
            }
            Err(e) => {
                notices.push((
                    "warning",
                    format!(
                        "profile \"{}\" has an invalid hotkey \"{}\": {e}; dictation has no \
                         hotkey for this profile until it is fixed in Settings",
                        profile.id, profile.hotkey
                    ),
                ));
            }
        }
    }

    (specs, kept, notices)
}

/// The label recorded on history entries for the active engine kind.
pub(crate) fn engine_label(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Whisper => "whisper",
        EngineKind::Cloud => "cloud",
        EngineKind::Sherpa => "sherpa",
    }
}

/// Maps an [`InjectionPreference`] to the string [`injection_order`] expects.
fn injection_preference_str(pref: InjectionPreference) -> &'static str {
    match pref {
        InjectionPreference::Auto => "auto",
        InjectionPreference::ClipboardPaste => "clipboard_paste",
        InjectionPreference::Type => "type",
        InjectionPreference::ClipboardOnly => "clipboard_only",
    }
}

/// A [`SttEngine`] stand-in booted when the configured engine could not be
/// built (no model downloaded, unsupported build, ...). Lets the app boot
/// rather than fail outright: every call fails with `reason`, which surfaces
/// to the user as a normal transcription-failed notice the first time they
/// actually try to dictate, rather than at boot.
struct UnavailableEngine {
    reason: String,
}

impl SttEngine for UnavailableEngine {
    fn begin(&mut self, _opts: &TranscribeOptions) -> Result<(), SttError> {
        Err(SttError::ModelNotFound(self.reason.clone()))
    }

    fn feed(&mut self, _samples: &[i16]) -> Result<Option<String>, SttError> {
        Err(SttError::ModelNotFound(self.reason.clone()))
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        Err(SttError::ModelNotFound(self.reason.clone()))
    }
}

pub(crate) fn unavailable_engine(reason: String) -> Box<dyn SttEngine> {
    Box::new(UnavailableEngine { reason })
}

pub(crate) fn build_engine(
    cfg: &EngineCfg,
    models: &ModelManager,
    dictionary_terms: &[String],
) -> (Box<dyn SttEngine>, Option<String>) {
    match cfg.active {
        EngineKind::Whisper => build_whisper(&cfg.whisper_model, models),
        EngineKind::Cloud => build_cloud(&cfg.cloud),
        EngineKind::Sherpa => build_sherpa(cfg.sherpa_model.as_deref(), models, dictionary_terms),
    }
}

fn build_whisper(model_id: &str, models: &ModelManager) -> (Box<dyn SttEngine>, Option<String>) {
    let Some(path) = models.path_for(model_id) else {
        let reason = format!(
            "whisper model \"{model_id}\" is not downloaded; open Settings > Engines to download it"
        );
        return (unavailable_engine(reason.clone()), Some(reason));
    };

    match WhisperEngine::load(&path) {
        Ok(engine) => (Box::new(engine), None),
        Err(e) => {
            let reason = format!("failed to load whisper model \"{model_id}\": {e}");
            (unavailable_engine(reason.clone()), Some(reason))
        }
    }
}

/// The number of onnxruntime inference threads sherpa-onnx is allowed to
/// use: half the detected core count, clamped by
/// `utter_stt::sherpa::default_threads`, so the desktop stays responsive
/// while transcribing. `available_parallelism` can fail on some
/// platforms/sandboxes; a single thread is a safe, always-available fallback
/// rather than propagating that as a boot failure.
#[cfg(feature = "sherpa")]
fn sherpa_thread_count() -> usize {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    utter_stt::sherpa::default_threads(available)
}

/// The catalog `engine` string of the offline models whose text is injected.
#[cfg(feature = "sherpa")]
const OFFLINE_ENGINE: &str = "sherpa";

/// The catalog `engine` string of the streaming models that drive the live
/// preview. Mirrored in the UI as `PREVIEW_ENGINE`
/// (`apps/desktop/ui/src/lib/models.ts`), which keeps the two out of each
/// other's pickers; this module is what keeps a config that got one wrong
/// anyway from reaching a decoder.
#[cfg(feature = "sherpa")]
const STREAMING_ENGINE: &str = "sherpa-streaming";

/// How a notice refers to the models catalogued under `engine`.
#[cfg(feature = "sherpa")]
fn model_kind_description(engine: &str) -> String {
    match engine {
        OFFLINE_ENGINE => "an offline transcription model".to_string(),
        STREAMING_ENGINE => "a streaming preview model".to_string(),
        "whisper" => "a whisper model".to_string(),
        other => format!("a \"{other}\" model"),
    }
}

/// Rejects a model id that is catalogued under an engine other than
/// `expected_engine`, returning the reason to report if so.
///
/// **This is a process-liveness check, not a validation nicety, and it must
/// run before [`ModelManager::verify_installed`].** `verify_installed`
/// answers "are these files intact"; it cannot answer "are these the files
/// this engine can read", and a model of the wrong kind is usually perfectly
/// intact. The two sherpa engines resolve overlapping fixed artifact names —
/// `parakeet-tdt-110m-en` (offline) installs as exactly the
/// `encoder.onnx`/`decoder.onnx`/`joiner.onnx`/`tokens.txt` quartet
/// `SherpaStreamingEngine::load` looks for — so an intact offline model
/// handed to the streaming recognizer sails through every existing check and
/// reaches sherpa-onnx, which reads streaming-only metadata keys
/// (`decode_chunk_len`, encoder dims) that an offline export does not carry
/// and terminates the process rather than returning an error. Nothing in Rust
/// can catch that, which is why the id's *kind* is settled here, first, on
/// catalog data alone.
///
/// An id that is not in the catalog at all is rejected too, with its own
/// wording: it has no kind to check, nothing could install it, and letting it
/// fall through would report a typo'd id as merely "not downloaded".
#[cfg(feature = "sherpa")]
fn wrong_model_kind(
    models: &ModelManager,
    model_id: &str,
    expected_engine: &str,
) -> Option<String> {
    match models.engine_of(model_id) {
        Some(engine) if engine == expected_engine => None,
        Some(engine) => Some(format!(
            "model \"{model_id}\" is {}, not {}; choose a different model in Settings > Profiles",
            model_kind_description(engine),
            model_kind_description(expected_engine)
        )),
        None => Some(format!(
            "model \"{model_id}\" is not in the model catalog; choose a model in Settings > \
             Profiles"
        )),
    }
}

#[cfg(feature = "sherpa")]
fn build_sherpa(
    model_id: Option<&str>,
    models: &ModelManager,
    dictionary_terms: &[String],
) -> (Box<dyn SttEngine>, Option<String>) {
    let Some(model_id) = model_id else {
        let reason =
            "no sherpa model configured; open Settings > Engines to download one".to_string();
        return (unavailable_engine(reason.clone()), Some(reason));
    };

    if let Some(reason) = wrong_model_kind(models, model_id, OFFLINE_ENGINE) {
        return (unavailable_engine(reason.clone()), Some(reason));
    }

    let path = match models.verify_installed(model_id) {
        Ok(path) => path,
        Err(IntegrityError::SizeMismatch { artifact, .. }) => {
            let reason = format!(
                "sherpa model \"{model_id}\" is damaged (artifact \"{artifact}\" has the wrong \
                 size); re-download it from Settings > Engines"
            );
            return (unavailable_engine(reason.clone()), Some(reason));
        }
        Err(_) => {
            let reason = format!(
                "sherpa model \"{model_id}\" is not downloaded; open Settings > Engines to \
                 download it"
            );
            return (unavailable_engine(reason.clone()), Some(reason));
        }
    };

    let cfg = utter_stt::SherpaConfig {
        num_threads: sherpa_thread_count(),
        hotwords: dictionary_terms.to_vec(),
    };

    match utter_stt::SherpaOfflineEngine::load(&path, cfg) {
        Ok(engine) => (Box::new(engine), None),
        Err(e) => {
            let reason = format!("failed to load sherpa model \"{model_id}\": {e}");
            (unavailable_engine(reason.clone()), Some(reason))
        }
    }
}

#[cfg(not(feature = "sherpa"))]
fn build_sherpa(
    _model_id: Option<&str>,
    _models: &ModelManager,
    _dictionary_terms: &[String],
) -> (Box<dyn SttEngine>, Option<String>) {
    let reason = "this build was compiled without sherpa support; switch this profile's engine \
                   in Settings > Profiles, or install a build with the sherpa feature enabled"
        .to_string();
    (unavailable_engine(reason.clone()), Some(reason))
}

/// Builds a profile's draft (preview) engine from its [`DraftCfg`], plus a
/// notice if one was configured but could not be built.
///
/// Unlike [`build_engine`], failure has no stand-in: `None` already means
/// "this profile has no preview", and the runtime treats it exactly that way
/// (see [`ProfileDeps::draft_engine`](crate::profiles::ProfileDeps::draft_engine)).
/// A missing, damaged or unloadable preview model therefore costs the user
/// nothing but the preview itself — the profile still dictates and still
/// injects the final engine's text — which is why callers queue this
/// function's notice as `"info"` rather than the `"warning"` a broken final
/// engine earns.
///
/// `None`, or a blank model id, is the configured-off state (what the
/// Profiles page writes when the preview is switched off) and is silent: it
/// is a choice, not a degradation.
pub(crate) fn build_draft_engine(
    cfg: Option<&DraftCfg>,
    models: &ModelManager,
    dictionary_terms: &[String],
) -> (Option<Box<dyn SttEngine>>, Option<String>) {
    let model_id = cfg.map(|draft| draft.model.trim()).unwrap_or_default();
    if model_id.is_empty() {
        return (None, None);
    }

    build_streaming_draft(model_id, models, dictionary_terms)
}

/// The number of onnxruntime inference threads the draft engine gets:
/// exactly one, deliberately *not* [`sherpa_thread_count`].
///
/// The draft engine decodes concurrently with the final engine on the same
/// machine, so its threads come out of the same pool. Benchmarking on the
/// target hardware put the final engine's latency optimum at 4 threads of 6,
/// with the oversubscription of running both at that width costing 18%. A
/// small int8 streaming model keeps up on a single thread, and staying out of
/// the final engine's way is the entire point of it: the preview is a
/// courtesy, the injected text is not.
#[cfg(feature = "sherpa")]
const DRAFT_THREADS: usize = 1;

/// The [`utter_stt::SherpaConfig`] every draft engine is loaded with.
///
/// Split out of [`build_streaming_draft`] so that [`DRAFT_THREADS`] actually
/// reaches something a test can look at: the only other observer of it is
/// onnxruntime, well past the point any test can go. It is the sole
/// construction site of a draft `SherpaConfig`, so what this returns is what
/// the draft engine gets.
#[cfg(feature = "sherpa")]
fn draft_sherpa_config(dictionary_terms: &[String]) -> utter_stt::SherpaConfig {
    utter_stt::SherpaConfig {
        num_threads: DRAFT_THREADS,
        hotwords: dictionary_terms.to_vec(),
    }
}

#[cfg(feature = "sherpa")]
fn build_streaming_draft(
    model_id: &str,
    models: &ModelManager,
    dictionary_terms: &[String],
) -> (Option<Box<dyn SttEngine>>, Option<String>) {
    // Kind before integrity: an offline model is a valid, intact install of
    // something this engine cannot read, and `verify_installed` would wave it
    // through — see `wrong_model_kind`.
    if let Some(reason) = wrong_model_kind(models, model_id, STREAMING_ENGINE) {
        let reason = format!("{reason}. Dictation is unaffected — only the live preview is off.");
        return (None, Some(reason));
    }

    // `verify_installed`, never `path_for`: a truncated or otherwise damaged
    // model makes sherpa-onnx's C++ layer call `_Exit()` on load, taking the
    // whole app down with no chance for Rust to catch it. See `build_sherpa`,
    // which guards the final engine's path the same way.
    let path = match models.verify_installed(model_id) {
        Ok(path) => path,
        Err(IntegrityError::SizeMismatch { artifact, .. }) => {
            let reason = format!(
                "preview model \"{model_id}\" is damaged (artifact \"{artifact}\" has the wrong \
                 size); re-download it from Settings > Engines. Dictation is unaffected — only \
                 the live preview is off."
            );
            return (None, Some(reason));
        }
        Err(_) => {
            let reason = format!(
                "preview model \"{model_id}\" is not downloaded; open Settings > Engines to \
                 download it. Dictation is unaffected — only the live preview is off."
            );
            return (None, Some(reason));
        }
    };

    match utter_stt::SherpaStreamingEngine::load(&path, draft_sherpa_config(dictionary_terms)) {
        Ok(engine) => (Some(Box::new(engine)), None),
        Err(e) => {
            let reason = format!(
                "failed to load preview model \"{model_id}\": {e}. Dictation is unaffected — \
                 only the live preview is off."
            );
            (None, Some(reason))
        }
    }
}

#[cfg(not(feature = "sherpa"))]
fn build_streaming_draft(
    model_id: &str,
    _models: &ModelManager,
    _dictionary_terms: &[String],
) -> (Option<Box<dyn SttEngine>>, Option<String>) {
    let reason = format!(
        "this build was compiled without sherpa support, so the preview model \"{model_id}\" \
         cannot be loaded; switch the preview off in Settings > Profiles, or install a build \
         with the sherpa feature enabled. Dictation is unaffected — only the live preview is off."
    );
    (None, Some(reason))
}

/// A generous but bounded default for the cloud engine's HTTP timeout:
/// `Settings` has no per-request timeout for speech-to-text (only refine has
/// one), and a single-utterance transcription call is not expected to run
/// long.
const CLOUD_STT_TIMEOUT: Duration = Duration::from_secs(30);

fn build_cloud(cfg: &CloudSttCfg) -> (Box<dyn SttEngine>, Option<String>) {
    let api_key = keyring_password(STT_KEY_SERVICE);
    let notice = api_key.is_none().then(|| {
        "no cloud speech-to-text API key configured; open Settings > Engines to add one".to_string()
    });

    let engine = CloudEngine::new(CloudSttConfig {
        base_url: cfg.base_url.clone(),
        api_key: api_key.unwrap_or_default(),
        model: cfg.model.clone(),
        timeout: CLOUD_STT_TIMEOUT,
    });

    (Box::new(engine), notice)
}

/// A refiner is only built when the user enabled refinement AND gave it a
/// base URL and model to call — the two fields with no sensible meaning
/// left empty. `Settings`'s defaults already fill both with a usable local
/// endpoint, so in practice this gate is just `cfg.enabled`.
fn refine_configured(cfg: &RefineCfg) -> bool {
    cfg.enabled && !cfg.base_url.trim().is_empty() && !cfg.model.trim().is_empty()
}

/// The notice queued when refinement is enabled/configured but no API key is
/// set in the keyring — pulled out as a pure function of `has_key` (rather
/// than inlined next to the `keyring_password` call) so it's testable
/// without touching the real keyring. Unlike a missing cloud STT key (always
/// an error — that endpoint always requires one), a missing refine key is
/// legitimate for local endpoints (e.g. Ollama), so this is an `"info"`
/// notice, not a `"warning"`, and never blocks building the refiner.
fn refine_missing_key_notice(has_key: bool) -> Option<String> {
    if has_key {
        None
    } else {
        Some(
            "Refinement is enabled without an API key; local endpoints (e.g. Ollama) work \
             without one — set a key in Settings if your provider requires it."
                .to_string(),
        )
    }
}

pub(crate) fn build_refiner(
    cfg: &RefineCfg,
    dictionary_terms: Vec<String>,
) -> (Option<Box<dyn TextRefiner>>, Option<String>) {
    if !refine_configured(cfg) {
        return (None, None);
    }

    let api_key = keyring_password(REFINE_KEY_SERVICE);
    let notice = refine_missing_key_notice(api_key.is_some());

    // A refiner that cannot even be constructed degrades to "no refiner",
    // like every other `build_*` here degrades to its own unavailable form.
    // This used to panic, which was survivable only while refiners were
    // built during boot; once they are built per profile on the dictation
    // worker, the same panic would take the worker down and every profile's
    // dictation with it.
    let refiner = match LlmRefiner::new(
        LlmConfig {
            base_url: cfg.base_url.clone(),
            api_key,
            model: cfg.model.clone(),
            timeout: Duration::from_secs(cfg.timeout_secs),
        },
        dictionary_terms,
    ) {
        Ok(refiner) => refiner,
        Err(e) => {
            let reason = format!("refinement is unavailable: could not build its HTTP client: {e}");
            return (None, Some(reason));
        }
    };

    (Some(Box::new(refiner)), notice)
}

fn build_injector(preference: InjectionPreference) -> Box<dyn TextInjector> {
    let mut injectors: Vec<Box<dyn TextInjector>> = Vec::new();

    for method in injection_order(injection_preference_str(preference)) {
        let built: Result<Box<dyn TextInjector>, InjectError> = match method {
            InjectionMethod::ClipboardPaste => {
                ClipboardPasteInjector::new().map(|i| Box::new(i) as Box<dyn TextInjector>)
            }
            InjectionMethod::Type => {
                TypeInjector::new().map(|i| Box::new(i) as Box<dyn TextInjector>)
            }
            InjectionMethod::ClipboardOnly => {
                Ok(Box::new(ClipboardOnlyInjector::new()) as Box<dyn TextInjector>)
            }
        };

        match built {
            Ok(injector) => injectors.push(injector),
            Err(e) => tracing::warn!("injector backend {method:?} unavailable: {e}"),
        }
    }

    Box::new(ChainInjector::new(injectors))
}

/// Starts the hotkey monitor thread watching every chord in `specs` at once (see
/// [`utter_inject::create_source`]) and returns the receiver side of its shared event channel,
/// plus a notice if capture couldn't be started (missing permissions, or the OS refusing to
/// spawn the monitor thread) — the runtime still boots with no hotkey rather than failing
/// outright. `specs` is assumed already validated: [`parse_profile_hotkeys`] is the only caller,
/// and it has already dropped anything `parse_hotkey` rejects, so this function does no parsing
/// of its own.
///
/// The channel's sender is cloned before being handed to the spawned
/// `HotkeySource` thread rather than moved directly: on the happy path the
/// thread's clone keeps the channel alive (until a later `save_settings`
/// supersedes it via the generation counter — see `utter_inject::create_source`)
/// for as long as it runs, so this function's own clone can simply be
/// dropped normally. On any failure path (no thread ever started to own a
/// clone), this function's clone is instead deliberately leaked: dropping it
/// would make every future `select!` in the runtime worker see the channel
/// as immediately "ready" with a disconnect error, spinning the worker
/// thread at 100% CPU forever. One leaked `Sender` per failed (re)boot is an
/// intentionally rare, negligible cost next to that.
fn spawn_hotkey_sources(specs: &[HotkeySpec]) -> (Receiver<HotkeyEvent>, Option<String>) {
    let (tx, rx) = unbounded::<HotkeyEvent>();

    let source = match create_source(specs) {
        Ok(source) => source,
        Err(e) => {
            std::mem::forget(tx);
            return (
                rx,
                Some(format!(
                    "failed to start hotkey capture: {e}; check input group / uinput \
                     permissions (the checks onboarding ran at first launch)"
                )),
            );
        }
    };

    let thread_tx = tx.clone();
    let spawned = thread::Builder::new()
        .name("utter-hotkey".to_string())
        .spawn(move || source.run(thread_tx));

    match spawned {
        Ok(_join_handle) => (rx, None),
        Err(e) => {
            tracing::error!("failed to spawn the utter-hotkey source thread: {e}");
            std::mem::forget(tx);
            (
                rx,
                Some(format!(
                    "failed to start hotkey capture: {e}; dictation has no hotkey until the \
                     app is restarted"
                )),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what a sink was asked to report, standing in for the
    /// `TauriEventSink` `boot` builds (which needs a running app).
    #[derive(Default)]
    struct RecordingSink {
        reported: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl EventSink for RecordingSink {
        fn emit_state(&self, _state: &str, _level: f32, _partial: Option<&str>) {}

        fn notify(&self, kind: &str, msg: &str) {
            self.reported
                .lock()
                .expect("lock")
                .push((kind.to_string(), msg.to_string()));
        }
    }

    /// Every notice boot reports must also be parked, because at boot time
    /// the live channel reaches nobody: the `notice` event has no listener
    /// yet (the webview is not loaded during `setup`), and the desktop
    /// notification throttle drops everything after the first in an
    /// undelayed loop like this one.
    ///
    /// The fixture is deliberately *two* notices, the real pairing of a
    /// missing transcription model with an unavailable preview: parking only
    /// the first would satisfy any assertion written against a single-notice
    /// fixture while leaving the exact configuration this exists for broken.
    #[test]
    fn every_notice_boot_reports_is_also_parked_for_the_first_window() {
        let sink = RecordingSink::default();
        let parked = PendingNotices::default();

        report_boot_notices(
            &sink,
            &parked,
            vec![
                ("warning", "no transcription model".to_string()),
                ("info", "live preview unavailable".to_string()),
            ],
        );

        assert_eq!(
            sink.reported
                .lock()
                .expect("lock")
                .iter()
                .map(|(kind, msg)| (kind.as_str(), msg.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("warning", "no transcription model"),
                ("info", "live preview unavailable"),
            ],
            "the live channel must still get everything it got before"
        );

        assert_eq!(
            parked
                .take()
                .iter()
                .map(|n| n.message.clone())
                .collect::<Vec<_>>(),
            vec![
                "no transcription model".to_string(),
                "live preview unavailable".to_string(),
            ],
            "both notices must be parked, not just the one the throttle would let through"
        );
    }

    #[test]
    fn engine_label_matches_each_kind() {
        assert_eq!(engine_label(EngineKind::Whisper), "whisper");
        assert_eq!(engine_label(EngineKind::Cloud), "cloud");
        assert_eq!(engine_label(EngineKind::Sherpa), "sherpa");
    }

    #[test]
    fn injection_preference_str_matches_injection_order_vocabulary() {
        assert_eq!(injection_preference_str(InjectionPreference::Auto), "auto");
        assert_eq!(
            injection_preference_str(InjectionPreference::ClipboardPaste),
            "clipboard_paste"
        );
        assert_eq!(injection_preference_str(InjectionPreference::Type), "type");
        assert_eq!(
            injection_preference_str(InjectionPreference::ClipboardOnly),
            "clipboard_only"
        );
    }

    fn refine_cfg(enabled: bool, base_url: &str, model: &str) -> RefineCfg {
        RefineCfg {
            enabled,
            base_url: base_url.to_string(),
            model: model.to_string(),
            ..RefineCfg::default()
        }
    }

    #[test]
    fn refine_configured_requires_enabled_and_nonempty_provider() {
        assert!(refine_configured(&refine_cfg(
            true,
            "http://localhost:11434/v1",
            "llama3.2"
        )));
        assert!(!refine_configured(&refine_cfg(
            false,
            "http://localhost:11434/v1",
            "llama3.2"
        )));
        assert!(!refine_configured(&refine_cfg(true, "  ", "llama3.2")));
        assert!(!refine_configured(&refine_cfg(
            true,
            "http://localhost:11434/v1",
            ""
        )));
    }

    #[test]
    fn refine_missing_key_notice_only_fires_when_key_is_absent() {
        let notice = refine_missing_key_notice(false).expect("missing key should notice");
        assert!(notice.contains("without an API key"));
        assert!(notice.contains("Ollama"));

        assert_eq!(refine_missing_key_notice(true), None);
    }

    fn profile(id: &str, hotkey: &str) -> LanguageProfile {
        LanguageProfile {
            id: id.to_string(),
            hotkey: hotkey.to_string(),
            ..LanguageProfile::default()
        }
    }

    #[test]
    fn parse_profile_hotkeys_keeps_every_profile_when_every_hotkey_parses() {
        let profiles = vec![profile("ru", "ctrl+super"), profile("en", "ctrl+alt+super")];

        let (specs, kept, notices) = parse_profile_hotkeys(profiles);

        assert_eq!(specs.len(), 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].id, "ru");
        assert_eq!(kept[1].id, "en");
        assert!(notices.is_empty());
    }

    /// The bad chord sits in the *middle* of the list on purpose: the naive `filter_map`
    /// implementation this guards against still gets index 0 right, so a fixture that only ever
    /// puts the bad hotkey last or first would not catch a positional-drift regression. `kept[1]`
    /// must be `"en"` (not `"de"`, which would be the result of `specs`/`kept` drifting out of
    /// lockstep), pinning that a dropped profile is dropped from *both* lists at once, keeping
    /// every id after it aligned.
    #[test]
    fn parse_profile_hotkeys_drops_a_bad_chord_in_the_middle_without_shifting_the_rest() {
        let profiles = vec![
            profile("ru", "ctrl+super"),
            profile("de", "not+a+real+hotkey+++"),
            profile("en", "ctrl+alt+super"),
        ];

        let (specs, kept, notices) = parse_profile_hotkeys(profiles);

        assert_eq!(specs.len(), 2, "the bad chord must not produce a spec");
        assert_eq!(kept.len(), 2, "the bad chord's profile must not be kept");
        assert_eq!(kept[0].id, "ru", "binding 0 is still the first profile");
        assert_eq!(
            kept[1].id, "en",
            "binding 1 must be \"en\", not the dropped \"de\" -- proves specs and kept stayed \
             in lockstep rather than one drifting relative to the other"
        );

        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, "warning");
        assert!(
            notices[0].1.contains("\"de\""),
            "the notice must name the profile whose hotkey was rejected, got {:?}",
            notices[0].1
        );

        assert_eq!(
            specs,
            vec![
                parse_hotkey("ctrl+super").expect("valid"),
                parse_hotkey("ctrl+alt+super").expect("valid"),
            ],
            "specs[i] must be kept[i]'s chord, not merely the right number of specs"
        );
    }

    #[test]
    fn missing_whisper_model_degrades_with_a_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::new(dir.path().to_path_buf());

        let (mut engine, notice) = build_whisper("tiny", &models);

        let notice = notice.expect("missing model should produce a notice");
        assert!(notice.contains("tiny"));

        let err = engine
            .begin(&TranscribeOptions::default())
            .expect_err("an unavailable engine must fail begin() informatively");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }

    /// A configured sherpa model is a catalog id, not a filesystem path: it
    /// has to be resolved through the `ModelManager` the same way whisper ids
    /// are. Passing the id straight to the engine is an easy mistake that has
    /// bitten this codebase before (v0.1).
    #[cfg(feature = "sherpa")]
    #[test]
    fn missing_sherpa_model_degrades_with_a_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::new(dir.path().to_path_buf());

        let (mut engine, notice) = build_sherpa(Some("gigaam-v3-e2e-rnnt"), &models, &[]);

        let notice = notice.expect("missing model should produce a notice");
        assert!(notice.contains("not downloaded"));

        let err = engine
            .begin(&TranscribeOptions::default())
            .expect_err("an unavailable engine must fail begin() informatively");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }

    /// A damaged model (here, a truncated `encoder.int8.onnx`) must never
    /// reach `SherpaOfflineEngine::load`: sherpa-onnx and onnxruntime abort
    /// the whole process on a malformed model file, uncatchable from Rust.
    /// `build_sherpa` must instead degrade the same way a missing model
    /// does, but name the offending artifact and tell the user to
    /// re-download rather than saying the model was never downloaded.
    #[cfg(feature = "sherpa")]
    #[test]
    fn damaged_sherpa_model_degrades_with_a_notice_naming_the_artifact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::new(dir.path().to_path_buf());

        let model_dir = dir.path().join("models").join("gigaam-v3-e2e-rnnt");
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        std::fs::write(model_dir.join("encoder.int8.onnx"), b"truncated")
            .expect("write truncated encoder");
        std::fs::write(model_dir.join("decoder.onnx"), vec![0u8; 4_600_132])
            .expect("write decoder");
        std::fs::write(model_dir.join("joiner.onnx"), vec![0u8; 2_712_896]).expect("write joiner");
        std::fs::write(model_dir.join("tokens.txt"), vec![0u8; 13_354]).expect("write tokens");

        let (mut engine, notice) = build_sherpa(Some("gigaam-v3-e2e-rnnt"), &models, &[]);

        let notice = notice.expect("a damaged model should produce a notice");
        assert!(notice.contains("damaged"));
        assert!(notice.contains("encoder.int8.onnx"));
        assert!(notice.contains("re-download"));

        let err = engine
            .begin(&TranscribeOptions::default())
            .expect_err("an unavailable engine must fail begin() informatively");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }

    /// A streaming preview model configured as the *final* engine's model must be rejected on
    /// its kind, before `verify_installed` and before any engine is constructed. The offline and
    /// streaming loaders resolve overlapping artifact names, so an intact model of the wrong kind
    /// reaches sherpa-onnx looking exactly like a right one and kills the process there.
    ///
    /// The fixture is deliberately a *damaged* install of a real streaming id: without the kind
    /// check `verify_installed` would reject it as damaged and the degradation would look fine
    /// from the outside, so asserting the notice talks about the model's kind and *not* about
    /// damage is what pins the check running first, on its own terms.
    #[cfg(feature = "sherpa")]
    #[test]
    fn a_streaming_model_is_rejected_as_the_injected_transcript_engine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::new(dir.path().to_path_buf());

        let model_dir = dir.path().join("models").join("zipformer-ru-small");
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        for name in ["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"] {
            std::fs::write(model_dir.join(name), b"wrong size on purpose").expect("write artifact");
        }

        let (mut engine, notice) = build_sherpa(Some("zipformer-ru-small"), &models, &[]);

        let notice = notice.expect("a model of the wrong kind must produce a notice");
        assert!(notice.contains("zipformer-ru-small"), "got {notice:?}");
        assert!(
            notice.contains("a streaming preview model"),
            "the notice must say what the model actually is, got {notice:?}"
        );
        assert!(
            notice.contains("an offline transcription model"),
            "the notice must say what was needed instead, got {notice:?}"
        );
        assert!(
            notice.contains("Settings > Profiles"),
            "the notice must name the page where this is changed, got {notice:?}"
        );
        assert!(
            !notice.contains("damaged"),
            "the kind check must run before the integrity check, got {notice:?}"
        );

        let err = engine
            .begin(&TranscribeOptions::default())
            .expect_err("a profile whose final engine is unusable must fail informatively");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }

    /// An id in no catalog entry at all is the third case: it has no kind to check and nothing
    /// could ever install it, so it is rejected here rather than falling through to
    /// `verify_installed`, which would report a typo as merely "not downloaded".
    #[cfg(feature = "sherpa")]
    #[test]
    fn an_uncatalogued_model_id_is_rejected_as_unknown_rather_than_undownloaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::new(dir.path().to_path_buf());

        let (_engine, notice) = build_sherpa(Some("typo-not-a-model"), &models, &[]);

        let notice = notice.expect("an uncatalogued id must produce a notice");
        assert!(notice.contains("typo-not-a-model"), "got {notice:?}");
        assert!(
            notice.contains("not in the model catalog"),
            "got {notice:?}"
        );
        assert!(
            !notice.contains("not downloaded"),
            "an id no catalog entry has cannot be downloaded, so saying so would misdirect the \
             user, got {notice:?}"
        );
    }

    /// The catalog installs every streaming model under exactly the file
    /// names `SherpaStreamingEngine::load` opens.
    ///
    /// The two halves live in crates that cannot see each other —
    /// `utter-store` decides the installed names via `Artifact.name`,
    /// `utter-stt` resolves four fixed ones — and this crate is the first
    /// place downstream of both. Nothing else checks them: renaming an
    /// artifact back to its upstream file name (`encoder.int8.onnx`,
    /// `encoder-epoch-99-avg-1.int8.onnx`) leaves every other test green
    /// while the preview quietly never loads for that language, since a
    /// preview that fails to load is by design only an `"info"` notice.
    ///
    /// Driven off the catalog rather than a hardcoded id list, so an entry
    /// added later is covered without anyone remembering to come back here;
    /// the emptiness guard is what keeps that from silently becoming a loop
    /// over nothing. Names are compared as sorted sets because the loader
    /// resolves each by name and does not care in what order the catalog
    /// happens to list them.
    #[cfg(feature = "sherpa")]
    #[test]
    fn every_streaming_catalog_entry_installs_the_filenames_the_loader_resolves() {
        let models = ModelManager::new(std::path::PathBuf::from("/nonexistent"));

        let mut expected = utter_stt::sherpa::STREAMING_MODEL_FILES.to_vec();
        expected.sort_unstable();

        let ids: Vec<String> = models
            .catalog()
            .into_iter()
            .filter(|m| m.engine == STREAMING_ENGINE)
            .map(|m| m.id)
            .collect();
        assert!(
            !ids.is_empty(),
            "the catalog has no {STREAMING_ENGINE} entries, so this test would assert nothing"
        );

        for id in ids {
            let mut names = models
                .artifact_names(&id)
                .expect("an id taken from the catalog is in the catalog");
            names.sort_unstable();
            assert_eq!(
                names, expected,
                "{id} must install the artifact names SherpaStreamingEngine::load resolves, or \
                 its preview will never load"
            );
        }
    }

    /// The draft engine is loaded on exactly one inference thread, never on
    /// [`sherpa_thread_count`] like the final engine.
    ///
    /// Nothing downstream of here can be observed from a test — the number's
    /// only other reader is onnxruntime — so this is asserted at the last
    /// point it is still visible, [`draft_sherpa_config`], which is the sole
    /// construction site of a draft `SherpaConfig`. Without it the constant
    /// is pinned by nothing at all: swapping it for `sherpa_thread_count()`
    /// leaves the whole suite green while reintroducing the 18% latency cost
    /// of the two engines oversubscribing the same cores, which only shows up
    /// on a stopwatch.
    ///
    /// Asserting the literal `1` rather than "less than the final engine's"
    /// is deliberate: on a single-core machine the two are equal and the
    /// difference is unassertable, but on such a machine there is also no
    /// oversubscription to prevent, so the invariant worth stating is the
    /// absolute one. The hotwords assertion rides along because a config
    /// helper that forgot to forward them would silently cost the preview its
    /// dictionary biasing.
    #[cfg(feature = "sherpa")]
    #[test]
    fn the_draft_engine_is_configured_for_a_single_inference_thread() {
        let cfg = draft_sherpa_config(&["Kubernetes".to_string()]);

        assert_eq!(
            cfg.num_threads, 1,
            "the draft engine runs concurrently with the final one and must stay out of its \
             way; see DRAFT_THREADS"
        );
        assert_eq!(
            cfg.hotwords,
            vec!["Kubernetes".to_string()],
            "the preview is biased by the same dictionary terms the final engine is"
        );
    }

    #[cfg(not(feature = "sherpa"))]
    #[test]
    fn sherpa_without_the_feature_degrades_with_a_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::new(dir.path().to_path_buf());

        let (mut engine, notice) = build_sherpa(Some("gigaam-v3-e2e-rnnt"), &models, &[]);

        let notice = notice.expect("a build without sherpa support should produce a notice");
        assert!(notice.contains("sherpa"));

        let err = engine
            .begin(&TranscribeOptions::default())
            .expect_err("an unavailable engine must fail begin() informatively");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }
}
