//! Settings, history and model storage.

pub mod error;
pub mod history;
pub mod models;
pub mod profile;
pub mod settings;

pub use error::IntegrityError;
pub use history::{HistoryEntry, HistoryRepo, NewEntry};
pub use models::{ModelInfo, ModelManager};
pub use profile::{DraftCfg, LanguageProfile, RefinePolicy};
pub use settings::{config_path, load, save, Settings};
