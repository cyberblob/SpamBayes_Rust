//! Folder-related utilities and MAPI FFI bindings for folder operations.
//!
//! Provides the COM vtable definitions for `IMAPIFolder` and `IMsgStore`
//! (extended with folder-level methods), property tag constants, and
//! helper functions for working with MAPI folders.
//!
//! # Architecture
//!
//! The folder operations use raw MAPI FFI calls to:
//! - Open folders via `IMsgStore::OpenEntry`
//! - Enumerate sub-folders via `IMAPIFolder::GetHierarchyTable`
//! - Read message counts via `IMAPIFolder::GetContentsTable`
//! - Create sub-folders via `IMAPIFolder::CreateFolder`
//!
//! # Requirements
//!
//! - Req 2.1: Folder enumeration with recursive sub-folder iteration
//! - Req 2.8: Create new folders under existing folders
//! - Req 2.9: `NotFound` for deleted/moved objects
//! - Req 2.10: `ReadOnly` for read-only stores
//! - Req 2.11: `ProviderUnavailable` for disconnected stores

#![cfg(target_os = "windows")]

use std::ffi::c_void;

use crate::errors::MsgStoreError;

// ─── MAPI Property Tags for Folder Operations ──────────────────────────────

/// `PR_IPM_SUBTREE_ENTRYID` — entry ID of the IPM subtree root folder.
pub(crate) const PR_IPM_SUBTREE_ENTRYID: u32 = 0x35E0_0102; // PT_BINARY

/// `PR_ENTRYID` — the entry ID property.
pub(crate) const PR_ENTRYID: u32 = 0x0FFF_0102; // PT_BINARY

/// `PR_STORE_ENTRYID` — the store entry ID.
pub(crate) const PR_STORE_ENTRYID: u32 = 0x0FFB_0102; // PT_BINARY

/// `PR_DISPLAY_NAME_A` — display name (ANSI).
pub(crate) const PR_DISPLAY_NAME_A: u32 = 0x3001_001E; // PT_STRING8

/// `PR_DISPLAY_NAME_W` — display name (Unicode).
pub(crate) const PR_DISPLAY_NAME_W: u32 = 0x3001_001F; // PT_UNICODE

/// `PR_CONTENT_COUNT` — number of messages in a folder.
pub(crate) const PR_CONTENT_COUNT: u32 = 0x3602_0003; // PT_LONG

/// `PR_FOLDER_TYPE` — folder type (generic, search, root).
pub(crate) const PR_FOLDER_TYPE: u32 = 0x3601_0003; // PT_LONG

/// `PR_PARENT_ENTRYID` — entry ID of the parent folder.
#[allow(dead_code)]
pub(crate) const PR_PARENT_ENTRYID: u32 = 0x0E09_0102; // PT_BINARY

// ─── MAPI Error Codes ────────────────────────────────────────────────────────

/// `MAPI_E_NOT_FOUND` — object has been deleted or moved.
pub(crate) const MAPI_E_NOT_FOUND: i32 = 0x8004010F_u32 as i32;

/// `MAPI_E_OBJECT_DELETED` — object has been permanently deleted.
pub(crate) const MAPI_E_OBJECT_DELETED: i32 = 0x8004010A_u32 as i32;

/// `MAPI_E_NO_ACCESS` — read-only store or insufficient permissions.
pub(crate) const MAPI_E_NO_ACCESS: i32 = 0x80040003_u32 as i32;

/// `MAPI_E_NETWORK_ERROR` — network connectivity failure.
pub(crate) const MAPI_E_NETWORK_ERROR: i32 = 0x80040115_u32 as i32;

/// `MAPI_E_FAILONEPROVIDER` — a single provider is unavailable.
pub(crate) const MAPI_E_FAILONEPROVIDER: i32 = 0x8004011D_u32 as i32;

/// `MAPI_E_OBJECT_CHANGED` — object was modified externally.
pub(crate) const MAPI_E_OBJECT_CHANGED: i32 = 0x80040109_u32 as i32;

// ─── MAPI Flags ──────────────────────────────────────────────────────────────

/// Flag for `OpenEntry`: request modify access.
pub(crate) const MAPI_MODIFY: u32 = 0x00000001;

/// Flag for deferred errors on table operations.
pub(crate) const MAPI_DEFERRED_ERRORS: u32 = 0x00000008;

/// MAPI best-access flag for `OpenEntry`.
pub(crate) const MAPI_BEST_ACCESS: u32 = 0x00000010;

/// Folder type: generic (standard) folder.
pub(crate) const FOLDER_GENERIC: u32 = 1;

/// Flag for `CreateFolder`: open existing folder if it already exists.
pub(crate) const OPEN_IF_EXISTS: u32 = 0x00000001;

// ─── MAPI Property Value Structures ─────────────────────────────────────────

