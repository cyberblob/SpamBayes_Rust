//! Progress dialog — reusable modal progress display for training and filtering.
//!
//! Shows a progress bar, status label, and Stop button. Progress updates
//! are received from worker threads via `glib::idle_add_local` and throttled
//! to 100ms minimum between visual updates.
//!
//! **Validates: Requirements 3.4, 10.4**

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;

/// Minimum interval between visual updates to avoid overwhelming the UI.
const THROTTLE_INTERVAL: Duration = Duration::from_millis(100);

/// Inner state shared via `Rc` so the dialog can be cloned into closures.
struct ProgressDialogInner {
    window: gtk4::Window,
    progress_bar: gtk4::ProgressBar,
    status_label: gtk4::Label,
    stop_btn: gtk4::Button,
    cancelled: Arc<AtomicBool>,
    last_update: RefCell<Instant>,
    /// Source ID for the pulse animation timer, if active.
    pulse_source: RefCell<Option<glib::SourceId>>,
}

/// A modal progress dialog with a progress bar, status label, and Stop button.
///
/// This struct is `Clone`-friendly (wraps inner state in `Rc`) so it can be
/// freely used from GTK signal closures. The `cancelled` flag uses `Arc<AtomicBool>`
/// and can be checked from any thread via [`is_cancelled`](Self::is_cancelled).
///
/// # Thread Safety
///
/// - All UI methods (`set_status`, `set_progress`, `set_indeterminate`, `close`)
///   must be called on the GTK thread.
/// - Worker threads should send updates to the GTK thread using
///   `glib::idle_add_local` and call these methods inside the closure.
/// - `is_cancelled()` and `cancelled()` are safe to call from any thread.
#[derive(Clone)]
pub struct ProgressDialog {
    inner: Rc<ProgressDialogInner>,
}

impl ProgressDialog {
    /// Create and display a new progress dialog.
    ///
    /// # Arguments
    /// * `parent` - Optional parent window for transient/modal positioning
    /// * `title` - Window title for the dialog
    pub fn new(parent: Option<&gtk4::Window>, title: &str) -> Self {
        // Build the window
        let window = gtk4::Window::builder()
            .title(title)
            .default_width(400)
            .default_height(150)
            .modal(true)
            .resizable(false)
            .deletable(false) // Disable the X button during operation
            .build();

        if let Some(parent_win) = parent {
            window.set_transient_for(Some(parent_win));
        }

        // Layout container
        let vbox = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(12)
            .margin_top(20)
            .margin_bottom(20)
            .margin_start(20)
            .margin_end(20)
            .build();

        // Status label
        let status_label = gtk4::Label::builder()
            .label("Initializing...")
            .halign(gtk4::Align::Start)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .build();

        // Progress bar
        let progress_bar = gtk4::ProgressBar::builder()
            .show_text(true)
            .build();

        // Stop button
        let stop_btn = gtk4::Button::builder()
            .label("Stop")
            .halign(gtk4::Align::Center)
            .build();

        // Assemble layout
        vbox.append(&status_label);
        vbox.append(&progress_bar);
        vbox.append(&stop_btn);

        window.set_child(Some(&vbox));

        // Cancelled flag
        let cancelled = Arc::new(AtomicBool::new(false));

        let dialog = Self {
            inner: Rc::new(ProgressDialogInner {
                window,
                progress_bar,
                status_label,
                stop_btn,
                cancelled,
                last_update: RefCell::new(Instant::now() - THROTTLE_INTERVAL),
                pulse_source: RefCell::new(None),
            }),
        };

        // Connect Stop button click
        let cancelled_flag = dialog.inner.cancelled.clone();
        let btn_ref = dialog.inner.stop_btn.clone();
        btn_ref.connect_clicked(move |btn| {
            cancelled_flag.store(true, Ordering::SeqCst);
            btn.set_sensitive(false);
            btn.set_label("Stopping...");
        });

        // Handle window close-request: treat as cancellation
        let cancelled_for_close = dialog.inner.cancelled.clone();
        dialog.inner.window.connect_close_request(move |_| {
            cancelled_for_close.store(true, Ordering::SeqCst);
            // Inhibit close — the caller should use close() explicitly
            glib::Propagation::Stop
        });

        // Present the window
        dialog.inner.window.present();

        dialog
    }

    /// Update the status text displayed below the progress bar.
    ///
    /// Updates are throttled to [`THROTTLE_INTERVAL`] minimum. If called more
    /// frequently, the intermediate updates are silently dropped.
    pub fn set_status(&self, message: &str) {
        if !self.should_update() {
            return;
        }
        self.inner.status_label.set_label(message);
        self.record_update();
    }

    /// Set the progress bar fraction (0.0 to 1.0).
    ///
    /// Updates are throttled to [`THROTTLE_INTERVAL`] minimum.
    pub fn set_progress(&self, fraction: f64) {
        if !self.should_update() {
            return;
        }
        let clamped = fraction.clamp(0.0, 1.0);
        self.inner.progress_bar.set_fraction(clamped);
        // Show percentage text
        let pct = (clamped * 100.0) as u32;
        self.inner
            .progress_bar
            .set_text(Some(&format!("{}%", pct)));
        self.record_update();
    }

    /// Set the progress bar to indeterminate (pulsing) mode or back to
    /// determinate mode.
    ///
    /// When `pulse` is `true`, the progress bar pulses continuously via a
    /// GLib timeout source. When `false`, the pulse animation stops and the
    /// bar returns to fraction-based display.
    pub fn set_indeterminate(&self, pulse: bool) {
        // Remove any existing pulse source
        if let Some(source_id) = self.inner.pulse_source.borrow_mut().take() {
            source_id.remove();
        }

        if pulse {
            self.inner.progress_bar.set_fraction(0.0);
            self.inner.progress_bar.set_text(None);

            // Start a pulse timer at ~50ms intervals for smooth animation
            let bar = self.inner.progress_bar.clone();
            let source_id = glib::timeout_add_local(Duration::from_millis(50), move || {
                bar.pulse();
                glib::ControlFlow::Continue
            });
            *self.inner.pulse_source.borrow_mut() = Some(source_id);
        } else {
            // Reset to determinate mode
            self.inner.progress_bar.set_fraction(0.0);
            self.inner.progress_bar.set_text(Some("0%"));
        }
    }

    /// Returns `true` if the user clicked Stop.
    ///
    /// This method is safe to call from any thread.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Get a clone of the `Arc<AtomicBool>` cancellation flag.
    ///
    /// Worker threads can hold this to check for cancellation without
    /// needing a reference to the dialog itself.
    pub fn cancelled(&self) -> Arc<AtomicBool> {
        self.inner.cancelled.clone()
    }

    /// Close and destroy the progress dialog.
    pub fn close(&self) {
        // Stop any pulse animation
        if let Some(source_id) = self.inner.pulse_source.borrow_mut().take() {
            source_id.remove();
        }
        self.inner.window.close();
    }

    /// Check if enough time has passed since the last visual update.
    fn should_update(&self) -> bool {
        let elapsed = self.inner.last_update.borrow().elapsed();
        elapsed >= THROTTLE_INTERVAL
    }

    /// Record the current time as the last update time.
    fn record_update(&self) {
        *self.inner.last_update.borrow_mut() = Instant::now();
    }
}
