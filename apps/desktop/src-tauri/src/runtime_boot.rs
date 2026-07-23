//! Boots the dictation [`Runtime`] from persisted [`Settings`]: builds
//! [`RuntimeDeps`] (STT engine, refiner, injector chain, hotkey source,
//! history) and starts (or gracefully skips) the hotkey monitor thread.
//!
//! ## Degrade, don't fail
//!
//! Every piece of configuration that can plausibly be wrong or unavailable
//! (no whisper model downloaded yet, no hotkey permissions, an unconfigured
//! refiner, a build without the `vosk` feature) degrades to a stand-in that
//! boots the runtime anyway and reports a notice, rather than aborting boot.
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
    ClipboardPasteInjector, HotkeyEvent, TypeInjector,
};
use utter_refine::{LlmConfig, LlmRefiner};
use utter_store::settings::{CloudSttCfg, EngineCfg, EngineKind, InjectionPreference, RefineCfg};
use utter_store::{ModelManager, Settings};
use utter_stt::{CloudEngine, CloudSttConfig, WhisperEngine};

use crate::runtime::{EventSink, HistoryHandle, RealCaptureBackend, Runtime, RuntimeDeps};
use crate::sink::TauriEventSink;
use crate::state::AppState;
use crate::{keyring_password, REFINE_KEY_SERVICE, STT_KEY_SERVICE};

