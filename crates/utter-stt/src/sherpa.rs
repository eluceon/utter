//! sherpa-onnx-backed [`SttEngine`] adapters.
//!
//! sherpa-onnx's offline recognizer is a batch API: the whole utterance is
//! handed to it in one `accept_waveform` call rather than streamed
//! incrementally. [`SherpaOfflineEngine`] therefore buffers samples during
//! [`SttEngine::feed`] and runs the full decode in [`SttEngine::finish`]; it
//! never emits partial transcripts.
//!
//! [`SherpaStreamingEngine`] wraps sherpa-onnx's *online* recognizer instead:
//! each [`SttEngine::feed`] call decodes immediately and may return a
//! partial hypothesis, which is what drives the live preview HUD. It is a
//! draft engine — fast and streaming, but lower accuracy than the offline
//! models — so its output is never the text that gets injected.

use std::path::{Path, PathBuf};

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
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

/// Configuration for [`SherpaOfflineEngine::load`] and
/// [`SherpaStreamingEngine::load`]. Shared between the two engines rather
/// than duplicated: both load a transducer from a model directory and both
/// take the same knobs.
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
            // Both explicit, never inherited — see the constants' own doc
            // comments for the differing-defaults trap they exist to close.
            max_active_paths: MAX_ACTIVE_PATHS,
            hotwords_score: HOTWORDS_SCORE,
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
    /// Dictionary terms reach this engine as hotwords fixed at [`Self::load`]
    /// time, not through `opts.initial_prompt`: sherpa-onnx's offline API has
    /// no per-utterance prompt hook, so `opts.initial_prompt` is accepted for
    /// port-contract compatibility but ignored by design. This is the
    /// opposite lifecycle from whisper.cpp, which re-supplies it on every
    /// `begin()` — see `RuntimeDeps::dictionary_terms` for the full picture.
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
    let audio = samples_to_f32(samples);

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

/// Converts `i16` PCM samples to `f32` in `[-1.0, 1.0)`, the format
/// sherpa-onnx's feature extractor expects. Shared by the offline and
/// streaming engines, which otherwise each convert a `&[i16]` buffer just
/// before handing it to their respective `accept_waveform`.
fn samples_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| s as f32 / 32768.0).collect()
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
/// [`SherpaOfflineEngine::load`] and [`SherpaStreamingEngine::load`] only
/// ever build transducer configs (see their doc comments), so that
/// assumption always holds at both call sites. A loader that also accepted
/// CTC models would need to detect that case
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

/// Beam width for `modified_beam_search`, matching upstream sherpa-onnx's own
/// default.
///
/// Set explicitly at *both* recognizer call sites, and deliberately never left
/// to `..Default::default()`, because the two sherpa-onnx config types
/// disagree on it: `OfflineRecognizerConfig::default` uses `4` (commented "a
/// reasonable default" in the crate source) while `OnlineRecognizerConfig`
/// uses `0`. A beam width of zero is not merely a narrower search — it is an
/// empty hypothesis set, and [`decoding_method`] selects beam search for every
/// user whose dictionary is non-empty. Pinning it on both engines is what
/// stops the streaming loader from silently inheriting the one default that
/// breaks it.
const MAX_ACTIVE_PATHS: i32 = 4;

/// The score added to a hotword during `modified_beam_search`, matching
/// upstream sherpa-onnx's `--hotwords-score` default.
///
/// Pinned for the same reason as [`MAX_ACTIVE_PATHS`] but against a different
/// failure: *both* config types default this to `0.0`, which boosts a hotword
/// by nothing at all and makes passing hotwords a no-op. Leaving it at the
/// crate default silently renders the whole dictionary-terms-as-hotwords
/// feature inert rather than failing visibly.
const HOTWORDS_SCORE: f32 = 1.5;

/// Half the machine's cores, at least one and at most four.
///
/// Saturating every core freezes the desktop exactly while the user is
/// waiting for text to appear, which is the worst possible moment.
pub fn default_threads(available: usize) -> usize {
    (available / 2).clamp(1, 4)
}

