//! Folder event sink for monitoring watched folders.
//!
//! Implements a COM `IDispatch` sink that connects to Outlook's
//! `Items.ItemAdd` event on each configured watch folder. When a new
//! message arrives in a watched folder, the sink invokes the filter engine
//! to classify and move the message.
//!
//! This is the Rust equivalent of the Python `HamFolderItemsEvent` class
//! in `Outlook2000/addin.py`.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{GUID, HRESULT, PCWSTR};

use crate::com_invoke::{dispatch_get, dispatch_get_string, VariantArg};
use crate::filter::{FilterEngine};
use crate::notification::NotificationManager;

use spambayes_config::{AppConfig, FolderId, GeneralConfig};
use spambayes_core::Classification;

// ─── Constants ───────────────────────────────────────────────────────────────

/// IID for `ItemsEvents` dispatch source interface.
/// {00063077-0000-0000-C000-000000000046}
const IID_ITEMS_EVENTS: GUID = GUID::from_u128(
    0x00063077_0000_0000_C000_000000000046,
);

/// DISPID for ItemAdd event in Outlook's Items collection.
const DISPID_ITEM_ADD: i32 = 0xF001;

/// IID for IConnectionPointContainer: {B196B284-BAB4-101A-B69C-00AA00341D07}
const IID_ICONNECTION_POINT_CONTAINER: GUID = GUID::from_u128(
    0xB196B284_BAB4_101A_B69C_00AA00341D07,
);

/// IID for IUnknown
const IID_IUNKNOWN: GUID = GUID::from_u128(
    0x00000000_0000_0000_C000_000000000046,
);

/// IID for IDispatch
const IID_IDISPATCH: GUID = GUID::from_u128(
    0x00020400_0000_0000_C000_000000000046,
);

// ─── VARIANT / DISPPARAMS (duplicated from com_invoke for self-containment) ──

#[repr(C)]
#[derive(Clone)]
struct Variant {
    vt: u16,
    _reserved1: u16,
    _reserved2: u16,
    _reserved3: u16,
    data: [u8; 16],
}

impl Default for Variant {
    fn default() -> Self {
        Self {
            vt: 0,
            _reserved1: 0,
            _reserved2: 0,
            _reserved3: 0,
            data: [0u8; 16],
        }
    }
}

#[repr(C)]
struct DispParams {
    rgvarg: *mut Variant,
    rgdispid_named_args: *mut i32,
    c_args: u32,
    c_named_args: u32,
}

const VT_DISPATCH: u16 = 9;

// ─── IDispatch VTable (for sink) ─────────────────────────────────────────────

#[repr(C)]
struct IDispatchVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_type_info_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    get_type_info: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    get_ids_of_names: unsafe extern "system" fn(*mut c_void, *const GUID, *mut PCWSTR, u32, u32, *mut i32) -> HRESULT,
    invoke: unsafe extern "system" fn(*mut c_void, i32, *const GUID, u32, u16, *mut DispParams, *mut Variant, *mut c_void, *mut u32) -> HRESULT,
}

// ─── Connection Point VTables ────────────────────────────────────────────────

#[repr(C)]
struct IConnectionPointContainerVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    enum_connection_points: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    find_connection_point: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IConnectionPointVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_connection_interface: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    get_connection_point_container: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    advise: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u32) -> HRESULT,
    unadvise: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    enum_connections: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

// ─── Shared State for Folder Hooks ───────────────────────────────────────────

/// Shared state accessible by all folder event sinks.
///
/// This is stored in an `Arc<Mutex<_>>` and passed to each `FolderItemsSink`.
/// When an `ItemAdd` event fires, the sink locks this state to access the
/// filter engine, config, and notification manager.
pub struct FolderHookState {
    /// The filter engine for scoring messages.
    pub filter_engine: Arc<Mutex<FilterEngine>>,
    /// Application configuration.
    pub config: AppConfig,
    /// Notification manager for sounds/popups.
    pub notification_mgr: Arc<Mutex<NotificationManager>>,
    /// Logger for debug output.
    pub logger: Option<Arc<crate::logger::Logger>>,
}

// ─── FolderHook ──────────────────────────────────────────────────────────────

/// Represents one active folder hook (an advise connection on a folder's Items).
pub struct FolderHook {
    /// The Items IDispatch pointer we're sinking events on.
    items_ptr: *mut c_void,
    /// The connection point pointer (needed for Unadvise).
    connection_point: *mut c_void,
    /// The advise cookie returned by IConnectionPoint::Advise.
    cookie: u32,
    /// The folder name (for logging).
    pub folder_name: String,
}

// SAFETY: FolderHook is only accessed from the COM STA thread.
unsafe impl Send for FolderHook {}

impl FolderHook {
    /// Disconnect the event sink and release COM references.
    pub fn disconnect(&mut self) {
        unsafe {
            if !self.connection_point.is_null() {
                let vtbl = *(self.connection_point as *const *const IConnectionPointVtbl);
                let _ = ((*vtbl).unadvise)(self.connection_point, self.cookie);
                let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute((*vtbl).release);
                release(self.connection_point);
                self.connection_point = ptr::null_mut();
            }
            if !self.items_ptr.is_null() {
                // Release the Items collection
                let vtbl = *(self.items_ptr as *const *const [usize; 3]);
                let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute((*vtbl)[2]);
                release(self.items_ptr);
                self.items_ptr = ptr::null_mut();
            }
        }
    }
}

