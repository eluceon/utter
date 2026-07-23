//! OpenAI-compatible cloud [`SttEngine`] adapter.
//!
//! Like whisper.cpp (see [`crate::whisper`]), the OpenAI `/audio/transcriptions`
//! endpoint is a batch API: it wants the whole utterance in one request rather
//! than being streamed incrementally. [`CloudEngine`] therefore buffers
//! samples during [`SttEngine::feed`] and, on [`SttEngine::finish`], WAV-encodes
//! the buffer in memory (16 kHz mono 16-bit PCM) and POSTs it as a
//! `multipart/form-data` request; it never emits partial transcripts.
//!
//! `CloudEngine::finish` is a blocking call: it is meant to run on a worker
//! thread in the desktop app, so a blocking `reqwest` client is the
//! deliberate choice here rather than async plumbing (mirrors
//! [`utter_refine::LlmRefiner`]).

use std::cell::RefCell;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::rc::Rc;
use std::time::Duration;

use serde::Deserialize;
use utter_core::{SttEngine, SttError, TranscribeOptions, Transcript, SAMPLE_RATE};

/// Maximum bytes of a non-2xx response body echoed back in [`SttError::Engine`].
const ERROR_BODY_TRUNCATE_LEN: usize = 200;

/// Connection settings for an OpenAI-compatible `/audio/transcriptions` endpoint.
pub struct CloudSttConfig {
    /// API base URL, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

/// A cloud speech-to-text engine, calling an OpenAI-compatible
/// `/audio/transcriptions` endpoint and reusable across many
/// begin/feed/finish transcription cycles.
pub struct CloudEngine {
    client: reqwest::blocking::Client,
    config: CloudSttConfig,
    /// `Some` between `begin` and `finish`; `None` otherwise. Doubles as the
    /// "has begin() been called yet" flag that `feed`/`finish` check.
    opts: Option<TranscribeOptions>,
    buffer: Vec<i16>,
}

impl CloudEngine {
    /// Builds an engine from `cfg`.
    ///
    /// # Panics
    /// Panics only if the underlying blocking HTTP client cannot be built
    /// (e.g. TLS backend initialization failure) — that signals a broken
    /// environment, not a user-recoverable error.
    pub fn new(cfg: CloudSttConfig) -> Self {
        let connect_timeout = cfg.timeout.min(Duration::from_secs(5));
        let client = reqwest::blocking::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(connect_timeout)
            .build()
            .expect("invariant: failed to build blocking HTTP client");

        Self {
            client,
            config: cfg,
            opts: None,
            buffer: Vec::new(),
        }
    }
}

impl SttEngine for CloudEngine {
    fn begin(&mut self, opts: &TranscribeOptions) -> Result<(), SttError> {
        begin_session(&mut self.opts, &mut self.buffer, opts);
        Ok(())
    }

    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError> {
        feed_session(&self.opts, &mut self.buffer, samples)
    }

