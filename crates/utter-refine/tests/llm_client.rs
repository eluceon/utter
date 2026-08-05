//! Wiremock-backed integration tests for `LlmRefiner`, the OpenAI-compatible
//! chat-completions client. `LlmRefiner::refine` is a blocking call (it runs
//! on a worker thread in the real app), so every test spawns it via
//! `tokio::task::spawn_blocking` from an async, multi-threaded test runtime —
//! calling blocking reqwest directly inside an async context panics.

use std::time::Duration;

use serde_json::json;
use utter_core::{RefineError, TextRefiner, Tone};
use utter_refine::{build_prompt, LlmConfig, LlmRefiner};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(base_url: String) -> LlmConfig {
    LlmConfig {
        base_url,
        api_key: None,
        model: "gpt-4o-mini".to_string(),
        timeout: Duration::from_secs(5),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn happy_path_returns_refined_text_and_sends_expected_body() {
    let server = MockServer::start().await;
    let (system, user) = build_prompt("so um hello there", Tone::Clean, &[], None);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({
            "model": "gpt-4o-mini",
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": 0.2,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [
                {"message": {"role": "assistant", "content": "\"Hello there.\"  "}}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("so um hello there", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");

    assert_eq!(result, Ok("Hello there.".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn server_error_returns_http_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");

    match result {
        Err(RefineError::Http(msg)) => {
            assert!(msg.contains("500"), "expected status in message: {msg}");
            assert!(
                msg.contains("Internal Server Error"),
                "expected body in message: {msg}"
            );
        }
        other => panic!("expected Http error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_json_returns_bad_response() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");

    assert!(
        matches!(result, Err(RefineError::BadResponse(_))),
        "expected BadResponse, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_choices_returns_bad_response() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");

    assert!(
        matches!(result, Err(RefineError::BadResponse(_))),
        "expected BadResponse, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_response_beyond_timeout_returns_timeout() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "hi"}}]}))
                .set_delay(Duration::from_millis(800)),
        )
        .mount(&server)
        .await;

    let mut cfg = config(server.uri());
    cfg.timeout = Duration::from_millis(100);
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");

    assert_eq!(result, Err(RefineError::Timeout));
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_header_present_when_api_key_set() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "hi"}}]
        })))
        .mount(&server)
        .await;

    let mut cfg = config(server.uri());
    cfg.api_key = Some("secret-key".to_string());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");
    assert!(result.is_ok());

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let auth = requests[0]
        .headers
        .get("authorization")
        .expect("authorization header missing")
        .to_str()
        .unwrap();
    assert_eq!(auth, "Bearer secret-key");
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_header_absent_when_no_api_key() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "hi"}}]
        })))
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");
    assert!(result.is_ok());

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("authorization").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn verbatim_tone_returns_input_unchanged_without_http_call() {
    let server = MockServer::start().await;

    // No mock is registered at all: any request would 404 (and fail the
    // `.expect(0)` guard below), proving refine() never calls out for
    // Tone::Verbatim.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "should not be used"}}]
        })))
        .expect(0)
        .mount(&server)
        .await;

    let cfg = config(server.uri());
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("  keep me verbatim  ", Tone::Verbatim)
    })
    .await
    .expect("blocking task panicked");

    assert_eq!(result, Ok("  keep me verbatim  ".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn trailing_slash_in_base_url_does_not_double_slash_the_path() {
    let server = MockServer::start().await;

    // The mock only matches the exact single-slash path. If the client
    // naively concatenated `base_url` + "/chat/completions" without
    // trimming the trailing slash already present in `base_url`, the
    // request would hit "/v1//chat/completions" instead and this mock
    // would never match, failing the request with a 404.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "hi"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = config(format!("{}/v1/", server.uri()));
    let result = tokio::task::spawn_blocking(move || {
        let refiner = LlmRefiner::new(cfg, Vec::new()).expect("build refiner");
        refiner.refine("hello", Tone::Clean)
    })
    .await
    .expect("blocking task panicked");

    assert_eq!(result, Ok("hi".to_string()));
}
