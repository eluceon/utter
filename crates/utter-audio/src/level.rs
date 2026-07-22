//! Pure audio level and silence-detection helpers.

use std::time::{Duration, Instant};

/// Computes the RMS (root-mean-square) level of `samples`, relative to the
/// full-scale range of `i16`.
///
/// Returns `0.0..=1.0`, where `1.0` is a full-scale RMS signal (e.g. a
/// full-scale square wave). Returns `0.0` for an empty slice.
pub fn rms_level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f64 = samples
        .iter()
        .map(|&s| {
            let v = f64::from(s);
            v * v
        })
        .sum();
    let mean_square = sum_squares / samples.len() as f64;
    let rms = mean_square.sqrt() / f64::from(i16::MAX);
    (rms as f32).clamp(0.0, 1.0)
}

/// Detects sustained silence in a stream of per-frame RMS observations.
///
/// `sensitivity` (`0.0..=1.0`) is mapped linearly to an RMS threshold at or
/// below which a frame counts as silence:
/// `threshold = sensitivity.clamp(0.0, 1.0) * MAX_RMS_THRESHOLD`.
/// `0.0` requires bit-exact digital silence; `1.0` treats anything under
/// `MAX_RMS_THRESHOLD` of full-scale RMS as silence, which comfortably
/// covers typical room-noise floors.
///
/// # Semantics
/// [`observe`](SilenceDetector::observe) returns `true` exactly once
/// continuous silence has lasted at least `hold`. A speech frame (RMS above
/// the threshold) immediately resets the silence timer. Once fired, the
/// detector will not fire again on subsequent silent frames until a speech
/// frame is observed, re-arming it.
pub struct SilenceDetector {
    threshold: f32,
    hold: Duration,
    silence_since: Option<Instant>,
    fired: bool,
}

impl SilenceDetector {
    /// Builds a detector with the given `sensitivity` (see the type-level
    /// doc comment for the threshold mapping) and required continuous
    /// silence `hold` duration.
    pub fn new(sensitivity: f32, hold: Duration) -> Self {
        /// Upper bound of the sensitivity-to-threshold mapping: at
        /// `sensitivity == 1.0`, anything under 20% of full-scale RMS
        /// counts as silence, comfortably covering typical room-noise
        /// floors without ever mistaking quiet speech for silence.
        const MAX_RMS_THRESHOLD: f32 = 0.2;

        Self {
            threshold: sensitivity.clamp(0.0, 1.0) * MAX_RMS_THRESHOLD,
            hold,
            silence_since: None,
            fired: false,
        }
    }

    /// Feeds one frame's RMS level observed at time `now`. See the
    /// type-level doc comment for firing semantics.
    pub fn observe(&mut self, frame_rms: f32, now: Instant) -> bool {
        if frame_rms > self.threshold {
            // Speech: reset the silence run and re-arm firing.
            self.silence_since = None;
            self.fired = false;
            return false;
        }

        let silence_started = *self.silence_since.get_or_insert(now);
        if self.fired {
            return false;
        }

        if now.saturating_duration_since(silence_started) >= self.hold {
            self.fired = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_level_of_empty_slice_is_zero() {
        assert_eq!(rms_level(&[]), 0.0);
    }

    #[test]
    fn rms_level_of_silence_is_zero() {
        let samples = vec![0i16; 1600];
        assert_eq!(rms_level(&samples), 0.0);
    }

    #[test]
    fn rms_level_of_full_scale_square_wave_is_near_one() {
        let samples: Vec<i16> = (0..1600)
            .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
            .collect();
        let level = rms_level(&samples);
        assert!(
            (level - 1.0).abs() < 0.01,
            "expected level close to 1.0, got {level}"
        );
    }

    #[test]
    fn rms_level_is_never_above_one() {
        let samples: Vec<i16> = (0..1600)
            .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
            .collect();
        assert!(rms_level(&samples) <= 1.0);
    }

    #[test]
    fn rms_level_of_half_scale_is_about_half() {
        let half = i16::MAX / 2;
        let samples: Vec<i16> = (0..1600)
            .map(|i| if i % 2 == 0 { half } else { -half })
            .collect();
        let level = rms_level(&samples);
        assert!((level - 0.5).abs() < 0.02, "got {level}");
    }

    #[test]
    fn silence_detector_does_not_fire_before_hold_elapses() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::new(1.0, Duration::from_millis(500));

        assert!(!detector.observe(0.0, t0));
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(200)));
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(499)));
    }

    #[test]
    fn silence_detector_fires_once_hold_of_continuous_silence_elapses() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::new(1.0, Duration::from_millis(500));

        assert!(!detector.observe(0.0, t0));
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(300)));
        assert!(detector.observe(0.0, t0 + Duration::from_millis(500)));
    }

    #[test]
    fn silence_detector_resets_on_speech_before_hold_elapses() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::new(1.0, Duration::from_millis(500));

        assert!(!detector.observe(0.0, t0));
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(300)));
        // Speech interrupts the silence run.
        assert!(!detector.observe(0.8, t0 + Duration::from_millis(350)));
        // Silence resumes at t0+500ms, restarting the timer, so the original
        // deadline (t0 + 500ms) does not fire.
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(500)));
        // A full new `hold` after the restarted timer (t0+500ms + 500ms) does fire.
        assert!(detector.observe(0.0, t0 + Duration::from_millis(1000)));
    }

    #[test]
    fn silence_detector_does_not_refire_until_speech_resumes() {
        let t0 = Instant::now();
        let mut detector = SilenceDetector::new(1.0, Duration::from_millis(500));

        assert!(!detector.observe(0.0, t0));
        assert!(detector.observe(0.0, t0 + Duration::from_millis(500)));
        // Still silent: must not fire again.
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(600)));
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(2000)));

        // Speech re-arms the detector.
        assert!(!detector.observe(0.8, t0 + Duration::from_millis(2100)));
        // Silence resumes at t0+2200ms, restarting the timer.
        assert!(!detector.observe(0.0, t0 + Duration::from_millis(2200)));
        assert!(detector.observe(0.0, t0 + Duration::from_millis(2700)));
    }

    #[test]
    fn sensitivity_is_clamped_to_unit_range() {
        // Sensitivities outside 0..=1 must not panic and should behave as
        // their nearest boundary value.
        let mut low = SilenceDetector::new(-1.0, Duration::from_millis(100));
        let mut high = SilenceDetector::new(2.0, Duration::from_millis(100));
        let t0 = Instant::now();

        // `low` clamps to sensitivity 0.0 (threshold 0.0): 0.05 rms counts as
        // speech, so it never starts a silence run, let alone fires.
        assert!(!low.observe(0.05, t0));
        assert!(!low.observe(0.05, t0 + Duration::from_millis(200)));

        // `high` clamps to sensitivity 1.0 (threshold 0.2): 0.05 rms counts
        // as silence, so it fires once `hold` has elapsed.
        assert!(!high.observe(0.05, t0));
        assert!(high.observe(0.05, t0 + Duration::from_millis(100)));
    }
}
