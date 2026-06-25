//! MAPI session management.
//!
//! Provides safe wrappers around the raw MAPI/COM APIs for session
//! initialization, logon, profile name retrieval, and message store
//! enumeration.
//!
//! # Architecture
//!
//! MAPI APIs are not available directly in the `windows` crate, so we
//! define raw FFI bindings to MAPI32.dll functions (`MAPIInitialize`,
//! `MAPILogonEx`, `MAPIUninitialize`) and the required COM interface vtables
//! (`IMAPISession`, `IMAPITable`, `IMsgStore`).
//!
//! # Requirements
//!
//! - Req 1.3: Initialize MAPI session during `OnConnection`
//! - Req 1.7: Set `LC_NUMERIC` locale to "C" after MAPI initialization
//! - Req 2.13: Retrieve MAPI profile name for per-profile configuration

#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::ptr;

use crate::errors::MsgStoreError;

// ─── MAPI Constants ──────────────────────────────────────────────────────────

/// MAPI logon flags.
const MAPI_NO_MAIL: u32 = 0x00000008;
const MAPI_EXTENDED: u32 = 0x00000020;
const MAPI_USE_DEFAULT: u32 = 0x00000040;

/// MAPI initialization flags.
const MAPI_MULTITHREAD_NOTIFICATIONS: u32 = 0x00000001;

/// MAPI table flags.
const MAPI_DEFERRED_ERRORS: u32 = 0x00000008;

/// Message store access flags.
const MDB_WRITE: u32 = 0x00000004;
const MDB_NO_MAIL: u32 = 0x00000080;

/// MAPI property tags.
const PR_ENTRYID: u32 = 0x0FFF_0102; // PT_BINARY
const PR_DEFAULT_STORE: u32 = 0x3400_000B; // PT_BOOLEAN
const PR_DISPLAY_NAME_A: u32 = 0x3001_001E; // PT_STRING8
const PR_DISPLAY_NAME_W: u32 = 0x3001_001F; // PT_UNICODE
const PR_RESOURCE_TYPE: u32 = 0x3E03_0003; // PT_LONG

/// MAPI resource type for the subsystem row in status table.
const MAPI_SUBSYSTEM: u32 = 39;

/// MAPI restriction types.
const RES_PROPERTY: u32 = 4;
/// Relational operator: equality.
const RELOP_EQ: u32 = 1;

/// HRESULT success code.
const S_OK: i32 = 0;

/// Property type constants.
const PT_BINARY: u32 = 0x0102;
const PT_BOOLEAN: u32 = 0x000B;
const PT_LONG: u32 = 0x0003;
const PT_STRING8: u32 = 0x001E;
const PT_UNICODE: u32 = 0x001F;

/// `LC_NUMERIC` locale category (from C runtime).
const LC_NUMERIC: i32 = 1;

// ─── MAPI Property Value Structures ─────────────────────────────────────────

/// MAPI binary property value (`SBinary`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SBinary {
    cb: u32,
    lpb: *mut u8,
}

/// A single MAPI property value (`SPropValue`).
///
/// In MAPI, `SPropValue` contains a union for the actual value. We model
/// this as a struct with the tag and a large enough union field.
#[repr(C)]
#[derive(Clone, Copy)]
struct SPropValue {
    ul_prop_tag: u32,
    dw_align_pad: u32,
    value: PropValueUnion,
}

/// Union for `SPropValue` data. Sized to hold the largest variant (`SBinary`).
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(non_snake_case)]
union PropValueUnion {
    b: i16,       // PT_BOOLEAN
    l: i32,       // PT_LONG
    lpszA: *mut u8,  // PT_STRING8
    lpszW: *mut u16, // PT_UNICODE
    bin: SBinary, // PT_BINARY
    raw: [u8; 16], // Ensure union is large enough
}

/// A row from a MAPI table (`SRow`) — array of property values.
#[repr(C)]
struct SRow {
    ul_adrentry_pad: u32,
    c_values: u32,
    lp_props: *mut SPropValue,
}