impl Drop for FolderHook {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ─── FolderItemsSink ─────────────────────────────────────────────────────────

/// COM event sink for Outlook Items.ItemAdd events.
///
/// When Outlook fires ItemAdd (DISPID 0xF001) on a watched folder's Items
/// collection, this sink's Invoke method is called with the new MailItem as
/// a DISPATCH argument. We then extract the message body, score it, and
/// perform the configured filter action.
#[repr(C)]
struct FolderItemsSink {
    vtbl: *const IDispatchVtbl,
    ref_count: AtomicU32,
    /// Shared state for accessing filter engine, config, etc.
    state: Arc<Mutex<FolderHookState>>,
    /// Name of the folder being watched (for logging).
    folder_name: String,
}

// SAFETY: FolderItemsSink is allocated on the heap and accessed via COM
// pointers on the STA thread.
unsafe impl Send for FolderItemsSink {}
unsafe impl Sync for FolderItemsSink {}

static FOLDER_SINK_VTBL: IDispatchVtbl = IDispatchVtbl {
    query_interface: folder_sink_qi,
    add_ref: folder_sink_add_ref,
    release: folder_sink_release,
    get_type_info_count: folder_sink_get_type_info_count,
    get_type_info: folder_sink_get_type_info,
    get_ids_of_names: folder_sink_get_ids_of_names,
    invoke: folder_sink_invoke,
};

// ─── IUnknown / IDispatch implementation for FolderItemsSink ─────────────────

unsafe extern "system" fn folder_sink_qi(
    this: *mut c_void,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return HRESULT(0x80004003_u32 as i32); // E_POINTER
    }
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == IID_IDISPATCH || *iid == IID_ITEMS_EVENTS {
        *ppv = this;
        folder_sink_add_ref(this);
        HRESULT(0)
    } else {
        *ppv = ptr::null_mut();
        HRESULT(0x80004002_u32 as i32) // E_NOINTERFACE
    }
}

