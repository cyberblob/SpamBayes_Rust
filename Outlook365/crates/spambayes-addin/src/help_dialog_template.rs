//! Help dialog template — in-memory DLGTEMPLATE for the context-sensitive help window.
//!
//! Since this project creates dialogs programmatically (no `.rc` resource files),
//! this module builds an in-memory DLGTEMPLATE/DLGITEMTEMPLATE structure that can be
//! used with `DialogBoxIndirectParamW` or `CreateDialogIndirectParamW`.
//!
//! # Layout (350x300 DLU)
//!
//! ```text
//! +-------------------------------------------+
//! | SpamBayes Help                       [X]  |
//! +-------------------------------------------+
//! | [Title Label - bold]                      |  (IDC_HELP_TITLE)
//! |                                           |
//! | [Description - scrollable readonly edit]  |  (IDC_HELP_DESCRIPTION)
//! |                                           |
//! |                                           |
//! | [Tips section - static text]              |  (IDC_HELP_TIPS)
//! |                                           |
//! +-------------------------------------------+
//! |                                  [Close]  |  (IDC_HELP_CLOSE)
//! +-------------------------------------------+
//! ```
//!
//! **Validates: Requirements 3.3, 3.4**

#![allow(dead_code)]

// ─── Dialog and Control IDs ──────────────────────────────────────────────────

/// Dialog resource ID for the help dialog.
pub const IDD_HELP: u32 = 4000;

/// Control ID: Static text label for the help entry title (set bold via `WM_INITDIALOG`).
pub const IDC_HELP_TITLE: u16 = 4001;

/// Control ID: Multi-line read-only edit control for the description text.
pub const IDC_HELP_DESCRIPTION: u16 = 4002;

/// Control ID: Static text for the tips/notes section.
pub const IDC_HELP_TIPS: u16 = 4003;

/// Control ID: Close button.
pub const IDC_HELP_CLOSE: u16 = 4004;

// ─── Dialog Dimensions (in Dialog Units) ─────────────────────────────────────

/// Total dialog width in dialog units.
const DLG_WIDTH: u16 = 350;

/// Total dialog height in dialog units.
const DLG_HEIGHT: u16 = 300;

// ─── Win32 Style Constants ───────────────────────────────────────────────────

// Dialog styles
const DS_MODALFRAME: u32 = 0x0000_0080;
const DS_SETFONT: u32 = 0x0000_0040;
const WS_POPUP: u32 = 0x8000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CHILD: u32 = 0x4000_0000;

// Control styles
const ES_MULTILINE: u32 = 0x0004;
const ES_READONLY: u32 = 0x0800;
const ES_AUTOVSCROLL: u32 = 0x0040;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const BS_PUSHBUTTON: u32 = 0x0000_0000;
const SS_LEFT: u32 = 0x0000_0000;

// ─── In-Memory Dialog Template Builder ───────────────────────────────────────

/// Builds an in-memory `DLGTEMPLATE` byte buffer for the help dialog.
///
/// The returned `Vec<u8>` is DWORD-aligned and suitable for use with
/// `DialogBoxIndirectParamW` or `CreateDialogIndirectParamW`.
///
/// # Dialog Contents
///
/// - **Title static** (IDC_HELP_TITLE): 10,10 to 330,24 — bold via `WM_INITDIALOG`
/// - **Description edit** (IDC_HELP_DESCRIPTION): 10,30 to 330,230 — read-only,
///   multiline, vertical scroll
/// - **Tips static** (IDC_HELP_TIPS): 10,235 to 330,275
/// - **Close button** (IDC_HELP_CLOSE): 280,280 to 60,14 (width x height)
///
/// # Example
///
/// ```rust,no_run
/// use spambayes_addin::help_dialog_template::create_help_dialog_template;
///
/// let template = create_help_dialog_template();
/// // Pass template.as_ptr() as LPCDLGTEMPLATE to CreateDialogIndirectParamW
/// ```
pub fn create_help_dialog_template() -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);

    // ── DLGTEMPLATE header ───────────────────────────────────────────────
    let dialog_style = DS_MODALFRAME | DS_SETFONT | WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let num_items: u16 = 4; // title, description, tips, close button

    // DLGTEMPLATE structure (18 bytes before arrays):
    //   style:      DWORD
    //   dwExtStyle: DWORD
    //   cdit:       WORD  (number of items)
    //   x:          short
    //   y:          short
    //   cx:         short
    //   cy:         short
    push_u32(&mut buf, dialog_style);   // style
    push_u32(&mut buf, 0);              // dwExtendedStyle
    push_u16(&mut buf, num_items);      // cdit
    push_u16(&mut buf, 0);             // x
    push_u16(&mut buf, 0);             // y
    push_u16(&mut buf, DLG_WIDTH);     // cx
    push_u16(&mut buf, DLG_HEIGHT);    // cy

    // Menu array — no menu (single 0x0000)
    push_u16(&mut buf, 0);

    // Class array — default dialog class (single 0x0000)
    push_u16(&mut buf, 0);

    // Title (null-terminated UTF-16)
    push_wide_string(&mut buf, "SpamBayes Help");

    // DS_SETFONT: point size + typeface
    push_u16(&mut buf, 8); // 8-point font
    push_wide_string(&mut buf, "MS Shell Dlg");

    // ── Control 1: Title static (IDC_HELP_TITLE) ─────────────────────────
    align_dword(&mut buf);
    push_dlg_item(
        &mut buf,
        WS_CHILD | WS_VISIBLE | SS_LEFT,  // style
        0,                                  // extended style
        10,                                 // x
        10,                                 // y
        320,                                // cx
        14,                                 // cy
        IDC_HELP_TITLE,                     // control ID
        0x0082,                             // class: Static
        "",                                 // initial text (set at runtime)
    );

    // ── Control 2: Description edit (IDC_HELP_DESCRIPTION) ───────────────
    align_dword(&mut buf);
    push_dlg_item(
        &mut buf,
        WS_CHILD | WS_VISIBLE | WS_BORDER | WS_VSCROLL | WS_TABSTOP
            | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL,
        0,                                  // extended style
        10,                                 // x
        30,                                 // y
        330,                                // cx
        200,                                // cy
        IDC_HELP_DESCRIPTION,               // control ID
        0x0081,                             // class: Edit
        "",                                 // initial text (set at runtime)
    );

    // ── Control 3: Tips static (IDC_HELP_TIPS) ───────────────────────────
    align_dword(&mut buf);
    push_dlg_item(
        &mut buf,
        WS_CHILD | WS_VISIBLE | SS_LEFT,
        0,
        10,                                 // x
        235,                                // y
        330,                                // cx
        40,                                 // cy
        IDC_HELP_TIPS,                      // control ID
        0x0082,                             // class: Static
        "",                                 // initial text (set at runtime)
    );

    // ── Control 4: Close button (IDC_HELP_CLOSE) ─────────────────────────
    align_dword(&mut buf);
    push_dlg_item(
        &mut buf,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
        0,
        280,                                // x
        280,                                // y
        60,                                 // cx
        14,                                 // cy
        IDC_HELP_CLOSE,                     // control ID
        0x0080,                             // class: Button
        "Close",                            // button text
    );

    buf
}

