//! Filtering tab — thresholds, folder destinations, and filter actions.
//!
//! Provides controls for:
//! - Watched folder selection (multi-select)
//! - Certain spam threshold + action + destination folder
//! - Possible spam threshold + action + destination folder
//! - Certain good action + destination folder
//! - Spam auto-cleanup (enable + days)
//!
//! **Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5, 2.8**

use gtk4::prelude::*;
use gtk4::{
    Adjustment, Align, Box as GtkBox, Button, CheckButton, ComboBoxText, Entry,
    EventControllerFocus, Frame, Label, Orientation, Scale, ScrolledWindow, SpinButton,
};

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use spambayes_config::{FilterAction, FolderId};

use crate::gui::folder_browser::{FolderBrowserDialog, FolderProvider, SelectionMode};
use crate::manager_dlg::ManagerState;

// ─── Helper: map FilterAction to ComboBoxText index ──────────────────────────

fn action_to_index(action: &FilterAction) -> u32 {
    match action {
        FilterAction::Move => 0,
        FilterAction::Copy => 1,
        FilterAction::Untouched => 2,
    }
}

// ─── FilteringTab ────────────────────────────────────────────────────────────

/// The Filtering tab content.
///
/// Contains threshold sliders, action combo boxes, folder selectors,
/// mark-as-read checkboxes, and auto-cleanup controls.
///
/// **Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5, 2.8**
pub struct FilteringTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Folder Provider ─────────────────────────────────────────────────
    /// Provider for folder hierarchy data (used by browse buttons).
    pub folder_provider: Rc<dyn FolderProvider>,

    // ─── Stored Folder IDs (for save) ────────────────────────────────────
    /// Selected watched folder IDs (multi-select).
    pub watched_folder_ids: Rc<RefCell<Vec<FolderId>>>,
    /// Selected spam destination folder ID (single-select).
    pub spam_folder_id: Rc<RefCell<Option<FolderId>>>,
    /// Selected unsure destination folder ID (single-select).
    pub unsure_folder_id: Rc<RefCell<Option<FolderId>>>,
    /// Selected ham destination folder ID (single-select).
    pub ham_folder_id: Rc<RefCell<Option<FolderId>>>,

    // ─── Watched Folders (Req 2.1) ───────────────────────────────────────
    /// Label showing currently selected watched folders.
    pub watched_folders_label: Label,
    /// Browse button for watched folders (multi-select).
    pub watched_folders_browse_btn: Button,

    // ─── Certain Spam (Req 2.2) ──────────────────────────────────────────
    /// Threshold slider (0–100).
    pub spam_scale: Scale,
    /// Entry showing threshold value with 1 decimal place.
    pub spam_entry: Entry,
    /// Validation label (hidden by default).
    pub spam_validation_label: Label,
    /// Action combo: Move / Copy / Untouched.
    pub spam_action_combo: ComboBoxText,
    /// Destination folder entry.
    pub spam_folder_entry: Entry,
    /// Browse button for spam folder.
    pub spam_folder_browse_btn: Button,
    /// Mark as read checkbox.
    pub spam_mark_read: CheckButton,

    // ─── Possible Spam (Req 2.3) ─────────────────────────────────────────
    /// Threshold slider (0–100).
    pub unsure_scale: Scale,
    /// Entry showing threshold value with 1 decimal place.
    pub unsure_entry: Entry,
    /// Validation label (hidden by default).
    pub unsure_validation_label: Label,
    /// Action combo: Move / Copy / Untouched.
    pub unsure_action_combo: ComboBoxText,
    /// Destination folder entry.
    pub unsure_folder_entry: Entry,
    /// Browse button for unsure folder.
    pub unsure_folder_browse_btn: Button,
    /// Mark as read checkbox.
    pub unsure_mark_read: CheckButton,

    // ─── Certain Good (Req 2.4) ──────────────────────────────────────────
    /// Action combo: Move / Copy / Untouched.
    pub ham_action_combo: ComboBoxText,
    /// Destination folder entry.
    pub ham_folder_entry: Entry,
    /// Browse button for ham folder.
    pub ham_folder_browse_btn: Button,
    /// Mark as read checkbox.
    pub ham_mark_read: CheckButton,

    // ─── Spam Auto-Cleanup (Req 2.5) ─────────────────────────────────────
    /// Enable auto-cleanup checkbox.
    pub cleanup_enabled: CheckButton,
    /// Days spin button (1–365).
    pub cleanup_days_spin: SpinButton,
}

