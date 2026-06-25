//! Message store operations.
//!
//! Provides the `MessageStore` implementation with folder enumeration,
//! folder access, folder creation, and recursive sub-folder iteration.
//!
//! # Architecture
//!
//! The `MessageStore` wraps a raw `IMsgStore` COM pointer (obtained from
//! `MapiSession::open_store()`) and provides high-level operations for
//! navigating the folder hierarchy and accessing messages.
//!
//! All MAPI error codes are mapped to structured `MsgStoreError` variants
//! using the `map_mapi_error` function from the `folder` module.
//!
//! # Requirements
//!
//! - Req 2.1: Folder enumeration with recursive sub-folder iteration
//! - Req 2.3: Open messages by Entry ID and Store ID pairs
//! - Req 2.8: Create new folders under existing folders
//! - Req 2.9: `NotFound` for deleted/moved MAPI objects
//! - Req 2.10: `ReadOnly` for read-only stores
//! - Req 2.11: `ProviderUnavailable` for disconnected providers

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::ptr;

use crate::errors::MsgStoreError;
use crate::folder::{IMsgStoreExtObj, PR_ENTRYID, PR_IPM_SUBTREE_ENTRYID, SPropValue, SPropTagArray, S_OK, map_mapi_error, MAPIFreeBuffer, read_binary_prop, MAPI_MODIFY, IMAPIFolderObj, FOLDER_GENERIC, OPEN_IF_EXISTS, release_com, MAPI_DEFERRED_ERRORS, MAPI_BEST_ACCESS, IMAPITableObj, PR_DISPLAY_NAME_W, read_display_name, PR_STORE_ENTRYID, SRowSet, SRow, PT_UNICODE, read_unicode_prop, PT_STRING8, read_string8_prop};
use crate::Folder;

// ─── MessageStore Implementation ─────────────────────────────────────────────

/// Provides operations on a single MAPI message store (mailbox).
///
/// A `MessageStoreOps` wraps a raw `IMsgStore` COM pointer and its store
/// entry ID. It exposes folder navigation, folder creation, and message
/// access using the MAPI C API via FFI.
///
/// # Safety
///
/// This struct holds a raw COM pointer. It must only be used from the
/// COM apartment thread that created it. The pointer is NOT owned —
/// it is owned by the `MapiSession` that opened the store.
///
/// # Example
///
/// ```no_run
/// use spambayes_mapi::store::MessageStoreOps;
///
/// // Obtain store_ptr from MapiSession::open_store()
/// // let ops = unsafe { MessageStoreOps::new(store_ptr, store_eid) };
/// // let root = ops.get_root_folder()?;
/// ```
pub struct MessageStoreOps {
    /// Raw `IMsgStore` COM pointer (not owned; owned by `MapiSession`).
    store_ptr: *mut c_void,
    /// Binary entry ID of this message store.
    store_id: Vec<u8>,
}

// SAFETY: MessageStoreOps is used only from the COM apartment thread.
unsafe impl Send for MessageStoreOps {}

impl std::fmt::Debug for MessageStoreOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageStoreOps")
            .field("store_ptr_valid", &(!self.store_ptr.is_null()))
            .field("store_id_len", &self.store_id.len())
            .finish()
    }
}

impl MessageStoreOps {
    /// Create a new `MessageStoreOps` wrapping a raw `IMsgStore` pointer.
    ///
    /// # Safety
    ///
    /// - `store_ptr` must be a valid `IMsgStore` COM pointer that remains
    ///   valid for the lifetime of this `MessageStoreOps`.
    /// - The pointer is NOT released on drop (it is owned by `MapiSession`).
    pub unsafe fn new(store_ptr: *mut c_void, store_id: Vec<u8>) -> Self {
        Self { store_ptr, store_id }
    }

    /// Returns the binary store entry ID.
    #[must_use]
    pub fn store_id(&self) -> &[u8] {
        &self.store_id
    }

