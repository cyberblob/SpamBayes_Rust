//! Advanced tab — timer settings, environment info, diagnostics.
//!
//! Provides controls for:
//! - Filter timer settings (enable, start delay, interval, only-new-mail folders)
//! - Rust Environment info (build version, architecture)
//! - "Show Data Folder" button (opens data directory in file explorer)
//! - "Diagnostics..." button (opens sub-dialog with verbosity, view log, save score)
//!
//! **Validates: Requirements 7.1, 7.2, 7.3, 7.4, 7.5**

use gtk4::prelude::*;
use gtk4::{
    Adjustment, Align, Box as GtkBox, Button, CheckButton, Entry, EventControllerFocus, Frame,
    Label, Orientation, Scale, ScrolledWindow, Window,
};

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use spambayes_config::AppConfig;

// ─── AdvancedValues ──────────────────────────────────────────────────────────

/// Values read from the Advanced tab for saving.
#[derive(Clone, Debug)]
pub struct AdvancedValues {
    pub timer_enabled: bool,
    pub timer_start_delay: f64,
    pub timer_interval: f64,
    pub timer_only_receive_folders: bool,
    pub verbose: u32,
    pub save_spam_info: bool,
}

// ─── AdvancedTab ─────────────────────────────────────────────────────────────

/// The Advanced tab content.
///
/// Contains timer settings, environment info, show data folder button,
/// and diagnostics button. When the timer enable checkbox is unchecked,
/// all timer controls are greyed out (insensitive).
///
/// **Validates: Requirements 7.1, 7.2, 7.3, 7.4, 7.5**
pub struct AdvancedTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Timer controls (Req 7.1) ────────────────────────────────────────
    /// "Enable background filtering" checkbox.
    pub timer_enable_check: CheckButton,
    /// "Processing start delay" scale (0–60 seconds).
    pub start_delay_scale: Scale,
    /// "Processing start delay" entry (editable, synced with scale).
    pub start_delay_entry: Entry,
    /// "Delay between processing items" scale (0–60 seconds).
    pub interval_scale: Scale,
    /// "Delay between processing items" entry (editable, synced with scale).
    pub interval_entry: Entry,
    /// "Only for folders that receive new mail" checkbox.
    pub only_receive_check: CheckButton,

    // ─── Diagnostics values (stored from sub-dialog) ─────────────────────
    /// Current verbosity level (updated when diagnostics dialog saves).
    pub verbose: Rc<Cell<u32>>,
    /// Current save_spam_info state (updated when diagnostics dialog saves).
    pub save_spam_info: Rc<Cell<bool>>,
}

impl AdvancedTab {
    /// Build the Advanced tab widget tree.
    ///
    /// # Arguments
    ///
    /// * `config` – Current application configuration values
    /// * `data_directory` – Path to the SpamBayes data directory
    ///
    /// **Validates: Requirements 7.1, 7.2, 7.3, 7.4, 7.5**
    #[must_use]
    pub fn new(config: &AppConfig, data_directory: &Path) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Filter timer section (Req 7.1) ──────────────────────────
        let timer_frame = Frame::new(Some("Filter timer:"));
        let timer_box = GtkBox::new(Orientation::Vertical, 8);
        timer_box.set_margin_top(8);
        timer_box.set_margin_bottom(8);
        timer_box.set_margin_start(12);
        timer_box.set_margin_end(12);

        // Enable checkbox
        let timer_enable_check =
            CheckButton::with_label("Enable background filtering");
        timer_enable_check.set_active(config.filter.timer_enabled);
        timer_box.append(&timer_enable_check);

        // Start delay row
        let start_delay_label = Label::new(Some("Processing start delay:"));
        start_delay_label.set_halign(Align::Start);
        timer_box.append(&start_delay_label);

        let start_delay_row = GtkBox::new(Orientation::Horizontal, 8);
        start_delay_row.set_valign(Align::Center);

        let start_delay_adj = Adjustment::new(
            config.filter.timer_start_delay,
            0.0,
            60.0,
            0.5,
            5.0,
            0.0,
        );
        let start_delay_scale = Scale::new(Orientation::Horizontal, Some(&start_delay_adj));
        start_delay_scale.set_hexpand(true);
        start_delay_scale.set_digits(1);

        let start_delay_entry = Entry::new();
        start_delay_entry.set_width_chars(5);
        start_delay_entry.set_text(&format!("{:.1}", config.filter.timer_start_delay));

        let start_seconds_label = Label::new(Some("seconds"));
        start_seconds_label.set_halign(Align::Start);

