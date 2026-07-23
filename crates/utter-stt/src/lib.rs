//! Speech-to-text engine adapters.

#[cfg(feature = "whisper")]
pub mod whisper;

#[cfg(feature = "whisper")]
pub use whisper::WhisperEngine;
