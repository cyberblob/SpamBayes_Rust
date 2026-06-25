//! Once-per-session error reporting for the `SpamBayes` add-in.
//!
//! Provides an [`ErrorReporter`] that displays user-facing error messages via
//! a Windows `MessageBox`, but suppresses duplicate notifications for the same
//! HRESULT error code within a single Outlook session.
//!
//! **Validates: Requirements 17.4, 17.5**

use std::collections::HashSet;
use std::sync::Mutex;

use windows::core::HRESULT;

/// Trait abstracting the message-box display mechanism.
///
/// This allows unit tests to verify reporting logic without spawning
/// real Win32 message boxes.
pub trait MessageDisplay: Send {
    /// Show an error message to the user with the given title and body.
    fn show_error(&self, title: &str, message: &str);
}

/// Default implementation that calls the Win32 `MessageBoxW`.
pub struct Win32MessageDisplay;

impl MessageDisplay for Win32MessageDisplay {
    fn show_error(&self, title: &str, message: &str) {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

        let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let msg_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            MessageBoxW(
                None,
                PCWSTR(msg_wide.as_ptr()),
                PCWSTR(title_wide.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

/// Once-per-session error reporter.
///
/// Tracks which HRESULT error codes have already been shown to the user
/// during the current session. Subsequent occurrences of the same code
/// are silently suppressed to avoid spamming the user with repeated
/// dialogs for recurring errors.
///
/// Thread-safe: the internal set of reported codes is protected by a
/// [`Mutex`].
///
/// **Validates: Requirements 17.4, 17.5**
pub struct ErrorReporter {
    reported: Mutex<HashSet<i32>>,
    display: Box<dyn MessageDisplay>,
}

impl ErrorReporter {
    /// Creates a new `ErrorReporter` using the real Win32 message box.
    #[must_use]
    pub fn new() -> Self {
        Self {
            reported: Mutex::new(HashSet::new()),
            display: Box::new(Win32MessageDisplay),
        }
    }

    /// Creates a new `ErrorReporter` with a custom [`MessageDisplay`]
    /// implementation. Useful for testing.
    #[must_use]
    pub fn with_display(display: Box<dyn MessageDisplay>) -> Self {
        Self {
            reported: Mutex::new(HashSet::new()),
            display,
        }
    }

    /// Reports an error to the user, but only on the first occurrence
    /// of the given HRESULT code within this session.
    ///
    /// If the same `hr` value has already been reported, this method
    /// returns `false` and does nothing. Otherwise it displays a
    /// message box with the error description and returns `true`.
    ///
    /// # Arguments
    ///
    /// * `hr` — The HRESULT error code to report.
    /// * `description` — A human-readable description of the error.
    ///
    /// # Returns
    ///
    /// `true` if the message was displayed (first occurrence),
    /// `false` if suppressed (duplicate).
    pub fn report_once(&self, hr: HRESULT, description: &str) -> bool {
        let code = hr.0;

        let mut reported = self.reported.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        if reported.contains(&code) {
            return false;
        }

        reported.insert(code);
        // Drop the lock before showing the message box to avoid holding
        // it during a potentially blocking UI call.
        drop(reported);

        let title = "SpamBayes Error";
        let message = format!("{}\n\n(HRESULT: 0x{:08X})", description, code as u32);

        self.display.show_error(title, &message);

        true
    }

    /// Returns `true` if the given HRESULT has already been reported
    /// during this session.
    pub fn was_reported(&self, hr: HRESULT) -> bool {
        let reported = self.reported.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reported.contains(&hr.0)
    }

    /// Resets the reporter, clearing all tracked HRESULT codes.
    ///
    /// This is primarily useful for testing or session-reset scenarios.
    pub fn reset(&self) {
        let mut reported = self.reported.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reported.clear();
    }
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    /// A test double that records messages instead of displaying them.
    struct FakeDisplay {
        messages: Arc<StdMutex<Vec<(String, String)>>>,
    }

    impl MessageDisplay for FakeDisplay {
        fn show_error(&self, title: &str, message: &str) {
            self.messages
                .lock()
                .unwrap()
                .push((title.to_string(), message.to_string()));
        }
    }

    fn make_reporter() -> (ErrorReporter, Arc<StdMutex<Vec<(String, String)>>>) {
        let messages = Arc::new(StdMutex::new(Vec::new()));
        let display = FakeDisplay {
            messages: Arc::clone(&messages),
        };
        let reporter = ErrorReporter::with_display(Box::new(display));
        (reporter, messages)
    }

    #[test]
    fn first_report_shows_message() {
        let (reporter, messages) = make_reporter();
        let hr = HRESULT(0x80004005_u32 as i32); // E_FAIL

        let shown = reporter.report_once(hr, "Something failed");

        assert!(shown);
        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].1.contains("Something failed"));
        assert!(msgs[0].1.contains("80004005"));
    }

    #[test]
    fn duplicate_report_is_suppressed() {
        let (reporter, messages) = make_reporter();
        let hr = HRESULT(0x80004005_u32 as i32);

        reporter.report_once(hr, "First");
        let shown = reporter.report_once(hr, "Second");

        assert!(!shown);
        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 1); // Only the first message was shown
    }

    #[test]
    fn different_hresults_are_reported_independently() {
        let (reporter, messages) = make_reporter();
        let hr1 = HRESULT(0x80004005_u32 as i32); // E_FAIL
        let hr2 = HRESULT(0x80070005_u32 as i32); // E_ACCESSDENIED

        let shown1 = reporter.report_once(hr1, "Fail");
        let shown2 = reporter.report_once(hr2, "Access denied");

        assert!(shown1);
        assert!(shown2);
        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn was_reported_tracks_state() {
        let (reporter, _) = make_reporter();
        let hr = HRESULT(0x80004005_u32 as i32);

        assert!(!reporter.was_reported(hr));
        reporter.report_once(hr, "Error");
        assert!(reporter.was_reported(hr));
    }

    #[test]
    fn reset_clears_all_tracked_codes() {
        let (reporter, messages) = make_reporter();
        let hr = HRESULT(0x80004005_u32 as i32);

        reporter.report_once(hr, "First");
        reporter.reset();
        let shown = reporter.report_once(hr, "After reset");

        assert!(shown);
        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 2); // Both shown
    }
}
