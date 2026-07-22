//! Domain model and ports for the Utter dictation pipeline.

pub mod error;
pub mod ports;
pub mod types;

pub use error::{InjectError, RefineError, SttError};
pub use ports::{SttEngine, TextInjector, TextRefiner};
pub use types::{InjectionMethod, Tone, TranscribeOptions, Transcript, SAMPLE_RATE};
