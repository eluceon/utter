//! Settings, history and model storage.

pub mod history;
pub mod settings;

pub use history::{HistoryEntry, HistoryRepo, NewEntry};
pub use settings::{config_path, load, save, Settings};
