//! COM `ClassFactory` implementation for the `SpamBayes` add-in.
//!
//! Implements `IClassFactory` (and `IUnknown`) using a raw vtable approach,
//! which is the standard pattern for Rust-based COM servers.
//!
//! **Validates: Requirement 19.5**

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{GUID, HRESULT};
use windows::Win32::Foundation::BOOL;

use crate::{
    dll_add_ref, dll_release, E_NOINTERFACE, E_POINTER, IID_ICLASS_FACTORY, IID_IUNKNOWN, S_OK,
};

// ─── VTable Compatibility Types ──────────────────────────────────────────────

/// Minimal vtable layout for calling `QueryInterface` on any COM object.
/// This only needs the first function pointer (QI) for our use in `CreateInstance`.
#[repr(C)]
struct IUnknownVtblCompat {
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

// ─── VTable Definitions ──────────────────────────────────────────────────────

/// `IClassFactory` vtable layout (extends `IUnknown`) matching the COM binary standard.
#[repr(C)]
struct IClassFactoryVtbl {
    // IUnknown methods
    query_interface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    // IClassFactory methods
    create_instance: unsafe extern "system" fn(
        this: *mut c_void,
        p_unk_outer: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT,
    lock_server: unsafe extern "system" fn(this: *mut c_void, f_lock: BOOL) -> HRESULT,
}

/// Static vtable instance for `ClassFactory`. This is shared by all instances.
static CLASS_FACTORY_VTBL: IClassFactoryVtbl = IClassFactoryVtbl {
    query_interface: ClassFactory::query_interface,
    add_ref: ClassFactory::add_ref,
    release: ClassFactory::release,
    create_instance: ClassFactory::create_instance,
    lock_server: ClassFactory::lock_server,
};

// ─── ClassFactory ────────────────────────────────────────────────────────────

/// COM `ClassFactory` that creates instances of the `SpamBayes` add-in.
///
/// This struct uses the standard COM raw vtable pattern:
/// - First field is a pointer to the vtable
/// - Reference count is managed atomically
/// - The struct is heap-allocated via `Box` and manually freed on Release
///
/// **Validates: Requirement 19.5**
#[repr(C)]
pub struct ClassFactory {
    vtbl: *const IClassFactoryVtbl,
    ref_count: AtomicU32,
}

impl ClassFactory {
    /// Creates a new `ClassFactory` instance on the heap.
    ///
    /// Returns a raw pointer suitable for use as a COM interface pointer.
    /// The initial reference count is 1. The caller owns this reference
    /// and must call Release when done.
    #[must_use]
    pub fn new() -> *mut c_void {
        let factory = Box::new(ClassFactory {
            vtbl: &raw const CLASS_FACTORY_VTBL,
            ref_count: AtomicU32::new(1),
        });
        // Increment the global DLL lock count for this COM object.
        dll_add_ref();
        Box::into_raw(factory).cast::<c_void>()
    }

    // ─── IUnknown ────────────────────────────────────────────────────────

    /// `IUnknown::QueryInterface` implementation.
    ///
    /// Supports `IID_IUnknown` and `IID_IClassFactory`, returning the same
    /// pointer (since `IClassFactory` extends `IUnknown` at offset 0).
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
        if *iid == IID_IUNKNOWN || *iid == IID_ICLASS_FACTORY {
            *ppv = this;
            Self::add_ref(this);
            S_OK
        } else {
            E_NOINTERFACE
        }
    }

    /// `IUnknown::AddRef` implementation.
    ///
    /// Atomically increments the reference count and returns the new value.
    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        let factory = &*(this as *const ClassFactory);
        factory.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// `IUnknown::Release` implementation.
    ///
    /// Atomically decrements the reference count. When it reaches 0,
    /// the `ClassFactory` is deallocated and the global DLL lock is released.
    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let factory = &*(this as *const ClassFactory);
        let prev = factory.ref_count.fetch_sub(1, Ordering::SeqCst);
        let new_count = prev - 1;

        if new_count == 0 {
            // Reclaim the Box allocation.
            let _ = Box::from_raw(this.cast::<ClassFactory>());
            // Decrement global lock count since this object is gone.
            dll_release();
        }

        new_count
    }

    // ─── IClassFactory ───────────────────────────────────────────────────

    /// `IClassFactory::CreateInstance` implementation.
    ///
    /// Creates an instance of the `SpamBayes` add-in (`AddinCore`). Aggregation
    /// is not supported — if `p_unk_outer` is non-null, returns
    /// `CLASS_E_NOAGGREGATION`.
    ///
    /// **Validates: Requirement 19.5**
    unsafe extern "system" fn create_instance(
        _this: *mut c_void,
        p_unk_outer: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        if ppv.is_null() {
            return E_POINTER;
        }
        *ppv = std::ptr::null_mut();

        // Aggregation is not supported.
        if !p_unk_outer.is_null() {
            // CLASS_E_NOAGGREGATION (0x80040110)
            return HRESULT(0x80040110_u32 as i32);
        }

        if riid.is_null() {
            return E_POINTER;
        }

        // Create an AddinCore instance and QueryInterface for the requested IID.
        let addin_ptr = crate::addin_core::AddinCore::new();

        // QueryInterface on the new object for the requested interface.
        // AddinCore::new() returns with ref_count=1. QI will AddRef on success.
        // We then Release the initial creation reference, leaving the caller
        // with exactly one reference via *ppv.
        let hr = Self::query_interface_on(addin_ptr, riid, ppv);

        // Always release the initial creation reference.
        // If QI succeeded, the caller has a ref via *ppv (QI called AddRef).
        // If QI failed, this will drop ref_count to 0 and deallocate.
        Self::release_on(addin_ptr);

        hr
    }

    /// Helper: call `QueryInterface` on a COM object via its vtable.
    unsafe fn query_interface_on(
        obj: *mut c_void,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        let vtbl_ptr = *(obj as *const *const IUnknownVtblCompat);
        ((*vtbl_ptr).query_interface)(obj, riid, ppv)
    }

    /// Helper: call Release on a COM object via its vtable.
    unsafe fn release_on(obj: *mut c_void) -> u32 {
        let vtbl_ptr = *(obj as *const *const IUnknownVtblCompat);
        ((*vtbl_ptr).release)(obj)
    }

    /// `IClassFactory::LockServer` implementation.
    ///
    /// When `f_lock` is TRUE, increments the global DLL lock count
    /// (preventing DLL unload). When FALSE, decrements the count.
    ///
    /// **Validates: Requirement 19.5**
    unsafe extern "system" fn lock_server(_this: *mut c_void, f_lock: BOOL) -> HRESULT {
        if f_lock.as_bool() {
            dll_add_ref();
        } else {
            dll_release();
        }
        S_OK
    }
}
