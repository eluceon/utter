//! Audio capture, resampling and level detection.
//!
//! Captures microphone audio via cpal, downmixes and resamples it to 16 kHz
//! mono `i16` (matching [`utter_core::SAMPLE_RATE`]), and exposes pure
//! level/silence-detection helpers. Captured audio is never written to
//! disk: it only ever exists in memory as it flows toward transcription.

mod capture;
mod error;
mod level;
mod resample;

pub use capture::Capture;
pub use error::AudioError;
pub use level::{rms_level, SilenceDetector};
pub use resample::Resampler;

/// A chunk of captured audio: 16 kHz mono `i16` samples, nominally ~100 ms
/// (1600 samples) per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub samples: Vec<i16>,
}

/// Lists the names of available audio input devices.
///
/// Best-effort: returns an empty vector if the audio host cannot be queried
/// (e.g. no audio subsystem is running) rather than propagating an error.
pub fn list_input_devices() -> Vec<String> {
    use cpal::traits::HostTrait;

    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    devices.map(|d| d.to_string()).collect()
}
