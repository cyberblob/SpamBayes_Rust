//! Training tab — folder selection, batch training, and incremental settings.
//!
//! Provides controls for:
//! - Known good (ham) training folder selection (multi-select)
//! - Spam training folder selection (multi-select)
//! - Score after training / rebuild database checkboxes
//! - Start Training button
//! - Incremental Training section (recover spam, manual spam, message state combos)
//!
//! **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8**

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, CheckButton, ComboBoxText, Entry, Frame, Label, Orientation,
    ScrolledWindow,
};

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use spambayes_config::{AppConfig, FolderId, MessageReadState};

use crate::gui::folder_browser::{FolderBrowserDialog, FolderProvider, SelectionMode};
use crate::gui::message_boxes;
use crate::gui::progress_dialog::ProgressDialog;
use crate::manager_dlg::ManagerState;

use crossbeam_channel;

// ─── TrainingExecutor Trait ──────────────────────────────────────────────────

/// Result of a batch training operation, reported back to the GUI.
#[derive(Debug, Clone)]
pub struct TrainingResult {
    /// Total number of messages processed (including skipped).
    pub total_processed: u32,
    /// Number of messages newly trained or retrained.
    pub new_entries: u32,
    /// Number of messages that failed to process.
    pub errors: u32,
}

/// Trait for providing training capabilities to the GUI.
///
/// The implementer handles creating the training engine and folder provider
/// on the background thread with proper COM initialization. This abstraction
/// allows the training tab to be tested without actual MAPI/COM dependencies.
///
/// **Validates: Requirements 3.4, 3.7, 3.8**
pub trait TrainingExecutor: Send + Sync + 'static {
    /// Execute batch training on a background thread.
    ///
    /// # Arguments
    /// * `ham_folder_ids` - Folder IDs containing known good messages
    /// * `spam_folder_ids` - Folder IDs containing spam messages
    /// * `rescore` - Whether to rescore messages after training
    /// * `rebuild` - Whether to rebuild the entire database
    /// * `progress_reporter` - Callback for reporting progress (folder_name, current, total)
    /// * `is_cancelled` - Flag checked between messages to support cancellation
    ///
    /// # Returns
    /// `Ok(TrainingResult)` on success, `Err(String)` on failure.
    fn train_batch(
        &self,
        ham_folder_ids: Vec<FolderId>,
        spam_folder_ids: Vec<FolderId>,
        rescore: bool,
        rebuild: bool,
        progress_reporter: Box<dyn Fn(&str, u32, u32) + Send>,
        is_cancelled: Arc<AtomicBool>,
    ) -> Result<TrainingResult, String>;
}

// ─── TrainingMessage (channel messages from worker to GTK thread) ────────────

/// Messages sent from the training worker thread to the GTK main loop.
enum TrainingMessage {
    /// Progress update: folder name, current count, total count.
    Progress {
        folder_name: String,
        current: u32,
        total: u32,
    },
    /// Training completed successfully.
    Complete(TrainingResult),
    /// Training failed with an error message.
    Error(String),
}

// ─── Helper: map MessageReadState to ComboBoxText index ──────────────────────

fn message_read_state_to_index(state: &MessageReadState) -> u32 {
    match state {
        MessageReadState::None => 0,
        MessageReadState::Read => 1,
        MessageReadState::Unread => 2,
    }
}

// ─── TrainingTab ─────────────────────────────────────────────────────────────

