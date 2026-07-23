//! Vosk-backed [`SttEngine`] adapter.
//!
//! Unlike whisper.cpp, Vosk is a genuinely streaming engine: [`begin`] opens a
//! fresh [`Recognizer`] against the loaded [`Model`], [`feed`] pushes audio
//! into it via [`Recognizer::accept_waveform`] and may surface partial
//! transcripts as they firm up, and [`finish`] asks the recognizer to flush
//! and finalize.
//!
//! Two things [`TranscribeOptions`] offers have no Vosk equivalent and are
//! deliberately ignored here:
//! - `initial_prompt` — Vosk has no notion of a decoding hint/prompt.
//! - `language` — determined entirely by which model directory was loaded
//!   via [`VoskEngine::load`], not by anything passed at transcription time.
//!
//! Recognizers are also created with no grammar restriction (plain
//! [`Recognizer::new`], never [`Recognizer::new_with_grammar`]): utter has no
//! fixed phrase list to constrain decoding to.
//!
//! [`begin`]: SttEngine::begin
//! [`feed`]: SttEngine::feed
//! [`finish`]: SttEngine::finish

use std::path::Path;

use utter_core::{SttEngine, SttError, TranscribeOptions, Transcript};
use vosk::{DecodingState, Model, Recognizer};

/// A Vosk speech-to-text engine, loaded from a single model directory and
/// reusable across many begin/feed/finish transcription cycles.
pub struct VoskEngine {
    model: Model,
    /// `Some` between `begin` and `finish`; `None` otherwise. Doubles as the
    /// "has begin() been called yet" flag that `feed`/`finish` check.
    recognizer: Option<Recognizer>,
    /// The last partial transcript emitted from `feed` during the
    /// in-progress utterance, used to suppress repeats of unchanged partials.
    /// Cleared on `begin` and `finish`.
    last_partial: String,
}

impl VoskEngine {
    /// Loads a Vosk model from `model_dir`.
    ///
    /// # Errors
    /// Returns [`SttError::ModelNotFound`] if `model_dir` does not exist, or
    /// [`SttError::Engine`] if Vosk rejects the directory (e.g. it exists but
    /// is not a valid model).
    pub fn load(model_dir: &Path) -> Result<Self, SttError> {
        if !model_dir.is_dir() {
            return Err(SttError::ModelNotFound(model_dir.display().to_string()));
        }

        let path = model_dir.to_str().ok_or_else(|| {
            SttError::Engine(format!(
                "model path is not valid UTF-8: {}",
                model_dir.display()
            ))
        })?;

        let model = Model::new(path).ok_or_else(|| {
            SttError::Engine(format!(
                "failed to load vosk model at {}",
                model_dir.display()
            ))
        })?;

        Ok(Self {
            model,
            recognizer: None,
            last_partial: String::new(),
        })
    }
}

impl SttEngine for VoskEngine {
    /// Opens a fresh [`Recognizer`] at [`utter_core::SAMPLE_RATE`]. `opts` is
    /// ignored: see the module-level docs for why.
    fn begin(&mut self, _opts: &TranscribeOptions) -> Result<(), SttError> {
        let recognizer = Recognizer::new(&self.model, utter_core::SAMPLE_RATE as f32)
            .ok_or_else(|| SttError::Engine("failed to create vosk recognizer".to_string()))?;
        begin_session(&mut self.recognizer, &mut self.last_partial, recognizer);
        Ok(())
    }

    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError> {
        feed_session(&mut self.recognizer, &mut self.last_partial, samples)
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        let mut recognizer = take_session(&mut self.recognizer, &mut self.last_partial)?;
        let text = final_result_text(&mut recognizer)?;
        Ok(Transcript {
            text,
            // Vosk gives no language detection/identification.
            language: None,
        })
    }
}