    /// Get the root folder (IPM subtree) of this message store.
    ///
    /// Reads `PR_IPM_SUBTREE_ENTRYID` from the store, then opens that
    /// folder and returns it with its display name and message count.
    ///
    /// # Errors
    ///
    /// - `MsgStoreError::NotFound` if the store has been deleted
    /// - `MsgStoreError::ProviderUnavailable` if the store is disconnected
    /// - `MsgStoreError::Mapi` for other MAPI failures
    ///
    /// # Requirement
    ///
    /// - Req 2.1: Folder enumeration (root folder access)
    pub fn get_root_folder(&self) -> Result<Folder, MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null".to_string(),
            });
        }

        unsafe {
            // Get PR_IPM_SUBTREE_ENTRYID from the store
            let store_obj = self.store_ptr.cast::<IMsgStoreExtObj>();
            let vtbl = &*(*store_obj).vtbl;

            #[repr(C)]
            struct PropTagArray2 {
                c_values: u32,
                tags: [u32; 2],
            }
            let columns = PropTagArray2 {
                c_values: 2,
                tags: [PR_ENTRYID, PR_IPM_SUBTREE_ENTRYID],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.store_ptr,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK && (hr & 0x80000000_u32 as i32) != 0 {
                return Err(map_mapi_error(hr, "IMsgStore::GetProps(IPM_SUBTREE)"));
            }

            if props_ptr.is_null() || count < 2 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return Err(MsgStoreError::Mapi {
                    hr: -1,
                    message: "Failed to read IPM subtree entry ID".to_string(),
                });
            }

            // Read the store entry ID (column 0) and subtree entry ID (column 1)
            let store_eid = read_binary_prop(props_ptr.add(0));
            let subtree_eid = read_binary_prop(props_ptr.add(1));
            MAPIFreeBuffer(props_ptr.cast::<c_void>());

            if subtree_eid.is_empty() {
                return Err(MsgStoreError::Mapi {
                    hr: -1,
                    message: "IPM subtree entry ID is empty".to_string(),
                });
            }

            // Use the store EID from the property if available, otherwise
            // fall back to the one we were constructed with.
            let effective_store_id = if store_eid.is_empty() {
                self.store_id.clone()
            } else {
                store_eid
            };

            // Open the subtree folder
            self.open_folder_by_eid(&subtree_eid, &effective_store_id)
        }
    }

    /// Open a folder by its binary entry ID.
    ///
    /// Returns a `Folder` with the folder's display name and message count.
    ///
    /// # Arguments
    ///
    /// * `folder_eid` - Binary entry ID of the folder to open
    /// * `store_id` - Binary store entry ID (used in the returned Folder)
    ///
    /// # Errors
    ///
    /// - `MsgStoreError::NotFound` if the folder has been deleted (Req 2.9)
    /// - `MsgStoreError::ReadOnly` if access is denied (Req 2.10)
    /// - `MsgStoreError::ProviderUnavailable` if disconnected (Req 2.11)
    pub fn get_folder(
        &self,
        folder_eid: &[u8],
        store_id: &[u8],
    ) -> Result<Folder, MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null".to_string(),
            });
        }

        unsafe { self.open_folder_by_eid(folder_eid, store_id) }
    }

    /// Create a new sub-folder under an existing parent folder.
    ///
    /// If a folder with the given name already exists, it will be opened
    /// (using `OPEN_IF_EXISTS` flag) rather than failing.
    ///
    /// # Arguments
    ///
    /// * `parent_eid` - Binary entry ID of the parent folder
    /// * `name` - Display name for the new folder
    ///
    /// # Errors
    ///
    /// - `MsgStoreError::NotFound` if the parent folder doesn't exist (Req 2.9)
    /// - `MsgStoreError::ReadOnly` if the store is read-only (Req 2.10)
    /// - `MsgStoreError::ProviderUnavailable` if disconnected (Req 2.11)
    ///
    /// # Requirement
    ///
    /// - Req 2.8: Create new folders under existing folders
    pub fn create_folder(
        &self,
        parent_eid: &[u8],
        name: &str,
    ) -> Result<Folder, MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null".to_string(),
            });
        }

        unsafe {
            // Open the parent folder
            let parent_ptr = self.open_entry_raw(parent_eid, MAPI_MODIFY)?;

            let folder_obj = parent_ptr.cast::<IMAPIFolderObj>();
            let vtbl = &*(*folder_obj).vtbl;

            // Encode name as null-terminated UTF-16
            let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

            let mut new_folder_ptr: *mut c_void = ptr::null_mut();
            let hr = (vtbl.create_folder)(
                parent_ptr,
                FOLDER_GENERIC,
                name_wide.as_ptr(),
                ptr::null(),  // no comment
                ptr::null(),  // default IID
                OPEN_IF_EXISTS | MAPI_UNICODE_FLAG,
                &raw mut new_folder_ptr,
            );

            if hr != S_OK || new_folder_ptr.is_null() {
                release_com(parent_ptr);
                return Err(map_mapi_error(hr, "IMAPIFolder::CreateFolder"));
            }

            // Read the new folder's properties
            let result = self.read_folder_props(new_folder_ptr);

            release_com(new_folder_ptr);
            release_com(parent_ptr);

            result
        }
    }

    /// Iterate over a set of folders, optionally including sub-folders.
    ///
    /// This is the Rust equivalent of Python's `GetFolderGenerator`. It
    /// yields each folder identified by `folder_eids`, and if
    /// `include_sub` is true, recursively yields all sub-folders.
    ///
    /// # Arguments
    ///
    /// * `folder_eids` - List of (`store_id`, `entry_id`) pairs to iterate
    /// * `include_sub` - Whether to recurse into sub-folders
    ///
    /// # Returns
    ///
    /// A `Vec` of results. Each entry is either a successfully opened
    /// `Folder` or an error. Errors for individual folders do not stop
    /// iteration — the caller can skip unavailable folders.
    ///
    /// # Requirement
    ///
    /// - Req 2.1: Folder enumeration with recursive sub-folder iteration
    #[must_use]
    pub fn folder_iter(
        &self,
        folder_eids: &[(&[u8], &[u8])],
        include_sub: bool,
    ) -> Vec<Result<Folder, MsgStoreError>> {
        let mut results = Vec::new();

        for &(store_id, entry_id) in folder_eids {
            match self.get_folder(entry_id, store_id) {
                Ok(folder) => {
                    results.push(Ok(folder.clone()));
                    if include_sub {
                        self.collect_sub_folders(entry_id, store_id, &mut results);
                    }
                }
                Err(e) => {
                    // Individual folder errors don't stop iteration.
                    // ProviderUnavailable and NotFound are expected for
                    // disconnected stores or deleted folders.
                    results.push(Err(e));
                }
            }
        }

        results
    }

    /// Open a message by its entry ID.
    ///
    /// Returns a raw COM pointer to the `IMessage` object. The caller is
    /// responsible for using and releasing this pointer.
    ///
    /// # Arguments
    ///
    /// * `message_eid` - Binary entry ID of the message
    ///
    /// # Errors
    ///
    /// - `MsgStoreError::NotFound` if the message has been deleted (Req 2.9)
    /// - `MsgStoreError::ReadOnly` if no modify access (Req 2.10)
    /// - `MsgStoreError::ProviderUnavailable` if disconnected (Req 2.11)
    ///
    /// # Requirement
    ///
    /// - Req 2.3: Open messages by Entry ID and Store ID pairs
    ///
    /// # Safety
    ///
    /// The returned pointer must be released by the caller when done.
    pub fn open_message(&self, message_eid: &[u8]) -> Result<*mut c_void, MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null".to_string(),
            });
        }

        unsafe {
            self.open_entry_raw(message_eid, MAPI_MODIFY | MAPI_DEFERRED_ERRORS)
        }
    }

    // ─── Internal Helper Methods ─────────────────────────────────────────

    /// Open a MAPI entry (folder or message) by its entry ID.
    ///
    /// # Safety
    ///
    /// The returned pointer must be released by the caller.
    unsafe fn open_entry_raw(
        &self,
        entry_id: &[u8],
        flags: u32,
    ) -> Result<*mut c_void, MsgStoreError> {
        let store_obj = self.store_ptr.cast::<IMsgStoreExtObj>();
        let vtbl = &*(*store_obj).vtbl;

        let mut obj_type: u32 = 0;
        let mut obj_ptr: *mut c_void = ptr::null_mut();

        let hr = (vtbl.open_entry)(
            self.store_ptr,
            entry_id.len() as u32,
            entry_id.as_ptr(),
            ptr::null(),  // default IID
            flags,
            &raw mut obj_type,
            &raw mut obj_ptr,
        );

        if hr != S_OK || obj_ptr.is_null() {
            return Err(map_mapi_error(hr, "IMsgStore::OpenEntry"));
        }

        Ok(obj_ptr)
    }

    /// Open a folder by entry ID and read its properties into a `Folder`.
    ///
    /// # Safety
    ///
    /// Must be called from the COM apartment thread.
    unsafe fn open_folder_by_eid(
        &self,
        folder_eid: &[u8],
        store_id: &[u8],
    ) -> Result<Folder, MsgStoreError> {
        let folder_ptr = self.open_entry_raw(
            folder_eid,
            MAPI_BEST_ACCESS | MAPI_DEFERRED_ERRORS,
        )?;

        // Read folder properties (name, count, entry ID)
        let folder_obj = folder_ptr.cast::<IMAPIFolderObj>();
        let vtbl = &*(*folder_obj).vtbl;

        // Get the contents table to read the message count
        let mut table_ptr: *mut c_void = ptr::null_mut();
        let hr = (vtbl.get_contents_table)(folder_ptr, MAPI_DEFERRED_ERRORS, &raw mut table_ptr);

        let count = if hr == S_OK && !table_ptr.is_null() {
            let table_obj = table_ptr.cast::<IMAPITableObj>();
            let table_vtbl = &*(*table_obj).vtbl;
            let mut row_count: u32 = 0;
            let hr2 = (table_vtbl.get_row_count)(table_ptr, 0, &raw mut row_count);
            release_com(table_ptr);
            if hr2 == S_OK {
                row_count
            } else {
                0
            }
        } else {
            // Some folders (search folders) may not have a contents table
            if !table_ptr.is_null() {
                release_com(table_ptr);
            }
            0
        };

        // Read folder's display name and entry ID via GetProps
        #[repr(C)]
        struct PropTagArray2 {
            c_values: u32,
            tags: [u32; 2],
        }
        let columns = PropTagArray2 {
            c_values: 2,
            tags: [PR_ENTRYID, PR_DISPLAY_NAME_W],
        };

        let mut prop_count: u32 = 0;
        let mut props_ptr: *mut SPropValue = ptr::null_mut();
        let hr = (vtbl.get_props)(
            folder_ptr,
            (&raw const columns).cast::<SPropTagArray>(),
            0,
            &raw mut prop_count,
            &raw mut props_ptr,
        );

        let folder = if (hr == S_OK || hr == MAPI_W_ERRORS_RETURNED)
            && !props_ptr.is_null()
            && prop_count >= 2
        {
            let actual_eid = read_binary_prop(props_ptr.add(0));
            let name = read_display_name(props_ptr.add(1));
            MAPIFreeBuffer(props_ptr.cast::<c_void>());

            // Use the actual entry ID from the folder if available
            let final_eid = if actual_eid.is_empty() {
                folder_eid.to_vec()
            } else {
                actual_eid
            };

            Ok(Folder {
                store_id: store_id.to_vec(),
                entry_id: final_eid,
                name,
                count,
            })
        } else {
            if !props_ptr.is_null() {
                MAPIFreeBuffer(props_ptr.cast::<c_void>());
            }
            // Fall back to using the provided entry ID with unknown name
            Ok(Folder {
                store_id: store_id.to_vec(),
                entry_id: folder_eid.to_vec(),
                name: String::from("<unknown>"),
                count,
            })
        };

        release_com(folder_ptr);
        folder
    }

    /// Read a folder's properties from an already-opened folder pointer.
    ///
    /// # Safety
    ///
    /// `folder_ptr` must be a valid `IMAPIFolder` COM pointer.
    unsafe fn read_folder_props(
        &self,
        folder_ptr: *mut c_void,
    ) -> Result<Folder, MsgStoreError> {
        let folder_obj = folder_ptr.cast::<IMAPIFolderObj>();
        let vtbl = &*(*folder_obj).vtbl;

        // Get contents table for message count
        let mut table_ptr: *mut c_void = ptr::null_mut();
        let hr = (vtbl.get_contents_table)(folder_ptr, MAPI_DEFERRED_ERRORS, &raw mut table_ptr);

        let count = if hr == S_OK && !table_ptr.is_null() {
            let table_obj = table_ptr.cast::<IMAPITableObj>();
            let table_vtbl = &*(*table_obj).vtbl;
            let mut row_count: u32 = 0;
            let hr2 = (table_vtbl.get_row_count)(table_ptr, 0, &raw mut row_count);
            release_com(table_ptr);
            if hr2 == S_OK { row_count } else { 0 }
        } else {
            if !table_ptr.is_null() {
                release_com(table_ptr);
            }
            0
        };

        // Read folder properties
        #[repr(C)]
        struct PropTagArray3 {
            c_values: u32,
            tags: [u32; 3],
        }
        let columns = PropTagArray3 {
            c_values: 3,
            tags: [PR_ENTRYID, PR_STORE_ENTRYID, PR_DISPLAY_NAME_W],
        };

        let mut prop_count: u32 = 0;
        let mut props_ptr: *mut SPropValue = ptr::null_mut();
        let hr = (vtbl.get_props)(
            folder_ptr,
            (&raw const columns).cast::<SPropTagArray>(),
            0,
            &raw mut prop_count,
            &raw mut props_ptr,
        );

        if (hr != S_OK && hr != MAPI_W_ERRORS_RETURNED) || props_ptr.is_null() {
            return Err(map_mapi_error(hr, "IMAPIFolder::GetProps"));
        }

        let entry_id = read_binary_prop(props_ptr.add(0));
        let store_eid = read_binary_prop(props_ptr.add(1));
        let name = read_display_name(props_ptr.add(2));
        MAPIFreeBuffer(props_ptr.cast::<c_void>());

        let effective_store_id = if store_eid.is_empty() {
            self.store_id.clone()
        } else {
            store_eid
        };

        Ok(Folder {
            store_id: effective_store_id,
            entry_id,
            name,
            count,
        })
    }

    /// Recursively collect sub-folders of a given folder.
    ///
    /// Uses the folder's hierarchy table to enumerate direct children,
    /// then recurses into each child.
    ///
    /// # Safety
    ///
    /// Uses raw MAPI FFI calls internally.
    fn collect_sub_folders(
        &self,
        parent_eid: &[u8],
        store_id: &[u8],
        results: &mut Vec<Result<Folder, MsgStoreError>>,
    ) {
        // Open the parent folder to get its hierarchy table
        let Ok(folder_ptr) = (unsafe {
            self.open_entry_raw(parent_eid, MAPI_BEST_ACCESS | MAPI_DEFERRED_ERRORS)
        }) else {
            return; // Skip if we can't open the parent
        };

        unsafe {
            let folder_obj = folder_ptr.cast::<IMAPIFolderObj>();
            let vtbl = &*(*folder_obj).vtbl;

            // Get the hierarchy table (sub-folders)
            let mut table_ptr: *mut c_void = ptr::null_mut();
            let hr = (vtbl.get_hierarchy_table)(folder_ptr, MAPI_DEFERRED_ERRORS, &raw mut table_ptr);

            if hr != S_OK || table_ptr.is_null() {
                release_com(folder_ptr);
                return;
            }

            let table_obj = table_ptr.cast::<IMAPITableObj>();
            let table_vtbl = &*(*table_obj).vtbl;

            // Set columns to retrieve: entry ID, store entry ID, display name
            #[repr(C)]
            struct PropTagArray3 {
                c_values: u32,
                tags: [u32; 3],
            }
            let columns = PropTagArray3 {
                c_values: 3,
                tags: [PR_ENTRYID, PR_STORE_ENTRYID, PR_DISPLAY_NAME_W],
            };

            let hr = (table_vtbl.set_columns)(
                table_ptr,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
            );
            if hr != S_OK {
                release_com(table_ptr);
                release_com(folder_ptr);
                return;
            }

            // Query all rows (up to 1000 sub-folders)
            let mut row_set: *mut SRowSet = ptr::null_mut();
            let hr = (table_vtbl.query_rows)(table_ptr, 1000, 0, &raw mut row_set);

            if hr != S_OK || row_set.is_null() {
                release_com(table_ptr);
                release_com(folder_ptr);
                return;
            }

            let num_rows = (*row_set).c_rows;

            for i in 0..num_rows {
                let row_ptr = (row_set as *const u8)
                    .add(std::mem::offset_of!(SRowSet, a_row))
                    .add(i as usize * std::mem::size_of::<SRow>()).cast::<SRow>();
                let row = &*row_ptr;

                if row.c_values < 3 || row.lp_props.is_null() {
                    continue;
                }

                let props = row.lp_props;
                let sub_eid = read_binary_prop(props.add(0));
                let sub_store_eid = read_binary_prop(props.add(1));
                let sub_name = read_display_name(props.add(2));

                if sub_eid.is_empty() {
                    continue;
                }

                let effective_store = if sub_store_eid.is_empty() {
                    store_id.to_vec()
                } else {
                    sub_store_eid
                };

                // Open the sub-folder to get its message count
                match self.get_folder(&sub_eid, &effective_store) {
                    Ok(mut folder) => {
                        // Use the name from the hierarchy table if GetProps
                        // returned <unknown>
                        if folder.name == "<unknown>" {
                            folder.name = sub_name;
                        }
                        let sub_entry_id = folder.entry_id.clone();
                        let sub_store_id = folder.store_id.clone();
                        results.push(Ok(folder));
                        // Recurse into this sub-folder
                        self.collect_sub_folders(
                            &sub_entry_id,
                            &sub_store_id,
                            results,
                        );
                    }
                    Err(e) => {
                        results.push(Err(e));
                    }
                }
            }

            MAPIFreeBuffer(row_set.cast::<c_void>());
            release_com(table_ptr);
            release_com(folder_ptr);
        }
    }
}

