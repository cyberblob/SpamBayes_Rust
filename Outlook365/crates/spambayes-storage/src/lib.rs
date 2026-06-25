#![warn(clippy::pedantic)]
// ── Pedantic allow-list (documented exceptions) ──────────────────────────────
// module_name_repetitions: Storage type names use crate prefix for clarity.
#![allow(clippy::module_name_repetitions)]
// cast_possible_truncation: Binary format I/O uses controlled u64→usize and
// usize→u32 casts; values are bounded by file/record sizes.
#![allow(clippy::cast_possible_truncation)]
// cast_sign_loss: Pickle format uses signed integers for inherently non-negative
// counts; sign is validated before cast.
#![allow(clippy::cast_sign_loss)]
// missing_errors_doc: Storage trait methods where error conditions are
// self-evident from the trait context.
#![allow(clippy::missing_errors_doc)]

//! `SpamBayes` Storage - Database persistence layer.
//!
//! Provides the [`StorageBackend`] trait and implementations for
//! persisting classifier token data (dbm-compatible, pickle import).
//!
//! # Overview
//!
//! This crate defines the storage abstractions used by the `SpamBayes`
//! classifier and message tracking system:
//!
//! - [`StorageBackend`] — persistence for classifier state and token data
//! - [`MessageDatabase`] — per-message classification metadata storage
//! - [`StorageError`] — error types for storage operations
//!
//! Concrete implementations (memory-mapped dbm, in-memory, etc.) will be
//! provided in separate modules.

pub mod dbm;
pub mod message_db;
pub mod migration;
pub mod pickle;
pub mod safe_write;
pub mod traits;

// Re-export core storage types for convenient access.
pub use dbm::MmapDbmBackend;
pub use message_db::MmapMessageDb;
pub use migration::{migrate_databases, try_migrate_classifier, MigrationResult};
pub use pickle::PickleImporter;
pub use traits::{
    ClassifierState, MessageDatabase, MessageInfo, StorageBackend, WordChange,
};

// ─── StorageError ────────────────────────────────────────────────────────────

/// Errors that can occur during storage operations.
///
/// Covers database corruption, I/O failures, and deserialization issues.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The database file is corrupted or contains invalid data.
    #[error("corrupted database: {0}")]
    Corrupted(String),

    /// An underlying I/O error occurred (file not found, permission denied, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to deserialize stored data (incompatible format, truncated record, etc.).
    #[error("deserialization error: {0}")]
    Deserialize(String),
}
