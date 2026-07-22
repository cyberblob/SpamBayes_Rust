#![warn(clippy::pedantic)]
// ── Pedantic allow-list (documented exceptions) ──────────────────────────────
// 1. doc_markdown: COM/Windows identifiers (IClassFactory, SpamBayes, HRESULT,
//    etc.) are domain terms that don't benefit from backtick formatting.
#![allow(clippy::doc_markdown)]
// 2. cast_possible_wrap: COM HRESULT constants are defined as u32 bit patterns
//    intentionally cast to i32 — this is standard Windows COM practice.
#![allow(clippy::cast_possible_wrap)]
// 3. missing_errors_doc: Trait methods and internal helpers where error
//    conditions are self-evident from the return type and context.
#![allow(clippy::missing_errors_doc)]
// 4. missing_panics_doc: Functions with documented-safe unwraps (checked above).
#![allow(clippy::missing_panics_doc)]
// 5. module_name_repetitions: Crate-prefixed type names (e.g., FilterError in
//    filter module) follow project convention for clarity at use-sites.
#![allow(clippy::module_name_repetitions)]
// 6. cast_possible_truncation: Controlled numeric casts (usize→u32 for message
//    counts, f64→u32 for timer ms) are bounded by domain constraints.
#![allow(clippy::cast_possible_truncation)]
// 7. cast_sign_loss: Timer and timestamp conversions where values are
//    always non-negative by construction.
#![allow(clippy::cast_sign_loss)]
// 8. too_many_lines: Complex orchestration functions (filter_now, train_batch)
//    are naturally long due to error handling and progress reporting.
#![allow(clippy::too_many_lines)]
// 9. too_many_arguments: Functions matching the Python interface that require
//    many config/state parameters.
#![allow(clippy::too_many_arguments)]
// 10. unreadable_literal: COM HRESULT hex constants follow Windows convention.
#![allow(clippy::unreadable_literal)]
// ── Additional style allows for this FFI/COM integration crate ───────────────
// items_after_statements: Struct declarations near their FFI usage point.
#![allow(clippy::items_after_statements)]
// unused_self: Methods that will use self.config/state in future iterations.
#![allow(clippy::unused_self)]
// trivially_copy_pass_by_ref: Consistent API with trait method signatures.
#![allow(clippy::trivially_copy_pass_by_ref)]
// manual_let_else: Many match-then-return patterns in COM/registry code are
// clearer with explicit match for commented early-return reasoning.
#![allow(clippy::manual_let_else)]
// assigning_clones: clone_from() optimization—marginal benefit for config
// structs that are assigned once during setup.
#![allow(clippy::assigning_clones)]
// new_without_default: COM objects (ClassFactory, ErrorReporter) have special
// initialization that doesn't fit Default semantics.
#![allow(clippy::new_without_default)]
// match_same_arms: Placeholder match arms in wizard/manager stubs document
// future distinct behavior per dialog control.
#![allow(clippy::match_same_arms)]
// format_collect: Hex encoding uses map+format for readability over fold+write.
#![allow(clippy::format_collect)]
// new_ret_no_self: COM ClassFactory::new() returns raw *mut c_void for COM ABI.
#![allow(clippy::new_ret_no_self)]
// ref_option: &Option<T> in trait method signatures for API compatibility.
#![allow(clippy::ref_option)]

//! `SpamBayes` Addin - COM add-in DLL for Outlook.
//!
//! This is the final artifact that Outlook loads. It implements
//! `IDTExtensibility2`, `IClassFactory`, and the DLL entry points
//! (`DllGetClassObject`, `DllRegisterServer`, etc.).
//!
//! # Error Handling
//!
//! All sub-system errors are unified under [`AppError`], which provides
//! automatic `From` conversions for each crate's error type. This
//! satisfies Requirement 21.1.
//!
//! # Logging
//!
//! The [`LogLevel`] enum supports configurable verbosity where higher
//! levels include lower ones (Requirement 17.3).

pub mod addin_core;
pub mod class_factory;
pub mod com_invoke;
pub mod error_reporter;
pub mod export;
pub mod filter;
pub mod folder_sink;
#[cfg(feature = "gui")]
pub mod gui;
pub mod help_content;
#[cfg(feature = "gui")]
pub mod splash_window;
pub mod help_dialog;
pub mod help_dialog_template;
pub mod logger;
pub mod manager_dlg;
pub mod notification;
pub mod registry;
pub mod ribbon;
pub mod statistics;
pub mod sync_sink;
pub mod timer;
pub mod toolbar;
pub mod tooltip_manager;
pub mod train;
#[cfg(feature = "gui")]
pub mod training_bridge;
pub mod updater;
pub mod version_manifest;
pub mod wizard;

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use thiserror::Error;
use windows::core::{GUID, HRESULT};
use windows::Win32::Foundation::HMODULE;

