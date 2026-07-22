//! SyncObject event sink for detecting Outlook sync completion.
//!
//! Implements a COM `IDispatch` sink that connects to Outlook's
//! `SyncObject.SyncEnd` event. When enabled, the add-in suppresses
//! background filtering until the initial sync completes, then processes
//! all queued messages.
//!
//! The event source interface is `SyncObjectEvents`:
//! IID: {00063085-0000-0000-C000-000000000046}
//! DISPIDs: SyncStart = 0xF001, SyncEnd = 0xF002

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, PCWSTR};

use crate::com_invoke::{dispatch_get, dispatch_invoke_method, VariantArg};

// ─── Constants ───────────────────────────────────────────────────────────────

/// IID for `SyncObjectEvents` dispatch source interface.
/// {00063085-0000-0000-C000-000000000046}
const IID_SYNC_OBJECT_EVENTS: GUID = GUID::from_u128(
    0x00063085_0000_0000_C000_000000000046,
);

/// DISPID for SyncEnd event.
const DISPID_SYNC_END: i32 = 0xF002;

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

// ─── Global Sync State ───────────────────────────────────────────────────────

/// Global flag indicating whether the initial Outlook sync has completed.
/// Starts as `false` when `timer_wait_for_sync` is enabled.
/// Set to `true` when `SyncEnd` fires or the timeout expires.
static SYNC_COMPLETE: AtomicBool = AtomicBool::new(true);

/// Returns `true` if the initial sync is complete (filtering may proceed).
///
/// When `timer_wait_for_sync` is disabled, this always returns `true`
/// because `SYNC_COMPLETE` is never reset to `false`.
#[inline]
pub fn is_sync_complete() -> bool {
    SYNC_COMPLETE.load(Ordering::Acquire)
}

/// Force-mark the sync as complete.
///
/// Called when:
/// - The `SyncEnd` event fires.
/// - The timeout fallback expires.
/// - The `SyncObject` connection fails (immediate fallback).
pub fn force_sync_complete() {
    SYNC_COMPLETE.store(true, Ordering::Release);
}

/// Reset the sync state to "not complete" (called at startup when
/// `timer_wait_for_sync` is enabled, before establishing the sync hook).
pub fn reset_sync_state() {
    SYNC_COMPLETE.store(false, Ordering::Release);
}

// ─── VARIANT / DISPPARAMS (local definitions) ────────────────────────────────

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

// ─── SyncSink ────────────────────────────────────────────────────────────────

/// COM event sink for Outlook `SyncObject.SyncEnd` events.
///
/// When `SyncEnd` fires (DISPID 0xF002), the sink sets `SYNC_COMPLETE`
/// to `true` and invokes the on-complete callback (which drains the
/// sync-pending queue).
#[repr(C)]
struct SyncSink {
    vtbl: *const IDispatchVtbl,
    ref_count: AtomicU32,
}

// SAFETY: SyncSink is allocated on the heap and accessed via COM
// pointers on the STA thread.
unsafe impl Send for SyncSink {}
unsafe impl Sync for SyncSink {}

static SYNC_SINK_VTBL: IDispatchVtbl = IDispatchVtbl {
    query_interface: sync_sink_qi,
    add_ref: sync_sink_add_ref,
    release: sync_sink_release,
    get_type_info_count: sync_sink_get_type_info_count,
    get_type_info: sync_sink_get_type_info,
    get_ids_of_names: sync_sink_get_ids_of_names,
    invoke: sync_sink_invoke,
};

// ─── IUnknown / IDispatch implementation for SyncSink ────────────────────────

unsafe extern "system" fn sync_sink_qi(
    this: *mut c_void,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return HRESULT(0x80004003_u32 as i32); // E_POINTER
    }
    let iid = &*riid;
    if *iid == IID_IUNKNOWN || *iid == IID_IDISPATCH || *iid == IID_SYNC_OBJECT_EVENTS {
        *ppv = this;
        sync_sink_add_ref(this);
        HRESULT(0)
    } else {
        *ppv = ptr::null_mut();
        HRESULT(0x80004002_u32 as i32) // E_NOINTERFACE
    }
}

