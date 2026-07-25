//! Spike: prove end-to-end offline transcription with sherpa-onnx.
//!
//! This is throwaway exploration code for Task 1 of the v0.2 plan — it exists
//! to pin down the real `sherpa-onnx` API surface before any adapter code is
//! written against it. Run with:
//!
//! ```bash
//! cargo run -p utter-stt --features sherpa --example sherpa_spike -- /path/to/ru.wav
//! ```

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    Wave,
};

fn main() -> ExitCode {
    let Some(wav_path) = env::args().nth(1) else {
        eprintln!("usage: sherpa_spike <path-to-16khz-mono-wav>");
        return ExitCode::FAILURE;
    };

    let model_dir = env::var("SHERPA_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_model_dir().join("gigaam-v3-e2e-rnnt"));

    let encoder = model_dir.join("encoder.int8.onnx");
    let decoder = model_dir.join("decoder.onnx");
    let joiner = model_dir.join("joiner.onnx");
    let tokens = model_dir.join("tokens.txt");

    let config = OfflineRecognizerConfig {
        model_config: OfflineModelConfig {
            transducer: OfflineTransducerModelConfig {
                encoder: Some(encoder.to_string_lossy().into_owned()),
                decoder: Some(decoder.to_string_lossy().into_owned()),
                joiner: Some(joiner.to_string_lossy().into_owned()),
            },
            tokens: Some(tokens.to_string_lossy().into_owned()),
            num_threads: 2,
            // The NeMo transducer export needs this hint; without it
            // sherpa-onnx assumes the icefall transducer layout, which has a
            // different state shape and fails to load.
            model_type: Some("nemo_transducer".into()),
            ..Default::default()
        },
        decoding_method: Some("greedy_search".into()),
        ..Default::default()
    };

    let Some(recognizer) = OfflineRecognizer::create(&config) else {
        eprintln!("failed to create recognizer from {}", model_dir.display());
        return ExitCode::FAILURE;
    };

    let Some(wave) = Wave::read(&wav_path) else {
        eprintln!("failed to read wav file {wav_path}");
        return ExitCode::FAILURE;
    };

    let stream = recognizer.create_stream();
    stream.accept_waveform(wave.sample_rate(), wave.samples());
    recognizer.decode(&stream);

    match stream.get_result() {
        Some(result) => {
            println!("{}", result.text);
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("recognizer returned no result");
            ExitCode::FAILURE
        }
    }
}

fn dirs_model_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".local/share/utter/models")
}