/// A sherpa-onnx online (streaming) speech-to-text engine, loaded from a
/// directory of streaming transducer model files and reusable across many
/// begin/feed/finish transcription cycles.
///
/// Unlike [`SherpaOfflineEngine`], this decodes incrementally as samples
/// arrive in [`SttEngine::feed`], which is what lets it surface a partial
/// hypothesis while the user is still speaking.
pub struct SherpaStreamingEngine {
    /// The loaded recognizer. Only `None` for the `test_engine` double used
    /// in this module's tests to exercise the begin/feed/finish ordering
    /// rules without a real model; [`SherpaStreamingEngine::load`] is the
    /// sole public constructor and always sets it to `Some`.
    recognizer: Option<OnlineRecognizer>,
    /// Joined hotwords string for `create_stream_with_hotwords`, or `None`
    /// when the dictionary is empty and a plain stream should be used.
    hotwords: Option<String>,
    /// `Some` between `begin` and `finish`; `None` otherwise. Doubles as the
    /// "has begin() been called yet" flag that `feed`/`finish` check.
    opts: Option<TranscribeOptions>,
    /// The stream for the in-progress utterance. Created lazily on the first
    /// `feed()` call after `begin()` rather than in `begin()` itself, so
    /// `begin()` never needs the recognizer — that keeps the begin/feed
    /// ordering rules testable without a real model, the same way
    /// [`begin_session`] is for the offline engine.
    stream: Option<OnlineStream>,
    /// The most recent partial text handed back by [`SttEngine::feed`], so
    /// [`track_partial`] can suppress re-emitting an unchanged hypothesis.
    last_partial: String,
}

// `OnlineRecognizer` and `OnlineStream` do not implement `Debug` (they wrap
// raw FFI pointers), so this is written by hand instead of derived,
// reporting only whether a recognizer is loaded.
impl std::fmt::Debug for SherpaStreamingEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SherpaStreamingEngine")
            .field("loaded", &self.recognizer.is_some())
            .field("hotwords", &self.hotwords)
            .field("in_progress", &self.opts.is_some())
            .finish()
    }
}

/// The four files a streaming model directory must contain, in the order
/// [`SherpaStreamingEngine::load`] resolves them.
///
/// This is one half of a contract with whatever installed the directory: the
/// model catalog normalises every streaming artifact to these names, no
/// matter what upstream calls the file in its URL (`encoder.int8.onnx`,
/// `encoder-epoch-99-avg-1.int8.onnx`, ...). It is public so the crate that
/// owns the other half can be held against it rather than restating it — a
/// renamed artifact would otherwise cost nothing but a preview that silently
/// never loads.
pub const STREAMING_MODEL_FILES: [&str; 4] =
    ["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"];

