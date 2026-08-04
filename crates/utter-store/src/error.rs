//! Typed errors for this crate's public API.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from [`crate::ModelManager::verify_installed`]: everything that
/// can stop an installed model's files from being safe to hand to a native
/// speech engine.
#[derive(Debug, Error)]
pub enum IntegrityError {
    /// `id` does not match any entry in the model catalog.
    #[error("unknown model id: {0}")]
    UnknownModel(String),

    /// The model is not installed at all (never downloaded, removed, or
    /// only partially present).
    #[error("model \"{0}\" is not installed")]
    NotInstalled(String),

    /// An installed artifact's size on disk does not match the size
    /// recorded in the catalog — most likely an interrupted download that
    /// left a file of the right name and the wrong length.
    #[error(
        "model \"{model}\" artifact \"{artifact}\" at {path} is {actual} bytes, expected {expected}"
    )]
    SizeMismatch {
        /// The catalog id of the model being verified.
        model: String,
        /// The file name of the offending artifact.
        artifact: String,
        /// The on-disk path that was checked.
        path: PathBuf,
        /// The size, in bytes, recorded in the catalog for this artifact.
        expected: u64,
        /// The size, in bytes, actually found on disk.
        actual: u64,
    },

    /// An installed artifact's size could not be read from disk (e.g. a
    /// permissions error), despite [`crate::ModelManager::path_for`]-style
    /// presence checks having just confirmed the file exists.
    #[error("failed to read metadata for model \"{model}\" artifact \"{artifact}\" at {path}")]
    Io {
        /// The catalog id of the model being verified.
        model: String,
        /// The file name of the offending artifact.
        artifact: String,
        /// The on-disk path that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Errors from [`crate::migrate::migrate_v1`].
#[derive(Debug, Error)]
pub enum MigrateError {
    /// `raw` is not valid TOML, so neither the presence check nor the
    /// migration itself could read it.
    #[error("not valid toml: {0}")]
    Parse(#[from] toml::de::Error),

    /// `raw` already has a `[[profiles]]` table, so it is a v0.2 document
    /// (or an unusual v0.1 one) and does not need migrating.
    #[error("document already has a [[profiles]] table; it does not need migrating")]
    AlreadyMigrated,
}

/// Marks a [`crate::settings::load`] failure that happened while migrating a
/// v0.1 config into v0.2's schema, as opposed to an unrelated I/O or parse
/// failure. A caller that wants to degrade to `Settings::default()` rather
/// than abort startup can recognize this case with
/// [`anyhow::Error::downcast_ref`], since `load` reports it through
/// `anyhow::Context::context`.
///
/// The config file at `path` was left untouched. `backup` is `Some` only
/// when `fs::copy` reported success for the pre-migration copy of `path` —
/// meaning every byte was actually written — and `None` when the backup
/// step is what failed (or never ran). A caller building a user-facing
/// message from this must not name a backup unless `backup` is `Some`: a
/// `Some` value that turned out to be wrong would mean a truncated or
/// missing file being presented as a safety net that isn't there.
#[derive(Debug, Error)]
#[error("failed to migrate {path}; the original file was left untouched")]
pub struct MigrationFailed {
    /// The config file that could not be migrated.
    pub path: PathBuf,
    /// Where a pre-migration backup of `path` was written, if the backup
    /// step itself succeeded.
    pub backup: Option<PathBuf>,
}
