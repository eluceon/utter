//! Microphone capture via cpal, resampled to 16 kHz mono before being sent downstream.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    FromSample, InputCallbackInfo, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
};
use crossbeam_channel::Sender;
use utter_core::SAMPLE_RATE;

use crate::error::AudioError;
use crate::resample::Resampler;
use crate::AudioFrame;

/// Number of mono 16 kHz samples per emitted [`AudioFrame`] (~100 ms).
const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize) / 10;

/// Bridges [`Resampler`]'s streaming output to fixed-size [`AudioFrame`]s.
///
/// This is the piece of state shared (behind an `Arc<Mutex<_>>`) between the
/// cpal capture callback, which calls [`push`](FrameAssembler::push) as
/// audio arrives, and [`Capture::stop`], which calls
/// [`finish`](FrameAssembler::finish) once to flush whatever is left
/// buffered — up to one resampler chunk plus up to one partial frame —
/// instead of silently discarding the last fragment of speech.
struct FrameAssembler {
    resampler: Resampler,
    pending_frame: Vec<i16>,
}

impl FrameAssembler {
    fn new(resampler: Resampler) -> Self {
        Self {
            resampler,
            pending_frame: Vec::new(),
        }
    }

    /// Resamples `floats` and returns zero or more complete `~100ms` frames.
    fn push(&mut self, floats: &[f32]) -> Vec<AudioFrame> {
        self.pending_frame.extend(self.resampler.process(floats));
        self.drain_complete_frames()
    }

    /// Flushes the resampler's internal buffer and the frame accumulator,
    /// returning any newly complete frames plus a final, possibly short,
    /// trailing frame if samples remain. Intended to be called exactly once,
    /// at end of stream; the assembler is left empty afterwards.
    fn finish(&mut self) -> Vec<AudioFrame> {
        self.pending_frame.extend(self.resampler.flush());
        let mut frames = self.drain_complete_frames();
        if !self.pending_frame.is_empty() {
            frames.push(AudioFrame {
                samples: std::mem::take(&mut self.pending_frame),
            });
        }
        frames
    }

    fn drain_complete_frames(&mut self) -> Vec<AudioFrame> {
        let mut frames = Vec::new();
        while self.pending_frame.len() >= FRAME_SAMPLES {
            let samples: Vec<i16> = self.pending_frame.drain(..FRAME_SAMPLES).collect();
            frames.push(AudioFrame { samples });
        }
        frames
    }
}

/// Locks `assembler`, recovering from mutex poisoning rather than panicking.
/// The audio callback and `Capture::stop` must never panic; if a prior
/// holder of the lock panicked, its half-updated state is still the best
/// data available and is preferable to losing captured audio outright.
fn lock_assembler(assembler: &Mutex<FrameAssembler>) -> std::sync::MutexGuard<'_, FrameAssembler> {
    assembler
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An active microphone capture stream.
///
/// Wraps a [`cpal::Stream`]. cpal stream handles are not `Send` on every
/// platform (some backends hold platform-specific handles tied to the
/// thread that created them), so `Capture` is intentionally not `Send`
/// either: it must be created, used, and dropped on the same worker
/// thread — which is how the desktop app's audio worker is structured.
pub struct Capture {
    stream: Stream,
    assembler: Arc<Mutex<FrameAssembler>>,
    tx: Sender<AudioFrame>,
}

impl Capture {
    /// Starts capturing from `device` (by name; `None` selects the host's
    /// default input device), using that device's default input
    /// configuration. Captured audio is downmixed and resampled to 16 kHz
    /// mono and delivered as ~100 ms [`AudioFrame`]s via `tx` until
    /// [`stop`](Capture::stop) is called or `tx`'s receiver is dropped (in
    /// which case frames are silently discarded rather than panicking).
    pub fn start(device: Option<&str>, tx: Sender<AudioFrame>) -> Result<Capture, AudioError> {
        let host = cpal::default_host();

        let device = match device {
            Some(name) => host
                .input_devices()
                .map_err(|e| AudioError::Host(e.to_string()))?
                .find(|d| d.to_string() == name)
                .ok_or_else(|| AudioError::DeviceNotFound(name.to_string()))?,
            None => host
                .default_input_device()
                .ok_or(AudioError::NoDefaultDevice)?,
        };

        let supported_config = device
            .default_input_config()
            .map_err(|e| AudioError::UnsupportedFormat(e.to_string()))?;

        let sample_format = supported_config.sample_format();
        let in_rate = supported_config.sample_rate();
        let in_channels = supported_config.channels();
        let config: StreamConfig = supported_config.config();

        let resampler = Resampler::new(in_rate, in_channels)?;
        let assembler = Arc::new(Mutex::new(FrameAssembler::new(resampler)));

        let stream = match sample_format {
            SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, assembler.clone(), tx.clone())?
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, assembler.clone(), tx.clone())?
            }
            SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, assembler.clone(), tx.clone())?
            }
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "unsupported sample format: {other:?}"
                )))
            }
        };

        stream.play().map_err(|e| AudioError::Play(e.to_string()))?;

        Ok(Capture {
            stream,
            assembler,
            tx,
        })
    }

    /// Stops capture, flushing any audio buffered by the resampler or the
    /// frame accumulator as one final (possibly short) [`AudioFrame`]
    /// before releasing the underlying stream.
    pub fn stop(self) {
        // Drop the stream first so the capture callback stops running
        // before its shared state is flushed from this thread.
        drop(self.stream);

        let mut assembler = lock_assembler(&self.assembler);
        for frame in assembler.finish() {
            // The receiver may already be gone (e.g. the app is shutting
            // down); the flushed audio is then discarded rather than
            // panicking.
            let _ = self.tx.send(frame);
        }
    }
}

