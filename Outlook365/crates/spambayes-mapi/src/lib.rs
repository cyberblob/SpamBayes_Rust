#![warn(clippy::pedantic)]
// ── Pedantic allow-list (documented exceptions) ──────────────────────────────
// cast_possible_wrap: MAPI property tags and HRESULT values are defined as u32
// bit patterns intentionally cast to i32 — standard Windows MAPI practice.
#![allow(clippy::cast_possible_wrap)]
// cast_sign_loss: MAPI property counts (i32→u32) are always non-negative.
#![allow(clippy::cast_sign_loss)]
// cast_possible_truncation: MAPI buffer sizes use controlled pointer-width casts.
#![allow(clippy::cast_possible_truncation)]
// cast_ptr_alignment: MAPI row pointers require pointer casting to read fixed
// structs from byte buffers — alignment is guaranteed by the MAPI allocator.
#![allow(clippy::cast_ptr_alignment)]
// missing_errors_doc: MAPI trait methods where errors are self-evident.
#![allow(clippy::missing_errors_doc)]
// unreadable_literal: MAPI property tag hex constants (e.g., 0x0E080003) follow
// standard MAPI documentation format without separators for cross-referencing.
#![allow(clippy::unreadable_literal)]
// similar_names: MAPI variables like `store_id`/`store_eid` are domain terms.
#![allow(clippy::similar_names)]
// items_after_statements: MAPI FFI code declares structs inline near their use
// for locality with the unsafe code that accesses them.
#![allow(clippy::items_after_statements)]
// struct_field_names: Field names like `store_id` in MessageStore are clear.
#![allow(clippy::struct_field_names)]
// Suppress dead-code warnings for fields that will be used once method
// implementations are added in task 9.
#![allow(dead_code)]

//! `SpamBayes` MAPI - MAPI abstraction layer.
//!
//! Provides safe abstractions over the raw MAPI/COM APIs for
//! session management, message store operations, folder access,
//! and message property manipulation.
//!
//! # Overview
//!
//! This crate defines the MAPI abstraction types used by the `SpamBayes`
//! Outlook add-in:
//!
//! - [`MsgStoreError`] — structured error hierarchy for MAPI operations
//! - [`Folder`] — a MAPI folder with identity and metadata
//! - [`Message`] — a MAPI message with lazy-loaded property fields
//! - [`MapiSession`] — MAPI session management (logon, store enumeration)
//! - [`MessageStore`] — operations on a single message store
//!
//! # Architecture
//!
//! The types defined here are signatures only. Method implementations
//! that interact with the Windows MAPI/COM APIs will be added in a
//! subsequent task once the COM interop layer is established.

pub mod errors;
#[cfg(target_os = "windows")]
pub mod folder;
#[cfg(target_os = "windows")]
pub mod message;
#[cfg(target_os = "windows")]
pub mod session;
#[cfg(target_os = "windows")]
pub mod store;

// Re-export the error type for convenient access.
pub use errors::MsgStoreError;
#[cfg(target_os = "windows")]
pub use message::FieldValue;
#[cfg(target_os = "windows")]
pub use session::{MapiSession as MapiSessionImpl, StoreInfo};
#[cfg(target_os = "windows")]
pub use store::MessageStoreOps;
#[cfg(target_os = "windows")]
pub use store::MessageIterator;

use std::ffi::c_void;

// ─── Folder ──────────────────────────────────────────────────────────────────

/// Represents a MAPI folder within a message store.
///
/// A folder is identified by the combination of its `store_id` (which
/// store it belongs to) and `entry_id` (unique within that store).
/// These are raw MAPI binary entry IDs — not the hex-encoded strings
/// used in configuration files.
///
/// # Example
///
/// ```
/// use spambayes_mapi::Folder;
///
/// let folder = Folder {
///     store_id: vec![0x01, 0x02, 0x03],
///     entry_id: vec![0xAA, 0xBB, 0xCC],
///     name: String::from("Inbox"),
///     count: 42,
/// };
/// assert_eq!(folder.name, "Inbox");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Raw binary store entry ID identifying the parent message store.
    pub store_id: Vec<u8>,
    /// Raw binary entry ID uniquely identifying this folder within its store.
    pub entry_id: Vec<u8>,
    /// Display name of the folder (e.g., "Inbox", "Junk Email").
    pub name: String,
    /// Number of messages currently in this folder.
    pub count: u32,
}

// ─── Message ─────────────────────────────────────────────────────────────────