/// Row set returned by table queries (`SRowSet`).
#[repr(C)]
struct SRowSet {
    c_rows: u32,
    a_row: [SRow; 1], // Variable-length array (C-style)
}

/// MAPI property tag array (`SPropTagArray`).
#[repr(C)]
struct SPropTagArray {
    c_values: u32,
    aul_prop_tag: [u32; 1], // Variable-length array
}

/// MAPI restriction structure (`SRestriction`).
/// We only model the property restriction variant needed for our queries.
#[repr(C)]
struct SRestriction {
    rt: u32,
    res: SRestrictionUnion,
}

/// Restriction union — we only use the property restriction variant.
#[repr(C)]
#[derive(Clone, Copy)]
struct SPropertyRestriction {
    rel_op: u32,
    ul_prop_tag: u32,
    lp_prop: *mut SPropValue,
}

#[repr(C)]
union SRestrictionUnion {
    res_property: SPropertyRestriction,
    raw: [u8; 24], // Ensure union is large enough
}

// ─── COM Interface VTable Definitions ────────────────────────────────────────

/// `IUnknown` vtable (base for all COM interfaces).
#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const [u8; 16],
        ppv: *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

/// `IMAPISession` vtable.
///
/// We only define the methods we actually call. The vtable slots are
/// laid out in order, so we pad with `*const c_void` for unused entries.
#[repr(C)]
struct IMAPISessionVtbl {
    // IUnknown (slots 0-2)
    query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IMAPISession methods (slots 3+)
    get_last_error: *const c_void,       // slot 3
    get_msg_stores_table: unsafe extern "system" fn(
        this: *mut c_void, flags: u32, table: *mut *mut c_void,
    ) -> i32,                            // slot 4
    open_msg_store: unsafe extern "system" fn(
        this: *mut c_void,
        ui_param: usize,
        cb_entry_id: u32,
        entry_id: *const u8,
        interface: *const c_void,
        flags: u32,
        msg_store: *mut *mut c_void,
    ) -> i32,                            // slot 5
    open_address_book: *const c_void,    // slot 6
    open_profile_section: *const c_void, // slot 7
    get_status_table: unsafe extern "system" fn(
        this: *mut c_void, flags: u32, table: *mut *mut c_void,
    ) -> i32,                            // slot 8
    open_entry: *const c_void,           // slot 9
    compare_entry_ids: *const c_void,    // slot 10
    advise: *const c_void,               // slot 11
    unadvise: *const c_void,             // slot 12
    _padding_13: *const c_void,          // slot 13 (MessageOptions)
    _padding_14: *const c_void,          // slot 14 (QueryDefaultMessageOpt)
    _padding_15: *const c_void,          // slot 15 (EnumAdrTypes)
    _padding_16: *const c_void,          // slot 16 (QueryIdentity)
    logoff: unsafe extern "system" fn(
        this: *mut c_void, ui_param: usize, flags: u32, reserved: u32,
    ) -> i32,                            // slot 17
}

/// `IMAPITable` vtable.
///
/// Used for querying message store tables and status tables.
#[repr(C)]
struct IMAPITableVtbl {
    // IUnknown (slots 0-2)
    query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IMAPITable methods
    get_last_error: *const c_void,       // slot 3
    advise: *const c_void,               // slot 4
    unadvise: *const c_void,             // slot 5
    get_status: *const c_void,           // slot 6
    set_columns: unsafe extern "system" fn(
        this: *mut c_void, prop_tags: *const SPropTagArray, flags: u32,
    ) -> i32,                            // slot 7
    query_columns: *const c_void,        // slot 8
    get_row_count: *const c_void,        // slot 9
    seek_row: *const c_void,             // slot 10
    seek_row_approx: *const c_void,      // slot 11
    query_position: *const c_void,       // slot 12
    find_row: *const c_void,             // slot 13
    restrict: unsafe extern "system" fn(
        this: *mut c_void, restriction: *const SRestriction, flags: u32,
    ) -> i32,                            // slot 14
    create_bookmark: *const c_void,      // slot 15
    free_bookmark: *const c_void,        // slot 16
    sort_table: *const c_void,           // slot 17
    query_sort_order: *const c_void,     // slot 18
    query_rows: unsafe extern "system" fn(
        this: *mut c_void, row_count: i32, flags: u32, rows: *mut *mut SRowSet,
    ) -> i32,                            // slot 19
    abort: *const c_void,                // slot 20
    expand_row: *const c_void,           // slot 21
    collapse_row: *const c_void,         // slot 22
    wait_for_completion: *const c_void,  // slot 23
    get_collapse_state: *const c_void,   // slot 24
    set_collapse_state: *const c_void,   // slot 25
}

