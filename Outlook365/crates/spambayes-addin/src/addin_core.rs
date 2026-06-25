//! `IDTExtensibility2` COM interface implementation for the `SpamBayes` add-in.
//!
//! This module implements the core add-in lifecycle using a raw vtable COM
//! pattern identical to `class_factory.rs`. The `AddinCore` struct holds all
//! runtime state (MAPI session, classifier, filter engine, etc.) and
//! implements the five `IDTExtensibility2` callbacks plus `IUnknown`.
//!
//! **Validates: Requirements 1.1, 1.3, 1.4, 1.6, 1.7, 1.8**

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{GUID, HRESULT};

use spambayes_config::AppConfig;
use spambayes_core::classifier::Classifier;
use spambayes_storage::{MmapDbmBackend, StorageBackend};

use crate::error_reporter::ErrorReporter;
use crate::filter::FilterEngine;
use crate::logger::Logger;
use crate::notification::NotificationManager;
use crate::timer::TimerFilterState;
use crate::train::TrainingEngine;
use crate::{
    dll_add_ref, dll_release, E_NOINTERFACE, E_POINTER, IID_IUNKNOWN, LogLevel, S_OK,
};

// ─── COM Interface IDs ───────────────────────────────────────────────────────

/// IID for `IDTExtensibility2`: {B65AD801-ABAF-11D0-BB8B-00A0C90F2744}
const IID_IDTEXTENSIBILITY2: GUID = GUID::from_u128(
    0xB65AD801_ABAF_11D0_BB8B_00A0C90F2744,
);

/// IID for `IDispatch`: {00020400-0000-0000-C000-000000000046}
const IID_IDISPATCH: GUID = GUID::from_u128(
    0x00020400_0000_0000_C000_000000000046,
);

// ─── Deferred Toolbar Setup (Timer) ─────────────────────────────────────────

/// Timer ID for deferred toolbar creation.
const TOOLBAR_TIMER_ID: usize = 0x5B01;

/// Stored Application pointer for the button-click polling timer.
static mut POLL_APP_PTR: *mut c_void = std::ptr::null_mut();

/// Stored Application pointer (unused but cleared in timer proc for safety).
static mut PENDING_TOOLBAR_APP_PTR: *mut c_void = std::ptr::null_mut();

/// Timer ID for button click polling.
const BUTTON_POLL_TIMER_ID: usize = 0x5B02;

/// Timer ID for deferred folder hook setup.
const FOLDER_HOOK_TIMER_ID: usize = 0x5B03;

/// Global pointer to the AddinCore instance for timer callbacks.
/// SAFETY: Only accessed from the COM STA thread.
static mut GLOBAL_ADDIN_PTR: *mut AddinCore = std::ptr::null_mut();

/// Timer callback that polls CommandBars.ActionControl for button clicks.
unsafe extern "system" fn button_poll_timer_proc(
    _hwnd: windows::Win32::Foundation::HWND,
    _msg: u32,
    _id_event: usize,
    _dw_time: u32,
) {
    use crate::com_invoke::{dispatch_get, dispatch_invoke_method, VariantArg};

    let app_ptr = POLL_APP_PTR;
    if app_ptr.is_null() {
        return;
    }

    // Get Application.CommandBars.ActionControl
    // ActionControl returns the button that was most recently clicked
    let command_bars = match dispatch_get(app_ptr, "CommandBars") {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };

    let action_ctrl = match dispatch_get(command_bars, "ActionControl") {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };

    // Get the Tag of the clicked control
    // We read Tag as a BSTR via IDispatch
    let tag_dispid = match crate::com_invoke::dispatch_get_string(action_ctrl, "Tag") {
        Some(tag) => tag,
        None => return,
    };

    // Check if it's one of our buttons and handle it
    match tag_dispid.as_str() {
        "SpamBayesCommand.Manager" => {
            // Reset ActionControl by setting it to empty (prevent re-triggering)
            AddinCore::launch_manager();
        }
        "SpamBayesCommand.DeleteAsSpam" => {
            // TODO: handle spam button
        }
        "SpamBayesCommand.RecoverFromSpam" => {
            // TODO: handle not-spam button
        }
        _ => {}
    }
}

/// Timer callback that creates the toolbar after OnStartupComplete returns.
unsafe extern "system" fn toolbar_timer_proc(
    _hwnd: windows::Win32::Foundation::HWND,
    _msg: u32,
    id_event: usize,
    _dw_time: u32,
) {
    use windows::Win32::UI::WindowsAndMessaging::KillTimer;
    use windows::Win32::Foundation::HWND;

    // Kill the timer immediately (one-shot)
    KillTimer(HWND::default(), id_event).ok();

    PENDING_TOOLBAR_APP_PTR = std::ptr::null_mut();

    let debug_path = {
        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        format!("{data_dir}\\SpamBayes\\addin_debug.log")
    };

    let _ = std::fs::OpenOptions::new().append(true).create(true).open(&debug_path)
        .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar_timer_proc: FIRED") });

    // Get Application via CoCreateInstance("Outlook.Application")
    // CLSID for Outlook.Application: {0006F03A-0000-0000-C000-000000000046}
    let outlook_clsid = GUID::from_u128(0x0006F03A_0000_0000_C000_000000000046);

    // Use raw COM call since windows crate CoCreateInstance has generics
    #[link(name = "ole32")]
    extern "system" {
        fn CoCreateInstance(
            rclsid: *const GUID,
            p_unk_outer: *mut std::ffi::c_void,
            dw_cls_context: u32,
            riid: *const GUID,
            ppv: *mut *mut std::ffi::c_void,
        ) -> i32;
    }

    let iid_dispatch = GUID::from_u128(0x00020400_0000_0000_C000_000000000046);
    let mut app_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = CoCreateInstance(
        &outlook_clsid,
        std::ptr::null_mut(),
        4, // CLSCTX_LOCAL_SERVER
        &iid_dispatch,
        &raw mut app_ptr,
    );

    let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
        .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar_timer_proc: CoCreateInstance hr={:#X} ptr={:?}", hr, app_ptr) });

    if hr != 0 || app_ptr.is_null() {
        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar_timer_proc: Failed to get Outlook.Application") });
        return;
    }

    let images_dir = AddinCore::get_data_directory().join("images");
    let mut toolbar_mgr = crate::toolbar::ToolbarManager::new(app_ptr, images_dir);

    let result = toolbar_mgr.execute_setup();
    match result {
        Ok(()) => {
            let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar_timer_proc: SUCCESS") });
            // Start button click polling timer
            POLL_APP_PTR = app_ptr;
            use windows::Win32::UI::WindowsAndMessaging::SetTimer;
            SetTimer(HWND::default(), BUTTON_POLL_TIMER_ID, 200, Some(button_poll_timer_proc));
        }
        Err(e) => {
            let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar_timer_proc: FAILED: {}", e) });
        }
    }
}

/// Timer callback that sets up folder hooks after OnStartupComplete returns.
///
/// Deferred because direct COM calls during OnStartupComplete can fail with
/// RPC_E_CANTCALLOUT_ININPUTSYNCCALL. By the time this timer fires (2.5s),
/// Outlook's message pump is fully running.
unsafe extern "system" fn folder_hook_timer_proc(
    _hwnd: windows::Win32::Foundation::HWND,
    _msg: u32,
    id_event: usize,
    _dw_time: u32,
) {
    use windows::Win32::UI::WindowsAndMessaging::KillTimer;
    use windows::Win32::Foundation::HWND;

    // Kill the timer immediately (one-shot)
    KillTimer(HWND::default(), id_event).ok();

    let debug_path = {
        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        format!("{data_dir}\\SpamBayes\\addin_debug.log")
    };

    let _ = std::fs::OpenOptions::new().append(true).create(true).open(&debug_path)
        .and_then(|mut f| { use std::io::Write; writeln!(f, "folder_hook_timer_proc: FIRED") });

    let addin = GLOBAL_ADDIN_PTR;
    if addin.is_null() {
        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "folder_hook_timer_proc: GLOBAL_ADDIN_PTR is null") });
        return;
    }

    let addin = &mut *addin;
    addin.setup_folder_hooks();

    let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
        .and_then(|mut f| { use std::io::Write; writeln!(f, "folder_hook_timer_proc: COMPLETE") });
}

