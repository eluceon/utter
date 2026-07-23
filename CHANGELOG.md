# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