/// Builds and configures (but does not start) an input stream that reads
/// samples of type `T`, converts them to `f32`, feeds them through the
/// shared `assembler`, and sends completed ~100 ms frames to `tx`.
fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    assembler: Arc<Mutex<FrameAssembler>>,
    tx: Sender<AudioFrame>,
) -> Result<Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let data_callback = move |data: &[T], _info: &InputCallbackInfo| {
        let floats: Vec<f32> = data.iter().map(|&s| f32::from_sample(s)).collect();

        let frames = lock_assembler(&assembler).push(&floats);
        for frame in frames {
            // The receiver may have been dropped (e.g. the app is shutting
            // down); that just means this and future frames are discarded.
            // The audio callback must never panic, so the error is ignored.
            let _ = tx.send(frame);
        }
    };

    let error_callback = |err: cpal::Error| {
        tracing::error!("audio input stream error: {err}");
    };

    device
        .build_input_stream(*config, data_callback, error_callback, None)
        .map_err(|e| AudioError::BuildStream(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// Hardware-bound smoke test: records ~1s from the default input device
    /// and checks that at least a few frames of plausible size arrive. Not
    /// run in CI (no guaranteed audio hardware); run manually with:
    /// `cargo test -p utter-audio --lib records_one_second -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn records_one_second() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let capture = Capture::start(None, tx).expect("failed to start capture");

        thread::sleep(Duration::from_secs(1));
        capture.stop();

        let mut total_samples = 0;
        while let Ok(frame) = rx.try_recv() {
            total_samples += frame.samples.len();
        }

        println!("captured {total_samples} samples in ~1s");
        assert!(total_samples > 0, "expected some captured audio");
    }

    /// Reproduces the trailing-audio-loss bug: pushes a number of samples
    /// that is not a multiple of the frame size (so some audio is left
    /// sitting in the frame accumulator), then calls `finish` the same way
    /// `Capture::stop` does, and checks that every sample makes it out.
    #[test]
    fn finish_emits_trailing_partial_frame_without_losing_samples() {
        // in_rate == SAMPLE_RATE, mono: a pure downmix passthrough, so the
        // sample count out is deterministic and directly comparable to the
        // sample count in.
        let resampler = Resampler::new(SAMPLE_RATE, 1).expect("valid resampler config");
        let mut assembler = FrameAssembler::new(resampler);

        // Two full frames plus a 400-sample remainder, fed across several
        // `push` calls of irregular size, like a real capture callback would.
        let chunk_sizes = [300usize, 700, 1000, 1600];
        let total_in: usize = chunk_sizes.iter().sum();
        assert_eq!(total_in, FRAME_SAMPLES * 2 + 400);

        let mut collected = Vec::new();
        for &chunk in &chunk_sizes {
            let floats: Vec<f32> = (0..chunk).map(|i| (i as f32 % 100.0) / 100.0).collect();
            collected.extend(assembler.push(&floats));
        }
        collected.extend(assembler.finish());

        let total_out: usize = collected.iter().map(|f| f.samples.len()).sum();
        assert_eq!(
            total_out, total_in,
            "no samples should be lost across push/finish"
        );

        // The final frame should be the short trailing one, not a full 1600.
        let last = collected.last().expect("at least one frame emitted");
        assert_eq!(last.samples.len(), 400);
    }

    /// Same guarantee, but with actual resampling (48kHz -> 16kHz) in play,
    /// so `finish` must flush both the resampler's internal leftover buffer
    /// (see `resample::tests`) and the frame accumulator.
    #[test]
    fn finish_flushes_resampler_leftovers_through_the_frame_accumulator() {
        let resampler = Resampler::new(48_000, 1).expect("valid resampler config");
        let mut assembler = FrameAssembler::new(resampler);

        let total_in = 9600; // 0.2s at 48kHz mono
        let floats: Vec<f32> = (0..total_in)
            .map(|i| (i as f32 * 0.01).sin() * 0.5)
            .collect();

        let mut collected = Vec::new();
        for chunk in floats.chunks(777) {
            collected.extend(assembler.push(chunk));
        }
        collected.extend(assembler.finish());

        let total_out: usize = collected.iter().map(|f| f.samples.len()).sum();
        let expected = total_in / 3; // 48kHz -> 16kHz is an exact 3:1 ratio
        let diff = (total_out as i64 - expected as i64).abs();
        assert!(
            diff <= 2,
            "expected ~{expected} samples out, got {total_out} (diff {diff})"
        );
    }
}