/// `IMsgStore` vtable — minimal definition for store access.
#[repr(C)]
struct IMsgStoreVtbl {
    // IUnknown (slots 0-2)
    query_interface: unsafe extern "system" fn(
        this: *mut c_void, riid: *const [u8; 16], ppv: *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

// ─── COM Interface Wrappers ──────────────────────────────────────────────────

/// Raw COM pointer wrapper for `IMAPISession`.
#[repr(C)]
struct IMAPISessionObj {
    vtbl: *const IMAPISessionVtbl,
}

/// Raw COM pointer wrapper for `IMAPITable`.
#[repr(C)]
struct IMAPITableObj {
    vtbl: *const IMAPITableVtbl,
}

/// Raw COM pointer wrapper for `IMsgStore`.
#[repr(C)]
struct IMsgStoreObj {
    vtbl: *const IMsgStoreVtbl,
}

// ─── MAPI32.dll FFI Bindings ─────────────────────────────────────────────────

#[link(name = "mapi32")]
extern "system" {
    /// Initialize the MAPI subsystem.
    fn MAPIInitialize(lp_map_init: *const c_void) -> i32;

    /// Log on to a MAPI session.
    fn MAPILogonEx(
        ui_param: usize,
        profile_name: *const u8,
        password: *const u8,
        flags: u32,
        session: *mut *mut c_void,
    ) -> i32;

    /// Shut down the MAPI subsystem.
    fn MAPIUninitialize();

    /// Free a MAPI buffer allocated by the MAPI subsystem.
    fn MAPIFreeBuffer(pv: *mut c_void) -> i32;
}

// C runtime locale function.
extern "C" {
    fn setlocale(category: i32, locale: *const u8) -> *const u8;
}

// ─── Helper Functions ────────────────────────────────────────────────────────

/// Convert an HRESULT error code into a `MsgStoreError::Mapi`.
fn mapi_error(hr: i32, context: &str) -> MsgStoreError {
    MsgStoreError::Mapi {
        hr,
        message: format!("{context} failed"),
    }
}

/// Release a COM object pointer if non-null.
///
/// # Safety
///
/// The pointer must be a valid COM object or null.
unsafe fn release_com(ptr: *mut c_void) {
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
unsafe fn read_binary_prop(prop: *const SPropValue) -> Vec<u8> {
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
unsafe fn read_string8_prop(prop: *const SPropValue) -> String {
    let ptr = (*prop).value.lpszA;
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr as *const i8)
        .to_string_lossy()
        .into_owned()
}

/// Read a wide string (`PT_UNICODE`) property value from an `SPropValue`.
///
/// # Safety
///
/// The prop pointer must be valid and the property type must be `PT_UNICODE`.
unsafe fn read_unicode_prop(prop: *const SPropValue) -> String {
    let ptr = (*prop).value.lpszW;
    if ptr.is_null() {
        return String::new();
    }
    // Find null terminator
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

// ─── Store Information ───────────────────────────────────────────────────────

/// Information about a message store discovered during enumeration.
#[derive(Debug, Clone)]
pub struct StoreInfo {
    /// Raw binary entry ID for this store.
    pub entry_id: Vec<u8>,
    /// Display name of the store (e.g., "Personal Folders").
    pub display_name: String,
    /// Whether this is the default message store.
    pub is_default: bool,
}

// ─── MapiSession ─────────────────────────────────────────────────────────────

/// Manages a MAPI session lifecycle: initialization, logon, store
/// enumeration, and profile name retrieval.
///
/// # Safety
///
/// This struct holds raw COM pointers to MAPI objects. It must only be
/// used from the COM apartment thread that created it. The `Drop`
/// implementation properly releases all resources.
///
/// # Example
///
/// ```no_run
/// use spambayes_mapi::session::MapiSession;
///
/// let session = MapiSession::initialize_and_logon()?;
/// let profile = session.get_profile_name()?;
/// let stores = session.enumerate_stores()?;
/// ```
pub struct MapiSession {
    /// Pointer to the underlying `IMAPISession` COM object.
    session: *mut c_void,
    /// Cache of opened message stores (`entry_id` -> `IMsgStore` pointer).
    stores: HashMap<Vec<u8>, *mut c_void>,
    /// Entry ID of the default message store for this profile.
    default_store_eid: Option<Vec<u8>>,
    /// Whether `MAPIInitialize` was called (controls `MAPIUninitialize` on drop).
    initialized: bool,
}

// SAFETY: MapiSession is used only from the COM apartment thread that
// created it. The raw pointers are not shared across threads.
unsafe impl Send for MapiSession {}

impl std::fmt::Debug for MapiSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapiSession")
            .field("session", &(!self.session.is_null()))
            .field("stores_count", &self.stores.len())
            .field("default_store_eid", &self.default_store_eid.as_ref().map(std::vec::Vec::len))
            .field("initialized", &self.initialized)
            .finish()
    }
}

impl MapiSession {
    /// Initialize MAPI and log on to a session using the default profile.
    ///
    /// This performs the full initialization sequence:
    /// 1. Call `MAPIInitialize` to start the MAPI subsystem
    /// 2. Call `MAPILogonEx` with `MAPI_USE_DEFAULT` to log on
    /// 3. Set `LC_NUMERIC` locale to "C" (Requirement 1.7)
    ///
    /// # Errors
    ///
    /// Returns `MsgStoreError::Mapi` if MAPI initialization or logon fails.
    ///
    /// # Requirements
    ///
    /// - Req 1.3: Initialize MAPI session during `OnConnection`
    /// - Req 1.7: Set `LC_NUMERIC` to "C" after initialization
    pub fn initialize_and_logon() -> Result<Self, MsgStoreError> {
        // Step 1: Initialize MAPI subsystem
        let hr = unsafe { MAPIInitialize(ptr::null()) };
        if hr != S_OK {
            return Err(mapi_error(hr, "MAPIInitialize"));
        }

        // Step 2: Log on with default profile (no UI)
        let flags = MAPI_NO_MAIL | MAPI_EXTENDED | MAPI_USE_DEFAULT;
        let mut session_ptr: *mut c_void = ptr::null_mut();

        let hr = unsafe {
            MAPILogonEx(
                0,                       // no parent window
                ptr::null(),             // no profile name (use default)
                ptr::null(),             // no password
                flags,
                &raw mut session_ptr,
            )
        };

        if hr != S_OK || session_ptr.is_null() {
            // Clean up: uninitialize since we already initialized
            unsafe { MAPIUninitialize() };
            return Err(mapi_error(hr, "MAPILogonEx"));
        }

        // Step 3: Set LC_NUMERIC locale to "C" (Requirement 1.7).
        //
        // MAPILogonEx may change the CRT locale away from "C". The
        // classifier and config parser rely on "C" locale for
        // consistent floating-point formatting (e.g., "0.5" not "0,5").
        #[allow(clippy::manual_c_str_literals)] // setlocale declared with *const u8, not CStr
        unsafe {
            setlocale(LC_NUMERIC, b"C\0".as_ptr());
        }

        Ok(Self {
            session: session_ptr,
            stores: HashMap::new(),
            default_store_eid: None,
            initialized: true,
        })
    }

    /// Returns the raw MAPI session pointer.
    ///
    /// # Safety
    ///
    /// The caller must not release or invalidate this pointer. It
    /// remains owned by this `MapiSession` instance.
    #[must_use]
    pub fn session_ptr(&self) -> *mut c_void {
        self.session
    }

    /// Returns the entry ID of the default message store, if known.
    ///
    /// This is populated after calling [`enumerate_stores`] or
    /// [`open_default_store`].
    #[must_use]
    pub fn default_store_eid(&self) -> Option<&[u8]> {
        self.default_store_eid.as_deref()
    }

    /// Retrieve the MAPI profile name for the current session.
    ///
    /// Queries the MAPI status table for the `MAPI_SUBSYSTEM` row and
    /// reads its `PR_DISPLAY_NAME` property, which contains the profile
    /// name.
    ///
    /// # Errors
    ///
    /// Returns `MsgStoreError::Mapi` if the status table cannot be
    /// queried or the profile name row is not found.
    ///
    /// # Requirement
    ///
    /// - Req 2.13: Retrieve MAPI profile name for per-profile configuration
    pub fn get_profile_name(&self) -> Result<String, MsgStoreError> {
        if self.session.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Session not initialized".to_string(),
            });
        }

