//! Notifications tab — notification sound configuration.
//!
//! Provides controls for:
//! - Enable/disable notification sounds
//! - Good/Unsure/Spam sound file paths (WAV) with Browse buttons
//! - Accumulation delay (time to wait for additional messages) Scale + Entry
//!
//! **Validates: Requirements 5.1, 5.2, 5.3, 5.4**

use gtk4::prelude::*;
use gtk4::{
    Adjustment, Align, Box as GtkBox, Button, CheckButton, Entry, EventControllerFocus, FileDialog,
    FileFilter, Frame, Label, Orientation, Scale, ScrolledWindow,
};

use std::cell::Cell;
use std::rc::Rc;

use spambayes_config::NotificationConfig;

// ─── NotificationsTab ────────────────────────────────────────────────────────

/// The Notifications tab content.
///
/// Contains an enable checkbox, sound file entries with browse buttons,
/// and a timing delay scale/entry. When the enable checkbox is unchecked,
/// all sound and timing controls are greyed out (insensitive).
///
/// **Validates: Requirements 5.1, 5.2, 5.3, 5.4**
pub struct NotificationsTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Enable checkbox (Req 5.1) ───────────────────────────────────────
    /// "Enable new mail notification sounds" checkbox.
    pub enable_check: CheckButton,

    // ─── Sound file entries (Req 5.2) ────────────────────────────────────
    /// Good (ham) messages sound file path entry.
    pub ham_sound_entry: Entry,
    /// Possible spam (unsure) messages sound file path entry.
    pub unsure_sound_entry: Entry,
    /// Spam messages sound file path entry.
    pub spam_sound_entry: Entry,

    // ─── Timing controls (Req 5.3) ──────────────────────────────────────
    /// Delay scale (0–60 seconds).
    pub delay_scale: Scale,
    /// Delay entry (editable, synced with scale).
    pub delay_entry: Entry,
}

impl NotificationsTab {
    /// Build the Notifications tab widget tree.
    ///
    /// # Arguments
    ///
    /// * `config` – Current notification configuration values
    ///
    /// **Validates: Requirements 5.1, 5.2, 5.3, 5.4**
    #[must_use]
    pub fn new(config: &NotificationConfig) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Enable checkbox (Req 5.1) ────────────────────────────────
        let enable_check = CheckButton::with_label("Enable new mail notification sounds");
        enable_check.set_active(config.notify_sound_enabled);
        content_box.append(&enable_check);

        // ─── 2. Sound files section (Req 5.2) ───────────────────────────
        let sound_frame = Frame::new(Some("Sound files:"));
        let sound_box = GtkBox::new(Orientation::Vertical, 6);
        sound_box.set_margin_top(8);
        sound_box.set_margin_bottom(8);
        sound_box.set_margin_start(12);
        sound_box.set_margin_end(12);

        // Good messages row
        let ham_sound_entry = Self::build_sound_row(
            &sound_box,
            "Good messages:",
            &config.notify_ham_sound,
        );

        // Possible spam row
        let unsure_sound_entry = Self::build_sound_row(
            &sound_box,
            "Possible spam:",
            &config.notify_unsure_sound,
        );

        // Spam messages row
        let spam_sound_entry = Self::build_sound_row(
            &sound_box,
            "Spam messages:",
            &config.notify_spam_sound,
        );

        sound_frame.set_child(Some(&sound_box));
        content_box.append(&sound_frame);

        // ─── 3. Timing section (Req 5.3) ────────────────────────────────
        let timing_frame = Frame::new(Some("Timing:"));
        let timing_box = GtkBox::new(Orientation::Vertical, 6);
        timing_box.set_margin_top(8);
        timing_box.set_margin_bottom(8);
        timing_box.set_margin_start(12);
        timing_box.set_margin_end(12);

        let timing_label = Label::new(Some("Time to wait for additional messages:"));
        timing_label.set_halign(Align::Start);
        timing_box.append(&timing_label);

        let timing_row = GtkBox::new(Orientation::Horizontal, 8);
        timing_row.set_valign(Align::Center);

        let delay_adj = Adjustment::new(
            config.notify_accumulate_delay,
            0.0,
            60.0,
            1.0,
            5.0,
            0.0,
        );
        let delay_scale = Scale::new(Orientation::Horizontal, Some(&delay_adj));
        delay_scale.set_hexpand(true);
        delay_scale.set_digits(0);

        let delay_entry = Entry::new();
        delay_entry.set_width_chars(4);
        delay_entry.set_text(&format!("{}", config.notify_accumulate_delay as u32));

        let seconds_label = Label::new(Some("seconds"));
        seconds_label.set_halign(Align::Start);

        timing_row.append(&delay_scale);
        timing_row.append(&delay_entry);
        timing_row.append(&seconds_label);
        timing_box.append(&timing_row);

        timing_frame.set_child(Some(&timing_box));
        content_box.append(&timing_frame);