// ─── MAPI Warning Code ──────────────────────────────────────────────────────

/// `MAPI_W_ERRORS_RETURNED` — some properties could not be returned but
/// the call partially succeeded. We treat this as success and read what
/// we can.
const MAPI_W_ERRORS_RETURNED: i32 = 0x00040380;

/// `MAPI_UNICODE` flag — request Unicode string properties.
const MAPI_UNICODE_FLAG: u32 = 0x80000000;

// ─── Message Class Property Tag ──────────────────────────────────────────────

/// `PR_MESSAGE_CLASS_W` — Unicode message class (e.g., "IPM.Note").
const PR_MESSAGE_CLASS_W: u32 = 0x001A_001F; // PT_UNICODE

/// `PR_MESSAGE_CLASS_A` — ANSI message class (fallback).
#[allow(dead_code)]
const PR_MESSAGE_CLASS_A: u32 = 0x001A_001E; // PT_STRING8

// ─── Message Iterator ────────────────────────────────────────────────────────

/// An entry in the pre-fetched buffer of qualifying message entry IDs.
struct QualifiedEntry {
    entry_id: Vec<u8>,
}

/// Iterator over messages in a folder that yields only received mail items.
///
/// Filters messages by message class, yielding only those whose class
/// starts with "IPM.Note" or "IPM.Anti-Virus" (case-insensitive prefix
/// match). Messages are opened lazily — only when `next()` is called.
///
/// # Generator-style Batching
///
/// Rows are fetched from the MAPI contents table in batches (50 at a
/// time) for efficient memory usage, mimicking Python's generator behavior.
///
/// # Requirement
///
/// - Req 2.2: Message enumeration via generator-style iterator with
///   message class filtering
pub struct MessageIterator {
    /// Raw `IMAPITable` COM pointer (owned; released on Drop).
    table_ptr: *mut c_void,
    /// Raw `IMsgStore` COM pointer (not owned).
    store_ptr: *mut c_void,
    /// Binary store entry ID.
    store_id: Vec<u8>,
    /// Pre-fetched buffer of qualifying entry IDs (those that passed the filter).
    buffer: Vec<QualifiedEntry>,
    /// Current read position within the buffer.
    buffer_pos: usize,
    /// Whether the MAPI table has been exhausted (no more rows to fetch).
    table_exhausted: bool,
}

