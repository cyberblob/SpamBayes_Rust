//! IRibbonExtensibility implementation for the SpamBayes add-in.
//!
//! This module provides a separate COM object that implements the
//! IRibbonExtensibility interface. It must be a separate object because
//! IRibbonExtensibility and IDTExtensibility2 both inherit from IDispatch
//! and have conflicting vtable layouts (slot 7 is GetCustomUI vs OnConnection).
//!
//! The AddinCore's QueryInterface returns this object when asked for
//! IRibbonExtensibility.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT, PCWSTR};

use crate::{dll_add_ref, dll_release, E_NOINTERFACE, E_POINTER, S_OK};

// ─── Constants ───────────────────────────────────────────────────────────────

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_C000_000000000046);
const IID_IDISPATCH: GUID = GUID::from_u128(0x00020400_0000_0000_C000_000000000046);
const IID_RIBBON_EXTENSIBILITY: GUID = GUID::from_u128(0x000C0396_0000_0000_C000_000000000046);

/// DISP_E_UNKNOWNNAME
const DISP_E_UNKNOWNNAME: HRESULT = HRESULT(0x80020006_u32 as i32);

// ─── VTable ──────────────────────────────────────────────────────────────────

/// IRibbonExtensibility vtable: IUnknown(3) + IDispatch(4) + GetCustomUI(1) = 8 slots
#[repr(C)]
struct IRibbonExtensibilityVtbl {
    // IUnknown
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // IDispatch
    get_type_info_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    get_type_info: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    get_ids_of_names: unsafe extern "system" fn(*mut c_void, *const GUID, *mut PCWSTR, u32, u32, *mut i32) -> HRESULT,
    invoke: unsafe extern "system" fn(*mut c_void, i32, *const GUID, u32, u16, *mut c_void, *mut c_void, *mut c_void, *mut u32) -> HRESULT,
    // IRibbonExtensibility
    get_custom_ui: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut u16) -> HRESULT,
}

static RIBBON_VTBL: IRibbonExtensibilityVtbl = IRibbonExtensibilityVtbl {
    query_interface: RibbonExtensibility::query_interface,
    add_ref: RibbonExtensibility::add_ref,
    release: RibbonExtensibility::release,
    get_type_info_count: RibbonExtensibility::get_type_info_count,
    get_type_info: RibbonExtensibility::get_type_info,
    get_ids_of_names: RibbonExtensibility::get_ids_of_names,
    invoke: RibbonExtensibility::invoke,
    get_custom_ui: RibbonExtensibility::get_custom_ui,
};

// ─── RibbonExtensibility Object ──────────────────────────────────────────────

/// Separate COM object implementing IRibbonExtensibility.
#[repr(C)]
pub struct RibbonExtensibility {
    vtbl: *const IRibbonExtensibilityVtbl,
    ref_count: AtomicU32,
    /// Back-pointer to the parent AddinCore (raw, non-ref-counted).
    addin: *mut c_void,
}

unsafe impl Send for RibbonExtensibility {}

impl RibbonExtensibility {
    /// Create a new RibbonExtensibility object on the heap.
    /// `addin_ptr` is the AddinCore raw pointer (for dispatching callbacks).
    pub fn new(addin_ptr: *mut c_void) -> *mut c_void {
        let obj = Box::new(RibbonExtensibility {
            vtbl: &raw const RIBBON_VTBL,
            ref_count: AtomicU32::new(1),
            addin: addin_ptr,
        });
        dll_add_ref();
        Box::into_raw(obj).cast::<c_void>()
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void,
    ) -> HRESULT {
        if ppv.is_null() { return E_POINTER; }
        *ppv = std::ptr::null_mut();
        if riid.is_null() { return E_POINTER; }
        let iid = &*riid;
        if *iid == IID_IUNKNOWN || *iid == IID_IDISPATCH || *iid == IID_RIBBON_EXTENSIBILITY {
            *ppv = this;
            Self::add_ref(this);
            S_OK
        } else {
            E_NOINTERFACE
        }
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        let obj = &*(this as *const RibbonExtensibility);
        obj.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let obj = &*(this as *const RibbonExtensibility);
        let prev = obj.ref_count.fetch_sub(1, Ordering::SeqCst);
        let new_count = prev - 1;
        if new_count == 0 {
            let _ = Box::from_raw(this.cast::<RibbonExtensibility>());
            dll_release();
        }
        new_count
    }

    unsafe extern "system" fn get_type_info_count(_this: *mut c_void, pctinfo: *mut u32) -> HRESULT {
        if !pctinfo.is_null() { *pctinfo = 0; }
        S_OK
    }

