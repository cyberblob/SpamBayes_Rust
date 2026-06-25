//! Message property access and manipulation.
//!
//! Provides lazy-loaded property access for MAPI messages, including
//! subject, sender, body (plain text and HTML), headers, read state,
//! and custom named properties in the `PS_PUBLIC_STRINGS` namespace.
//!
//! # Architecture
//!
//! Properties are loaded on first access via the `IMessage` COM interface.
//! Text properties that are absent or unavailable return an empty string
//! rather than an error (Requirement 2.4). Fixed-size property types
//! (i32, bool) are read directly from the `PropValueUnion` without
//! allocation (Requirement 22.5 - zero-copy parsing).
//!
//! # Requirements
//!
//! - Req 2.4: Reading message properties (subject, sender, body, headers, read state, named props)
//! - Req 2.5: Writing custom named properties (`PS_PUBLIC_STRINGS` namespace)
//! - Req 2.6: Move messages between folders, including across different message stores
//! - Req 2.7: Copy messages between folders, including across different message stores
//! - Req 2.12: Save with `ObjectChanged` retry logic (up to 3 retries)
//! - Req 22.5: Zero-copy parsing for fixed-size MAPI property types

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::ptr;

use log::warn;

use crate::errors::MsgStoreError;
use crate::folder::{SPropTagArray, SPropValue, S_OK, MAPIFreeBuffer, PropValueUnion, map_mapi_error, PT_UNICODE, read_unicode_prop, PT_STRING8, read_string8_prop, PT_LONG, MAPI_E_OBJECT_CHANGED, MAPI_MODIFY, release_com, SBinary, SBinaryArray, IMAPIFolderObj, IMsgStoreExtObj, read_binary_prop, MAPI_BEST_ACCESS, PT_BINARY};
use crate::Folder;
use crate::Message;

// --- MAPI Property Tags for Message Properties ---

/// `PR_SUBJECT_W` - message subject (Unicode).
const PR_SUBJECT_W: u32 = 0x0037_001F; // PT_UNICODE

/// `PR_BODY_W` - plain text body (Unicode).
const PR_BODY_W: u32 = 0x1000_001F; // PT_UNICODE

/// `PR_BODY_HTML_W` - HTML body (Unicode).
/// Note: Some stores use `PR_BODY_HTML` (`PT_BINARY` 0x10130102) instead.
/// We try Unicode first, fall back to binary.
const PR_BODY_HTML_W: u32 = 0x1013_001F; // PT_UNICODE

/// `PR_BODY_HTML` - HTML body as binary (ANSI/UTF-8 encoded).
const PR_BODY_HTML_BINARY: u32 = 0x1013_0102; // PT_BINARY

/// `PR_TRANSPORT_MESSAGE_HEADERS_W` - RFC 2822 headers (Unicode).
const PR_TRANSPORT_MESSAGE_HEADERS_W: u32 = 0x007D_001F; // PT_UNICODE

/// `PR_TRANSPORT_MESSAGE_HEADERS_A` - RFC 2822 headers (ANSI).
const PR_TRANSPORT_MESSAGE_HEADERS_A: u32 = 0x007D_001E; // PT_STRING8

/// `PR_MESSAGE_FLAGS` - message flags (contains `MSGFLAG_READ`).
const PR_MESSAGE_FLAGS: u32 = 0x0E07_0003; // PT_LONG

/// `PR_SENDER_NAME_W` - sender display name (Unicode).
const PR_SENDER_NAME_W: u32 = 0x0C1A_001F; // PT_UNICODE

/// `PR_SEARCH_KEY` - binary search key for message identification.
const PR_SEARCH_KEY: u32 = 0x300B_0102; // PT_BINARY

/// `PR_INTERNET_CONTENT` - full RFC 2822 message as binary (`PT_BINARY`).
/// Contains the raw MIME message including headers and body.
const PR_INTERNET_CONTENT: u32 = 0x0069_0102; // PT_BINARY

/// `PR_PARENT_ENTRYID` - entry ID of the parent folder.
const PR_PARENT_ENTRYID: u32 = 0x0E09_0102; // PT_BINARY

// --- Message Flags ---

/// `MSGFLAG_READ` - message has been read.
const MSGFLAG_READ: i32 = 0x00000001;

/// `MSGFLAG_UNSENT` - message has not been sent yet.
#[allow(dead_code)]
const MSGFLAG_UNSENT: i32 = 0x00000008;

/// `MESSAGE_MOVE` flag for `CopyMessages` - move instead of copy.
const MESSAGE_MOVE: u32 = 0x00000001;

// --- Named Property Constants ---

