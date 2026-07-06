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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Reentrancy guard: prevents processing a new event while a dialog is open.
/// Only one calendar prompt (or filter action) runs at a time on the STA thread.
static PROCESSING_EVENT: AtomicBool = AtomicBool::new(false);

/// Set of message identifiers (Subject + SenderName hash) recently moved by
/// user action (train as ham/spam). Messages in this set are skipped by
/// `handle_item_add` to avoid re-filtering after a deliberate move.
/// The Python version uses a message database keyed by PR_SEARCH_KEY;
/// we use a simple in-memory set that's cleared periodically.
///
/// The value is `true` if moved as ham (Not Spam), `false` if moved as spam.
/// Only ham moves should skip re-filtering; spam moves that bounce back
/// need to be re-moved to the spam folder.
static RECENTLY_MOVED: std::sync::LazyLock<Mutex<std::collections::HashMap<String, bool>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Tracks messages recently moved by the filter engine (not user action).
/// Used for bounce-back detection on Exchange/Outlook.com/Hotmail stores
/// where we cannot persist the score to the message's UserProperties.
/// Key: message identity, Value: score percentage at time of filtering.
static RECENTLY_FILTERED: std::sync::LazyLock<Mutex<std::collections::HashMap<String, f64>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Record that a message was moved by user action (to skip re-filtering).
/// `is_ham` = true means "Not Spam" (should stay in watch folder if it bounces back).
/// `is_ham` = false means "Spam" (should be re-moved to spam if it bounces back).
pub fn mark_as_user_moved(mail_item: *mut c_void, is_ham: bool) {
    unsafe {
        let key = get_message_identity(mail_item);
        if let Some(k) = key {
            if let Ok(mut map) = RECENTLY_MOVED.lock() {
                let _ = map.insert(k, is_ham);
            }
        }
    }
}

/// Check if a message was recently moved by user action as ham (Not Spam).
/// Returns true only for ham moves — these should be skipped.
/// Spam moves that bounce back are NOT skipped (they need re-moving).
fn was_user_moved_as_ham(mail_item: *mut c_void) -> bool {
    unsafe {
        let key = get_message_identity(mail_item);
        if let Some(k) = key {
            if let Ok(map) = RECENTLY_MOVED.lock() {
                return map.get(&k).copied() == Some(true);
            }
        }
    }
    false
}

/// Remove a message from the recently-moved map (after it's been handled).
fn clear_user_moved(mail_item: *mut c_void) {
    unsafe {
        let key = get_message_identity(mail_item);
        if let Some(k) = key {
            if let Ok(mut map) = RECENTLY_MOVED.lock() {
                map.remove(&k);
            }
        }
    }
}

/// Record that a message was moved by the filter engine (for bounce-back detection
/// on Exchange stores that don't support persisting scores to UserProperties).
fn mark_as_filter_moved(mail_item: *mut c_void, score_pct: f64) {
    unsafe {
        let key = get_message_identity(mail_item);
        if let Some(k) = key {
            if let Ok(mut map) = RECENTLY_FILTERED.lock() {
                let _ = map.insert(k, score_pct);
            }
        }
    }
}

/// Check if a message was recently moved by the filter engine.
/// Returns the score if found, None otherwise.
fn get_filter_moved_score(mail_item: *mut c_void) -> Option<f64> {
    unsafe {
        let key = get_message_identity(mail_item);
        if let Some(k) = key {
            if let Ok(map) = RECENTLY_FILTERED.lock() {
                return map.get(&k).copied();
            }
        }
    }
    None
}

/// Get a stable identity for a message that survives moves.
/// Uses InternetMessageId (Message-ID header) which is unique and immutable.
/// Falls back to Subject + SenderName + Size as a composite key.
unsafe fn get_message_identity(mail_item: *mut c_void) -> Option<String> {
    // Try PR_INTERNET_MESSAGE_ID first (most reliable, doesn't change on move)
    if let Some(msg_id) = dispatch_get_string(mail_item, "InternetMessageId") {
        if !msg_id.is_empty() {
            return Some(msg_id);
        }
    }
    // Fallback: composite key from Subject + SenderName + Size
    let subject = dispatch_get_string(mail_item, "Subject").unwrap_or_default();
    let sender = dispatch_get_string(mail_item, "SenderName").unwrap_or_default();
    if subject.is_empty() && sender.is_empty() {
        return None;
    }
    Some(format!("{}|{}", subject, sender))
}