unsafe extern "system" fn folder_sink_add_ref(this: *mut c_void) -> u32 {
    let sink = &*(this as *const FolderItemsSink);
    sink.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn folder_sink_release(this: *mut c_void) -> u32 {
    let sink = &*(this as *const FolderItemsSink);
    let prev = sink.ref_count.fetch_sub(1, Ordering::SeqCst);
    let new_count = prev - 1;
    if new_count == 0 {
        let _ = Box::from_raw(this as *mut FolderItemsSink);
    }
    new_count
}

unsafe extern "system" fn folder_sink_get_type_info_count(
    _this: *mut c_void,
    pctinfo: *mut u32,
) -> HRESULT {
    if !pctinfo.is_null() {
        *pctinfo = 0;
    }
    HRESULT(0)
}

unsafe extern "system" fn folder_sink_get_type_info(
    _this: *mut c_void, _: u32, _: u32, _: *mut *mut c_void,
) -> HRESULT {
    HRESULT(0x80004001_u32 as i32) // E_NOTIMPL
}

unsafe extern "system" fn folder_sink_get_ids_of_names(
    _this: *mut c_void, _: *const GUID, _: *mut PCWSTR, _: u32, _: u32, _: *mut i32,
) -> HRESULT {
    HRESULT(0x80020006_u32 as i32) // DISP_E_UNKNOWNNAME
}

/// The main event handler. Outlook calls this when ItemAdd fires.
///
/// For ItemAdd (DISPID 0xF001), the first argument in rgvarg is the
/// IDispatch pointer to the new MailItem.
unsafe extern "system" fn folder_sink_invoke(
    this: *mut c_void,
    disp_id: i32,
    _riid: *const GUID,
    _lcid: u32,
    _flags: u16,
    params: *mut DispParams,
    _result: *mut Variant,
    _excep: *mut c_void,
    _arg_err: *mut u32,
) -> HRESULT {
    if disp_id != DISPID_ITEM_ADD {
        return HRESULT(0); // S_OK — ignore other events
    }

    let sink = &*(this as *const FolderItemsSink);

    // Extract the MailItem IDispatch pointer from the event arguments.
    // ItemAdd passes one argument: the new Item (VT_DISPATCH).
    if params.is_null() {
        return HRESULT(0);
    }
    let dp = &*params;
    if dp.c_args == 0 || dp.rgvarg.is_null() {
        return HRESULT(0);
    }

    let arg = &*dp.rgvarg;
    if arg.vt != VT_DISPATCH {
        return HRESULT(0);
    }

    let mail_item = *(arg.data.as_ptr().cast::<*mut c_void>());
    if mail_item.is_null() {
        return HRESULT(0);
    }

    // Process the new item
    handle_item_add(sink, mail_item);

    HRESULT(0) // S_OK
}

// ─── Message Processing ──────────────────────────────────────────────────────

/// Handle a new item arriving in a watched folder.
///
/// Extracts the message body/headers from the Outlook MailItem, scores it
/// with the filter engine, and performs the configured action (move/copy).
unsafe fn handle_item_add(sink: &FolderItemsSink, mail_item: *mut c_void) {
    let debug_path = debug_log_path();

    // Get message properties for logging
    let subject = dispatch_get_string(mail_item, "Subject")
        .unwrap_or_else(|| "<no subject>".to_string());
    let sender = dispatch_get_string(mail_item, "SenderName")
        .unwrap_or_else(|| "<unknown>".to_string());
    let msg_class = dispatch_get_string(mail_item, "MessageClass")
        .unwrap_or_else(|| "IPM.Note".to_string());

    log_debug(&debug_path, &format!(
        "========== ItemAdd FIRED in '{}' ==========", sink.folder_name
    ));
    log_debug(&debug_path, &format!("  Subject: {}", subject));
    log_debug(&debug_path, &format!("  Sender: {}", sender));
    log_debug(&debug_path, &format!("  MessageClass: {}", msg_class));

    // Lock shared state — extract what we need quickly, then release
    let (filter_enabled, save_spam_info, general_config, filter_config) = {
        let state = match sink.state.lock() {
            Ok(s) => s,
            Err(_) => {
                log_debug(&debug_path, "ERROR: FolderHookState lock poisoned");
                return;
            }
        };
        (
            state.config.filter.enabled,
            state.config.filter.save_spam_info,
            state.config.general.clone(),
            state.config.filter.clone(),
        )
    };

    // Check if filtering is enabled
    if !filter_enabled {
        log_debug(&debug_path, "Filter disabled, ignoring message");
        return;
    }

    // Get the raw message content via MailItem MIME content.
    // Use PropertyAccessor to get the PR_TRANSPORT_MESSAGE_HEADERS + Body,
    // or use the MIME content property.
    let mime_content = get_mime_content(mail_item);
    let message_bytes = match mime_content {
        Some(bytes) if !bytes.is_empty() => {
            log_debug(&debug_path, &format!(
                "  Content source: MIME ({} bytes)", bytes.len()
            ));
            bytes
        }
        _ => {
            // Fallback: construct pseudo-message from Subject + Body
            let body = dispatch_get_string(mail_item, "Body")
                .unwrap_or_default();
            let headers = get_transport_headers(mail_item)
                .unwrap_or_default();
            if headers.is_empty() && body.is_empty() {
                log_debug(&debug_path, "  No content available, skipping");
                return;
            }
            let mut content = String::new();
            if !headers.is_empty() {
                content.push_str(&headers);
                content.push_str("\r\n\r\n");
                log_debug(&debug_path, &format!(
                    "  Content source: Headers ({} bytes) + Body ({} bytes)",
                    headers.len(), body.len()
                ));
            } else {
                content.push_str(&format!("Subject: {}\r\n\r\n", subject));
                log_debug(&debug_path, &format!(
                    "  Content source: Subject + Body ({} bytes)", body.len()
                ));
            }
            content.push_str(&body);
            content.into_bytes()
        }
    };

    // Lock filter engine (briefly) to classify
    let result = {
        let state = match sink.state.lock() {
            Ok(s) => s,
            Err(_) => {
                log_debug(&debug_path, "ERROR: FolderHookState lock poisoned");
                return;
            }
        };
        let filter_engine = match state.filter_engine.lock() {
            Ok(fe) => fe,
            Err(_) => {
                log_debug(&debug_path, "ERROR: FilterEngine lock poisoned");
                return;
            }
        };
        match filter_engine.classify_raw(&message_bytes) {
            Ok(r) => r,
            Err(e) => {
                log_debug(&debug_path, &format!("Classify error: {:?}", e));
                return;
            }
        }
    };

    let classification_name = match result.classification {
        Classification::Spam => "Spam",
        Classification::Unsure => "Unsure",
        Classification::Ham => "Ham",
    };

    log_debug(&debug_path, &format!(
        "  *** CLASSIFICATION: {} ***", classification_name
    ));
    log_debug(&debug_path, &format!(
        "  Score: {:.2}%", result.score_pct
    ));
    log_debug(&debug_path, &format!(
        "  Spam threshold: {:.2}%, Unsure threshold: {:.2}%",
        filter_config.spam_threshold, filter_config.unsure_threshold
    ));

    // Save score to the message if configured
    if save_spam_info {
        save_score_to_item(mail_item, &general_config, result.score_pct);
    }

    // Determine action based on classification (using local config copy)
    let (action, folder_id, mark_as_read) = match result.classification {
        Classification::Spam => (
            &filter_config.spam_action,
            &filter_config.spam_folder_id,
            filter_config.spam_mark_as_read,
        ),
        Classification::Unsure => (
            &filter_config.unsure_action,
            &filter_config.unsure_folder_id,
            filter_config.unsure_mark_as_read,
        ),
        Classification::Ham => (
            &filter_config.ham_action,
            &filter_config.ham_folder_id,
            filter_config.ham_mark_as_read,
        ),
    };

    // Mark as read if configured
    if mark_as_read {
        let _ = crate::com_invoke::dispatch_put(
            mail_item, "UnRead", VariantArg::Bool(false)
        );
        log_debug(&debug_path, "  Marked as read");
    }

    // Perform filter action (move/copy)
    use spambayes_config::FilterAction;
    match action {
        FilterAction::Move => {
            if let Some(dest_folder_id) = folder_id {
                log_debug(&debug_path, &format!(
                    "  Action: MOVE to {} folder (entry={}...)",
                    classification_name,
                    &dest_folder_id.entry_id.0[..16.min(dest_folder_id.entry_id.0.len())]
                ));
                move_item_to_folder(mail_item, dest_folder_id, &debug_path);
            } else {
                log_debug(&debug_path, &format!(
                    "  Action: MOVE but no {} folder configured, leaving in place",
                    classification_name
                ));
            }
        }
        FilterAction::Copy => {
            if let Some(dest_folder_id) = folder_id {
                log_debug(&debug_path, &format!(
                    "  Action: COPY to {} folder", classification_name
                ));
                copy_item_to_folder(mail_item, dest_folder_id, &debug_path);
            } else {
                log_debug(&debug_path, &format!(
                    "  Action: COPY but no {} folder configured", classification_name
                ));
            }
        }
        FilterAction::Untouched => {
            log_debug(&debug_path, "  Action: UNTOUCHED (leave in place)");
        }
    }

    // Fire notification
    if let Ok(state) = sink.state.lock() {
        if let Ok(mut notif) = state.notification_mgr.lock() {
            let _commands = notif.record_classification(result.classification);
            // Commands (start timer etc.) would be handled by the main event loop.
            // For now we just record — sound playback is deferred.
        }
    }

    log_debug(&debug_path, &format!(
        "  Processing COMPLETE for '{}'\n", subject
    ));
}

// ─── COM Helpers for MailItem operations ─────────────────────────────────────

/// Get the MIME content of a mail item via PropertyAccessor.
///
/// Uses the MAPI property PR_MIME_CONTENT (schema:
/// "http://schemas.microsoft.com/mapi/proptag/0x10090102").
pub unsafe fn get_mime_content(mail_item: *mut c_void) -> Option<Vec<u8>> {
    // Get PropertyAccessor
    let prop_accessor = dispatch_get(mail_item, "PropertyAccessor").ok()?;
    if prop_accessor.is_null() {
        return None;
    }

    // PR_MIME_CONTENT schema URL
    let schema = "http://schemas.microsoft.com/mapi/proptag/0x10090102";

    // Call GetProperty(schema) — returns a byte array VARIANT
    let dispid = get_dispid_on(prop_accessor, "GetProperty")?;
    let vtbl = *(prop_accessor as *const *const IDispatchVtblRaw);

    let mut schema_variant = Variant::default();
    schema_variant.vt = 8; // VT_BSTR
    let bstr = sys_alloc_string_local(schema);
    *(schema_variant.data.as_mut_ptr().cast::<*mut u16>()) = bstr;

    let mut params = DispParams {
        rgvarg: &raw mut schema_variant,
        rgdispid_named_args: ptr::null_mut(),
        c_args: 1,
        c_named_args: 0,
    };

    let mut result = Variant::default();
    let hr = ((*vtbl).invoke)(
        prop_accessor,
        dispid,
        &GUID::from_u128(0),
        0,
        1, // DISPATCH_METHOD
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    SysFreeString(bstr);
    release_dispatch(prop_accessor);

    if hr.0 != 0 {
        return None;
    }

    // Result should be VT_ARRAY|VT_UI1 (0x2011) or a BSTR with base64.
    // For simplicity, try reading as a string and converting.
    // Outlook often returns MIME as a string via this property.
    if result.vt == 8 {
        // VT_BSTR
        let bstr_ptr = *(result.data.as_ptr().cast::<*const u16>());
        if bstr_ptr.is_null() {
            return None;
        }
        let len_ptr = (bstr_ptr as *const u8).sub(4) as *const u32;
        let byte_len = *len_ptr as usize;
        let char_len = byte_len / 2;
        let slice = std::slice::from_raw_parts(bstr_ptr, char_len);
        let s = String::from_utf16_lossy(slice);
        SysFreeString(bstr_ptr as *mut u16);
        Some(s.into_bytes())
    } else if result.vt == (0x2000 | 0x11) {
        // VT_ARRAY | VT_UI1 — SafeArray of bytes
        // For now, return None and fall back to headers+body
        None
    } else {
        None
    }
}

/// Get transport headers from a mail item via PropertyAccessor.
unsafe fn get_transport_headers(mail_item: *mut c_void) -> Option<String> {
    // PR_TRANSPORT_MESSAGE_HEADERS schema
    let prop_accessor = dispatch_get(mail_item, "PropertyAccessor").ok()?;
    if prop_accessor.is_null() {
        return None;
    }

    let schema = "http://schemas.microsoft.com/mapi/proptag/0x007D001F";
    let dispid = get_dispid_on(prop_accessor, "GetProperty")?;
    let vtbl = *(prop_accessor as *const *const IDispatchVtblRaw);

    let mut schema_variant = Variant::default();
    schema_variant.vt = 8; // VT_BSTR
    let bstr = sys_alloc_string_local(schema);
    *(schema_variant.data.as_mut_ptr().cast::<*mut u16>()) = bstr;

    let mut params = DispParams {
        rgvarg: &raw mut schema_variant,
        rgdispid_named_args: ptr::null_mut(),
        c_args: 1,
        c_named_args: 0,
    };

    let mut result_var = Variant::default();
    let hr = ((*vtbl).invoke)(
        prop_accessor,
        dispid,
        &GUID::from_u128(0),
        0,
        1,
        &raw mut params,
        &raw mut result_var,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    SysFreeString(bstr);
    release_dispatch(prop_accessor);

    if hr.0 != 0 || result_var.vt != 8 {
        return None;
    }

    let bstr_ptr = *(result_var.data.as_ptr().cast::<*const u16>());
    if bstr_ptr.is_null() {
        return None;
    }
    let len_ptr = (bstr_ptr as *const u8).sub(4) as *const u32;
    let byte_len = *len_ptr as usize;
    let char_len = byte_len / 2;
    let slice = std::slice::from_raw_parts(bstr_ptr, char_len);
    let s = String::from_utf16_lossy(slice);
    SysFreeString(bstr_ptr as *mut u16);
    Some(s)
}

/// Save the spam score to the message's custom property field.
unsafe fn save_score_to_item(
    mail_item: *mut c_void,
    general: &GeneralConfig,
    score_pct: f64,
) {
    // Use UserProperties to set the score field
    let user_props = match dispatch_get(mail_item, "UserProperties") {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };

    // Add or find the property: UserProperties.Find(field_name)
    // If not found, Add it: UserProperties.Add(field_name, olNumber)
    let field_name = &general.field_score_name;

    // Try Find first
    let existing = dispatch_invoke_with_bstr(user_props, "Find", field_name);
    let prop = if !existing.is_null() {
        existing
    } else {
        // Add(Name, Type) where olNumber = 1
        let added = dispatch_add_property(user_props, field_name, 1);
        if added.is_null() {
            release_dispatch(user_props);
            return;
        }
        added
    };

    // Set Value property on the UserProperty
    // Score is stored as a float percentage
    set_variant_property(prop, "Value", score_pct);

    release_dispatch(prop);
    release_dispatch(user_props);

    // Save the MailItem
    let _ = crate::com_invoke::dispatch_invoke_method(
        mail_item, "Save", &[]
    );
}

/// Move a MailItem to a destination folder identified by FolderId.
///
/// Resolves the folder via Namespace.GetFolderFromID and calls MailItem.Move.
pub unsafe fn move_item_to_folder(
    mail_item: *mut c_void,
    dest: &FolderId,
    debug_path: &str,
) {
    // Get the MailItem's Application.GetNamespace("MAPI")
    let app = match dispatch_get(mail_item, "Application") {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(debug_path, "move_item: cannot get Application");
            return;
        }
    };

    let namespace = match crate::com_invoke::dispatch_invoke_method(
        app, "GetNamespace", &[VariantArg::BStr("MAPI")]
    ) {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(debug_path, "move_item: cannot get MAPI namespace");
            return;
        }
    };

    // GetFolderFromID(EntryID, StoreID)
    let folder = match dispatch_invoke_2bstr(
        namespace,
        "GetFolderFromID",
        &dest.entry_id.0,
        &dest.store_id.0,
    ) {
        Some(p) if !p.is_null() => p,
        _ => {
            log_debug(debug_path, "move_item: GetFolderFromID failed");
            release_dispatch(namespace);
            return;
        }
    };

    // MailItem.Move(DestFolder)
    let dispid = match get_dispid_on(mail_item, "Move") {
        Some(id) => id,
        None => {
            log_debug(debug_path, "move_item: cannot resolve Move DISPID");
            release_dispatch(folder);
            release_dispatch(namespace);
            return;
        }
    };

    let mut arg = Variant::default();
    arg.vt = VT_DISPATCH;
    *(arg.data.as_mut_ptr().cast::<*mut c_void>()) = folder;

    let mut params = DispParams {
        rgvarg: &raw mut arg,
        rgdispid_named_args: ptr::null_mut(),
        c_args: 1,
        c_named_args: 0,
    };

    let vtbl = *(mail_item as *const *const IDispatchVtblRaw);
    let _hr = ((*vtbl).invoke)(
        mail_item,
        dispid,
        &GUID::from_u128(0),
        0,
        1, // DISPATCH_METHOD
        &raw mut params,
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null_mut(),
    );

    release_dispatch(folder);
    release_dispatch(namespace);
}

/// Copy a MailItem to a destination folder identified by FolderId.
unsafe fn copy_item_to_folder(
    mail_item: *mut c_void,
    dest: &FolderId,
    debug_path: &str,
) {
    let app = match dispatch_get(mail_item, "Application") {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };

    let namespace = match crate::com_invoke::dispatch_invoke_method(
        app, "GetNamespace", &[VariantArg::BStr("MAPI")]
    ) {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };

    let folder = match dispatch_invoke_2bstr(
        namespace,
        "GetFolderFromID",
        &dest.entry_id.0,
        &dest.store_id.0,
    ) {
        Some(p) if !p.is_null() => p,
        _ => {
            release_dispatch(namespace);
            return;
        }
    };

    // MailItem.Copy() returns a new MailItem
    let copy = match crate::com_invoke::dispatch_invoke_method(
        mail_item, "Copy", &[]
    ) {
        Ok(p) if !p.is_null() => p,
        _ => {
            release_dispatch(folder);
            release_dispatch(namespace);
            return;
        }
    };

    // copy.Move(folder)
    if let Some(dispid) = get_dispid_on(copy, "Move") {
        let mut arg = Variant::default();
        arg.vt = VT_DISPATCH;
        *(arg.data.as_mut_ptr().cast::<*mut c_void>()) = folder;

        let mut params = DispParams {
            rgvarg: &raw mut arg,
            rgdispid_named_args: ptr::null_mut(),
            c_args: 1,
            c_named_args: 0,
        };

        let vtbl = *(copy as *const *const IDispatchVtblRaw);
        let _hr = ((*vtbl).invoke)(
            copy,
            dispid,
            &GUID::from_u128(0),
            0,
            1,
            &raw mut params,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
    }

    release_dispatch(copy);
    release_dispatch(folder);
    release_dispatch(namespace);
    log_debug(debug_path, "copy_item_to_folder: done");
}

// ─── Public API: Setup Folder Hooks ──────────────────────────────────────────

/// Set up folder monitoring hooks for all configured watch folders.
///
/// For each folder in `config.filter.watch_folder_ids`, resolves the folder
/// via the Outlook Object Model, gets its `Items` collection, and connects
/// our `FolderItemsSink` to receive `ItemAdd` events.
///
/// Returns a Vec of active `FolderHook`s (caller must keep them alive).
pub unsafe fn setup_folder_hooks(
    _app_ptr: *mut c_void,
    state: Arc<Mutex<FolderHookState>>,
    watch_folder_ids: &[FolderId],
) -> Vec<FolderHook> {
    let debug_path = debug_log_path();
    let mut hooks = Vec::new();

    log_debug(&debug_path, &format!(
        "setup_folder_hooks: {} folders to watch", watch_folder_ids.len()
    ));

    // Get Application via CoCreateInstance("Outlook.Application")
    // The pointer passed from OnConnection doesn't reliably support
    // IDispatch::GetIDsOfNames for Outlook methods. Use CoCreateInstance
    // to get a proper Outlook.Application dispatch pointer.
    let app_ptr = get_outlook_application();
    if app_ptr.is_null() {
        log_debug(&debug_path, "setup_folder_hooks: cannot get Outlook.Application");
        return hooks;
    }

    // Get Application.GetNamespace("MAPI")
    let namespace = match crate::com_invoke::dispatch_invoke_method(
        app_ptr, "GetNamespace", &[VariantArg::BStr("MAPI")]
    ) {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(&debug_path, "setup_folder_hooks: cannot get MAPI namespace");
            release_dispatch(app_ptr);
            return hooks;
        }
    };

    for folder_id in watch_folder_ids {
        match hook_single_folder(namespace, folder_id, Arc::clone(&state)) {
            Some(hook) => {
                log_debug(&debug_path, &format!(
                    "Watching folder '{}' for new messages", hook.folder_name
                ));
                hooks.push(hook);
            }
            None => {
                log_debug(&debug_path, &format!(
                    "Failed to hook folder: store={}, entry={}",
                    &folder_id.store_id.0[..8.min(folder_id.store_id.0.len())],
                    &folder_id.entry_id.0[..8.min(folder_id.entry_id.0.len())]
                ));
            }
        }
    }

    release_dispatch(namespace);
    release_dispatch(app_ptr);
    log_debug(&debug_path, &format!(
        "setup_folder_hooks: {} hooks active", hooks.len()
    ));
    hooks
}

/// Scan existing items in watched folders and filter any that haven't been scored.
///
/// Called once after folder hooks are established to process messages that
/// arrived before the add-in started (e.g., messages Outlook's junk filter
/// moved while Outlook was loading).
pub unsafe fn scan_existing_items(
    hooks: &[FolderHook],
    state: &Arc<Mutex<FolderHookState>>,
) {
    let debug_path = debug_log_path();

    let app_ptr = get_outlook_application();
    if app_ptr.is_null() {
        log_debug(&debug_path, "scan_existing_items: cannot get Outlook.Application");
        return;
    }

    let namespace = match crate::com_invoke::dispatch_invoke_method(
        app_ptr, "GetNamespace", &[VariantArg::BStr("MAPI")]
    ) {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(&debug_path, "scan_existing_items: cannot get MAPI namespace");
            release_dispatch(app_ptr);
            return;
        }
    };

    // Get the score field name from config
    let score_field = {
        let st = match state.lock() {
            Ok(s) => s,
            Err(_) => {
                release_dispatch(namespace);
                release_dispatch(app_ptr);
                return;
            }
        };
        st.config.general.field_score_name.clone()
    };

    for hook in hooks {
        scan_folder_items(
            &hook.items_ptr,
            &hook.folder_name,
            state,
            &score_field,
            &debug_path,
        );
    }

    release_dispatch(namespace);
    release_dispatch(app_ptr);
}