unsafe extern "system" fn sync_sink_add_ref(this: *mut c_void) -> u32 {
    let sink = &*(this as *const SyncSink);
    sink.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn sync_sink_release(this: *mut c_void) -> u32 {
    let sink = &*(this as *const SyncSink);
    let prev = sink.ref_count.fetch_sub(1, Ordering::SeqCst);
    let new_count = prev - 1;
    if new_count == 0 {
        let _ = Box::from_raw(this as *mut SyncSink);
    }
    new_count
}

unsafe extern "system" fn sync_sink_get_type_info_count(
    _this: *mut c_void,
    pctinfo: *mut u32,
) -> HRESULT {
    if !pctinfo.is_null() {
        *pctinfo = 0;
    }
    HRESULT(0)
}

unsafe extern "system" fn sync_sink_get_type_info(
    _this: *mut c_void, _: u32, _: u32, _: *mut *mut c_void,
) -> HRESULT {
    HRESULT(0x80004001_u32 as i32) // E_NOTIMPL
}

unsafe extern "system" fn sync_sink_get_ids_of_names(
    _this: *mut c_void, _: *const GUID, _: *mut PCWSTR, _: u32, _: u32, _: *mut i32,
) -> HRESULT {
    HRESULT(0x80020006_u32 as i32) // DISP_E_UNKNOWNNAME
}

/// The main event handler. Outlook calls this when SyncStart or SyncEnd fires.
///
/// We only care about SyncEnd (DISPID 0xF002). When received, we mark
/// sync as complete and immediately drain the pending queue.
unsafe extern "system" fn sync_sink_invoke(
    _this: *mut c_void,
    disp_id: i32,
    _riid: *const GUID,
    _lcid: u32,
    _flags: u16,
    _params: *mut DispParams,
    _result: *mut Variant,
    _excep: *mut c_void,
    _arg_err: *mut u32,
) -> HRESULT {
    let log_path = sync_log_path();

    if disp_id == DISPID_SYNC_END {
        log_sync(&log_path, "SyncEnd event received — marking sync complete and draining queue");
        force_sync_complete();
        // Drain the sync-pending queue immediately. This is safe because
        // SyncEnd fires on the STA thread (same thread as ItemAdd events).
        crate::folder_sink::drain_sync_queue();
        // Kill the timeout timer since sync completed normally.
        {
            use windows::Win32::UI::WindowsAndMessaging::KillTimer;
            use windows::Win32::Foundation::HWND;
            KillTimer(HWND::default(), 0x5B08).ok(); // SYNC_TIMEOUT_TIMER_ID
        }
    } else {
        log_sync(&log_path, &format!(
            "SyncObjectEvents DISPID=0x{:X} received (ignored)", disp_id
        ));
    }

    HRESULT(0) // S_OK
}

// ─── SyncHook (connection lifecycle) ─────────────────────────────────────────

/// Represents an active sync event hook (advise connection on a SyncObject).
///
/// Holds the connection point and cookie for cleanup on disconnect.
pub struct SyncHook {
    /// The SyncObject IDispatch pointer we're sinking events on.
    sync_object_ptr: *mut c_void,
    /// The connection point pointer (needed for Unadvise).
    connection_point: *mut c_void,
    /// The advise cookie returned by IConnectionPoint::Advise.
    cookie: u32,
}

// SAFETY: SyncHook is only accessed from the COM STA thread.
unsafe impl Send for SyncHook {}

impl SyncHook {
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
            if !self.sync_object_ptr.is_null() {
                release_dispatch(self.sync_object_ptr);
                self.sync_object_ptr = ptr::null_mut();
            }
        }
    }
}

impl Drop for SyncHook {
    fn drop(&mut self) {
        self.disconnect();
    }
}

// ─── Public API: Setup Sync Hook ─────────────────────────────────────────────

