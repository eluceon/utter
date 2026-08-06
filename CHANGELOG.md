# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Language profiles: an independent hotkey chord per language, each binding
  its own speech-to-text engine, model, and refinement policy. Pressing a
  profile's hotkey dictates in that language with everything else following
  automatically — no separate engine switch. Engines are built lazily, the
  first time a profile's hotkey is actually pressed rather than at app boot,
  and a profile whose model is missing or damaged degrades on its own
  without disabling any other profile's hotkey.
- Per-profile refinement policy: whether refinement runs, and which tone
  preset it uses, is now set per profile instead of once globally.
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
- `profile_id` recorded on every history entry, identifying which profile
  produced it — useful for a bilingual setup where two profiles share the
  same engine. The History page doesn't display it yet; this release only
  adds the column.

### Changed

- Decoding switches from greedy to beam search automatically once the
  dictionary has terms, so hotword biasing is available without the
  latency cost falling on users with an empty dictionary.
- sherpa-onnx inference threads default to half the available CPU cores,
  capped at four, to keep the desktop responsive during transcription.
- An unrecognized `engine.active` value on a profile in `config.toml` (for
  example `"vosk"`, left over from a v0.1 install) now falls back to the
  default engine at startup instead of preventing the app from starting.
- A v0.1 `config.toml` is migrated automatically the first time it is
  loaded: the original file is backed up to `config.toml.v1.bak`, and its
  hotkey, engine, refinement policy and tone are folded into one
  `LanguageProfile`. A config with `engine.active = "vosk"` is routed to the
  sherpa-onnx model for the same language, inferred from the vosk model's
  own name — `gigaam-v3-e2e-rnnt` for Russian, `parakeet-tdt-110m-en` for
  English or anything the name doesn't identify.

### Removed

- **Breaking:** the Vosk speech-to-text engine has been removed, replaced
  by sherpa-onnx. `scripts/setup-libvosk.sh` and the `vosk` Cargo feature
  are gone; sherpa-onnx links statically and needs no `RUSTFLAGS` /
  `LD_LIBRARY_PATH` setup. A v0.1 config that had `engine.active = "vosk"`
  is migrated to the sherpa-onnx model for its language rather than losing
  that setting — see Changed, above.
- **Breaking:** the top-level `[engine]` table and `dictation.hotkey` are
  gone from the config schema. Each language profile now carries its own
  `[[profiles]].engine` and `[[profiles]].hotkey` instead of the app having
  one engine and one hotkey shared by everything.
- **Breaking:** `refine.tone` moved to `[[profiles]].refine.tone` — the tone
  preset is set per profile now, not once globally.
- **Breaking:** `general.language` no longer affects dictation; each
  profile's own `language` field is what reaches the engine.

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
