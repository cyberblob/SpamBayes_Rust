//! General tab — filter status, database health, and quick actions.
//!
//! Displays version information, training database status (ham/spam counts
//! with imbalance warning), filter status (active/paused indicator, folder
//! names), "Run Filter Now" button with inline progress, "Enable SpamBayes"
//! checkbox, and configuration buttons.
//!
//! **Validates: Requirements 1.2–1.8**

use std::f64::consts::PI;

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, CheckButton, DrawingArea, Frame, Label, Orientation,
    ProgressBar, ScrolledWindow, Separator,
};

use spambayes_config::AppConfig;

use crate::gui::folder_browser::{FolderBrowserDialog, FolderProvider};
use crate::gui::wizard_window::WizardWindow;
use crate::manager_dlg::{ManagerState, ManagerStats};

// ─── CSS class names ─────────────────────────────────────────────────────────

const CSS_STATUS_ACTIVE: &str = "status-active";
const CSS_STATUS_PAUSED: &str = "status-paused";
const CSS_IMBALANCE_WARNING: &str = "imbalance-warning";

/// CSS for General-tab-specific styling.
///
/// Loaded via [`load_general_tab_css`] at application startup.
pub const GENERAL_TAB_CSS: &str = r#"
.status-active {
    color: #2E7D32;
    font-weight: bold;
}

.status-paused {
    color: #C62828;
    font-weight: bold;
}

.imbalance-warning {
    color: #E65100;
    font-weight: bold;
    font-size: 9pt;
}

.section-title {
    font-weight: bold;
    font-size: 10pt;
}

.version-label {
    font-size: 9pt;
    color: #555555;
}

.training-hint {
    font-size: 9pt;
    color: #666666;
}
"#;

/// Register General-tab CSS with the given display.
///
/// Should be called once during application startup alongside
/// [`super::super::manager_window::load_header_css`].
pub fn load_general_tab_css(display: &gtk4::gdk::Display) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(GENERAL_TAB_CSS);
    gtk4::style_context_add_provider_for_display(
        display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ─── GeneralTab ──────────────────────────────────────────────────────────────

/// The General tab content.
///
/// Contains status indicators, database health metrics, and quick-action
/// buttons. Widgets that need to be read on save or updated dynamically
/// are stored as struct fields.
///
/// **Validates: Requirements 1.2–1.8**
pub struct GeneralTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Configuration ───────────────────────────────────────────────────
    /// Copy of the app config for launching the wizard.
    #[allow(dead_code)]
    config: AppConfig,

    // ─── Dynamic labels ──────────────────────────────────────────────────
    /// "Database has X good and Y spam." label.
    db_status_label: Label,
    /// Imbalance warning label (hidden when balanced).
    imbalance_label: Label,
    /// Filter status text ("Filter is ACTIVE" / "Filter is PAUSED").
    filter_status_label: Label,
    /// Status indicator dot (green=active, red=paused).
    status_dot: DrawingArea,
    /// Watched folder name label.
    #[allow(dead_code)]
    watched_folder_label: Label,
    /// Spam folder name label.
    #[allow(dead_code)]
    spam_folder_label: Label,
    /// Unsure folder name label.
    #[allow(dead_code)]
    unsure_folder_label: Label,

    // ─── Run Filter Now ──────────────────────────────────────────────────
    /// "Run Filter Now" button.
    pub run_filter_btn: Button,
    /// Progress bar (hidden until filter is running).
    pub filter_progress: ProgressBar,
    /// Results summary label (hidden until filter completes).
    pub filter_results_label: Label,

    // ─── Persistent controls ─────────────────────────────────────────────
    /// "Enable SpamBayes" checkbox — read on save.
    pub enable_checkbox: CheckButton,
    /// "Reset Configuration..." button.
    pub reset_config_btn: Button,
    /// "Configuration Wizard..." button.
    pub wizard_btn: Button,
}