/// Attempt to establish a sync event hook on Outlook's first SyncObject.
///
/// Accesses `Application.Session.SyncObjects.Item(1)` (the "All Accounts"
/// send/receive group) and connects our `SyncSink` to receive `SyncEnd`.
///
/// Returns `Some(SyncHook)` on success, `None` if the SyncObject could not
/// be accessed (e.g., no send/receive groups configured, or Outlook version
/// doesn't support this).
///
/// # Safety
/// Must be called from the COM STA thread. `app_ptr` must be a valid
/// Outlook.Application IDispatch pointer.
pub unsafe fn setup_sync_hook(app_ptr: *mut c_void) -> Option<SyncHook> {
    let log_path = sync_log_path();

    // The app_ptr from OnConnection doesn't reliably support GetIDsOfNames.
    // Use CoCreateInstance to get a proper Outlook.Application dispatch pointer
    // (same workaround as folder_sink::get_outlook_application).
    let app = get_outlook_application();
    if app.is_null() {
        // Fallback: try the passed-in pointer
        if app_ptr.is_null() {
            log_sync(&log_path, "setup_sync_hook: both CoCreateInstance and app_ptr failed");
            return None;
        }
        log_sync(&log_path, "setup_sync_hook: CoCreateInstance failed, trying passed-in app_ptr");
        return setup_sync_hook_with_app(app_ptr, false);
    }

    let result = setup_sync_hook_with_app(app, true);
    if result.is_none() {
        // Release the app pointer we got from CoCreateInstance
        release_dispatch(app);
    }
    result
}

/// Internal implementation that works with a given Application pointer.
/// If `release_app` is true, the app pointer was obtained via CoCreateInstance
/// and should be released on failure paths (on success, the SyncObject holds a ref).
unsafe fn setup_sync_hook_with_app(app_ptr: *mut c_void, release_app: bool) -> Option<SyncHook> {
    let log_path = sync_log_path();

    // Application.Session (returns Namespace)
    let session = match dispatch_get(app_ptr, "Session") {
        Ok(p) if !p.is_null() => {
            log_sync(&log_path, "setup_sync_hook: got Application.Session OK");
            p
        }
        Ok(_) => {
            log_sync(&log_path, "setup_sync_hook: Application.Session returned null pointer");
            if release_app { release_dispatch(app_ptr); }
            return None;
        }
        Err(hr) => {
            log_sync(&log_path, &format!(
                "setup_sync_hook: Application.Session failed (HRESULT={:#X})", hr.0 as u32
            ));
            if release_app { release_dispatch(app_ptr); }
            return None;
        }
    };

    // Session.SyncObjects (returns SyncObjects collection)
    let sync_objects = match dispatch_get(session, "SyncObjects") {
        Ok(p) if !p.is_null() => {
            log_sync(&log_path, "setup_sync_hook: got Session.SyncObjects OK");
            p
        }
        Ok(_) => {
            log_sync(&log_path, "setup_sync_hook: Session.SyncObjects returned null pointer");
            release_dispatch(session);
            if release_app { release_dispatch(app_ptr); }
            return None;
        }
        Err(hr) => {
            log_sync(&log_path, &format!(
                "setup_sync_hook: Session.SyncObjects failed (HRESULT={:#X})", hr.0 as u32
            ));
            release_dispatch(session);
            if release_app { release_dispatch(app_ptr); }
            return None;
        }
    };
    release_dispatch(session);

    // SyncObjects.Item(1) — the first (usually "All Accounts") group
    let sync_obj = match dispatch_invoke_method(
        sync_objects, "Item", &[VariantArg::I4(1)]
    ) {
        Ok(p) if !p.is_null() => {
            log_sync(&log_path, "setup_sync_hook: got SyncObjects.Item(1) OK");
            p
        }
        Ok(_) => {
            log_sync(&log_path, "setup_sync_hook: SyncObjects.Item(1) returned null pointer");
            release_dispatch(sync_objects);
            if release_app { release_dispatch(app_ptr); }
            return None;
        }
        Err(hr) => {
            log_sync(&log_path, &format!(
                "setup_sync_hook: SyncObjects.Item(1) failed (HRESULT={:#X})", hr.0 as u32
            ));
            release_dispatch(sync_objects);
            if release_app { release_dispatch(app_ptr); }
            return None;
        }
    };
    release_dispatch(sync_objects);
    if release_app { release_dispatch(app_ptr); }

    // Now advise our SyncSink on this SyncObject's SyncObjectEvents
    let result = advise_sync_event(sync_obj);
    match result {
        Some((cp_ptr, cookie)) => {
            log_sync(&log_path, "setup_sync_hook: SyncEnd hook established successfully");
            Some(SyncHook {
                sync_object_ptr: sync_obj,
                connection_point: cp_ptr,
                cookie,
            })
        }
        None => {
            log_sync(&log_path, "setup_sync_hook: advise failed on SyncObject");
            release_dispatch(sync_obj);
            None
        }
    }
}

