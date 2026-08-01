//! sherpa-onnx-backed offline [`SttEngine`] adapter.
//!
//! sherpa-onnx's offline recognizer is a batch API: the whole utterance is
//! handed to it in one `accept_waveform` call rather than streamed
//! incrementally. [`SherpaOfflineEngine`] therefore buffers samples during
//! [`SttEngine::feed`] and runs the full decode in [`SttEngine::finish`]; it
//! never emits partial transcripts. (Phase 3 adds a separate streaming
//! engine over sherpa-onnx's *online* recognizer for that.)

use std::path::{Path, PathBuf};

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
};
use utter_core::{SttEngine, SttError, TranscribeOptions, Transcript, SAMPLE_RATE};

/// Filenames tried for the transducer encoder, in order.
///
/// The catalog's two models package the same three-file transducer layout
/// under different encoder filenames: GigaAM-v3 ships a quantized
/// `encoder.int8.onnx`, while the English Parakeet entry ships a
/// full-precision `encoder.onnx` (see the Task 3 catalog notes — the two are
/// deliberately not normalized to one shared name upstream).
const ENCODER_CANDIDATES: [&str; 2] = ["encoder.int8.onnx", "encoder.onnx"];

/// Configuration for [`SherpaOfflineEngine::load`].
#[derive(Debug, Clone, Default)]
pub struct SherpaConfig {
    /// Number of onnxruntime inference threads. Clamped to at least one.
    pub num_threads: usize,
    /// Dictionary terms to bias recognition towards (spec D9/D13 hotwords).
    /// Only takes effect once decoding uses `modified_beam_search`.
    pub hotwords: Vec<String>,
}

/// A sherpa-onnx offline speech-to-text engine, loaded from a directory of
/// transducer model files and reusable across many begin/feed/finish
/// transcription cycles.
pub struct SherpaOfflineEngine {
    /// The loaded recognizer. Only `None` for the `test_engine` double used
    /// in this module's tests to exercise `begin`/`feed` without a real
    /// model; [`SherpaOfflineEngine::load`] is the sole public constructor
    /// and always sets it to `Some`.
    recognizer: Option<OfflineRecognizer>,
    /// Joined hotwords string for `create_stream_with_hotwords`, or `None`
    /// when the dictionary is empty and a plain stream should be used.
    hotwords: Option<String>,
    /// `Some` between `begin` and `finish`; `None` otherwise. Doubles as the
    /// "has begin() been called yet" flag that `feed`/`finish` check.
    opts: Option<TranscribeOptions>,
    buffer: Vec<i16>,
}

// `OfflineRecognizer` does not implement `Debug` (it wraps a raw FFI
// pointer), so this is written by hand instead of derived, reporting only
// whether a recognizer is loaded.
impl std::fmt::Debug for SherpaOfflineEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SherpaOfflineEngine")
            .field("loaded", &self.recognizer.is_some())
            .field("hotwords", &self.hotwords)
            .field("in_progress", &self.opts.is_some())
            .finish()
    }
}

