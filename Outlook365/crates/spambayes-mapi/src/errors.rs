//! Error types for MAPI message store operations.
//!
//! Provides a structured error hierarchy covering the failure modes
//! encountered when interacting with MAPI message stores, folders,
//! and messages through the Outlook Object Model or raw MAPI.

use thiserror::Error;

// ─── MsgStoreError ───────────────────────────────────────────────────────────

/// Errors that can occur during message store operations.
///
/// Each variant maps to a specific MAPI failure scenario that the
/// Python `msgstore.py` handled via exceptions.
///
/// # Variants
///
/// - [`NotFound`](MsgStoreError::NotFound) — The requested MAPI object
///   (folder, message, attachment) has been deleted or moved.
///   (Requirement 2.9)
///
/// - [`ReadOnly`](MsgStoreError::ReadOnly) — The message or store does not
///   permit write operations (e.g., public folders, archive stores).
///   (Requirement 2.10)
///
/// - [`ProviderUnavailable`](MsgStoreError::ProviderUnavailable) — The
///   underlying MAPI provider (Exchange, PST, OST) is not responding.
///   (Requirement 2.11)
///
/// - [`ObjectChanged`](MsgStoreError::ObjectChanged) — The object was
///   modified externally between read and write, requiring a retry.
///   (Requirement 2.12)
///
/// - [`Mapi`](MsgStoreError::Mapi) — A raw MAPI/COM HRESULT error that
///   doesn't map to a more specific variant.
#[derive(Debug, Error)]
pub enum MsgStoreError {
    /// The requested object (folder, message, or attachment) was not found.
    ///
    /// This occurs when a MAPI object has been deleted or moved since the
    /// last reference was obtained.
    #[error("Object not found: {0}")]
    NotFound(String),

    /// The store or message is read-only and cannot be modified.
    ///
    /// Returned when attempting to write properties or save changes to
    /// objects in read-only stores (public folders, shared mailboxes
    /// without write permission, etc.).
    #[error("Read-only store: {0}")]
    ReadOnly(String),

    /// The MAPI provider (Exchange, PST, OST) is unavailable.
    ///
    /// Indicates a transport-level failure — the provider process crashed,
    /// the network connection to Exchange was lost, or the PST file is
    /// locked by another process.
    #[error("Provider unavailable: {0}")]
    ProviderUnavailable(String),

    /// The object was modified externally between read and save.
    ///
    /// When this error is encountered, the caller should re-read the
    /// object and retry the save operation (up to 3 attempts per
    /// Requirement 2.12).
    #[error("Object changed externally")]
    ObjectChanged,

    /// A raw MAPI/COM error that does not map to a more specific variant.
    ///
    /// Contains the HRESULT error code and a human-readable description.
    #[error("MAPI error 0x{hr:08X}: {message}")]
    Mapi {
        /// The HRESULT error code returned by the MAPI call.
        hr: i32,
        /// Human-readable description of the error.
        message: String,
    },
}
