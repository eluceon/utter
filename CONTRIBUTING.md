# Contributing

## Dev setup

System dependencies (Debian/Ubuntu; see the [Tauri prerequisites
guide](https://tauri.app/start/prerequisites/) for other distributions):

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libasound2-dev \
  libayatana-appindicator3-dev librsvg2-dev \
  pkg-config build-essential cmake
```

You'll also need Node.js 20+ and a stable Rust toolchain (`rustfmt` and
`clippy` components included).

```sh
cd apps/desktop/ui && npm ci && cd ../../..
cargo tauri dev
```

The `sherpa` engine feature links sherpa-onnx statically; its build script
downloads a prebuilt native archive on first build, so building with
`--features sherpa` needs network access the first time but no extra linker
setup. It's optional: the default feature set (whisper.cpp + cloud STT)
builds and runs without it.

## Workspace layout

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the crate map, the
ports-and-adapters design, the session state machine, and the reasoning
behind the bigger structural decisions. Read it before adding a new crate or
crossing a port boundary.

## Running tests

```sh
cargo test --workspace
npm test --prefix apps/desktop/ui
```

A handful of Rust tests are `#[ignore]`d because they need real hardware or
network access rather than being genuinely non-deterministic:

- `crates/utter-stt/src/whisper.rs` — downloads a real model over the
  network and runs inference against it.
- `crates/utter-audio/src/capture.rs` — opens a real microphone via `cpal`.
- `crates/utter-inject/src/inject.rs`, `crates/utter-inject/src/hotkey_evdev.rs`
  — need a readable `/dev/input` device and/or a writable `/dev/uinput`
  (see the permissions fix in the README's Quick start section).

Run them explicitly and selectively when touching that code, e.g.:

```sh
cargo test -p utter-stt -- --ignored
```

## Lint gates

CI (`.github/workflows/ci.yml`) runs, and any change must pass, all of:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo clippy --workspace --all-targets --features sherpa -- -D warnings
cargo test --workspace --features sherpa
```

## Commit style

[Conventional Commits](https://www.conventionalcommits.org/), imperative
mood, one logical change per commit — e.g. `fix(inject): restore clipboard
after paste`, `feat(store): add snippet CRUD`. Keep commits small enough to
review in isolation.

## Pull requests

- Add or update tests for any behavior change; a PR that changes behavior
  with no corresponding test needs a good reason in the description.
- `cargo fmt`, `clippy -D warnings`, and the full test suite must be green
  before requesting review.
- Keep the diff focused — unrelated cleanup belongs in its own PR.