/// MAPI binary property value (`SBinary`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SBinary {
    pub cb: u32,
    pub lpb: *mut u8,
}

/// A single MAPI property value (`SPropValue`).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SPropValue {
    pub ul_prop_tag: u32,
    pub dw_align_pad: u32,
    pub value: PropValueUnion,
}

/// Union for `SPropValue` data.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(non_snake_case)]
pub(crate) union PropValueUnion {
    pub b: i16,          // PT_BOOLEAN
    pub l: i32,          // PT_LONG
    pub lpszA: *mut u8,  // PT_STRING8
    pub lpszW: *mut u16, // PT_UNICODE
    pub bin: SBinary,    // PT_BINARY
    pub raw: [u8; 16],   // Ensure union is large enough
}

/// A row from a MAPI table (`SRow`).
#[repr(C)]
pub(crate) struct SRow {
    pub ul_adrentry_pad: u32,
    pub c_values: u32,
    pub lp_props: *mut SPropValue,
}

/// Row set returned by table queries (`SRowSet`).
#[repr(C)]
pub(crate) struct SRowSet {
    pub c_rows: u32,
    pub a_row: [SRow; 1], // Variable-length array (C-style)
}

/// MAPI property tag array (`SPropTagArray`).
#[repr(C)]
pub(crate) struct SPropTagArray {
    pub c_values: u32,
    pub aul_prop_tag: [u32; 1], // Variable-length array
}

/// MAPI entry list (`SBinaryArray` / ENTRYLIST) for bulk message operations.
#[repr(C)]
pub(crate) struct SBinaryArray {
    pub c_values: u32,
    pub lpbin: *const SBinary,
}

// ─── COM Interface VTable Definitions ────────────────────────────────────────

/// `IUnknown` vtable (base for all COM interfaces).
#[repr(C)]
pub(crate) struct IUnknownVtbl {
    pub query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const [u8; 16],
        ppv: *mut *mut c_void,
    ) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

/// `IMsgStore` vtable (extended) — includes `OpenEntry` and `GetProps` for store-level
/// folder operations.
///
/// `IMsgStore` inherits from `IMAPIProp`, which inherits from `IUnknown`.
/// The vtable layout is:
///   `IUnknown` (0-2), `IMAPIProp` (3-12), `IMsgStore` (13+)
#[repr(C)]
pub(crate) struct IMsgStoreExtVtbl {
    // IUnknown (slots 0-2)
    pub query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IMAPIProp (slots 3-12)
    pub get_last_error: *const c_void,    // slot 3
    pub save_changes: *const c_void,      // slot 4
    pub get_props: unsafe extern "system" fn(
        this: *mut c_void,
        prop_tag_array: *const SPropTagArray,
        flags: u32,
        count: *mut u32,
        props: *mut *mut SPropValue,
    ) -> i32,                             // slot 5
    pub get_prop_list: *const c_void,     // slot 6
    pub open_property: *const c_void,     // slot 7
    pub set_props: *const c_void,         // slot 8
    pub delete_props: *const c_void,      // slot 9
    pub copy_to: *const c_void,           // slot 10
    pub copy_props: *const c_void,        // slot 11
    pub get_names_from_ids: *const c_void, // slot 12
    pub get_ids_from_names: *const c_void, // slot 13
    // IMsgStore (slots 14+)
    pub advise: *const c_void,            // slot 14
    pub unadvise: *const c_void,          // slot 15
    pub compare_entry_ids: *const c_void, // slot 16
    pub open_entry: unsafe extern "system" fn(
        this: *mut c_void,
        cb_entry_id: u32,
        entry_id: *const u8,
        interface: *const c_void,
        flags: u32,
        obj_type: *mut u32,
        obj: *mut *mut c_void,
    ) -> i32,                             // slot 17
    pub set_receive_folder: *const c_void, // slot 18
    pub get_receive_folder: *const c_void, // slot 19
    pub get_receive_folder_table: *const c_void, // slot 20
    pub store_logoff: *const c_void,      // slot 21
    pub abort_submit: *const c_void,      // slot 22
    pub get_outgoing_queue: *const c_void, // slot 23
    pub set_lock_state: *const c_void,    // slot 24
    pub finished_msg: *const c_void,      // slot 25
    pub notify_new_mail: *const c_void,   // slot 26
}