/// `PS_PUBLIC_STRINGS` GUID: {00020329-0000-0000-C000-000000000046}
/// Used for custom named properties written by `SpamBayes`.
const PS_PUBLIC_STRINGS: [u8; 16] = [
    0x29, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

/// Named property kind: name is a Unicode string.
const MNID_STRING: u32 = 1;

/// Property type for named string properties.
const PT_UNICODE_TYPE: u32 = 0x001F;

/// Property type for named integer properties.
const PT_LONG_TYPE: u32 = 0x0003;

/// Property type for named double properties.
const PT_DOUBLE_TYPE: u32 = 0x0005;

// --- SaveChanges Flags ---

/// `KEEP_OPEN_READWRITE` - keep the object open after `SaveChanges`.
const KEEP_OPEN_READWRITE: u32 = 0x00000002;

// --- IMessage COM VTable ---

/// `IMessage` vtable layout.
///
/// `IMessage` inherits from `IMAPIProp`, which inherits from `IUnknown`.
/// Layout: `IUnknown` (0-2), `IMAPIProp` (3-13), `IMessage` (14+)
#[repr(C)]
struct IMessageVtbl {
    // IUnknown (slots 0-2)
    pub query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IMAPIProp (slots 3-13)
    pub get_last_error: *const c_void,    // slot 3
    pub save_changes: unsafe extern "system" fn(
        this: *mut c_void, flags: u32,
    ) -> i32,                             // slot 4
    pub get_props: unsafe extern "system" fn(
        this: *mut c_void,
        prop_tag_array: *const SPropTagArray,
        flags: u32,
        count: *mut u32,
        props: *mut *mut SPropValue,
    ) -> i32,                             // slot 5
    pub get_prop_list: *const c_void,     // slot 6
    pub open_property: *const c_void,     // slot 7
    pub set_props: unsafe extern "system" fn(
        this: *mut c_void,
        count: u32,
        props: *mut SPropValue,
        problems: *mut *mut c_void,
    ) -> i32,                             // slot 8
    pub delete_props: *const c_void,      // slot 9
    pub copy_to: *const c_void,           // slot 10
    pub copy_props: *const c_void,        // slot 11
    pub get_names_from_ids: *const c_void, // slot 12
    pub get_ids_from_names: unsafe extern "system" fn(
        this: *mut c_void,
        count: u32,
        prop_set_guids: *const *const [u8; 16],
        prop_names: *const *const MapiNameId,
        flags: u32,
        prop_tags: *mut *mut SPropTagArray,
    ) -> i32,                             // slot 13
}

/// Raw COM pointer wrapper for `IMessage`.
#[repr(C)]
struct IMessageObj {
    pub vtbl: *const IMessageVtbl,
}

// --- Named Property Structures ---

/// MAPINAMEID structure for resolving named properties.
#[repr(C)]
struct MapiNameId {
    /// Pointer to the property set GUID.
    pub lp_guid: *const [u8; 16],
    /// Kind of name (`MNID_STRING` or `MNID_ID`).
    pub ul_kind: u32,
    /// Union: either a numeric ID or a pointer to a Unicode string name.
    pub kind: MapiNameIdUnion,
}

/// Union for MAPINAMEID - either a string pointer or an integer ID.
#[repr(C)]
union MapiNameIdUnion {
    pub lpwstr_name: *const u16,
    pub ul_id: u32,
}

// --- FieldValue ---

/// Represents a typed value for custom named properties.
///
/// Used with `get_field()` and `set_field()` to read/write named
/// properties in the `PS_PUBLIC_STRINGS` namespace.
///
/// # Requirement
///
/// - Req 2.5: String, integer, and floating-point value types
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// A Unicode string value.
    String(String),
    /// A 32-bit integer value.
    Int(i32),
    /// A 64-bit floating-point value.
    Float(f64),
}

// --- Message Property Access Implementation ---

impl Message {
    /// Returns the message subject, lazy-loading from MAPI on first access.
    ///
    /// Returns an empty string if the property is absent or unavailable.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading message subject property
    pub fn subject(&mut self) -> &str {
        if self.subject.is_none() {
            self.subject = Some(self.load_unicode_prop(PR_SUBJECT_W));
        }
        self.subject.as_deref().unwrap_or("")
    }

    /// Returns the sender display name, lazy-loading from MAPI on first access.
    ///
    /// Returns an empty string if the property is absent or unavailable.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading message sender property
    pub fn sender(&mut self) -> &str {
        if self.sender_cache.is_none() {
            self.sender_cache = Some(self.load_unicode_prop(PR_SENDER_NAME_W));
        }
        self.sender_cache.as_deref().unwrap_or("")
    }

    /// Returns the plain text body, lazy-loading from MAPI on first access.
    ///
    /// Returns an empty string if the property is absent or unavailable.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading message body (plain text)
    pub fn body_plain(&mut self) -> &str {
        if self.body_plain.is_none() {
            self.body_plain = Some(self.load_unicode_prop(PR_BODY_W));
        }
        self.body_plain.as_deref().unwrap_or("")
    }

    /// Returns the HTML body content, lazy-loading from MAPI on first access.
    ///
    /// Tries `PR_BODY_HTML` (Unicode) first, then falls back to the binary
    /// variant and decodes as UTF-8.
    ///
    /// Returns an empty string if the property is absent or unavailable.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading message body (HTML)
    pub fn body_html(&mut self) -> &str {
        if self.body_html.is_none() {
            let html = self.load_unicode_prop(PR_BODY_HTML_W);
            if html.is_empty() {
                // Fall back to binary HTML property (common on some stores)
                self.body_html = Some(self.load_binary_as_string(PR_BODY_HTML_BINARY));
            } else {
                self.body_html = Some(html);
            }
        }
        self.body_html.as_deref().unwrap_or("")
    }

