//! Downmixing and resampling of raw microphone audio to 16 kHz mono.

use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler as _};

use utter_core::SAMPLE_RATE;

use crate::error::AudioError;

/// Number of input frames the internal resampler consumes per fixed-size
/// chunk. Chosen as a compromise between resampling efficiency and the
/// buffering latency introduced before a chunk's worth of audio is
/// resampled.
const CHUNK_FRAMES: usize = 1024;

/// Downmixes interleaved multi-channel audio to mono and resamples it to
/// [`SAMPLE_RATE`] (16 kHz), emitting `i16` samples.
///
/// # Resampler choice
/// Resampling uses rubato's [`Async::new_poly`] with cubic interpolation.
/// rubato 4 replaced the older `FastFixedIn`/`SincFixedIn` types with a
/// single [`Async`] resampler offering either polynomial or sinc
/// interpolation; `new_poly` is the direct successor to `FastFixedIn`. It
/// has no anti-aliasing filter, so it is lower quality than sinc
/// interpolation, but consumer microphones already band-limit their signal
/// well below the source Nyquist frequency, and speech dictation has no need
/// for the extra quality (or the extra CPU cost and startup latency of sinc
/// interpolation).
///
/// # Streaming and buffering
/// Audio arrives from cpal's callback in irregularly sized chunks, while
/// rubato's `Async` resampler requires fixed-size input chunks. `Resampler`
/// bridges the two by buffering incoming (downmixed) samples internally and
/// only running the resampler once a full chunk is available; leftover
/// samples are carried over to the next [`process`](Resampler::process)
/// call, so no audio is ever dropped between calls. Call
/// [`flush`](Resampler::flush) at end-of-stream to push out the last,
/// possibly short, chunk.
pub struct Resampler {
    channels: usize,
    ratio: f64,
    inner: Option<Async<f32>>,
    pending: Vec<f32>,
}

impl Resampler {
    /// Builds a resampler that downmixes `in_channels`-channel audio at
    /// `in_rate` Hz to mono at [`SAMPLE_RATE`].
    ///
    /// When `in_rate` already equals [`SAMPLE_RATE`], no rubato instance is
    /// built and `process` becomes a pure downmix passthrough.
    ///
    /// # Errors
    /// Returns [`AudioError::Resampler`] if rubato's resampler cannot be
    /// constructed for the negotiated ratio. In practice this should not
    /// happen for any real device format (`in_rate` is always positive and
    /// the chunk size is a fixed non-zero constant), but the failure is
    /// surfaced to the caller rather than swallowed or panicked on.
    pub fn new(in_rate: u32, in_channels: u16) -> Result<Self, AudioError> {
        let channels = usize::from(in_channels.max(1));
        let ratio = f64::from(SAMPLE_RATE) / f64::from(in_rate.max(1));

        let inner = if in_rate == SAMPLE_RATE {
            None
        } else {
            // `max_resample_ratio_relative = 1.0`: the ratio is fixed for the
            // lifetime of this resampler, so no runtime adjustment range is needed.
            let resampler = Async::<f32>::new_poly(
                ratio,
                1.0,
                PolynomialDegree::Cubic,
                CHUNK_FRAMES,
                1,
                FixedAsync::Input,
            )
            .map_err(|e| AudioError::Resampler(e.to_string()))?;
            Some(resampler)
        };

        Ok(Self {
            channels,
            ratio,
            inner,
            pending: Vec::new(),
        })
    }

    /// Downmixes `interleaved` (channel-interleaved samples for
    /// `in_channels` channels) to mono and resamples it to 16 kHz, returning
    /// as many finished `i16` samples as are ready.
    ///
    /// Any samples that do not yet fill a full internal chunk are buffered
    /// and included in the next call (or in [`flush`](Resampler::flush));
    /// no audio is lost between calls. A trailing partial frame in
    /// `interleaved` (i.e. `interleaved.len()` not a multiple of the channel
    /// count) is dropped; cpal callbacks always deliver whole frames, so
    /// this should not occur in practice.
    pub fn process(&mut self, interleaved: &[f32]) -> Vec<i16> {
        self.pending.extend(downmix(interleaved, self.channels));

        let Some(inner) = self.inner.as_mut() else {
            return to_i16(&std::mem::take(&mut self.pending));
        };

        let mut out = Vec::new();
        loop {
            let chunk_frames = inner.input_frames_next();
            if self.pending.len() < chunk_frames {
                break;
            }
            match run_chunk(inner, &self.pending[..chunk_frames], None) {
                Some(samples) => out.extend(samples),
                None => break,
            }
            self.pending.drain(..chunk_frames);
        }
        out
    }

    /// Pushes any buffered, not-yet-resampled samples through the
    /// resampler, padding them with silence to fill the resampler's fixed
    /// chunk size, and returns the resulting trailing output.
    ///
    /// Call this once at end-of-stream to recover the last partial chunk.
    pub fn flush(&mut self) -> Vec<i16> {
        let Some(inner) = self.inner.as_mut() else {
            return to_i16(&std::mem::take(&mut self.pending));
        };

        if self.pending.is_empty() {
            return Vec::new();
        }

        let valid_frames = self.pending.len();
        let chunk_frames = inner.input_frames_next();
        self.pending.resize(chunk_frames, 0.0);

        let indexing = Indexing {
            partial_len: Some(valid_frames),
            ..Indexing::new()
        };
        let samples = run_chunk(inner, &self.pending, Some(&indexing)).unwrap_or_default();
        self.pending.clear();

        // The resampler always emits a full chunk's worth of output, padded
        // with whatever silence-derived values fall past the valid input;
        // trim to the number of samples the valid input actually accounts for.
        let expected_out = ((valid_frames as f64) * self.ratio).round() as usize;
        let keep = expected_out.min(samples.len());
        samples[..keep].to_vec()
    }
}