/// Starts a new utterance: stores the freshly created `recognizer` and
/// clears any partial transcript left over from a previous begin/feed/finish
/// cycle (or from a `begin` that was never followed by `finish`).
///
/// Split out from [`SttEngine::begin`] as a free function, for the same
/// testability reason as [`feed_session`] and [`take_session`].
fn begin_session(
    session: &mut Option<Recognizer>,
    last_partial: &mut String,
    recognizer: Recognizer,
) {
    *session = Some(recognizer);
    last_partial.clear();
}

/// Pushes `samples` into the in-progress recognizer and decides whether to
/// surface a partial transcript.
///
/// Split out from [`SttEngine::feed`] as a free function, taking `session`
/// and `last_partial` directly instead of `&mut VoskEngine`, so the
/// begin/feed-ordering rule can be unit tested without loading a real Vosk
/// model.
fn feed_session(
    session: &mut Option<Recognizer>,
    last_partial: &mut String,
    samples: &[i16],
) -> Result<Option<String>, SttError> {
    let recognizer = session.as_mut().ok_or_else(|| {
        SttError::Engine("feed called before begin: no transcription in progress".to_string())
    })?;

    // `accept_waveform`'s `Err` case (`AcceptWaveformError::BufferTooLong`)
    // only fires for a `data` slice too long to fit in a C `int`, which never
    // happens for the short chunks `feed` is called with in practice, but is
    // still mapped rather than unwrapped.
    let state = recognizer
        .accept_waveform(samples)
        .map_err(|e| SttError::Engine(format!("vosk failed to accept waveform: {e}")))?;

    check_decoding_state(state)?;

    let partial = recognizer.partial_result().partial;
    Ok(track_partial(last_partial, partial))
}

/// Turns a [`DecodingState`] from `accept_waveform` into an error if the
/// decoder failed, or `Ok(())` to keep going.
///
/// `Failed` means the decoder hit an internal exception (see `vosk_api.h`'s
/// docs for `vosk_recognizer_accept_waveform`, which documents a `-1` return
/// as "exception occured"). It is a real error, not a normal decoding state
/// to shrug off: swallowing it would let every later `feed` on the same
/// recognizer look normal while quietly producing garbage, and `finish`
/// would return an empty/stale transcript with no error at all. `Running`/
/// `Finalized` both just mean "keep going" as far as `feed`'s contract (an
/// optional partial) is concerned.
///
/// Split out as its own function — separate from [`feed_session`] — because
/// `DecodingState` is a plain, fieldless, publicly-constructible enum (unlike
/// `Recognizer`, which needs a real model and `libvosk` to construct), so
/// this piece of logic can be unit tested directly against all three
/// variants.
fn check_decoding_state(state: DecodingState) -> Result<(), SttError> {
    match state {
        DecodingState::Failed => Err(SttError::Engine(
            "vosk decoder reported a failure while accepting waveform data".to_string(),
        )),
        DecodingState::Running | DecodingState::Finalized => Ok(()),
    }
}

/// Takes ownership of the in-progress recognizer for `finish`, resetting the
/// session to its "no transcription in progress" state so the engine is
/// ready for a fresh begin/feed/finish cycle.
///
/// Split out for the same testability reason as [`feed_session`].
fn take_session(
    session: &mut Option<Recognizer>,
    last_partial: &mut String,
) -> Result<Recognizer, SttError> {
    let recognizer = session.take().ok_or_else(|| {
        SttError::Engine("finish called before begin: no transcription in progress".to_string())
    })?;
    last_partial.clear();
    Ok(recognizer)
}

/// Decides whether newly observed partial text should be emitted as
/// `Some(...)` from [`SttEngine::feed`], per the port contract: a partial is
/// surfaced only when it is non-empty *and* different from the last one
/// emitted during this utterance. Updates `last_partial` when it is.
///
/// This is the one piece of session logic with no dependency on the `vosk`
/// crate's types at all, so it is exercised directly (no `Recognizer`, model,
/// or `libvosk` needed at runtime — only at link time, since this whole
/// module is compiled only under the `vosk` feature).
fn track_partial(last_partial: &mut String, partial: &str) -> Option<String> {
    if partial.is_empty() || partial == last_partial {
        return None;
    }
    last_partial.clear();
    last_partial.push_str(partial);
    Some(partial.to_string())
}

