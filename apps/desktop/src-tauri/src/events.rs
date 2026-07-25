//! Event payload shapes emitted to the UI over Tauri's event bus.
//!
//! Fixed here, once, so every emitter (this task's `download_model`, and the
//! runtime orchestrator that lands in later tasks) shares the same wire
//! shape rather than each defining its own ad hoc payload.
//!
//! Only [`ModelProgress`] (the `model-progress` event) is actually emitted
//! by this task; [`DictationState`] (`dictation-state`) and [`Notice`]
//! (`notice`) are defined ahead of time because their shape is part of this
//! task's contract with the frontend, even though nothing emits them yet.

use serde::Serialize;

/// Payload for the `model-progress` event, emitted while a model download is
/// in flight: `done`/`total` are bytes received so far / expected (`total`
/// is `0` if the server didn't report a `Content-Length`).
#[derive(Debug, Clone, Serialize)]
pub struct ModelProgress {
    pub id: String,
    pub done: u64,
    pub total: u64,
}

/// The dictation pipeline's current phase, part of the `dictation-state`
/// event payload. Serializes to the lowercase strings the frontend expects
/// (`"idle"`, `"recording"`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationPhase {
    Idle,
    Recording,
    Transcribing,
    Refining,
    Injecting,
}

/// Payload for the `dictation-state` event.
#[derive(Debug, Clone, Serialize)]
pub struct DictationState {
    pub state: DictationPhase,
    pub level: f32,
    pub partial: Option<String>,
}

/// Severity of a `notice` event, shown to the user as a toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Info,
    Warning,
    Error,
}

/// Payload for the `notice` event.
#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    pub kind: NoticeKind,
    pub message: String,
}