/// Runs one fixed-size chunk through `inner`, returning `i16` output
/// samples, or `None` if rubato rejected the input/output buffer sizes.
/// This is never expected given the sizes computed by the caller, but audio
/// code must never panic, so it is handled and logged instead.
fn run_chunk(
    inner: &mut Async<f32>,
    chunk: &[f32],
    indexing: Option<&Indexing>,
) -> Option<Vec<i16>> {
    let chunk_frames = chunk.len();
    let out_frames = inner.output_frames_next();

    let input_adapter = match SequentialSlice::new(chunk, 1, chunk_frames) {
        Ok(adapter) => adapter,
        Err(err) => {
            tracing::warn!("resampler rejected input buffer: {err}");
            return None;
        }
    };

    let mut out_buf = vec![0.0f32; out_frames];
    let mut output_adapter = match SequentialSlice::new_mut(&mut out_buf, 1, out_frames) {
        Ok(adapter) => adapter,
        Err(err) => {
            tracing::warn!("resampler rejected output buffer: {err}");
            return None;
        }
    };

    match inner.process_into_buffer(&input_adapter, &mut output_adapter, indexing) {
        Ok((_consumed, produced)) => {
            out_buf.truncate(produced);
            Some(to_i16(&out_buf))
        }
        Err(err) => {
            tracing::warn!("resampling failed: {err}");
            None
        }
    }
}

/// Downmixes `interleaved` (channel-interleaved samples, `channels` per
/// frame) to mono by averaging channels. A trailing partial frame (when
/// `interleaved.len()` is not a multiple of `channels`) is dropped.
fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Converts `f32` samples in `-1.0..=1.0` to clamped, full-scale `i16` samples.
fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generates `frames` interleaved stereo samples of a `freq` Hz sine at
    /// `rate` Hz, both channels identical, at `amplitude` (< 1.0 to avoid
    /// clipping on i16 conversion).
    fn sine_stereo(freq: f32, rate: u32, frames: usize, amplitude: f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let s = amplitude * (2.0 * std::f32::consts::PI * freq * t).sin();
            out.push(s);
            out.push(s);
        }
        out
    }

    fn to_i16(samples: &[f32]) -> Vec<i16> {
        samples
            .iter()
            .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
            .collect()
    }

    #[test]
    fn downsamples_48k_stereo_to_16k_mono_with_expected_length() {
        let frames = 9600; // 0.2s at 48kHz
        let input = sine_stereo(440.0, 48_000, frames, 0.8);

        let mut resampler = Resampler::new(48_000, 2).expect("valid resampler config");
        let mut output = resampler.process(&input);
        output.extend(resampler.flush());

        let expected = frames / 3;
        let diff = (output.len() as i64 - expected as i64).abs();
        assert!(
            diff <= 2,
            "expected ~{expected} samples, got {} (diff {diff})",
            output.len()
        );
    }

    #[test]
    fn downsampling_preserves_energy_within_20_percent() {
        let frames = 9600;
        let amplitude = 0.8;
        let input = sine_stereo(440.0, 48_000, frames, amplitude);

        // Downmixed mono input (both channels identical, so downmix is a no-op here).
        let mono_input: Vec<f32> = input.iter().step_by(2).copied().collect();
        let input_rms = crate::level::rms_level(&to_i16(&mono_input));

        let mut resampler = Resampler::new(48_000, 2).expect("valid resampler config");
        let mut output = resampler.process(&input);
        output.extend(resampler.flush());
        let output_rms = crate::level::rms_level(&output);

        let relative_diff = (output_rms - input_rms).abs() / input_rms;
        assert!(
            relative_diff < 0.2,
            "input rms {input_rms}, output rms {output_rms}, relative diff {relative_diff}"
        );
    }

    #[test]
    fn feeding_in_small_pieces_loses_no_samples_compared_to_one_big_call() {
        let frames = 9600;
        let input = sine_stereo(440.0, 48_000, frames, 0.8);

        let mut whole = Resampler::new(48_000, 2).expect("valid resampler config");
        let mut whole_out = whole.process(&input);
        whole_out.extend(whole.flush());

        let mut piecewise = Resampler::new(48_000, 2).expect("valid resampler config");
        let mut piecewise_out = Vec::new();
        // Odd chunk size (37 stereo frames = 74 values) that does not evenly
        // divide the internal chunk size, to exercise the leftover-buffering path.
        for chunk in input.chunks(74) {
            piecewise_out.extend(piecewise.process(chunk));
        }
        piecewise_out.extend(piecewise.flush());

        let diff = (whole_out.len() as i64 - piecewise_out.len() as i64).abs();
        assert!(
            diff <= 2,
            "whole-call output {} vs piecewise output {} (diff {diff})",
            whole_out.len(),
            piecewise_out.len()
        );
    }

    #[test]
    fn matching_input_rate_is_a_pure_downmix_passthrough() {
        let mut resampler = Resampler::new(SAMPLE_RATE, 1).expect("valid resampler config");
        let input = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let output = resampler.process(&input);
        assert_eq!(output, to_i16(&input));
    }

    #[test]
    fn passthrough_downmixes_stereo_to_mono() {
        // Stereo, in_rate == SAMPLE_RATE: each output sample is the average
        // of the two channel values in its frame.
        let mut resampler = Resampler::new(SAMPLE_RATE, 2).expect("valid resampler config");
        let input = vec![1.0f32, -1.0, 0.5, 0.5];
        let output = resampler.process(&input);
        assert_eq!(output, to_i16(&[0.0, 0.5]));
    }
}