impl SherpaOfflineEngine {
    /// Loads a sherpa-onnx offline transducer model from `dir`.
    ///
    /// `dir` must be a model *directory* as resolved by
    /// `ModelManager::path_for` — never a bare catalog id. Treating an id as
    /// a path is the exact mistake that broke Vosk model resolution in v0.1.
    ///
    /// # Errors
    /// Returns [`SttError::ModelNotFound`] if `dir` does not exist, if any of
    /// the expected encoder/decoder/joiner/tokens files are missing from it,
    /// or if sherpa-onnx itself refuses to build a recognizer from the
    /// resolved files. Returns [`SttError::Engine`] if `cfg.hotwords`
    /// contains an interior null byte.
    ///
    /// `OfflineRecognizer::create` reports failure as `None` rather than an
    /// error value, so in that last case the path that was tried is the only
    /// diagnostic available and is included in the message.
    pub fn load(dir: &Path, cfg: SherpaConfig) -> Result<Self, SttError> {
        if !dir.is_dir() {
            return Err(SttError::ModelNotFound(dir.display().to_string()));
        }

        let hotwords = build_hotwords_arg(&cfg.hotwords)?;

        let encoder = resolve_required_file(dir, &ENCODER_CANDIDATES)?;
        let decoder = resolve_required_file(dir, &["decoder.onnx"])?;
        let joiner = resolve_required_file(dir, &["joiner.onnx"])?;
        let tokens = resolve_required_file(dir, &["tokens.txt"])?;

        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Some(path_to_string(&encoder)),
                    decoder: Some(path_to_string(&decoder)),
                    joiner: Some(path_to_string(&joiner)),
                },
                tokens: Some(path_to_string(&tokens)),
                num_threads: cfg.num_threads.max(1) as i32,
                // Every model in the catalog (GigaAM-v3, Parakeet English) is
                // a NeMo transducer export; without this hint sherpa-onnx
                // assumes the icefall transducer layout and fails to load.
                model_type: Some("nemo_transducer".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            SttError::ModelNotFound(format!(
                "sherpa-onnx rejected the model in {}",
                dir.display()
            ))
        })?;

        Ok(Self {
            recognizer: Some(recognizer),
            hotwords,
            opts: None,
            buffer: Vec::new(),
        })
    }

    /// Returns the loaded recognizer.
    ///
    /// # Panics
    /// Only if called on an engine that was never loaded via [`Self::load`]
    /// (the `test_engine` double in this module's tests, which never calls
    /// `finish`).
    fn recognizer(&self) -> &OfflineRecognizer {
        self.recognizer
            .as_ref()
            .expect("invariant: SherpaOfflineEngine::load always sets recognizer to Some")
    }
}

impl SttEngine for SherpaOfflineEngine {
    fn begin(&mut self, opts: &TranscribeOptions) -> Result<(), SttError> {
        begin_session(&mut self.opts, &mut self.buffer, opts);
        Ok(())
    }

    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError> {
        feed_session(&self.opts, &mut self.buffer, samples)
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        let (opts, buffer) = take_session(&mut self.opts, &mut self.buffer)?;
        run_offline_decode(self.recognizer(), self.hotwords.as_deref(), &opts, &buffer)
    }
}

/// Starts a new utterance: records `new_opts` and clears any samples left
/// over from a previous begin/feed/finish cycle (or from a `begin` that was
/// never followed by `finish`).
///
/// Split out from [`SttEngine::begin`] as a free function, for the same
/// testability reason as [`feed_session`] and [`take_session`].
fn begin_session(
    opts: &mut Option<TranscribeOptions>,
    buffer: &mut Vec<i16>,
    new_opts: &TranscribeOptions,
) {
    *opts = Some(new_opts.clone());
    buffer.clear();
}

/// Buffers `samples` for the in-progress utterance.
///
/// Split out from [`SttEngine::feed`] as a free function, taking `opts` and
/// `buffer` directly instead of `&mut SherpaOfflineEngine`, so the
/// begin/feed-ordering rule can be unit tested without loading a real model.
fn feed_session(
    opts: &Option<TranscribeOptions>,
    buffer: &mut Vec<i16>,
    samples: &[i16],
) -> Result<Option<String>, SttError> {
    if opts.is_none() {
        return Err(SttError::Engine(
            "feed called before begin: no transcription in progress".to_string(),
        ));
    }
    buffer.extend_from_slice(samples);
    Ok(None) // sherpa-onnx's offline recognizer is a batch API: never emits partials.
}

/// Takes ownership of the buffered options and samples for `finish`,
/// resetting both to their "no transcription in progress" state so the
/// engine is ready for a fresh begin/feed/finish cycle.
///
/// Split out for the same testability reason as [`feed_session`].
fn take_session(
    opts: &mut Option<TranscribeOptions>,
    buffer: &mut Vec<i16>,
) -> Result<(TranscribeOptions, Vec<i16>), SttError> {
    let opts = opts.take().ok_or_else(|| {
        SttError::Engine("finish called before begin: no transcription in progress".to_string())
    })?;
    Ok((opts, std::mem::take(buffer)))
}