/// Connect our SyncSink to the SyncObject's event connection point.
///
/// Returns (IConnectionPoint*, cookie) on success.
unsafe fn advise_sync_event(
    sync_obj: *mut c_void,
) -> Option<(*mut c_void, u32)> {
    // QI for IConnectionPointContainer on the SyncObject
    let vtbl = *(sync_obj as *const *const [usize; 3]);
    let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
        std::mem::transmute((*vtbl)[0]);

    let mut cpc_ptr: *mut c_void = ptr::null_mut();
    let hr = qi(sync_obj, &IID_ICONNECTION_POINT_CONTAINER, &raw mut cpc_ptr);
    if hr.0 != 0 || cpc_ptr.is_null() {
        return None;
    }

    // FindConnectionPoint for SyncObjectEvents
    let cpc_vtbl = *(cpc_ptr as *const *const IConnectionPointContainerVtbl);
    let mut cp_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*cpc_vtbl).find_connection_point)(
        cpc_ptr, &IID_SYNC_OBJECT_EVENTS, &raw mut cp_ptr
    );

    // Release CPC
    let cpc_release: unsafe extern "system" fn(*mut c_void) -> u32 =
        std::mem::transmute((*cpc_vtbl).release);
    cpc_release(cpc_ptr);

    if hr.0 != 0 || cp_ptr.is_null() {
        return None;
    }

    // Create our sink
    let sink = Box::new(SyncSink {
        vtbl: &raw const SYNC_SINK_VTBL,
        ref_count: AtomicU32::new(1),
    });
    let sink_ptr = Box::into_raw(sink) as *mut c_void;

    // Advise
    let cp_vtbl = *(cp_ptr as *const *const IConnectionPointVtbl);
    let mut cookie: u32 = 0;
    let hr = ((*cp_vtbl).advise)(cp_ptr, sink_ptr, &raw mut cookie);

    if hr.0 != 0 {
        // Advise failed — release our sink
        sync_sink_release(sink_ptr);
        // Release CP
        let cp_release: unsafe extern "system" fn(*mut c_void) -> u32 =
            std::mem::transmute((*cp_vtbl).release);
        cp_release(cp_ptr);
        return None;
    }

    Some((cp_ptr, cookie))
}

// ─── Utility Functions ───────────────────────────────────────────────────────

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
        return ptr::null_mut();
    }

    app_ptr
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

// ─── Debug Logging ───────────────────────────────────────────────────────────

fn sync_log_path() -> String {
    let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
    format!("{data_dir}\\SpamBayes\\folder_monitor.log")
}

/// Log a sync-related message to folder_monitor.log at importance level 1.
fn log_sync(path: &str, msg: &str) {
    use std::io::Write;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(
            f, "[{:02}:{:02}:{:02}] [sync] {}", hours, minutes, seconds, msg
        ));
}

// ─── Unit Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_state_default_is_complete() {
        // Default state is complete (no suppression when feature disabled)
        assert!(is_sync_complete());
    }

    #[test]
    fn test_reset_and_force_complete() {
        // Reset → not complete
        reset_sync_state();
        assert!(!is_sync_complete());

        // Force complete → complete again
        force_sync_complete();
        assert!(is_sync_complete());
    }

    #[test]
    fn test_force_complete_is_idempotent() {
        force_sync_complete();
        assert!(is_sync_complete());
        force_sync_complete();
        assert!(is_sync_complete());
    }
}
