//! Wiremock-backed integration tests for `CloudEngine`, the OpenAI-compatible
//! `/audio/transcriptions` batch adapter. `CloudEngine::finish` is a blocking
//! call (it runs on a worker thread in the real app), so every server-backed
//! test spawns it via `tokio::task::spawn_blocking` from an async,
//! multi-threaded test runtime — calling blocking reqwest directly inside an
//! async context panics.
#![cfg(feature = "cloud")]

use std::time::Duration;

use utter_core::{SttEngine, SttError, TranscribeOptions};
use utter_stt::{CloudEngine, CloudSttConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn config(base_url: String) -> CloudSttConfig {
    CloudSttConfig {
        base_url,
        api_key: "secret-key".to_string(),
        model: "whisper-1".to_string(),
        timeout: Duration::from_secs(5),
    }
}

/// A few hundred milliseconds of near-silence: real audio content is
/// irrelevant to these tests (the mock server never actually transcribes
/// anything), only that it round-trips through WAV encoding and multipart.
fn sample_audio() -> Vec<i16> {
    vec![1, -1, 2, -2, 3, -3, 0, 0]
}

#[tokio::test(flavor = "multi_thread")]
async fn happy_path_sends_expected_multipart_request_and_returns_trimmed_text() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .and(header("authorization", "Bearer secret-key"))
        .and(is_multipart_with_wav_file_and_model("whisper-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "  hello there  "
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let samples = sample_audio();
    let transcript = tokio::task::spawn_blocking(move || {
        let mut engine = CloudEngine::new(cfg);
        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        assert_eq!(engine.feed(&samples).expect("feed failed"), None);
        engine.finish()
    })
    .await
    .expect("blocking task panicked")
    .expect("finish should succeed");

    assert_eq!(transcript.text, "hello there");
    assert_eq!(transcript.language, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn language_option_is_forwarded_and_echoed_back() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "bonjour"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let samples = sample_audio();
    let opts = TranscribeOptions {
        language: Some("fr".to_string()),
        ..TranscribeOptions::default()
    };

    let transcript = tokio::task::spawn_blocking(move || {
        let mut engine = CloudEngine::new(cfg);
        engine.begin(&opts).expect("begin failed");
        engine.feed(&samples).expect("feed failed");
        engine.finish()
    })
    .await
    .expect("blocking task panicked")
    .expect("finish should succeed");

    assert_eq!(transcript.text, "bonjour");
    assert_eq!(transcript.language, Some("fr".to_string()));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains("name=\"language\"") && body.contains("fr"),
        "expected language field in multipart body: {body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthorized_returns_engine_error_with_status() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let samples = sample_audio();
    let result = tokio::task::spawn_blocking(move || {
        let mut engine = CloudEngine::new(cfg);
        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        engine.feed(&samples).expect("feed failed");
        engine.finish()
    })
    .await
    .expect("blocking task panicked");

    match result {
        Err(SttError::Engine(msg)) => {
            assert!(msg.contains("401"), "expected status in message: {msg}");
        }
        other => panic!("expected Engine error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_response_beyond_timeout_returns_engine_error_mentioning_timeout() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"text": "hi"}))
                .set_delay(Duration::from_millis(800)),
        )
        .mount(&server)
        .await;

    let mut cfg = config(server.uri());
    cfg.timeout = Duration::from_millis(100);
    let samples = sample_audio();
    let result = tokio::task::spawn_blocking(move || {
        let mut engine = CloudEngine::new(cfg);
        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        engine.feed(&samples).expect("feed failed");
        engine.finish()
    })
    .await
    .expect("blocking task panicked");

    match result {
        Err(SttError::Engine(msg)) => {
            assert!(
                msg.to_lowercase().contains("timeout") || msg.to_lowercase().contains("timed out"),
                "expected timeout mentioned in message: {msg}"
            );
        }
        other => panic!("expected Engine error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_json_response_returns_engine_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let samples = sample_audio();
    let result = tokio::task::spawn_blocking(move || {
        let mut engine = CloudEngine::new(cfg);
        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        engine.feed(&samples).expect("feed failed");
        engine.finish()
    })
    .await
    .expect("blocking task panicked");

    assert!(
        matches!(result, Err(SttError::Engine(_))),
        "expected Engine error, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_text_field_returns_engine_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "not_text": "oops"
        })))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let samples = sample_audio();
    let result = tokio::task::spawn_blocking(move || {
        let mut engine = CloudEngine::new(cfg);
        engine
            .begin(&TranscribeOptions::default())
            .expect("begin failed");
        engine.feed(&samples).expect("feed failed");
        engine.finish()
    })
    .await
    .expect("blocking task panicked");

    assert!(
        matches!(result, Err(SttError::Engine(_))),
        "expected Engine error, got {result:?}"
    );
}

#[test]
fn feed_before_begin_returns_engine_error() {
    let cfg = CloudSttConfig {
        base_url: "http://localhost:0".to_string(),
        api_key: "key".to_string(),
        model: "whisper-1".to_string(),
        timeout: Duration::from_secs(5),
    };
    let mut engine = CloudEngine::new(cfg);

    let err = engine
        .feed(&[0i16; 10])
        .expect_err("feed before begin must fail");

    assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
}

#[test]
fn finish_before_begin_returns_engine_error() {
    let cfg = CloudSttConfig {
        base_url: "http://localhost:0".to_string(),
        api_key: "key".to_string(),
        model: "whisper-1".to_string(),
        timeout: Duration::from_secs(5),
    };
    let mut engine = CloudEngine::new(cfg);

    let err = engine.finish().expect_err("finish before begin must fail");

    assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
}

#[test]
fn second_finish_without_new_begin_returns_engine_error() {
    let cfg = CloudSttConfig {
        base_url: "http://localhost:0".to_string(),
        api_key: "key".to_string(),
        model: "whisper-1".to_string(),
        timeout: Duration::from_secs(5),
    };
    let mut engine = CloudEngine::new(cfg);
    engine
        .begin(&TranscribeOptions::default())
        .expect("begin failed");
    // First finish will fail (no server at localhost:0), but must still
    // consume the session so a second finish reports "no transcription in
    // progress" rather than trying (and failing differently) again.
    let _ = engine.finish();

    let err = engine
        .finish()
        .expect_err("second finish without a new begin must fail");

    assert!(matches!(err, SttError::Engine(_)), "got {err:?}");
}

/// Matches a multipart/form-data body containing a `file` part whose bytes
/// start with a RIFF/WAVE header, and a `model` text field equal to
/// `expected_model`. Written as a custom matcher (rather than
/// `body_partial_json`, which only understands JSON bodies) because
/// multipart bodies are not JSON.
fn is_multipart_with_wav_file_and_model(expected_model: &'static str) -> impl wiremock::Match {
    struct MultipartWavMatcher {
        expected_model: &'static str,
    }

    impl wiremock::Match for MultipartWavMatcher {
        fn matches(&self, request: &Request) -> bool {
            let content_type = request
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if !content_type.starts_with("multipart/form-data") {
                return false;
            }

            let body = String::from_utf8_lossy(&request.body);
            let has_file_part = body.contains("name=\"file\"")
                && body.contains("filename=\"audio.wav\"")
                && body.contains("audio/wav");
            let has_riff_header = request.body.windows(4).any(|w| w == b"RIFF");
            let has_model_field =
                body.contains("name=\"model\"") && body.contains(self.expected_model);

            has_file_part && has_riff_header && has_model_field
        }
    }

    MultipartWavMatcher { expected_model }
}
