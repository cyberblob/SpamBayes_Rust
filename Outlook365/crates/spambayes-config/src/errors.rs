//! Configuration error hierarchy.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during configuration operations.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file could not be found at the expected path.
    #[error("configuration file not found: {0}")]
    FileNotFound(PathBuf),

    /// An I/O error occurred while reading or writing the configuration file.
    #[error("I/O error on config file {path}: {source}")]
    IoError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The INI file content could not be parsed (malformed syntax).
    #[error("parse error in {path} at line {line}: {message}")]
    ParseError {
        path: PathBuf,
        line: usize,
        message: String,
    },

    /// A configuration value could not be converted to the expected type.
    #[error("invalid value for [{section}] {key}: expected {expected}, got {actual:?}")]
    InvalidValue {
        section: String,
        key: String,
        expected: String,
        actual: String,
    },

    /// A required section is missing from the configuration file.
    #[error("missing section: [{0}]")]
    MissingSection(String),

    /// A required key is missing from a configuration section.
    #[error("missing key [{section}] {key}")]
    MissingKey { section: String, key: String },

    /// The folder ID string could not be parsed from the INI tuple format.
    #[error("invalid folder ID format: {0:?}")]
    InvalidFolderId(String),
}