/// Converts buffered `i16` samples to `f32`, decodes them with `recognizer`
/// in a single offline pass (optionally biased by `hotwords`), and returns
/// the resulting text as a [`Transcript`].
fn run_offline_decode(
    recognizer: &OfflineRecognizer,
    hotwords: Option<&str>,
    opts: &TranscribeOptions,
    samples: &[i16],
) -> Result<Transcript, SttError> {
    // i16 -> f32 in [-1.0, 1.0), the format sherpa-onnx's feature extractor expects.
    let audio: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

    let stream = match hotwords {
        Some(hotwords) => recognizer.create_stream_with_hotwords(hotwords),
        None => recognizer.create_stream(),
    };

    stream.accept_waveform(SAMPLE_RATE as i32, &audio);
    recognizer.decode(&stream);

    let result = stream.get_result().ok_or_else(|| {
        SttError::Engine("sherpa-onnx produced no recognition result".to_string())
    })?;

    Ok(Transcript {
        text: result.text.trim().to_string(),
        language: opts.language.clone(),
    })
}

/// Locates the first of `candidates` that exists as a file inside `dir`.
///
/// Some artifacts vary in filename between catalog entries (see
/// [`ENCODER_CANDIDATES`]), so lookups try each candidate name in turn
/// rather than assuming one fixed name; single-name lookups just pass a
/// one-element slice.
fn resolve_required_file(dir: &Path, candidates: &[&str]) -> Result<PathBuf, SttError> {
    candidates
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            SttError::ModelNotFound(format!("{}: expected one of {candidates:?}", dir.display()))
        })
}

