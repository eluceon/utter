//! whisper.cpp-backed [`SttEngine`] adapter.
//!
//! whisper.cpp is a batch engine: it transcribes best when given the whole
//! utterance at once rather than being fed incrementally. [`WhisperEngine`]
//! therefore just buffers samples during [`SttEngine::feed`] and runs full
//! inference in [`SttEngine::finish`]; it never emits partial transcripts.

use std::path::Path;
use std::sync::Once;

use utter_core::{SttEngine, SttError, TranscribeOptions, Transcript};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Ensures [`whisper_rs::install_logging_hooks`] runs exactly once per
/// process, however many [`WhisperEngine`]s are loaded. whisper.cpp/GGML log
/// straight to stderr by default; routing them into `tracing` (via the
/// `tracing_backend` feature) keeps test and application output clean.
static INSTALL_LOGGING_HOOKS: Once = Once::new();

fn ensure_logging_hooks_installed() {
    INSTALL_LOGGING_HOOKS.call_once(whisper_rs::install_logging_hooks);
}

/// Number of decode threads to use: `min(4, available parallelism)`.
///
/// `std::thread::available_parallelism` reports *logical* cores, not
/// physical ones. Distinguishing them would need an extra dependency (e.g.
/// `num_cpus`), which isn't already present in this workspace, so this is a
/// deliberate approximation: on machines with hyperthreading it may pick a
/// couple more threads than strictly "physical cores", which is harmless for
/// whisper.cpp's decode loop.
fn default_n_threads() -> i32 {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    available.min(4) as i32
}

/// A whisper.cpp speech-to-text engine, loaded from a single GGML/GGUF model
/// file and reusable across many begin/feed/finish transcription cycles.
#[derive(Debug)]
pub struct WhisperEngine {
    ctx: WhisperContext,
    n_threads: i32,
    /// `Some` between `begin` and `finish`; `None` otherwise. Doubles as the
    /// "has begin() been called yet" flag that `feed`/`finish` check.
    opts: Option<TranscribeOptions>,
    buffer: Vec<i16>,
}

impl WhisperEngine {
    /// Loads a whisper.cpp model from `model_path`.
    ///
    /// # Errors
    /// Returns [`SttError::ModelNotFound`] if `model_path` does not exist, or
    /// [`SttError::Engine`] if whisper.cpp rejects the file (e.g. it exists
    /// but is not a valid model).
    pub fn load(model_path: &Path) -> Result<Self, SttError> {
        if !model_path.is_file() {
            return Err(SttError::ModelNotFound(model_path.display().to_string()));
        }

        ensure_logging_hooks_installed();

        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .map_err(|e| SttError::Engine(format!("failed to load whisper model: {e}")))?;

        Ok(Self {
            ctx,
            n_threads: default_n_threads(),
            opts: None,
            buffer: Vec::new(),
        })
    }
}

