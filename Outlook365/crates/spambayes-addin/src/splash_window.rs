//! Lightweight Win32 splash/loading window.
//!
//! Displays a small borderless window with "Loading SpamBayes Manager..."
//! immediately at process start, before GTK4 or any heavy dependencies
//! are initialized. Closed automatically once the GTK4 Manager window
//! is ready to present.
//!
//! Uses only Win32 APIs (no GTK4 dependency) so it can appear instantly.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, DeleteObject, EndPaint, FillRect, SelectObject, SetBkMode,
    SetTextColor, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, DEFAULT_CHARSET, DEFAULT_QUALITY,
    FF_SWISS, HBRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetSystemMetrics, IsWindow, PeekMessageW, PostQuitMessage,
    RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWDEFAULT, WM_DESTROY,
    WM_PAINT, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP,
};

/// Width of the splash window in pixels.
const SPLASH_WIDTH: i32 = 340;
/// Height of the splash window in pixels.
const SPLASH_HEIGHT: i32 = 100;

/// Window class name for the splash window.
const CLASS_NAME: &str = "SpamBayesSplash";

/// A lightweight splash window shown during Manager startup.
///
/// Call `show()` at the very beginning of `main()` to create and display
/// the window. Call `.close()` once the GTK4 Manager window is ready.
pub struct SplashHandle {
    hwnd: HWND,
    closed: Arc<AtomicBool>,
}

// SAFETY: The HWND is only accessed from the main thread (STA).
unsafe impl Send for SplashHandle {}
unsafe impl Sync for SplashHandle {}

impl SplashHandle {
    /// Close the splash window and clean up.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            unsafe {
                if IsWindow(self.hwnd).as_bool() {
                    let _ = DestroyWindow(self.hwnd);
                }
            }
            // Pump remaining messages so WM_DESTROY is processed.
            pump_messages();
        }
    }
}

impl Drop for SplashHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Show the splash window and return a handle to close it later.
///
/// This function registers a minimal Win32 window class, creates a centered
/// popup window, and displays the loading message. It returns immediately
/// after showing the window (non-blocking).
pub fn show() -> Option<SplashHandle> {
    unsafe {
        let hinstance = GetModuleHandleW(None).ok()?;

        // Register the window class (idempotent — repeated calls are harmless).
        let class_name_wide: Vec<u16> =
            CLASS_NAME.encode_utf16().chain(std::iter::once(0)).collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(splash_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name_wide.as_ptr()),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            ..Default::default()
        };
        // RegisterClass returns 0 if the class already exists — that's fine.
        let _ = RegisterClassW(&wc);

        // Center the splash on screen.
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let x = (screen_w - SPLASH_WIDTH) / 2;
        let y = (screen_h - SPLASH_HEIGHT) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            PCWSTR(class_name_wide.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            x,
            y,
            SPLASH_WIDTH,
            SPLASH_HEIGHT,
            None,
            None,
            hinstance,
            None,
        )
        .ok()?;

        let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);

        // Pump messages so the window paints immediately.
        pump_messages();

        Some(SplashHandle {
            hwnd,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }
}

/// Pump all pending Win32 messages (non-blocking).
fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Window procedure for the splash window.
///
/// Handles WM_PAINT to draw the loading text and WM_DESTROY to clean up.
unsafe extern "system" fn splash_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            // Get client area dimensions.
            let mut rect = std::mem::zeroed();
            let _ = GetClientRect(hwnd, &mut rect);

            // Fill background with white.
            let bg_brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(
                windows::Win32::Foundation::COLORREF(0x00FF_FFFF), // White
            );
            FillRect(hdc, &rect, bg_brush);
            let _ = DeleteObject(bg_brush);

            // Draw a thin border.
            let border_brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(
                windows::Win32::Foundation::COLORREF(0x00A0_A0A0), // Gray
            );
            windows::Win32::Graphics::Gdi::FrameRect(hdc, &rect, border_brush);
            let _ = DeleteObject(border_brush);

            // Create a font for the text.
            let font_name: Vec<u16> = "Segoe UI"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut face_name = [0u16; 32];
            for (i, &ch) in font_name.iter().enumerate().take(31) {
                face_name[i] = ch;
            }

            let hfont = CreateFontW(
                20,  // height
                0,   // width (auto)
                0,   // escapement
                0,   // orientation
                400, // weight (normal)
                0,   // italic
                0,   // underline
                0,   // strikeout
                DEFAULT_CHARSET.0.into(),
                OUT_DEFAULT_PRECIS.0.into(),
                CLIP_DEFAULT_PRECIS.0.into(),
                DEFAULT_QUALITY.0.into(),
                FF_SWISS.0.into(),
                PCWSTR(face_name.as_ptr()),
            );

            let old_font = SelectObject(hdc, hfont);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(
                hdc,
                windows::Win32::Foundation::COLORREF(0x0050_5050), // Dark gray text
            );

            // Draw the loading text centered.
            let text = "Loading SpamBayes Manager...";
            let mut text_wide: Vec<u16> = text.encode_utf16().collect();

            windows::Win32::Graphics::Gdi::DrawTextW(
                hdc,
                &mut text_wide,
                &mut rect,
                windows::Win32::Graphics::Gdi::DT_CENTER
                    | windows::Win32::Graphics::Gdi::DT_VCENTER
                    | windows::Win32::Graphics::Gdi::DT_SINGLELINE,
            );

            SelectObject(hdc, old_font);
            let _ = DeleteObject(hfont);
            let _ = EndPaint(hwnd, &ps);

            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
