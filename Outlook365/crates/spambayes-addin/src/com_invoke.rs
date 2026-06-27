//! Low-level COM `IDispatch` invocation helpers.
//!
//! Provides functions to call methods and get/set properties on COM objects
//! via `IDispatch::GetIDsOfNames` + `IDispatch::Invoke`. This is the Rust
//! equivalent of Python's `win32com.client.Dispatch` late-binding mechanism.
//!
//! These helpers are used by the toolbar setup code to interact with the
//! Outlook Object Model (Application, Explorer, CommandBars, etc.).

use std::ffi::c_void;
use std::ptr;

use windows::core::{GUID, HRESULT, PCWSTR};

// ─── IDispatch VTable Layout ─────────────────────────────────────────────────

/// Minimal IDispatch vtable for raw invocation.
#[repr(C)]
pub struct IDispatchVtbl {
    // IUnknown (3 slots)
    pub query_interface: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
    // IDispatch (4 slots)
    pub get_type_info_count:
        unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    pub get_type_info: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u32,
        *mut *mut c_void,
    ) -> HRESULT,
    pub get_ids_of_names: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        *mut PCWSTR,
        u32,
        u32,
        *mut i32,
    ) -> HRESULT,
    pub invoke: unsafe extern "system" fn(
        *mut c_void,
        i32,         // dispIdMember
        *const GUID, // riid
        u32,         // lcid
        u16,         // wFlags
        *mut DISPPARAMS,
        *mut VARIANT,
        *mut c_void, // pExcepInfo
        *mut u32,    // puArgErr
    ) -> HRESULT,
}

// ─── VARIANT / DISPPARAMS ────────────────────────────────────────────────────

/// Simplified VARIANT structure for COM invocation.
/// Layout must match the Windows VARIANT struct (16 bytes on 32-bit, 24 on 64-bit).
#[repr(C)]
#[derive(Clone)]
pub struct VARIANT {
    pub vt: u16,
    _reserved1: u16,
    _reserved2: u16,
    _reserved3: u16,
    pub data: [u8; 16], // Union data (largest member: 8 bytes + padding)
}

impl Default for VARIANT {
    fn default() -> Self {
        Self {
            vt: 0, // VT_EMPTY
            _reserved1: 0,
            _reserved2: 0,
            _reserved3: 0,
            data: [0u8; 16],
        }
    }
}

/// DISPPARAMS structure for IDispatch::Invoke.
#[repr(C)]
pub struct DISPPARAMS {
    pub rgvarg: *mut VARIANT,
    pub rgdispid_named_args: *mut i32,
    pub c_args: u32,
    pub c_named_args: u32,
}

// ─── VARIANT Type Constants ──────────────────────────────────────────────────

const VT_EMPTY: u16 = 0;
const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_BOOL: u16 = 11;

// ─── IDispatch Invoke Flags ──────────────────────────────────────────────────

const DISPATCH_METHOD: u16 = 1;
const DISPATCH_PROPERTYGET: u16 = 2;
const DISPATCH_PROPERTYPUT: u16 = 4;

/// Named arg DISPID for property put (DISPID_PROPERTYPUT = -3).
const DISPID_PROPERTYPUT: i32 = -3;

/// IID_NULL for IDispatch calls.
pub static IID_NULL: GUID = GUID::from_u128(0);

// ─── VariantArg ──────────────────────────────────────────────────────────────

/// High-level argument type for COM invocations.
#[derive(Debug, Clone)]
pub enum VariantArg<'a> {
    /// 32-bit integer (VT_I4).
    I4(i32),
    /// Boolean (VT_BOOL). COM VARIANT_BOOL: -1 = TRUE, 0 = FALSE.
    Bool(bool),
    /// Wide string (VT_BSTR).
    BStr(&'a str),
}

impl VariantArg<'_> {
    /// Convert to a raw VARIANT. Caller must free any BSTR allocations.
    unsafe fn to_variant(&self) -> VARIANT {
        let mut v = VARIANT::default();
        match self {
            VariantArg::I4(val) => {
                v.vt = VT_I4;
                let ptr = v.data.as_mut_ptr().cast::<i32>();
                *ptr = *val;
            }
            VariantArg::Bool(val) => {
                v.vt = VT_BOOL;
                let ptr = v.data.as_mut_ptr().cast::<i16>();
                // VARIANT_BOOL: TRUE = -1 (0xFFFF), FALSE = 0
                *ptr = if *val { -1 } else { 0 };
            }
            VariantArg::BStr(s) => {
                v.vt = VT_BSTR;
                let bstr = sys_alloc_string(s);
                let ptr = v.data.as_mut_ptr().cast::<*mut u16>();
                *ptr = bstr;
            }
        }
        v
    }
}