impl SherpaStreamingEngine {
    /// Loads a sherpa-onnx online (streaming) transducer model from `dir`.
    ///
    /// Unlike [`SherpaOfflineEngine::load`], filenames are not tried against
    /// a candidate list: the catalog installs streaming models under exactly
    /// the names in [`STREAMING_MODEL_FILES`], so a single fixed name is
    /// resolved for each.
    ///
    /// `dir` must be a model *directory* as resolved by
    /// `ModelManager::path_for` — never a bare catalog id, for the same
    /// reason documented on [`SherpaOfflineEngine::load`].
    ///
    /// # Errors
    /// Returns [`SttError::ModelNotFound`] if `dir` does not exist, or if any
    /// of the expected encoder/decoder/joiner/tokens files are missing from
    /// it. Returns [`SttError::Engine`] if `cfg.hotwords` contains an
    /// interior null byte, if any resolved path is not valid UTF-8, or if
    /// sherpa-onnx refuses to build a recognizer from files that are all
    /// present — see [`SherpaOfflineEngine::load`]'s doc comment for why
    /// that last case is treated as an engine error rather than a
    /// missing-model one.
    pub fn load(dir: &Path, cfg: SherpaConfig) -> Result<Self, SttError> {
        if !dir.is_dir() {
            return Err(SttError::ModelNotFound(dir.display().to_string()));
        }

        let hotwords = build_hotwords_arg(&cfg.hotwords)?;

        let [encoder_name, decoder_name, joiner_name, tokens_name] = STREAMING_MODEL_FILES;
        let encoder = resolve_required_file(dir, &[encoder_name])?;
        let decoder = resolve_required_file(dir, &[decoder_name])?;
        let joiner = resolve_required_file(dir, &[joiner_name])?;
        let tokens = resolve_required_file(dir, &[tokens_name])?;

        let config = OnlineRecognizerConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: Some(path_to_string(&encoder)?),
                    decoder: Some(path_to_string(&decoder)?),
                    joiner: Some(path_to_string(&joiner)?),
                },
                tokens: Some(path_to_string(&tokens)?),
                num_threads: cfg.num_threads.clamp(1, i32::MAX as usize) as i32,
                // These are icefall-style streaming zipformer transducers,
                // not NeMo exports (unlike the offline engine's catalog
                // models) — model_type is left at the config default so
                // sherpa-onnx auto-detects the layout instead.
                ..Default::default()
            },
            decoding_method: Some(decoding_method(&cfg.hotwords).to_string()),
            // Both explicit, never inherited. This is the call site the trap
            // actually bites: `OnlineRecognizerConfig::default` sets
            // `max_active_paths` to 0, so `..Default::default()` alone would
            // run the beam search `decoding_method` just selected with a beam
            // width of zero. See the constants' doc comments.
            max_active_paths: MAX_ACTIVE_PATHS,
            hotwords_score: HOTWORDS_SCORE,
            ..Default::default()
        };

        // As in `SherpaOfflineEngine::load`: every expected file is already
        // confirmed present above, so a rejection here means sherpa-onnx
        // itself refused their contents, which is an engine failure rather
        // than a missing-model one.
        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            SttError::Engine(format!(
                "sherpa-onnx rejected the model in {}",
                dir.display()
            ))
        })?;

        Ok(Self {
            recognizer: Some(recognizer),
            hotwords,
            opts: None,
            stream: None,
            last_partial: String::new(),
        })
    }
}

impl SttEngine for SherpaStreamingEngine {
    fn begin(&mut self, opts: &TranscribeOptions) -> Result<(), SttError> {
        begin_streaming_session(
            &mut self.opts,
            &mut self.stream,
            &mut self.last_partial,
            opts,
        );
        Ok(())
    }

    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError> {
        if self.opts.is_none() {
            return Err(SttError::Engine(
                "feed called before begin: no transcription in progress".to_string(),
            ));
        }

        // Borrowed directly from the fields (rather than through a
        // `&self` helper method, as `SherpaOfflineEngine::recognizer` uses)
        // so this and the `self.stream` borrow below stay disjoint: a
        // method call would borrow all of `self` and rule out the `&mut`
        // access `get_or_insert_with` needs.
        let recognizer = self
            .recognizer
            .as_ref()
            .expect("invariant: SherpaStreamingEngine::load always sets recognizer to Some");
        let hotwords = self.hotwords.as_deref();
        let stream = self.stream.get_or_insert_with(|| match hotwords {
            Some(hotwords) => recognizer.create_stream_with_hotwords(hotwords),
            None => recognizer.create_stream(),
        });

        stream.accept_waveform(SAMPLE_RATE as i32, &samples_to_f32(samples));
        while recognizer.is_ready(stream) {
            recognizer.decode(stream);
        }
        let text = recognizer.get_result(stream).map(|result| result.text);

        Ok(feed_result(&mut self.last_partial, text.as_deref()))
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        let opts = self.opts.take().ok_or_else(|| {
            SttError::Engine("finish called before begin: no transcription in progress".to_string())
        })?;

        let recognizer = self
            .recognizer
            .as_ref()
            .expect("invariant: SherpaStreamingEngine::load always sets recognizer to Some");
        let hotwords = self.hotwords.as_deref();
        // `feed` is expected to have run at least once, but a begin()
        // immediately followed by finish() must not panic — fall back to a
        // fresh, silent stream so it decodes to empty text instead.
        let stream = self.stream.take().unwrap_or_else(|| match hotwords {
            Some(hotwords) => recognizer.create_stream_with_hotwords(hotwords),
            None => recognizer.create_stream(),
        });

        stream.input_finished();
        while recognizer.is_ready(&stream) {
            recognizer.decode(&stream);
        }
        let result = recognizer.get_result(&stream).ok_or_else(|| {
            SttError::Engine("sherpa-onnx produced no recognition result".to_string())
        })?;

        Ok(Transcript {
            text: result.text.trim().to_string(),
            language: opts.language,
        })
    }
}

