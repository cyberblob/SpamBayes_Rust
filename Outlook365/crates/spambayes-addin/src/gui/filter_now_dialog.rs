//! Filter Now dialog — batch filtering on selected folders.
//!
//! This is a 1-to-1 replacement of `FilterNowDialog` from
//! `tkinter_filter_now.py`. It allows users to select folders, choose
//! filter actions, and run the filter with progress feedback.
//!
//! **Validates: Requirements 10.1–10.7**

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk4::prelude::*;
use spambayes_config::{AppConfig, FolderId};

use super::folder_browser::{FolderBrowserDialog, FolderProvider, SelectionMode};
use super::message_boxes;

// ─── FilterProgress Messages ─────────────────────────────────────────────────

/// Messages sent from the filter worker thread back to the GTK thread.
enum FilterProgress {
    /// A status text update.
    Status(String),
    /// Filtering is complete.
    Complete { was_cancelled: bool },
}

// ─── FilterNowDialog ─────────────────────────────────────────────────────────

/// Inner state shared via `Rc` for closure access.
struct FilterNowDialogInner {
    window: gtk4::Window,
    /// Label showing selected folder names.
    folder_label: gtk4::Label,
    /// "Perform all filter actions" radio button (group leader).
    action_all_radio: gtk4::CheckButton,
    /// "Score messages, but don't perform filter action" radio button.
    #[allow(dead_code)]
    action_score_only_radio: gtk4::CheckButton,
    /// "Unread mail" checkbox.
    only_unread_check: gtk4::CheckButton,
    /// "Mail never previously spam filtered" checkbox.
    only_unseen_check: gtk4::CheckButton,
    /// Progress bar (indeterminate mode during filtering).
    progress_bar: gtk4::ProgressBar,
    /// Status label below progress bar.
    status_label: gtk4::Label,
    /// Start/Stop toggle button.
    start_stop_btn: gtk4::Button,
    /// Currently selected folder IDs.
    folder_ids: RefCell<Vec<FolderId>>,
    /// Cached folder names for display.
    folder_names: RefCell<Vec<String>>,
    /// Whether filtering is currently in progress.
    is_filtering: Arc<AtomicBool>,
    /// Cancellation flag checked by the worker thread.
    cancelled: Arc<AtomicBool>,
    /// The application config (for load/save).
    config: RefCell<AppConfig>,
    /// Folder provider for the browse dialog.
    folder_provider: Rc<dyn FolderProvider>,
    /// Source ID for the pulse animation timer, if active.
    pulse_source: RefCell<Option<glib::SourceId>>,
}

/// The Filter Now dialog.
///
/// Provides:
/// - Folder selection via Browse button
/// - Filter action radio buttons (perform all / score only)
/// - Restriction checkboxes (unread / never filtered)
/// - Progress bar + status label during filtering
/// - Start/Stop toggle + Close button
///
/// **Validates: Requirements 10.1, 10.2, 10.3, 10.4, 10.5, 10.6, 10.7**
pub struct FilterNowDialog {
    inner: Rc<FilterNowDialogInner>,
}

