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
    /// Set when settings could not be loaded because a v0.1 config failed to
    /// migrate (see `utter_store::MigrationFailed`): the app boots with
    /// `Settings::default()` for this run and `runtime_boot::boot` queues
    /// this as a user-facing notice once the runtime is up. `None` on every
    /// other startup, including a normal migration.
    pub startup_notice: Option<String>,
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
    ///
    /// A config that fails to migrate degrades rather than aborting startup:
    /// the original file is left exactly as `utter_store::load` left it (see
    /// its doc comment), this run boots with `Settings::default()`, and
    /// `startup_notice` carries a message for `runtime_boot::boot` to queue.
    /// Any other load failure (unreadable file, genuinely malformed TOML
    /// unrelated to migration) still aborts startup, as it did before.
    pub fn new() -> Result<Self> {
        let config_path = utter_store::config_path();
        let (settings, startup_notice) = match utter_store::load(&config_path) {
            Ok(settings) => (settings, None),
            Err(err) => match err.downcast_ref::<utter_store::MigrationFailed>() {
                Some(failed) => {
                    tracing::warn!("{err:#}");
                    (Settings::default(), Some(migration_notice(failed)))
                }
                None => return Err(err).context("failed to load settings"),
            },
        };
        let hud_enabled = Arc::new(AtomicBool::new(settings.dictation.hud));

        let models = Arc::new(ModelManager::new(data_dir()?));
        let history =
            HistoryRepo::open(&history_db_path()?).context("failed to open history database")?;

        Ok(Self {
            settings: RwLock::new(settings),
            models,
            history: Mutex::new(history),
            session_ctl: Mutex::new(None),
            startup_notice,
            hud_enabled,
        })
    }
}

/// Builds the notice `AppState::new` queues for a config that could not be
/// migrated. Names the backup only when `failed.backup` is `Some` — a
/// `None` means the backup step itself is what failed, so the file at
/// `failed.path` (left untouched) is the user's only copy, and the message
/// must not claim a safety net that was never written.
fn migration_notice(failed: &utter_store::MigrationFailed) -> String {
    match &failed.backup {
        Some(backup) => format!(
            "Your settings at {} could not be upgraded to the new format and \
             were left unchanged; a backup was saved at {}. Utter is running \
             with default settings for now.",
            failed.path.display(),
            backup.display()
        ),
        None => format!(
            "Your settings at {} could not be upgraded to the new format and \
             were left unchanged. Utter is running with default settings for now.",
            failed.path.display()
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_without_a_backup_does_not_claim_one_was_saved() {
        // `backup: None` is what a real `MigrationFailed` carries when the
        // backup step itself is what failed — see
        // `utter_store::settings::migrate_and_persist`. The notice built
        // from it must not tell the user a backup exists.
        let failed = utter_store::MigrationFailed {
            path: PathBuf::from("/home/user/.config/utter/config.toml"),
            backup: None,
        };

        let notice = migration_notice(&failed);

        assert!(
            !notice.to_lowercase().contains("backup"),
            "no backup was written, so the notice must not mention one: {notice}"
        );
        assert!(notice.contains("config.toml"), "must still name the file");
    }

    #[test]
    fn a_notice_with_a_backup_names_it() {
        let failed = utter_store::MigrationFailed {
            path: PathBuf::from("/home/user/.config/utter/config.toml"),
            backup: Some(PathBuf::from("/home/user/.config/utter/config.toml.v1.bak")),
        };

        let notice = migration_notice(&failed);

        assert!(notice.contains("config.toml.v1.bak"));
    }
}
