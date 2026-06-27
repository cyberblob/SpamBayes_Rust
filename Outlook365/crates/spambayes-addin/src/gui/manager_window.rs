//! Manager window — the main SpamBayes settings window with 7 tabs.
//!
//! This is a 1-to-1 replacement of the `SpamBayesManager` class from
//! `tkinter_manager.py`. It provides a GTK4 `Window` with a `Notebook`
//! containing all configuration tabs, a header banner, and a bottom button
//! bar (About + Close).
//!
//! The window auto-saves settings on close, matching the current tkinter
//! `cancel()` → `apply_changes()` behavior.
//!
//! **Validates: Requirements 1–8, 14.1**

use gtk4::prelude::*;
use gtk4::{
    self, Align, Box as GtkBox, Button, CssProvider, Label, Notebook, Orientation, Window,
};
use gtk4::gdk;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use spambayes_config::AppConfig;

use crate::gui::folder_browser::FolderProvider;
use crate::gui::message_boxes;
use crate::manager_dlg::{ManagerState, ManagerStats};

use super::tabs::{
    AdvancedTab, CalendarTab, FilteringTab, GeneralTab, NotificationsTab, StatisticsTab,
    TrainingTab,
};

/// CSS for the header banner styling.
///
/// Uses class-based selectors to apply colors and fonts matching the
/// original tkinter `create_header()` layout.
const HEADER_CSS: &str = r#"
.header-banner {
    background-color: #2B579A;
    padding: 12px 20px;
}

.header-title {
    font-family: "Segoe UI", sans-serif;
    font-size: 15pt;
    font-weight: bold;
    color: white;
}

.header-x64 {
    font-family: "Segoe UI", sans-serif;
    font-size: 15pt;
    font-weight: normal;
    color: #B4C7E7;
}

.header-separator {
    font-family: "Segoe UI", sans-serif;
    font-size: 11pt;
    color: #6B8EC2;
}

.header-subtitle {
    font-family: "Segoe UI", sans-serif;
    font-size: 10pt;
    color: #B4C7E7;
}

.header-badge {
    background-color: #1E3F73;
    font-family: "Segoe UI", sans-serif;
    font-size: 7pt;
    color: #8FAADC;
    padding: 2px 6px;
    border-radius: 2px;
}

.header-accent {
    background-color: #F4B942;
}
"#;

/// Create the header banner widget.
///
/// Returns a vertical `GtkBox` containing:
/// 1. A horizontal header row (blue background #2B579A) with:
///    - Left side: "SpamBayes" (bold white 15pt) + " x64" (light blue 15pt)
///      + separator " │ " (gray-blue) + "Anti-spam Classifier" (light blue 10pt)
///    - Right side: "RUST POWERED" badge (darker blue background)
/// 2. A 3px gold accent line (#F4B942)
///
/// Matches the current tkinter `create_header()` layout from `tkinter_manager.py`.
///
/// **Important**: Call [`load_header_css`] once during application startup
/// (after `gtk::init()`) to register the CSS styles before creating this widget.
///
/// **Validates: Requirement 1.1**
pub fn create_header_banner() -> GtkBox {
    // Outer vertical box holds header row + accent line
    let vbox = GtkBox::new(Orientation::Vertical, 0);

    // --- Header row (blue background) ---
    let header_row = GtkBox::new(Orientation::Horizontal, 0);
    header_row.add_css_class("header-banner");

    // Left side: title labels
    let title_box = GtkBox::new(Orientation::Horizontal, 0);
    title_box.set_valign(Align::Center);

    let title_label = Label::new(Some("SpamBayes"));
    title_label.add_css_class("header-title");

    let x64_label = Label::new(Some(" x64"));
    x64_label.add_css_class("header-x64");

    let sep_label = Label::new(Some("  │  "));
    sep_label.add_css_class("header-separator");

    let subtitle_label = Label::new(Some("Anti-spam Classifier"));
    subtitle_label.add_css_class("header-subtitle");

    title_box.append(&title_label);
    title_box.append(&x64_label);
    title_box.append(&sep_label);
    title_box.append(&subtitle_label);

    // Right side: "RUST POWERED" badge
    let badge_label = Label::new(Some("  RUST POWERED  "));
    badge_label.add_css_class("header-badge");
    badge_label.set_valign(Align::Center);

    header_row.append(&title_box);
    // Spacer to push badge to the right
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header_row.append(&spacer);
    header_row.append(&badge_label);

    // --- Gold accent line (3px) ---
    let accent_line = GtkBox::new(Orientation::Horizontal, 0);
    accent_line.add_css_class("header-accent");
    accent_line.set_size_request(-1, 3);

    // Assemble
    vbox.append(&header_row);
    vbox.append(&accent_line);

    vbox
}

