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
    Adjustment, Align, Box as GtkBox, Button, CheckButton, DropDown, Entry,
    EventControllerFocus, EventControllerScroll, EventControllerScrollFlags, Frame, Label,
    Orientation, Scale, ScrolledWindow, StringList, Window,
};
use gtk4::glib;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use spambayes_config::{AppConfig, UpdateCheckInterval};

// ─── AdvancedValues ──────────────────────────────────────────────────────────

/// Values read from the Advanced tab for saving.
#[derive(Clone, Debug)]
pub struct AdvancedValues {
    pub timer_enabled: bool,
    pub timer_wait_for_sync: bool,
    pub timer_start_delay: f64,
    pub timer_interval: f64,
    pub timer_only_receive_folders: bool,
    pub verbose: u32,
    pub save_spam_info: bool,
    pub update_check_interval: UpdateCheckInterval,
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
    /// "Wait for sync before filtering" checkbox.
    pub wait_for_sync_check: CheckButton,
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

    // ─── Update settings ─────────────────────────────────────────────────
    /// "Check for updates" interval dropdown (Weekly / Monthly).
    pub update_interval_dropdown: DropDown,
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
        timer_enable_check.set_tooltip_text(Some(
            "Enable automatic background filtering of messages at regular intervals.",
        ));
        timer_box.append(&timer_enable_check);

        // Wait for sync checkbox
        let wait_for_sync_check =
            CheckButton::with_label("Wait for sync before filtering");
        wait_for_sync_check.set_active(config.filter.timer_wait_for_sync);
        wait_for_sync_check.set_tooltip_text(Some(
            "Wait for Outlook to finish syncing before starting background filtering.",
        ));
        wait_for_sync_check.set_sensitive(config.filter.timer_enabled);
        timer_box.append(&wait_for_sync_check);

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
        start_delay_scale.set_tooltip_text(Some(
            "How long to wait after Outlook starts before beginning background filtering.",
        ));

        // Inhibit mouse scroll on the scale so scrolling the page doesn't change the value
        let sd_scroll_ctrl = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
        sd_scroll_ctrl.connect_scroll(|_, _, _| glib::Propagation::Stop);
        start_delay_scale.add_controller(sd_scroll_ctrl);

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
        interval_scale.set_tooltip_text(Some(
            "Delay between processing each message during background filtering.",
        ));

        // Inhibit mouse scroll on the scale so scrolling the page doesn't change the value
        let iv_scroll_ctrl = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
        iv_scroll_ctrl.connect_scroll(|_, _, _| glib::Propagation::Stop);
        interval_scale.add_controller(iv_scroll_ctrl);

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
        only_receive_check.set_tooltip_text(Some(
            "Only process folders that receive new mail, not sent items or archives.",
        ));
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
        wait_for_sync_check.set_sensitive(timer_controls_sensitive);

