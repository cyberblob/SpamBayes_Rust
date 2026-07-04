//! Context-sensitive help dialog for SpamBayes.
//!
//! Displays detailed help content in a modeless dialog window. The dialog is
//! a singleton — if already open, it is brought to the front and its content
//! is updated rather than creating a new instance.
//!
//! **Validates: Requirements 3.1, 3.2, 3.3, 3.4**

#![cfg(target_os = "windows")]

use std::sync::Mutex;

use crate::help_content::HelpEntry;
use crate::help_dialog_template::{
    self, IDC_HELP_CLOSE, IDC_HELP_DESCRIPTION, IDC_HELP_TIPS, IDC_HELP_TITLE,
};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_QUALITY,
    FF_DONTCARE, FW_BOLD, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, DestroyWindow, GetDlgItem, IsWindow, SendMessageW,
    SetDlgItemTextW, SetForegroundWindow, ShowWindow, DLGTEMPLATE, SW_SHOW, WM_CLOSE,
    WM_COMMAND, WM_INITDIALOG, WM_SETFONT,
};

// ─── Singleton State ─────────────────────────────────────────────────────────

/// Tracks the singleton help dialog window handle and bold font handle.
///
/// When the help dialog is open, `hwnd` holds its window handle and `hfont`
/// holds the bold font used for the title label. Both are cleared when the
/// dialog is destroyed.
struct HelpDialogState {
    hwnd: HWND,
    hfont: HFONT,
}

// SAFETY: The help dialog is only accessed from the STA (UI) thread. HWND and
// HFONT are raw pointers that don't implement Send, but COM single-threaded
// apartment guarantees all UI operations happen on one thread.
unsafe impl Send for HelpDialogState {}

impl Default for HelpDialogState {
    fn default() -> Self {
        Self {
            hwnd: HWND::default(),
            hfont: HFONT::default(),
        }
    }
}

/// Global singleton state for the help dialog.
///
/// Protected by a Mutex since COM operations happen on the STA thread,
/// but this provides correctness even if help is triggered from different
/// code paths.
static HELP_DIALOG_STATE: Mutex<Option<HelpDialogState>> = Mutex::new(None);

// ─── Public API ──────────────────────────────────────────────────────────────

/// Display the help dialog for the given `HelpEntry`.
///
/// If the help dialog is already open, it is brought to the front and its
/// content is updated to reflect the new entry. Otherwise, a new modeless
/// dialog is created.
///
/// # Arguments
///
/// * `parent` - Handle to the parent dialog window.
/// * `entry` - The help content entry to display.
///
/// # Safety
///
/// This function calls Win32 APIs that require valid HWND handles.
pub fn show_help(parent: HWND, entry: &'static HelpEntry) {
    unsafe {
        let mut state = HELP_DIALOG_STATE.lock().unwrap_or_else(|e| e.into_inner());

        // Check if the help dialog is already open and still valid.
        if let Some(ref s) = *state {
            if IsWindow(s.hwnd).as_bool() {
                // Dialog exists — update content and bring to front.
                update_help_content(s.hwnd, entry);
                let _ = SetForegroundWindow(s.hwnd);
                return;
            }
        }

        // Create the in-memory dialog template.
        let template = help_dialog_template::create_help_dialog_template();
        let template_ptr = template.as_ptr() as *const DLGTEMPLATE;

        // Store entry pointer as LPARAM so the dialog proc can retrieve it.
        let entry_ptr = entry as *const HelpEntry as isize;

        // Create modeless dialog using the in-memory template.
        let hwnd = CreateDialogIndirectParamW(
            None,
            template_ptr,
            parent,
            Some(help_dlg_proc),
            LPARAM(entry_ptr),
        );

        let hwnd = hwnd.unwrap_or(HWND::default());

        if !hwnd.is_invalid() && hwnd != HWND::default() {
            let _ = ShowWindow(hwnd, SW_SHOW);

            // Create bold font for the title.
            let hfont = create_bold_font();

            // Apply bold font to title control.
            if !hfont.is_invalid() {
                if let Ok(title_ctrl) =
                    GetDlgItem(hwnd, i32::from(IDC_HELP_TITLE))
                {
                    SendMessageW(
                        title_ctrl,
                        WM_SETFONT,
                        WPARAM(hfont.0 as usize),
                        LPARAM(1), // redraw = TRUE
                    );
                }
            }

            // Populate content.
            update_help_content(hwnd, entry);

            // Store state.
            *state = Some(HelpDialogState { hwnd, hfont });
        }
    }
}