// ─── ext_ConnectMode ─────────────────────────────────────────────────────────

/// Connection mode passed to `OnConnection`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ConnectMode {
    /// Connected on startup.
    Startup = 0,
    /// Connected after startup (e.g., via COM Add-ins dialog).
    AfterStartup = 1,
    /// Connected externally (command-line, etc.).
    External = 2,
    /// Connected via command-line.
    CommandLine = 3,
}

/// Disconnection mode passed to `OnDisconnection`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DisconnectMode {
    /// Host is shutting down.
    HostShutdown = 0,
    /// User removed the add-in.
    UserClosed = 1,
}

// ─── HRESULT Constants ───────────────────────────────────────────────────────

/// `E_FAIL` (0x80004005)
const E_FAIL: HRESULT = HRESULT(0x80004005_u32 as i32);

// ─── VTable Definition ───────────────────────────────────────────────────────

/// `IDTExtensibility2` vtable layout (extends `IDispatch` which extends `IUnknown`).
///
/// This matches the COM binary standard for `IDTExtensibility2` exactly.
/// The inheritance chain is: IUnknown (3) → IDispatch (4) → IDTExtensibility2 (5).
/// All 12 slots must be present for correct vtable layout.
#[repr(C)]
struct IDTExtensibility2Vtbl {
    // IUnknown methods (3)
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IDispatch methods (4)
    get_type_info_count: unsafe extern "system" fn(
        this: *mut c_void,
        pctinfo: *mut u32,
    ) -> HRESULT,
    get_type_info: unsafe extern "system" fn(
        this: *mut c_void,
        i_tinfo: u32,
        lcid: u32,
        pp_tinfo: *mut *mut c_void,
    ) -> HRESULT,
    get_ids_of_names: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const GUID,
        rgsz_names: *mut *mut u16,
        c_names: u32,
        lcid: u32,
        rg_disp_id: *mut i32,
    ) -> HRESULT,
    invoke: unsafe extern "system" fn(
        this: *mut c_void,
        disp_id_member: i32,
        riid: *const GUID,
        lcid: u32,
        w_flags: u16,
        p_disp_params: *mut c_void,
        p_var_result: *mut c_void,
        p_excep_info: *mut c_void,
        pu_arg_err: *mut u32,
    ) -> HRESULT,
    // IDTExtensibility2 methods (5)
    on_connection: unsafe extern "system" fn(
        this: *mut c_void,
        application: *mut c_void,
        connect_mode: i32,
        add_in_inst: *mut c_void,
        custom: *mut c_void,
    ) -> HRESULT,
    on_disconnection: unsafe extern "system" fn(
        this: *mut c_void,
        remove_mode: i32,
        custom: *mut c_void,
    ) -> HRESULT,
    on_add_ins_update: unsafe extern "system" fn(
        this: *mut c_void,
        custom: *mut c_void,
    ) -> HRESULT,
    on_startup_complete: unsafe extern "system" fn(
        this: *mut c_void,
        custom: *mut c_void,
    ) -> HRESULT,
    on_begin_shutdown: unsafe extern "system" fn(
        this: *mut c_void,
        custom: *mut c_void,
    ) -> HRESULT,
}

/// `E_NOTIMPL` (0x80004001)
const E_NOTIMPL: HRESULT = HRESULT(0x80004001_u32 as i32);

/// `DISP_E_UNKNOWNNAME` (0x80020006)
const DISP_E_UNKNOWNNAME: HRESULT = HRESULT(0x80020006_u32 as i32);

/// Static vtable instance for `AddinCore`. Shared by all instances.
static ADDIN_CORE_VTBL: IDTExtensibility2Vtbl = IDTExtensibility2Vtbl {
    query_interface: AddinCore::query_interface,
    add_ref: AddinCore::add_ref,
    release: AddinCore::release,
    get_type_info_count: AddinCore::get_type_info_count,
    get_type_info: AddinCore::get_type_info,
    get_ids_of_names: AddinCore::get_ids_of_names,
    invoke: AddinCore::invoke,
    on_connection: AddinCore::on_connection_raw,
    on_disconnection: AddinCore::on_disconnection_raw,
    on_add_ins_update: AddinCore::on_add_ins_update_raw,
    on_startup_complete: AddinCore::on_startup_complete_raw,
    on_begin_shutdown: AddinCore::on_begin_shutdown_raw,
};

// ─── AddinCore ───────────────────────────────────────────────────────────────

/// Core COM object implementing `IDTExtensibility2` for the `SpamBayes` add-in.
///
/// This struct is heap-allocated by [`ClassFactory::CreateInstance`] and
/// represents the running add-in instance. It holds all shared state:
/// - The Outlook Application COM reference
/// - MAPI session and message store
/// - Classifier and storage (shared via Arc<Mutex<>>)
/// - Configuration
/// - Filter engine, training engine, notification manager
/// - Logger and error reporter
///
/// **Validates: Requirement 1.1**
#[repr(C)]
pub struct AddinCore {
    /// Pointer to the static vtable (must be the first field for COM compat).
    vtbl: *const IDTExtensibility2Vtbl,
    /// Atomic reference count for `IUnknown`.
    ref_count: AtomicU32,