/// Register header banner CSS with the given display.
///
/// Should be called once during application startup (after `gtk::init()`).
/// This is the preferred approach — avoids per-widget CSS loading.
pub fn load_header_css(display: &gdk::Display) {
    let provider = CssProvider::new();
    provider.load_from_string(HEADER_CSS);
    gtk4::style_context_add_provider_for_display(
        display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ─── ManagerWindow ───────────────────────────────────────────────────────────

/// The main Manager window.
///
/// Contains a `Notebook` with 7 tabs and manages the auto-save-on-close
/// behavior. Only one instance can be open at a time (guarded by
/// `GtkRuntime::is_open`).
///
/// **Validates: Requirements 8.1, 8.2, 8.3, 8.4**
pub struct ManagerWindow {
    /// The top-level GTK4 window.
    window: Window,
    /// The tabbed notebook containing all 7 tabs.
    notebook: Notebook,
    /// A clone of the config for save operations.
    config: RefCell<AppConfig>,
    /// The data directory for config save operations.
    data_directory: PathBuf,
    /// Callback invoked when the window is closed (sends completion to COM thread).
    on_close: RefCell<Option<Box<dyn FnOnce() + 'static>>>,
    // ─── Tab references for reading values on save ───────────────────────
    general: GeneralTab,
    filtering: FilteringTab,
    training: TrainingTab,
    #[allow(dead_code)]
    statistics: StatisticsTab,
    notifications: NotificationsTab,
    calendar: CalendarTab,
    advanced: AdvancedTab,
}

impl ManagerWindow {
    /// Build and display the Manager window with all tabs.
    ///
    /// The window size is 750×770 (matching current tkinter), with a
    /// minimum size of 600×500.
    ///
    /// **Validates: Requirement 8.1**
    pub fn new(
        state: &ManagerState,
        stats: &ManagerStats,
        config: &AppConfig,
        folder_provider: Rc<dyn FolderProvider>,
    ) -> Rc<Self> {
        // Determine data directory
        let data_directory = if config.general.data_directory.is_empty() {
            dirs_or_default()
        } else {
            PathBuf::from(&config.general.data_directory)
        };

        // ─── Build all tabs ──────────────────────────────────────────────
        let general = GeneralTab::new(state, stats, config, folder_provider.as_ref());
        let filtering = FilteringTab::new(state, Rc::clone(&folder_provider));
        let training = TrainingTab::new(state, config, Rc::clone(&folder_provider));
        let statistics = StatisticsTab::new(stats, None);
        let notifications = NotificationsTab::new(&config.notification);
        let calendar = CalendarTab::new(&config.calendar);
        let advanced = AdvancedTab::new(config, &data_directory);

        // ─── Create the window ───────────────────────────────────────────
        let window = Window::new();
        window.set_title(Some("SpamBayes Manager"));
        window.set_default_size(750, 770);
        // Set minimum size via size_request on the content
        window.set_size_request(600, 500);

        // ─── Main vertical layout ────────────────────────────────────────
        let main_vbox = GtkBox::new(Orientation::Vertical, 0);

        // 1. Header banner
        let header = create_header_banner();
        main_vbox.append(&header);

        // 2. Notebook with 7 tabs
        let notebook = Notebook::new();
        notebook.set_vexpand(true);
        notebook.set_hexpand(true);

        // Add tabs to notebook
        notebook.append_page(
            &general.container,
            Some(&Label::new(Some("General"))),
        );
        notebook.append_page(
            &filtering.container,
            Some(&Label::new(Some("Filtering"))),
        );
        notebook.append_page(
            &training.container,
            Some(&Label::new(Some("Training"))),
        );
        notebook.append_page(
            &statistics.container,
            Some(&Label::new(Some("Statistics"))),
        );
        notebook.append_page(
            &notifications.container,
            Some(&Label::new(Some("Notifications"))),
        );
        notebook.append_page(
            &calendar.container,
            Some(&Label::new(Some("Calendar"))),
        );
        notebook.append_page(
            &advanced.container,
            Some(&Label::new(Some("Advanced"))),
        );

        main_vbox.append(&notebook);

        // 3. Bottom button bar: About + Close
        let button_bar = GtkBox::new(Orientation::Horizontal, 8);
        button_bar.set_margin_top(8);
        button_bar.set_margin_bottom(8);
        button_bar.set_margin_start(12);
        button_bar.set_margin_end(12);

        let about_btn = Button::with_label("About");
        about_btn.set_tooltip_text(Some("About SpamBayes (Alt+A)"));

        let spacer = GtkBox::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);

        let close_btn = Button::with_label("Close");
        close_btn.set_tooltip_text(Some("Save and close (Alt+C)"));

        button_bar.append(&about_btn);
        button_bar.append(&spacer);
        button_bar.append(&close_btn);

        main_vbox.append(&button_bar);

        // Set the window content
        window.set_child(Some(&main_vbox));

        // ─── Build the ManagerWindow Rc ──────────────────────────────────
        let manager = Rc::new(Self {
            window,
            notebook,
            config: RefCell::new(config.clone()),
            data_directory,
            on_close: RefCell::new(None),
            general,
            filtering,
            training,
            statistics,
            notifications,
            calendar,
            advanced,
        });

        // ─── Wire close-request signal (Task 8.2: auto-save on close) ───
        {
            let mgr = Rc::clone(&manager);
            manager.window.connect_close_request(move |_win| {
                mgr.handle_close_request()
            });
        }

        // ─── Wire Close button ───────────────────────────────────────────
        {
            let mgr = Rc::clone(&manager);
            close_btn.connect_clicked(move |_| {
                mgr.window.close();
            });
        }

        // ─── Wire About button (Task 8.4) ───────────────────────────────
        {
            let mgr = Rc::clone(&manager);
            about_btn.connect_clicked(move |_| {
                mgr.show_about();
            });
        }

        // ─── Wire keyboard shortcuts (Task 8.3) ─────────────────────────
        {
            let mgr = Rc::clone(&manager);
            let key_controller = gtk4::EventControllerKey::new();
            key_controller.connect_key_pressed(move |_ctrl, keyval, _keycode, modifier| {
                mgr.handle_key_pressed(keyval, modifier)
            });
            manager.window.add_controller(key_controller);
        }

        manager
    }

    /// Set the on-close callback that sends completion back to the COM thread.
    pub fn set_on_close(&self, callback: Box<dyn FnOnce() + 'static>) {
        *self.on_close.borrow_mut() = Some(callback);
    }

    /// Set the training executor on the training tab.
    ///
    /// This must be called after construction to enable the "Start Training"
    /// button. Without this, clicking the button shows an error message.
    pub fn set_training_executor(&self, executor: Arc<dyn super::tabs::training::TrainingExecutor>) {
        self.training.set_training_executor(executor);
    }

    /// Present (show and bring to front) the window.
    ///
    /// Also loads CSS for the header banner if a display is available.
    pub fn present(&self) {
        // Load CSS on first present (display is available now)
        let display = gtk4::prelude::WidgetExt::display(&self.window);
        load_header_css(&display);
        super::tabs::general::load_general_tab_css(&display);
        self.window.present();
    }

    // ─── Task 8.2: Auto-save on close ───────────────────────────────────

    /// Handle the window's `close-request` signal.
    ///
    /// Validates all tab settings, saves to config INI, and allows the
    /// window to close on success. On validation failure, prevents close,
    /// shows an error, and switches to the failing tab.
    ///
    /// **Validates: Requirement 8.1**
    fn handle_close_request(&self) -> glib::Propagation {
        match self.apply_changes() {
            Ok(()) => {
                // Save succeeded — invoke the on_close callback and allow close
                if let Some(callback) = self.on_close.borrow_mut().take() {
                    callback();
                }
                glib::Propagation::Proceed
            }
            Err(error_msg) => {
                // Validation failed — show error and prevent close
                message_boxes::report_error(
                    Some(&self.window),
                    "SpamBayes",
                    &format!("Cannot save settings:\n\n{error_msg}"),
                );
                // Switch to the Filtering tab (index 1) where validation errors occur
                self.notebook.set_current_page(Some(1));
                glib::Propagation::Stop
            }
        }
    }

    /// Validate and save all settings from all tabs.
    ///
    /// Reads values from each tab, validates filtering thresholds, applies
    /// values to the config, and saves to INI file.
    ///
    /// Returns `Ok(())` on success, `Err(message)` on validation failure.
    ///
    /// **Validates: Requirement 8.1**
    fn apply_changes(&self) -> Result<(), String> {
        // 1. Validate filtering tab thresholds
        self.filtering.validate()?;

        // 2. Read values from all tabs
        let notification_values = self.notifications.read_values();
        let calendar_values = self.calendar.read_values();
        let advanced_values = self.advanced.read_values();
        let filter_enabled = self.general.is_filter_enabled();

        // 3. Apply values to config
        let mut config = self.config.borrow_mut();

        // General: filter enabled state
        config.filter.enabled = filter_enabled;

        // Filtering tab: read threshold values and folder IDs from widgets
        config.filter.spam_threshold = self.filtering.spam_scale.value();
        config.filter.unsure_threshold = self.filtering.unsure_scale.value();
        config.filter.spam_mark_as_read = self.filtering.spam_mark_read.is_active();
        config.filter.unsure_mark_as_read = self.filtering.unsure_mark_read.is_active();
        config.filter.ham_mark_as_read = self.filtering.ham_mark_read.is_active();
        config.filter.watch_folder_ids = self.filtering.watched_folder_ids.borrow().clone();
        config.filter.spam_folder_id = self.filtering.spam_folder_id.borrow().clone();
        config.filter.unsure_folder_id = self.filtering.unsure_folder_id.borrow().clone();
        config.filter.ham_folder_id = self.filtering.ham_folder_id.borrow().clone();

        // Filtering tab: actions from combo boxes
        config.filter.spam_action = combo_to_filter_action(&self.filtering.spam_action_combo);
        config.filter.unsure_action = combo_to_filter_action(&self.filtering.unsure_action_combo);
        config.filter.ham_action = combo_to_filter_action(&self.filtering.ham_action_combo);

        // Filtering tab: auto-cleanup
        config.filter.spam_auto_cleanup_enabled = self.filtering.cleanup_enabled.is_active();
        config.filter.spam_auto_cleanup_days = self.filtering.cleanup_days_spin.value_as_int() as u32;

        // Notifications tab
        config.notification = notification_values;

        // Calendar tab
        config.calendar = calendar_values;

        // Advanced tab: timer settings
        config.filter.timer_enabled = advanced_values.timer_enabled;
        config.filter.timer_start_delay = advanced_values.timer_start_delay;
        config.filter.timer_interval = advanced_values.timer_interval;
        config.filter.timer_only_receive_folders = advanced_values.timer_only_receive_folders;
        config.general.verbose = advanced_values.verbose;
        config.filter.save_spam_info = advanced_values.save_spam_info;

        // Training tab
        config.training.ham_folder_ids = self.training.ham_folder_ids.borrow().clone();
        config.training.spam_folder_ids = self.training.spam_folder_ids.borrow().clone();
        config.training.rescore = self.training.rescore_check.is_active();
        config.training.rebuild = self.training.rebuild_check.is_active();
        config.training.train_recovered_spam = self.training.train_recovered_spam_check.is_active();
        config.training.train_manual_spam = self.training.train_manual_spam_check.is_active();

        // 4. Save config to INI file
        config
            .save(&self.data_directory, "default")
            .map_err(|e| format!("Failed to save configuration: {e}"))?;

        Ok(())
    }

    // ─── Task 8.3: Keyboard shortcuts ────────────────────────────────────

    /// Handle key-pressed events for keyboard shortcuts.
    ///
    /// - F1 = About/Help
    /// - Alt+A = About
    /// - Alt+C = Close
    /// - Escape = Close
    /// - Alt+R = Run Filter Now
    /// - Alt+T = Start Training
    ///
    /// **Validates: Requirement 8.2**
    fn handle_key_pressed(
        &self,
        keyval: gdk::Key,
        modifier: gdk::ModifierType,
    ) -> glib::Propagation {
        let alt = modifier.contains(gdk::ModifierType::ALT_MASK);

        match keyval {
            gdk::Key::F1 => {
                self.show_about();
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                self.window.close();
                glib::Propagation::Stop
            }
            gdk::Key::a | gdk::Key::A if alt => {
                self.show_about();
                glib::Propagation::Stop
            }
            gdk::Key::c | gdk::Key::C if alt => {
                self.window.close();
                glib::Propagation::Stop
            }
            gdk::Key::r | gdk::Key::R if alt => {
                // Switch to General tab and trigger Run Filter Now
                self.notebook.set_current_page(Some(0));
                self.general.run_filter_btn.emit_clicked();
                glib::Propagation::Stop
            }
            gdk::Key::t | gdk::Key::T if alt => {
                // Switch to Training tab and trigger Start Training
                self.notebook.set_current_page(Some(2));
                self.training.start_training_btn.emit_clicked();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    // ─── Task 8.4: About dialog ──────────────────────────────────────────

    /// Show the About dialog.
    ///
    /// Displays version, description, and credits matching the current
    /// tkinter `show_about()` function.
    ///
    /// **Validates: Requirement 8.3**
    fn show_about(&self) {
        let about = gtk4::AboutDialog::new();
        about.set_transient_for(Some(&self.window));
        about.set_modal(true);
        about.set_program_name(Some("SpamBayes X64"));
        about.set_version(Some("0.3.0a1"));
        about.set_comments(Some(
            "Anti-spam Classifier for Microsoft Outlook\n\n\
             A Bayesian anti-spam filter integrated with Microsoft Outlook.\n\
             Classifies incoming email as spam, ham, or unsure using \
             statistical analysis of message content.",
        ));
        about.set_copyright(Some(
            "Copyright © 2026 Doug Farrell\n\
             Based on SpamBayes but a complete rewrite in Rust",
        ));
        about.set_website(Some("https://github.com/cyberblob/SpamBayes_Rust"));
        about.set_website_label("SpamBayes on GitHub");
        about.set_authors(&[
            "Doug Farrell and Kiro.dev",
        ]);
        about.set_license_type(gtk4::License::Custom);
        about.set_license(Some(
            "Licensed under the MIT License.",
        ));
        about.present();
    }
}

// ─── Helper Functions ────────────────────────────────────────────────────────

/// Convert a ComboBoxText active value to a `FilterAction`.
///
/// Mapping: 0 = Move, 1 = Copy, 2 = Untouched.
#[allow(deprecated)]
fn combo_to_filter_action(combo: &gtk4::ComboBoxText) -> spambayes_config::FilterAction {
    use spambayes_config::FilterAction;
    match combo.active() {
        Some(0) => FilterAction::Move,
        Some(1) => FilterAction::Copy,
        _ => FilterAction::Untouched,
    }
}

/// Get a default data directory path.
///
/// Falls back to `./SpamBayes` in the current directory if no
/// standard location is available.
fn dirs_or_default() -> PathBuf {
    // Try LOCALAPPDATA on Windows
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local_app_data).join("SpamBayes");
    }
    // Fallback
    PathBuf::from("SpamBayes")
}