/// The Training tab content.
///
/// Contains training folder selectors, batch training controls, and
/// incremental training settings.
///
/// **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8**
pub struct TrainingTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Folder Provider ─────────────────────────────────────────────────
    /// Provider for folder hierarchy data (used by browse buttons).
    pub folder_provider: Rc<dyn FolderProvider>,

    // ─── Training Executor ───────────────────────────────────────────────
    /// Optional training executor for performing batch training operations.
    /// Set via `set_training_executor` after construction.
    training_executor: Rc<RefCell<Option<Arc<dyn TrainingExecutor>>>>,

    // ─── Stored Folder IDs (for save) ────────────────────────────────────
    /// Selected ham training folder IDs (multi-select).
    pub ham_folder_ids: Rc<RefCell<Vec<FolderId>>>,
    /// Selected spam training folder IDs (multi-select).
    pub spam_folder_ids: Rc<RefCell<Vec<FolderId>>>,

    // ─── Ham Training Folders (Req 3.1) ──────────────────────────────────
    /// Entry showing currently selected ham training folders.
    pub ham_folder_entry: Entry,
    /// Browse button for ham training folders (multi-select).
    pub ham_folder_browse_btn: Button,

    // ─── Spam Training Folders (Req 3.2) ─────────────────────────────────
    /// Entry showing currently selected spam training folders.
    pub spam_folder_entry: Entry,
    /// Browse button for spam training folders (multi-select).
    pub spam_folder_browse_btn: Button,

    // ─── Training Options (Req 3.3) ──────────────────────────────────────
    /// "Score messages after training" checkbox.
    pub rescore_check: CheckButton,
    /// "Rebuild entire database" checkbox.
    pub rebuild_check: CheckButton,

    // ─── Start Training (Req 3.4) ────────────────────────────────────────
    /// "Start Training" button.
    pub start_training_btn: Button,

    // ─── Incremental Training (Req 3.5) ──────────────────────────────────
    /// "Train good when moved from spam back to Inbox" checkbox.
    pub train_recovered_spam_check: CheckButton,
    /// "Clicking 'Not Spam' should" combo box.
    pub not_spam_action_combo: ComboBoxText,
    /// "Train spam when manually moved to spam folder" checkbox.
    pub train_manual_spam_check: CheckButton,
    /// "Clicking 'Spam' should" combo box.
    pub spam_action_combo: ComboBoxText,
}

impl TrainingTab {
    /// Build the Training tab widget tree.
    ///
    /// # Arguments
    ///
    /// * `state` – Current manager state (ham/spam training folder IDs)
    /// * `config` – Application config (training options, message read states)
    /// * `folder_provider` – Provider for folder hierarchy data (used by browse buttons)
    ///
    /// **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8**
    #[must_use]
    pub fn new(
        state: &ManagerState,
        config: &AppConfig,
        folder_provider: Rc<dyn FolderProvider>,
    ) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Ham Training Folders section (Req 3.1) ───────────────────
        let ham_frame = Frame::new(Some("Folders with known good messages."));
        let ham_box = GtkBox::new(Orientation::Horizontal, 8);
        ham_box.set_margin_top(8);
        ham_box.set_margin_bottom(8);
        ham_box.set_margin_start(12);
        ham_box.set_margin_end(12);
        ham_box.set_valign(Align::Center);

        let ham_folder_entry = Entry::new();
        ham_folder_entry.set_hexpand(true);
        ham_folder_entry.set_editable(false);
        ham_folder_entry.set_placeholder_text(Some("Select folder(s)..."));
        // Resolve current selection to display names immediately
        if !state.ham_training_folder_ids.is_empty() {
            let names = FolderBrowserDialog::resolve_folder_names(
                folder_provider.as_ref(),
                &state.ham_training_folder_ids,
            );
            ham_folder_entry.set_text(&names.join("; "));
        }

        let ham_folder_browse_btn = Button::with_label("Browse...");
        ham_folder_browse_btn.set_tooltip_text(Some(
            "Select folders containing known good (ham) messages for training.",
        ));

        ham_box.append(&ham_folder_entry);
        ham_box.append(&ham_folder_browse_btn);
        ham_frame.set_child(Some(&ham_box));
        content_box.append(&ham_frame);

