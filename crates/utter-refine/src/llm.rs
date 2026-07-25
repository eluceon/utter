//! OpenAI-compatible chat-completions client used to refine dictated text.
//!
//! `LlmRefiner::refine` is a blocking call: it is meant to run on a worker
//! thread in the desktop app, so a blocking `reqwest` client is the
//! deliberate choice here rather than async plumbing.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use utter_core::{RefineError, TextRefiner, Tone};

use crate::build_prompt;

/// Maximum bytes of a non-2xx response body echoed back in `RefineError::Http`.
const ERROR_BODY_TRUNCATE_LEN: usize = 200;

/// Connection settings for an OpenAI-compatible `/chat/completions` endpoint.
pub struct LlmConfig {
    /// API base URL, e.g. `https://api.openai.com/v1` or `http://localhost:11434/v1`.
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub timeout: Duration,
}

/// Refines transcripts by calling an OpenAI-compatible chat-completions endpoint.
pub struct LlmRefiner {
    client: reqwest::blocking::Client,
    config: LlmConfig,
    dictionary_terms: Vec<String>,
}

impl LlmRefiner {
    /// Builds a refiner from `cfg`.
    ///
    /// # Panics
    /// Panics only if the underlying blocking HTTP client cannot be built
    /// (e.g. TLS backend initialization failure) — that signals a broken
    /// environment, not a user-recoverable error.
    pub fn new(cfg: LlmConfig, dictionary_terms: Vec<String>) -> Self {
        let connect_timeout = cfg.timeout.min(Duration::from_secs(5));
        let client = reqwest::blocking::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(connect_timeout)
            .build()
            .expect("invariant: failed to build blocking HTTP client");

        Self {
            client,
            config: cfg,
            dictionary_terms,
        }
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

impl TextRefiner for LlmRefiner {
    fn refine(&self, text: &str, tone: Tone) -> Result<String, RefineError> {
        if tone == Tone::Verbatim {
            return Ok(text.to_string());
        }

        let (system, user) = build_prompt(text, tone, &self.dictionary_terms, None);
        let request_body = ChatRequest {
            model: &self.config.model,
            messages: [
                ChatMessage {
                    role: "system",
                    content: &system,
                },
                ChatMessage {
                    role: "user",
                    content: &user,
                },
            ],
            temperature: 0.2,
        };

        let base_url = self.config.base_url.trim_end_matches('/');
        let url = format!("{base_url}/chat/completions");
        let mut request = self.client.post(url).json(&request_body);
        if let Some(api_key) = &self.config.api_key {
            request = request.bearer_auth(api_key);
        }

        let response = request.send().map_err(map_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            // Body is best-effort context for the error message; if reading
            // it fails, the status code alone is still reported below.
            let body = response.text().unwrap_or_default();
            return Err(RefineError::Http(format!(
                "{status}: {}",
                truncate_chars(&body, ERROR_BODY_TRUNCATE_LEN)
            )));
        }

        let body_text = response.text().map_err(map_reqwest_error)?;

        let parsed: ChatResponse = serde_json::from_str(&body_text)
            .map_err(|e| RefineError::BadResponse(format!("invalid JSON: {e}")))?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| RefineError::BadResponse("missing choices in response".to_string()))?;

        Ok(strip_quotes(content.trim()).to_string())
    }
}

fn map_reqwest_error(e: reqwest::Error) -> RefineError {
    if e.is_timeout() {
        RefineError::Timeout
    } else {
        RefineError::Http(e.to_string())
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// Strips one pair of symmetric surrounding quotes (`"…"`, `'…'`, `«…»`) from
/// an already whitespace-trimmed string, if present. Leaves `s` unchanged
/// when it isn't wrapped in a matching pair.
fn strip_quotes(s: &str) -> &str {
    const PAIRS: [(char, char); 3] = [('"', '"'), ('\'', '\''), ('«', '»')];
    for (open, close) in PAIRS {
        if let Some(rest) = s.strip_prefix(open) {
            if let Some(inner) = rest.strip_suffix(close) {
                return inner;
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_double_quotes() {
        assert_eq!(strip_quotes("\"hello world\""), "hello world");
    }

    #[test]
    fn strips_single_quotes() {
        assert_eq!(strip_quotes("'hello world'"), "hello world");
    }

    #[test]
    fn strips_guillemets() {
        assert_eq!(strip_quotes("«hello world»"), "hello world");
    }

    #[test]
    fn leaves_unquoted_text_unchanged() {
        assert_eq!(strip_quotes("hello world"), "hello world");
    }

    #[test]
    fn leaves_mismatched_quotes_unchanged() {
        assert_eq!(strip_quotes("\"hello world'"), "\"hello world'");
    }

    #[test]
    fn leaves_single_lone_quote_unchanged() {
        assert_eq!(strip_quotes("\""), "\"");
    }

    #[test]
    fn strips_only_one_pair_leaving_inner_quotes() {
        assert_eq!(strip_quotes("\"'hello'\""), "'hello'");
    }

    #[test]
    fn truncate_chars_keeps_short_strings() {
        assert_eq!(truncate_chars("short", 200), "short");
    }

    #[test]
    fn truncate_chars_cuts_long_strings() {
        let long = "a".repeat(300);
        assert_eq!(truncate_chars(&long, 200), "a".repeat(200));
    }
}
