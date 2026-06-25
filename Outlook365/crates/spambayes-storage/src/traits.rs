//! Storage trait definitions for the `SpamBayes` persistence layer.
//!
//! Defines the core abstractions for classifier state persistence
//! and message metadata storage.

use std::collections::HashMap;

use spambayes_config::FolderId;
use spambayes_core::{Classification, WordInfo};

use crate::StorageError;

// ─── ClassifierState ─────────────────────────────────────────────────────────

/// In-memory classifier state that the Classifier operates on.
///
/// This represents the global counters and version metadata that must
/// be persisted alongside the per-token data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifierState {
    /// Total number of spam messages trained.
    pub nspam: u64,
    /// Total number of ham (legitimate) messages trained.
    pub nham: u64,
    /// Schema version for forward-compatible database upgrades.
    pub version: u32,
}

impl Default for ClassifierState {
    fn default() -> Self {
        Self {
            nspam: 0,
            nham: 0,
            version: 1,
        }
    }
}

// ─── MessageInfo ─────────────────────────────────────────────────────────────

/// Metadata stored per classified message.
///
/// Tracks whether a message has been trained, its classification result,
/// the spam probability score, and its unique identifier.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageInfo {
    /// Training disposition: `None` = untrained, `Some(true)` = trained as spam,
    /// `Some(false)` = trained as ham.
    pub trained_as: Option<bool>,
    /// The classification assigned by the classifier, if scored.
    pub classification: Option<Classification>,
    /// The spam probability score in `0.0..=1.0`, if scored.
    pub score: Option<f64>,
    /// Unique message identifier (e.g., MAPI Entry ID or Message-ID header).
    pub message_id: String,
    /// The folder the message was in when originally filtered by `SpamBayes`.
    /// Used for incremental training to detect recovery (drag back to original folder).
    pub original_folder: Option<FolderId>,
}

// ─── WordChange ──────────────────────────────────────────────────────────────

/// Represents a change to a single token record for incremental persistence.
///
/// Used by [`StorageBackend::store`] to write only modified tokens
/// rather than the full token database on every save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordChange {
    /// The token was added or its counts were updated.
    Updated(WordInfo),
    /// The token was removed from the classifier.
    Removed,
}

// ─── StorageBackend Trait ────────────────────────────────────────────────────

/// Trait defining the classifier persistence interface.
///
/// Implementations handle loading and saving the classifier's global state
/// (spam/ham counts) and per-token data to a persistent store (e.g., dbm,
/// memory-mapped files, or `SQLite`).
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent access
/// from the filter and training engines.
pub trait StorageBackend: Send + Sync {
    /// Load classifier state from persistent storage.
    ///
    /// Returns the stored [`ClassifierState`] or an error if the database
    /// is corrupted or unavailable.
    fn load(&mut self) -> Result<ClassifierState, StorageError>;

    /// Save only changed token records (incremental).
    ///
    /// Persists the updated [`ClassifierState`] along with any token records
    /// that have been modified since the last save. The `changed` map contains
    /// token keys mapped to their [`WordChange`] (updated counts or removal).
    fn store(
        &mut self,
        state: &ClassifierState,
        changed: &HashMap<Vec<u8>, WordChange>,
    ) -> Result<(), StorageError>;

    /// Close and release all resources (file handles, memory maps, etc.).
    ///
    /// After calling `close`, subsequent calls to `load` or `store` may
    /// return an error or reinitialize the backend.
    fn close(&mut self) -> Result<(), StorageError>;
}

// ─── MessageDatabase Trait ───────────────────────────────────────────────────

/// Trait for the message metadata database.
///
/// Stores per-message classification metadata so the add-in can track
/// which messages have been trained, their scores, and classifications
/// without re-scoring on every access.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` for concurrent access from
/// filter and training operations.
pub trait MessageDatabase: Send + Sync {
    /// Look up message metadata by search key.
    ///
    /// Returns `None` if no record exists for the given key.
    fn load_msg(&self, search_key: &[u8]) -> Option<MessageInfo>;

    /// Store or update message metadata for the given search key.
    fn store_msg(&mut self, search_key: &[u8], info: &MessageInfo);

    /// Remove message metadata for the given search key.
    ///
    /// This is a no-op if no record exists for the key.
    fn remove_msg(&mut self, search_key: &[u8]);
}
