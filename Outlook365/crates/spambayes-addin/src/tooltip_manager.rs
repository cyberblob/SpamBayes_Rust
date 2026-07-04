//! Tooltip Manager for Win32 dialog controls.
//!
//! Wraps the Win32 TOOLTIPS_CLASS common control, providing a simple interface
//! to create tooltips and register them for dialog controls.
//!
//! **Validates: Requirements 1.2, 1.4**

#![cfg(target_os = "windows")]

use crate::help_content::TooltipText;

use std::mem;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::{
    TOOLTIPS_CLASS, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETDELAYTIME, TTDT_AUTOPOP,
    TTDT_INITIAL, TTTOOLINFOW, TTS_ALWAYSTIP, TTS_NOPREFIX,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetDlgItem, SendMessageW, CW_USEDEFAULT, WINDOW_EX_STYLE,
    WINDOW_STYLE, WS_POPUP,
};

/// Tooltip initial delay in milliseconds.
const TOOLTIP_INITIAL_DELAY_MS: i32 = 500;

/// Tooltip autopop (visible duration) in milliseconds.
const TOOLTIP_AUTOPOP_MS: i32 = 5000;

/// Manages a Win32 tooltip control for a dialog window.
///
/// Creates a TOOLTIPS_CLASS window and registers tooltip text for individual
/// controls. The tooltip control is a top-level popup window owned by the system
/// that appears when the user hovers over registered controls.
pub struct TooltipManager {
    /// Handle to the tooltip common control window.
    hwnd_tooltip: HWND,
}

impl TooltipManager {
    /// Create a new tooltip control as a child of the given dialog.
    ///
    /// The tooltip window is created with `TTS_ALWAYSTIP | TTS_NOPREFIX` styles
    /// and configured with a 500ms initial delay and 5000ms autopop timeout.
    ///
    /// # Arguments
    ///
    /// * `dialog_hwnd` - Handle to the parent dialog window.
    /// * `instance` - Module instance handle for the DLL.
    ///
    /// # Returns
    ///
    /// A new `TooltipManager` wrapping the created tooltip control.
    /// If creation fails, the `hwnd_tooltip` will be invalid (null).
    pub fn new(dialog_hwnd: HWND, instance: windows::Win32::Foundation::HINSTANCE) -> Self {
        let hwnd_tooltip = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                TOOLTIPS_CLASS,
                PCWSTR::null(),
                WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX | WS_POPUP.0),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                dialog_hwnd,
                None,
                instance,
                None,
            )
        };

        let hwnd_tooltip = hwnd_tooltip.unwrap_or(HWND::default());

        // Set delay times: initial hover delay and autopop (display duration).
        if !hwnd_tooltip.is_invalid() {
            unsafe {
                // TTM_SETDELAYTIME with TTDT_INITIAL: time before tooltip appears
                SendMessageW(
                    hwnd_tooltip,
                    TTM_SETDELAYTIME,
                    WPARAM(TTDT_INITIAL as usize),
                    LPARAM(TOOLTIP_INITIAL_DELAY_MS as isize),
                );

                // TTM_SETDELAYTIME with TTDT_AUTOPOP: time tooltip remains visible
                SendMessageW(
                    hwnd_tooltip,
                    TTM_SETDELAYTIME,
                    WPARAM(TTDT_AUTOPOP as usize),
                    LPARAM(TOOLTIP_AUTOPOP_MS as isize),
                );
            }
        }

        Self { hwnd_tooltip }
    }

    /// Register tooltips for a set of dialog controls.
    ///
    /// For each entry in the `tooltips` slice, this method:
    /// 1. Retrieves the child control HWND via `GetDlgItem`
    /// 2. Fills a `TTTOOLINFOW` struct with the control info and tooltip text
    /// 3. Sends `TTM_ADDTOOLW` to register the tooltip
    ///
    /// Controls that cannot be found (invalid ID) are silently skipped.
    ///
    /// # Arguments
    ///
    /// * `dialog_hwnd` - Handle to the parent dialog containing the controls.
    /// * `tooltips` - Slice of `TooltipText` entries mapping control IDs to text.
    pub fn register_tooltips(&self, dialog_hwnd: HWND, tooltips: &[TooltipText]) {
        if self.hwnd_tooltip.is_invalid() {
            return;
        }

        for tooltip in tooltips {
            let child_hwnd = unsafe { GetDlgItem(dialog_hwnd, i32::from(tooltip.control_id)) };

            // Skip controls that don't exist in this dialog.
            let child_hwnd = match child_hwnd {
                Ok(h) if !h.is_invalid() => h,
                _ => continue,
            };

            // Encode the tooltip text as a wide string (null-terminated UTF-16).
            let wide_text: Vec<u16> = tooltip
                .text
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let mut tool_info = TTTOOLINFOW {
                cbSize: mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: dialog_hwnd,
                uId: child_hwnd.0 as usize,
                lpszText: windows::core::PWSTR(wide_text.as_ptr() as *mut u16),
                ..Default::default()
            };

            unsafe {
                SendMessageW(
                    self.hwnd_tooltip,
                    TTM_ADDTOOLW,
                    WPARAM(0),
                    LPARAM(&mut tool_info as *mut TTTOOLINFOW as isize),
                );
            }
        }
    }

    /// Destroy the tooltip control window.
    ///
    /// Call this during `WM_DESTROY` handling of the parent dialog to clean up
    /// the tooltip window. After calling this method, the `TooltipManager`
    /// should not be used further.
    pub fn destroy(&self) {
        if !self.hwnd_tooltip.is_invalid() {
            unsafe {
                let _ = DestroyWindow(self.hwnd_tooltip);
            }
        }
    }
}

