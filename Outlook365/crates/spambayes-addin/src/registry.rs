//! COM server registration and unregistration logic.
//!
//! Writes and removes Windows Registry entries required for Outlook to discover
//! and load the `SpamBayes` COM add-in DLL.
//!
//! **Validates: Requirements 1.2, 19.4, 20.5**

use windows::core::HRESULT;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteKeyW, RegSetValueExW, HKEY, HKEY_CLASSES_ROOT,
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
};

use crate::SPAMBAYES_CLSID_STR;

/// The `ProgID` used for COM registration.
const PROG_ID: &str = "SpamBayes.OutlookAddin";

/// The friendly name displayed in Outlook's add-in manager.
const FRIENDLY_NAME: &str = "SpamBayes";

/// Threading model for the in-process server.
const THREADING_MODEL: &str = "Apartment";

/// LoadBehavior=3 means "load at startup".
const LOAD_BEHAVIOR: u32 = 3;

/// CommandLineSafe=0 means the add-in is not safe for command-line automation.
const COMMAND_LINE_SAFE: u32 = 0;

/// Registry path for the CLSID entry under HKCR.
fn clsid_key_path() -> String {
    format!("CLSID\\{{{SPAMBAYES_CLSID_STR}}}")
}

/// Registry path for the `InprocServer32` entry.
fn inproc_server32_key_path() -> String {
    format!("CLSID\\{{{SPAMBAYES_CLSID_STR}}}\\InprocServer32")
}

/// Registry path for the `ProgID` entry under the CLSID.
fn clsid_progid_key_path() -> String {
    format!("CLSID\\{{{SPAMBAYES_CLSID_STR}}}\\ProgID")
}

/// Registry path for the Outlook Addins entry.
const OUTLOOK_ADDINS_SUBKEY: &str =
    "Software\\Microsoft\\Office\\Outlook\\Addins\\SpamBayes.OutlookAddin";

/// Helper to encode a Rust string as a null-terminated wide (UTF-16) vector.
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Helper to set a `REG_SZ` value on an open registry key.
unsafe fn set_string_value(key: HKEY, value_name: &str, data: &str) -> bool {
    let name_wide = to_wide_null(value_name);
    let data_wide = to_wide_null(data);
    // REG_SZ data size is in bytes, including the null terminator.
    let data_bytes = std::slice::from_raw_parts(
        data_wide.as_ptr().cast::<u8>(),
        data_wide.len() * 2,
    );
    let result = RegSetValueExW(
        key,
        windows::core::PCWSTR(name_wide.as_ptr()),
        0,
        REG_SZ,
        Some(data_bytes),
    );
    result == ERROR_SUCCESS
}

/// Helper to set a `REG_DWORD` value on an open registry key.
unsafe fn set_dword_value(key: HKEY, value_name: &str, data: u32) -> bool {
    let name_wide = to_wide_null(value_name);
    let data_bytes = data.to_le_bytes();
    let result = RegSetValueExW(
        key,
        windows::core::PCWSTR(name_wide.as_ptr()),
        0,
        REG_DWORD,
        Some(&data_bytes),
    );
    result == ERROR_SUCCESS
}

/// Helper to create or open a registry key for writing.
unsafe fn create_key(root: HKEY, subkey: &str) -> Option<HKEY> {
    let subkey_wide = to_wide_null(subkey);
    let mut hkey = HKEY::default();
    let result = RegCreateKeyW(
        root,
        windows::core::PCWSTR(subkey_wide.as_ptr()),
        &raw mut hkey,
    );
    if result == ERROR_SUCCESS {
        Some(hkey)
    } else {
        None
    }
}

/// Helper to delete a registry key (non-recursive, leaf keys).
unsafe fn delete_key(root: HKEY, subkey: &str) -> bool {
    let subkey_wide = to_wide_null(subkey);
    let result = RegDeleteKeyW(root, windows::core::PCWSTR(subkey_wide.as_ptr()));
    result == ERROR_SUCCESS
}

/// Gets the path to this DLL.
///
/// Uses `GetModuleFileNameW` with the DLL's stored module handle to determine
/// the full filesystem path to the loaded DLL.
fn get_dll_path() -> Option<String> {
    use windows::Win32::Foundation::MAX_PATH;
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

    let hmodule = crate::get_dll_module();
    let mut buffer = vec![0u16; MAX_PATH as usize];
    let len = unsafe { GetModuleFileNameW(hmodule, &mut buffer) } as usize;
    if len == 0 {
        return None;
    }
    buffer.truncate(len);
    Some(String::from_utf16_lossy(&buffer))
}