// SAFETY: MessageIterator is used only from the COM apartment thread.
unsafe impl Send for MessageIterator {}

impl MessageIterator {
    /// Batch size for fetching rows from the contents table.
    const BATCH_SIZE: i32 = 50;

    /// Check if a message class qualifies for iteration.
    ///
    /// Returns `true` if the class starts with "IPM.Note" or "IPM.Anti-Virus"
    /// (case-insensitive prefix match).
    fn is_qualifying_class(message_class: &str) -> bool {
        let upper = message_class.to_uppercase();
        upper.starts_with("IPM.NOTE") || upper.starts_with("IPM.ANTI-VIRUS")
    }

    /// Fetch the next batch of qualifying entry IDs from the MAPI table.
    ///
    /// Queries rows in batches and filters by message class, adding
    /// qualifying entries to the internal buffer.
    ///
    /// # Safety
    ///
    /// The `table_ptr` must be a valid `IMAPITable` COM pointer.
    unsafe fn fetch_next_batch(&mut self) -> Result<(), MsgStoreError> {
        if self.table_exhausted {
            return Ok(());
        }

        let table_obj = self.table_ptr.cast::<IMAPITableObj>();
        let table_vtbl = &*(*table_obj).vtbl;

        let mut row_set: *mut SRowSet = ptr::null_mut();
        let hr = (table_vtbl.query_rows)(
            self.table_ptr,
            Self::BATCH_SIZE,
            0,
            &raw mut row_set,
        );

        if hr != S_OK || row_set.is_null() {
            self.table_exhausted = true;
            if hr != S_OK {
                return Err(map_mapi_error(hr, "IMAPITable::QueryRows (messages)"));
            }
            return Ok(());
        }

        let num_rows = (*row_set).c_rows;
        if num_rows == 0 {
            self.table_exhausted = true;
            MAPIFreeBuffer(row_set.cast::<c_void>());
            return Ok(());
        }

        // If we got fewer rows than requested, the table is exhausted
        if (num_rows as i32) < Self::BATCH_SIZE {
            self.table_exhausted = true;
        }

        for i in 0..num_rows {
            let row_ptr = (row_set as *const u8)
                .add(std::mem::offset_of!(SRowSet, a_row))
                .add(i as usize * std::mem::size_of::<SRow>()).cast::<SRow>();
            let row = &*row_ptr;

            if row.c_values < 2 || row.lp_props.is_null() {
                continue;
            }

            let props = row.lp_props;

            // Column 0: PR_ENTRYID (binary)
            let entry_id = read_binary_prop(props.add(0));
            if entry_id.is_empty() {
                continue;
            }

            // Column 1: PR_MESSAGE_CLASS_W or PR_MESSAGE_CLASS_A
            let msg_class_prop = props.add(1);
            let prop_type = (*msg_class_prop).ul_prop_tag & 0xFFFF;
            let message_class = match prop_type {
                PT_UNICODE => read_unicode_prop(msg_class_prop),
                PT_STRING8 => read_string8_prop(msg_class_prop),
                _ => String::new(),
            };

            // Apply message class filter
            if Self::is_qualifying_class(&message_class) {
                self.buffer.push(QualifiedEntry { entry_id });
            }
        }

        MAPIFreeBuffer(row_set.cast::<c_void>());
        Ok(())
    }
}