use spambayes_config::ConfigError;
use spambayes_core::ClassifierError;
use spambayes_mapi::MsgStoreError;
use spambayes_storage::StorageError;

// ─── DLL Module Handle ───────────────────────────────────────────────────────

/// Stored module handle for this DLL, set during `DllMain`.
///
/// Used by `registry::get_dll_path()` to retrieve the DLL's filesystem path
/// (as opposed to the host process path).
static DLL_HMODULE: AtomicUsize = AtomicUsize::new(0);

/// Store the DLL module handle (called from `DllMain`).
pub fn set_dll_module(h: HMODULE) {
    DLL_HMODULE.store(h.0 as usize, Ordering::SeqCst);
}

/// Retrieve the stored DLL module handle.
pub fn get_dll_module() -> HMODULE {
    HMODULE(DLL_HMODULE.load(Ordering::SeqCst) as *mut c_void)
}

/// DLL entry point — captures the module handle when the DLL is loaded.
///
/// # Safety
///
/// Called by the Windows loader. `hinstdll` is the DLL's base address.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    hinstdll: HMODULE,
    fdw_reason: u32,
    _lpv_reserved: *mut c_void,
) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if fdw_reason == DLL_PROCESS_ATTACH {
        set_dll_module(hinstdll);
        // Early trace to confirm DLL is actually loaded
        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&debug_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "DllMain: DLL_PROCESS_ATTACH, hinstance={:?}", hinstdll.0)
            });
    }
    1 // TRUE
}

// ─── COM Constants ───────────────────────────────────────────────────────────

/// The CLSID for the `SpamBayes` Outlook Add-in COM object.
///
/// {A3B9E8D1-4F2C-4A6E-B8D7-1234567890AB}
pub const SPAMBAYES_CLSID: GUID = GUID::from_u128(
    0xA3B9E8D1_4F2C_4A6E_B8D7_1234567890AB,
);

/// String representation of the CLSID (without braces) for registry paths.
pub const SPAMBAYES_CLSID_STR: &str = "A3B9E8D1-4F2C-4A6E-B8D7-1234567890AB";

/// IID for `IClassFactory`: {00000001-0000-0000-C000-000000000046}
const IID_ICLASS_FACTORY: GUID = GUID::from_u128(
    0x00000001_0000_0000_C000_000000000046,
);

/// IID for `IUnknown`: {00000000-0000-0000-C000-000000000046}
const IID_IUNKNOWN: GUID = GUID::from_u128(
    0x00000000_0000_0000_C000_000000000046,
);

/// `CLASS_E_CLASSNOTAVAILABLE` (0x80040111)
const CLASS_E_CLASSNOTAVAILABLE: HRESULT = HRESULT(0x80040111_u32 as i32);

/// `E_NOINTERFACE` (0x80004002)
const E_NOINTERFACE: HRESULT = HRESULT(0x80004002_u32 as i32);

/// `E_POINTER` (0x80004003)
const E_POINTER: HRESULT = HRESULT(0x80004003_u32 as i32);

/// `S_OK` (0x00000000)
const S_OK: HRESULT = HRESULT(0);

/// `S_FALSE` (0x00000001)
const S_FALSE: HRESULT = HRESULT(1);

// ─── Global Lock Count ───────────────────────────────────────────────────────

/// Global lock count tracking active COM object references.
///
/// When this count is 0, `DllCanUnloadNow` returns `S_OK` indicating the
/// DLL can be safely unloaded. Other modules (`ClassFactory`, `AddinCore`)
/// increment/decrement this count as COM objects are created/destroyed.
///
/// **Validates: Requirement 19.7**
static GLOBAL_LOCK_COUNT: AtomicU32 = AtomicU32::new(0);

/// Increment the global DLL lock count.
///
/// Called when a new COM object is created or a server lock is acquired.
pub fn dll_add_ref() {
    GLOBAL_LOCK_COUNT.fetch_add(1, Ordering::SeqCst);
}

/// Decrement the global DLL lock count.
///
/// Called when a COM object is released or a server lock is released.
pub fn dll_release() {
    GLOBAL_LOCK_COUNT.fetch_sub(1, Ordering::SeqCst);
}