impl GeneralTab {
    /// Build the General tab widget tree from the given state and stats.
    ///
    /// # Arguments
    ///
    /// * `state` – Current manager state (filter enabled, folder IDs, etc.)
    /// * `stats` – Classifier statistics (ham/spam trained counts)
    /// * `config` – Application configuration (used for launching the wizard)
    /// * `folder_provider` – Provider for resolving folder IDs to display names
    ///
    /// **Validates: Requirements 1.2–1.8**
    #[must_use]
    pub fn new(state: &ManagerState, stats: &ManagerStats, config: &AppConfig, folder_provider: &dyn FolderProvider) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Version info (Req 1.2) ──────────────────────────────────
        let version_label = Label::new(Some(&format!(
            "SpamBayes Outlook Add-in Version {}",
            env!("CARGO_PKG_VERSION")
        )));
        version_label.add_css_class("version-label");
        version_label.set_halign(Align::Start);
        content_box.append(&version_label);

        // Training requirement hint
        let hint_label = Label::new(Some(
            "SpamBayes requires training before it can classify messages.\n\
             Use the Training tab to train on known good and spam messages.",
        ));
        hint_label.add_css_class("training-hint");
        hint_label.set_halign(Align::Start);
        hint_label.set_wrap(true);
        content_box.append(&hint_label);

        // ─── 2. Training database status (Req 1.3) ──────────────────────
        let db_frame = Frame::new(Some("Training database status:"));
        let db_box = GtkBox::new(Orientation::Vertical, 6);
        db_box.set_margin_top(8);
        db_box.set_margin_bottom(8);
        db_box.set_margin_start(12);
        db_box.set_margin_end(12);

        let db_status_label = Label::new(Some(&format!(
            "Database has {} good and {} spam.",
            stats.ham_trained, stats.spam_trained
        )));
        db_status_label.set_halign(Align::Start);
        db_box.append(&db_status_label);

        // Imbalance warning (show if one category > 2x the other)
        let imbalance_label = Label::new(None);
        imbalance_label.add_css_class(CSS_IMBALANCE_WARNING);
        imbalance_label.set_halign(Align::Start);
        imbalance_label.set_wrap(true);
        Self::update_imbalance_warning(&imbalance_label, stats.ham_trained, stats.spam_trained);
        db_box.append(&imbalance_label);

        db_frame.set_child(Some(&db_box));
        content_box.append(&db_frame);

        // ─── 3. Filter status (Req 1.4) ─────────────────────────────────
        let filter_frame = Frame::new(Some("Filter status:"));
        let filter_box = GtkBox::new(Orientation::Vertical, 6);
        filter_box.set_margin_top(8);
        filter_box.set_margin_bottom(8);
        filter_box.set_margin_start(12);
        filter_box.set_margin_end(12);

        // Status indicator row: colored dot + text
        let status_row = GtkBox::new(Orientation::Horizontal, 8);
        status_row.set_valign(Align::Center);

        let status_dot = DrawingArea::new();
        status_dot.set_size_request(12, 12);
        status_dot.set_valign(Align::Center);

        let is_active = state.filter_enabled;
        // Draw the colored circle
        Self::setup_status_dot(&status_dot, is_active);

        let filter_status_label = Label::new(Some(if is_active {
            "Filter is ACTIVE"
        } else {
            "Filter is PAUSED"
        }));
        if is_active {
            filter_status_label.add_css_class(CSS_STATUS_ACTIVE);
        } else {
            filter_status_label.add_css_class(CSS_STATUS_PAUSED);
        }

        status_row.append(&status_dot);
        status_row.append(&filter_status_label);
        filter_box.append(&status_row);

        // Folder name labels
        let watched_folder_label = Label::new(Some(&Self::format_watched_folders(state, folder_provider)));
        watched_folder_label.set_halign(Align::Start);
        watched_folder_label.set_wrap(true);
        filter_box.append(&watched_folder_label);

        let spam_folder_label = Label::new(Some(&Self::format_spam_folder(state, folder_provider)));
        spam_folder_label.set_halign(Align::Start);
        filter_box.append(&spam_folder_label);

        let unsure_folder_label = Label::new(Some(&Self::format_unsure_folder(state, folder_provider)));
        unsure_folder_label.set_halign(Align::Start);
        filter_box.append(&unsure_folder_label);

        // ─── "Run Filter Now" button + progress (Req 1.5) ───────────────
        let filter_action_box = GtkBox::new(Orientation::Vertical, 6);
        filter_action_box.set_margin_top(8);

