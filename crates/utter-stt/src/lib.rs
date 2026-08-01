//! Speech-to-text engine adapters.

#[cfg(feature = "whisper")]
pub mod whisper;

#[cfg(feature = "whisper")]
pub use whisper::WhisperEngine;

#[cfg(feature = "vosk")]
pub mod vosk;

#[cfg(feature = "vosk")]
pub use vosk::VoskEngine;

#[cfg(feature = "cloud")]
pub mod cloud;

#[cfg(feature = "cloud")]
pub use cloud::{CloudEngine, CloudSttConfig};

#[cfg(feature = "sherpa")]
pub mod sherpa;

#[cfg(feature = "sherpa")]
pub use sherpa::{SherpaConfig, SherpaOfflineEngine};
