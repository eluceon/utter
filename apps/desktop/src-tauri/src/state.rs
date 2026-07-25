//! Shared application state, managed by Tauri and reached from every
//! command through `tauri::State<AppState>`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use directories::ProjectDirs;

use utter_store::{HistoryRepo, ModelManager, Settings};

use crate::runtime::RuntimeHandle;

/// The name of the history database file under the app's XDG data directory.
const HISTORY_DB_FILE: &str = "history.sqlite3";

/// Application state shared across all Tauri commands.
///
/// `models` is wrapped in an `Arc` (rather than owned directly) so the async
/// `download_model` command can clone a handle to it into a `spawn_blocking`
/// closure without borrowing from a short-lived `tauri::State` guard.
pub struct AppState {
    pub settings: RwLock<Settings>,
    pub models: Arc<ModelManager>,
    pub history: Mutex<HistoryRepo>,
    /// The running dictation runtime's control handle. `None` only if boot
    /// (`runtime_boot::boot`) itself failed outright (an unexpected I/O
    /// error, not a degraded-but-booted condition like a missing model or
    /// hotkey permissions) — every command that reaches into this treats
    /// `None` as "no session control available yet" rather than panicking;
    /// the next successful `save_settings` spins one up (see
    /// `runtime_boot::rebuild`).
    pub session_ctl: Mutex<Option<RuntimeHandle>>,
    /// Live mirror of `settings.dictation.hud`, shared with every
    /// `TauriEventSink` (see `crate::sink`). A plain `RwLock<Settings>` read
    /// isn't enough on its own: the sink used by an already-running
    /// dictation `Runtime` is constructed once (at boot or the next
    /// `rebuild`) and kept for that runtime's whole lifetime, so this needs
    /// to be a shared, in-place-updatable cell rather than a value read
    /// fresh at sink-construction time, or a settings change wouldn't reach
    /// a sink that already exists.
    pub hud_enabled: Arc<AtomicBool>,
}

impl AppState {
    /// Builds application state: loads settings from disk (defaulting if
    /// absent), and opens the history database, creating both the on-disk
    /// config and data directories as needed.
    pub fn new() -> Result<Self> {
        let settings =
            utter_store::load(&utter_store::config_path()).context("failed to load settings")?;
        let hud_enabled = Arc::new(AtomicBool::new(settings.dictation.hud));

        let models = Arc::new(ModelManager::new(data_dir()?));
        let history =
            HistoryRepo::open(&history_db_path()?).context("failed to open history database")?;

        Ok(Self {
            settings: RwLock::new(settings),
            models,
            history: Mutex::new(history),
            session_ctl: Mutex::new(None),
            hud_enabled,
        })
    }
}

/// The per-user data directory for the app, under the `dev.utter.utter`
/// application identifier (matching [`utter_store::config_path`]'s
/// identifier triple).
fn data_dir() -> Result<PathBuf> {
    ProjectDirs::from("dev", "utter", "utter")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .context("failed to resolve the platform data directory")
}

/// The history database's on-disk path. Shared by [`AppState::new`] (which
/// opens the command-facing connection kept for the app's lifetime) and
/// `runtime_boot`, which opens its own separate connection for the dictation
/// worker thread whenever `Settings.history.enabled` is true.
pub(crate) fn history_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join(HISTORY_DB_FILE))
}