/// Display help for a specific dialog control by looking up its section.
///
/// Maps the control ID to the appropriate `HelpEntry` and calls `show_help`.
/// If no mapping exists for the control, this is a no-op.
///
/// # Arguments
///
/// * `parent` - Handle to the parent dialog window.
/// * `control_id` - The Win32 control identifier (IDC_*) that has focus.
pub fn show_help_for_control(parent: HWND, control_id: u16) {
    if let Some(entry) = help_section_for_control(control_id) {
        show_help(parent, entry);
    }
}

// ─── Dialog Procedure ────────────────────────────────────────────────────────

/// Window procedure for the help dialog.
///
/// Handles:
/// - `WM_INITDIALOG`: Returns TRUE to accept default focus.
/// - `WM_COMMAND` with `IDC_HELP_CLOSE` or `IDCANCEL`: Destroys the dialog.
/// - `WM_CLOSE`: Destroys the dialog.
///
/// On destruction, the global singleton state is cleared and the bold font
/// is freed.
///
/// # Safety
///
/// This is a Win32 DLGPROC callback. The system guarantees valid parameters.
unsafe extern "system" fn help_dlg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    _lparam: LPARAM,
) -> isize {
    const IDCANCEL: u16 = 2;

    match msg {
        WM_INITDIALOG => {
            // Return TRUE (1) to accept default keyboard focus.
            1
        }
        WM_COMMAND => {
            let control_id = (wparam.0 & 0xFFFF) as u16;
            if control_id == IDC_HELP_CLOSE || control_id == IDCANCEL {
                destroy_help_dialog(hwnd);
            }
            0
        }
        WM_CLOSE => {
            destroy_help_dialog(hwnd);
            0
        }
        _ => 0,
    }
}

// ─── Internal Helpers ────────────────────────────────────────────────────────

/// Destroy the help dialog and clean up singleton state.
///
/// Destroys the window, deletes the bold font object, and clears the
/// global `HELP_DIALOG_STATE`.
unsafe fn destroy_help_dialog(hwnd: HWND) {
    let _ = DestroyWindow(hwnd);

    let mut state = HELP_DIALOG_STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = state.take() {
        if !s.hfont.is_invalid() {
            let _ = DeleteObject(s.hfont);
        }
    }
}

/// Update the help dialog content controls with the given entry's text.
///
/// Sets the title, description, and tips (if present) using `SetDlgItemTextW`.
unsafe fn update_help_content(hwnd: HWND, entry: &HelpEntry) {
    set_dialog_text(hwnd, IDC_HELP_TITLE, entry.title);
    set_dialog_text(hwnd, IDC_HELP_DESCRIPTION, entry.description);

    let tips_text = entry.tips.unwrap_or("");
    set_dialog_text(hwnd, IDC_HELP_TIPS, tips_text);
}

/// Set the text of a dialog control using `SetDlgItemTextW`.
///
/// Encodes the string as null-terminated UTF-16 and passes it to the Win32 API.
unsafe fn set_dialog_text(hwnd: HWND, control_id: u16, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SetDlgItemTextW(hwnd, i32::from(control_id), PCWSTR::from_raw(wide.as_ptr()));
}

/// Create a bold font for the help dialog title.
///
/// Uses MS Shell Dlg at 10pt bold. Returns an invalid `HFONT` on failure
/// (the caller handles this gracefully by not applying the font).
unsafe fn create_bold_font() -> HFONT {
    let face_name: Vec<u16> = "MS Shell Dlg"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // Font height: negative value indicates point size (device-independent).
    // -13 is approximately 10pt at 96 DPI.
    let hfont = CreateFontW(
        -13,                    // nHeight (negative = point size)
        0,                      // nWidth (default)
        0,                      // nEscapement
        0,                      // nOrientation
        FW_BOLD.0 as i32,       // fnWeight
        0,                      // fdwItalic
        0,                      // fdwUnderline
        0,                      // fdwStrikeOut
        DEFAULT_CHARSET.0.into(), // fdwCharSet
        OUT_DEFAULT_PRECIS.0.into(), // fdwOutputPrecision
        CLIP_DEFAULT_PRECIS.0.into(), // fdwClipPrecision
        DEFAULT_QUALITY.0.into(), // fdwQuality
        FF_DONTCARE.0.into(),   // fdwPitchAndFamily
        PCWSTR::from_raw(face_name.as_ptr()), // lpszFaceName
    );

    hfont
}