        // ─── 2. Spam Training Folders section (Req 3.2) ──────────────────
        let spam_frame = Frame::new(Some("Folders with spam or other junk messages."));
        let spam_box = GtkBox::new(Orientation::Horizontal, 8);
        spam_box.set_margin_top(8);
        spam_box.set_margin_bottom(8);
        spam_box.set_margin_start(12);
        spam_box.set_margin_end(12);
        spam_box.set_valign(Align::Center);

        let spam_folder_entry = Entry::new();
        spam_folder_entry.set_hexpand(true);
        spam_folder_entry.set_editable(false);
        spam_folder_entry.set_placeholder_text(Some("Select folder(s)..."));
        // Resolve current selection to display names immediately
        if !state.spam_training_folder_ids.is_empty() {
            let names = FolderBrowserDialog::resolve_folder_names(
                folder_provider.as_ref(),
                &state.spam_training_folder_ids,
            );
            spam_folder_entry.set_text(&names.join("; "));
        }

        let spam_folder_browse_btn = Button::with_label("Browse...");
        spam_folder_browse_btn.set_tooltip_text(Some(
            "Select folders containing known spam messages for training.",
        ));

        spam_box.append(&spam_folder_entry);
        spam_box.append(&spam_folder_browse_btn);
        spam_frame.set_child(Some(&spam_box));
        content_box.append(&spam_frame);

        // ─── 3. Training Options row (Req 3.3) ──────────────────────────
        let options_row = GtkBox::new(Orientation::Horizontal, 16);
        options_row.set_margin_top(4);
        options_row.set_margin_bottom(4);

        let rescore_check = CheckButton::with_label("Score messages after training");
        rescore_check.set_active(config.training.rescore);
        rescore_check.set_tooltip_text(Some(
            "After training, re-classify all messages in watched folders.",
        ));

        let rebuild_check = CheckButton::with_label("Rebuild entire database");
        rebuild_check.set_active(config.training.rebuild);
        rebuild_check.set_tooltip_text(Some(
            "Clear all learned data and rebuild from scratch using training folders.",
        ));

        options_row.append(&rescore_check);
        options_row.append(&rebuild_check);
        content_box.append(&options_row);

        // ─── 4. Start Training button (Req 3.4) ─────────────────────────
        let start_training_btn = Button::with_label("Start Training");
        start_training_btn.set_halign(Align::Center);
        start_training_btn.set_margin_top(8);
        start_training_btn.set_margin_bottom(8);
        start_training_btn.set_tooltip_text(Some(
            "Train the classifier using the selected ham and spam folders.",
        ));
        content_box.append(&start_training_btn);

        // ─── 5. Incremental Training section (Req 3.5) ──────────────────
        let incr_frame = Frame::new(Some("Incremental Training"));
        let incr_box = GtkBox::new(Orientation::Vertical, 8);
        incr_box.set_margin_top(8);
        incr_box.set_margin_bottom(8);
        incr_box.set_margin_start(12);
        incr_box.set_margin_end(12);

        // "Train good when recovered from spam" checkbox
        let train_recovered_spam_check = CheckButton::with_label(
            "Train that a message is good when it is moved from a spam\nfolder back to the Inbox.",
        );
        train_recovered_spam_check.set_active(config.training.train_recovered_spam);
        train_recovered_spam_check.set_tooltip_text(Some(
            "Automatically train messages as good when you move them from the spam folder back to the Inbox.",
        ));
        incr_box.append(&train_recovered_spam_check);

        // "Clicking 'Not Spam' should" row
        let not_spam_row = GtkBox::new(Orientation::Horizontal, 8);
        not_spam_row.set_valign(Align::Center);
        not_spam_row.set_margin_start(20);

        let not_spam_label = Label::new(Some("Clicking 'Not Spam' button should"));
        not_spam_label.set_halign(Align::Start);

        let not_spam_action_combo = ComboBoxText::new();
        not_spam_action_combo.append_text("not change the message");
        not_spam_action_combo.append_text("mark as read");
        not_spam_action_combo.append_text("mark as unread");
        not_spam_action_combo.set_active(Some(message_read_state_to_index(
            &config.general.recover_from_spam_message_state,
        )));

