use crate::error::{InjectError, RefineError, SttError};
use crate::types::{InjectionMethod, Tone, TranscribeOptions, Transcript};

/// Speech-to-text engine: call begin(), then feed*() (zero or more), then finish().
/// Implementations must be Send; single-threaded streaming allowed.
pub trait SttEngine: Send {
    fn begin(&mut self, opts: &TranscribeOptions) -> Result<(), SttError>;
    /// Feed 16 kHz mono i16 PCM. Streaming engines may return a partial transcript.
    fn feed(&mut self, samples: &[i16]) -> Result<Option<String>, SttError>;
    fn finish(&mut self) -> Result<Transcript, SttError>;
}

/// Text refiner: refines transcribed text by tone. Thread-safe (Send + Sync).
pub trait TextRefiner: Send + Sync {
    fn refine(&self, text: &str, tone: Tone) -> Result<String, RefineError>;
}

/// Text injector: injects refined text into the active window or clipboard.
/// Implementations must be Send; call order is flexible, but only one inject() call at a time.
pub trait TextInjector: Send {
    /// Returns the method that actually succeeded.
    fn inject(&mut self, text: &str) -> Result<InjectionMethod, InjectError>;
}