impl FilterNowDialog {
    /// Create and display the Filter Now dialog.
    ///
    /// Loads saved folder selections and filter options from the config's
    /// `filter_now` section.
    ///
    /// # Arguments
    /// * `config` - Application config to load/save filter settings
    /// * `folder_provider` - Provider for folder browsing
    pub fn new(config: &AppConfig, folder_provider: Rc<dyn FolderProvider>) -> Self {
        let filter_now = &config.filter_now;

        // ─── Build the window ────────────────────────────────────────────
        let window = gtk4::Window::builder()
            .title("SpamBayes — Filter Now")
            .default_width(500)
            .default_height(400)
            .resizable(true)
            .build();

        // ─── Main vertical layout ────────────────────────────────────────
        let main_vbox = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(10)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();

        // ═══ Section 1: "Filter the following folders" ═══════════════════
        let folder_frame = gtk4::Frame::builder()
            .label("Filter the following folders")
            .build();

        let folder_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();

        let folder_label = gtk4::Label::builder()
            .label("No folders selected")
            .halign(gtk4::Align::Start)
            .hexpand(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .xalign(0.0)
            .build();
        // Give it a sunken appearance via CSS class
        folder_label.add_css_class("dim-label");

        let browse_btn = gtk4::Button::builder()
            .label("Browse...")
            .build();

        folder_box.append(&folder_label);
        folder_box.append(&browse_btn);
        folder_frame.set_child(Some(&folder_box));

        main_vbox.append(&folder_frame);

        // ═══ Section 2: "Filter action" ══════════════════════════════════
        let action_frame = gtk4::Frame::builder()
            .label("Filter action")
            .build();

        let action_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(4)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();

        // GTK4 radio buttons are CheckButtons linked via set_group
        let action_all_radio = gtk4::CheckButton::builder()
            .label("Perform all filter actions")
            .active(filter_now.action_all)
            .build();

        let action_score_only_radio = gtk4::CheckButton::builder()
            .label("Score messages, but don't perform filter action")
            .active(!filter_now.action_all)
            .build();
        // Link as radio group
        action_score_only_radio.set_group(Some(&action_all_radio));

        action_box.append(&action_all_radio);
        action_box.append(&action_score_only_radio);
        action_frame.set_child(Some(&action_box));

        main_vbox.append(&action_frame);

        // ═══ Section 3: "Restrict the filter to" ═════════════════════════
        let restrict_frame = gtk4::Frame::builder()
            .label("Restrict the filter to")
            .build();

        let restrict_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(4)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();

        let only_unread_check = gtk4::CheckButton::builder()
            .label("Unread mail")
            .active(filter_now.only_unread)
            .build();

        let only_unseen_check = gtk4::CheckButton::builder()
            .label("Mail never previously spam filtered")
            .active(filter_now.only_unseen)
            .build();

        restrict_box.append(&only_unread_check);
        restrict_box.append(&only_unseen_check);
        restrict_frame.set_child(Some(&restrict_box));

        main_vbox.append(&restrict_frame);

        // ═══ Section 4: Progress ═════════════════════════════════════════
        let progress_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(4)
            .build();

        let progress_bar = gtk4::ProgressBar::builder()
            .show_text(false)
            .build();

        let status_label = gtk4::Label::builder()
            .label("Ready to filter")
            .halign(gtk4::Align::Start)
            .xalign(0.0)
            .build();

        progress_box.append(&progress_bar);
        progress_box.append(&status_label);

        main_vbox.append(&progress_box);

        // ═══ Section 5: Buttons ══════════════════════════════════════════
        // Spacer to push buttons to bottom
        let spacer = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .vexpand(true)
            .build();
        main_vbox.append(&spacer);

        let button_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk4::Align::End)
            .build();

        let start_stop_btn = gtk4::Button::builder()
            .label("Start Filtering")
            .width_request(120)
            .build();

        let close_btn = gtk4::Button::builder()
            .label("Close")
            .width_request(80)
            .build();

        button_box.append(&start_stop_btn);
        button_box.append(&close_btn);

        main_vbox.append(&button_box);

        // ─── Set window content ──────────────────────────────────────────
        window.set_child(Some(&main_vbox));

        // ─── Load saved folder IDs from config ───────────────────────────
        let folder_ids = filter_now.folder_ids.clone();
        let folder_names: Vec<String> = Vec::new();

        // ─── Build the inner Rc ──────────────────────────────────────────
        let inner = Rc::new(FilterNowDialogInner {
            window,
            folder_label,
            action_all_radio,
            action_score_only_radio,
            only_unread_check,
            only_unseen_check,
            progress_bar,
            status_label,
            start_stop_btn,
            folder_ids: RefCell::new(folder_ids),
            folder_names: RefCell::new(folder_names),
            is_filtering: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            config: RefCell::new(config.clone()),
            folder_provider,
            pulse_source: RefCell::new(None),
        });

        // ─── Update folder display if we have saved folder IDs ───────────
        Self::update_folder_display_static(&inner);

        // ─── Wire Browse button ──────────────────────────────────────────
        {
            let inner_browse = Rc::clone(&inner);
            inner.window.child().unwrap(); // verify child exists
            browse_btn.connect_clicked(move |btn| {
                let parent_window = btn.root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let current_ids = inner_browse.folder_ids.borrow().clone();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    inner_browse.folder_provider.as_ref(),
                    SelectionMode::Multi,
                    &current_ids,
                );
                if let Some(selections) = dialog.run() {
                    let new_ids: Vec<FolderId> = selections.iter()
                        .map(|(id, _name)| id.clone())
                        .collect();
                    let new_names: Vec<String> = selections.iter()
                        .map(|(_id, name)| name.clone())
                        .collect();
                    *inner_browse.folder_ids.borrow_mut() = new_ids;
                    *inner_browse.folder_names.borrow_mut() = new_names;
                    Self::update_folder_display_static(&inner_browse);
                }
            });
        }