        not_spam_row.append(&not_spam_label);
        not_spam_row.append(&not_spam_action_combo);
        incr_box.append(&not_spam_row);

        // "Train spam when manually moved" checkbox
        let train_manual_spam_check = CheckButton::with_label(
            "Train that a message is spam when it is moved to the spam\nfolder.",
        );
        train_manual_spam_check.set_active(config.training.train_manual_spam);
        train_manual_spam_check.set_tooltip_text(Some(
            "Automatically train messages as spam when you manually move them to the spam folder.",
        ));
        incr_box.append(&train_manual_spam_check);

        // "Clicking 'Spam' should" row
        let spam_action_row = GtkBox::new(Orientation::Horizontal, 8);
        spam_action_row.set_valign(Align::Center);
        spam_action_row.set_margin_start(20);

        let spam_action_label = Label::new(Some("Clicking 'Spam' button should"));
        spam_action_label.set_halign(Align::Start);

        let spam_action_combo = ComboBoxText::new();
        spam_action_combo.append_text("not change the message");
        spam_action_combo.append_text("mark as read");
        spam_action_combo.append_text("mark as unread");
        spam_action_combo.set_active(Some(message_read_state_to_index(
            &config.general.delete_as_spam_message_state,
        )));

        spam_action_row.append(&spam_action_label);
        spam_action_row.append(&spam_action_combo);
        incr_box.append(&spam_action_row);

        incr_frame.set_child(Some(&incr_box));
        content_box.append(&incr_frame);

        // ─── Wrap in ScrolledWindow (Req 3.6) ───────────────────────────
        let container = ScrolledWindow::new();
        container.set_vexpand(true);
        container.set_hexpand(true);
        container.set_child(Some(&content_box));

        // ─── Initialize stored folder ID state ──────────────────────────
        let ham_folder_ids = Rc::new(RefCell::new(state.ham_training_folder_ids.clone()));
        let spam_folder_ids = Rc::new(RefCell::new(state.spam_training_folder_ids.clone()));

        // ─── Training executor (set later via set_training_executor) ─────
        let training_executor: Rc<RefCell<Option<Arc<dyn TrainingExecutor>>>> =
            Rc::new(RefCell::new(None));

        // ─── Wire Browse buttons to FolderBrowserDialog ─────────────────