/// Map a control ID to the appropriate help section entry.
///
/// Groups of related controls map to the same `HelpEntry` so that pressing
/// F1 on any control in a section shows the section-level help.
///
/// # Control ID Ranges
///
/// - `3001..=3009` — Statistics display (no specific help section)
/// - `3010..=3015` — Filter settings → `FILTER_SETTINGS`
/// - `3020..=3024` — Folder selection → `FOLDER_CONFIG`
/// - `3030..=3034` — Browse buttons → `FOLDER_CONFIG`
/// - `3040` — Train Now → `TRAINING`
/// - `3041` — Filter Now → `FILTER_SETTINGS`
/// - `3050..=3051` — Cleanup controls → `CLEANUP`
/// - `3060..=3061` — Notification controls → `NOTIFICATION`
///
/// Returns `None` for unmapped control IDs (no help available).
pub fn help_section_for_control(control_id: u16) -> Option<&'static HelpEntry> {
    use crate::help_content::sections;

    // Control ID ranges matching the Manager dialog layout.
    // These must align with IDC_* constants in manager_dlg.rs and help_content.rs.
    match control_id {
        // Filter settings: thresholds, actions, enable checkbox
        3010..=3015 => Some(&sections::FILTER_SETTINGS),
        // Folder selection controls and browse buttons → Folder Configuration
        3020..=3024 | 3030..=3034 => Some(&sections::FOLDER_CONFIG),
        // Train Now button → Training
        3040 => Some(&sections::TRAINING),
        // Filter Now button → Filter Settings
        3041 => Some(&sections::FILTER_SETTINGS),
        // Cleanup: enable checkbox, days spinner
        3050..=3051 => Some(&sections::CLEANUP),
        // Notification: sound enable, accumulate timer
        3060..=3061 => Some(&sections::NOTIFICATION),
        // Statistics, OK/Cancel, and unmapped controls — no help available
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help_content::sections;

    #[test]
    fn help_section_mapping_filter_settings() {
        // All filter threshold/action controls should map to FILTER_SETTINGS
        for id in 3010..=3015 {
            let entry = help_section_for_control(id);
            assert!(entry.is_some(), "Control ID {id} should have a help mapping");
            assert_eq!(entry.unwrap().title, sections::FILTER_SETTINGS.title);
        }
    }

    #[test]
    fn help_section_mapping_folder_config() {
        // Folder selection controls (3020..=3024) and browse buttons (3030..=3034)
        for id in 3020..=3024 {
            let entry = help_section_for_control(id);
            assert!(entry.is_some(), "Control ID {id} should have a help mapping");
            assert_eq!(entry.unwrap().title, sections::FOLDER_CONFIG.title);
        }
        for id in 3030..=3034 {
            let entry = help_section_for_control(id);
            assert!(entry.is_some(), "Control ID {id} should have a help mapping");
            assert_eq!(entry.unwrap().title, sections::FOLDER_CONFIG.title);
        }
    }

    #[test]
    fn help_section_mapping_train_now() {
        // Train Now (3040) → Training
        let entry = help_section_for_control(3040);
        assert!(entry.is_some(), "Control ID 3040 should have a help mapping");
        assert_eq!(entry.unwrap().title, sections::TRAINING.title);
    }

    #[test]
    fn help_section_mapping_filter_now() {
        // Filter Now (3041) → Filter Settings
        let entry = help_section_for_control(3041);
        assert!(entry.is_some(), "Control ID 3041 should have a help mapping");
        assert_eq!(entry.unwrap().title, sections::FILTER_SETTINGS.title);
    }

    #[test]
    fn help_section_mapping_cleanup() {
        for id in 3050..=3051 {
            let entry = help_section_for_control(id);
            assert!(entry.is_some(), "Control ID {id} should have a help mapping");
            assert_eq!(entry.unwrap().title, sections::CLEANUP.title);
        }
    }

    #[test]
    fn help_section_mapping_notification() {
        for id in 3060..=3061 {
            let entry = help_section_for_control(id);
            assert!(entry.is_some(), "Control ID {id} should have a help mapping");
            assert_eq!(entry.unwrap().title, sections::NOTIFICATION.title);
        }
    }

    #[test]
    fn help_section_mapping_statistics_returns_none() {
        // Statistics controls (3001..=3009) have no specific help section
        for id in 3001..=3009 {
            assert!(
                help_section_for_control(id).is_none(),
                "Statistics control {id} should return None"
            );
        }
    }

    #[test]
    fn help_section_mapping_unknown_returns_none() {
        // IDs outside known ranges should return None
        assert!(help_section_for_control(0).is_none());
        assert!(help_section_for_control(1000).is_none());
        assert!(help_section_for_control(9999).is_none());
        assert!(help_section_for_control(u16::MAX).is_none());
    }

    #[test]
    fn help_section_mapping_ok_cancel_returns_none() {
        // OK (3042), Cancel (3043), Reset Stats (3044) — no help
        assert!(help_section_for_control(3042).is_none());
        assert!(help_section_for_control(3043).is_none());
        assert!(help_section_for_control(3044).is_none());
    }
}
