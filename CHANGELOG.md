# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-08-20

### Fixed

- Dictating into a terminal inserted text from some earlier point in the
  session instead of what was just said, while every other application
  received the right text. The transcript is published to both the CLIPBOARD
  and the PRIMARY selection, because the paste chord is Shift+Insert and VTE
  terminals read PRIMARY from it where other toolkits read CLIPBOARD — but
  each selection was published over a clipboard connection that was closed
  again the moment the call returned. A selection has no storage of its own;
  it is served by whoever owns it, and closing the connection gives that
  ownership up. CLIPBOARD hid the mistake, since the session's clipboard
  manager copies it the instant an owner disappears, which is what a
  clipboard manager is for. PRIMARY has no manager, so it was empty before
  the paste chord was even synthesized and the terminal pasted whatever the
  user had last selected with the mouse. One connection is now held open for
  as long as the injector lives.

## [0.3.0] - 2026-08-20

### Added

- Live preview: an optional second, streaming speech-to-text engine that
  shows words in the HUD while you are still speaking. It is a draft only —
  the text that gets injected still comes from the profile's own engine at
  the end of the utterance, and no preview text ever reaches the injected
  result or the dictation history. The preview is off by default, and a
  profile with no preview model selected behaves exactly as it did in 0.2.0.
- `SherpaStreamingEngine`, a second sherpa-onnx adapter built on that
  library's online recognizer, behind the same `SttEngine` port as the
  offline one. It decodes as samples arrive and hands back a partial only
  when the recognized text actually changed, so the HUD is not redrawn
  between chunks the recognizer had nothing new to say about.
- Two streaming models in the catalog, filed under an engine kind of their
  own (`sherpa-streaming`) so they can never be offered where the engine
  whose text gets injected is chosen: Zipformer Small for Russian (27 MB,
  int8) and for English (43 MB, int8). They are small deliberately — the
  preview decodes concurrently with the engine the user is waiting on, and
  is given a single inference thread to stay out of its way. The price is
  lower accuracy than the offline models and no punctuation at all, which is
  why their output never leaves the HUD.
- Per-profile preview selection: `[[profiles]].draft.model` names the
  streaming model a profile previews with, edited from a picker on the
  Profiles page that marks the models not downloaded yet. Omitting the table
  entirely, or leaving the model id blank, is the off state and the default.
- A "Live preview models" section on the Engines page, for downloading and
  removing the streaming models.
- A preview model that is missing, damaged, catalogued under the wrong kind,
  or unsupported by the running build leaves that profile's preview dark and
  says so, as an informational notice rather than a warning — it costs no
  word of anyone's transcript. A model that fails mid-utterance is reported
  the same way and switches the preview off for that profile until the app
  restarts: an engine that failed to decode one frame will fail on the next,
  and a notice per frame would be far worse than one notice and a dark
  preview.
- A model's catalog kind is now checked before any of its files are opened,
  on the streaming and the offline load path alike. sherpa-onnx terminates
  the process rather than returning an error when handed a model it cannot
  read, and the two kinds install under identical file names, so a perfectly
  intact model of the wrong kind was indistinguishable from a right one
  until the moment it was fatal. This is separate from the artifact size
  verification added in 0.2.0, which guards against a truncated download,
  and deliberately runs before it: neither question answers the other.

### Fixed

- The live preview was never drawn. The HUD window is a fixed size, and the
  pill inside it had a height set by hand that fitted the phase row and the
  level meter and nothing else, so the preview's row was laid out past the
  bottom edge of a window with no scrollbar and no room to grow, where the
  compositor clipped it away. The pill's height is now derived from the rows
  it actually shows, and the window is sized for the taller of its two
  states — a profile with no preview keeps exactly the pill it had before.
- Personal dictionary terms handed to sherpa-onnx as recognition hotwords had
  no effect on what it recognized. The score added to a hotword was left at
  the crate's default of `0.0`, which boosts a hotword by nothing at all,
  quietly making the whole feature inert instead of failing visibly. Both
  sherpa engines now set that score, and the beam width, explicitly rather
  than inheriting either — the two sherpa-onnx configuration types disagree
  on the beam width, and the streaming one defaults it to zero, which is an
  empty hypothesis set rather than a narrower search.
- Notices were emitted and then displayed nowhere unless they were errors.
  Every message about a degraded condition — a missing model, an unavailable
  preview, a profile with no hotkey, a history entry that could not be
  saved — reached the frontend and was dropped there, so the app's whole
  "degrade and say so" design said nothing at all. Notices of every severity
  are now shown as desktop notifications, which is the only channel that
  works during dictation, when there is no window open to show anything in;
  they are also listed in the settings window while it is open, where the
  full wording stays readable until dismissed. A message that repeats, or
  arrives on the heels of another, is rate limited: some conditions are
  reported once per audio frame, and a desktop notification per frame would
  be worse than the condition being reported.
- Notices reported while the app was starting up reached no one at all. Boot
  runs before the settings window's webview is loaded, and the event bus has
  no replay, so every `notice` fired there landed on nothing; the desktop
  notification was not a second chance either, since boot reports its notices
  back to back and the rate limit drops all but the first. A startup with two
  things to explain — no transcription model downloaded *and* a preview that
  could not be loaded — announced one of them, once. Startup notices are now
  held until a window exists and handed over when one opens, so the settings
  window lists all of them however many there were.
- `advanced.log_level` controlled nothing, because no tracing subscriber was
  ever installed. Every log line the app wrote, at every level, was
  discarded — including the ones explaining why something had just degraded.
  The setting now selects the maximum level actually written to stderr.

## [0.2.0] - 2026-08-07

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

- Clipboard-paste injection synthesizes Shift+Insert instead of Ctrl+V, and
  publishes the transcript to both the CLIPBOARD and PRIMARY selections.
  uinput emits raw key codes, which the compositor reads through whichever
  keyboard layout is active: with a Russian layout the Ctrl+V code means
  Ctrl+м, so nothing pasted and the bare letter "м" was inserted instead of
  the transcript. Insert carries no character, so it survives any layout.
- A synthesized chord now presses its modifier in its own input frame and
  releases it after the key, the way a real keyboard does, instead of
  reporting both keys in one frame. Applications that process an input frame
  in order could otherwise see the key before the modifier had been applied —
  a bare Insert, which pastes nothing. Wine and GTK tolerated the batched
  form; Chrome under Wayland did not, so dictation silently inserted nothing
  there.
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