/// Iterate items in a folder's Items collection and process unscored ones.
unsafe fn scan_folder_items(
    items_ptr: &*mut c_void,
    folder_name: &str,
    state: &Arc<Mutex<FolderHookState>>,
    score_field: &str,
    debug_path: &str,
) {
    if items_ptr.is_null() {
        return;
    }

    // Get Items.Count
    let count_str = dispatch_get_string(*items_ptr, "Count");
    let count: i32 = match count_str {
        Some(ref s) => s.parse().unwrap_or(0),
        None => {
            // Try getting it as a dispatch property that returns a number
            // Use dispatch_get which returns VT_DISPATCH or null
            // Actually Items.Count returns VT_I4, need a different approach
            match get_items_count(*items_ptr) {
                Some(c) => c,
                None => {
                    log_debug(debug_path, &format!(
                        "scan_folder_items: cannot get count for '{}'", folder_name
                    ));
                    return;
                }
            }
        }
    };

    log_debug(debug_path, &format!(
        "scan_folder_items: '{}' has {} items", folder_name, count
    ));

    if count == 0 {
        return;
    }

    // Iterate items (1-based index in COM)
    let mut processed = 0;
    let mut skipped = 0;
    for i in 1..=count {
        let item = get_items_item(*items_ptr, i);
        if item.is_null() {
            continue;
        }

        // Check if already scored (has the score field set)
        let has_score = check_has_score(item, score_field);
        if has_score {
            skipped += 1;
            release_dispatch(item);
            continue;
        }

        // Build a temporary sink reference to reuse handle_item_add
        let temp_sink = FolderItemsSink {
            vtbl: &raw const FOLDER_SINK_VTBL,
            ref_count: AtomicU32::new(1),
            state: Arc::clone(state),
            folder_name: folder_name.to_string(),
        };

        handle_item_add(&temp_sink, item);
        processed += 1;

        // Don't release item — handle_item_add doesn't AddRef it,
        // and moving it invalidates the pointer anyway.
        // But if it wasn't moved (ham), we should release.
        // For safety, don't release — the Items collection owns it.
    }

    log_debug(debug_path, &format!(
        "scan_folder_items: '{}' processed={}, skipped={} (already scored)",
        folder_name, processed, skipped
    ));
}

