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
/// full-precision `encoder.onnx`; upstream does not normalize the two to one
/// shared name.
const ENCODER_CANDIDATES: [&str; 2] = ["encoder.int8.onnx", "encoder.onnx"];

/// Configuration for [`SherpaOfflineEngine::load`].
#[derive(Debug, Clone, Default)]
pub struct SherpaConfig {
    /// Number of onnxruntime inference threads. Clamped to at least one.
    pub num_threads: usize,
    /// Dictionary terms to bias recognition towards. Only takes effect once
    /// decoding uses `modified_beam_search` — see [`decoding_method`].
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
    /// a path is an easy mistake that has bitten this codebase before (v0.1).
    ///
    /// # Errors
    /// Returns [`SttError::ModelNotFound`] if `dir` does not exist, or if any
    /// of the expected encoder/decoder/joiner/tokens files are missing from
    /// it. Returns [`SttError::Engine`] if `cfg.hotwords` contains an
    /// interior null byte, if any resolved path is not valid UTF-8, or if
    /// sherpa-onnx refuses to build a recognizer from files that are all
    /// present (a corrupt or truncated download, the wrong ONNX format, or a
    /// model-family mismatch) — by the time that call happens every expected
    /// file has already been confirmed to exist, so a rejection there is an
    /// engine problem, not a missing-model one.
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
                    encoder: Some(path_to_string(&encoder)?),
                    decoder: Some(path_to_string(&decoder)?),
                    joiner: Some(path_to_string(&joiner)?),
                },
                tokens: Some(path_to_string(&tokens)?),
                num_threads: cfg.num_threads.clamp(1, i32::MAX as usize) as i32,
                // Every model in the catalog (GigaAM-v3, Parakeet English) is
                // a NeMo transducer export; without this hint sherpa-onnx
                // assumes the icefall transducer layout and fails to load.
                model_type: Some("nemo_transducer".to_string()),
                ..Default::default()
            },
            // Greedy unless the dictionary actually has terms to bias towards.
            // Safe to apply unconditionally here: this function only ever
            // builds a transducer config above (encoder, decoder and joiner
            // are all required to exist by this point), and transducer is
            // the one model family `decoding_method` assumes — see its doc
            // comment for why that assumption matters.
            decoding_method: Some(decoding_method(&cfg.hotwords).to_string()),
            ..Default::default()
        };

        // Every expected file is already confirmed present above, so a
        // rejection here means sherpa-onnx itself refused their contents
        // (corrupt/truncated download, wrong format, family mismatch) —
        // that is an engine failure, not a missing-model one.
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            SttError::Engine(format!(
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
///
/// # Errors
/// Returns [`SttError::Engine`] if `path` is not valid UTF-8, rather than
/// silently lossy-converting it into a path that would no longer point at
/// the file on disk.
fn path_to_string(path: &Path) -> Result<String, SttError> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        SttError::Engine(format!("model path is not valid UTF-8: {}", path.display()))
    })
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

/// Chooses sherpa-onnx's `decoding_method` from whether `hotwords` is empty.
///
/// Per the upstream hotwords guide
/// <https://k2-fsa.github.io/sherpa/onnx/hotwords/index.html>, sherpa-onnx's
/// default `"greedy_search"` decoder ignores hotwords entirely; using them
/// requires switching to `"modified_beam_search"`. Beam search is
/// meaningfully slower than greedy search, and most users have an empty
/// dictionary, so this is a policy rather than a setting: nothing configures
/// it directly, and it is derived purely from the dictionary's contents so
/// that beam search is only ever paid for by the users who actually benefit
/// from it.
///
/// This assumes the model being decoded is a transducer: the same upstream
/// page states that hotwords only work for that model family.
/// [`SherpaOfflineEngine::load`] only ever builds transducer configs (see
/// its doc comment), so that assumption always holds at its call site. A
/// loader that also accepted CTC models would need to detect that case
/// itself, log a warning naming the limitation, and force
/// `"greedy_search"` regardless of the dictionary — silently dropping
/// hotwords is safer than failing the load, but applying this function's
/// result unconditionally would silently ignore them instead of falling
/// back deliberately.
pub fn decoding_method(hotwords: &[String]) -> &'static str {
    if hotwords.is_empty() {
        "greedy_search"
    } else {
        "modified_beam_search"
    }
}

/// Half the machine's cores, at least one and at most four.
///
/// Saturating every core freezes the desktop exactly while the user is
/// waiting for text to appear, which is the worst possible moment.
pub fn default_threads(available: usize) -> usize {
    (available / 2).clamp(1, 4)
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
        // would make it indistinguishable from a draft engine.
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

    // No in-process test exercises `OfflineRecognizer::create` returning
    // `None` for present-but-invalid files (the `SttError::Engine` branch in
    // `load`). Both ways of constructing such a fixture were tried and both
    // crash the whole test binary rather than failing gracefully: a
    // malformed `tokens.txt` makes sherpa-onnx's C++ layer log and call
    // `exit()` directly (process exit status 255, no signal), and malformed
    // `.onnx` files make onnxruntime throw a C++ exception while parsing the
    // protobuf, which unwinds across the FFI boundary uncaught and aborts
    // the process (SIGABRT: "Rust cannot catch foreign exceptions"). Unlike
    // whisper.cpp's C API, sherpa-onnx does not appear to guarantee a
    // graceful `None`/error return for every malformed-input shape, so this
    // branch is verified by inspection and the doc comment on `load` rather
    // than by a test that would otherwise take down the whole suite.

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
    fn beam_search_is_only_paid_for_when_hotwords_exist() {
        assert_eq!(decoding_method(&[]), "greedy_search");
        assert_eq!(
            decoding_method(&["PostgreSQL".to_string()]),
            "modified_beam_search",
            "hotwords require beam search; without them the user must not pay for it"
        );
    }

    #[test]
    fn thread_default_leaves_headroom_for_the_desktop() {
        assert_eq!(default_threads(1), 1, "never zero");
        assert_eq!(default_threads(2), 1);
        assert_eq!(default_threads(8), 4);
        assert_eq!(default_threads(32), 4, "capped: more threads stop helping");
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