/// Represents a MAPI message with lazy-loaded property fields.
///
/// Property values (subject, body, headers) are loaded on first access
/// rather than eagerly when the message is opened. This avoids expensive
/// MAPI property reads for messages that may only need their entry ID
/// checked (e.g., during folder enumeration).
///
/// The `dirty` flag tracks whether any properties have been modified
/// and need to be saved back to the store.
///
/// # Safety
///
/// The `mapi_message` pointer is an opaque handle to the underlying
/// MAPI message object. It will be replaced with a properly typed COM
/// interface pointer once the COM interop layer is established.
#[derive(Debug)]
pub struct Message {
    /// Raw binary store entry ID identifying the parent message store.
    store_id: Vec<u8>,
    /// Raw binary entry ID uniquely identifying this message.
    entry_id: Vec<u8>,
    /// Opaque pointer to the underlying MAPI message COM object.
    ///
    /// Will be replaced with a typed COM interface pointer in a future task.
    mapi_message: *mut c_void,
    /// Opaque pointer to the parent `IMsgStore` COM object.
    ///
    /// Used for operations that require store-level access (move, copy,
    /// re-open on `ObjectChanged`). NOT owned by Message — the session
    /// keeps it alive.
    store_ptr: *mut c_void,
    /// Whether any properties have been modified since last save.
    dirty: bool,

    // ── Cached / lazy-loaded properties ──────────────────────────────────

    /// The message subject line (lazy-loaded on first access).
    subject: Option<String>,
    /// Sender display name (lazy-loaded on first access).
    sender_cache: Option<String>,
    /// Plain-text body content (lazy-loaded on first access).
    body_plain: Option<String>,
    /// HTML body content (lazy-loaded on first access).
    body_html: Option<String>,
    /// Raw RFC 2822 headers (lazy-loaded on first access).
    headers: Option<String>,
}

// SAFETY: Message will only be accessed from the COM apartment thread
// that created it. The raw pointer is not shared across threads.
// This unsafe impl will be revisited when proper COM pointers are used.
unsafe impl Send for Message {}

impl Message {
    /// Creates a new `Message` with the given identity and MAPI handle.
    ///
    /// All property fields start as `None` (not yet loaded).
    ///
    /// # Safety
    ///
    /// The caller must ensure that `mapi_message` is a valid pointer to
    /// a MAPI message object that remains valid for the lifetime of this
    /// `Message` instance. The `store_ptr` must be a valid `IMsgStore`
    /// pointer that outlives this `Message`.
    pub unsafe fn new(
        store_id: Vec<u8>,
        entry_id: Vec<u8>,
        mapi_message: *mut c_void,
        store_ptr: *mut c_void,
    ) -> Self {
        Self {
            store_id,
            entry_id,
            mapi_message,
            store_ptr,
            dirty: false,
            subject: None,
            sender_cache: None,
            body_plain: None,
            body_html: None,
            headers: None,
        }
    }

    /// Returns the store entry ID for this message's parent store.
    #[must_use]
    pub fn store_id(&self) -> &[u8] {
        &self.store_id
    }

    /// Returns the entry ID uniquely identifying this message.
    #[must_use]
    pub fn entry_id(&self) -> &[u8] {
        &self.entry_id
    }

    /// Returns whether any properties have been modified since last save.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

// ─── MapiSession ─────────────────────────────────────────────────────────────

/// Manages a MAPI session (logon, profile, store enumeration).
///
/// `MapiSession` wraps the MAPI session handle and provides methods for
/// logging on, enumerating available message stores, and opening
/// individual stores.
///
/// # Note
///
/// This is a signature-only definition. The actual MAPI logon and store
/// enumeration logic will be implemented once the COM interop layer is
/// established (task 9).
#[derive(Debug)]
pub struct MapiSession {
    /// Opaque session handle (will be a typed MAPI session pointer).
    session_handle: *mut c_void,
    /// Entry ID of the default message store for this profile.
    default_store_id: Option<Vec<u8>>,
}

// SAFETY: MapiSession is used only from the COM apartment thread.
unsafe impl Send for MapiSession {}

impl MapiSession {
    /// Creates a new uninitialized session.
    ///
    /// Call a logon method to establish the actual MAPI session before
    /// using any store operations.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session_handle: std::ptr::null_mut(),
            default_store_id: None,
        }
    }

    /// Returns the entry ID of the default message store, if known.
    #[must_use]
    pub fn default_store_id(&self) -> Option<&[u8]> {
        self.default_store_id.as_deref()
    }
}

impl Default for MapiSession {
    fn default() -> Self {
        Self::new()
    }
}

// ─── MessageStore ────────────────────────────────────────────────────────────

/// Provides operations on a single MAPI message store.
///
/// A `MessageStore` is obtained from a [`MapiSession`] and represents
/// one mailbox (Exchange, PST, OST, etc.). It provides access to the
/// store's folder hierarchy and individual messages.
///
/// # Note
///
/// This is a signature-only definition. The actual folder/message
/// operations will be implemented once the COM interop layer is
/// established (task 9).
#[derive(Debug)]
pub struct MessageStore {
    /// Reference back to the owning session (raw pointer for now).
    session: *mut c_void,
    /// Entry ID of this message store.
    store_id: Vec<u8>,
    /// Display name of this message store.
    store_name: String,
}

// SAFETY: MessageStore is used only from the COM apartment thread.
unsafe impl Send for MessageStore {}

impl MessageStore {
    /// Returns the binary entry ID of this message store.
    #[must_use]
    pub fn store_id(&self) -> &[u8] {
        &self.store_id
    }

    /// Returns the display name of this message store.
    #[must_use]
    pub fn store_name(&self) -> &str {
        &self.store_name
    }
}