impl Iterator for MessageIterator {
    type Item = Result<crate::Message, MsgStoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        // If buffer is exhausted, try to fetch more
        while self.buffer_pos >= self.buffer.len() {
            if self.table_exhausted {
                return None;
            }
            // Reset buffer for next batch
            self.buffer.clear();
            self.buffer_pos = 0;

            // Fetch next batch; propagate errors
            match unsafe { self.fetch_next_batch() } {
                Ok(()) => {}
                Err(e) => return Some(Err(e)),
            }

            // If still no entries after fetch, we're done
            if self.buffer.is_empty() {
                return None;
            }
        }

        // Open the next qualifying message lazily
        let entry = &self.buffer[self.buffer_pos];
        self.buffer_pos += 1;

        // Open the message via IMsgStore::OpenEntry
        let message_eid = &entry.entry_id;
        let result = unsafe {
            let store_obj = self.store_ptr.cast::<IMsgStoreExtObj>();
            let vtbl = &*(*store_obj).vtbl;

            let mut obj_type: u32 = 0;
            let mut obj_ptr: *mut c_void = ptr::null_mut();

            let hr = (vtbl.open_entry)(
                self.store_ptr,
                message_eid.len() as u32,
                message_eid.as_ptr(),
                ptr::null(),
                MAPI_BEST_ACCESS | MAPI_DEFERRED_ERRORS,
                &raw mut obj_type,
                &raw mut obj_ptr,
            );

            if hr != S_OK || obj_ptr.is_null() {
                Err(map_mapi_error(hr, "IMsgStore::OpenEntry (message)"))
            } else {
                Ok(crate::Message::new(
                    self.store_id.clone(),
                    message_eid.clone(),
                    obj_ptr,
                    self.store_ptr,
                ))
            }
        };