    unsafe extern "system" fn get_type_info(
        _this: *mut c_void, _: u32, _: u32, _: *mut *mut c_void,
    ) -> HRESULT {
        HRESULT(0x80004001_u32 as i32) // E_NOTIMPL
    }

    unsafe extern "system" fn get_ids_of_names(
        _this: *mut c_void, _riid: *const GUID, rgsz_names: *mut PCWSTR,
        c_names: u32, _lcid: u32, rg_disp_id: *mut i32,
    ) -> HRESULT {
        if rgsz_names.is_null() || rg_disp_id.is_null() || c_names == 0 {
            return E_POINTER;
        }
        let name_ptr = (*rgsz_names).0;
        if name_ptr.is_null() { return DISP_E_UNKNOWNNAME; }
        let mut len = 0usize;
        while *name_ptr.add(len) != 0 { len += 1; }
        let name = String::from_utf16_lossy(std::slice::from_raw_parts(name_ptr, len));

        let dispid = match name.as_str() {
            "wireCall" | "wireCall2" => 1, // OnLoad uses wireCall internally
            "GetCustomUI" => 2,
            "OnSpamClick" => 101,
            "OnNotSpamClick" => 102,
            "OnManagerClick" => 103,
            "Ribbon_OnLoad" => 104,
            "GetSpamEnabled" => 105,
            "GetNotSpamEnabled" => 106,
            "GetNotSpamVisible" => 107,
            "GetSpamVisible" => 109,
            "LoadImage" => 108,
            "OnShowCluesClick" => 110,
            "GetShowCluesEnabled" => 111,
            _ => {
                *rg_disp_id = -1;
                return DISP_E_UNKNOWNNAME;
            }
        };
        *rg_disp_id = dispid;
        S_OK
    }