/// `IMAPIFolder` vtable.
///
/// `IMAPIFolder` inherits from `IMAPIContainer`, which inherits from
/// `IMAPIProp`, which inherits from `IUnknown`.
///
/// Layout:
///   `IUnknown` (0-2), `IMAPIProp` (3-13), `IMAPIContainer` (14-16),
///   `IMAPIFolder` (17+)
#[repr(C)]
pub(crate) struct IMAPIFolderVtbl {
    // IUnknown (slots 0-2)
    pub query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IMAPIProp (slots 3-13)
    pub get_last_error: *const c_void,    // slot 3
    pub save_changes: *const c_void,      // slot 4
    pub get_props: unsafe extern "system" fn(
        this: *mut c_void,
        prop_tag_array: *const SPropTagArray,
        flags: u32,
        count: *mut u32,
        props: *mut *mut SPropValue,
    ) -> i32,                             // slot 5
    pub get_prop_list: *const c_void,     // slot 6
    pub open_property: *const c_void,     // slot 7
    pub set_props: *const c_void,         // slot 8
    pub delete_props: *const c_void,      // slot 9
    pub copy_to: *const c_void,           // slot 10
    pub copy_props: *const c_void,        // slot 11
    pub get_names_from_ids: *const c_void, // slot 12
    pub get_ids_from_names: *const c_void, // slot 13
    // IMAPIContainer (slots 14-16)
    pub get_contents_table: unsafe extern "system" fn(
        this: *mut c_void, flags: u32, table: *mut *mut c_void,
    ) -> i32,                             // slot 14
    pub get_hierarchy_table: unsafe extern "system" fn(
        this: *mut c_void, flags: u32, table: *mut *mut c_void,
    ) -> i32,                             // slot 15
    pub open_entry: unsafe extern "system" fn(
        this: *mut c_void,
        cb_entry_id: u32,
        entry_id: *const u8,
        interface: *const c_void,
        flags: u32,
        obj_type: *mut u32,
        obj: *mut *mut c_void,
    ) -> i32,                             // slot 16
    // IMAPIFolder (slots 17+)
    pub create_message: *const c_void,    // slot 17
    pub copy_messages: unsafe extern "system" fn(
        this: *mut c_void,
        msg_list: *const SBinaryArray,
        interface: *const c_void,
        dest_folder: *mut c_void,
        ui_param: usize,
        progress: *mut c_void,
        flags: u32,
    ) -> i32,                             // slot 18
    pub delete_messages: unsafe extern "system" fn(
        this: *mut c_void,
        msg_list: *const SBinaryArray,
        ui_param: usize,
        progress: *mut c_void,
        flags: u32,
    ) -> i32,                             // slot 19
    pub create_folder: unsafe extern "system" fn(
        this: *mut c_void,
        folder_type: u32,
        name: *const u16,       // Unicode folder name
        comment: *const u16,    // Unicode comment (can be null)
        interface: *const c_void,
        flags: u32,
        folder: *mut *mut c_void,
    ) -> i32,                             // slot 20
    pub delete_folder: *const c_void,     // slot 21
    pub empty_folder: *const c_void,      // slot 22
    pub copy_folder: *const c_void,       // slot 23
    pub move_folder: *const c_void,       // slot 24
}

/// `IMAPITable` vtable — used for hierarchy and contents tables.
#[repr(C)]
pub(crate) struct IMAPITableVtbl {
    // IUnknown (slots 0-2)
    pub query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IMAPITable methods
    pub get_last_error: *const c_void,       // slot 3
    pub advise: *const c_void,               // slot 4
    pub unadvise: *const c_void,             // slot 5
    pub get_status: *const c_void,           // slot 6
    pub set_columns: unsafe extern "system" fn(
        this: *mut c_void, prop_tags: *const SPropTagArray, flags: u32,
    ) -> i32,                                // slot 7
    pub query_columns: *const c_void,        // slot 8
    pub get_row_count: unsafe extern "system" fn(
        this: *mut c_void, flags: u32, count: *mut u32,
    ) -> i32,                                // slot 9
    pub seek_row: *const c_void,             // slot 10
    pub seek_row_approx: *const c_void,      // slot 11
    pub query_position: *const c_void,       // slot 12
    pub find_row: *const c_void,             // slot 13
    pub restrict: *const c_void,             // slot 14
    pub create_bookmark: *const c_void,      // slot 15
    pub free_bookmark: *const c_void,        // slot 16
    pub sort_table: *const c_void,           // slot 17
    pub query_sort_order: *const c_void,     // slot 18
    pub query_rows: unsafe extern "system" fn(
        this: *mut c_void, row_count: i32, flags: u32, rows: *mut *mut SRowSet,
    ) -> i32,                                // slot 19
    pub abort: *const c_void,                // slot 20
    pub expand_row: *const c_void,           // slot 21
    pub collapse_row: *const c_void,         // slot 22
    pub wait_for_completion: *const c_void,  // slot 23
    pub get_collapse_state: *const c_void,   // slot 24
    pub set_collapse_state: *const c_void,   // slot 25
}

// ─── COM Interface Object Wrappers ──────────────────────────────────────────

/// Raw COM pointer wrapper for `IMsgStore` (extended).
#[repr(C)]
pub(crate) struct IMsgStoreExtObj {
    pub vtbl: *const IMsgStoreExtVtbl,
}