        // Ham Training Folders Browse — multi-select mode (Req 3.1)
        {
            let provider = Rc::clone(&folder_provider);
            let ids = Rc::clone(&ham_folder_ids);
            let entry = ham_folder_entry.clone();
            ham_folder_browse_btn.connect_clicked(move |btn| {
                let parent_window = btn
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let current_ids = ids.borrow().clone();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    provider.as_ref(),
                    SelectionMode::Multi,
                    &current_ids,
                );
                if let Some(selections) = dialog.run() {
                    let new_ids: Vec<FolderId> =
                        selections.iter().map(|(id, _name)| id.clone()).collect();
                    let names: Vec<&str> =
                        selections.iter().map(|(_id, name)| name.as_str()).collect();
                    *ids.borrow_mut() = new_ids;
                    // Update the entry with folder names separated by "; "
                    if names.is_empty() {
                        entry.set_text("");
                    } else {
                        entry.set_text(&names.join("; "));
                    }
                }
                // On Cancel: do nothing (keep existing selection)
            });
        }

        // Spam Training Folders Browse — multi-select mode (Req 3.2)
        {
            let provider = Rc::clone(&folder_provider);
            let ids = Rc::clone(&spam_folder_ids);
            let entry = spam_folder_entry.clone();
            spam_folder_browse_btn.connect_clicked(move |btn| {
                let parent_window = btn
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let current_ids = ids.borrow().clone();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    provider.as_ref(),
                    SelectionMode::Multi,
                    &current_ids,
                );
                if let Some(selections) = dialog.run() {
                    let new_ids: Vec<FolderId> =
                        selections.iter().map(|(id, _name)| id.clone()).collect();
                    let names: Vec<&str> =
                        selections.iter().map(|(_id, name)| name.as_str()).collect();
                    *ids.borrow_mut() = new_ids;
                    // Update the entry with folder names separated by "; "
                    if names.is_empty() {
                        entry.set_text("");
                    } else {
                        entry.set_text(&names.join("; "));
                    }
                }
                // On Cancel: do nothing (keep existing selection)
            });
        }

        // ─── Wire Start Training button (Req 3.4, 3.7, 3.8) ──────────
        {
            let ham_ids = Rc::clone(&ham_folder_ids);
            let spam_ids = Rc::clone(&spam_folder_ids);
            let rescore = rescore_check.clone();
            let rebuild = rebuild_check.clone();
            let executor = Rc::clone(&training_executor);

            start_training_btn.connect_clicked(move |btn| {
                let parent_window = btn
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());

                // ── Validate: at least one folder must be selected (Req 3.7) ──
                let ham = ham_ids.borrow().clone();
                let spam = spam_ids.borrow().clone();
                if ham.is_empty() && spam.is_empty() {
                    message_boxes::report_error(
                        parent_window.as_ref(),
                        "SpamBayes",
                        "You must select at least one ham or spam training folder \
                         before starting training.",
                    );
                    return;
                }

                // ── If Rebuild checked, confirm (Req 3.8) ────────────────────
                let do_rebuild = rebuild.is_active();
                if do_rebuild {
                    let confirmed = message_boxes::ask_question(
                        parent_window.as_ref(),
                        "SpamBayes",
                        "Rebuilding will erase the current training database and \
                         retrain from scratch.\n\nAre you sure you want to continue?",
                    );
                    if !confirmed {
                        return;
                    }
                }

                // ── Check if a training executor is available ─────────────────
                let executor_arc = {
                    let guard = executor.borrow();
                    match guard.as_ref() {
                        Some(exec) => Arc::clone(exec),
                        None => {
                            message_boxes::report_error(
                                parent_window.as_ref(),
                                "SpamBayes",
                                "Training is not available. The training engine has \
                                 not been initialized.",
                            );
                            return;
                        }
                    }
                };

                let do_rescore = rescore.is_active();

                // ── Open ProgressDialog ──────────────────────────────────────
                let progress_dialog =
                    ProgressDialog::new(parent_window.as_ref(), "Training Progress");

                let cancelled = progress_dialog.cancelled();

                // ── Set up crossbeam channel for worker → GTK communication ──
                let (sender, receiver) =
                    crossbeam_channel::unbounded::<TrainingMessage>();

                // ── Spawn background training thread ─────────────────────────
                let worker_cancelled = Arc::clone(&cancelled);

                std::thread::spawn(move || {
                    // Create progress reporter that sends updates to GTK thread
                    let progress_sender = sender.clone();
                    let progress_fn: Box<dyn Fn(&str, u32, u32) + Send> =
                        Box::new(move |folder_name: &str, current: u32, total: u32| {
                            let _ = progress_sender.send(TrainingMessage::Progress {
                                folder_name: folder_name.to_owned(),
                                current,
                                total,
                            });
                        });

                    // Execute training (catch panics to ensure Complete/Error is sent)
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        executor_arc.train_batch(
                            ham,
                            spam,
                            do_rescore,
                            do_rebuild,
                            progress_fn,
                            worker_cancelled,
                        )
                    }));

                    // Send completion/error to GTK thread
                    match result {
                        Ok(Ok(train_result)) => {
                            let _ = sender.send(TrainingMessage::Complete(train_result));
                        }
                        Ok(Err(err_msg)) => {
                            let _ = sender.send(TrainingMessage::Error(err_msg));
                        }
                        Err(_panic) => {
                            let _ = sender.send(TrainingMessage::Error(
                                "Training thread panicked unexpectedly".to_string(),
                            ));
                        }
                    }
                });

                // ── Poll the channel from GTK main loop ──────────────────────
                let dialog_for_poll = progress_dialog.clone();
                glib::timeout_add_local(
                    std::time::Duration::from_millis(50),
                    move || {
                        // Drain all available messages from the channel
                        loop {
                            match receiver.try_recv() {
                                Ok(TrainingMessage::Progress {
                                    folder_name,
                                    current,
                                    total,
                                }) => {
                                    if total > 0 {
                                        dialog_for_poll.set_progress(
                                            f64::from(current) / f64::from(total),
                                        );
                                    }
                                    dialog_for_poll.set_status(&format!(
                                        "Training: {} ({}/{})",
                                        folder_name, current, total
                                    ));
                                }
                                Ok(TrainingMessage::Complete(result)) => {
                                    let msg = format!(
                                        "Training complete!\n\n\
                                         Messages processed: {}\n\
                                         New entries trained: {}\n\
                                         Errors: {}",
                                        result.total_processed,
                                        result.new_entries,
                                        result.errors
                                    );
                                    message_boxes::report_information(
                                        None,
                                        "SpamBayes",
                                        &msg,
                                    );
                                    dialog_for_poll.close();
                                    return glib::ControlFlow::Break;
                                }
                                Ok(TrainingMessage::Error(err)) => {
                                    let err_msg = format!("Training failed:\n\n{}", err);
                                    message_boxes::report_error(
                                        None,
                                        "SpamBayes",
                                        &err_msg,
                                    );
                                    dialog_for_poll.close();
                                    return glib::ControlFlow::Break;
                                }
                                Err(crossbeam_channel::TryRecvError::Empty) => {
                                    // No messages yet — continue polling
                                    break;
                                }
                                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                    // Worker thread dropped sender without Complete/Error
                                    message_boxes::report_error(
                                        None,
                                        "SpamBayes",
                                        "Training ended unexpectedly (worker disconnected).",
                                    );
                                    dialog_for_poll.close();
                                    return glib::ControlFlow::Break;
                                }
                            }
                        }
                        glib::ControlFlow::Continue
                    },
                );
            });
        }

        Self {
            container,
            folder_provider,
            training_executor,
            ham_folder_ids,
            spam_folder_ids,
            ham_folder_entry,
            ham_folder_browse_btn,
            spam_folder_entry,
            spam_folder_browse_btn,
            rescore_check,
            rebuild_check,
            start_training_btn,
            train_recovered_spam_check,
            not_spam_action_combo,
            train_manual_spam_check,
            spam_action_combo,
        }
    }

    /// Set the training executor used by the Start Training button.
    ///
    /// This is called after construction to provide the actual training
    /// backend. If not set, clicking Start Training will show an error.
    pub fn set_training_executor(&self, executor: Arc<dyn TrainingExecutor>) {
        *self.training_executor.borrow_mut() = Some(executor);
    }

    /// Resolve stored folder IDs to display names using the folder provider
    /// and update the entry fields. Call after construction when the provider is ready.
    pub fn resolve_folder_names(&self, provider: &dyn FolderProvider) {
        // Resolve ham training folder names
        let ham_ids = self.ham_folder_ids.borrow();
        if !ham_ids.is_empty() {
            let names = FolderBrowserDialog::resolve_folder_names(provider, &ham_ids);
            self.ham_folder_entry.set_text(&names.join("; "));
        }
        drop(ham_ids);

        // Resolve spam training folder names
        let spam_ids = self.spam_folder_ids.borrow();
        if !spam_ids.is_empty() {
            let names = FolderBrowserDialog::resolve_folder_names(provider, &spam_ids);
            self.spam_folder_entry.set_text(&names.join("; "));
        }
    }
}