        let run_filter_btn = Button::with_label("Run Filter Now");
        run_filter_btn.set_halign(Align::Start);
        filter_action_box.append(&run_filter_btn);

        let filter_progress = ProgressBar::new();
        filter_progress.set_visible(false);
        filter_progress.set_show_text(true);
        filter_action_box.append(&filter_progress);

        let filter_results_label = Label::new(None);
        filter_results_label.set_visible(false);
        filter_results_label.set_halign(Align::Start);
        filter_results_label.set_wrap(true);
        filter_action_box.append(&filter_results_label);

        filter_box.append(&filter_action_box);
        filter_frame.set_child(Some(&filter_box));
        content_box.append(&filter_frame);

        // ─── 4. Separator ────────────────────────────────────────────────
        let sep1 = Separator::new(Orientation::Horizontal);
        sep1.set_margin_top(4);
        sep1.set_margin_bottom(4);
        content_box.append(&sep1);

        // ─── 5. Enable SpamBayes checkbox (Req 1.6) ─────────────────────
        let enable_checkbox = CheckButton::with_label("Enable SpamBayes");
        enable_checkbox.set_active(state.filter_enabled);
        content_box.append(&enable_checkbox);

        // ─── 6. Separator ────────────────────────────────────────────────
        let sep2 = Separator::new(Orientation::Horizontal);
        sep2.set_margin_top(4);
        sep2.set_margin_bottom(4);
        content_box.append(&sep2);

        // ─── 7. Button row (Req 1.7) ────────────────────────────────────
        let button_row = GtkBox::new(Orientation::Horizontal, 12);
        button_row.set_halign(Align::Start);

        let reset_config_btn = Button::with_label("Reset Configuration...");
        let wizard_btn = Button::with_label("Configuration Wizard...");

        button_row.append(&reset_config_btn);
        button_row.append(&wizard_btn);
        content_box.append(&button_row);

        // ─── Wire button signals (placeholder logging) ───────────────────
        run_filter_btn.connect_clicked(|_btn| {
            log::info!("Run Filter Now clicked (TODO: implement in task 10.x)");
        });

        reset_config_btn.connect_clicked(|_btn| {
            log::info!("Reset Configuration clicked (TODO: wire to reset logic)");
        });

        wizard_btn.connect_clicked({
            let config = config.clone();
            move |_btn| {
                log::info!("Configuration Wizard launched from Manager.");
                let wizard = WizardWindow::new(&config);
                wizard.connect_signals(Some(Box::new(|result| {
                    match result {
                        crate::gui::wizard_window::WizardResult::Completed {
                            spam_folder,
                            unsure_folder,
                        } => {
                            log::info!(
                                "Wizard completed: spam_folder={}, unsure_folder={}",
                                spam_folder,
                                unsure_folder
                            );
                        }
                        crate::gui::wizard_window::WizardResult::Cancelled => {
                            log::info!("Wizard cancelled by user.");
                        }
                    }
                })));
                wizard.present();
            }
        });

        // ─── 8. ScrolledWindow wrapper (Req 1.8) ────────────────────────
        let container = ScrolledWindow::new();
        container.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        container.set_child(Some(&content_box));
        container.set_vexpand(true);
        container.set_hexpand(true);