/// Hook a single folder for ItemAdd events.
unsafe fn hook_single_folder(
    namespace: *mut c_void,
    folder_id: &FolderId,
    state: Arc<Mutex<FolderHookState>>,
) -> Option<FolderHook> {
    let debug_path = debug_log_path();

    // Namespace.GetFolderFromID(EntryID, StoreID)
    let folder = dispatch_invoke_2bstr(
        namespace,
        "GetFolderFromID",
        &folder_id.entry_id.0,
        &folder_id.store_id.0,
    )?;

    if folder.is_null() {
        return None;
    }

    // Get folder name for logging
    let folder_name = dispatch_get_string(folder, "Name")
        .unwrap_or_else(|| "Unknown".to_string());

    log_debug(&debug_path, &format!(
        "hook_single_folder: resolved folder '{}'", folder_name
    ));

    // Get folder.Items collection
    let items = match dispatch_get(folder, "Items") {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(&debug_path, "hook_single_folder: cannot get Items collection");
            release_dispatch(folder);
            return None;
        }
    };

    // Release the folder (we keep Items alive)
    release_dispatch(folder);

    // Connect our sink to the Items collection's ItemsEvents connection point
    let (cp_ptr, cookie) = match advise_items_event(items, state, &folder_name) {
        Some(result) => result,
        None => {
            log_debug(&debug_path, "hook_single_folder: advise failed");
            release_dispatch(items);
            return None;
        }
    };

    Some(FolderHook {
        items_ptr: items,
        connection_point: cp_ptr,
        cookie,
        folder_name,
    })
}