        Some(result)
    }
}

impl Drop for MessageIterator {
    fn drop(&mut self) {
        // Release the MAPI table COM pointer
        unsafe {
            release_com(self.table_ptr);
        }
    }
}

// ─── MessageStoreOps: message_iter ───────────────────────────────────────────

impl MessageStoreOps {
    /// Iterate over messages in a folder, yielding only received mail items.
    ///
    /// Returns a `MessageIterator` that lazily opens messages whose message
    /// class starts with "IPM.Note" or "IPM.Anti-Virus" (case-insensitive).
    /// Rows are fetched in batches of 50 for memory-efficient iteration.
    ///
    /// # Arguments
    ///
    /// * `folder` - The folder to iterate messages in
    ///
    /// # Errors
    ///
    /// - `MsgStoreError::NotFound` if the folder has been deleted (Req 2.9)
    /// - `MsgStoreError::ProviderUnavailable` if disconnected (Req 2.11)
    /// - `MsgStoreError::Mapi` for other MAPI failures
    ///
    /// # Requirement
    ///
    /// - Req 2.2: Message enumeration via generator-style iterator
    pub fn message_iter(&self, folder: &crate::Folder) -> Result<MessageIterator, MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null".to_string(),
            });
        }

        unsafe {
            // Open the folder
            let folder_ptr = self.open_entry_raw(
                &folder.entry_id,
                MAPI_BEST_ACCESS | MAPI_DEFERRED_ERRORS,
            )?;

            let folder_obj = folder_ptr.cast::<IMAPIFolderObj>();
            let vtbl = &*(*folder_obj).vtbl;

            // Get the contents table
            let mut table_ptr: *mut c_void = ptr::null_mut();
            let hr = (vtbl.get_contents_table)(
                folder_ptr,
                MAPI_DEFERRED_ERRORS,
                &raw mut table_ptr,
            );

            if hr != S_OK || table_ptr.is_null() {
                release_com(folder_ptr);
                return Err(map_mapi_error(hr, "IMAPIContainer::GetContentsTable"));
            }

            // Set columns: PR_ENTRYID, PR_MESSAGE_CLASS_W
            #[repr(C)]
            struct PropTagArray2 {
                c_values: u32,
                tags: [u32; 2],
            }
            let columns = PropTagArray2 {
                c_values: 2,
                tags: [PR_ENTRYID, PR_MESSAGE_CLASS_W],
            };

            let table_obj_inner = table_ptr.cast::<IMAPITableObj>();
            let table_vtbl = &*(*table_obj_inner).vtbl;

            let hr = (table_vtbl.set_columns)(
                table_ptr,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
            );

            if hr != S_OK {
                release_com(table_ptr);
                release_com(folder_ptr);
                return Err(map_mapi_error(hr, "IMAPITable::SetColumns (messages)"));
            }

            // Release the folder — the table keeps its own reference
            release_com(folder_ptr);

            Ok(MessageIterator {
                table_ptr,
                store_ptr: self.store_ptr,
                store_id: self.store_id.clone(),
                buffer: Vec::new(),
                buffer_pos: 0,
                table_exhausted: false,
            })
        }
    }
}