/// Registers the `SpamBayes` COM server in the Windows Registry.
///
/// Creates the following registry entries:
/// - `HKCR\CLSID\{<CLSID>}` — default value = `ProgID`
/// - `HKCR\CLSID\{<CLSID>}\InprocServer32` — default = DLL path, `ThreadingModel` = "Apartment"
/// - `HKCR\CLSID\{<CLSID>}\ProgID` — default = "SpamBayes.OutlookAddin"
/// - `HKCU\Software\Microsoft\Office\Outlook\Addins\SpamBayes.OutlookAddin` —
///   `FriendlyName`, LoadBehavior=3, CommandLineSafe=0
///
/// # Safety
///
/// Modifies the Windows registry. Caller must have appropriate privileges.
///
/// **Validates: Requirements 1.2, 19.4**
pub unsafe fn register_server() -> HRESULT {
    let Some(dll_path) = get_dll_path() else {
        return HRESULT(-1); // E_FAIL
    };

    // 1. Create HKCR\CLSID\{CLSID} with default = description
    let clsid_path = clsid_key_path();
    let Some(hkey_clsid) = create_key(HKEY_CLASSES_ROOT, &clsid_path) else {
        return HRESULT(-1);
    };
    set_string_value(hkey_clsid, "", "SpamBayes Outlook Addin");
    let _ = RegCloseKey(hkey_clsid);

    // 2. Create HKCR\CLSID\{CLSID}\InprocServer32
    let inproc_path = inproc_server32_key_path();
    let Some(hkey_inproc) = create_key(HKEY_CLASSES_ROOT, &inproc_path) else {
        return HRESULT(-1);
    };
    set_string_value(hkey_inproc, "", &dll_path);
    set_string_value(hkey_inproc, "ThreadingModel", THREADING_MODEL);
    let _ = RegCloseKey(hkey_inproc);

    // 3. Create HKCR\CLSID\{CLSID}\ProgID
    let progid_path = clsid_progid_key_path();
    let Some(hkey_progid) = create_key(HKEY_CLASSES_ROOT, &progid_path) else {
        return HRESULT(-1);
    };
    set_string_value(hkey_progid, "", PROG_ID);
    let _ = RegCloseKey(hkey_progid);

    // 4. Create HKCR\SpamBayes.OutlookAddin (ProgID -> CLSID reverse mapping)
    let Some(hkey_progid_root) = create_key(HKEY_CLASSES_ROOT, PROG_ID) else {
        return HRESULT(-1);
    };
    set_string_value(hkey_progid_root, "", "SpamBayes Outlook Addin");
    let _ = RegCloseKey(hkey_progid_root);

    let progid_clsid_path = format!("{PROG_ID}\\CLSID");
    let Some(hkey_progid_clsid) = create_key(HKEY_CLASSES_ROOT, &progid_clsid_path) else {
        return HRESULT(-1);
    };
    let clsid_with_braces = format!("{{{SPAMBAYES_CLSID_STR}}}");
    set_string_value(hkey_progid_clsid, "", &clsid_with_braces);
    let _ = RegCloseKey(hkey_progid_clsid);

    // 5. Create HKCU\Software\Microsoft\Office\Outlook\Addins\SpamBayes.OutlookAddin
    let Some(hkey_addin) = create_key(HKEY_CURRENT_USER, OUTLOOK_ADDINS_SUBKEY) else {
        return HRESULT(-1);
    };
    set_string_value(hkey_addin, "FriendlyName", FRIENDLY_NAME);
    set_string_value(hkey_addin, "Description", "SpamBayes anti-spam tool");
    set_dword_value(hkey_addin, "LoadBehavior", LOAD_BEHAVIOR);
    set_dword_value(hkey_addin, "CommandLineSafe", COMMAND_LINE_SAFE);
    let _ = RegCloseKey(hkey_addin);

    HRESULT(0) // S_OK
}

/// Removes all `SpamBayes` COM server registry entries.
///
/// Deletes the entries created by [`register_server`] in reverse order
/// (children before parents).
///
/// # Safety
///
/// Modifies the Windows registry. Caller must have appropriate privileges.
///
/// **Validates: Requirements 19.4, 19.7**
pub unsafe fn unregister_server() -> HRESULT {
    // Remove Outlook Addins entry
    delete_key(HKEY_CURRENT_USER, OUTLOOK_ADDINS_SUBKEY);

    // Remove ProgID reverse-lookup (children first)
    let progid_clsid_path = format!("{PROG_ID}\\CLSID");
    delete_key(HKEY_CLASSES_ROOT, &progid_clsid_path);
    delete_key(HKEY_CLASSES_ROOT, PROG_ID);

    // Remove CLSID sub-keys (children first)
    let progid_path = clsid_progid_key_path();
    delete_key(HKEY_CLASSES_ROOT, &progid_path);

    let inproc_path = inproc_server32_key_path();
    delete_key(HKEY_CLASSES_ROOT, &inproc_path);

    let clsid_path = clsid_key_path();
    delete_key(HKEY_CLASSES_ROOT, &clsid_path);

    HRESULT(0) // S_OK
}