impl FilteringTab {
    /// Build the Filtering tab widget tree.
    ///
    /// # Arguments
    ///
    /// * `state` – Current manager state (thresholds, actions, folder IDs)
    /// * `folder_provider` – Provider for folder hierarchy data (used by browse buttons)
    ///
    /// **Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5, 2.8**
    #[must_use]
    pub fn new(state: &ManagerState, folder_provider: Rc<dyn FolderProvider>) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Watched Folders section (Req 2.1) ────────────────────────
        let watched_frame = Frame::new(Some("Watched Folders"));
        let watched_box = GtkBox::new(Orientation::Vertical, 6);
        watched_box.set_margin_top(8);
        watched_box.set_margin_bottom(8);
        watched_box.set_margin_start(12);
        watched_box.set_margin_end(12);

        let watched_desc = Label::new(Some(
            "Folders to monitor for incoming mail that should be classified:",
        ));
        watched_desc.set_halign(Align::Start);
        watched_desc.set_wrap(true);
        watched_box.append(&watched_desc);

        let watched_row = GtkBox::new(Orientation::Horizontal, 8);
        watched_row.set_valign(Align::Center);

        let watched_folders_label = Label::new(Some(&Self::format_watched_folders(state)));
        watched_folders_label.set_halign(Align::Start);
        watched_folders_label.set_hexpand(true);

        let watched_folders_browse_btn = Button::with_label("Browse...");

        watched_row.append(&watched_folders_label);
        watched_row.append(&watched_folders_browse_btn);
        watched_box.append(&watched_row);

        watched_frame.set_child(Some(&watched_box));
        content_box.append(&watched_frame);

        // ─── 2. Certain Spam section (Req 2.2) ──────────────────────────
        let spam_frame = Frame::new(Some("Certain Spam"));
        let spam_box = GtkBox::new(Orientation::Vertical, 6);
        spam_box.set_margin_top(8);
        spam_box.set_margin_bottom(8);
        spam_box.set_margin_start(12);
        spam_box.set_margin_end(12);

        // Threshold row: Scale + Entry
        let spam_threshold_label = Label::new(Some("Spam threshold (%):"));
        spam_threshold_label.set_halign(Align::Start);
        spam_box.append(&spam_threshold_label);

        let spam_threshold_row = GtkBox::new(Orientation::Horizontal, 8);
        spam_threshold_row.set_valign(Align::Center);

        let spam_adj = Adjustment::new(state.spam_threshold, 0.0, 100.0, 0.1, 1.0, 0.0);
        let spam_scale = Scale::new(Orientation::Horizontal, Some(&spam_adj));
        spam_scale.set_hexpand(true);
        spam_scale.set_digits(1);

        let spam_entry = Entry::new();
        spam_entry.set_width_chars(6);
        spam_entry.set_text(&format!("{:.1}", state.spam_threshold));

        spam_threshold_row.append(&spam_scale);
        spam_threshold_row.append(&spam_entry);
        spam_box.append(&spam_threshold_row);

        // Validation label (hidden by default)
        let spam_validation_label = Label::new(None);
        spam_validation_label.set_halign(Align::Start);
        spam_validation_label.set_visible(false);
        spam_validation_label.add_css_class("error");
        spam_box.append(&spam_validation_label);

        // Action row: ComboBox + Folder Entry + Browse
        let spam_action_row = GtkBox::new(Orientation::Horizontal, 8);
        spam_action_row.set_valign(Align::Center);

        let spam_action_label = Label::new(Some("Action:"));
        spam_action_label.set_halign(Align::Start);