/// Starts a new utterance: records `new_opts`, clears the last emitted
/// partial, and drops any stream left over from a previous begin/feed/finish
/// cycle (or from a `begin` that was never followed by `finish`), so nothing
/// from a previous utterance leaks into this one.
///
/// Split out from [`SttEngine::begin`] as a free function, for the same
/// testability reason as [`SherpaOfflineEngine`]'s `begin_session`: unlike
/// `feed` and `finish`, it needs no recognizer, so the opts-recording and
/// last-partial-clearing behavior stay unit-testable without a real model on
/// disk. The `*stream = None` line is not independently exercised by that
/// test: building a populated `Some(OnlineStream)` fixture needs a real
/// recognizer, which needs a downloaded model. It is verified by inspection
/// instead — the assignment is unconditional, so it is correct regardless of
/// what `stream` held on entry.
fn begin_streaming_session(
    opts: &mut Option<TranscribeOptions>,
    stream: &mut Option<OnlineStream>,
    last_partial: &mut String,
    new_opts: &TranscribeOptions,
) {
    *opts = Some(new_opts.clone());
    *stream = None;
    last_partial.clear();
}

/// Compares the just-observed recognition hypothesis `observed` against the
/// last one emitted (`last`), returning `Some(observed)` and updating `last`
/// only when it actually changed.
///
/// An online recognizer's result grows monotonically as more audio arrives —
/// it is a hypothesis over everything decoded so far, not a delta — so
/// without this, [`SttEngine::feed`] would re-emit the same string on every
/// call between chunks where the recognizer had nothing new to add, and the
/// HUD would redraw pointlessly.
fn track_partial(last: &mut String, observed: &str) -> Option<String> {
    if observed == last {
        None
    } else {
        *last = observed.to_string();
        Some(observed.to_string())
    }
}