impl SttEngine for WhisperEngine {
    fn begin(&mut self, opts: &TranscribeOptions) -> Result<(), SttError> {
        begin_session(&mut self.opts, &mut self.buffer, opts);
        Ok(())
    }

    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError> {
        feed_session(&self.opts, &mut self.buffer, samples)
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        let (opts, buffer) = take_session(&mut self.opts, &mut self.buffer)?;
        run_full_inference(&self.ctx, self.n_threads, &opts, &buffer)
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
/// `buffer` directly instead of `&mut WhisperEngine`, so the
/// begin/feed-ordering rule can be unit tested without loading a real
/// whisper.cpp model.
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
    Ok(None) // whisper.cpp is a batch engine: never emits partials.
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

/// Converts buffered `i16` samples to `f32`, runs whisper.cpp's full
/// transcription pipeline over them, and joins the resulting segments into a
/// single trimmed [`Transcript`].
fn run_full_inference(
    ctx: &WhisperContext,
    n_threads: i32,
    opts: &TranscribeOptions,
    samples: &[i16],
) -> Result<Transcript, SttError> {
    if let Some(lang) = &opts.language {
        reject_null_byte(lang, "language")?;
    }
    if let Some(prompt) = &opts.initial_prompt {
        reject_null_byte(prompt, "initial prompt")?;
    }

    // i16 -> f32 in [-1.0, 1.0), the format whisper.cpp's spectrogram code expects.
    let audio: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();

    let mut state = ctx
        .create_state()
        .map_err(|e| SttError::Engine(format!("failed to create whisper state: {e}")))?;

    // best_of matches whisper.cpp's own greedy-sampling default.
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
    params.set_n_threads(n_threads);
    params.set_translate(false);
    params.set_single_segment(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // `None` means "auto" to whisper.cpp, same as not setting a language at all.
    params.set_language(opts.language.as_deref());
    if let Some(prompt) = &opts.initial_prompt {
        params.set_initial_prompt(prompt);
    }

    state
        .full(params, &audio)
        .map_err(|e| SttError::Engine(format!("whisper inference failed: {e}")))?;

    // `set_single_segment(true)` above means whisper.cpp should only ever
    // produce one segment, but segments are still joined defensively (with a
    // single space, dropping any empty ones) rather than assumed to be
    // exactly one, so a future change to that parameter can't silently glue
    // words together with no separator.
    let mut segments = Vec::new();
    for segment in state.as_iter() {
        let segment_text = segment
            .to_str_lossy()
            .map_err(|e| SttError::Engine(format!("failed to read segment text: {e}")))?;
        let trimmed = segment_text.trim();
        if !trimmed.is_empty() {
            segments.push(trimmed.to_string());
        }
    }

    let language = match &opts.language {
        Some(lang) => Some(lang.clone()),
        None => {
            let lang_id = state.full_lang_id_from_state();
            whisper_rs::get_lang_str(lang_id).map(str::to_string)
        }
    };

    Ok(Transcript {
        text: segments.join(" "),
        language,
    })
}

/// whisper-rs converts strings to `CString` internally and panics on
/// interior null bytes; checking up front turns that potential panic into an
/// ordinary [`SttError::Engine`].
fn reject_null_byte(s: &str, field: &str) -> Result<(), SttError> {
    if s.contains('\0') {
        return Err(SttError::Engine(format!(
            "{field} must not contain a null byte"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn load_missing_path_returns_model_not_found() {
        let path = Path::new("/nonexistent/path/to/ggml-model.bin");
        let err = WhisperEngine::load(path).expect_err("missing model path must not load");

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
        let dir = std::env::temp_dir();
        let path = dir.join("utter-stt-test-not-a-model.bin");
        std::fs::write(&path, b"not a whisper model")
            .expect("failed to write garbage test fixture");

        let err = WhisperEngine::load(&path).expect_err("garbage file must not load as a model");
        let _ = std::fs::remove_file(&path);

        // whisper.cpp cannot distinguish "missing file" from "not a valid
        // model" at this call (both surface as a null context pointer), so a
        // present-but-invalid file is reported as a generic engine error
        // rather than `ModelNotFound`.
        assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
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

        assert_eq!(result, None, "whisper.cpp never emits partial transcripts");
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

    /// Manual, network- and model-dependent smoke test: downloads the tiny
    /// whisper.cpp model (once, cached in the OS temp dir) and runs the full
    /// begin/feed/finish pipeline over one second of a synthetic sine wave.
    /// It is not speech, so the assertion only checks that inference
    /// completes without panicking or erroring — not on any particular
    /// transcribed text.
    ///
    /// Deliberately `#[ignore]`d: it needs network access and downloads
    /// ~75 MB, so it must never run in CI. Run manually with:
    /// `cargo test -p utter-stt --features whisper -- --ignored --nocapture transcribes_jfk_sample`
    #[test]
    #[ignore]
    fn transcribes_jfk_sample() {
        let model_path = ensure_tiny_model_downloaded();
        let mut engine = WhisperEngine::load(&model_path).expect("failed to load tiny model");

        let sine = generate_sine_wave(1.0, 440.0);

        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        assert_eq!(
            engine.feed(&sine).expect("feed failed"),
            None,
            "whisper.cpp never emits partial transcripts"
        );
        let transcript = engine.finish().expect("finish failed");

        println!("transcript: {transcript:?}");
    }

    /// Generates `seconds` of a mono 16 kHz `i16` sine wave at `hz`, at a
    /// quarter of full scale (loud enough for whisper.cpp's VAD/energy
    /// checks to see signal, quiet enough to avoid clipping).
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

    /// Downloads `ggml-tiny.bin` via the system `curl` binary into the OS
    /// temp dir, skipping the download if it is already cached there from a
    /// previous run. Shells out instead of adding an HTTP client dependency,
    /// since this is a manual-only, `#[ignore]`d test.
    fn ensure_tiny_model_downloaded() -> std::path::PathBuf {
        let path = std::env::temp_dir().join("utter-stt-test-ggml-tiny.bin");
        if !path.is_file() {
            let status = std::process::Command::new("curl")
                .args(["-L", "-sS", "--fail", "-o"])
                .arg(&path)
                .arg("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin")
                .status()
                .expect("failed to invoke curl to download the tiny model");
            assert!(status.success(), "curl failed to download the tiny model");
        }
        path
    }
}