        let spam_action_combo = ComboBoxText::new();
        spam_action_combo.append_text("Moved to specified folder");
        spam_action_combo.append_text("Copied to specified folder");
        spam_action_combo.append_text("Untouched");
        spam_action_combo.set_active(Some(action_to_index(&state.spam_action)));

        spam_action_row.append(&spam_action_label);
        spam_action_row.append(&spam_action_combo);
        spam_box.append(&spam_action_row);

        let spam_folder_row = GtkBox::new(Orientation::Horizontal, 8);
        spam_folder_row.set_valign(Align::Center);

        let spam_folder_label = Label::new(Some("Folder:"));
        spam_folder_label.set_halign(Align::Start);

        let spam_folder_entry = Entry::new();
        spam_folder_entry.set_hexpand(true);
        spam_folder_entry.set_placeholder_text(Some("Select a folder..."));
        if state.spam_folder_id.is_some() {
            spam_folder_entry.set_text("(configured)");
        }

        let spam_folder_browse_btn = Button::with_label("Browse...");

        spam_folder_row.append(&spam_folder_label);
        spam_folder_row.append(&spam_folder_entry);
        spam_folder_row.append(&spam_folder_browse_btn);
        spam_box.append(&spam_folder_row);

        // Disable folder controls when action is "Untouched"
        let folder_sensitive = state.spam_action != FilterAction::Untouched;
        spam_folder_entry.set_sensitive(folder_sensitive);
        spam_folder_browse_btn.set_sensitive(folder_sensitive);

        // Wire combo change → folder sensitivity
        let spam_folder_entry_clone = spam_folder_entry.clone();
        let spam_folder_browse_clone = spam_folder_browse_btn.clone();
        spam_action_combo.connect_changed(move |combo| {
            let sensitive = combo.active() != Some(2); // 2 = Untouched
            spam_folder_entry_clone.set_sensitive(sensitive);
            spam_folder_browse_clone.set_sensitive(sensitive);
        });

        // Mark as read checkbox
        // TODO: Wire to config when ManagerState is extended with spam_mark_as_read
        let spam_mark_read = CheckButton::with_label("Mark spam as read");
        spam_mark_read.set_active(false); // Default: not marked as read
        spam_box.append(&spam_mark_read);

        spam_frame.set_child(Some(&spam_box));
        content_box.append(&spam_frame);

        // ─── 3. Possible Spam section (Req 2.3) ─────────────────────────
        let unsure_frame = Frame::new(Some("Possible Spam"));
        let unsure_box = GtkBox::new(Orientation::Vertical, 6);
        unsure_box.set_margin_top(8);
        unsure_box.set_margin_bottom(8);
        unsure_box.set_margin_start(12);
        unsure_box.set_margin_end(12);

        // Threshold row: Scale + Entry
        let unsure_threshold_label = Label::new(Some("Unsure threshold (%):"));
        unsure_threshold_label.set_halign(Align::Start);
        unsure_box.append(&unsure_threshold_label);

        let unsure_threshold_row = GtkBox::new(Orientation::Horizontal, 8);
        unsure_threshold_row.set_valign(Align::Center);

        let unsure_adj = Adjustment::new(state.unsure_threshold, 0.0, 100.0, 0.1, 1.0, 0.0);
        let unsure_scale = Scale::new(Orientation::Horizontal, Some(&unsure_adj));
        unsure_scale.set_hexpand(true);
        unsure_scale.set_digits(1);

        let unsure_entry = Entry::new();
        unsure_entry.set_width_chars(6);
        unsure_entry.set_text(&format!("{:.1}", state.unsure_threshold));

        unsure_threshold_row.append(&unsure_scale);
        unsure_threshold_row.append(&unsure_entry);
        unsure_box.append(&unsure_threshold_row);

        // Validation label (hidden by default)
        let unsure_validation_label = Label::new(None);
        unsure_validation_label.set_halign(Align::Start);
        unsure_validation_label.set_visible(false);
        unsure_validation_label.add_css_class("error");
        unsure_box.append(&unsure_validation_label);