/// Turns a `get_result` outcome into what [`SttEngine::feed`] should return.
///
/// `text: None` means the read genuinely failed — a null result pointer, or
/// a JSON body that failed to deserialize, per `RecognizerResult`'s
/// `Deserialize` impl — the same condition [`SherpaStreamingEngine::finish`]
/// treats as [`SttError::Engine`]. It is *not* promoted to an error here,
/// unlike in `finish`: this is a draft engine whose `feed` is polled on
/// every chunk purely to refresh the HUD, so one failed poll must not abort
/// the in-progress utterance — the offline engine's `finish` is what
/// actually produces the injected text, unaffected by this engine's read
/// failures. Falling back to an empty string instead (an earlier version of
/// this code did exactly that) would feed that empty text through
/// `track_partial` and wipe a good partial off the HUD, misreporting a read
/// failure as "the user said nothing." Passing `None` straight through
/// instead, without touching `last_partial`, avoids that: a failed read
/// leaves whatever the HUD is already showing untouched rather than
/// erasing it.
fn feed_result(last_partial: &mut String, text: Option<&str>) -> Option<String> {
    text.and_then(|text| track_partial(last_partial, text))
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

    /// Builds a `SherpaStreamingEngine` double with no loaded recognizer, for
    /// tests that only exercise the begin/feed/finish ordering rules and
    /// never reach a real decode (which would panic on this double — see the
    /// `recognizer.as_ref().expect(...)` calls in `feed`/`finish`). Loading a
    /// real model needs a downloaded catalog entry, which unit tests must
    /// not depend on.
    fn test_streaming_engine() -> SherpaStreamingEngine {
        SherpaStreamingEngine {
            recognizer: None,
            hotwords: None,
            opts: None,
            stream: None,
            last_partial: String::new(),
        }
    }

    #[test]
    fn streaming_loading_a_missing_model_directory_reports_model_not_found() {
        let err =
            SherpaStreamingEngine::load(Path::new("/nonexistent/model"), SherpaConfig::default())
                .expect_err("a missing model directory must not load");
        assert!(matches!(err, SttError::ModelNotFound(_)));
    }

    #[test]
    fn streaming_loading_a_directory_missing_required_files_reports_model_not_found() {
        let dir = std::env::temp_dir().join("utter-stt-test-sherpa-streaming-empty-model");
        std::fs::create_dir_all(&dir).expect("failed to create empty test model dir");

        let err = SherpaStreamingEngine::load(&dir, SherpaConfig::default())
            .expect_err("a model directory missing its files must not load");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(matches!(err, SttError::ModelNotFound(_)), "got {err:?}");
    }

    #[test]
    fn streaming_feed_before_begin_returns_engine_error() {
        let mut engine = test_streaming_engine();

        let err = engine
            .feed(&[0i16; 10])
            .expect_err("feed before begin must fail");

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn streaming_finish_before_begin_returns_engine_error() {
        let mut engine = test_streaming_engine();

        let err = engine.finish().expect_err("finish before begin must fail");

        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
    }

    #[test]
    fn begin_streaming_session_records_opts_and_clears_last_partial() {
        // The `*stream = None` line in `begin_streaming_session` is not
        // covered here: a fixture that starts as `Some(OnlineStream)` would
        // need a real recognizer, which needs a downloaded model — see that
        // function's doc comment for why it is verified by inspection
        // instead. A `stream` fixture that starts at `None`, as below, would
        // pass this assertion whether or not that line existed, which is
        // exactly the kind of vacuous check this codebase has been burned by
        // before, so it is deliberately left out rather than kept as
        // decoration.
        let mut opts: Option<TranscribeOptions> = None;
        let mut stream: Option<OnlineStream> = None;
        let mut last_partial = "привет".to_string();

        begin_streaming_session(
            &mut opts,
            &mut stream,
            &mut last_partial,
            &TranscribeOptions::default(),
        );

        assert!(opts.is_some(), "begin must record the new opts");
        assert!(
            last_partial.is_empty(),
            "begin must clear the last emitted partial from a previous utterance"
        );
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

    #[test]
    fn an_unchanged_partial_is_not_re_emitted() {
        // Same port contract vosk followed: feed() returns Some only when the
        // partial actually changed, so the HUD is not redrawn pointlessly.
        let mut last = String::new();
        assert_eq!(track_partial(&mut last, "прив"), Some("прив".to_string()));
        assert_eq!(track_partial(&mut last, "прив"), None);
        assert_eq!(
            track_partial(&mut last, "привет"),
            Some("привет".to_string())
        );
    }

    #[test]
    fn a_failed_read_does_not_erase_a_shown_partial() {
        // `text: None` stands in for `get_result` returning `None` (a null
        // result pointer or undeserializable JSON) after a partial was
        // already on screen. The fix under test is that this must not be
        // reported as "the user said nothing": no partial is emitted, and
        // the HUD's last known-good text is left alone.
        let mut last_partial = "привет".to_string();

        let emitted = feed_result(&mut last_partial, None);

        assert_eq!(emitted, None, "a failed read must not emit anything");
        assert_eq!(
            last_partial, "привет",
            "a failed read must not erase what the HUD is already showing"
        );
    }

    #[test]
    fn a_successful_read_still_flows_through_track_partial() {
        // Guards against a fix that swallows every result, not just failed
        // ones: a real reading must still reach `track_partial` and follow
        // its change-only-emits contract.
        let mut last_partial = String::new();

        assert_eq!(
            feed_result(&mut last_partial, Some("прив")),
            Some("прив".to_string())
        );
        assert_eq!(
            feed_result(&mut last_partial, Some("прив")),
            None,
            "an unchanged successful read must still be suppressed"
        );
    }
}