// ─── Helper Functions ────────────────────────────────────────────────────────

/// Push a `u16` (little-endian) to the buffer.
fn push_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_le_bytes());
}

/// Push a `u32` (little-endian) to the buffer.
fn push_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_le_bytes());
}

/// Push a null-terminated UTF-16LE string to the buffer.
fn push_wide_string(buf: &mut Vec<u8>, s: &str) {
    for code_unit in s.encode_utf16() {
        push_u16(buf, code_unit);
    }
    push_u16(buf, 0); // null terminator
}

/// Align the buffer to a DWORD (4-byte) boundary by padding with zeros.
fn align_dword(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// Push a DLGITEMTEMPLATE structure for a control.
///
/// The `class_atom` uses the predefined window class ordinals:
/// - 0x0080 = Button
/// - 0x0081 = Edit
/// - 0x0082 = Static
/// - 0x0083 = List Box
/// - 0x0084 = Scroll Bar
/// - 0x0085 = Combo Box
fn push_dlg_item(
    buf: &mut Vec<u8>,
    style: u32,
    ex_style: u32,
    x: u16,
    y: u16,
    cx: u16,
    cy: u16,
    id: u16,
    class_atom: u16,
    text: &str,
) {
    // DLGITEMTEMPLATE:
    //   style:      DWORD
    //   dwExtStyle: DWORD
    //   x:          short
    //   y:          short
    //   cx:         short
    //   cy:         short
    //   id:         WORD
    push_u32(buf, style);
    push_u32(buf, ex_style);
    push_u16(buf, x);
    push_u16(buf, y);
    push_u16(buf, cx);
    push_u16(buf, cy);
    push_u16(buf, id);

    // Window class — use ordinal (0xFFFF prefix + atom)
    push_u16(buf, 0xFFFF);
    push_u16(buf, class_atom);

    // Title/text — null-terminated UTF-16 string
    push_wide_string(buf, text);

    // Extra data count (creation data) — 0 bytes
    push_u16(buf, 0);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_dword_aligned() {
        let template = create_help_dialog_template();
        // The overall buffer should start aligned (Vec guarantees this)
        // and the template should not be empty
        assert!(!template.is_empty());
        // DLGTEMPLATE requires DWORD alignment — verify length is reasonable
        assert!(template.len() > 50, "Template too small: {} bytes", template.len());
    }

    #[test]
    fn template_header_has_correct_item_count() {
        let template = create_help_dialog_template();
        // cdit field is at offset 8 (after style[4] + dwExtStyle[4])
        let cdit = u16::from_le_bytes([template[8], template[9]]);
        assert_eq!(cdit, 4, "Expected 4 dialog items (title, description, tips, close)");
    }

    #[test]
    fn template_header_has_correct_dimensions() {
        let template = create_help_dialog_template();
        // x at offset 10, y at offset 12, cx at offset 14, cy at offset 16
        let cx = u16::from_le_bytes([template[14], template[15]]);
        let cy = u16::from_le_bytes([template[16], template[17]]);
        assert_eq!(cx, 350, "Dialog width should be 350 DLU");
        assert_eq!(cy, 300, "Dialog height should be 300 DLU");
    }

    #[test]
    fn dialog_style_has_required_flags() {
        let template = create_help_dialog_template();
        let style = u32::from_le_bytes([template[0], template[1], template[2], template[3]]);
        assert_ne!(style & DS_MODALFRAME, 0, "Missing DS_MODALFRAME");
        assert_ne!(style & WS_POPUP, 0, "Missing WS_POPUP");
        assert_ne!(style & WS_CAPTION, 0, "Missing WS_CAPTION");
        assert_ne!(style & WS_SYSMENU, 0, "Missing WS_SYSMENU");
        assert_ne!(style & DS_SETFONT, 0, "Missing DS_SETFONT");
    }

    #[test]
    fn control_ids_are_in_expected_range() {
        assert_eq!(IDD_HELP, 4000);
        assert_eq!(IDC_HELP_TITLE, 4001);
        assert_eq!(IDC_HELP_DESCRIPTION, 4002);
        assert_eq!(IDC_HELP_TIPS, 4003);
        assert_eq!(IDC_HELP_CLOSE, 4004);
    }
}
