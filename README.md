# Utter

**Utter it. It types.**

Utter is a privacy-first, Linux-first desktop dictation app. Press a global
hotkey, speak, and clean, formatted text lands in whatever field currently
has focus — a terminal, an editor, a chat window, anything. Speech recognition
runs locally by default (whisper.cpp or Vosk); an optional LLM pass cleans up
filler words and punctuation before the text is typed. Nothing about the
pipeline is fixed: engine, model, refinement provider, and injection method
are all swappable in settings.

Audio never touches disk, API keys live in the OS keyring, and the app makes
no network calls except the ones you configure yourself (a refinement
endpoint, a model download).

## Features

- **Dictation session** — push-to-talk or toggle mode, a small always-on-top
  HUD showing recording/transcribing/refining state and a live input level
  meter, cancel with Escape or a hotkey tap.
- **Speech-to-text engines** — `whisper.cpp` for accurate batch transcription
  (tiny through large-v3-turbo, quantized variants included), `Vosk` for
  low-latency streaming with live partial results, or any OpenAI-compatible
  cloud `/audio/transcriptions` endpoint (BYOK).
- **AI text refinement** — optional pass over the transcript: removes filler
  words, fixes punctuation and casing, applies a tone preset (`verbatim`,
  `clean`, `formal`, `notes`, `code-comment`). Works against any
  OpenAI-compatible `/chat/completions` endpoint, including a fully local
  **Ollama** setup. If refinement fails or times out, the raw transcript is
  injected instead of losing the dictation.
- **Personal dictionary** — custom terms hinted to the engine and the
  refiner, plus literal "heard X, write Y" replacement rules applied to every
  transcript.
- **Snippets** — a spoken trigger phrase expands to a stored template,
  bypassing refinement entirely.
- **History** — a local SQLite log of past dictations (text, engine,
  duration, target app) with search and delete; disable it entirely if you
  don't want it. Audio itself is never stored, history setting or not.
- **Text injection** — clipboard-paste (fastest, default), direct typing, or
  clipboard-only as a universal fallback, tried in order until one works.
- **Tray and settings UI** — quick toggles for engine and refinement, a full
  settings window, and a first-run onboarding flow that walks through mic
  check, model download, hotkey choice, and permissions.

## Utter vs. the alternatives

| | **Utter** | Wispr Flow | Handy |
|---|---|---|---|
| Open source | Yes (MIT/Apache-2.0) | No | Yes |
| Linux support | Yes, first-class | No | Yes |
| Local processing | Yes, default | No (cloud-only) | Yes |
| AI text refinement | Yes — tone presets, any OpenAI-compatible endpoint incl. Ollama | Yes (cloud) | Yes |
| Personal dictionary | Yes | Yes | Yes |
| Snippets | Yes | No | No |
| Price | Free | Subscription | Free |

Wispr Flow and Handy are both good products; the comparison is here so you
can pick the right tool. Utter's bet is that flexibility — swappable engine,
model, refiner, and injection method — matters more than any single
default choice.

## Install

Prebuilt `.deb` and AppImage packages will be published on the
[Releases](https://github.com/eluceon/utter/releases) page starting with the
v0.1 release.

### Build from source

System dependencies (Debian/Ubuntu; see the [Tauri prerequisites
guide](https://tauri.app/start/prerequisites/) for other distributions):

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libasound2-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

You'll also need Node.js 20+ and a stable Rust toolchain.

```sh
git clone https://github.com/eluceon/utter.git
cd utter

cd apps/desktop/ui && npm ci && cd ../../..

cargo tauri dev     # run in development
cargo tauri build   # produce a release bundle
```

The `vosk` engine links against `libvosk`, a shared library not distributed
on crates.io. If you want it, run `scripts/setup-libvosk.sh` first and build
with `--features vosk` (`cargo tauri dev --features vosk` /
`cargo build -p utter-stt --features vosk`); without it, whisper.cpp and
cloud STT still work out of the box.

## Quick start

On first launch, onboarding walks through a microphone check, downloading a
speech-to-text model, picking a hotkey, and a permissions check.

The default hotkey is `Ctrl+Super`, held to record (push-to-talk). Utter
reads keyboard events directly from `/dev/input` (evdev) and synthesizes the
paste keystroke through its own virtual keyboard device (`/dev/uinput`),
since Wayland has no standard global-hotkey protocol. Both require the
current user to have access to those device nodes; if onboarding reports
missing permissions, it shows the exact fix:

```sh
sudo usermod -aG input $USER && \
  echo 'KERNEL=="uinput", MODE="0660", GROUP="input"' | \
  sudo tee /etc/udev/rules.d/60-utter-uinput.rules && \
  sudo udevadm control --reload-rules && sudo udevadm trigger
# log out and back in for group membership to take effect
```

## Configuration

Settings live in `~/.config/utter/config.toml`, a plain TOML file (XDG
config dir), reloaded automatically when changed through the settings UI. A
missing file just means defaults; unknown keys are ignored rather than
rejected, so the format tolerates being hand-edited or partially upgraded.

- **Engines** — pick `whisper`, `vosk`, or `cloud` as the active
  speech-to-text engine; whisper models download to
  `~/.local/share/utter/models`.
- **Refinement** — point `refine.base_url` / `refine.model` at any
  OpenAI-compatible chat endpoint. For a fully local setup, run
  [Ollama](https://ollama.com) and use its default `http://localhost:11434/v1`
  — no API key required. Cloud providers store their key in the OS keyring,
  never in `config.toml`.
- **Dictionary and snippets** — custom terms and replacement rules live
  under `[dictionary]`; snippets are a list of trigger/body pairs under
  `[[snippets]]`. Both are editable from the settings UI.

## Privacy

- Audio is processed in memory only and is never written to disk.
- API keys are stored in the OS keyring (Secret Service on Linux), never in
  the settings file.
- No telemetry, no analytics, no background network calls. The only network
  traffic Utter ever makes is to the STT/refinement endpoint you configure
  and to fetch model files you explicitly download.

## Architecture

Workspace layout, the ports-and-adapters design, the session state machine,
and the key engineering decisions are documented in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). Contribution guidelines,
including dev setup and test/lint gates, are in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Roadmap

- **v0.2** — Windows text-injection adapter, a hybrid mode (live Vosk draft
  replaced by a final Whisper pass), per-app tone profiles.
- **v0.3** — macOS adapter, sherpa-onnx as an additional streaming engine.
- **Later** — voice commands ("new line", "undo that"), translation mode.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at
your option.