        // ─── Wire Start/Stop button ─────────────────────────────────────
        {
            let inner_start = Rc::clone(&inner);
            inner.start_stop_btn.connect_clicked(move |_| {
                if inner_start.is_filtering.load(Ordering::SeqCst) {
                    Self::stop_filtering_static(&inner_start);
                } else {
                    Self::start_filtering_static(&inner_start);
                }
            });
        }

        // ─── Wire Close button ───────────────────────────────────────────
        {
            let inner_close = Rc::clone(&inner);
            close_btn.connect_clicked(move |_| {
                Self::close_static(&inner_close);
            });
        }

        // ─── Wire window close-request (X button) ───────────────────────
        {
            let inner_close_req = Rc::clone(&inner);
            inner.window.connect_close_request(move |_| {
                if inner_close_req.is_filtering.load(Ordering::SeqCst) {
                    let parent = Some(&inner_close_req.window);
                    let confirmed = message_boxes::ask_question(
                        parent.map(|w| w as &gtk4::Window),
                        "SpamBayes",
                        "Filtering is still in progress. Are you sure you want to close?",
                    );
                    if !confirmed {
                        return glib::Propagation::Stop;
                    }
                    // User confirmed — cancel the filter and close
                    inner_close_req.cancelled.store(true, Ordering::SeqCst);
                    inner_close_req.is_filtering.store(false, Ordering::SeqCst);
                }
                glib::Propagation::Proceed
            });
        }

