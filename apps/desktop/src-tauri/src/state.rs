//! Shared application state, managed by Tauri and reached from every
//! command through `tauri::State<AppState>`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use directories::ProjectDirs;

use utter_store::{HistoryRepo, ModelManager, Settings};

/// The name of the history database file under the app's XDG data directory.
const HISTORY_DB_FILE: &str = "history.sqlite3";

/// Application state shared across all Tauri commands.
///
/// `models` is wrapped in an `Arc` (rather than owned directly) so the async
/// `download_model` command can clone a handle to it into a `spawn_blocking`
/// closure without borrowing from a short-lived `tauri::State` guard.
///
/// Note: this struct intentionally has no `session_ctl` (dictation session
/// control) field yet — that belongs to the runtime orchestrator introduced
/// in a later task, not to this shell.
pub struct AppState {
    pub settings: RwLock<Settings>,
    pub models: Arc<ModelManager>,
    pub history: Mutex<HistoryRepo>,
}

impl AppState {
    /// Builds application state: loads settings from disk (defaulting if
    /// absent), and opens the history database, creating both the on-disk
    /// config and data directories as needed.
    pub fn new() -> Result<Self> {
        let settings =
            utter_store::load(&utter_store::config_path()).context("failed to load settings")?;

        let data_dir = data_dir()?;
        let models = Arc::new(ModelManager::new(data_dir.clone()));
        let history = HistoryRepo::open(&data_dir.join(HISTORY_DB_FILE))
            .context("failed to open history database")?;

        Ok(Self {
            settings: RwLock::new(settings),
            models,
            history: Mutex::new(history),
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