        {
            let sd_label = start_delay_label.clone();
            let sd_row = start_delay_row.clone();
            let iv_label = interval_label.clone();
            let iv_row = interval_row.clone();
            let or_check = only_receive_check.clone();
            let wfs_check = wait_for_sync_check.clone();
            timer_enable_check.connect_toggled(move |checkbox| {
                let active = checkbox.is_active();
                sd_label.set_sensitive(active);
                sd_row.set_sensitive(active);
                iv_label.set_sensitive(active);
                iv_row.set_sensitive(active);
                or_check.set_sensitive(active);
                wfs_check.set_sensitive(active);
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

        let build_date_label = Label::new(Some(&format!(
            "Build date: {}",
            env!("SPAMBAYES_BUILD_ID")
        )));
        build_date_label.set_halign(Align::Start);

        env_box.append(&version_label);
        env_box.append(&arch_label);
        env_box.append(&build_date_label);

        // "Check for Update" button within the environment section.
        let update_btn = Button::with_label("Check for Update");
        update_btn.set_halign(Align::Start);
        update_btn.set_margin_top(8);
        update_btn.set_tooltip_text(Some(
            "Check if a newer version of SpamBayes is available for download.",
        ));
        let update_url = config.update.update_url.clone();
        update_btn.connect_clicked(move |btn| {
            Self::check_for_update(btn, &update_url);
        });
        env_box.append(&update_btn);

        // "Check for updates" interval dropdown.
        let interval_row = GtkBox::new(Orientation::Horizontal, 8);
        interval_row.set_margin_top(8);
        let interval_label = Label::new(Some("Check for updates:"));
        interval_label.set_halign(Align::Start);
        let interval_model = StringList::new(&["Weekly", "Monthly"]);
        let update_interval_dropdown = DropDown::new(Some(interval_model), gtk4::Expression::NONE);
        update_interval_dropdown.set_tooltip_text(Some(
            "How often SpamBayes checks for new versions.",
        ));
        // Select the current config value (0 = Weekly, 1 = Monthly).
        let selected_idx = match config.update.check_interval {
            UpdateCheckInterval::Weekly => 0,
            UpdateCheckInterval::Monthly => 1,
        };
        update_interval_dropdown.set_selected(selected_idx);
        interval_row.append(&interval_label);
        interval_row.append(&update_interval_dropdown);
        env_box.append(&interval_row);

        env_frame.set_child(Some(&env_box));
        content_box.append(&env_frame);

        // ─── 6. Show Data Folder button (Req 7.3) ───────────────────────
        let data_folder_btn = Button::with_label("Show Data Folder");
        data_folder_btn.set_halign(Align::Start);
        data_folder_btn.set_tooltip_text(Some(
            "Open the data folder where SpamBayes stores configuration and database files.",
        ));
        let data_dir = data_directory.to_path_buf();
        data_folder_btn.connect_clicked(move |_| {
            Self::open_data_folder(&data_dir);
        });
        content_box.append(&data_folder_btn);

        // ─── 6b. Reset Database button ──────────────────────────────────
        let reset_db_btn = Button::with_label("Reset Database");
        reset_db_btn.set_halign(Align::Start);
        reset_db_btn.set_tooltip_text(Some(
            "Delete all training data and start fresh. This cannot be undone.",
        ));
        let data_dir_for_reset = data_directory.to_path_buf();
        reset_db_btn.connect_clicked(move |btn| {
            let parent_window = btn
                .root()
                .and_then(|root| root.downcast::<Window>().ok());
            Self::confirm_reset_database(parent_window.as_ref(), &data_dir_for_reset);
        });
        content_box.append(&reset_db_btn);

        // ─── 7. Diagnostics button (Req 7.4) ────────────────────────────
        let verbose = Rc::new(Cell::new(config.general.verbose));
        let save_spam_info = Rc::new(Cell::new(config.filter.save_spam_info));

        let diag_btn = Button::with_label("Diagnostics...");
        diag_btn.set_halign(Align::Start);
        diag_btn.set_tooltip_text(Some(
            "View diagnostic information and toggle verbose logging.",
        ));

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
            wait_for_sync_check,
            start_delay_scale,
            start_delay_entry,
            interval_scale,
            interval_entry,
            only_receive_check,
            verbose,
            save_spam_info,
            update_interval_dropdown,
        }
    }

    /// Read the current advanced config values from the widgets.
    #[must_use]
    pub fn read_values(&self) -> AdvancedValues {
        let update_check_interval = match self.update_interval_dropdown.selected() {
            0 => UpdateCheckInterval::Weekly,
            _ => UpdateCheckInterval::Monthly,
        };
        AdvancedValues {
            timer_enabled: self.timer_enable_check.is_active(),
            timer_wait_for_sync: self.wait_for_sync_check.is_active(),
            timer_start_delay: self.start_delay_scale.value(),
            timer_interval: self.interval_scale.value(),
            timer_only_receive_folders: self.only_receive_check.is_active(),
            verbose: self.verbose.get(),
            save_spam_info: self.save_spam_info.get(),
            update_check_interval,
        }
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Perform an update check and show the result to the user.
    ///
    /// Fetches the remote version manifest and compares against the running
    /// version. Shows a dialog with the result (up-to-date, or new version
    /// available with a download button).
    fn check_for_update(btn: &Button, update_url: &str) {
        use crate::version_manifest::{self, VersionManifest, CURRENT_VERSION, CURRENT_BUILD_NUMBER};

        let parent_window = btn
            .root()
            .and_then(|root| root.downcast::<Window>().ok());

        // Fetch manifest synchronously (acceptable for a user-triggered button click).
        let result = ureq::get(update_url)
            .timeout(std::time::Duration::from_secs(15))
            .call();

        let manifest = match result {
            Ok(response) => {
                if response.status() != 200 {
                    Self::show_update_result(
                        parent_window.as_ref(),
                        "Update Check Failed",
                        &format!("Server returned HTTP {}", response.status()),
                    );
                    return;
                }
                match response.into_string() {
                    Ok(body) => match VersionManifest::from_json(&body) {
                        Ok(m) => m,
                        Err(e) => {
                            Self::show_update_result(
                                parent_window.as_ref(),
                                "Update Check Failed",
                                &format!("Invalid version manifest: {e}"),
                            );
                            return;
                        }
                    },
                    Err(e) => {
                        Self::show_update_result(
                            parent_window.as_ref(),
                            "Update Check Failed",
                            &format!("Failed to read response: {e}"),
                        );
                        return;
                    }
                }
            }
            Err(e) => {
                Self::show_update_result(
                    parent_window.as_ref(),
                    "Update Check Failed",
                    &format!("Network error: {e}"),
                );
                return;
            }
        };

        // Compare against current version.
        let status = version_manifest::check_update_status(&manifest);

        match status {
            version_manifest::UpdateStatus::UpToDate => {
                Self::show_update_result(
                    parent_window.as_ref(),
                    "Up to Date",
                    &format!(
                        "You are running the latest version.\n\nVersion: {}\nBuild: {}",
                        CURRENT_VERSION, CURRENT_BUILD_NUMBER
                    ),
                );
            }
            version_manifest::UpdateStatus::NewVersionAvailable {
                current,
                latest,
                download_url,
                release_notes,
            } => {
                let notes_section = if release_notes.is_empty() {
                    String::new()
                } else {
                    format!("\n\nWhat's new: {release_notes}")
                };
                Self::show_update_with_download(
                    parent_window.as_ref(),
                    "Update Available",
                    &format!(
                        "A new version is available!\n\nCurrent: {current}\nLatest: {latest}{notes_section}"
                    ),
                    &download_url,
                );
            }
            version_manifest::UpdateStatus::NewBuildAvailable {
                version,
                current_build,
                latest_build,
                download_url,
            } => {
                Self::show_update_with_download(
                    parent_window.as_ref(),
                    "Build Update Available",
                    &format!(
                        "A newer build of {version} is available.\n\n\
                         Your build: {current_build}\nLatest build: {latest_build}\n\n\
                         This is a maintenance update with bug fixes."
                    ),
                    &download_url,
                );
            }
        }
    }

    /// Show a simple informational dialog with a title and message.
    fn show_update_result(parent: Option<&Window>, title: &str, message: &str) {
        let dialog = Window::new();
        dialog.set_title(Some(title));
        dialog.set_default_size(380, 180);
        dialog.set_modal(true);
        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        let vbox = GtkBox::new(Orientation::Vertical, 12);
        vbox.set_margin_top(16);
        vbox.set_margin_bottom(16);
        vbox.set_margin_start(16);
        vbox.set_margin_end(16);

        let label = Label::new(Some(message));
        label.set_wrap(true);
        label.set_halign(Align::Start);
        vbox.append(&label);

        let close_btn = Button::with_label("OK");
        close_btn.set_halign(Align::End);
        let dialog_ref = dialog.clone();
        close_btn.connect_clicked(move |_| {
            dialog_ref.close();
        });
        vbox.append(&close_btn);

        dialog.set_child(Some(&vbox));
        dialog.present();
    }

    /// Show an update-available dialog with a "Download" button.
    fn show_update_with_download(
        parent: Option<&Window>,
        title: &str,
        message: &str,
        download_url: &str,
    ) {
        let dialog = Window::new();
        dialog.set_title(Some(title));
        dialog.set_default_size(420, 220);
        dialog.set_modal(true);
        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        let vbox = GtkBox::new(Orientation::Vertical, 12);
        vbox.set_margin_top(16);
        vbox.set_margin_bottom(16);
        vbox.set_margin_start(16);
        vbox.set_margin_end(16);

        let label = Label::new(Some(message));
        label.set_wrap(true);
        label.set_halign(Align::Start);
        vbox.append(&label);

        let btn_box = GtkBox::new(Orientation::Horizontal, 8);
        btn_box.set_halign(Align::End);

        let download_btn = Button::with_label("Download");
        download_btn.set_tooltip_text(Some("Open the download page in your browser."));
        let url = download_url.to_string();
        let dialog_ref = dialog.clone();
        download_btn.connect_clicked(move |_| {
            Self::open_url(&url);
            dialog_ref.close();
        });
        btn_box.append(&download_btn);

        let close_btn = Button::with_label("Close");
        let dialog_ref2 = dialog.clone();
        close_btn.connect_clicked(move |_| {
            dialog_ref2.close();
        });
        btn_box.append(&close_btn);

        vbox.append(&btn_box);
        dialog.set_child(Some(&vbox));
        dialog.present();
    }

    /// Open a URL in the default browser.
    fn open_url(url: &str) {
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(url)
                .spawn();
        }
    }

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

    /// Show a confirmation dialog before resetting the database.
    ///
    /// Displays a warning that all training data will be lost, with
    /// "Reset" and "Cancel" buttons.
    fn confirm_reset_database(parent: Option<&Window>, data_directory: &Path) {
        let dialog = Window::new();
        dialog.set_title(Some("Reset Database"));
        dialog.set_default_size(420, 200);
        dialog.set_modal(true);
        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        let vbox = GtkBox::new(Orientation::Vertical, 12);
        vbox.set_margin_top(20);
        vbox.set_margin_bottom(20);
        vbox.set_margin_start(20);
        vbox.set_margin_end(20);

        let warning = Label::new(Some(
            "This will delete all training data (classifier database \
             and message database). The classifier will need to be \
             retrained from scratch.\n\n\
             This action cannot be undone.",
        ));
        warning.set_wrap(true);
        warning.set_halign(Align::Start);
        vbox.append(&warning);

        let button_bar = GtkBox::new(Orientation::Horizontal, 8);
        button_bar.set_halign(Align::End);
        button_bar.set_margin_top(12);

        let reset_btn = Button::with_label("Reset");
        let cancel_btn = Button::with_label("Cancel");

        button_bar.append(&reset_btn);
        button_bar.append(&cancel_btn);
        vbox.append(&button_bar);

        dialog.set_child(Some(&vbox));

        // Wire Reset button
        let data_dir_clone = data_directory.to_path_buf();
        let dialog_clone = dialog.clone();
        reset_btn.connect_clicked(move |_| {
            Self::reset_database(&data_dir_clone, Some(&dialog_clone));
            dialog_clone.close();
        });

        // Wire Cancel button
        let dialog_close = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            dialog_close.close();
        });

