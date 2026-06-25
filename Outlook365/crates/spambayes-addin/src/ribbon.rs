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
                // OnSpamClick
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnSpamClick!") });
                S_OK
            }
            102 => {
                // OnNotSpamClick  
                let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
                    .and_then(|mut f| { use std::io::Write; writeln!(f, "RIBBON: OnNotSpamClick!") });
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
                // Ribbon_OnLoad
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
                // GetNotSpamVisible -> return True
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
            _ => S_OK
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