/// Raw COM pointer wrapper for `IMAPIFolder`.
#[repr(C)]
pub(crate) struct IMAPIFolderObj {
    pub vtbl: *const IMAPIFolderVtbl,
}

/// Raw COM pointer wrapper for `IMAPITable`.
#[repr(C)]
pub(crate) struct IMAPITableObj {
    pub vtbl: *const IMAPITableVtbl,
}

// ─── MAPI FFI Bindings ──────────────────────────────────────────────────────

#[link(name = "mapi32")]
extern "system" {
    /// Free a MAPI buffer allocated by the MAPI subsystem.
    pub(crate) fn MAPIFreeBuffer(pv: *mut c_void) -> i32;
}

// ─── Helper Functions ────────────────────────────────────────────────────────

/// HRESULT success code.
pub(crate) const S_OK: i32 = 0;

/// Property type constants.
pub(crate) const PT_BINARY: u32 = 0x0102;
pub(crate) const PT_LONG: u32 = 0x0003;
pub(crate) const PT_STRING8: u32 = 0x001E;
pub(crate) const PT_UNICODE: u32 = 0x001F;

/// Convert an HRESULT error code into a `MsgStoreError`, mapping known
/// codes to specific variants.
///
/// # Error Mapping
///
/// | HRESULT | Error |
/// |---------|-------|
/// | MAPI_E_NOT_FOUND, MAPI_E_OBJECT_DELETED | NotFound |
/// | MAPI_E_NO_ACCESS | ReadOnly |
/// | MAPI_E_NETWORK_ERROR, MAPI_E_FAILONEPROVIDER | ProviderUnavailable |
/// | MAPI_E_OBJECT_CHANGED | ObjectChanged |
/// | Other | Mapi (generic) |
pub(crate) fn map_mapi_error(hr: i32, context: &str) -> MsgStoreError {
    match hr {
        MAPI_E_NOT_FOUND | MAPI_E_OBJECT_DELETED => {
            MsgStoreError::NotFound(format!("{} (HRESULT 0x{:08X})", context, hr as u32))
        }
        MAPI_E_NO_ACCESS => {
            MsgStoreError::ReadOnly(format!("{} (HRESULT 0x{:08X})", context, hr as u32))
        }
        MAPI_E_NETWORK_ERROR | MAPI_E_FAILONEPROVIDER => {
            MsgStoreError::ProviderUnavailable(format!(
                "{} (HRESULT 0x{:08X})",
                context, hr as u32
            ))
        }
        MAPI_E_OBJECT_CHANGED => MsgStoreError::ObjectChanged,
        _ => MsgStoreError::Mapi {
            hr,
            message: format!("{context} failed"),
        },
    }
}

/// Release a COM object pointer if non-null.
///
/// # Safety
///
/// The pointer must be a valid COM object or null.
pub(crate) unsafe fn release_com(ptr: *mut c_void) {
    if !ptr.is_null() {
        let unknown = ptr.cast::<*const IUnknownVtbl>();
        ((*(*unknown)).release)(ptr);
    }
}

/// Read a binary property value from an `SPropValue`.
///
/// # Safety
///
/// The prop pointer must be valid and the property type must be `PT_BINARY`.
pub(crate) unsafe fn read_binary_prop(prop: *const SPropValue) -> Vec<u8> {
    let bin = (*prop).value.bin;
    if bin.lpb.is_null() || bin.cb == 0 {
        return Vec::new();
    }
    std::slice::from_raw_parts(bin.lpb, bin.cb as usize).to_vec()
}

/// Read a string (`PT_STRING8`) property value from an `SPropValue`.
///
/// # Safety
///
/// The prop pointer must be valid and the property type must be `PT_STRING8`.
pub(crate) unsafe fn read_string8_prop(prop: *const SPropValue) -> String {
    let ptr = (*prop).value.lpszA;
    if ptr.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(ptr as *const i8)
        .to_string_lossy()
        .into_owned()
}

/// Read a wide string (`PT_UNICODE`) property value from an `SPropValue`.
///
/// # Safety
///
/// The prop pointer must be valid and the property type must be `PT_UNICODE`.
pub(crate) unsafe fn read_unicode_prop(prop: *const SPropValue) -> String {
    let ptr = (*prop).value.lpszW;
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

/// Read a display name property, preferring Unicode over ANSI.
///
/// # Safety
///
/// The prop pointer must be valid.
pub(crate) unsafe fn read_display_name(prop: *const SPropValue) -> String {
    let prop_type = (*prop).ul_prop_tag & 0xFFFF;
    match prop_type {
        PT_UNICODE => read_unicode_prop(prop),
        PT_STRING8 => read_string8_prop(prop),
        _ => String::from("<unknown>"),
    }
}