/// Connect our FolderItemsSink to the Items collection's event connection point.
///
/// Returns (IConnectionPoint*, cookie) on success.
unsafe fn advise_items_event(
    items_ptr: *mut c_void,
    state: Arc<Mutex<FolderHookState>>,
    folder_name: &str,
) -> Option<(*mut c_void, u32)> {
    // QI for IConnectionPointContainer on the Items collection
    let vtbl = *(items_ptr as *const *const [usize; 3]);
    let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
        std::mem::transmute((*vtbl)[0]);

    let mut cpc_ptr: *mut c_void = ptr::null_mut();
    let hr = qi(items_ptr, &IID_ICONNECTION_POINT_CONTAINER, &raw mut cpc_ptr);
    if hr.0 != 0 || cpc_ptr.is_null() {
        return None;
    }

    // FindConnectionPoint for ItemsEvents
    let cpc_vtbl = *(cpc_ptr as *const *const IConnectionPointContainerVtbl);
    let mut cp_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*cpc_vtbl).find_connection_point)(cpc_ptr, &IID_ITEMS_EVENTS, &raw mut cp_ptr);

    // Release CPC
    let cpc_release: unsafe extern "system" fn(*mut c_void) -> u32 =
        std::mem::transmute((*cpc_vtbl).release);
    cpc_release(cpc_ptr);

    if hr.0 != 0 || cp_ptr.is_null() {
        return None;
    }

    // Create our sink
    let sink = Box::new(FolderItemsSink {
        vtbl: &raw const FOLDER_SINK_VTBL,
        ref_count: AtomicU32::new(1),
        state,
        folder_name: folder_name.to_string(),
    });
    let sink_ptr = Box::into_raw(sink) as *mut c_void;

    // Advise
    let cp_vtbl = *(cp_ptr as *const *const IConnectionPointVtbl);
    let mut cookie: u32 = 0;
    let hr = ((*cp_vtbl).advise)(cp_ptr, sink_ptr, &raw mut cookie);

    if hr.0 != 0 {
        // Advise failed — release our sink
        folder_sink_release(sink_ptr);
        // Release CP
        let cp_release: unsafe extern "system" fn(*mut c_void) -> u32 =
            std::mem::transmute((*cp_vtbl).release);
        cp_release(cp_ptr);
        return None;
    }

    Some((cp_ptr, cookie))
}