        // Action row: ComboBox + Folder Entry + Browse
        let unsure_action_row = GtkBox::new(Orientation::Horizontal, 8);
        unsure_action_row.set_valign(Align::Center);

        let unsure_action_label = Label::new(Some("Action:"));
        unsure_action_label.set_halign(Align::Start);

        let unsure_action_combo = ComboBoxText::new();
        unsure_action_combo.append_text("Moved to specified folder");
        unsure_action_combo.append_text("Copied to specified folder");
        unsure_action_combo.append_text("Untouched");
        unsure_action_combo.set_active(Some(action_to_index(&state.unsure_action)));

        unsure_action_row.append(&unsure_action_label);
        unsure_action_row.append(&unsure_action_combo);
        unsure_box.append(&unsure_action_row);

        let unsure_folder_row = GtkBox::new(Orientation::Horizontal, 8);
        unsure_folder_row.set_valign(Align::Center);

        let unsure_folder_label = Label::new(Some("Folder:"));
        unsure_folder_label.set_halign(Align::Start);

        let unsure_folder_entry = Entry::new();
        unsure_folder_entry.set_hexpand(true);
        unsure_folder_entry.set_placeholder_text(Some("Select a folder..."));
        if state.unsure_folder_id.is_some() {
            unsure_folder_entry.set_text("(configured)");
        }

        let unsure_folder_browse_btn = Button::with_label("Browse...");

        unsure_folder_row.append(&unsure_folder_label);
        unsure_folder_row.append(&unsure_folder_entry);
        unsure_folder_row.append(&unsure_folder_browse_btn);
        unsure_box.append(&unsure_folder_row);

        // Disable folder controls when action is "Untouched"
        let folder_sensitive = state.unsure_action != FilterAction::Untouched;
        unsure_folder_entry.set_sensitive(folder_sensitive);
        unsure_folder_browse_btn.set_sensitive(folder_sensitive);

        // Wire combo change → folder sensitivity
        let unsure_folder_entry_clone = unsure_folder_entry.clone();
        let unsure_folder_browse_clone = unsure_folder_browse_btn.clone();
        unsure_action_combo.connect_changed(move |combo| {
            let sensitive = combo.active() != Some(2); // 2 = Untouched
            unsure_folder_entry_clone.set_sensitive(sensitive);
            unsure_folder_browse_clone.set_sensitive(sensitive);
        });

        // Mark as read checkbox
        // TODO: Wire to config when ManagerState is extended with unsure_mark_as_read
        let unsure_mark_read = CheckButton::with_label("Mark possible spam as read");
        unsure_mark_read.set_active(false); // Default: not marked as read
        unsure_box.append(&unsure_mark_read);

        unsure_frame.set_child(Some(&unsure_box));
        content_box.append(&unsure_frame);

        // ─── 4. Certain Good section (Req 2.4) ──────────────────────────
        let ham_frame = Frame::new(Some("Certain Good"));
        let ham_box = GtkBox::new(Orientation::Vertical, 6);
        ham_box.set_margin_top(8);
        ham_box.set_margin_bottom(8);
        ham_box.set_margin_start(12);
        ham_box.set_margin_end(12);

        // Action row: ComboBox
        let ham_action_row = GtkBox::new(Orientation::Horizontal, 8);
        ham_action_row.set_valign(Align::Center);

        let ham_action_label = Label::new(Some("Action:"));
        ham_action_label.set_halign(Align::Start);

        let ham_action_combo = ComboBoxText::new();
        ham_action_combo.append_text("Moved to specified folder");
        ham_action_combo.append_text("Copied to specified folder");
        ham_action_combo.append_text("Untouched");
        ham_action_combo.set_active(Some(action_to_index(&state.ham_action)));

        ham_action_row.append(&ham_action_label);
        ham_action_row.append(&ham_action_combo);
        ham_box.append(&ham_action_row);

        // Folder row: Entry + Browse
        let ham_folder_row = GtkBox::new(Orientation::Horizontal, 8);
        ham_folder_row.set_valign(Align::Center);