        unsafe {
            let session_obj = self.session.cast::<IMAPISessionObj>();
            let vtbl = &*(*session_obj).vtbl;

            // Get the status table
            let mut table_ptr: *mut c_void = ptr::null_mut();
            let hr = (vtbl.get_status_table)(self.session, 0, &raw mut table_ptr);
            if hr != S_OK || table_ptr.is_null() {
                return Err(mapi_error(hr, "GetStatusTable"));
            }

            // Set up restriction: PR_RESOURCE_TYPE == MAPI_SUBSYSTEM
            let mut prop_value = SPropValue {
                ul_prop_tag: PR_RESOURCE_TYPE,
                dw_align_pad: 0,
                value: PropValueUnion { l: MAPI_SUBSYSTEM as i32 },
            };

            let restriction = SRestriction {
                rt: RES_PROPERTY,
                res: SRestrictionUnion {
                    res_property: SPropertyRestriction {
                        rel_op: RELOP_EQ,
                        ul_prop_tag: PR_RESOURCE_TYPE,
                        lp_prop: &raw mut prop_value,
                    },
                },
            };

            let table_obj = table_ptr.cast::<IMAPITableObj>();
            let table_vtbl = &*(*table_obj).vtbl;

            // Apply the restriction to filter the table
            let hr = (table_vtbl.restrict)(table_ptr, &raw const restriction, 0);
            if hr != S_OK {
                release_com(table_ptr);
                return Err(mapi_error(hr, "IMAPITable::Restrict"));
            }

            // Set columns to retrieve PR_DISPLAY_NAME_A
            #[repr(C)]
            struct PropTagArray1 {
                c_values: u32,
                tags: [u32; 1],
            }
            let columns = PropTagArray1 {
                c_values: 1,
                tags: [PR_DISPLAY_NAME_A],
            };

            let hr = (table_vtbl.set_columns)(
                table_ptr,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
            );
            if hr != S_OK {
                release_com(table_ptr);
                return Err(mapi_error(hr, "IMAPITable::SetColumns"));
            }

            // Query one row
            let mut row_set: *mut SRowSet = ptr::null_mut();
            let hr = (table_vtbl.query_rows)(table_ptr, 1, 0, &raw mut row_set);
            if hr != S_OK || row_set.is_null() {
                release_com(table_ptr);
                return Err(mapi_error(hr, "IMAPITable::QueryRows"));
            }

            let result = if (*row_set).c_rows >= 1 {
                let row = &(*row_set).a_row[0];
                if row.c_values >= 1 && !row.lp_props.is_null() {
                    let prop = &*row.lp_props;
                    let prop_type = prop.ul_prop_tag & 0xFFFF;
                    match prop_type {
                        PT_STRING8 => Ok(read_string8_prop(row.lp_props)),
                        PT_UNICODE => Ok(read_unicode_prop(row.lp_props)),
                        _ => Err(MsgStoreError::Mapi {
                            hr: -1,
                            message: "Profile name has unexpected type".to_string(),
                        }),
                    }
                } else {
                    Err(MsgStoreError::Mapi {
                        hr: -1,
                        message: "No properties in status table row".to_string(),
                    })
                }
            } else {
                Err(MsgStoreError::Mapi {
                    hr: -1,
                    message: "No MAPI_SUBSYSTEM row in status table".to_string(),
                })
            };

            // Free the row set and release the table
            MAPIFreeBuffer(row_set.cast::<c_void>());
            release_com(table_ptr);

            result
        }
    }

    /// Enumerate all available message stores in the session.
    ///
    /// Queries the MAPI message stores table for all stores, returning
    /// their entry IDs, display names, and whether each is the default.
    /// Also caches the default store entry ID internally.
    ///
    /// # Errors
    ///
    /// Returns `MsgStoreError::Mapi` if the stores table cannot be
    /// queried.
    pub fn enumerate_stores(&mut self) -> Result<Vec<StoreInfo>, MsgStoreError> {
        if self.session.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Session not initialized".to_string(),
            });
        }

        unsafe {
            let session_obj = self.session.cast::<IMAPISessionObj>();
            let vtbl = &*(*session_obj).vtbl;

            // Get the message stores table
            let mut table_ptr: *mut c_void = ptr::null_mut();
            let hr = (vtbl.get_msg_stores_table)(
                self.session,
                MAPI_DEFERRED_ERRORS,
                &raw mut table_ptr,
            );
            if hr != S_OK || table_ptr.is_null() {
                return Err(mapi_error(hr, "GetMsgStoresTable"));
            }

            let table_obj = table_ptr.cast::<IMAPITableObj>();
            let table_vtbl = &*(*table_obj).vtbl;

            // Set columns: PR_ENTRYID, PR_DISPLAY_NAME_W, PR_DEFAULT_STORE
            #[repr(C)]
            struct PropTagArray3 {
                c_values: u32,
                tags: [u32; 3],
            }
            let columns = PropTagArray3 {
                c_values: 3,
                tags: [PR_ENTRYID, PR_DISPLAY_NAME_W, PR_DEFAULT_STORE],
            };

            let hr = (table_vtbl.set_columns)(
                table_ptr,
                (&raw const columns).cast::<SPropTagArray>(),
                0,
            );
            if hr != S_OK {
                release_com(table_ptr);
                return Err(mapi_error(hr, "IMAPITable::SetColumns (stores)"));
            }

            // Query all rows (request up to 100 stores)
            let mut row_set: *mut SRowSet = ptr::null_mut();
            let hr = (table_vtbl.query_rows)(table_ptr, 100, 0, &raw mut row_set);
            if hr != S_OK || row_set.is_null() {
                release_com(table_ptr);
                return Err(mapi_error(hr, "IMAPITable::QueryRows (stores)"));
            }

            let mut stores = Vec::new();
            let num_rows = (*row_set).c_rows;

            for i in 0..num_rows {
                // Access rows via pointer arithmetic since SRowSet uses
                // a flexible array member pattern.
                let row_ptr = (row_set as *const u8)
                    .add(std::mem::offset_of!(SRowSet, a_row))
                    .add(i as usize * std::mem::size_of::<SRow>()).cast::<SRow>();
                let row = &*row_ptr;

                if row.c_values < 3 || row.lp_props.is_null() {
                    continue;
                }

                let props = row.lp_props;

                // Column 0: PR_ENTRYID (PT_BINARY)
                let entry_id = read_binary_prop(props.add(0));

                // Column 1: PR_DISPLAY_NAME_W (PT_UNICODE)
                let display_name = {
                    let prop = &*props.add(1);
                    let prop_type = prop.ul_prop_tag & 0xFFFF;
                    match prop_type {
                        PT_UNICODE => read_unicode_prop(props.add(1)),
                        PT_STRING8 => read_string8_prop(props.add(1)),
                        _ => String::from("<unknown>"),
                    }
                };

                // Column 2: PR_DEFAULT_STORE (PT_BOOLEAN)
                let is_default = {
                    let prop = &*props.add(2);
                    let prop_type = prop.ul_prop_tag & 0xFFFF;
                    if prop_type == PT_BOOLEAN {
                        prop.value.b != 0
                    } else {
                        false
                    }
                };

                if is_default {
                    self.default_store_eid = Some(entry_id.clone());
                }

                stores.push(StoreInfo {
                    entry_id,
                    display_name,
                    is_default,
                });
            }

            // Free row set and release table
            MAPIFreeBuffer(row_set.cast::<c_void>());
            release_com(table_ptr);

            Ok(stores)
        }
    }

    /// Open a message store by its entry ID.
    ///
    /// Returns the raw `IMsgStore` pointer. The store is cached internally
    /// so subsequent calls with the same entry ID return the cached
    /// pointer without re-opening.
    ///
    /// # Errors
    ///
    /// Returns `MsgStoreError::Mapi` if the store cannot be opened.
    pub fn open_store(&mut self, store_eid: &[u8]) -> Result<*mut c_void, MsgStoreError> {
        // Check cache first
        if let Some(&store_ptr) = self.stores.get(store_eid) {
            return Ok(store_ptr);
        }

        if self.session.is_null() {
            return Err(MsgStoreError::Mapi {
                hr: -1,
                message: "Session not initialized".to_string(),
            });
        }

        unsafe {
            let session_obj = self.session.cast::<IMAPISessionObj>();
            let vtbl = &*(*session_obj).vtbl;

            let mut store_ptr: *mut c_void = ptr::null_mut();
            let flags = MDB_WRITE | MDB_NO_MAIL | MAPI_DEFERRED_ERRORS;

            let hr = (vtbl.open_msg_store)(
                self.session,
                0,                          // no parent window
                store_eid.len() as u32,     // entry ID size
                store_eid.as_ptr(),         // entry ID bytes
                ptr::null(),                // default IID (IMsgStore)
                flags,
                &raw mut store_ptr,
            );

            if hr != S_OK || store_ptr.is_null() {
                return Err(mapi_error(hr, "OpenMsgStore"));
            }

            // Cache the store pointer
            self.stores.insert(store_eid.to_vec(), store_ptr);
            Ok(store_ptr)
        }
    }

    /// Open the default message store.
    ///
    /// If the default store entry ID is not yet known, this enumerates
    /// stores first to discover it.
    ///
    /// # Errors
    ///
    /// Returns `MsgStoreError::Mapi` if the default store cannot be
    /// found or opened.
    pub fn open_default_store(&mut self) -> Result<*mut c_void, MsgStoreError> {
        if self.default_store_eid.is_none() {
            self.enumerate_stores()?;
        }

        let eid = self.default_store_eid.clone().ok_or_else(|| MsgStoreError::Mapi {
            hr: -1,
            message: "No default message store found".to_string(),
        })?;

        self.open_store(&eid)
    }

    /// Log off the session and release the session handle.
    ///
    /// This is called automatically by `Drop`, but can be invoked
    /// manually for explicit lifecycle control.
    pub fn logoff(&mut self) {
        if !self.session.is_null() {
            unsafe {
                let session_obj = self.session.cast::<IMAPISessionObj>();
                let vtbl = &*(*session_obj).vtbl;
                let _ = (vtbl.logoff)(self.session, 0, 0, 0);
            }
        }
    }
}

// ─── Drop Implementation ─────────────────────────────────────────────────────

impl Drop for MapiSession {
    /// Cleans up the MAPI session on drop:
    /// 1. Release all cached message store COM pointers
    /// 2. Log off the session
    /// 3. Release the session COM pointer
    /// 4. Call `MAPIUninitialize`
    fn drop(&mut self) {
        unsafe {
            // Release all cached store pointers
            for (_eid, store_ptr) in self.stores.drain() {
                release_com(store_ptr);
            }

            // Log off and release session
            if !self.session.is_null() {
                let session_obj = self.session.cast::<IMAPISessionObj>();
                let vtbl = &*(*session_obj).vtbl;
                let _ = (vtbl.logoff)(self.session, 0, 0, 0);
                release_com(self.session);
                self.session = ptr::null_mut();
            }

            // Uninitialize MAPI subsystem
            if self.initialized {
                MAPIUninitialize();
                self.initialized = false;
            }
        }
    }
}
