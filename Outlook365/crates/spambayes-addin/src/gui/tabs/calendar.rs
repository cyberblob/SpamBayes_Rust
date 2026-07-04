//! Calendar tab — calendar invitation filtering settings.
//!
//! Provides controls for:
//! - Enable/disable calendar invitation filtering (with BETA warning)
//! - "When classified as spam" combo box (Prompt/Trash/Move)
//! - Dynamic description labels for each action option
//!
//! **Validates: Requirements 6.1, 6.2, 6.3**

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, CheckButton, ComboBoxText, Frame, Label, Orientation, ScrolledWindow,
};

use spambayes_config::{CalendarConfig, CalendarSpamAction};

// ─── CalendarTab ─────────────────────────────────────────────────────────────

/// The Calendar tab content.
///
/// Contains an enable checkbox with BETA warning, a spam action combo box,
/// and a dynamic description label. When the enable checkbox is unchecked,
/// the action section is greyed out (insensitive).
///
/// **Validates: Requirements 6.1, 6.2, 6.3**
pub struct CalendarTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Enable checkbox (Req 6.1) ───────────────────────────────────────
    /// "Allow filtering of calendar invitations and meeting requests" checkbox.
    pub enable_check: CheckButton,

    // ─── Spam action combo (Req 6.2) ────────────────────────────────────
    /// "When classified as spam" combo box (Prompt/Trash/Move).
    pub spam_action_combo: ComboBoxText,

    // ─── Description label (Req 6.3) ────────────────────────────────────
    /// Dynamic description label that updates based on combo selection.
    pub description_label: Label,
}

impl CalendarTab {
    /// Build the Calendar tab widget tree.
    ///
    /// # Arguments
    ///
    /// * `config` – Current calendar configuration values
    ///
    /// **Validates: Requirements 6.1, 6.2, 6.3**
    #[must_use]
    pub fn new(config: &CalendarConfig) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Enable checkbox + BETA warning (Req 6.1) ────────────────
        let enable_box = GtkBox::new(Orientation::Vertical, 4);

        let enable_check = CheckButton::with_label(
            "Allow filtering of calendar invitations and meeting requests",
        );
        enable_check.set_active(config.calendar_filtering_enabled);
        enable_check.set_tooltip_text(Some(
            "Enable filtering of calendar invitations and meeting requests classified as spam. \
             Warning: This feature is still in beta testing.",
        ));
        enable_box.append(&enable_check);

        let beta_label = Label::new(Some("(Warning this is BETA!!)"));
        beta_label.set_halign(Align::Start);
        beta_label.set_margin_start(24);
        beta_label.add_css_class("dim-label");
        enable_box.append(&beta_label);

        content_box.append(&enable_box);

        // ─── 2. Spam action section (Req 6.2, 6.3) ──────────────────────
        let action_frame = Frame::new(Some("When classified as spam:"));
        let action_box = GtkBox::new(Orientation::Vertical, 8);
        action_box.set_margin_top(8);
        action_box.set_margin_bottom(8);
        action_box.set_margin_start(12);
        action_box.set_margin_end(12);

        // Combo box row
        let combo_row = GtkBox::new(Orientation::Horizontal, 8);
        combo_row.set_valign(Align::Center);

        let combo_label = Label::new(Some("Action:"));
        combo_label.set_halign(Align::Start);

        let spam_action_combo = ComboBoxText::new();
        spam_action_combo.append_text("Prompt");
        spam_action_combo.append_text("Trash");
        spam_action_combo.append_text("Move");

        // Set active based on config
        let active_index = match config.calendar_spam_action {
            CalendarSpamAction::Prompt => 0,
            CalendarSpamAction::Trash => 1,
            CalendarSpamAction::Move => 2,
        };
        spam_action_combo.set_active(Some(active_index));
        spam_action_combo.set_tooltip_text(Some(
            "Choose what to do with calendar invitations classified as spam: \
             Prompt (ask user), Trash (delete), or Move (move to spam folder).",
        ));

        combo_row.append(&combo_label);
        combo_row.append(&spam_action_combo);
        action_box.append(&combo_row);

        // Description label (Req 6.3)
        let description_label = Label::new(None);
        description_label.set_halign(Align::Start);
        description_label.set_wrap(true);
        description_label.set_margin_top(4);
        Self::update_description(&description_label, &config.calendar_spam_action);
        action_box.append(&description_label);

        action_frame.set_child(Some(&action_box));
        content_box.append(&action_frame);

        // ─── 3. Enable checkbox → sensitivity wiring ─────────────────────
        let controls_sensitive = config.calendar_filtering_enabled;
        action_frame.set_sensitive(controls_sensitive);

        let action_frame_clone = action_frame.clone();
        enable_check.connect_toggled(move |checkbox| {
            action_frame_clone.set_sensitive(checkbox.is_active());
        });

        // ─── 4. Combo box → description update wiring ───────────────────
        let desc_label_clone = description_label.clone();
        spam_action_combo.connect_changed(move |combo| {
            if let Some(active) = combo.active() {
                let action = match active {
                    0 => CalendarSpamAction::Prompt,
                    1 => CalendarSpamAction::Trash,
                    2 => CalendarSpamAction::Move,
                    _ => CalendarSpamAction::Prompt,
                };
                Self::update_description(&desc_label_clone, &action);
            }
        });

        // ─── 5. ScrolledWindow wrapper ───────────────────────────────────
        let container = ScrolledWindow::new();
        container.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        container.set_child(Some(&content_box));
        container.set_vexpand(true);
        container.set_hexpand(true);

        Self {
            container,
            enable_check,
            spam_action_combo,
            description_label,
        }
    }

    /// Read the current calendar config values from the widgets.
    #[must_use]
    pub fn read_values(&self) -> CalendarConfig {
        let action = match self.spam_action_combo.active() {
            Some(0) => CalendarSpamAction::Prompt,
            Some(1) => CalendarSpamAction::Trash,
            Some(2) => CalendarSpamAction::Move,
            _ => CalendarSpamAction::Prompt,
        };

        CalendarConfig {
            calendar_filtering_enabled: self.enable_check.is_active(),
            calendar_spam_action: action,
        }
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Update the description label text based on the selected action.
    fn update_description(label: &Label, action: &CalendarSpamAction) {
        let text = match action {
            CalendarSpamAction::Prompt => {
                "You will be asked what to do for each spam calendar item"
            }
            CalendarSpamAction::Trash => {
                "Calendar items classified as spam will be automatically deleted"
            }
            CalendarSpamAction::Move => {
                "Calendar items classified as spam will be moved to the spam folder"
            }
        };
        label.set_text(text);
    }
}