/// Boots the dictation runtime from the current in-memory settings and
/// stores its control handle in `AppState::session_ctl`.
///
/// Called once at app startup. Any degradation (missing model, no hotkey
/// permissions, ...) is reported through a freshly built [`TauriEventSink`]
/// once the runtime is up; only a genuinely unexpected failure (e.g. the
/// settings lock is poisoned, or the history database can't be opened)
/// short-circuits with `Err`, leaving `session_ctl` at `None`.
pub fn boot(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();

    let settings = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned".to_string())?
        .clone();

    let history = open_history(&settings)?;
    let (deps, notices) = build_deps(&settings, &state.models, history);

    let sink = Arc::new(TauriEventSink::new(app.clone()));
    let handle = Runtime::spawn(deps, sink.clone());

    *state
        .session_ctl
        .lock()
        .map_err(|_| "session control lock poisoned".to_string())? = Some(handle);

    for (kind, msg) in notices {
        sink.notify(kind, &msg);
    }

    Ok(())
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
type QueuedNotice = (&'static str, String);

/// Builds [`RuntimeDeps`] from `settings`, plus any degradation notices to
/// surface once the runtime is up.
fn build_deps(
    settings: &Settings,
    models: &ModelManager,
    history: Option<HistoryHandle>,
) -> (RuntimeDeps, Vec<QueuedNotice>) {
    let mut notices = Vec::new();

    let (engine, engine_notice) = build_engine(&settings.engine, models);
    if let Some(msg) = engine_notice {
        notices.push(("warning", msg));
    }

    let refiner = build_refiner(&settings.refine, settings.dictionary.terms.clone());
    let injector = build_injector(settings.advanced.injection);

    let (hotkey_rx, hotkey_notice) = spawn_hotkey_source(&settings.dictation.hotkey);
    if let Some(msg) = hotkey_notice {
        notices.push(("warning", msg));
    }

    let deps = RuntimeDeps {
        mode: settings.dictation.mode,
        refine_enabled: settings.refine.enabled,
        silence: settings
            .dictation
            .silence_timeout_secs
            .map(|secs| Duration::from_secs(u64::from(secs))),
        engine,
        refiner,
        injector,
        rules: settings.dictionary.rules.clone(),
        snippets: settings.snippets.clone(),
        history,
        capture_device: settings.advanced.audio_device.clone(),
        capture: Box::new(RealCaptureBackend),
        hotkey_rx,
        vad_sensitivity: settings.advanced.vad_sensitivity,
        refine_timeout: Duration::from_secs(settings.refine.timeout_secs),
        tone: settings.refine.tone,
        language: settings.general.language.clone(),
        engine_label: engine_label(settings.engine.active).to_string(),
    };

    (deps, notices)
}

/// The label recorded on history entries for the active engine kind.
fn engine_label(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Whisper => "whisper",
        EngineKind::Vosk => "vosk",
        EngineKind::Cloud => "cloud",
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

fn unavailable_engine(reason: String) -> Box<dyn SttEngine> {
    Box::new(UnavailableEngine { reason })
}

fn build_engine(cfg: &EngineCfg, models: &ModelManager) -> (Box<dyn SttEngine>, Option<String>) {
    match cfg.active {
        EngineKind::Whisper => build_whisper(&cfg.whisper_model, models),
        EngineKind::Vosk => build_vosk(cfg.vosk_model.as_deref()),
        EngineKind::Cloud => build_cloud(&cfg.cloud),
    }
}

fn build_whisper(model_id: &str, models: &ModelManager) -> (Box<dyn SttEngine>, Option<String>) {
    let Some(path) = models.path_for(model_id) else {
        let reason = format!(
            "whisper model \"{model_id}\" is not downloaded; open Settings > Models to download it"
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

#[cfg(feature = "vosk")]
fn build_vosk(model_dir: Option<&str>) -> (Box<dyn SttEngine>, Option<String>) {
    let Some(dir) = model_dir else {
        let reason = "no vosk model directory configured; open Settings > Models to download one"
            .to_string();
        return (unavailable_engine(reason.clone()), Some(reason));
    };

    match utter_stt::VoskEngine::load(std::path::Path::new(dir)) {
        Ok(engine) => (Box::new(engine), None),
        Err(e) => {
            let reason = format!("failed to load vosk model at \"{dir}\": {e}");
            (unavailable_engine(reason.clone()), Some(reason))
        }
    }
}

#[cfg(not(feature = "vosk"))]
fn build_vosk(_model_dir: Option<&str>) -> (Box<dyn SttEngine>, Option<String>) {
    let reason = "this build was compiled without vosk support; switch engines in Settings, \
                   or install a build with the vosk feature enabled"
        .to_string();
    (unavailable_engine(reason.clone()), Some(reason))
}

/// A generous but bounded default for the cloud engine's HTTP timeout:
/// `Settings` has no per-request timeout for speech-to-text (only refine has
/// one), and a single-utterance transcription call is not expected to run
/// long.
const CLOUD_STT_TIMEOUT: Duration = Duration::from_secs(30);

fn build_cloud(cfg: &CloudSttCfg) -> (Box<dyn SttEngine>, Option<String>) {
    let api_key = keyring_password(STT_KEY_SERVICE);
    let notice = api_key.is_none().then(|| {
        "no cloud speech-to-text API key configured; open Settings > Engine to add one".to_string()
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

fn build_refiner(cfg: &RefineCfg, dictionary_terms: Vec<String>) -> Option<Box<dyn TextRefiner>> {
    if !refine_configured(cfg) {
        return None;
    }

    let api_key = keyring_password(REFINE_KEY_SERVICE);
    Some(Box::new(LlmRefiner::new(
        LlmConfig {
            base_url: cfg.base_url.clone(),
            api_key,
            model: cfg.model.clone(),
            timeout: Duration::from_secs(cfg.timeout_secs),
        },
        dictionary_terms,
    )))
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

/// Starts the hotkey monitor thread for `hotkey` and returns the receiver
/// side of its event channel, plus a notice if capture couldn't be started
/// (an invalid chord, or missing permissions) — the runtime still boots with
/// no hotkey rather than failing outright.
///
/// The channel's sender is either owned by the spawned `HotkeySource` thread
/// (which runs, and keeps it alive, until a later `save_settings` supersedes
/// it via the generation counter — see `utter_inject::create_source`) or, on
/// failure, deliberately leaked: with no thread to own it, dropping it
/// instead would make every future `select!` in the runtime worker see the
/// channel as immediately "ready" with a disconnect error, spinning the
/// worker thread at 100% CPU forever. One leaked `Sender` per failed (re)boot
/// is an intentionally rare, negligible cost next to that.
fn spawn_hotkey_source(hotkey: &str) -> (Receiver<HotkeyEvent>, Option<String>) {
    let (tx, rx) = unbounded::<HotkeyEvent>();

    let spec = match parse_hotkey(hotkey) {
        Ok(spec) => spec,
        Err(e) => {
            std::mem::forget(tx);
            return (
                rx,
                Some(format!(
                    "invalid hotkey \"{hotkey}\": {e}; dictation has no hotkey until this is \
                     fixed in Settings"
                )),
            );
        }
    };

    match create_source(&spec) {
        Ok(source) => {
            thread::Builder::new()
                .name("utter-hotkey".to_string())
                .spawn(move || source.run(tx))
                .expect("failed to spawn the utter-hotkey source thread");
            (rx, None)
        }
        Err(e) => {
            std::mem::forget(tx);
            (
                rx,
                Some(format!(
                    "failed to start hotkey capture: {e}; check input group / uinput \
                     permissions in Settings > Permissions"
                )),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_label_matches_each_kind() {
        assert_eq!(engine_label(EngineKind::Whisper), "whisper");
        assert_eq!(engine_label(EngineKind::Vosk), "vosk");
        assert_eq!(engine_label(EngineKind::Cloud), "cloud");
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
    fn invalid_hotkey_boots_without_a_source_and_queues_a_notice() {
        let (rx, notice) = spawn_hotkey_source("not+a+real+hotkey+++");
        assert!(notice.is_some());
        assert!(notice.unwrap().contains("invalid hotkey"));
        // No source thread was spawned, so nothing is ever sent; the
        // channel must read as merely empty, never disconnected (see this
        // function's doc comment for why disconnection would matter).
        assert_eq!(rx.try_recv(), Err(crossbeam_channel::TryRecvError::Empty));
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

    #[cfg(not(feature = "vosk"))]
    #[test]
    fn vosk_without_the_feature_degrades_with_a_notice() {
        let (mut engine, notice) = build_vosk(Some("/tmp/does-not-matter"));

        let notice = notice.expect("a build without vosk support should produce a notice");
        assert!(notice.contains("vosk"));

        let err = engine
            .begin(&TranscribeOptions::default())
            .expect_err("an unavailable engine must fail begin() informatively");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }
}