        start_delay_row.append(&start_delay_scale);
        start_delay_row.append(&start_delay_entry);
        start_delay_row.append(&start_seconds_label);
        timer_box.append(&start_delay_row);

        // Interval row
        let interval_label = Label::new(Some("Delay between processing items:"));
        interval_label.set_halign(Align::Start);
        timer_box.append(&interval_label);

        let interval_row = GtkBox::new(Orientation::Horizontal, 8);
        interval_row.set_valign(Align::Center);

        let interval_adj = Adjustment::new(
            config.filter.timer_interval,
            0.0,
            60.0,
            0.5,
            5.0,
            0.0,
        );
        let interval_scale = Scale::new(Orientation::Horizontal, Some(&interval_adj));
        interval_scale.set_hexpand(true);
        interval_scale.set_digits(1);

        let interval_entry = Entry::new();
        interval_entry.set_width_chars(5);
        interval_entry.set_text(&format!("{:.1}", config.filter.timer_interval));

        let interval_seconds_label = Label::new(Some("seconds"));
        interval_seconds_label.set_halign(Align::Start);

        interval_row.append(&interval_scale);
        interval_row.append(&interval_entry);
        interval_row.append(&interval_seconds_label);
        timer_box.append(&interval_row);

        // Only receive folders checkbox
        let only_receive_check =
            CheckButton::with_label("Only for folders that receive new mail");
        only_receive_check.set_active(config.filter.timer_only_receive_folders);
        timer_box.append(&only_receive_check);

        timer_frame.set_child(Some(&timer_box));
        content_box.append(&timer_frame);

        // ─── 2. Enable checkbox → sensitivity wiring ─────────────────────
        // When timer is disabled, grey out all timer controls
        let timer_controls_sensitive = config.filter.timer_enabled;
        start_delay_label.set_sensitive(timer_controls_sensitive);
        start_delay_row.set_sensitive(timer_controls_sensitive);
        interval_label.set_sensitive(timer_controls_sensitive);
        interval_row.set_sensitive(timer_controls_sensitive);
        only_receive_check.set_sensitive(timer_controls_sensitive);

        {
            let sd_label = start_delay_label.clone();
            let sd_row = start_delay_row.clone();
            let iv_label = interval_label.clone();
            let iv_row = interval_row.clone();
            let or_check = only_receive_check.clone();
            timer_enable_check.connect_toggled(move |checkbox| {
                let active = checkbox.is_active();
                sd_label.set_sensitive(active);
                sd_row.set_sensitive(active);
                iv_label.set_sensitive(active);
                iv_row.set_sensitive(active);
                or_check.set_sensitive(active);
            });
        }