/// Returns the current global lock count (for testing/diagnostics).
pub fn dll_lock_count() -> u32 {
    GLOBAL_LOCK_COUNT.load(Ordering::SeqCst)
}

// ─── AppError ────────────────────────────────────────────────────────────────

/// Unified error type for the `SpamBayes` add-in.
///
/// Provides automatic `From` conversions for all sub-system error types,
/// allowing idiomatic `?` propagation throughout the add-in code.
///
/// **Validates: Requirement 21.1**
#[derive(Debug, Error)]
pub enum AppError {
    /// An error originating from the Bayesian classifier.
    #[error("classifier error: {0}")]
    Classifier(#[from] ClassifierError),

    /// An error originating from the storage/database layer.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// An error originating from the configuration system.
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    /// An error originating from the MAPI message store layer.
    #[error("MAPI error: {0}")]
    Mapi(#[from] MsgStoreError),
}

// ─── LogLevel ────────────────────────────────────────────────────────────────

/// Configurable verbosity levels for add-in logging.
///
/// Higher levels include all lower levels (e.g., `Verbose` includes
/// both `Info` and `Error` messages).
///
/// **Validates: Requirement 17.3**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    /// Only error messages are logged.
    Error = 0,
    /// Informational messages and errors are logged.
    Info = 1,
    /// All messages including detailed diagnostics are logged.
    Verbose = 2,
}

// ─── DLL Entry Points ────────────────────────────────────────────────────────

/// COM class factory entry point.
///
/// Called by the COM runtime to obtain a class factory for the requested
/// CLSID. Validates the CLSID against the `SpamBayes` add-in CLSID and
/// validates that the requested interface is `IClassFactory` or `IUnknown`.
///
/// Returns `CLASS_E_CLASSNOTAVAILABLE` for unknown CLSIDs and
/// `E_NOINTERFACE` if the requested interface is not supported.
///
/// # Safety
///
/// This function is called by the COM runtime. The caller must ensure
/// that all pointers are valid and that `ppv` points to writable memory.
///
/// **Validates: Requirements 19.5, 19.6**
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    // Early debug trace
    let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&debug_path)
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "DllGetClassObject: CALLED")
        });

    // Validate output pointer.
    if ppv.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();

    // Validate input pointers.
    if rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }

    let clsid = &*rclsid;
    let iid = &*riid;

    // Validate CLSID — only serve our own class.
    if *clsid != SPAMBAYES_CLSID {
        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "DllGetClassObject: wrong CLSID") });
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    // Validate requested interface — must be IClassFactory or IUnknown.
    if *iid != IID_ICLASS_FACTORY && *iid != IID_IUNKNOWN {
        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "DllGetClassObject: wrong IID") });
        return E_NOINTERFACE;
    }

    // Create a ClassFactory instance. ClassFactory::new() returns with
    // ref_count = 1 (one reference for the caller).
    let factory_ptr = class_factory::ClassFactory::new();
    *ppv = factory_ptr;

    let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
        .and_then(|mut f| { use std::io::Write; writeln!(f, "DllGetClassObject: SUCCESS, factory={:?}", factory_ptr) });

    S_OK
}

/// Registers the COM server in the Windows registry.
///
/// Delegates to the [`registry::register_server`] function which writes
/// CLSID, `InprocServer32`, `ProgID`, and Outlook Addins entries.
///
/// # Safety
///
/// This function modifies the Windows registry. It must be called with
/// appropriate privileges (typically elevated/admin for HKCR writes).
///
/// **Validates: Requirements 1.2, 19.4**
#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    registry::register_server()
}

/// Removes the COM server registration from the Windows registry.
///
/// Delegates to the [`registry::unregister_server`] function which removes
/// all entries created by `DllRegisterServer`.
///
/// # Safety
///
/// This function modifies the Windows registry. It must be called with
/// appropriate privileges.
///
/// **Validates: Requirements 19.4, 19.7**
#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    registry::unregister_server()
}

/// Indicates whether the DLL can be safely unloaded from memory.
///
/// Returns `S_OK` when no COM objects are held by clients (lock count is 0),
/// and `S_FALSE` when objects are still active.
///
/// # Safety
///
/// Called by the COM runtime. The implementation is thread-safe via atomic
/// operations on the global lock count.
///
/// **Validates: Requirement 19.7**
#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if GLOBAL_LOCK_COUNT.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}