    /// Returns the RFC 2822 transport headers, lazy-loading from MAPI.
    ///
    /// Tries the Unicode variant first, then falls back to ANSI.
    /// Returns an empty string if the property is absent or unavailable.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading message headers
    pub fn headers(&mut self) -> &str {
        if self.headers.is_none() {
            let hdrs = self.load_unicode_prop(PR_TRANSPORT_MESSAGE_HEADERS_W);
            if hdrs.is_empty() {
                self.headers = Some(self.load_string8_prop(PR_TRANSPORT_MESSAGE_HEADERS_A));
            } else {
                self.headers = Some(hdrs);
            }
        }
        self.headers.as_deref().unwrap_or("")
    }

    /// Returns the message read state.
    ///
    /// Reads `PR_MESSAGE_FLAGS` and checks the `MSGFLAG_READ` bit.
    /// Uses zero-copy parsing: the i32 value is read directly from
    /// the `PropValueUnion` without heap allocation (Req 22.5).
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading message read state
    /// - Req 22.5: Zero-copy parsing for fixed-size property types
    #[must_use]
    pub fn get_read_state(&self) -> bool {
        if self.mapi_message.is_null() {
            return false;
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [PR_MESSAGE_FLAGS],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return false;
            }

            // Zero-copy: read i32 directly from the union (Req 22.5)
            let flags = (*props_ptr).value.l;
            MAPIFreeBuffer(props_ptr.cast::<c_void>());

            (flags & MSGFLAG_READ) != 0
        }
    }

    /// Sets the message read state by modifying `PR_MESSAGE_FLAGS`.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Writing message read state
    pub fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError> {
        if self.mapi_message.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Message pointer is null".to_string(),
            });
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            // Read current flags
            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [PR_MESSAGE_FLAGS],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            let mut flags: i32 = 0;
            if hr == S_OK && !props_ptr.is_null() && count >= 1 {
                flags = (*props_ptr).value.l;
                MAPIFreeBuffer(props_ptr.cast::<c_void>());
            } else if !props_ptr.is_null() {
                MAPIFreeBuffer(props_ptr.cast::<c_void>());
            }

            // Modify the flag
            if read {
                flags |= MSGFLAG_READ;
            } else {
                flags &= !MSGFLAG_READ;
            }

            // Write back the modified flags
            let mut prop = SPropValue {
                ul_prop_tag: PR_MESSAGE_FLAGS,
                dw_align_pad: 0,
                value: PropValueUnion { l: flags },
            };

            let hr = (vtbl.set_props)(
                self.mapi_message,
                1,
                &raw mut prop,
                ptr::null_mut(),
            );

            if hr != S_OK {
                return Err(map_mapi_error(hr, "IMessage::SetProps(PR_MESSAGE_FLAGS)"));
            }

            self.dirty = true;
            Ok(())
        }
    }

    /// Gets a custom named property value from `PS_PUBLIC_STRINGS`.
    ///
    /// Resolves the property name to a MAPI property ID, then reads
    /// the value. Returns `None` if the property is not set or the
    /// name cannot be resolved.
    ///
    /// # Requirement
    ///
    /// - Req 2.4: Reading custom named properties
    /// - Req 22.5: Zero-copy for integer values
    #[must_use]
    pub fn get_field(&self, name: &str) -> Option<FieldValue> {
        if self.mapi_message.is_null() {
            return None;
        }

        unsafe {
            // Resolve the named property to a prop tag
            let prop_tag = self.resolve_named_prop(name, PT_UNICODE_TYPE)?;

            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [prop_tag],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return None;
            }

            let actual_type = (*props_ptr).ul_prop_tag & 0xFFFF;
            let result = match actual_type {
                PT_UNICODE => {
                    let s = read_unicode_prop(props_ptr);
                    if s.is_empty() {
                        None
                    } else {
                        Some(FieldValue::String(s))
                    }
                }
                PT_STRING8 => {
                    let s = read_string8_prop(props_ptr);
                    if s.is_empty() {
                        None
                    } else {
                        Some(FieldValue::String(s))
                    }
                }
                PT_LONG => {
                    // Zero-copy: read i32 directly (Req 22.5)
                    Some(FieldValue::Int((*props_ptr).value.l))
                }
                PT_DOUBLE_TYPE => {
                    // Read f64 from the raw bytes of the union
                    let raw = (*props_ptr).value.raw;
                    let val = f64::from_le_bytes([
                        raw[0], raw[1], raw[2], raw[3],
                        raw[4], raw[5], raw[6], raw[7],
                    ]);
                    Some(FieldValue::Float(val))
                }
                _ => None,
            };

            MAPIFreeBuffer(props_ptr.cast::<c_void>());
            result
        }
    }

    /// Sets a custom named property value in `PS_PUBLIC_STRINGS`.
    ///
    /// Resolves the property name to a MAPI property ID (creating it
    /// if necessary), then writes the value. Marks the message as dirty.
    ///
    /// # Requirement
    ///
    /// - Req 2.5: Writing custom named properties (string, integer, float)
    #[allow(clippy::needless_pass_by_value)] // FieldValue ownership is semantically correct for a setter
    pub fn set_field(
        &mut self,
        name: &str,
        value: FieldValue,
    ) -> Result<(), MsgStoreError> {
        if self.mapi_message.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Message pointer is null".to_string(),
            });
        }

        unsafe {
            // Determine the property type from the value
            let prop_type = match &value {
                FieldValue::String(_) => PT_UNICODE_TYPE,
                FieldValue::Int(_) => PT_LONG_TYPE,
                FieldValue::Float(_) => PT_DOUBLE_TYPE,
            };

            // Resolve the named property, creating if needed
            let prop_tag = self.resolve_named_prop(name, prop_type)
                .ok_or_else(|| MsgStoreError::Mapi {
                    hr: -1,
                    message: format!("Failed to resolve named property '{name}'"),
                })?;

            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            // Build the SPropValue based on the value type
            // name_wide must live until after set_props returns (holds the string data).
            #[allow(unused_assignments)]
            let mut name_wide: Vec<u16> = Vec::new();
            let mut prop = match &value {
                FieldValue::String(s) => {
                    name_wide = s.encode_utf16().chain(std::iter::once(0)).collect();
                    SPropValue {
                        ul_prop_tag: prop_tag,
                        dw_align_pad: 0,
                        value: PropValueUnion {
                            lpszW: name_wide.as_mut_ptr(),
                        },
                    }
                }
                FieldValue::Int(i) => SPropValue {
                    ul_prop_tag: prop_tag,
                    dw_align_pad: 0,
                    value: PropValueUnion { l: *i },
                },
                FieldValue::Float(f) => {
                    let bytes = f.to_le_bytes();
                    let mut raw = [0u8; 16];
                    raw[..8].copy_from_slice(&bytes);
                    SPropValue {
                        ul_prop_tag: prop_tag,
                        dw_align_pad: 0,
                        value: PropValueUnion { raw },
                    }
                }
            };

            let hr = (vtbl.set_props)(
                self.mapi_message,
                1,
                &raw mut prop,
                ptr::null_mut(),
            );

            if hr != S_OK {
                return Err(map_mapi_error(hr, "IMessage::SetProps(named property)"));
            }

            self.dirty = true;
            Ok(())
        }
    }

    /// Saves all pending changes to the MAPI message.
    ///
    /// Calls `IMessage::SaveChanges` with `KEEP_OPEN_READWRITE` so the
    /// message remains usable after saving.
    pub fn save(&mut self) -> Result<(), MsgStoreError> {
        if self.mapi_message.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Message pointer is null".to_string(),
            });
        }

        if !self.dirty {
            return Ok(());
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            let hr = (vtbl.save_changes)(self.mapi_message, KEEP_OPEN_READWRITE);

            if hr != S_OK {
                return Err(map_mapi_error(hr, "IMessage::SaveChanges"));
            }

            self.dirty = false;
            Ok(())
        }
    }

    /// Saves all pending changes with retry logic for `ObjectChanged` errors.
    ///
    /// When MAPI returns `MAPI_E_OBJECT_CHANGED` (indicating the message
    /// was modified externally during save), this method re-opens the
    /// message object and retries the save up to 3 times.
    ///
    /// If all 3 retries fail, unsaved changes are discarded and a warning
    /// is logged. The method returns `Ok(())` without raising an exception
    /// to the caller.
    ///
    /// # Requirement
    ///
    /// - Req 2.12: Retry save up to 3 times on `ObjectChanged`, then discard
    pub fn save_with_retry(&mut self) -> Result<(), MsgStoreError> {
        if self.mapi_message.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Message pointer is null".to_string(),
            });
        }

        if !self.dirty {
            return Ok(());
        }

        const MAX_RETRIES: u32 = 3;

        for attempt in 0..MAX_RETRIES {
            unsafe {
                let msg_obj = self.mapi_message.cast::<IMessageObj>();
                let vtbl = &*(*msg_obj).vtbl;

                let hr = (vtbl.save_changes)(self.mapi_message, KEEP_OPEN_READWRITE);

                if hr == S_OK {
                    self.dirty = false;
                    return Ok(());
                }

                if hr == MAPI_E_OBJECT_CHANGED {
                    // Re-open the message to get a fresh copy
                    if !self.reopen_message() {
                        warn!(
                            "save_with_retry: failed to re-open message on attempt {}/{}",
                            attempt + 1,
                            MAX_RETRIES
                        );
                        continue;
                    }
                    // Retry the save on the next loop iteration
                    continue;
                }

                // For non-ObjectChanged errors, fail immediately
                return Err(map_mapi_error(hr, "IMessage::SaveChanges"));
            }
        }

        // All retries exhausted: discard changes and log warning (Req 2.12)
        self.dirty = false;
        warn!(
            "save_with_retry: all {MAX_RETRIES} retries exhausted for message. \
             Discarding unsaved changes."
        );
        Ok(())
    }

    /// Moves this message to the destination folder.
    ///
    /// For same-store moves, uses `IMAPIFolder::CopyMessages` with the
    /// `MESSAGE_MOVE` flag on the source folder (efficient server-side move).
    ///
    /// For cross-store moves (different `store_id` between message and
    /// destination), copies properties to a new message in the destination
    /// folder, then deletes the original.
    ///
    /// # Arguments
    ///
    /// * `source_folder` - The folder this message currently resides in
    /// * `dest_folder` - The folder to move the message to
    ///
    /// # Requirement
    ///
    /// - Req 2.6: Move messages between folders, including cross-store
    pub fn move_to(
        &self,
        source_folder: &Folder,
        dest_folder: &Folder,
    ) -> Result<(), MsgStoreError> {
        self.copy_or_move(source_folder, dest_folder, true)
    }

    /// Copies this message to the destination folder.
    ///
    /// For same-store copies, uses `IMAPIFolder::CopyMessages` on the
    /// source folder (efficient server-side copy).
    ///
    /// For cross-store copies (different `store_id` between message and
    /// destination), copies properties to a new message in the destination.
    ///
    /// # Arguments
    ///
    /// * `source_folder` - The folder this message currently resides in
    /// * `dest_folder` - The folder to copy the message to
    ///
    /// # Requirement
    ///
    /// - Req 2.7: Copy messages between folders, including cross-store
    pub fn copy_to(
        &self,
        source_folder: &Folder,
        dest_folder: &Folder,
    ) -> Result<(), MsgStoreError> {
        self.copy_or_move(source_folder, dest_folder, false)
    }

    /// Exports the message in RFC 2822 format.
    ///
    /// Reads `PR_INTERNET_CONTENT` (the full MIME message as stored by
    /// the transport provider). If that property is unavailable, falls
    /// back to concatenating the transport headers with the plain text body.
    ///
    /// # Returns
    ///
    /// Raw bytes of the RFC 2822 message.
    ///
    /// # Errors
    ///
    /// Returns `MsgStoreError::Mapi` if the message content cannot be
    /// retrieved from either source.
    pub fn get_email_rfc2822(&mut self) -> Result<Vec<u8>, MsgStoreError> {
        if self.mapi_message.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Message pointer is null".to_string(),
            });
        }

        // Try PR_INTERNET_CONTENT first (full RFC 2822 message)
        let content = self.load_binary_prop(PR_INTERNET_CONTENT);
        if !content.is_empty() {
            return Ok(content);
        }

        // Fallback: concatenate transport headers + body
        let headers = self.headers().to_string();
        let body = self.body_plain().to_string();

        if headers.is_empty() && body.is_empty() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "No RFC 2822 content available (PR_INTERNET_CONTENT \
                          and headers/body are empty)"
                    .to_string(),
            });
        }

        let mut result = Vec::new();
        if !headers.is_empty() {
            result.extend_from_slice(headers.as_bytes());
            // Ensure headers end with double CRLF before body
            if !headers.ends_with("\r\n\r\n") {
                if headers.ends_with("\r\n") {
                    result.extend_from_slice(b"\r\n");
                } else {
                    result.extend_from_slice(b"\r\n\r\n");
                }
            }
        }
        if !body.is_empty() {
            result.extend_from_slice(body.as_bytes());
        }

        Ok(result)
    }

    // --- Private Helpers for Move/Copy/Save ---

    /// Internal implementation for move and copy operations.
    fn copy_or_move(
        &self,
        source_folder: &Folder,
        dest_folder: &Folder,
        is_move: bool,
    ) -> Result<(), MsgStoreError> {
        if self.mapi_message.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Message pointer is null".to_string(),
            });
        }

        let is_same_store = self.store_id == dest_folder.store_id;

        if is_same_store {
            self.copy_messages_same_store(source_folder, dest_folder, is_move)
        } else {
            self.copy_messages_cross_store(dest_folder, is_move)
        }
    }

    /// Perform same-store copy/move using `IMAPIFolder::CopyMessages`.
    fn copy_messages_same_store(
        &self,
        source_folder: &Folder,
        dest_folder: &Folder,
        is_move: bool,
    ) -> Result<(), MsgStoreError> {
        unsafe {
            let source_folder_ptr = self.open_folder_on_store(
                &source_folder.entry_id,
                MAPI_MODIFY,
            )?;

            let dest_folder_ptr = match self.open_folder_on_store(
                &dest_folder.entry_id,
                MAPI_MODIFY,
            ) {
                Ok(ptr) => ptr,
                Err(e) => {
                    release_com(source_folder_ptr);
                    return Err(e);
                }
            };

            // Build ENTRYLIST with this message's entry ID
            let eid = SBinary {
                cb: self.entry_id.len() as u32,
                lpb: self.entry_id.as_ptr().cast_mut(),
            };
            let entry_list = SBinaryArray {
                c_values: 1,
                lpbin: &raw const eid,
            };

            let src_folder_obj = source_folder_ptr.cast::<IMAPIFolderObj>();
            let src_vtbl = &*(*src_folder_obj).vtbl;

            let flags = if is_move { MESSAGE_MOVE } else { 0 };

            let hr = (src_vtbl.copy_messages)(
                source_folder_ptr,
                &raw const entry_list,
                ptr::null(),       // default IID
                dest_folder_ptr,
                0,                 // no UI param
                ptr::null_mut(),   // no progress
                flags,
            );

            release_com(dest_folder_ptr);
            release_com(source_folder_ptr);

            if hr != S_OK {
                return Err(map_mapi_error(
                    hr,
                    if is_move {
                        "IMAPIFolder::CopyMessages(MOVE)"
                    } else {
                        "IMAPIFolder::CopyMessages(COPY)"
                    },
                ));
            }

            Ok(())
        }
    }

    /// Perform cross-store copy/move using `IMessage` `CopyTo`.
    ///
    /// Creates a new message in the destination folder, copies all
    /// properties from this message to the new one, saves it, then
    /// (for moves) deletes the original.
    fn copy_messages_cross_store(
        &self,
        dest_folder: &Folder,
        is_move: bool,
    ) -> Result<(), MsgStoreError> {
        unsafe {
            // Open destination folder
            let dest_folder_ptr = self.open_folder_on_store(
                &dest_folder.entry_id,
                MAPI_MODIFY,
            )?;

            // Create a new message in the destination folder.
            // IMAPIFolder::CreateMessage is at slot 17.
            let dest_folder_obj = dest_folder_ptr.cast::<IMAPIFolderObj>();
            let dest_vtbl_raw = (*dest_folder_obj).vtbl.cast::<*const c_void>();

            type CreateMessageFn = unsafe extern "system" fn(
                this: *mut c_void,
                interface: *const c_void,
                flags: u32,
                msg: *mut *mut c_void,
            ) -> i32;

            let create_message_fn: CreateMessageFn =
                std::mem::transmute(*dest_vtbl_raw.add(17));

            let mut new_msg_ptr: *mut c_void = ptr::null_mut();
            let hr = create_message_fn(
                dest_folder_ptr,
                ptr::null(),
                0,
                &raw mut new_msg_ptr,
            );

            if hr != S_OK || new_msg_ptr.is_null() {
                release_com(dest_folder_ptr);
                return Err(map_mapi_error(hr, "IMAPIFolder::CreateMessage"));
            }

            // Use IMAPIProp::CopyTo (slot 10) to copy all properties
            // from source message to destination message.
            let src_msg_obj = self.mapi_message.cast::<IMessageObj>();
            let src_vtbl_raw = (*src_msg_obj).vtbl.cast::<*const c_void>();

            type CopyToFn = unsafe extern "system" fn(
                this: *mut c_void,
                ciid_exclude: u32,
                iid_exclude: *const c_void,
                prop_tags_exclude: *const c_void,
                ui_param: usize,
                progress: *mut c_void,
                interface: *const c_void,
                dest: *mut c_void,
                flags: u32,
                problems: *mut *mut c_void,
            ) -> i32;

            let copy_to_fn: CopyToFn =
                std::mem::transmute(*src_vtbl_raw.add(10));

            let hr = copy_to_fn(
                self.mapi_message,
                0,                 // no IIDs to exclude
                ptr::null(),       // no IID exclusion array
                ptr::null(),       // no prop tag exclusion
                0,                 // no UI param
                ptr::null_mut(),   // no progress
                ptr::null(),       // default interface
                new_msg_ptr,
                0,                 // no flags
                ptr::null_mut(),   // no problems output
            );

            if hr != S_OK {
                release_com(new_msg_ptr);
                release_com(dest_folder_ptr);
                return Err(map_mapi_error(hr, "IMessage::CopyTo (cross-store)"));
            }

            // Save the new message
            let new_msg_obj = new_msg_ptr.cast::<IMessageObj>();
            let new_vtbl = &*(*new_msg_obj).vtbl;
            let hr = (new_vtbl.save_changes)(new_msg_ptr, 0);

            release_com(new_msg_ptr);
            release_com(dest_folder_ptr);

            if hr != S_OK {
                return Err(map_mapi_error(
                    hr,
                    "IMessage::SaveChanges (cross-store dest)",
                ));
            }

            // For moves, delete the original message from source store
            if is_move {
                self.delete_self()?;
            }

            Ok(())
        }
    }

    /// Open a folder by entry ID using the message's stored store pointer.
    ///
    /// # Safety
    ///
    /// Returns a raw COM pointer that must be released by the caller.
    unsafe fn open_folder_on_store(
        &self,
        folder_eid: &[u8],
        flags: u32,
    ) -> Result<*mut c_void, MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null on Message".to_string(),
            });
        }

        let store_obj = self.store_ptr.cast::<IMsgStoreExtObj>();
        let vtbl = &*(*store_obj).vtbl;

        let mut obj_type: u32 = 0;
        let mut obj_ptr: *mut c_void = ptr::null_mut();

        let hr = (vtbl.open_entry)(
            self.store_ptr,
            folder_eid.len() as u32,
            folder_eid.as_ptr(),
            ptr::null(),
            flags,
            &raw mut obj_type,
            &raw mut obj_ptr,
        );

        if hr != S_OK || obj_ptr.is_null() {
            return Err(map_mapi_error(
                hr,
                "IMsgStore::OpenEntry(folder for move/copy)",
            ));
        }

        Ok(obj_ptr)
    }

    /// Delete this message from its containing folder.
    ///
    /// Reads `PR_PARENT_ENTRYID` to find the source folder, opens it,
    /// then calls `DeleteMessages`.
    unsafe fn delete_self(&self) -> Result<(), MsgStoreError> {
        if self.store_ptr.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Store pointer is null on Message".to_string(),
            });
        }

        // Get the parent folder entry ID from the message
        let msg_obj = self.mapi_message.cast::<IMessageObj>();
        let vtbl = &*(*msg_obj).vtbl;

        #[repr(C)]
        struct PropTagArray1 {
            c_values: u32,
            tags: [u32; 1],
        }
        let columns = PropTagArray1 {
            c_values: 1,
            tags: [PR_PARENT_ENTRYID],
        };

        let mut count: u32 = 0;
        let mut props_ptr: *mut SPropValue = ptr::null_mut();
        let hr = (vtbl.get_props)(
            self.mapi_message,
            (&raw const columns).cast::<SPropTagArray>(),
            0,
            &raw mut count,
            &raw mut props_ptr,
        );

        if hr != S_OK || props_ptr.is_null() || count < 1 {
            if !props_ptr.is_null() {
                MAPIFreeBuffer(props_ptr.cast::<c_void>());
            }
            return Err(MsgStoreError::Mapi {
                hr,
                message: "Failed to get parent folder entry ID".to_string(),
            });
        }

        let parent_eid = read_binary_prop(props_ptr);
        MAPIFreeBuffer(props_ptr.cast::<c_void>());

        if parent_eid.is_empty() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Parent folder entry ID is empty".to_string(),
            });
        }

        // Open the parent folder
        let folder_ptr = self.open_folder_on_store(&parent_eid, MAPI_MODIFY)?;

        // Build ENTRYLIST with this message's entry ID
        let eid = SBinary {
            cb: self.entry_id.len() as u32,
            lpb: self.entry_id.as_ptr().cast_mut(),
        };
        let entry_list = SBinaryArray {
            c_values: 1,
            lpbin: &raw const eid,
        };

        let folder_obj = folder_ptr.cast::<IMAPIFolderObj>();
        let folder_vtbl = &*(*folder_obj).vtbl;

        let hr = (folder_vtbl.delete_messages)(
            folder_ptr,
            &raw const entry_list,
            0,                 // no UI param
            ptr::null_mut(),   // no progress
            0,                 // no flags
        );

        release_com(folder_ptr);

        if hr != S_OK {
            return Err(map_mapi_error(hr, "IMAPIFolder::DeleteMessages"));
        }

        Ok(())
    }

    /// Re-open the MAPI message object (for save retry after `ObjectChanged`).
    ///
    /// Releases the current message pointer and re-opens it from the
    /// store using the stored `entry_id` and `store_ptr`.
    ///
    /// Returns true if the re-open succeeded.
    unsafe fn reopen_message(&mut self) -> bool {
        if self.store_ptr.is_null() || self.entry_id.is_empty() {
            return false;
        }

        // Release the current message pointer
        if !self.mapi_message.is_null() {
            release_com(self.mapi_message);
            self.mapi_message = ptr::null_mut();
        }

        // Re-open from store
        let store_obj = self.store_ptr.cast::<IMsgStoreExtObj>();
        let vtbl = &*(*store_obj).vtbl;

        let mut obj_type: u32 = 0;
        let mut obj_ptr: *mut c_void = ptr::null_mut();

        let hr = (vtbl.open_entry)(
            self.store_ptr,
            self.entry_id.len() as u32,
            self.entry_id.as_ptr(),
            ptr::null(),
            MAPI_MODIFY | MAPI_BEST_ACCESS,
            &raw mut obj_type,
            &raw mut obj_ptr,
        );

        if hr != S_OK || obj_ptr.is_null() {
            return false;
        }

        self.mapi_message = obj_ptr;
        true
    }

    // --- Private Property Loading Helpers ---

    /// Load a binary property from the message as raw bytes.
    ///
    /// Returns an empty Vec if the property is absent or cannot be read.
    fn load_binary_prop(&self, prop_tag: u32) -> Vec<u8> {
        if self.mapi_message.is_null() {
            return Vec::new();
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [prop_tag],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return Vec::new();
            }

            let actual_type = (*props_ptr).ul_prop_tag & 0xFFFF;
            let result = if actual_type == PT_BINARY {
                read_binary_prop(props_ptr)
            } else {
                Vec::new()
            };

            MAPIFreeBuffer(props_ptr.cast::<c_void>());
            result
        }
    }

    /// Returns the `PR_SEARCH_KEY` for message identification.
    ///
    /// The search key is used as a stable identifier for message
    /// metadata storage (message info database).
    #[must_use]
    pub fn get_search_key(&self) -> Vec<u8> {
        if self.mapi_message.is_null() {
            return Vec::new();
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [PR_SEARCH_KEY],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return Vec::new();
            }

            let result = read_binary_prop(props_ptr);
            MAPIFreeBuffer(props_ptr.cast::<c_void>());
            result
        }
    }

    /// Load a Unicode (`PT_UNICODE`) property from the message.
    ///
    /// Returns an empty string if the property is absent, has wrong type,
    /// or cannot be read (Req 2.4 - empty string for absent properties).
    fn load_unicode_prop(&self, prop_tag: u32) -> String {
        if self.mapi_message.is_null() {
            return String::new();
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [prop_tag],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return String::new();
            }

            // Check that the returned property type matches expected
            let actual_type = (*props_ptr).ul_prop_tag & 0xFFFF;
            let result = match actual_type {
                PT_UNICODE => read_unicode_prop(props_ptr),
                PT_STRING8 => read_string8_prop(props_ptr),
                _ => String::new(),
            };

            MAPIFreeBuffer(props_ptr.cast::<c_void>());
            result
        }
    }

    /// Load a `PT_STRING8` (ANSI) property from the message.
    ///
    /// Returns an empty string if the property is absent or has wrong type.
    fn load_string8_prop(&self, prop_tag: u32) -> String {
        if self.mapi_message.is_null() {
            return String::new();
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [prop_tag],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return String::new();
            }

            let actual_type = (*props_ptr).ul_prop_tag & 0xFFFF;
            let result = match actual_type {
                PT_STRING8 => read_string8_prop(props_ptr),
                PT_UNICODE => read_unicode_prop(props_ptr),
                _ => String::new(),
            };

            MAPIFreeBuffer(props_ptr.cast::<c_void>());
            result
        }
    }

    /// Load a binary property and interpret as UTF-8 string.
    ///
    /// Used for `PR_BODY_HTML` which some stores expose as `PT_BINARY`
    /// rather than `PT_UNICODE`. Returns empty string on failure.
    fn load_binary_as_string(&self, prop_tag: u32) -> String {
        if self.mapi_message.is_null() {
            return String::new();
        }

        unsafe {
            let msg_obj = self.mapi_message.cast::<IMessageObj>();
            let vtbl = &*(*msg_obj).vtbl;

            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [prop_tag],
            };

            let mut count: u32 = 0;
            let mut props_ptr: *mut SPropValue = ptr::null_mut();
            let hr = (vtbl.get_props)(
                self.mapi_message,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
                &raw mut count,
                &raw mut props_ptr,
            );

            if hr != S_OK || props_ptr.is_null() || count < 1 {
                if !props_ptr.is_null() {
                    MAPIFreeBuffer(props_ptr.cast::<c_void>());
                }
                return String::new();
            }

            let actual_type = (*props_ptr).ul_prop_tag & 0xFFFF;
            let result = if actual_type == PT_BINARY {
                let bytes = read_binary_prop(props_ptr);
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                String::new()
            };

            MAPIFreeBuffer(props_ptr.cast::<c_void>());
            result
        }
    }

    /// Resolve a named property string to a MAPI property tag.
    ///
    /// Uses `IMessage::GetIDsFromNames` to map a string name in the
    /// `PS_PUBLIC_STRINGS` namespace to a property ID. The returned tag
    /// combines the resolved property ID with the requested type.
    ///
    /// Returns `None` if the name cannot be resolved (property doesn't
    /// exist and creation is not supported).
    unsafe fn resolve_named_prop(&self, name: &str, prop_type: u32) -> Option<u32> {
        let msg_obj = self.mapi_message.cast::<IMessageObj>();
        let vtbl = &*(*msg_obj).vtbl;

        // Encode name as null-terminated UTF-16
        let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        // Build MAPINAMEID structure
        let guid = PS_PUBLIC_STRINGS;
        let name_id = MapiNameId {
            lp_guid: &raw const guid,
            ul_kind: MNID_STRING,
            kind: MapiNameIdUnion {
                lpwstr_name: name_wide.as_ptr(),
            },
        };

        let name_ids: [*const MapiNameId; 1] = [&raw const name_id];
        let guid_ptr: *const [u8; 16] = &raw const guid;
        let guid_ptrs: [*const [u8; 16]; 1] = [guid_ptr];

        let mut prop_tags_ptr: *mut SPropTagArray = ptr::null_mut();

        // MAPI_CREATE flag (0x00000002) to create if not existing
        let hr = (vtbl.get_ids_from_names)(
            self.mapi_message,
            1,
            guid_ptrs.as_ptr(),
            name_ids.as_ptr(),
            0x00000002, // MAPI_CREATE
            &raw mut prop_tags_ptr,
        );

        if hr != S_OK || prop_tags_ptr.is_null() {
            if !prop_tags_ptr.is_null() {
                MAPIFreeBuffer(prop_tags_ptr.cast::<c_void>());
            }
            return None;
        }

        // Read the resolved property ID from the tag array
        if (*prop_tags_ptr).c_values < 1 {
            MAPIFreeBuffer(prop_tags_ptr.cast::<c_void>());
            return None;
        }

        // The returned tag has the property ID in the high 16 bits
        // and PT_UNSPECIFIED (0x0000) in the low 16 bits.
        // We replace the type with the requested type.
        let raw_tag = (*prop_tags_ptr).aul_prop_tag[0];
        MAPIFreeBuffer(prop_tags_ptr.cast::<c_void>());

        // Check if the property ID is valid (non-zero high word)
        let prop_id = raw_tag & 0xFFFF0000;
        if prop_id == 0 {
            return None;
        }

        // Combine property ID with the requested type
        Some(prop_id | prop_type)
    }
}