use windows::core::{GUID, HRESULT, PCWSTR};

use crate::com_invoke::{dispatch_get, dispatch_get_string, VariantArg};
use crate::filter::{FilterEngine};
use crate::notification::NotificationManager;

use spambayes_config::{AppConfig, CalendarSpamAction, FolderId, GeneralConfig};
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
    /// Statistics manager for tracking classification counts.
    pub statistics: Option<crate::statistics::StatisticsManager>,
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
    /// If true, this hook only processes calendar items (no startup scan).
    pub calendar_only: bool,
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
    /// If true, only process calendar items (skip regular mail).
    /// Used for the Inbox hook that exists solely for calendar filtering.
    calendar_only: bool,
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
    let debug_path = debug_log_path();

    if disp_id != DISPID_ITEM_ADD {
        log_debug(&debug_path, &format!(
            "[folder_sink_invoke] Non-ItemAdd event received: DISPID=0x{:X}", disp_id
        ));
        return HRESULT(0); // S_OK — ignore other events
    }

    log_debug(&debug_path, "[folder_sink_invoke] ItemAdd event received");

    let sink = &*(this as *const FolderItemsSink);

    // Extract the MailItem IDispatch pointer from the event arguments.
    // ItemAdd passes one argument: the new Item (VT_DISPATCH).
    if params.is_null() {
        log_debug(&debug_path, "[folder_sink_invoke] params is NULL, ignoring");
        return HRESULT(0);
    }
    let dp = &*params;
    if dp.c_args == 0 || dp.rgvarg.is_null() {
        log_debug(&debug_path, &format!(
            "[folder_sink_invoke] No args: c_args={}, rgvarg_null={}",
            dp.c_args, dp.rgvarg.is_null()
        ));
        return HRESULT(0);
    }

    let arg = &*dp.rgvarg;
    if arg.vt != VT_DISPATCH {
        log_debug(&debug_path, &format!(
            "[folder_sink_invoke] Arg is not VT_DISPATCH: vt={}", arg.vt
        ));
        return HRESULT(0);
    }

    let mail_item = *(arg.data.as_ptr().cast::<*mut c_void>());
    if mail_item.is_null() {
        log_debug(&debug_path, "[folder_sink_invoke] mail_item pointer is NULL");
        return HRESULT(0);
    }

    log_debug(&debug_path, "[folder_sink_invoke] Dispatching to handle_item_add");

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

    // Reentrancy guard: if we're already processing an event (e.g., showing a
    // dialog), skip this one. COM can pump messages while a MessageBox is open,
    // causing new ItemAdd events to fire.
    if PROCESSING_EVENT.swap(true, Ordering::SeqCst) {
        log_debug(&debug_path, "  [SKIPPED] Already processing an event (reentrancy guard)");
        return;
    }
    // Ensure the flag is cleared when we exit, even on early returns.
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            PROCESSING_EVENT.store(false, Ordering::SeqCst);
        }
    }
    let _guard = Guard;

    // Get message properties for logging
    let raw_subject = dispatch_get_string(mail_item, "Subject");
    let raw_conv_topic = dispatch_get_string(mail_item, "ConversationTopic");
    let subject = raw_subject.clone()
        .or_else(|| raw_conv_topic.clone())
        .unwrap_or_else(|| "<no subject>".to_string());
    let sender = dispatch_get_string(mail_item, "SenderName")
        .or_else(|| dispatch_get_string(mail_item, "Organizer"))
        .unwrap_or_else(|| "<unknown>".to_string());
    let msg_class = dispatch_get_string(mail_item, "MessageClass")
        .unwrap_or_else(|| "IPM.Note".to_string());

    log_debug(&debug_path, &format!(
        "  [DEBUG] raw Subject={:?}, ConversationTopic={:?}, resolved='{}'",
        raw_subject, raw_conv_topic, subject
    ));

    // If this sink is calendar-only (Inbox hook), skip non-calendar items immediately
    if sink.calendar_only {
        let upper = msg_class.to_uppercase();
        if !upper.starts_with("IPM.SCHEDULE.MEETING")
            && !upper.starts_with("IPM.SCHEDULE.CANCELED")
            && !upper.starts_with("IPM.APPOINTMENT")
        {
            // Not a calendar item — ignore silently (don't filter regular Inbox mail)
            return;
        }
        log_debug(&debug_path, &format!(
            "========== ItemAdd FIRED in '{}' (calendar-only hook) ==========", sink.folder_name
        ));
    }

    log_debug(&debug_path, &format!(
        "========== ItemAdd FIRED in '{}' ==========", sink.folder_name
    ));
    log_debug(&debug_path, &format!("  Subject: {}", subject));
    log_debug(&debug_path, &format!("  Sender: {}", sender));
    log_debug(&debug_path, &format!("  MessageClass: {}", msg_class));

    // ── Message class gate ──────────────────────────────────────────────────
    // Only IPM.Note.* and IPM.Anti-Virus.* are regular mail. Anything else
    // (calendar invites, delivery receipts, posts, etc.) needs special handling.
    let upper_class = msg_class.to_uppercase();

    log_debug(&debug_path, &format!("  MessageClass (raw): '{}'", msg_class));
    log_debug(&debug_path, &format!("  MessageClass (upper): '{}'", upper_class));

    let is_calendar_item = upper_class.starts_with("IPM.SCHEDULE.MEETING")
        || upper_class.starts_with("IPM.SCHEDULE.CANCELED")
        || upper_class.starts_with("IPM.APPOINTMENT");

    log_debug(&debug_path, &format!("  is_calendar_item: {}", is_calendar_item));
    log_debug(&debug_path, &format!(
        "  starts_with IPM.SCHEDULE.MEETING: {}",
        upper_class.starts_with("IPM.SCHEDULE.MEETING")
    ));
    log_debug(&debug_path, &format!(
        "  starts_with IPM.SCHEDULE.CANCELED: {}",
        upper_class.starts_with("IPM.SCHEDULE.CANCELED")
    ));
    log_debug(&debug_path, &format!(
        "  starts_with IPM.APPOINTMENT: {}",
        upper_class.starts_with("IPM.APPOINTMENT")
    ));
    log_debug(&debug_path, &format!(
        "  starts_with IPM.NOTE: {}",
        upper_class.starts_with("IPM.NOTE")
    ));

    if is_calendar_item {
        log_debug(&debug_path, "  >> CALENDAR PATH: entering calendar handling");
        // Re-read the CalendarConfig from the INI file on disk, because the
        // in-memory config may be stale (the manager subprocess writes config
        // changes to disk but the COM addin doesn't reload automatically).
        let calendar_config = {
            let data_dir = {
                let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
                std::path::PathBuf::from(local_app_data).join("SpamBayes")
            };

            // Determine profile name from the state's config (general.data_directory
            // is empty when using the default profile name)
            let profile_name = {
                let state = match sink.state.lock() {
                    Ok(s) => s,
                    Err(_) => {
                        log_debug(&debug_path, "ERROR: FolderHookState lock poisoned");
                        return;
                    }
                };
                // Use the profile name from the config chain or default
                if state.config.general.data_directory.is_empty() {
                    "default".to_string()
                } else {
                    state.config.general.data_directory.clone()
                }
            };

            log_debug(&debug_path, &format!(
                "  Reloading CalendarConfig from disk: dir={}, profile={}",
                data_dir.display(), profile_name
            ));

            match spambayes_config::AppConfig::load(&data_dir, &profile_name) {
                Ok(fresh_config) => {
                    log_debug(&debug_path, &format!(
                        "  Fresh CalendarConfig from disk: enabled={}, action={:?}",
                        fresh_config.calendar.calendar_filtering_enabled,
                        fresh_config.calendar.calendar_spam_action
                    ));
                    fresh_config.calendar
                }
                Err(e) => {
                    log_debug(&debug_path, &format!(
                        "  Failed to reload config from disk: {:?}, falling back to in-memory",
                        e
                    ));
                    // Fall back to in-memory config
                    let state = match sink.state.lock() {
                        Ok(s) => s,
                        Err(_) => {
                            log_debug(&debug_path, "ERROR: FolderHookState lock poisoned");
                            return;
                        }
                    };
                    state.config.calendar.clone()
                }
            }
        };

        log_debug(&debug_path, &format!(
            "  CalendarConfig: enabled={}, action={:?}",
            calendar_config.calendar_filtering_enabled,
            calendar_config.calendar_spam_action
        ));

        if !calendar_config.calendar_filtering_enabled {
            log_debug(&debug_path, "  Calendar item detected but calendar filtering is DISABLED, skipping");
            return;
        }

        log_debug(&debug_path, &format!(
            "  Calendar item detected, calendar filtering ENABLED (action: {:?})",
            calendar_config.calendar_spam_action
        ));
        // Calendar filtering is enabled — fall through to classify the item
        // and apply the calendar-specific action below.
    } else if !upper_class.starts_with("IPM.NOTE") && !upper_class.starts_with("IPM.ANTI-VIRUS") {
        // Non-mail, non-calendar item (e.g. delivery receipt, post, task)
        log_debug(&debug_path, &format!(
            "  >> NON-MAIL PATH: message class '{}' not recognized, skipping", msg_class
        ));
        return;
    } else {
        log_debug(&debug_path, "  >> MAIL PATH: proceeding with normal mail filtering");
    }

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

    // Skip messages that have already been scored/trained.
    // When the user clicks "Not Spam" and the message is moved back to the
    // watch folder, ItemAdd fires again. The message already has a score
    // property set from its original classification, so we skip it to avoid
    // duplicates or re-filtering trained messages.
    //
    // However, if the message was previously classified as spam/unsure but
    // ended up back in the watch folder (Exchange server-side rules undid
    // our move), we need to re-move it to the correct SpamBayes folder.
    // If use_cached_scores is false (like Python), always re-score.
    if check_has_score(mail_item, &general_config.field_score_name) && filter_config.use_cached_scores {
        // Check if this was a user-initiated ham move (Not Spam button)
        if was_user_moved_as_ham(mail_item) {
            log_debug(&debug_path, "  Message already scored AND was user-moved as ham, skipping");
            clear_user_moved(mail_item);
            return;
        }

        let existing_score = get_score_value(mail_item, &general_config.field_score_name);
        if let Some(score) = existing_score {
            if score >= filter_config.spam_threshold {
                // Was spam — re-move to spam folder
                log_debug(&debug_path, &format!(
                    "  Bounce-back detected: score {:.1}% (spam), re-moving to spam folder",
                    score
                ));
                if let Some(dest) = &filter_config.spam_folder_id {
                    move_item_to_folder(mail_item, dest, &debug_path);
                }
                return;
            } else if score >= filter_config.unsure_threshold {
                // Was unsure — re-move to unsure folder
                log_debug(&debug_path, &format!(
                    "  Bounce-back detected: score {:.1}% (unsure), re-moving to unsure folder",
                    score
                ));
                if let Some(dest) = &filter_config.unsure_folder_id {
                    move_item_to_folder(mail_item, dest, &debug_path);
                }
                return;
            }
        }

        // Score is ham or couldn't read score — just skip
        log_debug(&debug_path, &format!(
            "  Message already has score field '{}', classified as ham — skipping",
            general_config.field_score_name
        ));
        return;
    }

    // Skip messages that were just moved by user as ham (Not Spam button).
    // Spam moves that bounce back are handled by the score-check above
    // which will re-move them to the correct folder.
    if was_user_moved_as_ham(mail_item) {
        log_debug(&debug_path, "  Message was recently moved by user as ham, skipping");
        clear_user_moved(mail_item);
        return;
    }

    // Bounce-back detection for Exchange/Outlook.com/Hotmail stores where
    // the score couldn't be persisted to UserProperties. If we recently
    // filter-moved this message, use the in-memory score to re-move it.
    if let Some(score) = get_filter_moved_score(mail_item) {
        if score >= filter_config.spam_threshold {
            log_debug(&debug_path, &format!(
                "  Bounce-back detected (in-memory): score {:.1}% (spam), re-moving to spam folder",
                score
            ));
            if let Some(dest) = &filter_config.spam_folder_id {
                move_item_to_folder(mail_item, dest, &debug_path);
            }
            return;
        } else if score >= filter_config.unsure_threshold {
            log_debug(&debug_path, &format!(
                "  Bounce-back detected (in-memory): score {:.1}% (unsure), re-moving to unsure folder",
                score
            ));
            if let Some(dest) = &filter_config.unsure_folder_id {
                move_item_to_folder(mail_item, dest, &debug_path);
            }
            return;
        }
        // Score was ham — message should stay in watch folder
        log_debug(&debug_path, "  Previously filtered as ham (in-memory), skipping");
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
        // Log classifier state for debugging score discrepancies
        {
            if let Ok(c) = filter_engine.classifier().lock() {
                log_debug(&debug_path, &format!(
                    "  Classifier state: nham={}, nspam={}, word_info_size={}",
                    c.nham(), c.nspam(), c.word_info().len()
                ));
            }
        }
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

    // Record the classification in the statistics manager.
    if let Ok(state) = sink.state.lock() {
        if let Some(stats) = &state.statistics {
            stats.on_classified(result.classification);
        }
    }

    // Detect whether the source store is Exchange/Outlook.com/Hotmail.
    // These online stores don't support saving custom properties back, and
    // modifying a message in the Exchange "Junk Email" folder can trigger
    // server-side re-evaluation that bounces it back.
    let is_online_store = is_exchange_or_online_store(mail_item);
    let should_save_score = save_spam_info && !is_online_store;

    if is_online_store && save_spam_info {
        log_debug(&debug_path, "  Skipping score save: Exchange/Outlook.com/Hotmail store detected");
    }

    // Determine action based on classification (using local config copy)
    // For calendar items with calendar filtering enabled, use the calendar-specific
    // action instead of the normal filter action.
    if is_calendar_item {
        // Calendar-specific handling — re-read from disk (same logic as above)
        let calendar_config = {
            let data_dir = {
                let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
                std::path::PathBuf::from(local_app_data).join("SpamBayes")
            };
            let profile_name = {
                let state = match sink.state.lock() {
                    Ok(s) => s,
                    Err(_) => { return; }
                };
                if state.config.general.data_directory.is_empty() {
                    "default".to_string()
                } else {
                    state.config.general.data_directory.clone()
                }
            };
            spambayes_config::AppConfig::load(&data_dir, &profile_name)
                .map(|c| c.calendar)
                .unwrap_or_else(|_| {
                    let state = sink.state.lock().unwrap();
                    state.config.calendar.clone()
                })
        };

        log_debug(&debug_path, &format!(
            "  Calendar action phase: action={:?}", calendar_config.calendar_spam_action
        ));

        match result.classification {
            Classification::Ham => {
                log_debug(&debug_path, "  Calendar item classified as Ham, leaving in place");
            }
            Classification::Spam | Classification::Unsure => {
                match calendar_config.calendar_spam_action {
                    CalendarSpamAction::Prompt => {
                        log_debug(&debug_path, "  Calendar spam action: PROMPT — showing dialog");
                        let user_action = show_calendar_prompt(&subject, &sender);
                        match user_action {
                            CalendarPromptResult::Ham => {
                                log_debug(&debug_path, "  User chose: Train as Ham (leave in place)");
                            }
                            CalendarPromptResult::Delete => {
                                log_debug(&debug_path, "  User chose: Delete");
                                delete_item(mail_item, &debug_path);
                            }
                            CalendarPromptResult::DoNothing => {
                                log_debug(&debug_path, "  User chose: Do Nothing (leaving in place)");
                            }
                        }
                    }
                    CalendarSpamAction::Trash => {
                        log_debug(&debug_path, "  Calendar spam action: TRASH (deleting item)");
                        delete_item(mail_item, &debug_path);
                    }
                    CalendarSpamAction::Move => {
                        if let Some(dest_folder_id) = &filter_config.spam_folder_id {
                            log_debug(&debug_path, &format!(
                                "  Calendar spam action: MOVE to spam folder (entry={}...)",
                                &dest_folder_id.entry_id.0[..16.min(dest_folder_id.entry_id.0.len())]
                            ));
                            move_item_to_folder(mail_item, dest_folder_id, &debug_path);
                        } else {
                            log_debug(&debug_path, "  Calendar spam action: MOVE but no spam folder configured, leaving in place");
                        }
                    }
                }
            }
        }
    } else {
    // Normal mail action determination
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
                // Track in memory for bounce-back detection on Exchange stores
                // where we can't persist the score to UserProperties.
                if is_online_store {
                    mark_as_filter_moved(mail_item, result.score_pct);
                }
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
                if is_online_store {
                    mark_as_filter_moved(mail_item, result.score_pct);
                }
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

    // Save score AFTER the move, not before.
    // Saving before the move modifies the message in the Exchange-managed
    // source folder (e.g. "Junk Email"), which can trigger server-side
    // re-evaluation and bounce the message back.
    // For Exchange/Outlook.com/Hotmail stores, skip entirely — these
    // platforms don't support saving custom UserProperties.
    if should_save_score {
        save_score_to_item(mail_item, &general_config, result.score_pct);
    }

    } // end else (normal mail path)

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

/// Detect whether the mail item resides in an Exchange, Outlook.com, or Hotmail store.
///
/// These online stores do not reliably support saving custom user properties
/// back to messages. Additionally, modifying a message in an Exchange-managed
/// folder (like "Junk Email") can trigger server-side rule re-evaluation and
/// cause bounce-back loops.
///
/// Detection strategy: check `MailItem.Parent.Store.ExchangeStoreType`.
/// If the property exists and is non-zero (olExchangeMailbox=1, olExchangePublicFolder=2,
/// olPrimaryExchangeMailbox=3, olAdditionalExchangeMailbox=4), it's an Exchange store.
/// Outlook.com and Hotmail accounts also present as Exchange stores in Outlook.
unsafe fn is_exchange_or_online_store(mail_item: *mut c_void) -> bool {
    // Navigate: MailItem → Parent (folder) → Store → ExchangeStoreType
    let parent = match dispatch_get(mail_item, "Parent") {
        Ok(p) if !p.is_null() => p,
        _ => return false,
    };

    let store = match dispatch_get(parent, "Store") {
        Ok(s) if !s.is_null() => {
            release_dispatch(parent);
            s
        }
        _ => {
            release_dispatch(parent);
            return false;
        }
    };

    // ExchangeStoreType: 0 = olNotExchange, 1+ = some form of Exchange
    let exchange_type = get_long_property(store, "ExchangeStoreType");
    release_dispatch(store);

    // Any non-zero value means Exchange/Outlook.com/Hotmail
    exchange_type.unwrap_or(0) != 0
}

/// Get a Long (i32) property from an IDispatch object by name.
///
/// Returns None if the property doesn't exist or can't be read.
unsafe fn get_long_property(obj: *mut c_void, name: &str) -> Option<i32> {
    let dispid = get_dispid_on(obj, name)?;
    let vtbl = *(obj as *const *const IDispatchVtblRaw);

    let mut params = DispParams {
        rgvarg: ptr::null_mut(),
        rgdispid_named_args: ptr::null_mut(),
        c_args: 0,
        c_named_args: 0,
    };

    let mut result_var = Variant::default();
    let hr = ((*vtbl).invoke)(
        obj,
        dispid,
        &GUID::from_u128(0),
        0,
        2, // DISPATCH_PROPERTYGET
        &raw mut params,
        &raw mut result_var,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    if hr.0 != 0 {
        return None;
    }

    // VT_I4 = 3, VT_INT = 22 — accept either
    match result_var.vt {
        3 | 22 => Some(*(result_var.data.as_ptr().cast::<i32>())),
        _ => None,
    }
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

/// Delete a MailItem by calling MailItem.Delete().
unsafe fn delete_item(mail_item: *mut c_void, debug_path: &str) {
    match crate::com_invoke::dispatch_invoke_method(mail_item, "Delete", &[]) {
        Ok(_) => {
            log_debug(debug_path, "  Item deleted successfully");
        }
        Err(e) => {
            log_debug(debug_path, &format!("  Failed to delete item: {:?}", e));
        }
    }
}

// ─── Calendar Prompt Dialog ──────────────────────────────────────────────────

/// Result of the calendar spam prompt dialog.
enum CalendarPromptResult {
    /// User chose to treat as ham (not spam).
    Ham,
    /// User chose to delete the item.
    Delete,
    /// User chose to do nothing (leave in place, no training).
    DoNothing,
}

/// Show a Win32 MessageBox prompting the user about a calendar item classified
/// as spam. Uses a single Yes/No/Cancel dialog:
///   - Yes = Delete (it's spam, get rid of it)
///   - No = Keep (it's not spam, train as ham)
///   - Cancel = Do Nothing (leave in place, don't train)
///
/// This is safe to call from the COM STA thread.
fn show_calendar_prompt(subject: &str, sender: &str) -> CalendarPromptResult {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, IDNO,
        MB_ICONWARNING, MB_YESNOCANCEL,
    };

    let title = "SpamBayes - Spam Calendar Invitation";
    let message = format!(
        "A calendar invitation has been classified as spam.\n\n\
         Subject: {}\n\
         From: {}\n\n\
         ─────────────────────────────────\n\
         [Yes]       Delete this item\n\
         [No]        Keep it (not spam)\n\
         [Cancel]   Do nothing\n\
         ─────────────────────────────────",
        subject, sender
    );

    let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let msg_w: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        MessageBoxW(
            None,
            PCWSTR(msg_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_YESNOCANCEL | MB_ICONWARNING,
        )
    };

    match result {
        r if r == IDYES => CalendarPromptResult::Delete,
        r if r == IDNO => CalendarPromptResult::Ham,
        _ => CalendarPromptResult::DoNothing, // Cancel or closed
    }
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
        "setup_folder_hooks: build={}", env!("SPAMBAYES_BUILD_ID")
    ));
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

    // If calendar filtering is enabled, also hook the Inbox (olFolderInbox = 6)
    // because calendar meeting requests arrive in the Inbox, not Junk Email.
    {
        let calendar_enabled = match state.lock() {
            Ok(s) => s.config.calendar.calendar_filtering_enabled,
            Err(_) => false,
        };

        if calendar_enabled {
            log_debug(&debug_path, "setup_folder_hooks: calendar filtering enabled, hooking Inbox for meeting requests");
            match hook_default_folder(namespace, 6, Arc::clone(&state)) {
                Some(hook) => {
                    log_debug(&debug_path, &format!(
                        "Watching Inbox '{}' for calendar items", hook.folder_name
                    ));
                    hooks.push(hook);
                }
                None => {
                    log_debug(&debug_path, "setup_folder_hooks: failed to hook Inbox for calendar items");
                }
            }
        } else {
            log_debug(&debug_path, "setup_folder_hooks: calendar filtering disabled, not hooking Inbox");
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

    // Get the score field name and use_cached_scores from config
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

    let use_cached_scores = {
        let st = match state.lock() {
            Ok(s) => s,
            Err(_) => {
                release_dispatch(namespace);
                release_dispatch(app_ptr);
                return;
            }
        };
        st.config.filter.use_cached_scores
    };

    for hook in hooks {
        // Skip calendar-only hooks (Inbox) — don't scan existing items there
        if hook.calendar_only {
            log_debug(&debug_path, &format!(
                "scan_existing_items: skipping '{}' (calendar-only hook)", hook.folder_name
            ));
            continue;
        }
        scan_folder_items(
            &hook.items_ptr,
            &hook.folder_name,
            state,
            &score_field,
            &debug_path,
            use_cached_scores,
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
    use_cached_scores: bool,
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
        
        // Build a temporary sink reference to reuse handle_item_add
        let temp_sink = FolderItemsSink {
            vtbl: &raw const FOLDER_SINK_VTBL,
            ref_count: AtomicU32::new(1),
            state: Arc::clone(state),
            folder_name: folder_name.to_string(),
            calendar_only: false,
        };

        // If has_score and use_cached_scores, still call handle_item_add
        // because it has bounce-back logic to re-move spam/unsure messages
        // that ended up back in the watch folder. We only skip fully-scored
        // ham messages to avoid re-processing them.
        if has_score && use_cached_scores {
            // Read the existing score - if ham, skip; if spam/unsure, re-move
            let existing_score = get_score_value(item, score_field);
            let state_guard = match temp_sink.state.lock() {
                Ok(s) => s,
                Err(_) => {
                    release_dispatch(item);
                    continue;
                }
            };
            let unsure_threshold = state_guard.config.filter.unsure_threshold;
            drop(state_guard);
            
            if let Some(score) = existing_score {
                if score < unsure_threshold {
                    // Already scored as ham - skip
                    skipped += 1;
                    release_dispatch(item);
                    continue;
                }
                // Score is spam or unsure - go through handle_item_add for bounce-back
                log_debug(debug_path, &format!(
                    "  Scanned spam/unsure (score {:.1}%), processing for bounce-back", score
                ));
            } else {
                // Couldn't read score - go through handle_item_add
            }
        }

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
    let (cp_ptr, cookie) = match advise_items_event(items, state, &folder_name, false) {
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
        calendar_only: false,
    })
}

/// Hook a default Outlook folder by type constant (e.g., olFolderInbox = 6).
///
/// Uses Namespace.GetDefaultFolder(folder_type) to resolve the folder.
unsafe fn hook_default_folder(
    namespace: *mut c_void,
    folder_type: i32,
    state: Arc<Mutex<FolderHookState>>,
) -> Option<FolderHook> {
    let debug_path = debug_log_path();

    // Namespace.GetDefaultFolder(folder_type)
    let folder = match crate::com_invoke::dispatch_invoke_method(
        namespace, "GetDefaultFolder", &[VariantArg::I4(folder_type)]
    ) {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(&debug_path, &format!(
                "hook_default_folder: GetDefaultFolder({}) failed", folder_type
            ));
            return None;
        }
    };

    // Get folder name for logging
    let folder_name = dispatch_get_string(folder, "Name")
        .unwrap_or_else(|| format!("DefaultFolder({})", folder_type));

    log_debug(&debug_path, &format!(
        "hook_default_folder: resolved to '{}'", folder_name
    ));

    // Get folder.Items collection
    let items = match dispatch_get(folder, "Items") {
        Ok(p) if !p.is_null() => p,
        _ => {
            log_debug(&debug_path, "hook_default_folder: cannot get Items collection");
            release_dispatch(folder);
            return None;
        }
    };

    // Release the folder (we keep Items alive)
    release_dispatch(folder);

    // Connect our sink (calendar-only for default folder hooks)
    let (cp_ptr, cookie) = match advise_items_event(items, state, &folder_name, true) {
        Some(result) => result,
        None => {
            log_debug(&debug_path, "hook_default_folder: advise failed");
            release_dispatch(items);
            return None;
        }
    };

    Some(FolderHook {
        items_ptr: items,
        connection_point: cp_ptr,
        cookie,
        folder_name,
        calendar_only: true,
    })
}

/// Connect our FolderItemsSink to the Items collection's event connection point.
///
/// Returns (IConnectionPoint*, cookie) on success.
unsafe fn advise_items_event(
    items_ptr: *mut c_void,
    state: Arc<Mutex<FolderHookState>>,
    folder_name: &str,
    calendar_only: bool,
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
        calendar_only,
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

/// Get the numeric score value from a MailItem's UserProperties.
/// Returns None if the property doesn't exist or can't be read as a number.
unsafe fn get_score_value(mail_item: *mut c_void, score_field: &str) -> Option<f64> {
    let user_props = match dispatch_get(mail_item, "UserProperties") {
        Ok(p) if !p.is_null() => p,
        _ => return None,
    };

    let prop = dispatch_invoke_with_bstr(user_props, "Find", score_field);
    release_dispatch(user_props);

    if prop.is_null() {
        return None;
    }

    // Read the Value property as a string and parse as f64
    let value_str = dispatch_get_string(prop, "Value");
    release_dispatch(prop);

    value_str.and_then(|s| s.parse::<f64>().ok())
}
