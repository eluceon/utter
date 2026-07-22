use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum SttError {
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("engine error: {0}")]
    Engine(String),
    #[error("transcription cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum RefineError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("request timeout")]
    Timeout,
    #[error("bad response: {0}")]
    BadResponse(String),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum InjectError {
    #[error("no backend available: {0}")]
    NoBackend(String),
    #[error("backend error: {0}")]
    Backend(String),
}