// ─── SysAllocString / SysFreeString ─────────────────────────────────────────

extern "system" {
    fn SysAllocString(psz: *const u16) -> *mut u16;
    fn SysFreeString(bstr_string: *mut u16);
}

/// Allocate a COM BSTR from a Rust string.
unsafe fn sys_alloc_string(s: &str) -> *mut u16 {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    SysAllocString(wide.as_ptr())
}

/// Free a VARIANT's BSTR data if it's a string.
unsafe fn variant_clear(v: &mut VARIANT) {
    if v.vt == VT_BSTR {
        let bstr_ptr = *(v.data.as_ptr().cast::<*mut u16>());
        if !bstr_ptr.is_null() {
            SysFreeString(bstr_ptr);
        }
    }
    // For VT_DISPATCH we do NOT release — caller owns the reference.
    v.vt = VT_EMPTY;
}

// ─── GetIDsOfNames Helper ────────────────────────────────────────────────────

/// Resolve a property/method name to a DISPID on an IDispatch object.
pub unsafe fn get_dispid(disp: *mut c_void, name: &str) -> Result<i32, HRESULT> {
    if disp.is_null() {
        return Err(HRESULT(0x80004003_u32 as i32)); // E_POINTER
    }

    let vtbl = *(disp as *const *const IDispatchVtbl);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut name_ptr = PCWSTR(wide.as_ptr());
    let mut dispid: i32 = 0;

    let hr = ((*vtbl).get_ids_of_names)(
        disp,
        &IID_NULL,
        &raw mut name_ptr,
        1,
        0, // LOCALE_SYSTEM_DEFAULT
        &raw mut dispid,
    );

    if hr.is_ok() {
        Ok(dispid)
    } else {
        Err(hr)
    }
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Get a property value from a COM object via IDispatch.
///
/// Returns the IDispatch pointer if the property is an object, or null
/// if the property is not a dispatch type.
///
/// # Safety
///
/// `disp` must be a valid IDispatch COM pointer.
pub unsafe fn dispatch_get(disp: *mut c_void, name: &str) -> Result<*mut c_void, HRESULT> {
    let dispid = get_dispid(disp, name)?;
    let vtbl = *(disp as *const *const IDispatchVtbl);

    let mut params = DISPPARAMS {
        rgvarg: ptr::null_mut(),
        rgdispid_named_args: ptr::null_mut(),
        c_args: 0,
        c_named_args: 0,
    };

    let mut result = VARIANT::default();

    let hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &IID_NULL,
        0,
        DISPATCH_PROPERTYGET | DISPATCH_METHOD,
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    if hr.is_err() {
        return Err(hr);
    }

    // Extract the dispatch pointer from the result VARIANT
    if result.vt == VT_DISPATCH {
        let obj_ptr = *(result.data.as_ptr().cast::<*mut c_void>());
        Ok(obj_ptr)
    } else {
        // Property returned a non-dispatch value — return null
        variant_clear(&mut result);
        Ok(ptr::null_mut())
    }
}

/// Set a property value on a COM object via IDispatch.
///
/// # Safety
///
/// `disp` must be a valid IDispatch COM pointer.
pub unsafe fn dispatch_put(
    disp: *mut c_void,
    name: &str,
    value: VariantArg<'_>,
) -> Result<(), HRESULT> {
    let dispid = get_dispid(disp, name)?;
    let vtbl = *(disp as *const *const IDispatchVtbl);

    let mut arg = value.to_variant();
    let mut named_arg = DISPID_PROPERTYPUT;

    let mut params = DISPPARAMS {
        rgvarg: &raw mut arg,
        rgdispid_named_args: &raw mut named_arg,
        c_args: 1,
        c_named_args: 1,
    };

    let hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &IID_NULL,
        0,
        DISPATCH_PROPERTYPUT,
        &raw mut params,
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null_mut(),
    );

    variant_clear(&mut arg);

    if hr.is_ok() {
        Ok(())
    } else {
        Err(hr)
    }
}

