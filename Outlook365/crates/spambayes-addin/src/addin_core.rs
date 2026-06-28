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
use spambayes_config::ConfigChain;
use spambayes_core::classifier::Classifier;
use spambayes_storage::{MmapDbmBackend, StorageBackend};

use crate::error_reporter::ErrorReporter;
use crate::filter::FilterEngine;
use crate::logger::Logger;
use crate::manager_dlg::ManagerState;
use crate::notification::NotificationManager;
use crate::statistics::StatisticsManager;
use crate::timer::TimerFilterState;
use crate::train::TrainingEngine;
use crate::wizard::ConfigWizard;
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

/// Timer ID for folder-switch ribbon invalidation polling.
const FOLDER_SWITCH_TIMER_ID: usize = 0x5B04;

/// Timer ID for manager process exit detection and config reload.
const MANAGER_WATCH_TIMER_ID: usize = 0x5B05;

/// Last known folder EntryID for change detection.
/// SAFETY: Only accessed from the COM STA thread.
static mut LAST_FOLDER_ENTRY_ID: Option<String> = None;

/// Global pointer to the AddinCore instance for timer callbacks.
/// SAFETY: Only accessed from the COM STA thread.
static mut GLOBAL_ADDIN_PTR: *mut AddinCore = std::ptr::null_mut();

/// Global pointer to the IRibbonUI interface for ribbon invalidation.
/// Stored during `Ribbon_OnLoad` callback, used to refresh button visibility
/// when the user switches folders.
/// SAFETY: Only accessed from the COM STA thread.
pub static mut GLOBAL_RIBBON_UI: *mut c_void = std::ptr::null_mut();

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
        .and_then(|mut f| { use std::io::Write; writeln!(f, "folder_hook_timer_proc: FIRED (build: {})", env!("SPAMBAYES_BUILD_ID")) });

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

/// Timer callback that polls the current folder and invalidates the ribbon
/// when the user navigates to a different folder. This ensures the Spam/Not Spam
/// button visibility is updated dynamically (matching the Python version's OnFolderSwitch).
///
/// Fires every 500ms — lightweight (one dispatch_get_string per tick).
unsafe extern "system" fn folder_switch_timer_proc(
    _hwnd: windows::Win32::Foundation::HWND,
    _msg: u32,
    _id_event: usize,
    _dw_time: u32,
) {
    use crate::com_invoke::{dispatch_get, dispatch_get_string};

    let addin = GLOBAL_ADDIN_PTR;
    if addin.is_null() { return; }
    let addin = &*addin;

    // Use POLL_APP_PTR (set by toolbar_timer_proc after CoCreateInstance) 
    // which is more reliable than the stored application pointer.
    let app_ptr = if !POLL_APP_PTR.is_null() {
        POLL_APP_PTR
    } else {
        match addin.application {
            Some(p) if !p.is_null() => p,
            _ => return,
        }
    };

    // Get Application.ActiveExplorer.CurrentFolder.EntryID
    let explorer = match dispatch_get(app_ptr, "ActiveExplorer") {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };
    let folder = match dispatch_get(explorer, "CurrentFolder") {
        Ok(p) if !p.is_null() => p,
        _ => {
            AddinCore::release_dispatch(explorer);
            return;
        }
    };
    let entry_id = dispatch_get_string(folder, "EntryID");
    AddinCore::release_dispatch(folder);
    AddinCore::release_dispatch(explorer);

    // Compare with last known folder
    let last = &raw const LAST_FOLDER_ENTRY_ID;
    let changed = match (&*last, &entry_id) {
        (None, Some(_)) => true,
        (Some(old), Some(new)) => old != new,
        (Some(_), None) => true,
        (None, None) => false,
    };

    if changed {
        LAST_FOLDER_ENTRY_ID = entry_id;
        // Invalidate ribbon so getVisible callbacks fire again
        AddinCore::invalidate_ribbon();
    }
}