        dialog.present();
    }

    /// Delete the classifier and message database files to reset to blank.
    ///
    /// Removes `spambayes.db` and `spambayes_msg.db` from the data directory.
    /// Shows an info dialog on success or an error dialog on failure.
    fn reset_database(data_directory: &Path, parent: Option<&Window>) {
        let db_path = data_directory.join("spambayes.db");
        let msg_db_path = data_directory.join("spambayes_msg.db");

        let mut errors: Vec<String> = Vec::new();

        // Delete classifier database
        if db_path.exists() {
            if let Err(e) = std::fs::remove_file(&db_path) {
                errors.push(format!("spambayes.db: {e}"));
            }
        }

        // Delete message database
        if msg_db_path.exists() {
            if let Err(e) = std::fs::remove_file(&msg_db_path) {
                errors.push(format!("spambayes_msg.db: {e}"));
            }
        }

        // Show result
        Self::show_reset_result(parent, &errors);
    }

    /// Show the result of a database reset operation.
    fn show_reset_result(parent: Option<&Window>, errors: &[String]) {
        let dialog = Window::new();
        dialog.set_default_size(400, 150);
        dialog.set_modal(true);
        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        let vbox = GtkBox::new(Orientation::Vertical, 12);
        vbox.set_margin_top(20);
        vbox.set_margin_bottom(20);
        vbox.set_margin_start(20);
        vbox.set_margin_end(20);

        if errors.is_empty() {
            dialog.set_title(Some("Database Reset"));
            let msg = Label::new(Some(
                "Database has been reset successfully.\n\n\
                 The classifier will start fresh on next use. \
                 You will need to retrain it with ham and spam messages.",
            ));
            msg.set_wrap(true);
            msg.set_halign(Align::Start);
            vbox.append(&msg);
        } else {
            dialog.set_title(Some("Reset Error"));
            let error_text = format!(
                "Some files could not be deleted:\n\n{}\n\n\
                 The database may be in use. Try closing Outlook first.",
                errors.join("\n")
            );
            let msg = Label::new(Some(&error_text));
            msg.set_wrap(true);
            msg.set_halign(Align::Start);
            vbox.append(&msg);
        }

        let ok_btn = Button::with_label("OK");
        ok_btn.set_halign(Align::End);
        let dialog_clone = dialog.clone();
        ok_btn.connect_clicked(move |_| {
            dialog_clone.close();
        });
        vbox.append(&ok_btn);

        dialog.set_child(Some(&vbox));
        dialog.present();
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
        let log_path = data_directory.join("addin_debug.log");
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