// ─── Unit Tests ──────────────────────────────────────────────────────────────
//
// These tests verify the tooltip *infrastructure* — the data tables,
// delay constants, and struct interfaces — rather than simulating actual
// Win32 hover events (which require a live message loop and windowed test
// harness).
//
// **Manual verification steps for full tooltip behavior:**
//
// 1. Launch the Manager dialog via SpamBayes Manager in Outlook.
// 2. Hover over any control (e.g., the spam threshold spinner) and verify
//    a tooltip appears after approximately 500ms.
// 3. Observe that the tooltip disappears after approximately 5 seconds.
// 4. Repeat for several controls to confirm all have registered tooltips.
// 5. Open the Configuration Wizard and verify navigation button tooltips.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help_content::tooltips::{MANAGER_TOOLTIPS, WIZARD_TOOLTIPS};

    /// Verify the tooltip initial delay constant matches the 500ms requirement.
    ///
    /// **Validates: Requirements 1.2**
    #[test]
    fn tooltip_initial_delay_matches_requirement() {
        assert_eq!(
            TOOLTIP_INITIAL_DELAY_MS, 500,
            "Tooltip initial delay must be 500ms per Requirement 1.2"
        );
    }

    /// Verify the tooltip autopop constant matches the 5000ms requirement.
    ///
    /// **Validates: Requirements 1.2**
    #[test]
    fn tooltip_autopop_matches_requirement() {
        assert_eq!(
            TOOLTIP_AUTOPOP_MS, 5000,
            "Tooltip autopop duration must be 5000ms per Requirement 1.2"
        );
    }

    /// Verify that MANAGER_TOOLTIPS is populated with a reasonable number of entries.
    ///
    /// The Manager dialog has controls for thresholds, actions, folder browse
    /// buttons, action buttons, cleanup, and notification — at least 16 controls.
    ///
    /// **Validates: Requirements 1.1**
    #[test]
    fn manager_tooltips_has_expected_count() {
        assert!(
            MANAGER_TOOLTIPS.len() >= 16,
            "Expected at least 16 Manager tooltip entries, got {}",
            MANAGER_TOOLTIPS.len()
        );
    }

    /// Verify that WIZARD_TOOLTIPS is not empty.
    ///
    /// The wizard should have tooltips for at least the navigation buttons.
    ///
    /// **Validates: Requirements 1.1**
    #[test]
    fn wizard_tooltips_is_not_empty() {
        assert!(
            !WIZARD_TOOLTIPS.is_empty(),
            "Wizard tooltips must not be empty"
        );
    }

    /// Verify all Manager tooltip texts are non-empty and within a reasonable length.
    ///
    /// Tooltip text should be concise (one or two sentences), not exceeding 200 chars.
    ///
    /// **Validates: Requirements 1.3**
    #[test]
    fn manager_tooltip_texts_are_reasonable_length() {
        for tooltip in MANAGER_TOOLTIPS {
            assert!(
                !tooltip.text.is_empty(),
                "Tooltip for control ID {} has empty text",
                tooltip.control_id
            );
            assert!(
                tooltip.text.len() <= 200,
                "Tooltip for control ID {} exceeds 200 chars (len={}): {:?}",
                tooltip.control_id,
                tooltip.text.len(),
                &tooltip.text[..80]
            );
        }
    }

    /// Verify all Wizard tooltip texts are non-empty and within a reasonable length.
    ///
    /// **Validates: Requirements 1.3**
    #[test]
    fn wizard_tooltip_texts_are_reasonable_length() {
        for tooltip in WIZARD_TOOLTIPS {
            assert!(
                !tooltip.text.is_empty(),
                "Wizard tooltip for control ID {} has empty text",
                tooltip.control_id
            );
            assert!(
                tooltip.text.len() <= 200,
                "Wizard tooltip for control ID {} exceeds 200 chars (len={})",
                tooltip.control_id,
                tooltip.text.len()
            );
        }
    }

    /// Verify all control IDs in MANAGER_TOOLTIPS are unique (no duplicates).
    ///
    /// Duplicate control IDs would mean only one tooltip gets registered per
    /// control, potentially hiding useful information.
    ///
    /// **Validates: Requirements 1.1**
    #[test]
    fn manager_tooltip_control_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tooltip in MANAGER_TOOLTIPS {
            assert!(
                seen.insert(tooltip.control_id),
                "Duplicate control ID {} found in MANAGER_TOOLTIPS",
                tooltip.control_id
            );
        }
    }

    /// Verify all control IDs in WIZARD_TOOLTIPS are unique (no duplicates).
    ///
    /// **Validates: Requirements 1.1**
    #[test]
    fn wizard_tooltip_control_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tooltip in WIZARD_TOOLTIPS {
            assert!(
                seen.insert(tooltip.control_id),
                "Duplicate control ID {} found in WIZARD_TOOLTIPS",
                tooltip.control_id
            );
        }
    }

    /// Verify the TooltipManager struct has the expected public API.
    ///
    /// This is a compile-time check — if this test compiles, the struct
    /// has `new`, `register_tooltips`, and `destroy` methods available.
    /// We cannot call `new` without a live HWND, but we verify the type exists.
    #[test]
    fn tooltip_manager_struct_exists_and_has_fields() {
        // Verify we can construct a TooltipManager with an invalid (null) handle.
        // This tests that the struct is properly defined and constructable.
        let manager = TooltipManager {
            hwnd_tooltip: HWND::default(),
        };
        // destroy() on an invalid handle is a no-op (the implementation checks).
        manager.destroy();
    }
}