/// Timer callback that checks if the manager subprocess has exited.
/// When it exits, we reload the config from disk so that any changes made in
/// the GUI take effect immediately without restarting Outlook.
///
/// Fires every 1s while the manager is running. Self-kills when no manager is active.
unsafe extern "system" fn manager_watch_timer_proc(
    _hwnd: windows::Win32::Foundation::HWND,
    _msg: u32,
    _id_event: usize,
    _dw_time: u32,
) {
    let addin = GLOBAL_ADDIN_PTR;
    if addin.is_null() {
        return;
    }
    let addin = &mut *addin;

    let manager_exited = match &mut addin.gtk_runtime {
        Some(child) => {
            match child.try_wait() {
                Ok(Some(_status)) => true,  // Process exited
                Ok(None) => false,          // Still running
                Err(_) => true,             // Error — treat as exited
            }
        }
        None => {
            // No manager process — kill this timer
            use windows::Win32::UI::WindowsAndMessaging::KillTimer;
            use windows::Win32::Foundation::HWND;
            KillTimer(HWND::default(), MANAGER_WATCH_TIMER_ID).ok();
            return;
        }
    };

    if !manager_exited {
        return; // Still running, check again on next tick
    }

    // Manager has exited — clear the handle and reload config
    addin.gtk_runtime = None;

    // Kill this timer since the manager is gone
    use windows::Win32::UI::WindowsAndMessaging::KillTimer;
    use windows::Win32::Foundation::HWND;
    KillTimer(HWND::default(), MANAGER_WATCH_TIMER_ID).ok();

    // Reload configuration from disk
    addin.reload_config_from_disk();
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
    /// Layered configuration chain (for save operations and profile resolution).
    config_chain: Option<ConfigChain>,
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
    /// Statistics tracking and persistence manager.
    statistics: Option<StatisticsManager>,
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
    /// GTK4 runtime for native GUI dialogs (None if GTK4 DLLs unavailable).
    gtk_runtime: Option<std::process::Child>,
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
            config_chain: None,
            classifier: None,
            storage: None,
            filter_engine: None,
            timer_state: None,
            training_engine: None,
            statistics: None,
            notification_mgr: None,
            logger: None,
            error_reporter: ErrorReporter::new(),
            initialized: false,
            folder_hooks: Vec::new(),
            folder_hook_state: None,
            gtk_runtime: None,
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
            "GetSpamVisible" => 108,
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
                        // The IRibbonUI is passed as the first argument in DISPPARAMS
                        if !p_disp_params.is_null() {
                            #[repr(C)]
                            struct DISPPARAMS {
                                rgvarg: *mut c_void,
                                rgdispid_named: *mut i32,
                                c_args: u32,
                                c_named_args: u32,
                            }
                            let dp = &*(p_disp_params as *const DISPPARAMS);
                            if dp.c_args > 0 && !dp.rgvarg.is_null() {
                                // The VARIANT at offset 0: VT_DISPATCH (9) with the IRibbonUI ptr at offset 8
                                let var_ptr = dp.rgvarg as *const u8;
                                let vt = *(var_ptr as *const u16);
                                if vt == 9 || vt == 13 { // VT_DISPATCH or VT_UNKNOWN
                                    let ribbon_ptr = *(var_ptr.add(8) as *const *mut c_void);
                                    if !ribbon_ptr.is_null() {
                                        // AddRef on the ribbon UI
                                        let vtbl = *(ribbon_ptr as *const *const usize);
                                        let addref_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                                            std::mem::transmute(*vtbl.add(1));
                                        addref_fn(ribbon_ptr);
                                        GLOBAL_RIBBON_UI = ribbon_ptr;
                                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                            .and_then(|mut f| { use std::io::Write; writeln!(f, "Ribbon_OnLoad: stored IRibbonUI={:?}", ribbon_ptr) });
                                    }
                                }
                            }
                        }
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
                        // GetNotSpamVisible: visible when in spam folder or unsure folder
                        let visible = Self::is_in_spam_or_unsure_folder();
                        if !_p_var_result.is_null() {
                            let result = &mut *(_p_var_result as *mut [u8; 24]);
                            result[0] = 11; result[1] = 0; // VT_BOOL
                            let bool_slot = &mut *(result.as_mut_ptr().add(8) as *mut i16);
                            *bool_slot = if visible { -1 } else { 0 };
                        }
                        S_OK
                    }
                    108 => {
                        // GetSpamVisible: visible when NOT in spam folder (but ok in unsure)
                        let in_spam = Self::is_in_spam_folder_only();
                        if !_p_var_result.is_null() {
                            let result = &mut *(_p_var_result as *mut [u8; 24]);
                            result[0] = 11; result[1] = 0; // VT_BOOL
                            let bool_slot = &mut *(result.as_mut_ptr().add(8) as *mut i16);
                            *bool_slot = if !in_spam { -1 } else { 0 };
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

        // Initialize MAPI session FIRST so we have the profile name for config loading.
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

            // Load config with "default" profile since MAPI is unavailable.
            self.load_config_chain();

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

        // Load configuration using ConfigChain (now that MAPI is available for profile name).
        self.load_config_chain();

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnConnection: Config loaded via ConfigChain")
            });

        // Requirement 1.7: Set LC_NUMERIC locale to "C" after MAPI init.
        self.set_lc_numeric_c();
        self.log_info("OnConnection: LC_NUMERIC set to \"C\"");

        // Load classifier database.
        // Requirement 1.6: If DB load fails, report error and init empty database.
        self.load_classifier_database();

        // Initialize statistics manager.
        let data_dir = Self::get_data_directory();
        let statistics = StatisticsManager::new(&data_dir, 10);
        self.statistics = Some(statistics);
        self.log_info("StatisticsManager initialized");

        // Setup the filter engine.
        self.setup_filter_engine();

        self.initialized = true;

        // GUI will be launched on-demand via spambayes_manager.exe subprocess
        // when the user clicks "SpamBayes Manager" button.
        self.gtk_runtime = None;

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

        // Persist statistics to disk before releasing components.
        if let Some(stats) = &self.statistics {
            stats.save();
        }

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

        // Release statistics manager.
        self.statistics = None;

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

        // Shut down manager process if still running.
        if let Some(mut child) = self.gtk_runtime.take() {
            let _ = child.kill();
        }

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

        // First-run detection: if no config exists for this profile, show wizard.
        {
            let data_dir = Self::get_data_directory();
            // Use "default" as the profile name — this matches how configs are
            // actually saved (default.ini). The MAPI profile name from the
            // status table is unreliable for this purpose.
            let profile_name = "default".to_string();

            if ConfigWizard::needs_wizard(&data_dir, &profile_name) {
                self.log_info("OnStartupComplete: First-run detected, launching manager/wizard");
                // Launch the manager exe which handles first-run wizard automatically.
                let dll_dir = Self::get_dll_directory();
                let manager_path = dll_dir.join("spambayes_manager.exe");
                if manager_path.is_file() {
                    match std::process::Command::new(&manager_path)
                        .current_dir(&dll_dir)
                        .spawn()
                    {
                        Ok(child) => {
                            log::info!("Launched SpamBayes Manager for first-run (PID {})", child.id());
                            self.gtk_runtime = Some(child);
                            // Start timer to detect manager exit and reload config
                            unsafe {
                                use windows::Win32::UI::WindowsAndMessaging::SetTimer;
                                use windows::Win32::Foundation::HWND;
                                SetTimer(HWND::default(), MANAGER_WATCH_TIMER_ID, 1000, Some(manager_watch_timer_proc));
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to launch manager for first-run: {e}");
                        }
                    }
                }
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
            // Start folder-switch polling timer for dynamic ribbon button visibility.
            // Fires every 500ms to detect folder navigation and invalidate the ribbon.
            SetTimer(HWND::default(), FOLDER_SWITCH_TIMER_ID, 500, Some(folder_switch_timer_proc));
        }

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "OnStartupComplete: COMPLETE")
            });

        self.log_info("OnStartupComplete: Startup complete");

        // Invalidate the ribbon now that config is loaded so button visibility
        // reflects the current folder state correctly.
        unsafe { Self::invalidate_ribbon(); }

        S_OK
    }

    /// `OnBeginShutdown`: cleanup resources.
    ///
    /// **Validates: Requirements 1.1, 1.4**
    fn handle_on_begin_shutdown(&mut self) -> HRESULT {
        self.log_info("OnBeginShutdown: Preparing for shutdown");

        // Kill the folder-switch polling timer.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::KillTimer;
            use windows::Win32::Foundation::HWND;
            KillTimer(HWND::default(), FOLDER_SWITCH_TIMER_ID).ok();
            // Release the IRibbonUI reference
            if !GLOBAL_RIBBON_UI.is_null() {
                Self::release_dispatch(GLOBAL_RIBBON_UI);
                GLOBAL_RIBBON_UI = std::ptr::null_mut();
            }
        }

        // Save any remaining dirty data before Outlook finalizes shutdown.
        self.save_dirty_data();

        // Persist statistics to disk before shutdown completes.
        if let Some(stats) = &self.statistics {
            stats.save();
        }

        self.log_info("OnBeginShutdown: Cleanup complete");
        S_OK
    }

    /// Return the Ribbon XML for the SpamBayes ribbon group.
    ///
    /// Called by Outlook via IRibbonExtensibility::GetCustomUI.
    pub fn get_ribbon_xml() -> String {
        r#"<customUI xmlns="http://schemas.microsoft.com/office/2009/07/customui"
  onLoad="Ribbon_OnLoad"
  loadImage="LoadImage">
  <ribbon>
    <tabs>
      <tab idMso="TabMail">
        <group id="grpSpamBayes"
               label="SpamBayes"
               insertAfterMso="GroupMailDelete">
          <button id="btnSpam"
                  label="Spam"
                  size="normal"
                  image="delete_as_spam"
                  onAction="OnSpamClick"
                  getEnabled="GetSpamEnabled"
                  getVisible="GetSpamVisible" />
          <button id="btnNotSpam"
                  label="Not Spam"
                  size="normal"
                  image="recover_ham"
                  onAction="OnNotSpamClick"
                  getEnabled="GetNotSpamEnabled"
                  getVisible="GetNotSpamVisible" />
          <button id="btnShowClues"
                  label="Show Clues"
                  size="normal"
                  imageMso="TraceDependents"
                  onAction="OnShowCluesClick"
                  getEnabled="GetShowCluesEnabled" />
          <button id="btnManager"
                  label="SpamBayes Manager"
                  size="normal"
                  imageMso="DatabaseProperties"
                  onAction="OnManagerClick" />
        </group>
      </tab>
    </tabs>
  </ribbon>
  <contextMenus>
    <contextMenu idMso="ContextMenuMailItem">
      <menuSeparator id="sepSpamBayes" />
      <button id="ctxBtnSpam"
              label="Spam"
              image="delete_as_spam"
              onAction="OnSpamClick"
              getVisible="GetSpamVisible" />
      <button id="ctxBtnNotSpam"
              label="Not Spam"
              image="recover_ham"
              onAction="OnNotSpamClick"
              getVisible="GetNotSpamVisible" />
    </contextMenu>
  </contextMenus>
</customUI>"#.to_string()
    }

    /// Launch the SpamBayes Manager GUI via GTK4.
    ///
    /// Uses the global `GLOBAL_ADDIN_PTR` to access the GTK runtime and config.
    /// If GTK4 is unavailable, shows a Win32 MessageBox error.
    ///
    /// **Validates: Requirements 14.1, 14.2, 14.3**
    pub fn launch_manager() {
        unsafe {
            if GLOBAL_ADDIN_PTR.is_null() {
                return;
            }
            let addin = &mut *GLOBAL_ADDIN_PTR;

            // Get config for building ManagerState.
            let config = match &addin.config {
                Some(c) => c.clone(),
                None => {
                    Self::show_win32_error(
                        "Configuration not loaded. Cannot open the manager.",
                    );
                    return;
                }
            };

            let state = ManagerState::from_config(&config);
            let data_dir = Self::get_data_directory();
            let profile_name = addin.get_mapi_profile_name()
                .unwrap_or_else(|| "default".to_string());

            match &mut addin.gtk_runtime {
                Some(child) => {
                    // Check if the process is still running
                    match child.try_wait() {
                        Ok(Some(_status)) => {
                            // Process has exited — clear it and launch a new one
                            addin.gtk_runtime = None;
                        }
                        Ok(None) => {
                            // Still running — don't launch another
                            log::info!("Manager already running, ignoring duplicate request.");
                            return;
                        }
                        Err(_) => {
                            // Error checking status — clear and re-launch
                            addin.gtk_runtime = None;
                        }
                    }
                }
                None => {}
            }

            // Launch spambayes_manager.exe alongside the DLL.
            {
                let dll_dir = Self::get_dll_directory();
                let manager_path = dll_dir.join("spambayes_manager.exe");

                if manager_path.is_file() {
                    match std::process::Command::new(&manager_path)
                        .current_dir(&dll_dir)
                        .spawn()
                    {
                        Ok(child) => {
                            log::info!("Launched SpamBayes Manager (PID {})", child.id());
                            addin.gtk_runtime = Some(child);
                            // Start timer to detect manager exit and reload config
                            use windows::Win32::UI::WindowsAndMessaging::SetTimer;
                            use windows::Win32::Foundation::HWND;
                            SetTimer(HWND::default(), MANAGER_WATCH_TIMER_ID, 1000, Some(manager_watch_timer_proc));
                        }
                        Err(e) => {
                            log::error!("Failed to launch manager: {e}");
                            Self::show_win32_error(&format!(
                                "Failed to launch SpamBayes Manager:\n{e}\n\n\
                                 Expected at: {}",
                                manager_path.display()
                            ));
                        }
                    }
                } else {
                    log::error!("Manager EXE not found at: {}", manager_path.display());
                    Self::show_win32_error(&format!(
                        "SpamBayes Manager not found.\n\n\
                         Expected at: {}\n\n\
                         Please reinstall SpamBayes.",
                        manager_path.display()
                    ));
                }
            }
        }
    }

    // ─── Show Clues ────────────────────────────────────────────────────

    // ─── Train as Spam / Ham ─────────────────────────────────────────────

    /// Train the selected message(s) as spam and move to the spam folder.
    pub fn train_selected_as_spam() {
        unsafe { Self::train_selected(true); }
    }

    /// Train the selected message(s) as ham (not spam) and move back to inbox.
    pub fn train_selected_as_ham() {
        unsafe { Self::train_selected(false); }
    }

    /// Core implementation for training selected messages.
    unsafe fn train_selected(is_spam: bool) {
        use crate::com_invoke::{dispatch_get, dispatch_get_string};

        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let label = if is_spam { "spam" } else { "ham" };

        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: entering", label) });

        if GLOBAL_ADDIN_PTR.is_null() {
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: GLOBAL_ADDIN_PTR is null", label) });
            return;
        }
        let addin = &*GLOBAL_ADDIN_PTR;

        let app_ptr = if !POLL_APP_PTR.is_null() {
            POLL_APP_PTR
        } else {
            match addin.application {
                Some(p) if !p.is_null() => p,
                _ => {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: no application ptr", label) });
                    return;
                }
            }
        };

        // Get Explorer and Selection
        let explorer = match dispatch_get(app_ptr, "ActiveExplorer") {
            Ok(p) if !p.is_null() => p,
            _ => {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: no ActiveExplorer", label) });
                return;
            }
        };
        let selection = match dispatch_get(explorer, "Selection") {
            Ok(p) if !p.is_null() => p,
            _ => {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: no Selection", label) });
                Self::release_dispatch(explorer); return;
            }
        };

        // Get Selection.Count via direct VARIANT inspection (it's an integer, not a string)
        let count = Self::get_selection_count(selection);

        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: selection count={}", label, count) });

        if count == 0 {
            Self::release_dispatch(selection);
            Self::release_dispatch(explorer);
            return;
        }

        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: {} message(s)", label, count) });

        // Get the destination folder ID from config
        let dest_folder_id = {
            let config = match &addin.config {
                Some(c) => c,
                None => {
                    Self::release_dispatch(selection);
                    Self::release_dispatch(explorer);
                    return;
                }
            };
            if is_spam {
                config.filter.spam_folder_id.clone()
            } else {
                // For ham (Not Spam), move back to the first watch folder (inbox)
                config.filter.watch_folder_ids.first().cloned()
            }
        };

        // Process each selected item
        let mut trained_count = 0u32;
        for i in 1..=count {
            let item = match crate::com_invoke::dispatch_invoke_method(
                selection, "Item", &[crate::com_invoke::VariantArg::I4(i)]
            ) {
                Ok(p) if !p.is_null() => p,
                _ => {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: Item({}) failed", label, i) });
                    continue;
                }
            };

            // Get MIME content for tokenization
            let content = crate::folder_sink::get_mime_content(item);
            let message_bytes = match content {
                Some(bytes) if !bytes.is_empty() => {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: MIME content {} bytes", label, bytes.len()) });
                    bytes
                }
                _ => {
                    // Fallback to headers + body
                    let body = dispatch_get_string(item, "Body").unwrap_or_default();
                    let headers = Self::get_message_headers(item);
                    let mut c = String::new();
                    if let Some(hdrs) = headers {
                        c.push_str(&hdrs);
                        c.push_str("\r\n\r\n");
                    } else {
                        let subj = dispatch_get_string(item, "Subject").unwrap_or_default();
                        c.push_str(&format!("Subject: {}\r\n\r\n", subj));
                    }
                    c.push_str(&body);
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: fallback content {} bytes", label, c.len()) });
                    c.into_bytes()
                }
            };

            // Tokenize and train
            if let Some(classifier_arc) = &addin.classifier {
                if let Ok(mut classifier) = classifier_arc.lock() {
                    let tokenizer = spambayes_core::tokenizer::Tokenizer::with_defaults();
                    let tokens = tokenizer.tokenize(&message_bytes);
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: {} tokens, training...", label, tokens.len()) });
                    classifier.learn(tokens.into_iter(), is_spam);
                    trained_count += 1;
                } else {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: classifier lock failed", label) });
                }
            } else {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: no classifier", label) });
            }

            // Move to destination folder (spam folder for spam, watch folder for ham)
            if let Some(ref folder_id) = dest_folder_id {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f,
                        "train_as_{}: moving to folder (store={}, entry={}...)",
                        label,
                        &folder_id.store_id.0[..20.min(folder_id.store_id.0.len())],
                        &folder_id.entry_id.0[..20.min(folder_id.entry_id.0.len())]
                    ) });
                // Mark the message as user-moved BEFORE the move, so the
                // ItemAdd event on the destination folder will skip it.
                // This is the Rust equivalent of Python's "train first, move second"
                // pattern that prevents re-filtering.
                crate::folder_sink::mark_as_user_moved(item, !is_spam);
                crate::folder_sink::move_item_to_folder(
                    item, folder_id, &debug_path,
                );
            } else {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f,
                        "train_as_{}: NO destination folder configured, message stays in place", label
                    ) });
            }

            Self::release_dispatch(item);
        }

        Self::release_dispatch(selection);
        Self::release_dispatch(explorer);

        // Save the classifier to disk
        if trained_count > 0 {
            addin.save_dirty_data();
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "train_as_{}: trained {} message(s), saved", label, trained_count) });
        }

        // Invalidate ribbon so button visibility updates
        Self::invalidate_ribbon();
    }

    // ─── Show Clues ────────────────────────────────────────────────────

    /// Score the currently selected message and show the Clues dialog.
    ///
    /// Gets the selected MailItem from the active Explorer, extracts its
    /// Subject and Body, tokenizes + scores it with evidence, formats the
    /// clues as text, and dispatches to the GTK4 CluesDialog.
    ///
    /// Falls back to Win32 MessageBox if GTK4 is unavailable.
    ///
    /// **Validates: Requirements 14.2, 14.3**
    pub fn show_clues_for_selected() {
        unsafe {
            use crate::com_invoke::{dispatch_get, dispatch_get_string};

            let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
            let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");

            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "show_clues: start") });

            if GLOBAL_ADDIN_PTR.is_null() { return; }
            let addin = &*GLOBAL_ADDIN_PTR;

            // Get Application pointer.
            let app_ptr = if !POLL_APP_PTR.is_null() {
                POLL_APP_PTR
            } else {
                match addin.application {
                    Some(p) if !p.is_null() => p,
                    _ => return,
                }
            };

            // Application.ActiveExplorer
            let explorer = match dispatch_get(app_ptr, "ActiveExplorer") {
                Ok(p) if !p.is_null() => p,
                _ => return,
            };

            // Explorer.Selection
            let selection = match dispatch_get(explorer, "Selection") {
                Ok(p) if !p.is_null() => p,
                _ => {
                    Self::release_dispatch(explorer);
                    return;
                }
            };

            // Selection.Item(1) — get the first selected message.
            // If Selection is empty or Item(1) fails, inform the user.
            let item = match crate::com_invoke::dispatch_invoke_method(
                selection, "Item", &[crate::com_invoke::VariantArg::I4(1)]
            ) {
                Ok(p) if !p.is_null() => p,
                _ => {
                    Self::release_dispatch(selection);
                    Self::release_dispatch(explorer);
                    Self::show_win32_error(
                        "Please select a message to show clues for.",
                    );
                    return;
                }
            };

            // Get Subject (for display in the dialog title)
            let subject = dispatch_get_string(item, "Subject")
                .unwrap_or_else(|| "(no subject)".to_string());

            // Get message content using the SAME approach as the filter:
            // 1. Try full MIME content (PR_MIME_CONTENT) — this is what the filter uses
            // 2. Fall back to transport headers + body
            // 3. Last resort: Subject + body
            let scoring_bytes = {
                let mime = crate::folder_sink::get_mime_content(item);
                match mime {
                    Some(bytes) if !bytes.is_empty() => bytes,
                    _ => {
                        // Fallback: headers + body (same as folder_sink fallback)
                        let body = dispatch_get_string(item, "Body")
                            .unwrap_or_default();
                        let headers = Self::get_message_headers(item);
                        let mut content = String::new();
                        if let Some(hdrs) = headers {
                            content.push_str(&hdrs);
                            content.push_str("\r\n\r\n");
                        } else {
                            content.push_str(&format!("Subject: {}\r\n\r\n", subject));
                        }
                        content.push_str(&body);
                        content.into_bytes()
                    }
                }
            };

            Self::release_dispatch(item);
            Self::release_dispatch(selection);
            Self::release_dispatch(explorer);

            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "show_clues: got subject='{}', content_len={}",
                    subject.chars().take(40).collect::<String>(), scoring_bytes.len()) });

            // Score the message with evidence using the classifier
            let clues_text = match &addin.classifier {
                Some(classifier_arc) => {
                    match classifier_arc.lock() {
                        Ok(classifier) => {
                            let tokenizer = spambayes_core::tokenizer::Tokenizer::with_defaults();
                            let tokens_vec = tokenizer.tokenize(&scoring_bytes);

                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                .and_then(|mut f| { use std::io::Write; writeln!(f,
                                    "show_clues: tokenized {} tokens, classifier nham={} nspam={} word_info_size={}",
                                    tokens_vec.len(), classifier.nham(), classifier.nspam(), classifier.word_info().len()
                                ) });

                            // Check how many message tokens exist in the classifier's word_info
                            let word_info = classifier.word_info();
                            {
                                use std::collections::HashSet;
                                let unique: HashSet<&Vec<u8>> = tokens_vec.iter().collect();
                                let matched = unique.iter().filter(|t| word_info.contains_key(**t)).count();
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                    .and_then(|mut f| { use std::io::Write; writeln!(f,
                                        "show_clues: {} unique tokens, {} matched in word_info",
                                        unique.len(), matched
                                    ) });

                                // Log first 5 message tokens and whether they're in word_info
                                for (i, tok) in unique.iter().take(5).enumerate() {
                                    let tok_str = String::from_utf8_lossy(tok);
                                    let in_db = word_info.contains_key(*tok);
                                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                        .and_then(|mut f| { use std::io::Write; writeln!(f,
                                            "show_clues:   token[{}]: '{}' in_db={}",
                                            i, tok_str.chars().take(60).collect::<String>(), in_db
                                        ) });
                                }

                                // Log first 5 word_info keys for comparison
                                for (i, key) in word_info.keys().take(5).enumerate() {
                                    let key_str = String::from_utf8_lossy(key);
                                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                        .and_then(|mut f| { use std::io::Write; writeln!(f,
                                            "show_clues:   db_key[{}]: '{}'",
                                            i, key_str.chars().take(60).collect::<String>()
                                        ) });
                                }

                                // Log probability for first 5 matched tokens
                                let mut prob_logged = 0;
                                for tok in unique.iter() {
                                    if prob_logged >= 5 { break; }
                                    if let Some(wi) = word_info.get(*tok) {
                                        let prob = classifier.probability(wi);
                                        let dist = (prob - 0.5).abs();
                                        let tok_str = String::from_utf8_lossy(tok);
                                        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                            .and_then(|mut f| { use std::io::Write; writeln!(f,
                                                "show_clues:   prob('{}') = {:.6} (ham={}, spam={}, dist={:.4})",
                                                tok_str.chars().take(40).collect::<String>(),
                                                prob, wi.ham_count, wi.spam_count, dist
                                            ) });
                                        prob_logged += 1;
                                    }
                                }

                                // Check: how many tokens have dist > 0 at all?
                                let nonzero_dist = unique.iter().filter(|tok| {
                                    if let Some(wi) = word_info.get(**tok) {
                                        let prob = classifier.probability(wi);
                                        (prob - 0.5).abs() > 0.0001
                                    } else { false }
                                }).count();
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                    .and_then(|mut f| { use std::io::Write; writeln!(f,
                                        "show_clues: tokens with dist>0.0001: {}, min_prob_strength threshold: {}",
                                        nonzero_dist, classifier.config().minimum_prob_strength
                                    ) });
                            }

                            let result = classifier.spam_prob_with_evidence(tokens_vec.iter().cloned());

                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                .and_then(|mut f| { use std::io::Write; writeln!(f,
                                    "show_clues: score={:.6}, clues_count={}",
                                    result.probability,
                                    result.clues.as_ref().map_or(0, |c| c.len())
                                ) });

                            // Gather per-token ham/spam counts from classifier
                            let nham = classifier.nham();
                            let nspam = classifier.nspam();

                            // Build enriched clue data with ham/spam counts
                            let enriched_clues: Option<Vec<(String, f64, u32, u32)>> = result.clues.as_ref().map(|clue_list| {
                                clue_list.iter().map(|(token_str, prob)| {
                                    let token_bytes = token_str.as_bytes().to_vec();
                                    let (hc, sc) = match word_info.get(&token_bytes) {
                                        Some(wi) => (wi.ham_count, wi.spam_count),
                                        None => (0, 0),
                                    };
                                    (token_str.clone(), *prob, hc, sc)
                                }).collect()
                            });

                            // Count total unique tokens in the message
                            let total_tokens = {
                                use std::collections::HashSet;
                                let unique: HashSet<&Vec<u8>> = tokens_vec.iter().collect();
                                unique.len()
                            };

                            Self::format_clues_text(
                                &subject, result.probability, &enriched_clues,
                                nham, nspam, total_tokens,
                            )
                        }
                        Err(_) => {
                            "Error: Could not access the classifier (lock poisoned).".to_string()
                        }
                    }
                }
                None => {
                    "Error: Classifier not loaded. Train some messages first.".to_string()
                }
            };

            // Show clues via the GTK4 clues viewer subprocess.
            // This mirrors the Python approach of spawning a subprocess to
            // avoid COM threading issues with modal dialogs.
            {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "show_clues: launching clues viewer ({} chars)", clues_text.len()) });

                let dll_dir = Self::get_dll_directory();
                let clues_exe = dll_dir.join("spambayes_clues.exe");

                if clues_exe.is_file() {
                    use std::process::{Command, Stdio};
                    use std::io::Write;
                    use std::os::windows::process::CommandExt;

                    match Command::new(&clues_exe)
                        .arg(&subject)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .creation_flags(0x08000000) // CREATE_NO_WINDOW
                        .spawn()
                    {
                        Ok(mut child) => {
                            // Write clues text to the child's stdin.
                            if let Some(mut stdin) = child.stdin.take() {
                                let _ = stdin.write_all(clues_text.as_bytes());
                                // stdin is dropped here, closing the pipe.
                            }
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                .and_then(|mut f| { use std::io::Write; writeln!(f, "show_clues: launched PID {}", child.id()) });
                        }
                        Err(e) => {
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                .and_then(|mut f| { use std::io::Write; writeln!(f, "show_clues: spawn failed: {}", e) });
                            // Fallback to MessageBox if the subprocess can't be launched.
                            let truncated: String = clues_text.chars().take(2000).collect();
                            Self::show_win32_info(&truncated);
                        }
                    }
                } else {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "show_clues: clues exe not found at {:?}", clues_exe) });
                    // Fallback to MessageBox
                    let truncated: String = clues_text.chars().take(2000).collect();
                    Self::show_win32_info(&truncated);
                }
            }
        }
    }

    /// Get transport message headers from an Outlook MailItem.
    ///
    /// Attempts to read the PR_TRANSPORT_MESSAGE_HEADERS property via
    /// PropertyAccessor. Returns None if unavailable.
    ///
    /// Note: The headers are accessed via the PropertyAccessor.GetProperty
    /// method. If the accessor or property is unavailable, returns None
    /// gracefully and the caller uses Subject + Body for scoring.
    unsafe fn get_message_headers(item: *mut c_void) -> Option<String> {
        use crate::com_invoke::dispatch_get;

        // MailItem.PropertyAccessor
        let prop_accessor = dispatch_get(item, "PropertyAccessor").ok()?;
        if prop_accessor.is_null() { return None; }

        // Try to read PR_TRANSPORT_MESSAGE_HEADERS (0x007D001F) as a string
        // property. We use a raw IDispatch::Invoke with PROPERTYGET on the
        // "GetProperty" method, extracting the BSTR result manually.
        let schema = "http://schemas.microsoft.com/mapi/proptag/0x007D001F";
        let result = Self::invoke_get_property_string(prop_accessor, schema);

        Self::release_dispatch(prop_accessor);
        result
    }

    /// Call PropertyAccessor.GetProperty(schema) and extract a string result.
    ///
    /// This is a specialized helper because `dispatch_invoke_method` only
    /// returns VT_DISPATCH results. We need to handle VT_BSTR returns directly.
    unsafe fn invoke_get_property_string(prop_accessor: *mut c_void, schema: &str) -> Option<String> {
        use std::ptr;

        // Resolve "GetProperty" DISPID via IDispatch::GetIDsOfNames
        let method_name = "GetProperty";
        let wide_name: Vec<u16> = method_name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut name_ptr = wide_name.as_ptr() as *mut u16;
        let mut dispid: i32 = 0;

        let vtbl = *(prop_accessor as *const *const usize);
        let get_ids_fn: unsafe extern "system" fn(
            *mut c_void, *const GUID, *mut *mut u16, u32, u32, *mut i32
        ) -> HRESULT = std::mem::transmute(*vtbl.add(5));

        let hr = get_ids_fn(
            prop_accessor,
            &GUID::from_u128(0),
            &mut name_ptr,
            1,
            0,
            &mut dispid,
        );
        if hr.0 != 0 { return None; }

        // Build the BSTR argument for the schema URL
        extern "system" { fn SysAllocString(psz: *const u16) -> *mut u16; }
        extern "system" { fn SysFreeString(bstr_string: *mut u16); }

        let wide_schema: Vec<u16> = schema.encode_utf16().chain(std::iter::once(0)).collect();
        let bstr = SysAllocString(wide_schema.as_ptr());
        if bstr.is_null() { return None; }

        // Use the same VARIANT layout as com_invoke.rs (24 bytes total on 64-bit).
        // Previous code used only 16 bytes which caused a stack buffer overflow
        // when IDispatch::Invoke wrote the 24-byte result VARIANT.
        #[repr(C)]
        struct RawVariant { vt: u16, _pad: [u16; 3], data: [u8; 16] }
        let mut arg_variant = RawVariant { vt: 8, _pad: [0; 3], data: [0; 16] }; // VT_BSTR = 8
        *(arg_variant.data.as_mut_ptr() as *mut *mut u16) = bstr;

        // DISPPARAMS with one arg
        #[repr(C)]
        struct RawDispParams {
            rgvarg: *mut RawVariant,
            rgdispid_named: *mut i32,
            c_args: u32,
            c_named_args: u32,
        }
        let mut params = RawDispParams {
            rgvarg: &mut arg_variant,
            rgdispid_named: ptr::null_mut(),
            c_args: 1,
            c_named_args: 0,
        };

        let mut result_variant = RawVariant { vt: 0, _pad: [0; 3], data: [0; 16] };

        // Invoke GetProperty(schema) — slot 6 in IDispatch vtable
        let invoke_fn: unsafe extern "system" fn(
            *mut c_void, i32, *const GUID, u32, u16,
            *mut RawDispParams, *mut RawVariant, *mut c_void, *mut u32
        ) -> HRESULT = std::mem::transmute(*vtbl.add(6));

        let hr = invoke_fn(
            prop_accessor,
            dispid,
            &GUID::from_u128(0),
            0,
            1 | 2, // DISPATCH_METHOD | DISPATCH_PROPERTYGET
            &mut params,
            &mut result_variant,
            ptr::null_mut(),
            ptr::null_mut() as *mut u32,
        );

        SysFreeString(bstr);

        if hr.0 != 0 { return None; }

        // Extract BSTR from result
        if result_variant.vt == 8 { // VT_BSTR
            let result_bstr = *(result_variant.data.as_ptr() as *const *const u16);
            if result_bstr.is_null() { return Some(String::new()); }
            // Read BSTR length from prefix
            let len_ptr = (result_bstr as *const u8).sub(4) as *const u32;
            let byte_len = *len_ptr as usize;
            let char_len = byte_len / 2;
            let slice = std::slice::from_raw_parts(result_bstr, char_len);
            let s = String::from_utf16_lossy(slice);
            SysFreeString(result_bstr as *mut u16);
            Some(s)
        } else {
            None
        }
    }

    /// Format clues text for display, matching the Python show_clues_dialog format.
    ///
    /// Produces output similar to the Python `GetClues()` function:
    /// - Combined score (percentage and raw probability)
    /// - Training statistics (# ham, # spam trained on)
    /// - Significant tokens table with token, spamprob, #ham, #spam columns
    /// - Total unique tokens in the message
    fn format_clues_text(
        subject: &str,
        probability: f64,
        clues: &Option<Vec<(String, f64, u32, u32)>>,
        nham: u64,
        nspam: u64,
        total_tokens: usize,
    ) -> String {
        let mut text = String::with_capacity(8192);

        let score_pct = probability * 100.0;

        // Classification label based on thresholds
        let classification = if score_pct >= 90.0 {
            "spam"
        } else if score_pct <= 15.0 {
            "good"
        } else {
            "unsure"
        };

        // Header — matches Python's "Combined Score: X% (raw)"
        text.push_str(&format!("Subject: {}\n", subject));
        text.push_str(&format!("=== Combined Score: {}% ({:.6}) ===\n\n",
            Self::format_score_pct(score_pct), probability));

        text.push_str(&format!("Classification: {}\n", classification));
        text.push_str(&format!("# ham trained on: {}\n", nham));
        text.push_str(&format!("# spam trained on: {}\n\n", nspam));

        // Significant tokens table — matches Python's column format:
        // "token                               spamprob         #ham  #spam"
        match clues {
            Some(clue_list) if !clue_list.is_empty() => {
                text.push_str(&format!("=== {} Significant Tokens ===\n\n", clue_list.len()));
                text.push_str(&format!(
                    "{:<35} {:>12} {:>6} {:>6}\n",
                    "token", "spamprob", "#ham", "#spam"
                ));
                text.push_str(&format!("{}\n", "-".repeat(65)));

                // Sort by distance from 0.5 (most discriminative first)
                let mut sorted_clues = clue_list.clone();
                sorted_clues.sort_by(|a, b| {
                    let dist_a = (a.1 - 0.5).abs();
                    let dist_b = (b.1 - 0.5).abs();
                    dist_b.partial_cmp(&dist_a).unwrap_or(std::cmp::Ordering::Equal)
                });

                for (token, prob, ham_count, spam_count) in &sorted_clues {
                    // Truncate long tokens for display
                    let display_token: String = token.chars().take(34).collect();
                    text.push_str(&format!(
                        "{:<35} {:>12} {:>6} {:>6}\n",
                        display_token,
                        Self::format_internal_score(*prob),
                        ham_count,
                        spam_count,
                    ));
                }
            }
            _ => {
                text.push_str("No significant clues found.\n");
                text.push_str("The classifier may need more training data.\n");
            }
        }

        // Total tokens in message
        text.push_str(&format!("\n{} unique tokens in message\n", total_tokens));

        text
    }

    /// Format a spam score percentage with appropriate precision,
    /// matching the Python `FormatScorePercent`.
    fn format_score_pct(score_pct: f64) -> String {
        if score_pct < 0.01 {
            format!("{:.4}", score_pct)
        } else if score_pct < 1.0 {
            format!("{:.2}", score_pct)
        } else {
            format!("{}", score_pct.round() as i64)
        }
    }

    /// Format an internal probability score with appropriate precision,
    /// matching the Python `FormatInternalScore`.
    fn format_internal_score(score: f64) -> String {
        if score < 1e-9 {
            format!("{:.2e}", score)
        } else if score < 0.000001 {
            format!("{:.9}", score)
        } else if score < 0.0001 {
            format!("{:.6}", score)
        } else if score > 0.9999 {
            format!("{:.6}", score)
        } else {
            format!("{:.4}", score)
        }
    }

    /// Check if the classifier is loaded and has training data.
    ///
    /// Used by the ribbon's `GetShowCluesEnabled` callback to enable/disable
    /// the "Show Clues" button.
    pub fn is_classifier_loaded() -> bool {
        unsafe {
            if GLOBAL_ADDIN_PTR.is_null() { return false; }
            let addin = &*GLOBAL_ADDIN_PTR;
            addin.classifier.is_some()
        }
    }

    // ─── Ribbon Visibility Helpers ───────────────────────────────────────

    /// Check if the current Explorer folder is the configured spam folder.
    ///
    /// Returns `true` if the user is viewing the spam folder (but NOT the unsure folder).
    /// Used by `GetSpamVisible` — the Spam button should be hidden when in the spam folder.
    ///
    /// Logic matches the Python version's `OnFolderSwitch`:
    /// - Spam folder: hide "Spam", show "Not Spam"
    /// - Unsure folder: show both
    /// - Normal folder: show "Spam", hide "Not Spam"
    pub fn is_in_spam_folder_only() -> bool {
        unsafe {
            let addin = GLOBAL_ADDIN_PTR;
            if addin.is_null() { return false; }
            let addin = &*addin;

            let config = match &addin.config {
                Some(c) => c,
                None => return false,
            };

            let spam_folder_id = match &config.filter.spam_folder_id {
                Some(id) => id,
                None => return false,
            };

            let current = match Self::get_current_folder_id(addin) {
                Some(id) => id,
                None => return false,
            };

            // Case-insensitive comparison (Outlook may return different case than INI)
            Self::folder_ids_equal(&current, spam_folder_id)
        }
    }

    /// Check if the current Explorer folder is the spam folder OR unsure folder.
    ///
    /// Returns `true` if viewing spam or unsure folder.
    /// Used by `GetNotSpamVisible` — the "Not Spam" button should be visible
    /// when in spam folder or unsure folder.
    pub fn is_in_spam_or_unsure_folder() -> bool {
        unsafe {
            let addin = GLOBAL_ADDIN_PTR;
            if addin.is_null() { return false; }
            let addin = &*addin;

            let config = match &addin.config {
                Some(c) => c,
                None => return false,
            };

            let current = match Self::get_current_folder_id(addin) {
                Some(id) => id,
                None => return false,
            };

            let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
            let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f,
                    "is_in_spam_or_unsure_folder: current=({}, {})",
                    current.store_id.0, current.entry_id.0) });

            // Check spam folder (case-insensitive)
            if let Some(spam_id) = &config.filter.spam_folder_id {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f,
                        "  spam_folder=({}, {})",
                        spam_id.store_id.0, spam_id.entry_id.0) });
                if Self::folder_ids_equal(&current, spam_id) {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "  -> MATCH spam folder") });
                    return true;
                }
            }

            // Check unsure folder (case-insensitive)
            if let Some(unsure_id) = &config.filter.unsure_folder_id {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f,
                        "  unsure_folder=({}, {})",
                        unsure_id.store_id.0, unsure_id.entry_id.0) });
                if Self::folder_ids_equal(&current, unsure_id) {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "  -> MATCH unsure folder") });
                    return true;
                }
            }

            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "  -> NO MATCH") });

            false
        }
    }

    /// Compare two FolderIds using MAPI CompareEntryIDs.
    ///
    /// Outlook uses different entry ID formats (short-term vs long-term).
    /// Simple string comparison fails because the INI may store short-term IDs
    /// while the Object Model returns long-term IDs. MAPI's CompareEntryIDs
    /// handles this correctly by comparing the underlying binary data.
    ///
    /// Falls back to case-insensitive hex string comparison if MAPI is unavailable.
    fn folder_ids_equal(a: &spambayes_config::FolderId, b: &spambayes_config::FolderId) -> bool {
        // Store IDs must match first (case-insensitive hex comparison is fine for store IDs)
        if !a.store_id.0.eq_ignore_ascii_case(&b.store_id.0) {
            return false;
        }

        // Try MAPI CompareEntryIDs for the entry IDs
        unsafe {
            let addin = GLOBAL_ADDIN_PTR;
            if !addin.is_null() {
                let addin = &mut *addin;
                if let Some(ref mut session) = addin.mapi_session {
                    // Decode both entry IDs from hex to bytes
                    if let (Some(eid_a), Some(eid_b)) = (
                        Self::hex_to_bytes(&a.entry_id.0),
                        Self::hex_to_bytes(&b.entry_id.0),
                    ) {
                        // Decode store ID to open the store
                        if let Some(store_eid) = Self::hex_to_bytes(&a.store_id.0) {
                            if let Ok(store_ptr) = session.open_store(&store_eid) {
                                return Self::mapi_compare_entry_ids(
                                    store_ptr, &eid_a, &eid_b,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Fallback: case-insensitive hex comparison
        a.entry_id.0.eq_ignore_ascii_case(&b.entry_id.0)
    }

    /// Decode a hex string to bytes.
    fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
        let hex = hex.trim();
        if hex.len() % 2 != 0 { return None; }
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect()
    }

    /// Call IMsgStore::CompareEntryIDs (vtable slot 16).
    ///
    /// Returns `true` if the two entry IDs refer to the same MAPI object.
    unsafe fn mapi_compare_entry_ids(
        store_ptr: *mut std::ffi::c_void,
        eid_a: &[u8],
        eid_b: &[u8],
    ) -> bool {
        // IMsgStore vtable layout:
        //   IUnknown (0-2), IMAPIProp (3-13), IMsgStore (14+)
        //   Slot 14: Advise, 15: Unadvise, 16: CompareEntryIDs, 17: OpenEntry
        //
        // We just read the function pointer at slot 16 from the vtable.
        let vtbl_ptr = *(store_ptr as *const *const *const std::ffi::c_void);

        // CompareEntryIDs is at slot 16
        let compare_fn_ptr = *vtbl_ptr.add(16);
        let compare_fn: unsafe extern "system" fn(
            *mut std::ffi::c_void, u32, *const u8, u32, *const u8, u32, *mut u32,
        ) -> i32 = std::mem::transmute(compare_fn_ptr);

        let mut result: u32 = 0;
        let hr = compare_fn(
            store_ptr,
            eid_a.len() as u32,
            eid_a.as_ptr(),
            eid_b.len() as u32,
            eid_b.as_ptr(),
            0, // flags
            &mut result,
        );

        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f,
                "  CompareEntryIDs: hr=0x{:08X}, result={}", hr as u32, result) });

        hr == 0 && result != 0
    }

    /// Get the FolderId of the currently displayed folder in the active Explorer.
    ///
    /// Uses Outlook Object Model:
    ///   Application.ActiveExplorer.CurrentFolder → get StoreID and EntryID
    unsafe fn get_current_folder_id(addin: &AddinCore) -> Option<spambayes_config::FolderId> {
        use crate::com_invoke::{dispatch_get, dispatch_get_string};

        // Prefer POLL_APP_PTR (obtained via CoCreateInstance, more reliable)
        // Fall back to the stored application pointer from OnConnection
        let app_ptr = if !POLL_APP_PTR.is_null() {
            POLL_APP_PTR
        } else {
            addin.application?
        };
        if app_ptr.is_null() { return None; }

        // Application.ActiveExplorer
        let explorer = dispatch_get(app_ptr, "ActiveExplorer").ok()?;
        if explorer.is_null() { return None; }

        // Explorer.CurrentFolder
        let folder = dispatch_get(explorer, "CurrentFolder").ok()?;
        if folder.is_null() {
            Self::release_dispatch(explorer);
            return None;
        }

        // Folder.StoreID
        let store_id_str = dispatch_get_string(folder, "StoreID");
        // Folder.EntryID
        let entry_id_str = dispatch_get_string(folder, "EntryID");

        Self::release_dispatch(folder);
        Self::release_dispatch(explorer);

        match (store_id_str, entry_id_str) {
            (Some(store), Some(entry)) if !store.is_empty() && !entry.is_empty() => {
                Some(spambayes_config::FolderId::new(
                    spambayes_config::StoreId::new(&store),
                    spambayes_config::EntryId::new(&entry),
                ))
            }
            _ => None,
        }
    }

    /// Release an IDispatch pointer (call IUnknown::Release).
    unsafe fn release_dispatch(ptr: *mut c_void) {
        if !ptr.is_null() {
            let vtbl = *(ptr as *const *const usize);
            let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*vtbl.add(2));
            release_fn(ptr);
        }
    }

    /// Get the Count property from a Selection object as an integer.
    ///
    /// Selection.Count returns VT_I4, so dispatch_get_string won't work.
    /// This reads the VARIANT result directly.
    unsafe fn get_selection_count(selection: *mut c_void) -> i32 {
        use crate::com_invoke::dispatch_get_string;

        // First try: Some versions return Count as a string
        if let Some(s) = dispatch_get_string(selection, "Count") {
            if let Ok(n) = s.trim().parse::<i32>() {
                return n;
            }
        }

        // Direct approach: invoke Count and read VT_I4 from the VARIANT
        let dispid = match crate::com_invoke::get_dispid(selection, "Count") {
            Ok(id) => id,
            Err(_) => return 0,
        };

        let vtbl = *(selection as *const *const crate::com_invoke::IDispatchVtbl);

        let mut params = crate::com_invoke::DISPPARAMS {
            rgvarg: std::ptr::null_mut(),
            rgdispid_named_args: std::ptr::null_mut(),
            c_args: 0,
            c_named_args: 0,
        };

        let mut result = crate::com_invoke::VARIANT::default();

        let hr = ((*vtbl).invoke)(
            selection,
            dispid,
            &crate::com_invoke::IID_NULL,
            0,
            2 | 1, // DISPATCH_PROPERTYGET | DISPATCH_METHOD
            &raw mut params,
            &raw mut result,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );

        if hr.is_err() {
            return 0;
        }

        // VT_I4 = 3
        if result.vt == 3 {
            *(result.data.as_ptr().cast::<i32>())
        } else {
            0
        }
    }

    /// Invalidate the ribbon UI, causing Outlook to re-query all getVisible/getEnabled callbacks.
    ///
    /// Should be called when the folder changes so button visibility updates.
    pub unsafe fn invalidate_ribbon() {
        let ribbon = GLOBAL_RIBBON_UI;
        if ribbon.is_null() { return; }

        // Call IRibbonUI::Invalidate() via IDispatch::Invoke
        // IRibbonUI exposes Invalidate as a method; we call it via its IDispatch interface.
        use crate::com_invoke::dispatch_invoke_method;
        let _ = dispatch_invoke_method(ribbon, "Invalidate", &[]);
    }

    // ─── Helper Methods ──────────────────────────────────────────────────

    /// Load application configuration using the ConfigChain layered approach.
    ///
    /// Resolution order:
    /// 1. Check `BAYESCUSTOMIZE` env var — if set, use `ConfigChain::load_from_env()`
    /// 2. Otherwise, get profile name from MAPI session (or "default" if unavailable)
    /// 3. Call `ConfigChain::load(profile_name)` for layered INI loading
    ///
    /// Stores results in both `self.config_chain` and `self.config` for
    /// backward compatibility with existing code that reads `self.config`.
    ///
    /// **Validates: Requirements 1.1, 1.6, 3.1, 3.2, 4.4**
    fn load_config_chain(&mut self) {
        // Check BAYESCUSTOMIZE environment variable first.
        if let Ok(env_value) = std::env::var("BAYESCUSTOMIZE") {
            if !env_value.is_empty() {
                self.log_info(&format!(
                    "BAYESCUSTOMIZE env var set: \"{}\", using env-based config loading",
                    env_value
                ));
                match ConfigChain::load_from_env(&env_value) {
                    Ok(chain) => {
                        self.log_info("Config loaded via BAYESCUSTOMIZE");
                        self.config = Some(chain.config().clone());
                        self.config_chain = Some(chain);
                        return;
                    }
                    Err(e) => {
                        self.log_error(&format!(
                            "Failed to load config from BAYESCUSTOMIZE: {e}, falling back to defaults"
                        ));
                    }
                }
            }
        }

        // Determine profile name from MAPI session (if available).
        let profile_name = self.get_mapi_profile_name().unwrap_or_else(|| {
            self.log_info("MAPI profile name not available, using \"default\"");
            "default".to_string()
        });

        self.log_info(&format!("Loading config chain for profile: \"{}\"", profile_name));

        // Load via ConfigChain (layered: defaults → default.ini → profile.ini).
        match ConfigChain::load(&profile_name) {
            Ok(chain) => {
                self.log_info(&format!(
                    "Config chain loaded for profile \"{}\" (data_dir: {})",
                    chain.profile_name(),
                    chain.data_directory().display()
                ));
                let mut config = chain.config().clone();

                // If the chain didn't find folder IDs, try migrating from the
                // Python Outlook.ini which stores them in a different location.
                if config.filter.spam_folder_id.is_none() {
                    self.log_info("No spam_folder_id in chain config, trying Python migration...");
                    if let Some(migrated) = spambayes_config::try_migrate(
                        chain.data_directory(),
                        &profile_name,
                    ) {
                        self.log_info("Python config migrated successfully");
                        // Merge the folder IDs from the migrated config
                        if config.filter.spam_folder_id.is_none() {
                            config.filter.spam_folder_id = migrated.filter.spam_folder_id;
                        }
                        if config.filter.unsure_folder_id.is_none() {
                            config.filter.unsure_folder_id = migrated.filter.unsure_folder_id;
                        }
                        if config.filter.watch_folder_ids.is_empty() {
                            config.filter.watch_folder_ids = migrated.filter.watch_folder_ids;
                        }
                        if config.training.ham_folder_ids.is_empty() {
                            config.training.ham_folder_ids = migrated.training.ham_folder_ids;
                        }
                        if config.training.spam_folder_ids.is_empty() {
                            config.training.spam_folder_ids = migrated.training.spam_folder_ids;
                        }
                    } else {
                        self.log_info("No Python config found for migration");
                    }
                }

                self.config = Some(config);
                self.config_chain = Some(chain);
            }
            Err(e) => {
                self.log_error(&format!(
                    "Failed to load config chain for profile \"{}\": {e}, using defaults",
                    profile_name
                ));
                self.config = Some(AppConfig::default());
            }
        }
    }

    /// Get the profile name from the MAPI session, if available.
    ///
    /// Returns `None` if the MAPI session hasn't been initialized or
    /// the profile name couldn't be retrieved.
    fn get_mapi_profile_name(&self) -> Option<String> {
        #[cfg(target_os = "windows")]
        {
            if let Some(session) = &self.mapi_session {
                match session.get_profile_name() {
                    Ok(name) => {
                        self.log_info(&format!("MAPI profile name: \"{}\"", name));
                        return Some(name);
                    }
                    Err(e) => {
                        self.log_error(&format!("Failed to get MAPI profile name: {e}"));
                    }
                }
            }
        }
        None
    }

    /// Load application configuration from INI files (legacy method).
    ///
    /// Falls back to defaults if the config file cannot be loaded.
    /// Retained for potential use as a fallback in non-standard scenarios.
    #[allow(dead_code)]
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

    /// Reload configuration from disk after the manager subprocess exits.
    ///
    /// Re-reads the INI file and updates both the in-memory `self.config` and
    /// the `FolderHookState` config so that filter settings (thresholds, actions,
    /// calendar config, etc.) take effect immediately without restarting Outlook.
    fn reload_config_from_disk(&mut self) {
        let data_dir = Self::get_data_directory();
        let profile_name = self.get_mapi_profile_name()
            .unwrap_or_else(|| "default".to_string());

        self.log_info(&format!(
            "reload_config_from_disk: reloading from {}/{}.ini",
            data_dir.display(), profile_name
        ));

        let new_config = match AppConfig::load(&data_dir, &profile_name) {
            Ok(config) => config,
            Err(e) => {
                self.log_error(&format!(
                    "reload_config_from_disk: failed to load config: {e}"
                ));
                return;
            }
        };

        // Update the in-memory config
        self.config = Some(new_config.clone());

        // Update the config chain if we have one
        if let Some(chain) = &mut self.config_chain {
            *chain.config_mut() = new_config.clone();
        }

        // Update the FolderHookState so the live event handler picks up changes
        if let Some(state_arc) = &self.folder_hook_state {
            if let Ok(mut state) = state_arc.lock() {
                state.config = new_config.clone();
                self.log_info("reload_config_from_disk: FolderHookState updated");
            } else {
                self.log_error("reload_config_from_disk: FolderHookState lock poisoned");
            }
        }

        self.log_info(&format!(
            "reload_config_from_disk: complete (filter.enabled={}, calendar.enabled={}, spam_threshold={:.1})",
            new_config.filter.enabled,
            new_config.calendar.calendar_filtering_enabled,
            new_config.filter.spam_threshold
        ));
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
            self.statistics.clone(),
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
                self.statistics.clone(),
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
            None => {
                self.log_info("setup_folder_hooks: config is None, skipping");
                return;
            }
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
            None,
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

    /// Get the directory containing this DLL.
    ///
    /// Used to locate `spambayes_manager.exe` which is installed alongside the DLL.
    fn get_dll_directory() -> PathBuf {
        use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
        let hmodule = crate::get_dll_module();
        let mut buf = vec![0u16; 1024];
        let len = unsafe { GetModuleFileNameW(hmodule, &mut buf) } as usize;
        if len > 0 && len < buf.len() {
            let path = String::from_utf16_lossy(&buf[..len]);
            if let Some(parent) = PathBuf::from(path).parent() {
                return parent.to_path_buf();
            }
        }
        // Fallback: assume installed in Program Files
        PathBuf::from(r"C:\Program Files\SpamBayes\x64")
    }

    /// Show a Win32 MessageBox error (no GTK4 dependency).
    fn show_win32_error(message: &str) {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let title: Vec<u16> = "SpamBayes\0".encode_utf16().collect();
        let msg: Vec<u16> = format!("{message}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(msg.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    /// Show a Win32 MessageBox with information icon.
    fn show_win32_info(message: &str) {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        let title: Vec<u16> = "SpamBayes - Clues\0".encode_utf16().collect();
        let msg: Vec<u16> = format!("{message}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(msg.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// Log an error message if the logger is available.
    fn log_error(&self, message: &str) {
        if let Some(logger) = &self.logger {
            logger.error("addin_core", message);
        }
    }
}


// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::AddinCore;
    use spambayes_core::classifier::Classifier;
    use spambayes_core::tokenizer::Tokenizer;
    use spambayes_core::ClassifierConfig;

    // ─── format_score_pct ────────────────────────────────────────────────

    #[test]
    fn format_score_pct_very_small() {
        assert_eq!(AddinCore::format_score_pct(0.005), "0.0050");
    }

    #[test]
    fn format_score_pct_small() {
        assert_eq!(AddinCore::format_score_pct(0.75), "0.75");
    }

    #[test]
    fn format_score_pct_normal() {
        assert_eq!(AddinCore::format_score_pct(42.7), "43");
    }

    #[test]
    fn format_score_pct_high() {
        assert_eq!(AddinCore::format_score_pct(99.9), "100");
    }

    // ─── format_internal_score ───────────────────────────────────────────

    #[test]
    fn format_internal_score_very_small() {
        let s = AddinCore::format_internal_score(1e-12);
        assert!(s.contains("e"), "Expected scientific notation, got: {}", s);
    }

    #[test]
    fn format_internal_score_small() {
        let s = AddinCore::format_internal_score(0.00005);
        assert_eq!(s, "0.000050");
    }

    #[test]
    fn format_internal_score_near_one() {
        let s = AddinCore::format_internal_score(0.99999);
        assert_eq!(s, "0.999990");
    }

    #[test]
    fn format_internal_score_normal() {
        let s = AddinCore::format_internal_score(0.85);
        assert_eq!(s, "0.8500");
    }

    // ─── format_clues_text ───────────────────────────────────────────────

    #[test]
    fn format_clues_text_with_clues() {
        let clues = Some(vec![
            ("viagra".to_string(), 0.99, 0u32, 15u32),
            ("hello".to_string(), 0.1, 20, 1),
        ]);
        let text = AddinCore::format_clues_text(
            "Test Subject", 0.95, &clues, 100, 200, 50,
        );
        assert!(text.contains("Subject: Test Subject"));
        assert!(text.contains("95%"));
        assert!(text.contains("spam"));
        assert!(text.contains("# ham trained on: 100"));
        assert!(text.contains("# spam trained on: 200"));
        assert!(text.contains("2 Significant Tokens"));
        assert!(text.contains("viagra"));
        assert!(text.contains("hello"));
        assert!(text.contains("50 unique tokens"));
    }

    #[test]
    fn format_clues_text_no_clues() {
        let text = AddinCore::format_clues_text(
            "Empty", 0.5, &Some(vec![]), 10, 20, 30,
        );
        assert!(text.contains("No significant clues found"));
        assert!(text.contains("30 unique tokens"));
    }

    #[test]
    fn format_clues_text_classification_good() {
        let text = AddinCore::format_clues_text(
            "Ham", 0.05, &None, 10, 10, 10,
        );
        assert!(text.contains("Classification: good"));
    }

    #[test]
    fn format_clues_text_classification_unsure() {
        let text = AddinCore::format_clues_text(
            "Maybe", 0.5, &None, 10, 10, 10,
        );
        assert!(text.contains("Classification: unsure"));
    }

    // ─── Training + Scoring integration ──────────────────────────────────

    /// Helper: build a simple RFC-822 message for tokenization.
    fn make_message(subject: &str, body: &str) -> Vec<u8> {
        format!(
            "From: sender@example.com\r\n\
             To: recipient@example.com\r\n\
             Subject: {}\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             {}",
            subject, body
        ).into_bytes()
    }

    #[test]
    fn train_and_score_produces_clues() {
        let config = ClassifierConfig::default();
        let mut classifier = Classifier::new(config);
        let tokenizer = Tokenizer::with_defaults();

        // Train several distinct spam messages
        let spam_msgs = vec![
            make_message("Buy cheap viagra now!", "Click here to buy viagra pills cheap price"),
            make_message("You won the lottery!", "Claim your million dollar prize money now"),
            make_message("Free pills online", "Order cheap pharmacy drugs online free shipping"),
        ];
        for msg in &spam_msgs {
            let tokens = tokenizer.tokenize(msg);
            classifier.learn(tokens.into_iter(), true);
        }

        // Train several distinct ham messages
        let ham_msgs = vec![
            make_message("Meeting tomorrow", "Hi team, reminder about the 10am meeting tomorrow"),
            make_message("Project update", "The deployment went well, all tests passing"),
            make_message("Lunch plans", "Want to grab lunch at the new place downtown?"),
        ];
        for msg in &ham_msgs {
            let tokens = tokenizer.tokenize(msg);
            classifier.learn(tokens.into_iter(), false);
        }

        // Now score a spam-like message
        let test_msg = make_message(
            "Amazing deals on viagra",
            "Buy cheap viagra pills online free shipping",
        );
        let test_tokens = tokenizer.tokenize(&test_msg);
        let result = classifier.spam_prob_with_evidence(test_tokens.into_iter());

        // Should have clues (tokens that are discriminative)
        let clues = result.clues.unwrap();
        assert!(!clues.is_empty(), "Expected clues but got none");

        // Score should lean toward spam (> 0.5)
        assert!(
            result.probability > 0.6,
            "Expected spam probability > 0.6, got {}",
            result.probability
        );

        // Verify that discriminative tokens appear in clues
        let clue_names: Vec<&str> = clues.iter().map(|(name, _)| name.as_str()).collect();
        // At least some spam-associated tokens should appear
        let has_spam_token = clue_names.iter().any(|t| {
            t.contains("viagra") || t.contains("cheap") || t.contains("pills")
                || t.contains("free") || t.contains("buy")
        });
        assert!(has_spam_token, "Expected spam tokens in clues, got: {:?}", clue_names);
    }

    #[test]
    fn train_and_score_ham_message() {
        let config = ClassifierConfig::default();
        let mut classifier = Classifier::new(config);
        let tokenizer = Tokenizer::with_defaults();

        // Train spam
        for msg in &[
            make_message("Buy now!", "Cheap viagra pills online pharmacy"),
            make_message("You won!", "Claim million dollar prize lottery winner"),
        ] {
            let tokens = tokenizer.tokenize(msg);
            classifier.learn(tokens.into_iter(), true);
        }

        // Train ham
        for msg in &[
            make_message("Meeting", "Team meeting tomorrow at 10am in conference room"),
            make_message("Code review", "Please review the pull request for the auth module"),
        ] {
            let tokens = tokenizer.tokenize(msg);
            classifier.learn(tokens.into_iter(), false);
        }

        // Score a ham-like message
        let test_msg = make_message(
            "Sprint planning",
            "Team standup and sprint planning tomorrow morning conference room",
        );
        let test_tokens = tokenizer.tokenize(&test_msg);
        let result = classifier.spam_prob_with_evidence(test_tokens.into_iter());

        // Score should lean toward ham (< 0.5)
        assert!(
            result.probability < 0.4,
            "Expected ham probability < 0.4, got {}",
            result.probability
        );
    }

    #[test]
    fn untrained_classifier_returns_neutral_score() {
        let config = ClassifierConfig::default();
        let classifier = Classifier::new(config);
        let tokenizer = Tokenizer::with_defaults();

        let msg = make_message("Hello", "This is a test message body");
        let tokens = tokenizer.tokenize(&msg);
        let result = classifier.spam_prob_with_evidence(tokens.into_iter());

        // Untrained classifier gives 0.5 (neutral)
        assert!(
            (result.probability - 0.5).abs() < 1e-10,
            "Expected 0.5, got {}",
            result.probability
        );
        // No clues because all tokens are unknown (prob = 0.5, dist = 0)
        assert_eq!(result.clues.unwrap().len(), 0);
    }

    #[test]
    fn training_same_messages_as_both_ham_and_spam_gives_neutral() {
        // This reproduces the bug we saw: if ham:spam ratio is constant across all tokens
        let config = ClassifierConfig::default();
        let mut classifier = Classifier::new(config);
        let tokenizer = Tokenizer::with_defaults();

        let msg = make_message("Test", "Some common words here");
        let tokens = tokenizer.tokenize(&msg);

        // Train the exact same message as both ham and spam
        classifier.learn(tokens.clone().into_iter(), false); // ham
        classifier.learn(tokens.clone().into_iter(), true);  // spam

        // Score should be neutral
        let result = classifier.spam_prob_with_evidence(tokens.into_iter());
        // With nham=1, nspam=1, and equal counts: prob should be ~0.5 for all tokens
        assert!(
            (result.probability - 0.5).abs() < 0.1,
            "Expected near 0.5, got {}",
            result.probability
        );
    }

    #[test]
    fn word_info_lookup_matches_clue_tokens() {
        // Verify that token strings from clues can be looked up in word_info
        let config = ClassifierConfig::default();
        let mut classifier = Classifier::new(config);
        let tokenizer = Tokenizer::with_defaults();

        // Train with distinct content
        let spam = make_message("SPAM", "viagra viagra viagra cheap pills");
        let ham = make_message("HAM", "meeting project code review deploy");
        classifier.learn(tokenizer.tokenize(&spam).into_iter(), true);
        classifier.learn(tokenizer.tokenize(&ham).into_iter(), false);

        // Score
        let test = make_message("Test", "viagra meeting");
        let tokens = tokenizer.tokenize(&test);
        let result = classifier.spam_prob_with_evidence(tokens.into_iter());

        let clues = result.clues.unwrap();
        let word_info = classifier.word_info();

        // Every clue token's bytes should be findable in word_info
        for (token_str, _prob) in &clues {
            let token_bytes = token_str.as_bytes().to_vec();
            assert!(
                word_info.contains_key(&token_bytes),
                "Clue token '{}' not found in word_info",
                token_str
            );
        }
    }
}