        Self {
            container,
            config: config.clone(),
            db_status_label,
            imbalance_label,
            filter_status_label,
            status_dot,
            watched_folder_label,
            spam_folder_label,
            unsure_folder_label,
            run_filter_btn,
            filter_progress,
            filter_results_label,
            enable_checkbox,
            reset_config_btn,
            wizard_btn,
        }
    }

    /// Returns whether the "Enable SpamBayes" checkbox is active.
    #[must_use]
    pub fn is_filter_enabled(&self) -> bool {
        self.enable_checkbox.is_active()
    }

    /// Update the filter status indicator and labels.
    ///
    /// Call this when the enable checkbox is toggled to keep the status
    /// section in sync.
    pub fn update_filter_status(&self, enabled: bool) {
        Self::setup_status_dot(&self.status_dot, enabled);
        self.status_dot.queue_draw();

        if enabled {
            self.filter_status_label.set_text("Filter is ACTIVE");
            self.filter_status_label.remove_css_class(CSS_STATUS_PAUSED);
            self.filter_status_label.add_css_class(CSS_STATUS_ACTIVE);
        } else {
            self.filter_status_label.set_text("Filter is PAUSED");
            self.filter_status_label.remove_css_class(CSS_STATUS_ACTIVE);
            self.filter_status_label.add_css_class(CSS_STATUS_PAUSED);
        }
    }

    /// Update the training database status labels.
    pub fn update_db_status(&self, ham_trained: u64, spam_trained: u64) {
        self.db_status_label.set_text(&format!(
            "Database has {} good and {} spam.",
            ham_trained, spam_trained
        ));
        Self::update_imbalance_warning(&self.imbalance_label, ham_trained, spam_trained);
    }

    /// Show filter results after "Run Filter Now" completes.
    pub fn show_filter_results(&self, total: u32, spam: u32, ham: u32, unsure: u32) {
        self.filter_progress.set_visible(false);
        self.filter_results_label.set_text(&format!(
            "Filtered {} messages: {} spam, {} good, {} unsure.",
            total, spam, ham, unsure
        ));
        self.filter_results_label.set_visible(true);
    }

    /// Show/hide the progress bar and set its fraction.
    pub fn set_filter_progress(&self, fraction: f64, visible: bool) {
        self.filter_progress.set_visible(visible);
        self.filter_progress.set_fraction(fraction);
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Configure the status dot `DrawingArea` to draw a filled circle.
    fn setup_status_dot(dot: &DrawingArea, active: bool) {
        let color = if active {
            (0.18, 0.49, 0.20) // green (#2E7D32)
        } else {
            (0.78, 0.16, 0.16) // red (#C62828)
        };

        dot.set_draw_func(move |_area, cr, width, height| {
            let cx = f64::from(width) / 2.0;
            let cy = f64::from(height) / 2.0;
            let radius = f64::from(width.min(height)) / 2.0 - 1.0;

            cr.arc(cx, cy, radius, 0.0, 2.0 * PI);
            cr.set_source_rgb(color.0, color.1, color.2);
            let _ = cr.fill();
        });
    }

    /// Update the imbalance warning label visibility and text.
    fn update_imbalance_warning(label: &Label, ham: u64, spam: u64) {
        if ham == 0 && spam == 0 {
            label.set_visible(false);
            return;
        }

        let show_warning = if ham > 0 && spam > 0 {
            ham > spam * 2 || spam > ham * 2
        } else {
            // One category is zero but the other isn't — that's an imbalance
            true
        };

        if show_warning {
            label.set_text(
                "Warning: The training database is imbalanced. \
                 For best results, train with roughly equal amounts of good and spam.",
            );
            label.set_visible(true);
        } else {
            label.set_visible(false);
        }
    }

    /// Format watched folder display text from state.
    /// Shows "Watching 'account/folder'; 'account/folder2'." with resolved names.
    fn format_watched_folders(state: &ManagerState, provider: &dyn FolderProvider) -> String {
        if state.watch_folder_ids.is_empty() {
            "Watching: (not configured)".to_string()
        } else {
            let names = FolderBrowserDialog::resolve_folder_names(provider, &state.watch_folder_ids);
            let display = names.join("; ");
            format!("Watching '{}'.", display)
        }
    }

    /// Format spam folder display text from state.
    fn format_spam_folder(state: &ManagerState, provider: &dyn FolderProvider) -> String {
        if let Some(ref folder_id) = state.spam_folder_id {
            let names = FolderBrowserDialog::resolve_folder_names(provider, &[folder_id.clone()]);
            let name = names.into_iter().next().unwrap_or_else(|| "(configured)".to_string());
            format!("Spam managed in '{}'.", name)
        } else {
            "Spam folder: (not configured)".to_string()
        }
    }

    /// Format unsure folder display text from state.
    fn format_unsure_folder(state: &ManagerState, provider: &dyn FolderProvider) -> String {
        if let Some(ref folder_id) = state.unsure_folder_id {
            let names = FolderBrowserDialog::resolve_folder_names(provider, &[folder_id.clone()]);
            let name = names.into_iter().next().unwrap_or_else(|| "(configured)".to_string());
            format!("Unsure managed in '{}'.", name)
        } else {
            "Unsure folder: (not configured)".to_string()
        }
    }
}