        // ─── 3. Scale ↔ Entry synchronization (start delay) ─────────────
        {
            let updating = Rc::new(Cell::new(false));

            // Scale → Entry
            {
                let entry = start_delay_entry.clone();
                let flag = Rc::clone(&updating);
                start_delay_scale.connect_value_changed(move |scale| {
                    if flag.get() {
                        return;
                    }
                    flag.set(true);
                    entry.set_text(&format!("{:.1}", scale.value()));
                    flag.set(false);
                });
            }

            // Entry → Scale (on activate / Enter key)
            {
                let scale = start_delay_scale.clone();
                let entry_clone = start_delay_entry.clone();
                let flag = Rc::clone(&updating);
                start_delay_entry.connect_activate(move |e| {
                    if flag.get() {
                        return;
                    }
                    flag.set(true);
                    if let Ok(val) = e.text().parse::<f64>() {
                        let clamped = val.clamp(0.0, 60.0);
                        scale.set_value(clamped);
                        entry_clone.set_text(&format!("{:.1}", clamped));
                    }
                    flag.set(false);
                });
            }

            // Entry → Scale (on focus-out)
            {
                let scale = start_delay_scale.clone();
                let entry_clone = start_delay_entry.clone();
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
                        entry_clone.set_text(&format!("{:.1}", clamped));
                    }
                    flag.set(false);
                });
                start_delay_entry.add_controller(focus_controller);
            }
        }

        // ─── 4. Scale ↔ Entry synchronization (interval) ────────────────
        {
            let updating = Rc::new(Cell::new(false));

            // Scale → Entry
            {
                let entry = interval_entry.clone();
                let flag = Rc::clone(&updating);
                interval_scale.connect_value_changed(move |scale| {
                    if flag.get() {
                        return;
                    }
                    flag.set(true);
                    entry.set_text(&format!("{:.1}", scale.value()));
                    flag.set(false);
                });
            }

            // Entry → Scale (on activate / Enter key)
            {
                let scale = interval_scale.clone();
                let entry_clone = interval_entry.clone();
                let flag = Rc::clone(&updating);
                interval_entry.connect_activate(move |e| {
                    if flag.get() {
                        return;
                    }
                    flag.set(true);
                    if let Ok(val) = e.text().parse::<f64>() {
                        let clamped = val.clamp(0.0, 60.0);
                        scale.set_value(clamped);
                        entry_clone.set_text(&format!("{:.1}", clamped));
                    }
                    flag.set(false);
                });
            }

            // Entry → Scale (on focus-out)
            {
                let scale = interval_scale.clone();
                let entry_clone = interval_entry.clone();
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
                        entry_clone.set_text(&format!("{:.1}", clamped));
                    }
                    flag.set(false);
                });
                interval_entry.add_controller(focus_controller);
            }
        }

        // ─── 5. Rust Environment section (Req 7.2) ──────────────────────
        let env_frame = Frame::new(Some("Rust Environment:"));
        let env_box = GtkBox::new(Orientation::Vertical, 4);
        env_box.set_margin_top(8);
        env_box.set_margin_bottom(8);
        env_box.set_margin_start(12);
        env_box.set_margin_end(12);

        let version_label = Label::new(Some(&format!(
            "Build version: {}",
            env!("CARGO_PKG_VERSION")
        )));
        version_label.set_halign(Align::Start);

        let arch_label = Label::new(Some(&format!(
            "Architecture: {}",
            std::env::consts::ARCH
        )));
        arch_label.set_halign(Align::Start);

        env_box.append(&version_label);
        env_box.append(&arch_label);
        env_frame.set_child(Some(&env_box));
        content_box.append(&env_frame);

        // ─── 6. Show Data Folder button (Req 7.3) ───────────────────────
        let data_folder_btn = Button::with_label("Show Data Folder");
        data_folder_btn.set_halign(Align::Start);
        let data_dir = data_directory.to_path_buf();
        data_folder_btn.connect_clicked(move |_| {
            Self::open_data_folder(&data_dir);
        });
        content_box.append(&data_folder_btn);

        // ─── 7. Diagnostics button (Req 7.4) ────────────────────────────
        let verbose = Rc::new(Cell::new(config.general.verbose));
        let save_spam_info = Rc::new(Cell::new(config.filter.save_spam_info));

        let diag_btn = Button::with_label("Diagnostics...");
        diag_btn.set_halign(Align::Start);

        let verbose_clone = Rc::clone(&verbose);
        let save_spam_info_clone = Rc::clone(&save_spam_info);
        let data_dir_for_diag = data_directory.to_path_buf();
        diag_btn.connect_clicked(move |btn| {
            let parent_window = btn
                .root()
                .and_then(|root| root.downcast::<Window>().ok());
            Self::show_diagnostics_dialog(
                parent_window.as_ref(),
                &verbose_clone,
                &save_spam_info_clone,
                &data_dir_for_diag,
            );
        });
        content_box.append(&diag_btn);

        // ─── 8. ScrolledWindow wrapper (Req 7.5) ────────────────────────
        let container = ScrolledWindow::new();
        container.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        container.set_child(Some(&content_box));
        container.set_vexpand(true);
        container.set_hexpand(true);

        Self {
            container,
            timer_enable_check,
            start_delay_scale,
            start_delay_entry,
            interval_scale,
            interval_entry,
            only_receive_check,
            verbose,
            save_spam_info,
        }
    }

    /// Read the current advanced config values from the widgets.
    #[must_use]
    pub fn read_values(&self) -> AdvancedValues {
        AdvancedValues {
            timer_enabled: self.timer_enable_check.is_active(),
            timer_start_delay: self.start_delay_scale.value(),
            timer_interval: self.interval_scale.value(),
            timer_only_receive_folders: self.only_receive_check.is_active(),
            verbose: self.verbose.get(),
            save_spam_info: self.save_spam_info.get(),
        }
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Open the data folder in the system file explorer.
    fn open_data_folder(path: &Path) {
        let path_str = path.to_string_lossy().to_string();
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("explorer")
                .arg(&path_str)
                .spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(&path_str)
                .spawn();
        }
    }

    /// Show the Diagnostics sub-dialog.
    ///
    /// Creates a modal window with verbosity entry, view log button,
    /// save spam score checkbox, and save/close buttons.
    fn show_diagnostics_dialog(
        parent: Option<&Window>,
        verbose: &Rc<Cell<u32>>,
        save_spam_info: &Rc<Cell<bool>>,
        data_directory: &Path,
    ) {
        let dialog = Window::new();
        dialog.set_title(Some("SpamBayes Diagnostics"));
        dialog.set_default_size(400, 280);
        dialog.set_modal(true);
        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // Warning text
        let warning_label = Label::new(Some(
            "Warning: These are advanced diagnostic options. \
             Changing these settings may affect performance and should \
             only be modified if you understand their purpose.",
        ));
        warning_label.set_wrap(true);
        warning_label.set_halign(Align::Start);
        content_box.append(&warning_label);

        // Verbosity row
        let verb_row = GtkBox::new(Orientation::Horizontal, 8);
        verb_row.set_valign(Align::Center);

        let verb_label = Label::new(Some("Log file verbosity:"));
        verb_label.set_halign(Align::Start);

        let verb_entry = Entry::new();
        verb_entry.set_width_chars(4);
        verb_entry.set_text(&format!("{}", verbose.get()));

        verb_row.append(&verb_label);
        verb_row.append(&verb_entry);
        content_box.append(&verb_row);

        // View log button
        let view_log_btn = Button::with_label("View log...");
        view_log_btn.set_halign(Align::Start);
        let log_path = data_directory.join("spambayes.log");
        view_log_btn.connect_clicked(move |_| {
            Self::open_log_file(&log_path);
        });
        content_box.append(&view_log_btn);

        // Save Spam Score checkbox
        let save_score_check = CheckButton::with_label("Save Spam Score");
        save_score_check.set_active(save_spam_info.get());
        content_box.append(&save_score_check);

        // Button bar (Save + Close)
        let button_bar = GtkBox::new(Orientation::Horizontal, 8);
        button_bar.set_halign(Align::End);
        button_bar.set_margin_top(12);

        let save_btn = Button::with_label("Save");
        let close_btn = Button::with_label("Close");

        button_bar.append(&save_btn);
        button_bar.append(&close_btn);
        content_box.append(&button_bar);

        dialog.set_child(Some(&content_box));

        // Wire Save button
        let verbose_clone = Rc::clone(verbose);
        let save_spam_clone = Rc::clone(save_spam_info);
        let verb_entry_clone = verb_entry.clone();
        let save_score_clone = save_score_check.clone();
        let dialog_clone = dialog.clone();
        save_btn.connect_clicked(move |_| {
            // Parse verbosity (default to 0 if invalid)
            let verb_val = verb_entry_clone
                .text()
                .parse::<u32>()
                .unwrap_or(0);
            verbose_clone.set(verb_val);
            save_spam_clone.set(save_score_clone.is_active());
            dialog_clone.close();
        });

        // Wire Close button (close without saving)
        let dialog_close = dialog.clone();
        close_btn.connect_clicked(move |_| {
            dialog_close.close();
        });

        dialog.present();
    }

    /// Open the log file with the system's default application.
    ///
    /// Shows an informational dialog if the log file does not yet exist.
    fn open_log_file(path: &Path) {
        if !path.exists() {
            let dialog = Window::new();
            dialog.set_title(Some("Log File"));
            dialog.set_default_size(400, 150);
            dialog.set_modal(true);

            let vbox = GtkBox::new(Orientation::Vertical, 12);
            vbox.set_margin_top(20);
            vbox.set_margin_bottom(20);
            vbox.set_margin_start(20);
            vbox.set_margin_end(20);

            let msg = format!(
                "Log file not found.\n\nExpected location:\n{}\n\n\
                 The log file is created when SpamBayes performs operations \
                 with verbosity > 0.",
                path.display()
            );
            let label = Label::new(Some(&msg));
            label.set_wrap(true);
            label.set_halign(Align::Start);
            vbox.append(&label);

            let ok_btn = Button::with_label("OK");
            ok_btn.set_halign(Align::End);
            let dialog_clone = dialog.clone();
            ok_btn.connect_clicked(move |_| {
                dialog_clone.close();
            });
            vbox.append(&ok_btn);

            dialog.set_child(Some(&vbox));
            dialog.present();
            return;
        }

        let path_str = path.to_string_lossy().to_string();
        #[cfg(target_os = "windows")]
        {
            // Use the full path to notepad — .log files often have no default
            // handler on Windows, causing `start` to silently fail.
            let notepad = format!(
                "{}\\notepad.exe",
                std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string())
            );
            let _ = std::process::Command::new(&notepad)
                .arg(&path_str)
                .spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(&path_str)
                .spawn();
        }
    }
}