/// Invoke a method on a COM object via IDispatch.
///
/// Returns the resulting IDispatch pointer (for methods that return objects),
/// or null if the method returns a non-dispatch value or void.
///
/// # Safety
///
/// `disp` must be a valid IDispatch COM pointer.
pub unsafe fn dispatch_invoke_method(
    disp: *mut c_void,
    name: &str,
    args: &[VariantArg<'_>],
) -> Result<*mut c_void, HRESULT> {
    let dispid = get_dispid(disp, name)?;
    let vtbl = *(disp as *const *const IDispatchVtbl);

    // COM args are passed in reverse order
    let mut variants: Vec<VARIANT> = args.iter().rev().map(|a| a.to_variant()).collect();

    let mut params = DISPPARAMS {
        rgvarg: if variants.is_empty() {
            ptr::null_mut()
        } else {
            variants.as_mut_ptr()
        },
        rgdispid_named_args: ptr::null_mut(),
        c_args: variants.len() as u32,
        c_named_args: 0,
    };

    let mut result = VARIANT::default();

    let hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &IID_NULL,
        0,
        DISPATCH_METHOD,
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    // Clean up BSTR arguments
    for v in &mut variants {
        variant_clear(v);
    }

    if hr.is_err() {
        return Err(hr);
    }

    // Extract dispatch pointer from result
    if result.vt == VT_DISPATCH {
        let obj_ptr = *(result.data.as_ptr().cast::<*mut c_void>());
        Ok(obj_ptr)
    } else {
        variant_clear(&mut result);
        Ok(ptr::null_mut())
    }
}


/// Get a string property value from a COM object via IDispatch.
///
/// Returns `Some(String)` if the property is a BSTR, `None` otherwise.
///
/// # Safety
///
/// `disp` must be a valid IDispatch COM pointer.
pub unsafe fn dispatch_get_string(disp: *mut c_void, name: &str) -> Option<String> {
    let dispid = get_dispid(disp, name).ok()?;
    let vtbl = *(disp as *const *const IDispatchVtbl);

    let mut params = DISPPARAMS {
        rgvarg: ptr::null_mut(),
        rgdispid_named_args: ptr::null_mut(),
        c_args: 0,
        c_named_args: 0,
    };

    let mut result = VARIANT::default();

    let hr = ((*vtbl).invoke)(
        disp,
        dispid,
        &IID_NULL,
        0,
        DISPATCH_PROPERTYGET | DISPATCH_METHOD,
        &raw mut params,
        &raw mut result,
        ptr::null_mut(),
        ptr::null_mut(),
    );

    if hr.is_err() {
        return None;
    }

    if result.vt == VT_BSTR {
        let bstr_ptr = *(result.data.as_ptr().cast::<*const u16>());
        if bstr_ptr.is_null() {
            return Some(String::new());
        }
        // Read the BSTR length (stored 4 bytes before the pointer)
        let len_ptr = (bstr_ptr as *const u8).sub(4) as *const u32;
        let byte_len = *len_ptr as usize;
        let char_len = byte_len / 2;
        let slice = std::slice::from_raw_parts(bstr_ptr, char_len);
        let s = String::from_utf16_lossy(slice);
        SysFreeString(bstr_ptr as *mut u16);
        Some(s)
    } else {
        variant_clear(&mut result);
        None
    }
}


// ─── Connection Point Sink for Button Events ─────────────────────────────────

/// IID for IConnectionPointContainer: {B196B284-BAB4-101A-B69C-00AA00341D07}
const IID_ICONNECTION_POINT_CONTAINER: GUID = GUID::from_u128(
    0xB196B284_BAB4_101A_B69C_00AA00341D07,
);

/// IID for _CommandBarButtonEvents: {000C033E-0000-0000-C000-000000000046}
const IID_COMMANDBAR_BUTTON_EVENTS: GUID = GUID::from_u128(
    0x000C033E_0000_0000_C000_000000000046,
);

/// IConnectionPointContainer vtable (extends IUnknown)
#[repr(C)]
struct IConnectionPointContainerVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    enum_connection_points: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    find_connection_point: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
}

/// IConnectionPoint vtable (extends IUnknown)
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

/// Sink for _CommandBarButtonEvents. When Outlook fires Click, our Invoke is called.
#[repr(C)]
struct ButtonEventSink {
    vtbl: *const IDispatchVtbl,
    ref_count: std::sync::atomic::AtomicU32,
    callback: fn(),
}

static BUTTON_SINK_VTBL: IDispatchVtbl = IDispatchVtbl {
    query_interface: button_sink_qi,
    add_ref: button_sink_add_ref,
    release: button_sink_release,
    get_type_info_count: button_sink_get_type_info_count,
    get_type_info: button_sink_get_type_info,
    get_ids_of_names: button_sink_get_ids_of_names,
    invoke: button_sink_invoke,
};

