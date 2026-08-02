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
