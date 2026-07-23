//! Speech-to-text engine adapters.

#[cfg(feature = "whisper")]
pub mod whisper;

#[cfg(feature = "whisper")]
pub use whisper::WhisperEngine;

#[cfg(feature = "vosk")]
pub mod vosk;

#[cfg(feature = "vosk")]
pub use vosk::VoskEngine;