    // ── Runtime State ────────────────────────────────────────────────────

    /// The Outlook.Application `IDispatch` pointer (stored on `OnConnection`).
    application: Option<*mut c_void>,
    /// MAPI session — initialized during `OnConnection`.
    #[cfg(target_os = "windows")]
    mapi_session: Option<spambayes_mapi::MapiSessionImpl>,
    /// Whether MAPI initialization succeeded.
    mapi_initialized: bool,
    /// Application configuration loaded from INI files.
    config: Option<AppConfig>,
    /// Shared Bayesian classifier instance.
    classifier: Option<Arc<Mutex<Classifier>>>,
    /// Shared storage backend for classifier persistence.
    storage: Option<Arc<Mutex<Box<dyn StorageBackend>>>>,
    /// Filter engine for scoring and classifying messages.
    filter_engine: Option<FilterEngine>,
    /// Timer-based filtering state machine.
    timer_state: Option<TimerFilterState>,
    /// Training engine for batch and incremental learning.
    training_engine: Option<TrainingEngine>,
    /// Notification sound manager.
    notification_mgr: Option<NotificationManager>,
    /// Centralized logger.
    logger: Option<Arc<Logger>>,
    /// Once-per-session error reporter.
    error_reporter: ErrorReporter,
    /// Whether the add-in has been fully initialized.
    initialized: bool,
    /// Active folder event hooks for watched folders.
    folder_hooks: Vec<crate::folder_sink::FolderHook>,
    /// Shared state for folder event sinks.
    folder_hook_state: Option<Arc<Mutex<crate::folder_sink::FolderHookState>>>,
}

// SAFETY: AddinCore is only accessed from the COM apartment thread (STA).
unsafe impl Send for AddinCore {}

impl AddinCore {
    /// Creates a new `AddinCore` instance on the heap.
    ///
    /// Returns a raw pointer suitable for use as a COM interface pointer.
    /// The initial reference count is 1. The caller owns this reference.
    #[allow(clippy::new_ret_no_self)]
    #[must_use]
    pub fn new() -> *mut c_void {
        let addin = Box::new(AddinCore {
            vtbl: &raw const ADDIN_CORE_VTBL,
            ref_count: AtomicU32::new(1),
            application: None,
            #[cfg(target_os = "windows")]
            mapi_session: None,
            mapi_initialized: false,
            config: None,
            classifier: None,
            storage: None,
            filter_engine: None,
            timer_state: None,
            training_engine: None,
            notification_mgr: None,
            logger: None,
            error_reporter: ErrorReporter::new(),
            initialized: false,
            folder_hooks: Vec::new(),
            folder_hook_state: None,
        });
        // Increment global DLL lock count for this COM object.
        dll_add_ref();
        Box::into_raw(addin).cast::<c_void>()
    }

    // ─── IUnknown ────────────────────────────────────────────────────────

    /// `IUnknown::QueryInterface` implementation.
    ///
    /// Supports `IID_IUnknown`, `IID_IDTExtensibility2`, and `IID_IDispatch`
    /// (returns the same pointer since our vtable starts at offset 0).
    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        if ppv.is_null() {
            return E_POINTER;
        }
        *ppv = std::ptr::null_mut();

        if riid.is_null() {
            return E_POINTER;
        }

        let iid = &*riid;

        // IID for _CommandBarButtonEvents: {000C033E-0000-0000-C000-000000000046}
        // Outlook QIs for this when a button with OnAction="<!ProgID>" is clicked.
        const IID_COMMANDBAR_BUTTON_EVENTS: GUID = GUID::from_u128(
            0x000C033E_0000_0000_C000_000000000046,
        );

        // IRibbonExtensibility: {000C0396-0000-0000-C000-000000000046}
        // Outlook QIs for this at startup to get ribbon XML customization.
        const IID_RIBBON_EXTENSIBILITY: GUID = GUID::from_u128(
            0x000C0396_0000_0000_C000_000000000046,
        );

        if *iid == IID_IUNKNOWN || *iid == IID_IDTEXTENSIBILITY2 || *iid == IID_IDISPATCH
            || *iid == IID_COMMANDBAR_BUTTON_EVENTS
        {
            *ppv = this;
            Self::add_ref(this);
            S_OK
        } else if *iid == IID_RIBBON_EXTENSIBILITY {
            // Return a SEPARATE COM object with the correct vtable for IRibbonExtensibility
            let ribbon_obj = crate::ribbon::RibbonExtensibility::new(this);
            *ppv = ribbon_obj;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }

    /// `IUnknown::AddRef` implementation.
    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        let addin = &*(this as *const AddinCore);
        addin.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// `IUnknown::Release` implementation.
    ///
    /// When the reference count reaches 0, the `AddinCore` is deallocated
    /// and the global DLL lock is released.
    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let addin = &*(this as *const AddinCore);
        let prev = addin.ref_count.fetch_sub(1, Ordering::SeqCst);
        let new_count = prev - 1;

        if new_count == 0 {
            let _ = Box::from_raw(this.cast::<AddinCore>());
            dll_release();
        }