        let ham_folder_label = Label::new(Some("Folder:"));
        ham_folder_label.set_halign(Align::Start);

        let ham_folder_entry = Entry::new();
        ham_folder_entry.set_hexpand(true);
        ham_folder_entry.set_placeholder_text(Some("Select a folder..."));

        let ham_folder_browse_btn = Button::with_label("Browse...");

        ham_folder_row.append(&ham_folder_label);
        ham_folder_row.append(&ham_folder_entry);
        ham_folder_row.append(&ham_folder_browse_btn);
        ham_box.append(&ham_folder_row);

        // Disable folder controls when action is "Untouched"
        let folder_sensitive = state.ham_action != FilterAction::Untouched;
        ham_folder_entry.set_sensitive(folder_sensitive);
        ham_folder_browse_btn.set_sensitive(folder_sensitive);

        // Wire combo change → folder sensitivity
        let ham_folder_entry_clone = ham_folder_entry.clone();
        let ham_folder_browse_clone = ham_folder_browse_btn.clone();
        ham_action_combo.connect_changed(move |combo| {
            let sensitive = combo.active() != Some(2); // 2 = Untouched
            ham_folder_entry_clone.set_sensitive(sensitive);
            ham_folder_browse_clone.set_sensitive(sensitive);
        });

        // Mark as read checkbox
        // TODO: Wire to config when ManagerState is extended with ham_mark_as_read
        let ham_mark_read = CheckButton::with_label("Mark good messages as read");
        ham_mark_read.set_active(false); // Default: not marked as read
        ham_box.append(&ham_mark_read);

        ham_frame.set_child(Some(&ham_box));
        content_box.append(&ham_frame);

        // ─── 5. Spam Auto-Cleanup section (Req 2.5) ─────────────────────
        let cleanup_frame = Frame::new(Some("Spam Auto-Cleanup"));
        let cleanup_box = GtkBox::new(Orientation::Vertical, 6);
        cleanup_box.set_margin_top(8);
        cleanup_box.set_margin_bottom(8);
        cleanup_box.set_margin_start(12);
        cleanup_box.set_margin_end(12);

        // TODO: Wire to config when ManagerState is extended with spam_auto_cleanup fields
        let cleanup_enabled = CheckButton::with_label(
            "Automatically delete spam messages older than:",
        );
        cleanup_enabled.set_active(false); // Default: disabled

        let cleanup_days_row = GtkBox::new(Orientation::Horizontal, 8);
        cleanup_days_row.set_valign(Align::Center);

        let cleanup_adj = Adjustment::new(30.0, 1.0, 365.0, 1.0, 7.0, 0.0);
        let cleanup_days_spin = SpinButton::new(Some(&cleanup_adj), 1.0, 0);
        cleanup_days_spin.set_sensitive(false); // Disabled until checkbox is checked

        let cleanup_days_label = Label::new(Some("days"));
        cleanup_days_label.set_halign(Align::Start);

        cleanup_days_row.append(&cleanup_days_spin);
        cleanup_days_row.append(&cleanup_days_label);

        cleanup_box.append(&cleanup_enabled);
        cleanup_box.append(&cleanup_days_row);

        // Wire checkbox → spinbutton sensitivity
        let cleanup_days_spin_clone = cleanup_days_spin.clone();
        cleanup_enabled.connect_toggled(move |checkbox| {
            cleanup_days_spin_clone.set_sensitive(checkbox.is_active());
        });

        cleanup_frame.set_child(Some(&cleanup_box));
        content_box.append(&cleanup_frame);

        // ─── Initialize stored folder ID state ─────────────────────────────
        let watched_folder_ids = Rc::new(RefCell::new(state.watch_folder_ids.clone()));
        let spam_folder_id_rc = Rc::new(RefCell::new(state.spam_folder_id.clone()));
        let unsure_folder_id_rc = Rc::new(RefCell::new(state.unsure_folder_id.clone()));
        let ham_folder_id_rc: Rc<RefCell<Option<FolderId>>> = Rc::new(RefCell::new(None));