    fn finish(&mut self) -> Result<Transcript, SttError> {
        let (opts, buffer) = take_session(&mut self.opts, &mut self.buffer)?;
        let wav_bytes = encode_wav(&buffer)?;
        transcribe(&self.client, &self.config, &opts, wav_bytes)
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
/// `buffer` directly instead of `&mut CloudEngine`, so the begin/feed-ordering
/// rule can be unit tested without an HTTP client.
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
    Ok(None) // The cloud endpoint is a batch API: never emits partials.
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

/// A `Write + Seek` handle over a `Vec<u8>` shared (via `Rc<RefCell<_>>`)
/// with the caller, so the encoded bytes can be recovered after
/// [`hound::WavWriter::finalize`] consumes the writer.
///
/// [`hound::WavWriter`] needs `Seek` to back-patch the RIFF/data chunk sizes
/// once the total sample count is known, and offers no `into_inner()` to hand
/// the underlying writer back — hence going through a shared handle instead
/// of a plain [`Cursor<Vec<u8>>`].
#[derive(Clone)]
struct SharedCursor(Rc<RefCell<Cursor<Vec<u8>>>>);

impl Write for SharedCursor {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

impl Seek for SharedCursor {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.borrow_mut().seek(pos)
    }
}

/// WAV-encodes `samples` (16 kHz mono 16-bit PCM) into an in-memory byte
/// buffer, suitable as the `file` part of a multipart upload.
fn encode_wav(samples: &[i16]) -> Result<Vec<u8>, SttError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let shared = Rc::new(RefCell::new(Cursor::new(Vec::new())));
    let mut writer = hound::WavWriter::new(SharedCursor(shared.clone()), spec)
        .map_err(|e| SttError::Engine(format!("failed to start WAV encoding: {e}")))?;
    for &sample in samples {
        writer
            .write_sample(sample)
            .map_err(|e| SttError::Engine(format!("failed to write WAV sample: {e}")))?;
    }
    writer
        .finalize()
        .map_err(|e| SttError::Engine(format!("failed to finalize WAV encoding: {e}")))?;

    let cursor = Rc::try_unwrap(shared)
        .map_err(|_| {
            SttError::Engine(
                "invariant: WAV writer did not release its shared buffer handle".to_string(),
            )
        })?
        .into_inner();
    Ok(cursor.into_inner())
}

#[derive(Deserialize)]
struct CloudTranscriptionResponse {
    #[serde(default)]
    text: Option<String>,
}

/// POSTs `wav_bytes` as a multipart `/audio/transcriptions` request and
/// parses the `{"text": ...}` response into a [`Transcript`].
fn transcribe(
    client: &reqwest::blocking::Client,
    config: &CloudSttConfig,
    opts: &TranscribeOptions,
    wav_bytes: Vec<u8>,
) -> Result<Transcript, SttError> {
    let file_part = reqwest::blocking::multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| SttError::Engine(format!("failed to build multipart file part: {e}")))?;

    let mut form = reqwest::blocking::multipart::Form::new()
        .part("file", file_part)
        .text("model", config.model.clone());
    if let Some(language) = &opts.language {
        form = form.text("language", language.clone());
    }

    let base_url = config.base_url.trim_end_matches('/');
    let url = format!("{base_url}/audio/transcriptions");
    let response = client
        .post(url)
        .bearer_auth(&config.api_key)
        .multipart(form)
        .send()
        .map_err(map_reqwest_error)?;

    let status = response.status();
    if !status.is_success() {
        // Body is best-effort context for the error message; if reading it
        // fails, the status code alone is still reported below.
        let body = response.text().unwrap_or_default();
        return Err(SttError::Engine(format!(
            "cloud stt request failed with {status}: {}",
            truncate_chars(&body, ERROR_BODY_TRUNCATE_LEN)
        )));
    }

    let body_text = response.text().map_err(map_reqwest_error)?;
    let parsed: CloudTranscriptionResponse = serde_json::from_str(&body_text)
        .map_err(|e| SttError::Engine(format!("invalid JSON in cloud stt response: {e}")))?;
    let text = parsed
        .text
        .ok_or_else(|| SttError::Engine("missing text field in cloud stt response".to_string()))?;

    Ok(Transcript {
        text: text.trim().to_string(),
        language: opts.language.clone(),
    })
}

fn map_reqwest_error(e: reqwest::Error) -> SttError {
    if e.is_timeout() {
        SttError::Engine(format!("cloud stt request timed out: {e}"))
    } else {
        SttError::Engine(format!("cloud stt request failed: {e}"))
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "cloud stt engine never emits partial transcripts"
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
    fn wav_round_trips_through_hound_at_16khz_mono_16bit() {
        let samples: Vec<i16> = vec![0, 100, -100, i16::MAX, i16::MIN, 42, -42];

        let bytes = encode_wav(&samples).expect("encode_wav should succeed");

        let mut reader =
            hound::WavReader::new(Cursor::new(bytes)).expect("hound should parse the encoded WAV");
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);

        let decoded: Vec<i16> = reader
            .samples::<i16>()
            .collect::<Result<_, _>>()
            .expect("all samples should decode");
        assert_eq!(decoded, samples);
    }

    #[test]
    fn encode_wav_of_empty_buffer_still_produces_a_valid_wav() {
        let bytes = encode_wav(&[]).expect("encode_wav of empty buffer should succeed");

        let reader =
            hound::WavReader::new(Cursor::new(bytes)).expect("hound should parse the encoded WAV");
        assert_eq!(reader.duration(), 0);
    }
}