// ─── Low-level COM Utility Functions ─────────────────────────────────────────

/// Get the Outlook.Application IDispatch pointer via CoCreateInstance.
///
/// Since Outlook is already running (we're loaded in-process), this
/// connects to the existing running instance rather than starting a new one.
unsafe fn get_outlook_application() -> *mut c_void {
    // CLSID for Outlook.Application: {0006F03A-0000-0000-C000-000000000046}
    let outlook_clsid = GUID::from_u128(0x0006F03A_0000_0000_C000_000000000046);
    let iid_dispatch = GUID::from_u128(0x00020400_0000_0000_C000_000000000046);

    #[link(name = "ole32")]
    extern "system" {
        fn CoCreateInstance(
            rclsid: *const GUID,
            p_unk_outer: *mut c_void,
            dw_cls_context: u32,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> i32;
    }

    let mut app_ptr: *mut c_void = ptr::null_mut();
    let hr = CoCreateInstance(
        &outlook_clsid,
        ptr::null_mut(),
        4, // CLSCTX_LOCAL_SERVER
        &iid_dispatch,
        &raw mut app_ptr,
    );

    if hr != 0 || app_ptr.is_null() {
        log_debug(&debug_log_path(), &format!(
            "get_outlook_application: CoCreateInstance failed hr={:#X}", hr
        ));
        return ptr::null_mut();
    }

    app_ptr
}

/// Raw IDispatch vtable pointer type (for direct invoke calls).
type IDispatchVtblRaw = IDispatchVtbl;

extern "system" {
    fn SysAllocString(psz: *const u16) -> *mut u16;
    fn SysFreeString(bstr: *mut u16);
}

unsafe fn sys_alloc_string_local(s: &str) -> *mut u16 {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    SysAllocString(wide.as_ptr())
}

/// Release a COM dispatch pointer.
unsafe fn release_dispatch(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let vtbl = *(ptr as *const *const [usize; 3]);
    let release: unsafe extern "system" fn(*mut c_void) -> u32 =
        std::mem::transmute((*vtbl)[2]);
    release(ptr);
}

/// Get a DISPID by name on a raw IDispatch pointer.
unsafe fn get_dispid_on(disp: *mut c_void, name: &str) -> Option<i32> {
    if disp.is_null() {
        return None;
    }
    let vtbl = *(disp as *const *const IDispatchVtbl);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut name_ptr = PCWSTR(wide.as_ptr());
    let mut dispid: i32 = 0;

    let hr = ((*vtbl).get_ids_of_names)(
        disp,
        &GUID::from_u128(0),
        &raw mut name_ptr,
        1,
        0,
        &raw mut dispid,
    );

    if hr.0 == 0 {
        Some(dispid)
    } else {
        None
    }
}

/// Invoke a method with two BSTR arguments and return an IDispatch pointer.
///
/// Used for Namespace.GetFolderFromID(EntryID, StoreID).
unsafe fn dispatch_invoke_2bstr(
    disp: *mut c_void,
    method_name: &str,
    arg1: &str,
    arg2: &str,
) -> Option<*mut c_void> {
    let dispid = get_dispid_on(disp, method_name)?;
    let vtbl = *(disp as *const *const IDispatchVtbl);

    // COM args are in reverse order
    let bstr1 = sys_alloc_string_local(arg1);
    let bstr2 = sys_alloc_string_local(arg2);

    let mut args = [Variant::default(), Variant::default()];
    // Reversed: arg2 first (index 0 = last positional arg in COM)
    args[0].vt = 8; // VT_BSTR
    *(args[0].data.as_mut_ptr().cast::<*mut u16>()) = bstr2;
    args[1].vt = 8; // VT_BSTR
    *(args[1].data.as_mut_ptr().cast::<*mut u16>()) = bstr1;

    let mut params = DispParams {
        rgvarg: args.as_mut_ptr(),
        rgdispid_named_args: ptr::null_mut(),
        c_args: 2,
        c_named_args: 0,
    };

    let mut result = Variant::default();
    let hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &GUID::from_u128(0),
        0,
        1, // DISPATCH_METHOD
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    SysFreeString(bstr1);
    SysFreeString(bstr2);

    if hr.0 != 0 {
        return None;
    }

    if result.vt == VT_DISPATCH {
        let ptr = *(result.data.as_ptr().cast::<*mut c_void>());
        Some(ptr)
    } else {
        None
    }
}

/// Invoke UserProperties.Find(name) — returns dispatch pointer or null.
unsafe fn dispatch_invoke_with_bstr(
    disp: *mut c_void,
    method_name: &str,
    arg: &str,
) -> *mut c_void {
    let dispid = match get_dispid_on(disp, method_name) {
        Some(id) => id,
        None => return ptr::null_mut(),
    };
    let vtbl = *(disp as *const *const IDispatchVtbl);

    let bstr = sys_alloc_string_local(arg);
    let mut variant_arg = Variant::default();
    variant_arg.vt = 8; // VT_BSTR
    *(variant_arg.data.as_mut_ptr().cast::<*mut u16>()) = bstr;

    let mut params = DispParams {
        rgvarg: &raw mut variant_arg,
        rgdispid_named_args: ptr::null_mut(),
        c_args: 1,
        c_named_args: 0,
    };

    let mut result = Variant::default();
    let hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &GUID::from_u128(0),
        0,
        1, // DISPATCH_METHOD
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    SysFreeString(bstr);

    if hr.0 != 0 || result.vt != VT_DISPATCH {
        return ptr::null_mut();
    }

    *(result.data.as_ptr().cast::<*mut c_void>())
}

/// Call UserProperties.Add(Name, Type) where Type is olNumber=1.
unsafe fn dispatch_add_property(
    user_props: *mut c_void,
    name: &str,
    prop_type: i32,
) -> *mut c_void {
    let dispid = match get_dispid_on(user_props, "Add") {
        Some(id) => id,
        None => return ptr::null_mut(),
    };
    let vtbl = *(user_props as *const *const IDispatchVtbl);

    let bstr = sys_alloc_string_local(name);

    // Args in reverse: type first (index 0), name second (index 1)
    let mut args = [Variant::default(), Variant::default()];
    args[0].vt = 3; // VT_I4
    *(args[0].data.as_mut_ptr().cast::<i32>()) = prop_type;
    args[1].vt = 8; // VT_BSTR
    *(args[1].data.as_mut_ptr().cast::<*mut u16>()) = bstr;

    let mut params = DispParams {
        rgvarg: args.as_mut_ptr(),
        rgdispid_named_args: ptr::null_mut(),
        c_args: 2,
        c_named_args: 0,
    };

    let mut result = Variant::default();
    let hr = ((*vtbl).invoke)(
        user_props,
        dispid,
        &GUID::from_u128(0),
        0,
        1,
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    SysFreeString(bstr);

    if hr.0 != 0 || result.vt != VT_DISPATCH {
        return ptr::null_mut();
    }

    *(result.data.as_ptr().cast::<*mut c_void>())
}

/// Set a float property via IDispatch PROPERTYPUT.
unsafe fn set_variant_property(disp: *mut c_void, name: &str, value: f64) {
    let dispid = match get_dispid_on(disp, name) {
        Some(id) => id,
        None => return,
    };
    let vtbl = *(disp as *const *const IDispatchVtbl);

    let mut arg = Variant::default();
    arg.vt = 5; // VT_R8 (double)
    *(arg.data.as_mut_ptr().cast::<f64>()) = value;

    let mut named_arg: i32 = -3; // DISPID_PROPERTYPUT

    let mut params = DispParams {
        rgvarg: &raw mut arg,
        rgdispid_named_args: &raw mut named_arg,
        c_args: 1,
        c_named_args: 1,
    };

    let _hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &GUID::from_u128(0),
        0,
        4, // DISPATCH_PROPERTYPUT
        &raw mut params,
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null_mut(),
    );
}

// ─── Debug Logging ───────────────────────────────────────────────────────────

fn debug_log_path() -> String {
    let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
    format!("{data_dir}\\SpamBayes\\folder_monitor.log")
}

fn log_debug(path: &str, msg: &str) {
    use std::io::Write;
    // Format as YYYY-MM-DD HH:MM:SS (approximate from epoch)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Simple time formatting: hours:minutes:seconds of day
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "[{:02}:{:02}:{:02}] {}", hours, minutes, seconds, msg));
}