        // ─── Wire Browse buttons to FolderBrowserDialog ──────────────────

        // Watched Folders Browse — multi-select mode (Req 2.1)
        {
            let provider = Rc::clone(&folder_provider);
            let ids = Rc::clone(&watched_folder_ids);
            let label = watched_folders_label.clone();
            watched_folders_browse_btn.connect_clicked(move |btn| {
                let parent_window = btn.root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let current_ids = ids.borrow().clone();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    provider.as_ref(),
                    SelectionMode::Multi,
                    &current_ids,
                );
                if let Some(selections) = dialog.run() {
                    let new_ids: Vec<FolderId> = selections.iter()
                        .map(|(id, _name)| id.clone())
                        .collect();
                    let count = new_ids.len();
                    *ids.borrow_mut() = new_ids;
                    // Update the label display
                    if count == 0 {
                        label.set_text("(no folders selected)");
                    } else {
                        label.set_text(&format!(
                            "{} folder{} selected",
                            count,
                            if count == 1 { "" } else { "s" }
                        ));
                    }
                }
                // On Cancel: do nothing (keep existing selection)
            });
        }

        // Spam Folder Browse — single-select mode (Req 2.2)
        {
            let provider = Rc::clone(&folder_provider);
            let id_rc = Rc::clone(&spam_folder_id_rc);
            let entry = spam_folder_entry.clone();
            spam_folder_browse_btn.connect_clicked(move |btn| {
                let parent_window = btn.root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let preselected: Vec<FolderId> = id_rc.borrow()
                    .iter()
                    .cloned()
                    .collect();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    provider.as_ref(),
                    SelectionMode::Single,
                    &preselected,
                );
                if let Some(selections) = dialog.run() {
                    if let Some((folder_id, display_name)) = selections.into_iter().next() {
                        *id_rc.borrow_mut() = Some(folder_id);
                        entry.set_text(&display_name);
                    }
                }
                // On Cancel: do nothing
            });
        }

        // Unsure Folder Browse — single-select mode (Req 2.3)
        {
            let provider = Rc::clone(&folder_provider);
            let id_rc = Rc::clone(&unsure_folder_id_rc);
            let entry = unsure_folder_entry.clone();
            unsure_folder_browse_btn.connect_clicked(move |btn| {
                let parent_window = btn.root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let preselected: Vec<FolderId> = id_rc.borrow()
                    .iter()
                    .cloned()
                    .collect();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    provider.as_ref(),
                    SelectionMode::Single,
                    &preselected,
                );
                if let Some(selections) = dialog.run() {
                    if let Some((folder_id, display_name)) = selections.into_iter().next() {
                        *id_rc.borrow_mut() = Some(folder_id);
                        entry.set_text(&display_name);
                    }
                }
                // On Cancel: do nothing
            });
        }

        // Ham (Good) Folder Browse — single-select mode (Req 2.4)
        {
            let provider = Rc::clone(&folder_provider);
            let id_rc = Rc::clone(&ham_folder_id_rc);
            let entry = ham_folder_entry.clone();
            ham_folder_browse_btn.connect_clicked(move |btn| {
                let parent_window = btn.root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok());
                let preselected: Vec<FolderId> = id_rc.borrow()
                    .iter()
                    .cloned()
                    .collect();
                let dialog = FolderBrowserDialog::new(
                    parent_window.as_ref(),
                    provider.as_ref(),
                    SelectionMode::Single,
                    &preselected,
                );
                if let Some(selections) = dialog.run() {
                    if let Some((folder_id, display_name)) = selections.into_iter().next() {
                        *id_rc.borrow_mut() = Some(folder_id);
                        entry.set_text(&display_name);
                    }
                }
                // On Cancel: do nothing
            });
        }

        // ─── Threshold slider ↔ entry synchronization (Req 2.6, 2.7) ────

        // Guard flags to prevent infinite update loops between Scale and Entry.
        let spam_updating = Rc::new(Cell::new(false));
        let unsure_updating = Rc::new(Cell::new(false));

        // Helper: validate thresholds and show/hide validation labels.
        // Returns true if valid.
        fn cross_validate(
            spam_scale: &Scale,
            unsure_scale: &Scale,
            spam_validation_label: &Label,
            unsure_validation_label: &Label,
        ) {
            let spam_val = spam_scale.value();
            let unsure_val = unsure_scale.value();

            if spam_val <= unsure_val {
                spam_validation_label.set_text(
                    "Spam threshold must be greater than unsure threshold",
                );
                spam_validation_label.set_visible(true);
                unsure_validation_label.set_text(
                    "Unsure threshold must be less than spam threshold",
                );
                unsure_validation_label.set_visible(true);
            } else {
                spam_validation_label.set_visible(false);
                unsure_validation_label.set_visible(false);
            }
        }

        // --- Spam Scale → Entry ---
        {
            let entry = spam_entry.clone();
            let updating = spam_updating.clone();
            let s_scale = spam_scale.clone();
            let u_scale = unsure_scale.clone();
            let s_label = spam_validation_label.clone();
            let u_label = unsure_validation_label.clone();
            spam_scale.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                updating.set(true);
                entry.set_text(&format!("{:.1}", scale.value()));
                cross_validate(&s_scale, &u_scale, &s_label, &u_label);
                updating.set(false);
            });
        }

        // --- Unsure Scale → Entry ---
        {
            let entry = unsure_entry.clone();
            let updating = unsure_updating.clone();
            let s_scale = spam_scale.clone();
            let u_scale = unsure_scale.clone();
            let s_label = spam_validation_label.clone();
            let u_label = unsure_validation_label.clone();
            unsure_scale.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                updating.set(true);
                entry.set_text(&format!("{:.1}", scale.value()));
                cross_validate(&s_scale, &u_scale, &s_label, &u_label);
                updating.set(false);
            });
        }

        // --- Spam Entry → Scale (on activate / Enter key) ---
        {
            let scale = spam_scale.clone();
            let entry = spam_entry.clone();
            let updating = spam_updating.clone();
            let s_scale = spam_scale.clone();
            let u_scale = unsure_scale.clone();
            let s_label = spam_validation_label.clone();
            let u_label = unsure_validation_label.clone();
            spam_entry.connect_activate(move |e| {
                if updating.get() {
                    return;
                }
                updating.set(true);
                if let Ok(val) = e.text().parse::<f64>() {
                    let clamped = val.clamp(0.0, 100.0);
                    scale.set_value(clamped);
                    // Update entry to show clamped value
                    entry.set_text(&format!("{:.1}", clamped));
                }
                cross_validate(&s_scale, &u_scale, &s_label, &u_label);
                updating.set(false);
            });
        }

        // --- Spam Entry → Scale (on focus-out) ---
        {
            let scale = spam_scale.clone();
            let entry = spam_entry.clone();
            let updating = spam_updating.clone();
            let s_scale = spam_scale.clone();
            let u_scale = unsure_scale.clone();
            let s_label = spam_validation_label.clone();
            let u_label = unsure_validation_label.clone();
            let focus_controller = EventControllerFocus::new();
            focus_controller.connect_leave(move |_| {
                if updating.get() {
                    return;
                }
                updating.set(true);
                if let Ok(val) = entry.text().parse::<f64>() {
                    let clamped = val.clamp(0.0, 100.0);
                    scale.set_value(clamped);
                    entry.set_text(&format!("{:.1}", clamped));
                }
                cross_validate(&s_scale, &u_scale, &s_label, &u_label);
                updating.set(false);
            });
            spam_entry.add_controller(focus_controller);
        }

        // --- Unsure Entry → Scale (on activate / Enter key) ---
        {
            let scale = unsure_scale.clone();
            let entry = unsure_entry.clone();
            let updating = unsure_updating.clone();
            let s_scale = spam_scale.clone();
            let u_scale = unsure_scale.clone();
            let s_label = spam_validation_label.clone();
            let u_label = unsure_validation_label.clone();
            unsure_entry.connect_activate(move |e| {
                if updating.get() {
                    return;
                }
                updating.set(true);
                if let Ok(val) = e.text().parse::<f64>() {
                    let clamped = val.clamp(0.0, 100.0);
                    scale.set_value(clamped);
                    entry.set_text(&format!("{:.1}", clamped));
                }
                cross_validate(&s_scale, &u_scale, &s_label, &u_label);
                updating.set(false);
            });
        }

        // --- Unsure Entry → Scale (on focus-out) ---
        {
            let scale = unsure_scale.clone();
            let entry = unsure_entry.clone();
            let updating = unsure_updating.clone();
            let s_scale = spam_scale.clone();
            let u_scale = unsure_scale.clone();
            let s_label = spam_validation_label.clone();
            let u_label = unsure_validation_label.clone();
            let focus_controller = EventControllerFocus::new();
            focus_controller.connect_leave(move |_| {
                if updating.get() {
                    return;
                }
                updating.set(true);
                if let Ok(val) = entry.text().parse::<f64>() {
                    let clamped = val.clamp(0.0, 100.0);
                    scale.set_value(clamped);
                    entry.set_text(&format!("{:.1}", clamped));
                }
                cross_validate(&s_scale, &u_scale, &s_label, &u_label);
                updating.set(false);
            });
            unsure_entry.add_controller(focus_controller);
        }

        // ─── ScrolledWindow wrapper (Req 2.8) ───────────────────────────
        let container = ScrolledWindow::new();
        container.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        container.set_child(Some(&content_box));
        container.set_vexpand(true);
        container.set_hexpand(true);

        Self {
            container,
            folder_provider,
            watched_folder_ids,
            spam_folder_id: spam_folder_id_rc,
            unsure_folder_id: unsure_folder_id_rc,
            ham_folder_id: ham_folder_id_rc,
            watched_folders_label,
            watched_folders_browse_btn,
            spam_scale,
            spam_entry,
            spam_validation_label,
            spam_action_combo,
            spam_folder_entry,
            spam_folder_browse_btn,
            spam_mark_read,
            unsure_scale,
            unsure_entry,
            unsure_validation_label,
            unsure_action_combo,
            unsure_folder_entry,
            unsure_folder_browse_btn,
            unsure_mark_read,
            ham_action_combo,
            ham_folder_entry,
            ham_folder_browse_btn,
            ham_mark_read,
            cleanup_enabled,
            cleanup_days_spin,
        }
    }

    /// Validate tab values. Returns `Ok(())` or an error message.
    ///
    /// Checks that spam_threshold > unsure_threshold and both are in [0, 100].
    ///
    /// **Validates: Requirements 2.6, 2.7**
    pub fn validate(&self) -> Result<(), String> {
        let spam_val = self.spam_scale.value();
        let unsure_val = self.unsure_scale.value();

        // Check range [0, 100] (Req 2.7)
        if !(0.0..=100.0).contains(&spam_val) {
            return Err(format!(
                "Spam threshold ({:.1}) must be between 0 and 100",
                spam_val
            ));
        }
        if !(0.0..=100.0).contains(&unsure_val) {
            return Err(format!(
                "Unsure threshold ({:.1}) must be between 0 and 100",
                unsure_val
            ));
        }

        // Check spam_threshold > unsure_threshold (Req 2.6)
        if spam_val <= unsure_val {
            return Err(format!(
                "Spam threshold ({:.1}) must be greater than unsure threshold ({:.1})",
                spam_val, unsure_val
            ));
        }

        Ok(())
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Format watched folder display text from state.
    fn format_watched_folders(state: &ManagerState) -> String {
        if state.watch_folder_ids.is_empty() {
            "(no folders selected)".to_string()
        } else {
            format!(
                "{} folder{} selected",
                state.watch_folder_ids.len(),
                if state.watch_folder_ids.len() == 1 { "" } else { "s" }
            )
        }
    }
}
