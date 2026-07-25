use thiserror::Error;

/// Errors that can occur while capturing or enumerating microphone audio.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum AudioError {
    /// No default input device is available on this host.
    #[error("no default input device available")]
    NoDefaultDevice,

    /// A device matching the requested name could not be found.
    #[error("input device not found: {0}")]
    DeviceNotFound(String),

    /// Failed to enumerate devices or query the audio host.
    #[error("audio host error: {0}")]
    Host(String),

    /// The device's default input configuration could not be used (e.g. an
    /// unsupported sample format).
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),

    /// Failed to build the input stream.
    #[error("failed to build audio stream: {0}")]
    BuildStream(String),

    /// Failed to start (play) the input stream.
    #[error("failed to start audio stream: {0}")]
    Play(String),

    /// The resampler could not be constructed for the negotiated device format.
    #[error("failed to construct resampler: {0}")]
    Resampler(String),
}