unsafe extern "system" fn button_sink_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() { return HRESULT(0x80004003_u32 as i32); }
    let iid = &*riid;
    let iid_iunknown = GUID::from_u128(0x00000000_0000_0000_C000_000000000046);
    let iid_idispatch = GUID::from_u128(0x00020400_0000_0000_C000_000000000046);
    if *iid == iid_iunknown || *iid == iid_idispatch || *iid == IID_COMMANDBAR_BUTTON_EVENTS {
        *ppv = this;
        button_sink_add_ref(this);
        HRESULT(0)
    } else {
        *ppv = ptr::null_mut();
        HRESULT(0x80004002_u32 as i32) // E_NOINTERFACE
    }
}

unsafe extern "system" fn button_sink_add_ref(this: *mut c_void) -> u32 {
    let sink = &*(this as *const ButtonEventSink);
    sink.ref_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

unsafe extern "system" fn button_sink_release(this: *mut c_void) -> u32 {
    let sink = &*(this as *const ButtonEventSink);
    let prev = sink.ref_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    let new_count = prev - 1;
    if new_count == 0 {
        let _ = Box::from_raw(this as *mut ButtonEventSink);
    }
    new_count
}

unsafe extern "system" fn button_sink_get_type_info_count(_this: *mut c_void, pctinfo: *mut u32) -> HRESULT {
    if !pctinfo.is_null() { *pctinfo = 0; }
    HRESULT(0)
}

unsafe extern "system" fn button_sink_get_type_info(_this: *mut c_void, _: u32, _: u32, _: *mut *mut c_void) -> HRESULT {
    HRESULT(0x80004001_u32 as i32) // E_NOTIMPL
}

unsafe extern "system" fn button_sink_get_ids_of_names(_this: *mut c_void, _: *const GUID, _: *mut PCWSTR, _: u32, _: u32, _: *mut i32) -> HRESULT {
    HRESULT(0x80020006_u32 as i32) // DISP_E_UNKNOWNNAME
}

unsafe extern "system" fn button_sink_invoke(
    this: *mut c_void,
    disp_id: i32,
    _riid: *const GUID,
    _lcid: u32,
    _flags: u16,
    _params: *mut DISPPARAMS,
    _result: *mut VARIANT,
    _excep: *mut c_void,
    _arg_err: *mut u32,
) -> HRESULT {
    // DISPID 1 = Click event for _CommandBarButtonEvents
    if disp_id == 1 {
        let sink = &*(this as *const ButtonEventSink);
        (sink.callback)();
    }
    HRESULT(0) // S_OK
}

/// Connect a button event sink to a CommandBarButton.
/// Returns the advise cookie (needed to disconnect later).
///
/// # Safety
/// `button_ptr` must be a valid IDispatch pointer to a CommandBarButton.
pub unsafe fn advise_button_click(button_ptr: *mut c_void, callback: fn()) -> Option<u32> {
    if button_ptr.is_null() { return None; }

    // QI for IConnectionPointContainer
    let vtbl = *(button_ptr as *const *const [usize; 3]);
    let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
        std::mem::transmute((*vtbl)[0]);
    let mut cpc_ptr: *mut c_void = ptr::null_mut();
    let hr = qi(button_ptr, &IID_ICONNECTION_POINT_CONTAINER, &raw mut cpc_ptr);
    if hr.0 != 0 || cpc_ptr.is_null() { return None; }

    // FindConnectionPoint for _CommandBarButtonEvents
    let cpc_vtbl = *(cpc_ptr as *const *const IConnectionPointContainerVtbl);
    let mut cp_ptr: *mut c_void = ptr::null_mut();
    let hr = ((*cpc_vtbl).find_connection_point)(cpc_ptr, &IID_COMMANDBAR_BUTTON_EVENTS, &raw mut cp_ptr);
    // Release CPC
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute((*cpc_vtbl).release);
    release(cpc_ptr);
    if hr.0 != 0 || cp_ptr.is_null() { return None; }

    // Create our sink
    let sink = Box::new(ButtonEventSink {
        vtbl: &raw const BUTTON_SINK_VTBL,
        ref_count: std::sync::atomic::AtomicU32::new(1),
        callback,
    });
    let sink_ptr = Box::into_raw(sink) as *mut c_void;

    // Advise
    let cp_vtbl = *(cp_ptr as *const *const IConnectionPointVtbl);
    let mut cookie: u32 = 0;
    let hr = ((*cp_vtbl).advise)(cp_ptr, sink_ptr, &raw mut cookie);
    // Release CP
    let cp_release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute((*cp_vtbl).release);
    cp_release(cp_ptr);

    if hr.0 != 0 { 
        // Advise failed - release our sink
        button_sink_release(sink_ptr);
        return None; 
    }

    Some(cookie)
}