    unsafe extern "system" fn invoke(
        _this: *mut c_void, disp_id: i32, _riid: *const GUID, _lcid: u32,
        _flags: u16, _params: *mut c_void, p_result: *mut c_void,
        _excep: *mut c_void, _arg_err: *mut u32,
    ) -> HRESULT {
        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "Ribbon.Invoke: dispid={}", disp_id) });

        match disp_id {
            101 => {
                // OnSpamClick — train selected message(s) as spam
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnSpamClick!") });
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::addin_core::AddinCore::train_selected_as_spam();
                }));
                S_OK
            }
            102 => {
                // OnNotSpamClick — train selected message(s) as ham
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnNotSpamClick!") });
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::addin_core::AddinCore::train_selected_as_ham();
                }));
                S_OK
            }
            103 => {
                // OnManagerClick
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnManagerClick!") });
                crate::addin_core::AddinCore::launch_manager();
                S_OK
            }
            104 => {
                // Ribbon_OnLoad - store the IRibbonUI reference for later Invalidate calls
                if !_params.is_null() {
                    #[repr(C)]
                    struct DISPPARAMS {
                        rgvarg: *mut c_void,
                        rgdispid_named: *mut i32,
                        c_args: u32,
                        c_named_args: u32,
                    }
                    let dp = &*(_params as *const DISPPARAMS);
                    if dp.c_args > 0 && !dp.rgvarg.is_null() {
                        let var_ptr = dp.rgvarg as *const u8;
                        let vt = *(var_ptr as *const u16);
                        if vt == 9 || vt == 13 { // VT_DISPATCH or VT_UNKNOWN
                            let ribbon_ptr = *(var_ptr.add(8) as *const *mut c_void);
                            if !ribbon_ptr.is_null() {
                                // AddRef on the ribbon UI
                                let vtbl_ptr = *(ribbon_ptr as *const *const usize);
                                let addref_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                                    std::mem::transmute(*vtbl_ptr.add(1));
                                addref_fn(ribbon_ptr);
                                crate::addin_core::GLOBAL_RIBBON_UI = ribbon_ptr;
                                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
                                    .and_then(|mut f| { use std::io::Write; writeln!(f, "Ribbon_OnLoad: stored IRibbonUI={:?}", ribbon_ptr) });
                            }
                        }
                    }
                }
                S_OK
            }
            105 | 106 => {
                // GetSpamEnabled / GetNotSpamEnabled -> return True
                if !p_result.is_null() {
                    let var_ptr = p_result as *mut u8;
                    *(var_ptr as *mut u16) = 11; // VT_BOOL
                    *(var_ptr.add(2) as *mut u16) = 0;
                    *(var_ptr.add(4) as *mut u16) = 0;
                    *(var_ptr.add(6) as *mut u16) = 0;
                    *(var_ptr.add(8) as *mut i16) = -1; // VARIANT_TRUE
                }
                S_OK
            }
            107 => {
                // GetNotSpamVisible -> check if in spam/unsure folder
                let visible = crate::addin_core::AddinCore::is_in_spam_or_unsure_folder();
                if !p_result.is_null() {
                    let var_ptr = p_result as *mut u8;
                    *(var_ptr as *mut u16) = 11; // VT_BOOL
                    *(var_ptr.add(2) as *mut u16) = 0;
                    *(var_ptr.add(4) as *mut u16) = 0;
                    *(var_ptr.add(6) as *mut u16) = 0;
                    *(var_ptr.add(8) as *mut i16) = if visible { -1 } else { 0 };
                }
                S_OK
            }
            109 => {
                // GetSpamVisible -> visible when NOT in spam folder
                let in_spam = crate::addin_core::AddinCore::is_in_spam_folder_only();
                if !p_result.is_null() {
                    let var_ptr = p_result as *mut u8;
                    *(var_ptr as *mut u16) = 11; // VT_BOOL
                    *(var_ptr.add(2) as *mut u16) = 0;
                    *(var_ptr.add(4) as *mut u16) = 0;
                    *(var_ptr.add(6) as *mut u16) = 0;
                    *(var_ptr.add(8) as *mut i16) = if !in_spam { -1 } else { 0 };
                }
                S_OK
            }
            108 => {
                // LoadImage callback: receives image ID string, returns IPictureDisp
                let image_id = Self::extract_first_bstr_param(_params);
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: LoadImage called with id={:?}", image_id) });

                if let Some(id) = image_id {
                    let filename = match id.as_str() {
                        "delete_as_spam" => "delete_as_spam.bmp",
                        "recover_ham" => "recover_ham.bmp",
                        _ => {
                            let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                                .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: LoadImage unknown id: {}", id) });
                            return S_OK;
                        }
                    };

                    // Find the images directory relative to the DLL location
                    let image_path = Self::find_image_path(filename);
                    let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: LoadImage path={:?}", image_path) });

                    if let Some(path) = image_path {
                        let picture = Self::load_picture_from_bmp(&path);
                        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                            .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: LoadImage picture={:?}", picture.map(|p| p as usize)) });

                        if let Some(pdisp) = picture {
                            // Return as VT_DISPATCH (vt=9) in the VARIANT result
                            if !p_result.is_null() {
                                let var_ptr = p_result as *mut u8;
                                // Clear the VARIANT first
                                std::ptr::write_bytes(var_ptr, 0, 16);
                                // VT_DISPATCH = 9
                                *(var_ptr as *mut u16) = 9;
                                // punkVal/pdispVal is at offset 8 in VARIANT
                                *(var_ptr.add(8) as *mut *mut c_void) = pdisp;
                            }
                        }
                    }
                }
                S_OK
            }
            110 => {
                // OnShowCluesClick — score the selected message and show clues
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnShowCluesClick!") });
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::addin_core::AddinCore::show_clues_for_selected();
                }));
                if let Err(e) = result {
                    let msg = if let Some(s) = e.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = e.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnShowCluesClick PANIC: {}", msg) });
                }
                S_OK
            }
            111 => {
                // GetShowCluesEnabled — enabled when classifier is loaded
                let enabled = crate::addin_core::AddinCore::is_classifier_loaded();
                if !p_result.is_null() {
                    let var_ptr = p_result as *mut u8;
                    *(var_ptr as *mut u16) = 11; // VT_BOOL
                    *(var_ptr.add(2) as *mut u16) = 0;
                    *(var_ptr.add(4) as *mut u16) = 0;
                    *(var_ptr.add(6) as *mut u16) = 0;
                    *(var_ptr.add(8) as *mut i16) = if enabled { -1 } else { 0 };
                }
                S_OK
            }
            _ => S_OK
        }
    }

    /// Extract the first BSTR parameter from DISPPARAMS.
    ///
    /// DISPPARAMS layout:
    ///   - rgvarg: *mut VARIANT (array of args, in reverse order)
    ///   - rgdispidNamedArgs: *mut i32
    ///   - cArgs: u32
    ///   - cNamedArgs: u32
    ///
    /// For loadImage callback, the first (and only) arg is the image ID string (VT_BSTR=8).
    unsafe fn extract_first_bstr_param(params: *mut c_void) -> Option<String> {
        if params.is_null() { return None; }

        #[repr(C)]
        struct DISPPARAMS {
            rgvarg: *mut c_void,       // VARIANT array
            rgdispid_named: *mut i32,
            c_args: u32,
            c_named_args: u32,
        }

        let dp = &*(params as *const DISPPARAMS);
        if dp.c_args == 0 || dp.rgvarg.is_null() { return None; }

        // Args are in reverse order; for a single arg, it's at index 0
        let variant_ptr = dp.rgvarg as *const u8;
        let vt = *(variant_ptr as *const u16);

        if vt == 8 {
            // VT_BSTR: BSTR pointer is at offset 8
            let bstr_ptr = *(variant_ptr.add(8) as *const *const u16);
            if bstr_ptr.is_null() { return None; }
            // BSTR: length prefix at bstr_ptr - 2 (in bytes), string starts at bstr_ptr
            let mut len = 0usize;
            while *bstr_ptr.add(len) != 0 { len += 1; }
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(bstr_ptr, len)))
        } else {
            None
        }
    }

    /// Find the path to an image file.
    ///
    /// Search order:
    /// 1. Next to the DLL: {dll_dir}\..\images\{filename}
    /// 2. Install directory: {dll_dir}\..\..\images\{filename}  
    /// 3. Data directory: %LOCALAPPDATA%\SpamBayes\images\{filename}
    fn find_image_path(filename: &str) -> Option<std::path::PathBuf> {
        use std::path::{Path, PathBuf};

        // Get DLL directory
        let dll_dir = Self::get_dll_directory();

        if let Some(ref dir) = dll_dir {
            // Pattern: {install_dir}\x64\spambayes_addin.dll -> {install_dir}\images\{filename}
            let parent = Path::new(dir).parent()?;
            let candidate = parent.join("images").join(filename);
            if candidate.exists() {
                return Some(candidate);
            }
            // Also check next to the DLL directly
            let candidate = PathBuf::from(dir).join("images").join(filename);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // Fallback: %LOCALAPPDATA%\SpamBayes\images\{filename}
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let candidate = PathBuf::from(local_app_data).join("SpamBayes").join("images").join(filename);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        None
    }

    /// Get the directory containing this DLL.
    fn get_dll_directory() -> Option<String> {
        use windows::Win32::Foundation::MAX_PATH;
        use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

        let hmodule = crate::get_dll_module();
        let mut buffer = vec![0u16; MAX_PATH as usize];
        let len = unsafe { GetModuleFileNameW(hmodule, &mut buffer) } as usize;
        if len == 0 { return None; }
        let path = String::from_utf16_lossy(&buffer[..len]);
        // Return the directory portion
        path.rfind('\\').map(|pos| path[..pos].to_string())
    }

    /// Load a BMP file and return an IPictureDisp pointer.
    ///
    /// Uses OleLoadPicturePath to load the bitmap as an IPictureDisp COM object.
    unsafe fn load_picture_from_bmp(path: &std::path::Path) -> Option<*mut c_void> {
        extern "system" {
            fn OleLoadPicturePath(
                sz_url_or_path: *const u16,
                punk_caller: *mut c_void,
                dw_reserved: u32,
                clr_reserved: u32,
                riid: *const GUID,
                ppv_ret: *mut *mut c_void,
            ) -> HRESULT;
        }

        // IID_IDispatch — OleLoadPicturePath returns IPictureDisp through IDispatch QI
        let iid_idispatch = GUID::from_u128(0x00020400_0000_0000_C000_000000000046);

        // Convert path to wide string (OleLoadPicturePath expects a file path or URL)
        let path_str = path.to_string_lossy();
        let wide_path: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();

        let mut ppic: *mut c_void = std::ptr::null_mut();
        let hr = OleLoadPicturePath(
            wide_path.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            &iid_idispatch,
            &mut ppic,
        );

        if hr.0 == 0 && !ppic.is_null() {
            Some(ppic)
        } else {
            None
        }
    }

    /// GetCustomUI vtable method (slot 7).
    /// Called by Outlook to retrieve the ribbon XML.
    unsafe extern "system" fn get_custom_ui(
        _this: *mut c_void,
        _ribbon_id: *const u16,
        ribbon_xml: *mut *mut u16,
    ) -> HRESULT {
        if ribbon_xml.is_null() {
            return E_POINTER;
        }

        let xml = crate::addin_core::AddinCore::get_ribbon_xml();

        extern "system" { fn SysAllocString(psz: *const u16) -> *mut u16; }
        let wide: Vec<u16> = xml.encode_utf16().chain(std::iter::once(0)).collect();
        *ribbon_xml = SysAllocString(wide.as_ptr());

        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "GetCustomUI: returned {} bytes of XML", xml.len()) });

        S_OK
    }
}