/// Flushes and finalizes `recognizer`, returning its final transcript text.
///
/// Vosk's `set_max_alternatives` defaults to (and is never changed from) 0,
/// so `final_result()` always yields [`vosk::CompleteResult::Single`]; the
/// `Multiple` branch is unreachable in practice but still handled as an
/// engine error rather than panicking, in case that ever changes.
fn final_result_text(recognizer: &mut Recognizer) -> Result<String, SttError> {
    let single = recognizer.final_result().single().ok_or_else(|| {
        SttError::Engine(
            "vosk returned a multiple-alternative result, but VoskEngine never enables \
             set_max_alternatives"
                .to_string(),
        )
    })?;
    Ok(single.text.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Like `Result::expect_err`, but doesn't require the `Ok` type to be
    /// `Debug`. `VoskEngine` and `vosk::Recognizer` wrap opaque FFI handles
    /// that don't implement it, which `expect_err` otherwise requires.
    fn expect_err<T, E>(result: Result<T, E>, msg: &str) -> E {
        match result {
            Ok(_) => panic!("{msg}"),
            Err(e) => e,
        }
    }

    #[test]
    fn load_missing_dir_returns_model_not_found() {
        let path = Path::new("/nonexistent/path/to/vosk-model");
        let err = expect_err(VoskEngine::load(path), "missing model dir must not load");

        match err {
            SttError::ModelNotFound(msg) => {
                assert!(
                    msg.contains(path.to_str().expect("test path is valid UTF-8")),
                    "error message {msg:?} should contain the missing path"
                );
            }
            other => panic!("expected SttError::ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn load_present_but_invalid_model_returns_engine_error() {
        let dir = std::env::temp_dir().join("utter-stt-test-not-a-vosk-model");
        std::fs::create_dir_all(&dir).expect("failed to create test fixture dir");

        let err = expect_err(VoskEngine::load(&dir), "empty dir must not load as a model");
        let _ = std::fs::remove_dir_all(&dir);

        // Vosk cannot distinguish "missing directory" from "not a valid
        // model" at this call, so a present-but-invalid directory is
        // reported as a generic engine error rather than `ModelNotFound`.
        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn feed_before_begin_returns_engine_error() {
        let mut session: Option<Recognizer> = None;
        let mut last_partial = String::new();

        let err = expect_err(
            feed_session(&mut session, &mut last_partial, &[0i16; 10]),
            "feed before begin must fail",
        );

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn finish_before_begin_returns_engine_error() {
        let mut session: Option<Recognizer> = None;
        let mut last_partial = String::new();

        let err = expect_err(
            take_session(&mut session, &mut last_partial),
            "finish before begin must fail",
        );

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn check_decoding_state_running_continues() {
        assert!(check_decoding_state(DecodingState::Running).is_ok());
    }

    #[test]
    fn check_decoding_state_finalized_continues() {
        assert!(check_decoding_state(DecodingState::Finalized).is_ok());
    }

    #[test]
    fn check_decoding_state_failed_returns_engine_error() {
        let err = expect_err(
            check_decoding_state(DecodingState::Failed),
            "Failed decoding state must be reported as an error",
        );

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn track_partial_suppresses_empty_partial() {
        let mut last_partial = String::new();
        assert_eq!(track_partial(&mut last_partial, ""), None);
        assert_eq!(last_partial, "");
    }

    #[test]
    fn track_partial_emits_first_nonempty_partial() {
        let mut last_partial = String::new();
        assert_eq!(
            track_partial(&mut last_partial, "hello"),
            Some("hello".to_string())
        );
        assert_eq!(last_partial, "hello");
    }

    #[test]
    fn track_partial_suppresses_unchanged_partial() {
        let mut last_partial = "hello".to_string();
        assert_eq!(track_partial(&mut last_partial, "hello"), None);
        assert_eq!(last_partial, "hello");
    }

    #[test]
    fn track_partial_emits_changed_partial() {
        let mut last_partial = "hello".to_string();
        assert_eq!(
            track_partial(&mut last_partial, "hello there"),
            Some("hello there".to_string())
        );
        assert_eq!(last_partial, "hello there");
    }

    #[test]
    fn track_partial_suppresses_partial_that_becomes_empty_again() {
        let mut last_partial = "hello".to_string();
        assert_eq!(track_partial(&mut last_partial, ""), None);
        // Not cleared: an empty partial is never "emitted", so the last
        // *emitted* partial is still "hello" until something else changes.
        assert_eq!(last_partial, "hello");
    }

    /// Manual, network- and model-dependent smoke test: downloads the small
    /// English Vosk model (once, cached in the OS temp dir) and runs the
    /// full begin/feed/finish pipeline over one second of a synthetic sine
    /// wave. It is not speech, so the assertion only checks that inference
    /// completes without panicking or erroring — not on any particular
    /// transcribed text.
    ///
    /// Deliberately `#[ignore]`d: it needs network access, downloads
    /// ~40 MB, and requires `libvosk` to be linked (see
    /// `scripts/setup-libvosk.sh`). Run manually with:
    /// `RUSTFLAGS="-L <libdir>" cargo test -p utter-stt --features vosk -- --ignored --nocapture transcribes_sine_wave`
    #[test]
    #[ignore]
    fn transcribes_sine_wave() {
        let model_dir = ensure_small_model_downloaded();
        let mut engine = VoskEngine::load(&model_dir).expect("failed to load vosk model");

        let sine = generate_sine_wave(1.0, 440.0);

        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        let partial = engine.feed(&sine).expect("feed failed");
        println!("partial: {partial:?}");
        let transcript = engine.finish().expect("finish failed");

        println!("transcript: {transcript:?}");
    }

    /// Generates `seconds` of a mono 16 kHz `i16` sine wave at `hz`, at a
    /// quarter of full scale.
    fn generate_sine_wave(seconds: f32, hz: f32) -> Vec<i16> {
        let sample_rate = utter_core::SAMPLE_RATE as f32;
        let n = (sample_rate * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate;
                let amplitude = (t * hz * std::f32::consts::TAU).sin() * 0.25;
                (amplitude * i16::MAX as f32) as i16
            })
            .collect()
    }

    /// Downloads and unpacks the small English Vosk model
    /// (`vosk-model-small-en-us-0.15`) via the system `curl`/`unzip`
    /// binaries into the OS temp dir, skipping both steps if the model
    /// directory already exists there from a previous run. Shells out
    /// instead of adding HTTP client/zip dependencies, since this is a
    /// manual-only, `#[ignore]`d test.
    fn ensure_small_model_downloaded() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("vosk-model-small-en-us-0.15");
        if !dir.is_dir() {
            let zip_path = std::env::temp_dir().join("utter-stt-test-vosk-small-model.zip");
            if !zip_path.is_file() {
                let status = std::process::Command::new("curl")
                    .args(["-L", "-sS", "--fail", "-o"])
                    .arg(&zip_path)
                    .arg("https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip")
                    .status()
                    .expect("failed to invoke curl to download the small model");
                assert!(status.success(), "curl failed to download the small model");
            }
            let status = std::process::Command::new("unzip")
                .args(["-q", "-o"])
                .arg(&zip_path)
                .args(["-d"])
                .arg(std::env::temp_dir())
                .status()
                .expect("failed to invoke unzip to extract the small model");
            assert!(status.success(), "unzip failed to extract the small model");
        }
        dir
    }
}