        let dialog = Self { inner };
        dialog
    }

    /// Present (show) the dialog window.
    pub fn present(&self) {
        self.inner.window.present();
    }

    // ─── Internal Static Helpers (for use in closures) ───────────────────

    /// Update the folder label display text.
    fn update_folder_display_static(inner: &Rc<FilterNowDialogInner>) {
        let ids = inner.folder_ids.borrow();
        let names = inner.folder_names.borrow();

        if ids.is_empty() {
            inner.folder_label.set_text("No folders selected");
            return;
        }

        if !names.is_empty() {
            // Display folder names, truncated if too long
            let display_text = names.join(", ");
            if display_text.len() > 60 {
                let truncated = &display_text[..57];
                inner.folder_label.set_text(&format!("{}...", truncated));
            } else {
                inner.folder_label.set_text(&display_text);
            }
        } else {
            // Only have IDs but no names — show count
            let count = ids.len();
            inner.folder_label.set_text(&format!(
                "{} folder{} selected",
                count,
                if count == 1 { "" } else { "s" }
            ));
        }
    }

    /// Start the filtering operation.
    ///
    /// Saves settings to config, spawns a filter worker on a background
    /// thread, toggles button text to "Stop Filtering", and starts the
    /// indeterminate progress bar.
    ///
    /// **Validates: Requirements 10.5, 10.6**
    fn start_filtering_static(inner: &Rc<FilterNowDialogInner>) {
        // Validate that folders are selected
        if inner.folder_ids.borrow().is_empty() {
            message_boxes::report_error(
                Some(&inner.window),
                "SpamBayes",
                "Please select at least one folder to filter.",
            );
            return;
        }

        // Save current settings to config
        {
            let mut config = inner.config.borrow_mut();
            config.filter_now.folder_ids = inner.folder_ids.borrow().clone();
            config.filter_now.action_all = inner.action_all_radio.is_active();
            config.filter_now.only_unread = inner.only_unread_check.is_active();
            config.filter_now.only_unseen = inner.only_unseen_check.is_active();
        }

        // Set filtering state
        inner.is_filtering.store(true, Ordering::SeqCst);
        inner.cancelled.store(false, Ordering::SeqCst);

        // Update UI
        inner.start_stop_btn.set_label("Stop Filtering");
        inner.status_label.set_label("Filtering messages...");

        // Start indeterminate progress bar
        Self::start_pulse(inner);

        // Clone values needed by the worker thread (only Send types)
        let cancelled = Arc::clone(&inner.cancelled);
        let is_filtering = Arc::clone(&inner.is_filtering);
        let folder_ids = inner.folder_ids.borrow().clone();
        let action_all = inner.action_all_radio.is_active();
        let only_unread = inner.only_unread_check.is_active();
        let only_unseen = inner.only_unseen_check.is_active();

        // Use a crossbeam channel to send progress messages back to the GTK thread.
        // The GTK thread polls this via glib::idle_add_local.
        let (progress_tx, progress_rx) = crossbeam_channel::unbounded::<FilterProgress>();

        // Set up a GLib idle source that polls the progress channel and
        // updates the UI. This runs on the GTK thread so it can touch widgets.
        let inner_for_progress = Rc::clone(inner);
        glib::idle_add_local(move || {
            // Drain all pending messages
            while let Ok(msg) = progress_rx.try_recv() {
                match msg {
                    FilterProgress::Status(text) => {
                        inner_for_progress.status_label.set_label(&text);
                    }
                    FilterProgress::Complete { was_cancelled } => {
                        inner_for_progress.is_filtering.store(false, Ordering::SeqCst);
                        inner_for_progress.start_stop_btn.set_label("Start Filtering");
                        Self::stop_pulse(&inner_for_progress);
                        if was_cancelled {
                            inner_for_progress.status_label.set_label("Filtering stopped.");
                        } else {
                            inner_for_progress.status_label.set_label("Filtering complete!");
                        }
                        // Remove this idle source — filtering is done
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });

        // Spawn filter worker on background thread
        std::thread::spawn(move || {
            // The real filter implementation will iterate over messages
            // in the selected folders. For now, this demonstrates the
            // threading pattern with simulated work.
            let total_folders = folder_ids.len();

            for (i, _folder_id) in folder_ids.iter().enumerate() {
                // Check cancellation
                if cancelled.load(Ordering::SeqCst) {
                    let _ = progress_tx.send(FilterProgress::Status(
                        "Filtering cancelled.".to_string(),
                    ));
                    break;
                }

                // Send progress update
                let msg = format!(
                    "Filtering folder {} of {} (action_all={}, unread={}, unseen={})...",
                    i + 1, total_folders, action_all, only_unread, only_unseen
                );
                let _ = progress_tx.send(FilterProgress::Status(msg));

                // Simulate work per folder (real implementation calls filter logic)
                std::thread::sleep(std::time::Duration::from_millis(500));
            }

            // Signal completion
            let was_cancelled = cancelled.load(Ordering::SeqCst);
            is_filtering.store(false, Ordering::SeqCst);
            let _ = progress_tx.send(FilterProgress::Complete { was_cancelled });
        });
    }

    /// Stop the currently running filter operation.
    ///
    /// Sets the cancelled flag; the worker thread checks it per message
    /// and will terminate its loop.
    fn stop_filtering_static(inner: &Rc<FilterNowDialogInner>) {
        inner.cancelled.store(true, Ordering::SeqCst);
        inner.start_stop_btn.set_label("Start Filtering");
        inner.status_label.set_label("Stopping...");
        Self::stop_pulse(inner);
    }

    /// Close the dialog (with confirmation if filtering is in progress).
    ///
    /// **Validates: Requirement 10.6**
    fn close_static(inner: &Rc<FilterNowDialogInner>) {
        if inner.is_filtering.load(Ordering::SeqCst) {
            let confirmed = message_boxes::ask_question(
                Some(&inner.window),
                "SpamBayes",
                "Filtering is still in progress. Are you sure you want to close?",
            );
            if !confirmed {
                return;
            }
            // Cancel the running filter
            inner.cancelled.store(true, Ordering::SeqCst);
            inner.is_filtering.store(false, Ordering::SeqCst);
        }
        Self::stop_pulse(inner);
        inner.window.close();
    }

    /// Start the pulse (indeterminate) animation on the progress bar.
    fn start_pulse(inner: &Rc<FilterNowDialogInner>) {
        // Remove any existing pulse source
        if let Some(source_id) = inner.pulse_source.borrow_mut().take() {
            source_id.remove();
        }

        let bar = inner.progress_bar.clone();
        let source_id = glib::timeout_add_local(
            std::time::Duration::from_millis(50),
            move || {
                bar.pulse();
                glib::ControlFlow::Continue
            },
        );
        *inner.pulse_source.borrow_mut() = Some(source_id);
    }

    /// Stop the pulse animation on the progress bar.
    fn stop_pulse(inner: &Rc<FilterNowDialogInner>) {
        if let Some(source_id) = inner.pulse_source.borrow_mut().take() {
            source_id.remove();
        }
        inner.progress_bar.set_fraction(0.0);
    }
}
