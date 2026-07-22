//! Settings, history and model storage.

pub mod history;
pub mod models;
pub mod settings;

pub use history::{HistoryEntry, HistoryRepo, NewEntry};
pub use models::{ModelInfo, ModelManager};
pub use settings::{config_path, load, save, Settings};
