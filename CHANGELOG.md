# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `sherpa-onnx` offline speech-to-text engine, with one transducer model per
  supported language: GigaAM-v3 for Russian and Parakeet TDT 110M for
  English. Both emit punctuation and capitalization directly from audio and
  both accept personal dictionary terms as recognition hotwords.
- Model catalog support for models made of several files (encoder, decoder,
  joiner, tokens), installed and verified as a set.
- Artifact size verification before a downloaded model is handed to the
  sherpa-onnx engine, so a truncated download is reported as a "damaged
  model" notice instead of reaching a native decoder that cannot fail
  gracefully.

### Changed

- Decoding switches from greedy to beam search automatically once the
  dictionary has terms, so hotword biasing is available without the
  latency cost falling on users with an empty dictionary.
- sherpa-onnx inference threads default to half the available CPU cores,
  capped at four, to keep the desktop responsive during transcription.
- An unrecognized `engine.active` value in `config.toml` (for example
  `"vosk"`, left over from a v0.1 install) now falls back to the default
  engine at startup instead of preventing the app from starting.

### Removed

- **Breaking:** the Vosk speech-to-text engine has been removed, replaced
  by sherpa-onnx. `scripts/setup-libvosk.sh` and the `vosk` Cargo feature
  are gone; sherpa-onnx links statically and needs no `RUSTFLAGS` /
  `LD_LIBRARY_PATH` setup. A `config.toml` with `engine.active = "vosk"`
  now falls back to the default engine rather than being migrated —
  carrying the rest of a v0.1 config forward (including which model was
  selected) is planned for a later release.

## [0.1.0] - 2026-07-25

### Added

- Dictation session with push-to-talk and toggle modes, a configurable
  global hotkey (default `Ctrl+Super`), and a HUD overlay showing recording /
  transcribing / refining state with a live input level meter.
- Speech-to-text via `whisper.cpp` (batch), `Vosk` (streaming, with live
  partial results), or an OpenAI-compatible cloud endpoint (BYOK).
- A model manager for browsing, downloading (with checksum verification),
  and removing speech-to-text models.
- Optional AI text refinement against any OpenAI-compatible
  `/chat/completions` endpoint, including local Ollama setups, with tone
  presets (verbatim, clean, formal, notes, code-comment) and a fallback to
  the raw transcript on timeout or failure.
- Personal dictionary: custom terms hinted to the engine and refiner, plus
  literal replacement rules applied to every transcript.
- Voice snippets: spoken trigger phrases that expand to a stored template.
- Local SQLite dictation history with search and delete, toggleable off
  entirely; audio itself is never persisted.
- Text injection strategy chain for Linux (Wayland and X11): clipboard-paste,
  direct typing, and clipboard-only, with automatic fallback between them.
- Tray icon with quick engine/refinement toggles, and a settings window
  covering general preferences, dictation, engines, refinement, dictionary,
  snippets, history, and advanced options.
- First-run onboarding: microphone check, model download, hotkey selection,
  and a permissions check with a one-line fix for missing `input`/`uinput`
  access.
- TOML settings persisted to `~/.config/utter/config.toml`, hot-reloaded on
  change.