        // ─── 4. Enable checkbox → sensitivity wiring ─────────────────────
        let controls_sensitive = config.notify_sound_enabled;
        sound_frame.set_sensitive(controls_sensitive);
        timing_frame.set_sensitive(controls_sensitive);

        let sound_frame_clone = sound_frame.clone();
        let timing_frame_clone = timing_frame.clone();
        enable_check.connect_toggled(move |checkbox| {
            let active = checkbox.is_active();
            sound_frame_clone.set_sensitive(active);
            timing_frame_clone.set_sensitive(active);
        });

        // ─── 5. Scale ↔ Entry synchronization ────────────────────────────
        let updating = Rc::new(Cell::new(false));

        // Scale → Entry
        {
            let entry = delay_entry.clone();
            let flag = Rc::clone(&updating);
            delay_scale.connect_value_changed(move |scale| {
                if flag.get() {
                    return;
                }
                flag.set(true);
                entry.set_text(&format!("{}", scale.value() as u32));
                flag.set(false);
            });
        }

        // Entry → Scale (on activate / Enter key)
        {
            let scale = delay_scale.clone();
            let entry_clone = delay_entry.clone();
            let flag = Rc::clone(&updating);
            delay_entry.connect_activate(move |e| {
                if flag.get() {
                    return;
                }
                flag.set(true);
                if let Ok(val) = e.text().parse::<f64>() {
                    let clamped = val.clamp(0.0, 60.0);
                    scale.set_value(clamped);
                    entry_clone.set_text(&format!("{}", clamped as u32));
                }
                flag.set(false);
            });
        }

        // Entry → Scale (on focus-out)
        {
            let scale = delay_scale.clone();
            let entry_clone = delay_entry.clone();
            let flag = Rc::clone(&updating);
            let focus_controller = EventControllerFocus::new();
            focus_controller.connect_leave(move |_| {
                if flag.get() {
                    return;
                }
                flag.set(true);
                if let Ok(val) = entry_clone.text().parse::<f64>() {
                    let clamped = val.clamp(0.0, 60.0);
                    scale.set_value(clamped);
                    entry_clone.set_text(&format!("{}", clamped as u32));
                }
                flag.set(false);
            });
            delay_entry.add_controller(focus_controller);
        }

        // ─── 6. ScrolledWindow wrapper (Req 5.4) ────────────────────────
        let container = ScrolledWindow::new();
        container.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        container.set_child(Some(&content_box));
        container.set_vexpand(true);
        container.set_hexpand(true);

        Self {
            container,
            enable_check,
            ham_sound_entry,
            unsure_sound_entry,
            spam_sound_entry,
            delay_scale,
            delay_entry,
        }
    }

    /// Read the current notification config values from the widgets.
    #[must_use]
    pub fn read_values(&self) -> NotificationConfig {
        NotificationConfig {
            notify_sound_enabled: self.enable_check.is_active(),
            notify_ham_sound: self.ham_sound_entry.text().to_string(),
            notify_unsure_sound: self.unsure_sound_entry.text().to_string(),
            notify_spam_sound: self.spam_sound_entry.text().to_string(),
            notify_accumulate_delay: self.delay_scale.value(),
        }
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Build a single sound file row (label + entry + browse button) and
    /// append it to the parent box. Returns the Entry widget for storage.
    fn build_sound_row(parent: &GtkBox, label_text: &str, initial_path: &str) -> Entry {
        let row_box = GtkBox::new(Orientation::Horizontal, 8);
        row_box.set_valign(Align::Center);

        let label = Label::new(Some(label_text));
        label.set_halign(Align::Start);
        label.set_width_chars(14);

        let entry = Entry::new();
        entry.set_hexpand(true);
        entry.set_placeholder_text(Some("Select a WAV file..."));
        if !initial_path.is_empty() {
            entry.set_text(initial_path);
        }

        let browse_btn = Button::with_label("Browse...");

        // Wire Browse button to open a file dialog with WAV filter
        let entry_clone = entry.clone();
        browse_btn.connect_clicked(move |btn| {
            let parent_window = btn
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok());

            let wav_filter = FileFilter::new();
            wav_filter.set_name(Some("WAV files (*.wav)"));
            wav_filter.add_pattern("*.wav");
            wav_filter.add_pattern("*.WAV");

            let all_filter = FileFilter::new();
            all_filter.set_name(Some("All files (*.*)"));
            all_filter.add_pattern("*");

            let filters = gtk4::gio::ListStore::new::<FileFilter>();
            filters.append(&wav_filter);
            filters.append(&all_filter);

            let dialog = FileDialog::new();
            dialog.set_title("Select notification sound");
            dialog.set_filters(Some(&filters));
            dialog.set_default_filter(Some(&wav_filter));

            let entry_for_cb = entry_clone.clone();
            dialog.open(
                parent_window.as_ref(),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            entry_for_cb.set_text(&path.to_string_lossy());
                        }
                    }
                },
            );
        });

        row_box.append(&label);
        row_box.append(&entry);
        row_box.append(&browse_btn);
        parent.append(&row_box);

        entry
    }
}