        new_count
    }

    // ─── IDispatch (stub implementations) ────────────────────────────────

    /// `IDispatch::GetTypeInfoCount` — we don't provide type info.
    unsafe extern "system" fn get_type_info_count(
        _this: *mut c_void,
        pctinfo: *mut u32,
    ) -> HRESULT {
        if !pctinfo.is_null() {
            *pctinfo = 0;
        }
        S_OK
    }

    /// `IDispatch::GetTypeInfo` — not implemented (no type library).
    unsafe extern "system" fn get_type_info(
        _this: *mut c_void,
        _i_tinfo: u32,
        _lcid: u32,
        _pp_tinfo: *mut *mut c_void,
    ) -> HRESULT {
        E_NOTIMPL
    }

    /// `IDispatch::GetIDsOfNames` — resolves IDTExtensibility2 method names to DISPIDs.
    unsafe extern "system" fn get_ids_of_names(
        _this: *mut c_void,
        _riid: *const GUID,
        rgsz_names: *mut *mut u16,
        c_names: u32,
        _lcid: u32,
        rg_disp_id: *mut i32,
    ) -> HRESULT {
        if rgsz_names.is_null() || rg_disp_id.is_null() || c_names == 0 {
            return E_POINTER;
        }

        // Only handle the first name (method name)
        let name_ptr = *rgsz_names;
        if name_ptr.is_null() {
            return DISP_E_UNKNOWNNAME;
        }

        // Read the wide string
        let mut len = 0usize;
        while *name_ptr.add(len) != 0 {
            len += 1;
        }
        let name_slice = std::slice::from_raw_parts(name_ptr, len);
        let name = String::from_utf16_lossy(name_slice);

        let dispid = match name.as_str() {
            "OnConnection" => 1,
            "OnDisconnection" => 2,
            "OnAddInsUpdate" => 3,
            "OnStartupComplete" => 4,
            "OnBeginShutdown" => 5,
            // IRibbonExtensibility
            "wireCall" | "wireCall2" => 6, // OnLoad uses wireCall internally
            "wireCall3" => 7,
            "wireCall4" => 8,
            "wireCall5" => 9,
            "wireCall6" => 10,
            "wireCall7" => 11,
            "wireCall8" => 12,
            "wireCall9" => 13,
            "wireCall10" => 14,
            "wireCall11" => 15,
            "wireCall12" => 16,
            "wireCall13" => 17,
            "wireCall14" => 18,
            "GetCustomUI" => 100,
            // Ribbon button callbacks
            "OnSpamClick" => 101,
            "OnNotSpamClick" => 102,
            "OnManagerClick" => 103,
            "Ribbon_OnLoad" => 104,
            "GetSpamEnabled" => 105,
            "GetNotSpamEnabled" => 106,
            "GetNotSpamVisible" => 107,
            _ => {
                *rg_disp_id = -1; // DISPID_UNKNOWN
                return DISP_E_UNKNOWNNAME;
            }
        };

        *rg_disp_id = dispid;
        S_OK
    }

    /// `IDispatch::Invoke` — dispatches IDTExtensibility2 method calls by DISPID.
    ///
    /// DISPIDs for IDTExtensibility2:
    /// - 1 = OnConnection(Application, ConnectMode, AddInInst, Custom)
    /// - 2 = OnDisconnection(RemoveMode, Custom)
    /// - 3 = OnAddInsUpdate(Custom)
    /// - 4 = OnStartupComplete(Custom)
    /// - 5 = OnBeginShutdown(Custom)
    unsafe extern "system" fn invoke(
        this: *mut c_void,
        disp_id_member: i32,
        _riid: *const GUID,
        _lcid: u32,
        _w_flags: u16,
        p_disp_params: *mut c_void,
        _p_var_result: *mut c_void,
        _p_excep_info: *mut c_void,
        _pu_arg_err: *mut u32,
    ) -> HRESULT {
        // DISPPARAMS layout: rgvarg (*VARIANT), rgdispidNamedArgs (*DISPID), cArgs (u32), cNamedArgs (u32)
        #[repr(C)]
        struct DispParams {
            rgvarg: *mut c_void,
            rgdispid_named_args: *mut i32,
            c_args: u32,
            c_named_args: u32,
        }

        let addin = &mut *this.cast::<AddinCore>();

        match disp_id_member {
            1 => {
                // This can be either:
                // - OnConnection(Application, ConnectMode, AddInInst, Custom) — 4 args
                // - _CommandBarButtonEvents::Click(Ctrl, CancelDefault) — 2 args
                let mut arg_count: u32 = 0;
                if !p_disp_params.is_null() {
                    let params = &*(p_disp_params as *const DispParams);
                    arg_count = params.c_args;
                }

                if arg_count >= 4 {
                    // OnConnection(Application, ConnectMode, AddInInst, Custom)
                    let mut app_ptr: *mut c_void = std::ptr::null_mut();
                    if !p_disp_params.is_null() {
                        let params = &*(p_disp_params as *const DispParams);
                        if params.c_args >= 4 && !params.rgvarg.is_null() {
                            #[cfg(target_pointer_width = "64")]
                            const VARIANT_SIZE: usize = 24;
                            #[cfg(target_pointer_width = "32")]
                            const VARIANT_SIZE: usize = 16;

                            let app_variant_base = (params.rgvarg as *const u8)
                                .add(3 * VARIANT_SIZE);
                            let data_ptr = app_variant_base.add(8) as *const *mut c_void;
                            app_ptr = *data_ptr;
                        }
                    }

                    // QI for IDispatch on the Application pointer
                    if !app_ptr.is_null() {
                        let iid_idispatch = GUID::from_u128(0x00020400_0000_0000_C000_000000000046);
                        let vtbl = *(app_ptr as *const *const usize);
                        let qi_fn: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
                            std::mem::transmute(*vtbl);
                        let mut disp_ptr: *mut c_void = std::ptr::null_mut();
                        let hr = qi_fn(app_ptr, &iid_idispatch, &raw mut disp_ptr);
                        if hr.is_ok() && !disp_ptr.is_null() {
                            app_ptr = disp_ptr;
                        }
                    }

                    addin.handle_on_connection(app_ptr)
                } else {
                    // Button Click event — launch the manager
                    let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
                    let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "Button CLICK received! args={}", arg_count) });

                    Self::launch_manager();
                    S_OK
                }
            }
            2 => {
                // OnDisconnection(RemoveMode, Custom)
                addin.handle_on_disconnection()
            }
            3 => {
                // OnAddInsUpdate(Custom)
                S_OK
            }
            4 => {
                // OnStartupComplete(Custom)
                addin.handle_on_startup_complete()
            }
            5 => {
                // OnBeginShutdown(Custom)
                addin.handle_on_begin_shutdown()
            }
            _ => {
                // Log unknown DISPIDs — might be button click callbacks
                let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
                let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "Invoke: dispid={}", disp_id_member) });

                match disp_id_member {
                    100 => {
                        // GetCustomUI(RibbonID) -> returns BSTR with XML
                        if !_p_var_result.is_null() {
                            let xml = Self::get_ribbon_xml();
                            // Write VARIANT { vt: VT_BSTR(8), data: BSTR ptr }
                            // VARIANT on 64-bit: [vt:2][pad:6][data:8] = 16 usable bytes at minimum
                            let var_ptr = _p_var_result as *mut u8;
                            // Set vt = VT_BSTR (8)
                            *(var_ptr as *mut u16) = 8;
                            // Zero reserved fields
                            *(var_ptr.add(2) as *mut u16) = 0;
                            *(var_ptr.add(4) as *mut u16) = 0;
                            *(var_ptr.add(6) as *mut u16) = 0;
                            // Allocate BSTR and store at offset 8
                            extern "system" { fn SysAllocString(psz: *const u16) -> *mut u16; }
                            let wide: Vec<u16> = xml.encode_utf16().chain(std::iter::once(0)).collect();
                            let bstr = SysAllocString(wide.as_ptr());
                            *(var_ptr.add(8) as *mut *mut u16) = bstr;
                        }
                        S_OK
                    }
                    101 => {
                        // OnSpamClick
                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                            .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnSpamClick!") });
                        // TODO: implement spam training
                        S_OK
                    }
                    102 => {
                        // OnNotSpamClick
                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                            .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnNotSpamClick!") });
                        // TODO: implement ham recovery
                        S_OK
                    }
                    103 => {
                        // OnManagerClick - launch the tkinter manager
                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                            .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnManagerClick!") });
                        Self::launch_manager();
                        S_OK
                    }
                    104 => {
                        // Ribbon_OnLoad - store the IRibbonUI reference
                        S_OK
                    }
                    105 | 106 => {
                        // GetSpamEnabled / GetNotSpamEnabled - return True
                        if !_p_var_result.is_null() {
                            let result = &mut *(_p_var_result as *mut [u8; 24]);
                            result[0] = 11; result[1] = 0; // VT_BOOL
                            let bool_slot = &mut *(result.as_mut_ptr().add(8) as *mut i16);
                            *bool_slot = -1; // VARIANT_TRUE
                        }
                        S_OK
                    }
                    107 => {
                        // GetNotSpamVisible - return True
                        if !_p_var_result.is_null() {
                            let result = &mut *(_p_var_result as *mut [u8; 24]);
                            result[0] = 11; result[1] = 0; // VT_BOOL
                            let bool_slot = &mut *(result.as_mut_ptr().add(8) as *mut i16);
                            *bool_slot = -1; // VARIANT_TRUE
                        }
                        S_OK
                    }
                    _ => S_OK
                }
            }
        }
    }

    // ─── IDTExtensibility2 Raw Vtable Entries ────────────────────────────

    /// Raw vtable entry for `OnConnection`.
    unsafe extern "system" fn on_connection_raw(
        this: *mut c_void,
        application: *mut c_void,
        _connect_mode: i32,
        _add_in_inst: *mut c_void,
        _custom: *mut c_void,
    ) -> HRESULT {
        // Log to confirm vtable path is used
        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "on_connection_raw (VTABLE): app={:?}", application) });

        let addin = &mut *this.cast::<AddinCore>();
        addin.handle_on_connection(application)
    }

    /// Raw vtable entry for `OnDisconnection`.
    unsafe extern "system" fn on_disconnection_raw(
        this: *mut c_void,
        _remove_mode: i32,
        _custom: *mut c_void,
    ) -> HRESULT {
        let addin = &mut *this.cast::<AddinCore>();
        addin.handle_on_disconnection()
    }

    /// Raw vtable entry for `OnAddInsUpdate`.
    unsafe extern "system" fn on_add_ins_update_raw(
        _this: *mut c_void,
        _custom: *mut c_void,
    ) -> HRESULT {
        // No action needed for OnAddInsUpdate.
        S_OK
    }

    /// Raw vtable entry for `OnStartupComplete`.
    unsafe extern "system" fn on_startup_complete_raw(
        this: *mut c_void,
        _custom: *mut c_void,
    ) -> HRESULT {
        let addin = &mut *this.cast::<AddinCore>();
        addin.handle_on_startup_complete()
    }

    /// Raw vtable entry for `OnBeginShutdown`.
    unsafe extern "system" fn on_begin_shutdown_raw(
        this: *mut c_void,
        _custom: *mut c_void,
    ) -> HRESULT {
        let addin = &mut *this.cast::<AddinCore>();
        addin.handle_on_begin_shutdown()
    }

    // ─── Lifecycle Implementations ───────────────────────────────────────

    /// `OnConnection`: store Application ref, init MAPI, load config, load DB,
    /// setup filter.
    ///
    /// **Validates: Requirements 1.1, 1.3, 1.6, 1.7, 1.8**
    fn handle_on_connection(&mut self, application: *mut c_void) -> HRESULT {
        // Early debug file to diagnose load issues (before logger is available).
        let debug_path = Self::get_data_directory().join("addin_debug.log");
        let _ = std::fs::write(&debug_path, "OnConnection: ENTRY\r\n");

        // Store the Outlook Application reference.
        // We must AddRef to keep it alive beyond this call.
        // The pointer comes from the vtable-based OnConnection call.
        if !application.is_null() {
            // IUnknown vtable: [0]=QueryInterface, [1]=AddRef, [2]=Release
            // The first pointer-sized value at *application is the vtable pointer.
            unsafe {
                let vtbl_ptr = *(application as *const *const usize);
                let add_ref: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(*vtbl_ptr.add(1));
                add_ref(application);
            }
        }
        self.application = Some(application);

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnConnection: Application stored, ptr={:?}", application)
            });

        // Initialize the logger first so all subsequent operations can log.
        let log_path = Logger::default_path();
        if let Ok(logger) = Logger::new(&log_path, LogLevel::Info) {
            let logger = Arc::new(logger);
            logger.info("addin_core", "OnConnection: SpamBayes add-in starting");
            self.logger = Some(logger);
        }

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnConnection: Logger initialized")
            });

        // Load configuration.
        let config = self.load_config();
        self.config = Some(config);

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnConnection: Config loaded")
            });

        // Initialize MAPI session.
        // Requirement 1.8: If MAPI init fails, show error and don't proceed
        // with classifier/filter setup.
        if !self.initialize_mapi() {
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(&debug_path)
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "OnConnection: MAPI init FAILED (non-fatal)")
                });
            self.log_info("OnConnection: MAPI initialization failed, skipping classifier and filter setup");
            self.error_reporter.report_once(
                HRESULT(E_FAIL.0),
                "SpamBayes failed to initialize the MAPI session.\n\
                 Spam filtering will not be available this session.",
            );
            // Return S_OK so Outlook doesn't unload us — we remain loaded but
            // non-functional for filtering. Mark as initialized so toolbar still gets created.
            self.initialized = true;
            return S_OK;
        }

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnConnection: MAPI init succeeded")
            });

        // Requirement 1.7: Set LC_NUMERIC locale to "C" after MAPI init.
        self.set_lc_numeric_c();
        self.log_info("OnConnection: LC_NUMERIC set to \"C\"");

        // Load classifier database.
        // Requirement 1.6: If DB load fails, report error and init empty database.
        self.load_classifier_database();

        // Setup the filter engine.
        self.setup_filter_engine();

        self.initialized = true;

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnConnection: COMPLETE, initialized=true")
            });

        self.log_info("OnConnection: SpamBayes add-in initialization complete");
        S_OK
    }

    /// `OnDisconnection`: save dirty data, release hooks, cancel timers,
    /// release MAPI.
    ///
    /// **Validates: Requirement 1.4**
    fn handle_on_disconnection(&mut self) -> HRESULT {
        self.log_info("OnDisconnection: SpamBayes add-in shutting down");

        // Save dirty classifier data.
        self.save_dirty_data();

        // Release filter engine (cancels timer state).
        self.filter_engine = None;
        self.timer_state = None;

        // Disconnect folder hooks.
        for hook in &mut self.folder_hooks {
            hook.disconnect();
        }
        self.folder_hooks.clear();
        self.folder_hook_state = None;

        // Clear global pointer.
        unsafe { GLOBAL_ADDIN_PTR = std::ptr::null_mut(); }

        // Release training engine.
        self.training_engine = None;

        // Release notification manager.
        self.notification_mgr = None;

        // Release classifier and storage.
        self.classifier = None;
        self.storage = None;

        // Release MAPI session.
        #[cfg(target_os = "windows")]
        {
            if let Some(mut session) = self.mapi_session.take() {
                session.logoff();
            }
        }
        self.mapi_initialized = false;

        // Release Application reference.
        self.application = None;

        self.initialized = false;
        self.log_info("OnDisconnection: Shutdown complete");

        // Release logger last.
        self.logger = None;

        S_OK
    }

    /// `OnStartupComplete`: setup toolbar, setup folder hooks, launch wizard
    /// if needed.
    ///
    /// **Validates: Requirements 1.1**
    fn handle_on_startup_complete(&mut self) -> HRESULT {
        let debug_path = Self::get_data_directory().join("addin_debug.log");
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnStartupComplete: ENTRY, initialized={}", self.initialized)
            });

        if !self.initialized {
            return S_OK;
        }

        self.log_info("OnStartupComplete: Setting up toolbar and folder hooks");

        // Check if wizard should be launched (no config file exists).
        if let Some(config) = &self.config {
            if !config.filter.enabled {
                self.log_info(
                    "OnStartupComplete: Filter not enabled, wizard may be needed",
                );
            }
        }

        // Defer toolbar setup via timer. Direct COM calls during
        // OnStartupComplete often fail with RPC_E_CANTCALLOUT_ININPUTSYNCCALL.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::SetTimer;
            use windows::Win32::Foundation::HWND;
            SetTimer(HWND::default(), TOOLBAR_TIMER_ID, 1500, Some(toolbar_timer_proc));
        }

        // Defer folder hook setup via timer (fires after toolbar is set up).
        // Store global pointer so the timer callback can access this instance.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::SetTimer;
            use windows::Win32::Foundation::HWND;
            // Store self pointer for the timer callback.
            // SAFETY: AddinCore lives for the duration of the Outlook session.
            // The pointer is only accessed from the STA thread.
            GLOBAL_ADDIN_PTR = self as *mut AddinCore;
            SetTimer(HWND::default(), FOLDER_HOOK_TIMER_ID, 2500, Some(folder_hook_timer_proc));
        }

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnStartupComplete: COMPLETE")
            });

        self.log_info("OnStartupComplete: Startup complete");
        S_OK
    }

    /// `OnBeginShutdown`: cleanup resources.
    ///
    /// **Validates: Requirements 1.1, 1.4**
    fn handle_on_begin_shutdown(&mut self) -> HRESULT {
        self.log_info("OnBeginShutdown: Preparing for shutdown");

        // Save any remaining dirty data before Outlook finalizes shutdown.
        self.save_dirty_data();

        self.log_info("OnBeginShutdown: Cleanup complete");
        S_OK
    }

    /// Return the Ribbon XML for the SpamBayes ribbon group.
    ///
    /// Called by Outlook via IRibbonExtensibility::GetCustomUI.
    pub fn get_ribbon_xml() -> String {
        r#"<customUI xmlns="http://schemas.microsoft.com/office/2009/07/customui"
  onLoad="Ribbon_OnLoad">
  <ribbon>
    <tabs>
      <tab idMso="TabMail">
        <group id="grpSpamBayes"
               label="SpamBayes"
               insertAfterMso="GroupMailDelete">
          <button id="btnSpam"
                  label="Spam"
                  size="normal"
                  imageMso="RecordsDeleteRecord"
                  onAction="OnSpamClick"
                  getEnabled="GetSpamEnabled" />
          <button id="btnNotSpam"
                  label="Not Spam"
                  size="normal"
                  imageMso="AcceptInvitation"
                  onAction="OnNotSpamClick"
                  getEnabled="GetNotSpamEnabled"
                  getVisible="GetNotSpamVisible" />
          <button id="btnManager"
                  label="Manager"
                  size="normal"
                  imageMso="DatabaseProperties"
                  onAction="OnManagerClick" />
        </group>
      </tab>
    </tabs>
  </ribbon>
</customUI>"#.to_string()
    }

    /// Launch the SpamBayes Manager GUI.
    pub fn launch_manager() {
        let exe_path = r"C:\Program Files\SpamBayes\spambayes_manager.exe";
        let alt_path = r"D:\My Apps\SpamBayes_Rust\Outlook365\target\x86_64-pc-windows-msvc\release\spambayes_manager.exe";

        let path = if std::path::Path::new(exe_path).exists() {
            exe_path
        } else if std::path::Path::new(alt_path).exists() {
            alt_path
        } else {
            return;
        };

        let _ = std::process::Command::new(path).spawn();
    }

    // ─── Helper Methods ──────────────────────────────────────────────────

    /// Load application configuration from INI files.
    ///
    /// Falls back to defaults if the config file cannot be loaded.
    fn load_config(&self) -> AppConfig {
        // Determine data directory (use %LOCALAPPDATA%\SpamBayes or fallback).
        let data_dir = Self::get_data_directory();
        let profile_name = "default";

        // If the Rust-specific config already exists, load it directly.
        if spambayes_config::rust_config_exists(&data_dir, profile_name) {
            return match AppConfig::load(&data_dir, profile_name) {
                Ok(config) => {
                    self.log_info("Config loaded successfully");
                    config
                }
                Err(e) => {
                    self.log_error(&format!("Failed to load config: {e}, using defaults"));
                    AppConfig::default()
                }
            };
        }

        // No Rust config exists — attempt to detect and migrate a Python SpamBayes config.
        // Requirement 20.1: Parse and apply all sections/keys from existing Python INI.
        // Requirement 20.4: Preserve folder ID references (hex tuples).
        if let Some(config) = spambayes_config::try_migrate(&data_dir, profile_name) {
            self.log_info(
                "Migrated configuration from existing Python SpamBayes INI file",
            );
            return config;
        }

        // No config found at all — try normal load (returns defaults).
        match AppConfig::load(&data_dir, profile_name) {
            Ok(config) => {
                self.log_info("Config loaded successfully (defaults)");
                config
            }
            Err(e) => {
                self.log_error(&format!("Failed to load config: {e}, using defaults"));
                AppConfig::default()
            }
        }
    }

    /// Initialize the MAPI session.
    ///
    /// Returns `true` if MAPI initialized successfully, `false` otherwise.
    ///
    /// **Validates: Requirement 1.3**
    fn initialize_mapi(&mut self) -> bool {
        #[cfg(target_os = "windows")]
        {
            match spambayes_mapi::MapiSessionImpl::initialize_and_logon() {
                Ok(session) => {
                    self.log_info("MAPI session initialized successfully");
                    self.mapi_session = Some(session);
                    self.mapi_initialized = true;
                    true
                }
                Err(e) => {
                    self.log_error(&format!("MAPI initialization failed: {e}"));
                    self.mapi_initialized = false;
                    false
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Non-Windows: MAPI is not available. Log and report failure.
            self.log_error("MAPI initialization not available on this platform");
            self.mapi_initialized = false;
            false
        }
    }

    /// Set the `LC_NUMERIC` locale to "C" to prevent floating-point parsing errors.
    ///
    /// **Validates: Requirement 1.7**
    fn set_lc_numeric_c(&self) {
        // LC_NUMERIC = 1 in the C runtime
        const LC_NUMERIC: i32 = 1;

        unsafe {
            libc::setlocale(LC_NUMERIC, c"C".as_ptr());
        }
    }

    /// Load the classifier database from disk.
    ///
    /// On failure, reports an error and initializes an empty database so that
    /// filtering can proceed. If no Rust-format database exists, attempts to
    /// migrate from a Python `SpamBayes` database.
    ///
    /// **Validates: Requirements 1.3, 1.6, 20.2, 20.3, 20.6, 20.7**
    fn load_classifier_database(&mut self) {
        let data_dir = Self::get_data_directory();
        let db_path = data_dir.join("spambayes.db");

        self.log_info(&format!("Loading classifier database from: {}", db_path.display()));

        // If the Rust-format database file exists, load it directly.
        if db_path.exists() {
            let mut backend = MmapDbmBackend::new(&db_path);
            match backend.load() {
                Ok(state) => {
                    self.log_info(&format!(
                        "Database loaded: nspam={}, nham={}",
                        state.nspam, state.nham
                    ));

                    let config = spambayes_core::ClassifierConfig::default();
                    let word_data = backend.data().clone();
                    let classifier = Classifier::from_state(
                        config,
                        state.nspam,
                        state.nham,
                        word_data,
                    );

                    let classifier = Arc::new(Mutex::new(classifier));
                    let storage: Arc<Mutex<Box<dyn StorageBackend>>> =
                        Arc::new(Mutex::new(Box::new(backend)));

                    self.classifier = Some(classifier);
                    self.storage = Some(storage);
                    return;
                }
                Err(e) => {
                    // Requirement 1.6: Report error, fall through to migration/empty.
                    self.log_error(&format!("Failed to load classifier database: {e}"));
                    self.error_reporter.report_once(
                        HRESULT(E_FAIL.0),
                        &format!(
                            "SpamBayes failed to load the classifier database.\n\
                             Attempting migration from Python database.\n\
                             Error: {e}"
                        ),
                    );
                }
            }
        }

        // No Rust database (or failed to load) — attempt migration from Python.
        // Requirement 20.3, 20.6: Import existing Python spambayes databases.
        if let Some((state, tokens)) = spambayes_storage::try_migrate_classifier(&data_dir) {
            self.log_info(&format!(
                "Migrated Python classifier database: nspam={}, nham={}, tokens={}",
                state.nspam, state.nham, tokens.len()
            ));

            let config = spambayes_core::ClassifierConfig::default();
            let classifier = Classifier::from_state(
                config,
                state.nspam,
                state.nham,
                tokens,
            );

            let classifier = Arc::new(Mutex::new(classifier));
            let backend = MmapDbmBackend::new(&db_path);
            let storage: Arc<Mutex<Box<dyn StorageBackend>>> =
                Arc::new(Mutex::new(Box::new(backend)));

            self.classifier = Some(classifier);
            self.storage = Some(storage);
            return;
        }

        // Requirement 20.7: No database found, initialize empty.
        self.log_info("No existing database found. Initializing empty classifier.");

        let config = spambayes_core::ClassifierConfig::default();
        let classifier = Arc::new(Mutex::new(Classifier::new(config)));
        let storage: Arc<Mutex<Box<dyn StorageBackend>>> =
            Arc::new(Mutex::new(Box::new(MmapDbmBackend::new(&db_path))));

        self.classifier = Some(classifier);
        self.storage = Some(storage);
    }

    /// Setup the toolbar in the Outlook Explorer window.
    ///
    /// Uses the Outlook Object Model (via IDispatch) to create a "SpamBayes"
    /// command bar with Spam/Not Spam buttons and a dropdown menu.
    fn setup_toolbar(&mut self) {
        let app_ptr = match self.application {
            Some(ptr) if !ptr.is_null() => ptr,
            _ => {
                self.log_error("setup_toolbar: No Application pointer available");
                return;
            }
        };

        let debug_path = Self::get_data_directory().join("addin_debug.log");
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "setup_toolbar: app_ptr={:?}", app_ptr)
            });

        let images_dir = Self::get_data_directory().join("images");
        let mut toolbar_mgr = crate::toolbar::ToolbarManager::new(app_ptr, images_dir);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            unsafe { toolbar_mgr.execute_setup() }
        }));

        match result {
            Ok(Ok(())) => {
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&debug_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "setup_toolbar: SUCCESS")
                    });
            }
            Ok(Err(e)) => {
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&debug_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "setup_toolbar: FAILED: {}", e)
                    });
            }
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                let _ = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&debug_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "setup_toolbar: PANICKED: {}", msg)
                    });
            }
        }
    }

    /// Setup the filter engine with classifier, storage, and config.
    fn setup_filter_engine(&mut self) {
        let config = match &self.config {
            Some(c) => c.clone(),
            None => return,
        };

        let classifier = match &self.classifier {
            Some(c) => Arc::clone(c),
            None => return,
        };

        let storage = match &self.storage {
            Some(s) => Arc::clone(s),
            None => return,
        };

        // Create a message database (in-memory placeholder for now).
        // The real message DB will be loaded from disk in production.
        let message_db: Arc<Mutex<Box<dyn spambayes_storage::MessageDatabase>>> =
            Arc::new(Mutex::new(Box::new(
                spambayes_storage::MmapMessageDb::new(
                    Self::get_data_directory().join("spambayes_msg.db"),
                ),
            )));

        // Create filter engine.
        let filter = FilterEngine::new(
            config.filter.clone(),
            Arc::clone(&classifier),
            Arc::clone(&storage),
            Arc::clone(&message_db),
        );
        self.filter_engine = Some(filter);

        // Create timer state.
        let timer_state = TimerFilterState::new(&config.filter);
        self.timer_state = Some(timer_state);

        // Create notification manager.
        let notification_mgr = NotificationManager::new(&config.notification);
        self.notification_mgr = Some(notification_mgr);

        // Create training engine (if logger is available).
        if let Some(logger) = &self.logger {
            let training_engine = TrainingEngine::new(
                Arc::clone(&classifier),
                Arc::clone(&storage),
                Arc::clone(&message_db),
                Arc::clone(logger),
            );
            self.training_engine = Some(training_engine);
        }

        self.log_info("Filter engine, timer, notification, and training engine initialized");
    }

    /// Setup folder hooks to monitor configured watch folders for new messages.
    ///
    /// Resolves each folder ID in `config.filter.watch_folder_ids` and connects
    /// a COM event sink to receive `ItemAdd` notifications. When a new message
    /// arrives, the sink scores it and performs the configured filter action.
    fn setup_folder_hooks(&mut self) {
        let config = match &self.config {
            Some(c) => c.clone(),
            None => return,
        };

        if !config.filter.enabled {
            self.log_info("setup_folder_hooks: filter disabled, skipping");
            return;
        }

        if config.filter.watch_folder_ids.is_empty() {
            self.log_info("setup_folder_hooks: no watch folders configured");
            return;
        }

        let app_ptr = match self.application {
            Some(ptr) if !ptr.is_null() => ptr,
            _ => {
                self.log_error("setup_folder_hooks: no Application pointer");
                return;
            }
        };

        // Build a FilterEngine that shares the same classifier and storage.
        let classifier = match &self.classifier {
            Some(c) => Arc::clone(c),
            None => {
                self.log_error("setup_folder_hooks: no classifier available");
                return;
            }
        };
        let storage = match &self.storage {
            Some(s) => Arc::clone(s),
            None => {
                self.log_error("setup_folder_hooks: no storage available");
                return;
            }
        };

        // Share the message DB path with the filter engine
        let message_db: Arc<Mutex<Box<dyn spambayes_storage::MessageDatabase>>> =
            Arc::new(Mutex::new(Box::new(
                spambayes_storage::MmapMessageDb::new(
                    Self::get_data_directory().join("spambayes_msg.db"),
                ),
            )));

        let hook_filter = FilterEngine::new(
            config.filter.clone(),
            classifier,
            storage,
            message_db,
        );
        let filter_engine = Arc::new(Mutex::new(hook_filter));

        let notification_mgr = Arc::new(Mutex::new(
            NotificationManager::new(&config.notification)
        ));

        let state = Arc::new(Mutex::new(crate::folder_sink::FolderHookState {
            filter_engine,
            config: config.clone(),
            notification_mgr,
            logger: self.logger.clone(),
        }));

        self.folder_hook_state = Some(Arc::clone(&state));

        // Setup the hooks
        let hooks = unsafe {
            crate::folder_sink::setup_folder_hooks(
                app_ptr,
                state,
                &config.filter.watch_folder_ids,
            )
        };

        let hook_count = hooks.len();
        self.folder_hooks = hooks;

        self.log_info(&format!(
            "setup_folder_hooks: {} folder hook(s) active", hook_count
        ));

        // Scan existing items in the watched folders to catch messages
        // that arrived before the add-in started.
        if hook_count > 0 {
            if let Some(state) = &self.folder_hook_state {
                unsafe {
                    crate::folder_sink::scan_existing_items(
                        &self.folder_hooks,
                        state,
                    );
                }
            }
        }
    }

    /// Save any dirty classifier data to disk.
    ///
    /// Called during `OnDisconnection` and `OnBeginShutdown` to persist
    /// modified token data.
    ///
    /// **Validates: Requirement 1.4**
    fn save_dirty_data(&self) {
        if let Some(storage) = &self.storage {
            if let Some(classifier) = &self.classifier {
                // Lock classifier to read current state.
                let classifier_guard = if let Ok(guard) = classifier.lock() { guard } else {
                    self.log_error("Failed to lock classifier for save (poisoned)");
                    return;
                };

                let state = spambayes_storage::ClassifierState {
                    nspam: classifier_guard.nspam(),
                    nham: classifier_guard.nham(),
                    version: 1,
                };

                // Collect all word info as changes for a full persist.
                let changed: std::collections::HashMap<Vec<u8>, spambayes_storage::WordChange> =
                    classifier_guard
                        .word_info()
                        .iter()
                        .map(|(token, info)| {
                            (token.clone(), spambayes_storage::WordChange::Updated(*info))
                        })
                        .collect();

                drop(classifier_guard);

                // Lock storage and save.
                match storage.lock() {
                    Ok(mut storage_guard) => {
                        if let Err(e) = storage_guard.store(&state, &changed) {
                            self.log_error(&format!("Failed to save classifier data: {e}"));
                        } else {
                            self.log_info("Classifier data saved successfully");
                        }
                    }
                    Err(_) => {
                        self.log_error("Failed to lock storage for save (poisoned)");
                    }
                }
            }
        }
    }

    /// Get the `SpamBayes` data directory path.
    ///
    /// Uses %LOCALAPPDATA%\SpamBayes, falling back to %TEMP% if unavailable.
    fn get_data_directory() -> PathBuf {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let path = PathBuf::from(local_app_data).join("SpamBayes");
            // Ensure directory exists.
            let _ = std::fs::create_dir_all(&path);
            return path;
        }

        // Fallback to TEMP directory.
        let temp = std::env::var("TEMP")
            .or_else(|_| std::env::var("TMP"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(temp)
    }

    /// Log an info message if the logger is available.
    fn log_info(&self, message: &str) {
        if let Some(logger) = &self.logger {
            logger.info("addin_core", message);
        }
    }

    /// Log an error message if the logger is available.
    fn log_error(&self, message: &str) {
        if let Some(logger) = &self.logger {
            logger.error("addin_core", message);
        }
    }
}