// ─── Items Collection Helpers ────────────────────────────────────────────────

/// Get Items.Count as an i32.
unsafe fn get_items_count(items: *mut c_void) -> Option<i32> {
    let dispid = get_dispid_on(items, "Count")?;
    let vtbl = *(items as *const *const IDispatchVtbl);

    let mut params = DispParams {
        rgvarg: ptr::null_mut(),
        rgdispid_named_args: ptr::null_mut(),
        c_args: 0,
        c_named_args: 0,
    };

    let mut result = Variant::default();
    let hr = ((*vtbl).invoke)(
        items,
        dispid,
        &GUID::from_u128(0),
        0,
        2, // DISPATCH_PROPERTYGET
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    if hr.0 != 0 {
        return None;
    }

    // Count is VT_I4 (3)
    if result.vt == 3 {
        let val = *(result.data.as_ptr().cast::<i32>());
        Some(val)
    } else {
        None
    }
}

/// Get Items.Item(index) — returns an IDispatch pointer to the mail item.
/// Index is 1-based.
unsafe fn get_items_item(items: *mut c_void, index: i32) -> *mut c_void {
    let dispid = match get_dispid_on(items, "Item") {
        Some(id) => id,
        None => return ptr::null_mut(),
    };
    let vtbl = *(items as *const *const IDispatchVtbl);

    let mut arg = Variant::default();
    arg.vt = 3; // VT_I4
    *(arg.data.as_mut_ptr().cast::<i32>()) = index;

    let mut params = DispParams {
        rgvarg: &raw mut arg,
        rgdispid_named_args: ptr::null_mut(),
        c_args: 1,
        c_named_args: 0,
    };

    let mut result = Variant::default();
    let hr = ((*vtbl).invoke)(
        items,
        dispid,
        &GUID::from_u128(0),
        0,
        1, // DISPATCH_METHOD
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    if hr.0 != 0 || result.vt != VT_DISPATCH {
        return ptr::null_mut();
    }

    *(result.data.as_ptr().cast::<*mut c_void>())
}

/// Check if a MailItem already has a spam score set in UserProperties.
unsafe fn check_has_score(mail_item: *mut c_void, score_field: &str) -> bool {
    let user_props = match dispatch_get(mail_item, "UserProperties") {
        Ok(p) if !p.is_null() => p,
        _ => return false,
    };

    let prop = dispatch_invoke_with_bstr(user_props, "Find", score_field);
    release_dispatch(user_props);

    if prop.is_null() {
        return false;
    }

    // Property exists — check if it has a value
    release_dispatch(prop);
    true
}
