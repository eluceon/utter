#!/usr/bin/env bash
# Downloads the prebuilt Linux x86_64 libvosk shared library into
# ~/.local/share/utter/lib, so `cargo build --features vosk` (utter-stt) has
# something to link against.
#
# libvosk is a system shared library most machines don't have installed, and
# it isn't distributed on crates.io, so this script does what the vosk crate
# itself expects: https://docs.rs/vosk (see "Setup" in its README).
#
# Idempotent: if the library already looks present, the download/unzip steps
# are skipped and only the export lines are printed again.
set -euo pipefail

VOSK_VERSION="0.3.45"
ARCHIVE_NAME="vosk-linux-x86_64-${VOSK_VERSION}.zip"
DOWNLOAD_URL="https://github.com/alphacep/vosk-api/releases/download/v${VOSK_VERSION}/${ARCHIVE_NAME}"

INSTALL_DIR="${HOME}/.local/share/utter/lib"
# The zip extracts into a directory of this name; this is also the `-L`
# search path the rust linker needs.
LIB_DIR="${INSTALL_DIR}/vosk-linux-x86_64-${VOSK_VERSION}"

require_command() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "error: '$cmd' is required but was not found on PATH." >&2
        echo "Install it with your system package manager and re-run this script." >&2
        exit 1
    fi
}

require_command curl
require_command unzip

if [ -f "${LIB_DIR}/libvosk.so" ]; then
    echo "libvosk already present at ${LIB_DIR}, skipping download."
else
    mkdir -p "${INSTALL_DIR}"

    ARCHIVE_PATH="${INSTALL_DIR}/${ARCHIVE_NAME}"
    echo "Downloading ${DOWNLOAD_URL} ..."
    if ! curl -L --fail -sS -o "${ARCHIVE_PATH}" "${DOWNLOAD_URL}"; then
        echo "error: failed to download vosk release; check network and URL." >&2
        echo "URL: ${DOWNLOAD_URL}" >&2
        rm -f "${ARCHIVE_PATH}"
        exit 1
    fi

    echo "Extracting to ${INSTALL_DIR} ..."
    unzip -q -o "${ARCHIVE_PATH}" -d "${INSTALL_DIR}"
    rm -f "${ARCHIVE_PATH}"

    if [ ! -f "${LIB_DIR}/libvosk.so" ]; then
        echo "error: expected ${LIB_DIR}/libvosk.so after extraction but it's missing." >&2
        echo "The release archive layout may have changed; check ${DOWNLOAD_URL}" >&2
        exit 1
    fi
fi

cat <<EOF

libvosk is ready at: ${LIB_DIR}

Add these to your shell profile (or export them before building/running):

    export LD_LIBRARY_PATH="${LIB_DIR}\${LD_LIBRARY_PATH:+:\${LD_LIBRARY_PATH}}"
    export RUSTFLAGS="-L ${LIB_DIR}"

LD_LIBRARY_PATH is needed to run binaries built with the "vosk" feature;
RUSTFLAGS is needed to build/link them in the first place, e.g.:

    RUSTFLAGS="-L ${LIB_DIR}" cargo build -p utter-stt --features vosk
EOF