/// Renders `path` as a `String` for the sherpa-onnx config, which takes file
/// paths as owned UTF-8 strings rather than `Path`s.
fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Joins `hotwords` into the single newline-separated string
/// `OfflineRecognizer::create_stream_with_hotwords` expects, or `None` if
/// there are no hotwords to bias recognition towards.
///
/// # Errors
/// Returns [`SttError::Engine`] if any hotword contains an interior null
/// byte: sherpa-onnx converts the joined string to a `CString` internally
/// and panics on one, so this turns that potential panic into an ordinary
/// error up front (mirrors [`crate::whisper`]'s `reject_null_byte`).
fn build_hotwords_arg(hotwords: &[String]) -> Result<Option<String>, SttError> {
    if hotwords.is_empty() {
        return Ok(None);
    }
    let joined = hotwords.join("\n");
    if joined.contains('\0') {
        return Err(SttError::Engine(
            "hotwords must not contain a null byte".to_string(),
        ));
    }
    Ok(Some(joined))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Builds a `SherpaOfflineEngine` double with no loaded recognizer, for
    /// tests that only exercise `begin`/`feed` and never call `finish` (which
    /// would panic on this double — see [`SherpaOfflineEngine::recognizer`]).
    /// Loading a real model needs a downloaded catalog entry, which unit
    /// tests must not depend on.
    fn test_engine() -> SherpaOfflineEngine {
        SherpaOfflineEngine {
            recognizer: None,
            hotwords: None,
            opts: None,
            buffer: Vec::new(),
        }
    }

    #[test]
    fn loading_a_missing_model_directory_reports_model_not_found() {
        let err =
            SherpaOfflineEngine::load(Path::new("/nonexistent/model"), SherpaConfig::default())
                .expect_err("a missing model directory must not load");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }

    #[test]
    fn feed_buffers_without_producing_partials() {
        // The offline engine is batch: per the port contract it accumulates in
        // feed() and does all its work in finish(). Returning a partial here
        // would make it indistinguishable from a draft engine (spec D9).
        let mut engine = test_engine();
        assert_eq!(engine.begin(&TranscribeOptions::default()), Ok(()));
        assert_eq!(engine.feed(&[0i16; 1600]), Ok(None));
    }

    #[test]
    fn loading_a_directory_missing_required_files_reports_model_not_found() {
        let dir = std::env::temp_dir().join("utter-stt-test-sherpa-empty-model");
        std::fs::create_dir_all(&dir).expect("failed to create empty test model dir");

        let err = SherpaOfflineEngine::load(&dir, SherpaConfig::default())
            .expect_err("a model directory missing its files must not load");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(matches!(err, SttError::ModelNotFound(_)), "got {err:?}");
    }

    #[test]
    fn feed_before_begin_returns_engine_error() {
        let opts: Option<TranscribeOptions> = None;
        let mut buffer = Vec::new();

        let err =
            feed_session(&opts, &mut buffer, &[0i16; 10]).expect_err("feed before begin must fail");

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
        assert!(buffer.is_empty(), "samples must not be buffered");
    }

    #[test]
    fn finish_before_begin_returns_engine_error() {
        let mut opts: Option<TranscribeOptions> = None;
        let mut buffer = Vec::new();

        let err = take_session(&mut opts, &mut buffer).expect_err("finish before begin must fail");

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn feed_after_begin_buffers_samples() {
        let opts = Some(TranscribeOptions::default());
        let mut buffer = Vec::new();

        let result =
            feed_session(&opts, &mut buffer, &[1, 2, 3]).expect("feed after begin must succeed");

        assert_eq!(
            result, None,
            "sherpa-onnx offline engine never emits partial transcripts"
        );
        assert_eq!(buffer, vec![1, 2, 3]);
    }

    #[test]
    fn begin_again_clears_buffer_from_previous_utterance() {
        let mut opts: Option<TranscribeOptions> = None;
        let mut buffer = Vec::new();

        begin_session(&mut opts, &mut buffer, &TranscribeOptions::default());
        feed_session(&opts, &mut buffer, &[1, 2, 3]).expect("feed after begin must succeed");
        assert_eq!(buffer, vec![1, 2, 3]);

        begin_session(&mut opts, &mut buffer, &TranscribeOptions::default());

        assert!(
            buffer.is_empty(),
            "begin must clear samples left over from a previous begin/feed cycle"
        );
        assert!(opts.is_some(), "begin must record the new opts");
    }

    #[test]
    fn take_session_resets_opts_and_buffer() {
        let mut opts = Some(TranscribeOptions::default());
        let mut buffer = vec![1, 2, 3];

        let (_, taken) =
            take_session(&mut opts, &mut buffer).expect("finish after begin must succeed");

        assert_eq!(taken, vec![1, 2, 3]);
        assert!(opts.is_none(), "opts must be cleared after finish");
        assert!(buffer.is_empty(), "buffer must be cleared after finish");
    }

    #[test]
    fn build_hotwords_arg_of_empty_list_is_none() {
        assert_eq!(build_hotwords_arg(&[]).expect("must succeed"), None);
    }

    #[test]
    fn build_hotwords_arg_joins_with_newlines() {
        let hotwords = vec!["PostgreSQL".to_string(), "Kubernetes".to_string()];
        assert_eq!(
            build_hotwords_arg(&hotwords).expect("must succeed"),
            Some("PostgreSQL\nKubernetes".to_string())
        );
    }

    #[test]
    fn build_hotwords_arg_rejects_null_byte() {
        let hotwords = vec!["bad\0word".to_string()];
        let err = build_hotwords_arg(&hotwords).expect_err("null byte must be rejected");
        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn resolve_required_file_tries_every_candidate_in_order() {
        let dir = std::env::temp_dir().join("utter-stt-test-sherpa-candidates");
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        let present = dir.join("encoder.onnx");
        std::fs::write(&present, b"x").expect("failed to write test fixture");

        let resolved = resolve_required_file(&dir, &ENCODER_CANDIDATES);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(resolved.expect("must resolve"), present);
    }
}
