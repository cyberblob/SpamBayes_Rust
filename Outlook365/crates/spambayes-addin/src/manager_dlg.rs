//! `SpamBayes` Manager Dialog.
//!
//! Implements the Manager dialog accessible from the `SpamBayes` toolbar or
//! Outlook Tools menu. Provides classifier statistics display, filter
//! settings adjustment, folder selection, and training/filter-now operations.
//!
//! # Requirements
//!
//! - Req 14.1: Accessible from toolbar or Tools menu
//! - Req 14.2: Display classifier statistics
//! - Req 14.3: Allow changing filter settings (thresholds, actions)
//! - Req 14.4: Initiate training and Filter Now with progress
//! - Req 14.5: Enable/disable filtering via checkbox
//! - Req 14.6: Select folders via MAPI folder picker
//! - Req 14.7: Save changed settings on dialog close

#![cfg(target_os = "windows")]

use spambayes_config::{AppConfig, ConfigChain, FilterAction, FolderId};

use crate::statistics::StatisticsManager;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EndDialog, GetWindowLongPtrW, KillTimer, MessageBoxW, SetDlgItemTextW, SetTimer,
    SetWindowLongPtrW, GWLP_USERDATA, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO,
    WM_CLOSE, WM_COMMAND, WM_INITDIALOG, WM_TIMER,
};

// ─── Dialog Control ID Constants ─────────────────────────────────────────────

/// Dialog resource ID for the Manager dialog.
#[allow(dead_code)]
const IDD_MANAGER: u32 = 3000;

// Statistics display controls — Training Statistics section (Req 3.1)
#[allow(dead_code)]
const IDC_STAT_HAM_TRAINED: u16 = 3001;
#[allow(dead_code)]
const IDC_STAT_SPAM_TRAINED: u16 = 3002;
#[allow(dead_code)]
const IDC_STAT_SESSION_CLASSIFIED: u16 = 3003;

// Statistics display controls — Session Activity section (Req 3.2)
#[allow(dead_code)]
const IDC_STAT_SESSION_HAM: u16 = 3004;
#[allow(dead_code)]
const IDC_STAT_SESSION_UNSURE: u16 = 3005;
#[allow(dead_code)]
const IDC_STAT_SESSION_SPAM: u16 = 3006;

// Statistics display controls — Lifetime Classification section (Req 3.3)
#[allow(dead_code)]
const IDC_STAT_LIFETIME_HAM_CLASSIFIED: u16 = 3007;
#[allow(dead_code)]
const IDC_STAT_LIFETIME_UNSURE_CLASSIFIED: u16 = 3008;
#[allow(dead_code)]
const IDC_STAT_LIFETIME_SPAM_CLASSIFIED: u16 = 3009;

// Filter settings controls
#[allow(dead_code)]
const IDC_SPAM_THRESHOLD: u16 = 3010;
#[allow(dead_code)]
const IDC_UNSURE_THRESHOLD: u16 = 3011;
#[allow(dead_code)]
const IDC_SPAM_ACTION: u16 = 3012;
#[allow(dead_code)]
const IDC_UNSURE_ACTION: u16 = 3013;
#[allow(dead_code)]
const IDC_HAM_ACTION: u16 = 3014;
#[allow(dead_code)]
const IDC_ENABLE_FILTERING: u16 = 3015;

// Folder selection controls
#[allow(dead_code)]
const IDC_WATCH_FOLDERS: u16 = 3020;
#[allow(dead_code)]
const IDC_SPAM_FOLDER: u16 = 3021;
#[allow(dead_code)]
const IDC_UNSURE_FOLDER: u16 = 3022;
#[allow(dead_code)]
const IDC_HAM_TRAIN_FOLDERS: u16 = 3023;
#[allow(dead_code)]
const IDC_SPAM_TRAIN_FOLDERS: u16 = 3024;

// Browse buttons for folder pickers
#[allow(dead_code)]
const IDC_BROWSE_WATCH: u16 = 3030;
#[allow(dead_code)]
const IDC_BROWSE_SPAM_FOLDER: u16 = 3031;
#[allow(dead_code)]
const IDC_BROWSE_UNSURE_FOLDER: u16 = 3032;
#[allow(dead_code)]
const IDC_BROWSE_HAM_TRAIN: u16 = 3033;
#[allow(dead_code)]
const IDC_BROWSE_SPAM_TRAIN: u16 = 3034;

// Action buttons
#[allow(dead_code)]
const IDC_TRAIN_NOW: u16 = 3040;
#[allow(dead_code)]
const IDC_FILTER_NOW: u16 = 3041;
#[allow(dead_code)]
const IDC_OK: u16 = 3042;
#[allow(dead_code)]
const IDC_CANCEL: u16 = 3043;
#[allow(dead_code)]
const IDC_RESET_STATS: u16 = 3044;

// Timer constants for real-time statistics refresh (Req 3.4)
/// Timer ID for periodic statistics refresh while the Manager dialog is open.
const IDC_STATS_TIMER: usize = 3100;
/// Refresh interval in milliseconds for the statistics timer (2 seconds).
const STATS_REFRESH_MS: u32 = 2000;

// ─── ManagerStats ────────────────────────────────────────────────────────────

/// Classifier statistics displayed in the Manager dialog.
///
/// **Validates: Requirements 14.2, 3.1, 3.2, 3.3**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerStats {
    // ─── Lifetime Training (Req 3.1) ─────────────────────────────────────
    /// Lifetime count of ham messages trained.
    pub ham_trained: u64,
    /// Lifetime count of spam messages trained.
    pub spam_trained: u64,

    // ─── Session Training ────────────────────────────────────────────────
    /// Ham messages trained in the current session.
    pub session_ham_trained: u32,
    /// Spam messages trained in the current session.
    pub session_spam_trained: u32,

    // ─── Session Classification (Req 3.2) ────────────────────────────────
    /// Total messages classified in the current Outlook session.
    pub session_classified: u32,
    /// Ham messages classified in the current session.
    pub session_ham_classified: u32,
    /// Unsure messages classified in the current session.
    pub session_unsure_classified: u32,
    /// Spam messages classified in the current session.
    pub session_spam_classified: u32,

    // ─── Lifetime Classification (Req 3.3) ───────────────────────────────
    /// Lifetime count of ham messages classified.
    pub total_ham_classified: u64,
    /// Lifetime count of unsure messages classified.
    pub total_unsure_classified: u64,
    /// Lifetime count of spam messages classified.
    pub total_spam_classified: u64,

    // ─── Accuracy Tracking (Req 4.2) ────────────────────────────────────
    /// Count of messages confirmed as correctly classified.
    pub correctly_classified: u64,
    /// Count of false positives (ham incorrectly classified as spam).
    pub false_positives: u64,
    /// Count of false negatives (spam incorrectly classified as ham).
    pub false_negatives: u64,

    // ─── Manual Classification (Req 4.3) ─────────────────────────────────
    /// Count of messages manually classified as good by the user.
    pub manually_classified_good: u64,
    /// Count of messages manually classified as spam by the user.
    pub manually_classified_spam: u64,

    // ─── Reset Tracking (Req 4.5) ────────────────────────────────────────
    /// Date of last statistics reset (ISO 8601 string), or None if never reset.
    pub last_reset_date: Option<String>,
}

impl ManagerStats {
    /// Create stats with zero values.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ham_trained: 0,
            spam_trained: 0,
            session_ham_trained: 0,
            session_spam_trained: 0,
            session_classified: 0,
            session_ham_classified: 0,
            session_unsure_classified: 0,
            session_spam_classified: 0,
            total_ham_classified: 0,
            total_unsure_classified: 0,
            total_spam_classified: 0,
            correctly_classified: 0,
            false_positives: 0,
            false_negatives: 0,
            manually_classified_good: 0,
            manually_classified_spam: 0,
            last_reset_date: None,
        }
    }

    /// Create stats from specific values (legacy convenience constructor).
    ///
    /// Populates lifetime training counts and total session classified.
    /// Other fields default to zero.
    #[must_use]
    pub fn with_values(ham_trained: u64, spam_trained: u64, session_classified: u32) -> Self {
        Self {
            ham_trained,
            spam_trained,
            session_classified,
            ..Self::new()
        }
    }

    /// Build `ManagerStats` from a `StatisticsManager` by reading both
    /// session and lifetime snapshots.
    ///
    /// **Validates: Requirements 3.1, 3.2, 3.3**
    #[must_use]
    pub fn from_statistics(stats_mgr: &StatisticsManager) -> Self {
        let session = stats_mgr.session_stats();
        let lifetime = stats_mgr.lifetime_stats();

        let session_classified =
            session.ham_classified + session.unsure_classified + session.spam_classified;

        Self {
            ham_trained: lifetime.total_ham_trained,
            spam_trained: lifetime.total_spam_trained,
            session_ham_trained: session.ham_trained,
            session_spam_trained: session.spam_trained,
            session_classified,
            session_ham_classified: session.ham_classified,
            session_unsure_classified: session.unsure_classified,
            session_spam_classified: session.spam_classified,
            total_ham_classified: lifetime.total_ham_classified,
            total_unsure_classified: lifetime.total_unsure_classified,
            total_spam_classified: lifetime.total_spam_classified,
            correctly_classified: 0,
            false_positives: 0,
            false_negatives: 0,
            manually_classified_good: 0,
            manually_classified_spam: 0,
            last_reset_date: None,
        }
    }
}

impl Default for ManagerStats {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Formatting Helpers ──────────────────────────────────────────────────────

/// Format a number with thousands separators (comma-delimited).
///
/// Example: `1523` → `"1,523"`, `0` → `"0"`, `1000000` → `"1,000,000"`
#[must_use]
pub fn format_with_thousands(n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }

    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    // Pre-allocate with room for commas.
    let mut result = String::with_capacity(len + (len - 1) / 3);

    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(b as char);
    }

    result
}

/// Format a statistic combining lifetime total and session count.
///
/// Produces output like: `"1,523 (session: 12)"` or `"0 (session: 0)"`.
///
/// **Validates: Requirements 3.1, 3.2, 3.3**
#[must_use]
pub fn format_stat(lifetime: u64, session: u32) -> String {
    format!(
        "{} (session: {})",
        format_with_thousands(lifetime),
        format_with_thousands(u64::from(session))
    )
}

// ─── ManagerState ────────────────────────────────────────────────────────────

/// Working copy of editable settings within the Manager dialog.
///
/// This struct holds a mutable copy of configuration values that the user
/// can change through the dialog. When the dialog is closed via OK,
/// the state is applied back to the `AppConfig` and saved to disk.
///
/// **Validates: Requirements 14.3, 14.5, 14.6**
#[derive(Debug, Clone)]
pub struct ManagerState {
    /// Whether filtering is enabled (checkbox).
    ///
    /// **Validates: Requirement 14.5**
    pub filter_enabled: bool,

    /// Spam threshold percentage (0.0–100.0).
    pub spam_threshold: f64,

    /// Unsure threshold percentage (0.0–100.0).
    pub unsure_threshold: f64,

    /// Action for spam-classified messages.
    pub spam_action: FilterAction,

    /// Action for unsure-classified messages.
    pub unsure_action: FilterAction,

    /// Action for ham-classified messages.
    pub ham_action: FilterAction,

    /// Watched folder IDs (folders to monitor for incoming mail).
    pub watch_folder_ids: Vec<FolderId>,

    /// Spam destination folder.
    pub spam_folder_id: Option<FolderId>,

    /// Unsure destination folder.
    pub unsure_folder_id: Option<FolderId>,

    /// Ham training folder IDs.
    pub ham_training_folder_ids: Vec<FolderId>,

    /// Spam training folder IDs.
    pub spam_training_folder_ids: Vec<FolderId>,

    /// Whether the state has been modified from the original config.
    dirty: bool,
}

impl ManagerState {
    /// Create a `ManagerState` from the current application config.
    ///
    /// Copies the relevant settings from `AppConfig` into an editable
    /// working copy.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            filter_enabled: config.filter.enabled,
            spam_threshold: config.filter.spam_threshold,
            unsure_threshold: config.filter.unsure_threshold,
            spam_action: config.filter.spam_action.clone(),
            unsure_action: config.filter.unsure_action.clone(),
            ham_action: config.filter.ham_action.clone(),
            watch_folder_ids: config.filter.watch_folder_ids.clone(),
            spam_folder_id: config.filter.spam_folder_id.clone(),
            unsure_folder_id: config.filter.unsure_folder_id.clone(),
            ham_training_folder_ids: config.training.ham_folder_ids.clone(),
            spam_training_folder_ids: config.training.spam_folder_ids.clone(),
            dirty: false,
        }
    }

    /// Apply this state's values back to an `AppConfig`.
    ///
    /// Updates the config with any values changed via the dialog.
    ///
    /// **Validates: Requirement 14.7**
    pub fn apply_to_config(&self, config: &mut AppConfig) {
        config.filter.enabled = self.filter_enabled;
        config.filter.spam_threshold = self.spam_threshold;
        config.filter.unsure_threshold = self.unsure_threshold;
        config.filter.spam_action = self.spam_action.clone();
        config.filter.unsure_action = self.unsure_action.clone();
        config.filter.ham_action = self.ham_action.clone();
        config.filter.watch_folder_ids = self.watch_folder_ids.clone();
        config.filter.spam_folder_id = self.spam_folder_id.clone();
        config.filter.unsure_folder_id = self.unsure_folder_id.clone();
        config.training.ham_folder_ids = self.ham_training_folder_ids.clone();
        config.training.spam_folder_ids = self.spam_training_folder_ids.clone();
    }

    /// Mark the state as modified.
    pub fn set_dirty(&mut self) {
        self.dirty = true;
    }

    /// Returns whether any settings have been modified.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

impl ManagerState {
    /// Set the spam threshold, clamping to valid range and marking dirty.
    ///
    /// **Validates: Requirement 14.3**
    pub fn set_spam_threshold(&mut self, value: f64) {
        let clamped = value.clamp(0.0, 100.0);
        if (clamped - self.spam_threshold).abs() > f64::EPSILON {
            self.spam_threshold = clamped;
            self.dirty = true;
        }
    }

    /// Set the unsure threshold, clamping to valid range and marking dirty.
    ///
    /// **Validates: Requirement 14.3**
    pub fn set_unsure_threshold(&mut self, value: f64) {
        let clamped = value.clamp(0.0, 100.0);
        if (clamped - self.unsure_threshold).abs() > f64::EPSILON {
            self.unsure_threshold = clamped;
            self.dirty = true;
        }
    }

    /// Set the spam action and mark dirty if changed.
    ///
    /// **Validates: Requirement 14.3**
    pub fn set_spam_action(&mut self, action: FilterAction) {
        if self.spam_action != action {
            self.spam_action = action;
            self.dirty = true;
        }
    }

    /// Set the unsure action and mark dirty if changed.
    ///
    /// **Validates: Requirement 14.3**
    pub fn set_unsure_action(&mut self, action: FilterAction) {
        if self.unsure_action != action {
            self.unsure_action = action;
            self.dirty = true;
        }
    }

    /// Set the ham action and mark dirty if changed.
    pub fn set_ham_action(&mut self, action: FilterAction) {
        if self.ham_action != action {
            self.ham_action = action;
            self.dirty = true;
        }
    }

    /// Toggle filtering enabled and mark dirty.
    ///
    /// **Validates: Requirement 14.5**
    pub fn set_filter_enabled(&mut self, enabled: bool) {
        if self.filter_enabled != enabled {
            self.filter_enabled = enabled;
            self.dirty = true;
        }
    }

    /// Set watched folder IDs and mark dirty.
    ///
    /// **Validates: Requirement 14.6**
    pub fn set_watch_folders(&mut self, folders: Vec<FolderId>) {
        self.watch_folder_ids = folders;
        self.dirty = true;
    }

    /// Set the spam destination folder and mark dirty.
    ///
    /// **Validates: Requirement 14.6**
    pub fn set_spam_folder(&mut self, folder_id: Option<FolderId>) {
        self.spam_folder_id = folder_id;
        self.dirty = true;
    }

    /// Set the unsure destination folder and mark dirty.
    ///
    /// **Validates: Requirement 14.6**
    pub fn set_unsure_folder(&mut self, folder_id: Option<FolderId>) {
        self.unsure_folder_id = folder_id;
        self.dirty = true;
    }

    /// Set ham training folders and mark dirty.
    ///
    /// **Validates: Requirement 14.6**
    pub fn set_ham_training_folders(&mut self, folders: Vec<FolderId>) {
        self.ham_training_folder_ids = folders;
        self.dirty = true;
    }

    /// Set spam training folders and mark dirty.
    ///
    /// **Validates: Requirement 14.6**
    pub fn set_spam_training_folders(&mut self, folders: Vec<FolderId>) {
        self.spam_training_folder_ids = folders;
        self.dirty = true;
    }
}

// ─── ManagerDialog ───────────────────────────────────────────────────────────

/// The Manager dialog controller.
///
/// Manages the Win32 dialog lifecycle, displays classifier statistics,
/// and coordinates settings changes, training, and Filter Now operations.
///
/// **Validates: Requirements 14.1, 14.2, 14.3, 14.4, 14.5, 14.6, 14.7**
pub struct ManagerDialog {
    /// The working copy of editable settings.
    state: ManagerState,
    /// Classifier statistics for display.
    stats: ManagerStats,
    /// Dialog handle (set when the dialog is created).
    #[allow(dead_code)]
    hwnd: HWND,
    /// Optional statistics manager for reset operations.
    ///
    /// **Validates: Requirement 3.5**
    statistics_manager: Option<StatisticsManager>,
}

impl ManagerDialog {
    /// Create a new Manager dialog instance from current config and stats.
    ///
    /// If a `StatisticsManager` is provided, the Reset Statistics button
    /// will be functional.
    ///
    /// **Validates: Requirements 14.1, 3.5**
    #[must_use]
    pub fn new(
        config: &AppConfig,
        stats: ManagerStats,
        statistics_manager: Option<StatisticsManager>,
    ) -> Self {
        Self {
            state: ManagerState::from_config(config),
            stats,
            hwnd: HWND::default(),
            statistics_manager,
        }
    }

    /// Launch the Manager dialog as a modal dialog.
    ///
    /// Creates the Win32 dialog and enters a message loop until the user
    /// closes the dialog. Returns `true` if the user closed with OK
    /// (settings should be saved) or `false` if cancelled.
    ///
    /// **Validates: Requirement 14.1**
    pub fn launch(&mut self, _hwnd_parent: HWND) -> bool {
        // In a full implementation, this would create the Win32 dialog:
        // unsafe {
        //     let hinst = GetModuleHandleW(None).unwrap_or_default();
        //     self.hwnd = CreateDialogParamW(
        //         hinst,
        //         PCWSTR::from_raw(IDD_MANAGER as *const u16),
        //         hwnd_parent,
        //         Some(Self::dialog_proc),
        //         LPARAM(self as *mut _ as isize),
        //     );
        // }
        //
        // The message loop would run here and return true/false based on
        // how the dialog was closed. This placeholder returns false until
        // the dialog resources are integrated.
        false
    }

    /// Apply changed settings to the config chain and save to disk.
    ///
    /// Called when the user closes the dialog with OK. If settings have
    /// been modified, applies them to the `ConfigChain`'s config and
    /// performs a sparse save to the profile-specific INI file.
    ///
    /// Returns `true` if settings were saved, `false` if nothing changed.
    ///
    /// **Validates: Requirements 2.1, 2.2, 14.7**
    pub fn apply_changes(
        &self,
        config_chain: &mut ConfigChain,
    ) -> bool {
        if !self.state.is_dirty() {
            return false;
        }

        self.state.apply_to_config(config_chain.config_mut());
        let _ = config_chain.save();
        true
    }

    /// Handle a "Train" button click.
    ///
    /// Signals that the caller should initiate a training operation with
    /// a progress dialog. The caller provides a progress callback.
    ///
    /// **Validates: Requirement 14.4**
    #[must_use]
    pub fn show_train_progress(&self) -> TrainRequest {
        TrainRequest {
            ham_folder_ids: self.state.ham_training_folder_ids.clone(),
            spam_folder_ids: self.state.spam_training_folder_ids.clone(),
        }
    }

    /// Handle a "Filter Now" button click.
    ///
    /// Signals that the caller should initiate a Filter Now operation
    /// with a progress dialog. The caller provides a progress callback.
    ///
    /// **Validates: Requirement 14.4**
    #[must_use]
    pub fn show_filter_now_progress(&self) -> FilterNowRequest {
        FilterNowRequest {
            folder_ids: self.state.watch_folder_ids.clone(),
        }
    }

    /// Handle a folder selection event from the MAPI folder picker.
    ///
    /// Updates the appropriate folder setting based on which browse button
    /// was clicked.
    ///
    /// **Validates: Requirement 14.6**
    pub fn on_folder_select(&mut self, target: FolderTarget, folder_ids: Vec<FolderId>) {
        match target {
            FolderTarget::Watch => {
                self.state.set_watch_folders(folder_ids);
            }
            FolderTarget::Spam => {
                self.state.set_spam_folder(folder_ids.into_iter().next());
            }
            FolderTarget::Unsure => {
                self.state.set_unsure_folder(folder_ids.into_iter().next());
            }
            FolderTarget::HamTraining => {
                self.state.set_ham_training_folders(folder_ids);
            }
            FolderTarget::SpamTraining => {
                self.state.set_spam_training_folders(folder_ids);
            }
        }
    }

    /// Returns a reference to the current dialog state.
    #[must_use]
    pub fn state(&self) -> &ManagerState {
        &self.state
    }

    /// Returns a mutable reference to the dialog state.
    pub fn state_mut(&mut self) -> &mut ManagerState {
        &mut self.state
    }

    /// Returns the current classifier statistics.
    #[must_use]
    pub fn stats(&self) -> &ManagerStats {
        &self.stats
    }

    /// Update the statistics display (e.g., after training completes).
    pub fn update_stats(&mut self, stats: ManagerStats) {
        self.stats = stats;
    }

    /// Reset lifetime statistics after user confirmation.
    ///
    /// Shows a confirmation `MessageBox`. If the user confirms, calls
    /// `reset_lifetime()` on the stored `StatisticsManager` and refreshes
    /// the in-memory stats snapshot. If no `StatisticsManager` is available
    /// or the user cancels, this is a no-op.
    ///
    /// Returns `true` if the reset was performed.
    ///
    /// **Validates: Requirement 3.5**
    pub fn reset_statistics(&mut self) -> bool {
        let stats_mgr = match &self.statistics_manager {
            Some(mgr) => mgr.clone(),
            None => return false,
        };

        // Show confirmation dialog.
        let confirmed = unsafe {
            let msg = "Reset all lifetime statistics?";
            let title = "SpamBayes Manager";
            let wide_msg: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
            let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

            let result = MessageBoxW(
                self.hwnd,
                PCWSTR::from_raw(wide_msg.as_ptr()),
                PCWSTR::from_raw(wide_title.as_ptr()),
                MB_YESNO | MB_ICONQUESTION,
            );
            result == IDYES
        };

        if confirmed {
            stats_mgr.reset_lifetime();
            self.stats = ManagerStats::from_statistics(&stats_mgr);

            // Refresh dialog labels if the dialog is currently visible.
            if self.hwnd != HWND::default() {
                unsafe {
                    Self::populate_stats_controls(self.hwnd, &self.stats);
                }
            }
            true
        } else {
            false
        }
    }

    /// Returns a reference to the stored `StatisticsManager`, if any.
    #[must_use]
    pub fn statistics_manager(&self) -> Option<&StatisticsManager> {
        self.statistics_manager.as_ref()
    }

    /// Win32 dialog procedure callback for the Manager dialog.
    ///
    /// Handles dialog messages: initialization, button clicks, threshold
    /// edits, checkbox toggles, and close actions.
    ///
    /// # Safety
    ///
    /// Called by the Windows message dispatcher. The `lparam` on
    /// `WM_INITDIALOG` carries a pointer to the `ManagerDialog` instance.
    #[allow(dead_code)]
    unsafe extern "system" fn dialog_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        match msg {
            WM_INITDIALOG => {
                // Store the dialog pointer in window user data for later retrieval.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
                // Populate controls from state and stats.
                let dialog = &*(lparam.0 as *const ManagerDialog);
                Self::populate_stats_controls(hwnd, &dialog.stats);
                // Start a periodic timer to refresh statistics while the dialog
                // is open (Req 3.4: real-time update).
                SetTimer(hwnd, IDC_STATS_TIMER, STATS_REFRESH_MS, None);
                1 // Return TRUE to accept default focus
            }
            WM_COMMAND => {
                let control_id = (wparam.0 & 0xFFFF) as u16;
                match control_id {
                    IDC_OK => {
                        let _ = EndDialog(hwnd, 1);
                        0
                    }
                    IDC_CANCEL => {
                        let _ = EndDialog(hwnd, 0);
                        0
                    }
                    IDC_TRAIN_NOW => {
                        // Signal training request to caller
                        0
                    }
                    IDC_FILTER_NOW => {
                        // Signal filter now request to caller
                        0
                    }
                    IDC_RESET_STATS => {
                        // Show confirmation dialog before resetting lifetime statistics.
                        // **Validates: Requirement 3.5**
                        let msg = "Reset all lifetime statistics?";
                        let title = "SpamBayes Manager";
                        let wide_msg: Vec<u16> =
                            msg.encode_utf16().chain(std::iter::once(0)).collect();
                        let wide_title: Vec<u16> =
                            title.encode_utf16().chain(std::iter::once(0)).collect();

                        let result = MessageBoxW(
                            hwnd,
                            PCWSTR::from_raw(wide_msg.as_ptr()),
                            PCWSTR::from_raw(wide_title.as_ptr()),
                            MB_YESNO | MB_ICONQUESTION,
                        );

                        if result == IDYES {
                            // Retrieve the dialog pointer from user data.
                            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                            if ptr != 0 {
                                let dialog = &mut *(ptr as *mut ManagerDialog);
                                if let Some(ref stats_mgr) = dialog.statistics_manager {
                                    stats_mgr.reset_lifetime();
                                    // Rebuild display stats from the reset manager.
                                    dialog.stats = ManagerStats::from_statistics(stats_mgr);
                                    Self::populate_stats_controls(hwnd, &dialog.stats);
                                }
                            }
                        }
                        0
                    }
                    IDC_BROWSE_WATCH
                    | IDC_BROWSE_SPAM_FOLDER
                    | IDC_BROWSE_UNSURE_FOLDER
                    | IDC_BROWSE_HAM_TRAIN
                    | IDC_BROWSE_SPAM_TRAIN => {
                        // Open MAPI folder picker for the target
                        0
                    }
                    IDC_ENABLE_FILTERING => {
                        // Toggle filtering enabled state
                        0
                    }
                    _ => 0,
                }
            }
            WM_TIMER => {
                // Periodic statistics refresh (Req 3.4).
                // Retrieve the dialog pointer stored during WM_INITDIALOG.
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    let dialog = &mut *(ptr as *mut ManagerDialog);
                    if let Some(ref stats_mgr) = dialog.statistics_manager {
                        dialog.stats = ManagerStats::from_statistics(stats_mgr);
                        Self::populate_stats_controls(hwnd, &dialog.stats);
                    }
                }
                0
            }
            WM_CLOSE => {
                // Kill the statistics refresh timer before closing (Req 3.4).
                let _ = KillTimer(hwnd, IDC_STATS_TIMER);
                let _ = EndDialog(hwnd, 0);
                0
            }
            _ => 0,
        }
    }

    /// Show an error message in the Manager dialog context.
    #[allow(dead_code)]
    fn show_error(hwnd: HWND, message: &str) {
        let wide_msg: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        let title = "SpamBayes Manager";
        let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            MessageBoxW(
                hwnd,
                PCWSTR::from_raw(wide_msg.as_ptr()),
                PCWSTR::from_raw(wide_title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    /// Set the text of a dialog control using `SetDlgItemTextW`.
    ///
    /// # Safety
    ///
    /// Caller must ensure `hwnd` is a valid dialog window handle and
    /// `control_id` refers to an existing control within the dialog.
    #[allow(dead_code)]
    unsafe fn set_control_text(hwnd: HWND, control_id: u16, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let _ = SetDlgItemTextW(hwnd, i32::from(control_id), PCWSTR::from_raw(wide.as_ptr()));
    }

    /// Populate all statistics label controls from a `ManagerStats` snapshot.
    ///
    /// Called during `WM_INITDIALOG` and when stats are refreshed.
    ///
    /// **Validates: Requirements 3.1, 3.2, 3.3**
    #[allow(dead_code)]
    unsafe fn populate_stats_controls(hwnd: HWND, stats: &ManagerStats) {
        // Training Statistics section (Req 3.1)
        let ham_trained_text = format_stat(stats.ham_trained, stats.session_ham_trained);
        let spam_trained_text = format_stat(stats.spam_trained, stats.session_spam_trained);
        Self::set_control_text(hwnd, IDC_STAT_HAM_TRAINED, &ham_trained_text);
        Self::set_control_text(hwnd, IDC_STAT_SPAM_TRAINED, &spam_trained_text);

        // Session Activity section (Req 3.2)
        let session_ham = format_with_thousands(u64::from(stats.session_ham_classified));
        let session_unsure = format_with_thousands(u64::from(stats.session_unsure_classified));
        let session_spam = format_with_thousands(u64::from(stats.session_spam_classified));
        Self::set_control_text(hwnd, IDC_STAT_SESSION_HAM, &session_ham);
        Self::set_control_text(hwnd, IDC_STAT_SESSION_UNSURE, &session_unsure);
        Self::set_control_text(hwnd, IDC_STAT_SESSION_SPAM, &session_spam);

        // Lifetime Classification section (Req 3.3)
        let lifetime_ham = format_stat(stats.total_ham_classified, stats.session_ham_classified);
        let lifetime_unsure =
            format_stat(stats.total_unsure_classified, stats.session_unsure_classified);
        let lifetime_spam =
            format_stat(stats.total_spam_classified, stats.session_spam_classified);
        Self::set_control_text(hwnd, IDC_STAT_LIFETIME_HAM_CLASSIFIED, &lifetime_ham);
        Self::set_control_text(hwnd, IDC_STAT_LIFETIME_UNSURE_CLASSIFIED, &lifetime_unsure);
        Self::set_control_text(hwnd, IDC_STAT_LIFETIME_SPAM_CLASSIFIED, &lifetime_spam);

        // Total session classified count
        let total_session = format_with_thousands(u64::from(stats.session_classified));
        Self::set_control_text(hwnd, IDC_STAT_SESSION_CLASSIFIED, &total_session);
    }
}

// ─── FolderTarget ────────────────────────────────────────────────────────────

/// Identifies which folder setting a folder picker selection applies to.
///
/// Used by `on_folder_select` to route the selected folder(s) to the
/// correct field in `ManagerState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderTarget {
    /// Watch folders (multiple selection allowed).
    Watch,
    /// Spam destination folder (single selection).
    Spam,
    /// Unsure destination folder (single selection).
    Unsure,
    /// Ham training folders (multiple selection allowed).
    HamTraining,
    /// Spam training folders (multiple selection allowed).
    SpamTraining,
}

// ─── TrainRequest ────────────────────────────────────────────────────────────

/// Request payload for initiating a training operation from the dialog.
///
/// The caller receives this from `show_train_progress()` and uses it
/// to invoke the `TrainingEngine` with the appropriate folder IDs.
///
/// **Validates: Requirement 14.4**
#[derive(Debug, Clone)]
pub struct TrainRequest {
    /// Ham training folder IDs.
    pub ham_folder_ids: Vec<FolderId>,
    /// Spam training folder IDs.
    pub spam_folder_ids: Vec<FolderId>,
}

// ─── FilterNowRequest ────────────────────────────────────────────────────────

/// Request payload for initiating a Filter Now operation from the dialog.
///
/// The caller receives this from `show_filter_now_progress()` and uses
/// it to invoke the `FilterEngine::filter_now()` method.
///
/// **Validates: Requirement 14.4**
#[derive(Debug, Clone)]
pub struct FilterNowRequest {
    /// Folder IDs to filter.
    pub folder_ids: Vec<FolderId>,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test assertions comparing exact threshold values
mod tests {
    use super::*;
    use spambayes_config::{ConfigChain, EntryId, StoreId};

    /// Helper: create a test `FolderId`.
    fn make_folder_id(store: &str, entry: &str) -> FolderId {
        FolderId::new(StoreId::new(store), EntryId::new(entry))
    }

    /// Helper: create a default `AppConfig` for testing.
    fn make_test_config() -> AppConfig {
        let mut config = AppConfig::default();
        config.filter.enabled = true;
        config.filter.spam_threshold = 90.0;
        config.filter.unsure_threshold = 15.0;
        config.filter.spam_action = FilterAction::Move;
        config.filter.unsure_action = FilterAction::Move;
        config.filter.ham_action = FilterAction::Untouched;
        config.filter.watch_folder_ids = vec![make_folder_id("STORE01", "INBOX01")];
        config.filter.spam_folder_id = Some(make_folder_id("STORE01", "SPAM01"));
        config.filter.unsure_folder_id = Some(make_folder_id("STORE01", "UNSURE01"));
        config.training.ham_folder_ids = vec![make_folder_id("STORE01", "HAM01")];
        config.training.spam_folder_ids = vec![make_folder_id("STORE01", "SPAMTRAIN01")];
        config
    }

    // ─── ManagerStats Tests ──────────────────────────────────────────────

    #[test]
    fn test_stats_new_is_zero() {
        let stats = ManagerStats::new();
        assert_eq!(stats.ham_trained, 0);
        assert_eq!(stats.spam_trained, 0);
        assert_eq!(stats.session_classified, 0);
        assert_eq!(stats.session_ham_trained, 0);
        assert_eq!(stats.session_spam_trained, 0);
        assert_eq!(stats.session_ham_classified, 0);
        assert_eq!(stats.session_unsure_classified, 0);
        assert_eq!(stats.session_spam_classified, 0);
        assert_eq!(stats.total_ham_classified, 0);
        assert_eq!(stats.total_unsure_classified, 0);
        assert_eq!(stats.total_spam_classified, 0);
    }

    #[test]
    fn test_stats_with_values() {
        let stats = ManagerStats::with_values(100, 200, 50);
        assert_eq!(stats.ham_trained, 100);
        assert_eq!(stats.spam_trained, 200);
        assert_eq!(stats.session_classified, 50);
        // Other fields should be zero via ..Self::new()
        assert_eq!(stats.session_ham_trained, 0);
        assert_eq!(stats.session_spam_trained, 0);
        assert_eq!(stats.session_ham_classified, 0);
        assert_eq!(stats.session_unsure_classified, 0);
        assert_eq!(stats.session_spam_classified, 0);
        assert_eq!(stats.total_ham_classified, 0);
        assert_eq!(stats.total_unsure_classified, 0);
        assert_eq!(stats.total_spam_classified, 0);
    }

    #[test]
    fn test_stats_default() {
        let stats = ManagerStats::default();
        assert_eq!(stats, ManagerStats::new());
    }

    #[test]
    fn test_stats_from_statistics() {
        use crate::statistics::StatisticsManager;
        use spambayes_core::Classification;

        let dir = std::env::temp_dir().join("spambayes_mgr_from_stats_test");
        let _ = std::fs::create_dir_all(&dir);
        let mgr = StatisticsManager::new(&dir, 100);

        // Simulate some activity.
        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Spam);
        mgr.on_classified(Classification::Unsure);
        mgr.on_trained(false); // ham
        mgr.on_trained(true); // spam
        mgr.on_trained(true); // spam

        let stats = ManagerStats::from_statistics(&mgr);

        // Lifetime training
        assert_eq!(stats.ham_trained, 1);
        assert_eq!(stats.spam_trained, 2);

        // Session training
        assert_eq!(stats.session_ham_trained, 1);
        assert_eq!(stats.session_spam_trained, 2);

        // Session classification
        assert_eq!(stats.session_ham_classified, 2);
        assert_eq!(stats.session_unsure_classified, 1);
        assert_eq!(stats.session_spam_classified, 1);
        assert_eq!(stats.session_classified, 4); // 2 + 1 + 1

        // Lifetime classification
        assert_eq!(stats.total_ham_classified, 2);
        assert_eq!(stats.total_unsure_classified, 1);
        assert_eq!(stats.total_spam_classified, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── Formatting Helper Tests ─────────────────────────────────────────

    #[test]
    fn test_format_with_thousands_zero() {
        assert_eq!(format_with_thousands(0), "0");
    }

    #[test]
    fn test_format_with_thousands_small() {
        assert_eq!(format_with_thousands(1), "1");
        assert_eq!(format_with_thousands(12), "12");
        assert_eq!(format_with_thousands(123), "123");
    }

    #[test]
    fn test_format_with_thousands_thousands() {
        assert_eq!(format_with_thousands(1_523), "1,523");
        assert_eq!(format_with_thousands(12_345), "12,345");
        assert_eq!(format_with_thousands(123_456), "123,456");
    }

    #[test]
    fn test_format_with_thousands_millions() {
        assert_eq!(format_with_thousands(1_000_000), "1,000,000");
        assert_eq!(format_with_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn test_format_with_thousands_exact_boundary() {
        assert_eq!(format_with_thousands(999), "999");
        assert_eq!(format_with_thousands(1_000), "1,000");
        assert_eq!(format_with_thousands(999_999), "999,999");
        assert_eq!(format_with_thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn test_format_stat_combined() {
        assert_eq!(format_stat(1_523, 12), "1,523 (session: 12)");
        assert_eq!(format_stat(0, 0), "0 (session: 0)");
        assert_eq!(format_stat(1_000_000, 1_000), "1,000,000 (session: 1,000)");
    }

    // ─── ManagerState Tests ──────────────────────────────────────────────

    #[test]
    fn test_state_from_config() {
        let config = make_test_config();
        let state = ManagerState::from_config(&config);

        assert!(state.filter_enabled);
        assert_eq!(state.spam_threshold, 90.0);
        assert_eq!(state.unsure_threshold, 15.0);
        assert_eq!(state.spam_action, FilterAction::Move);
        assert_eq!(state.unsure_action, FilterAction::Move);
        assert_eq!(state.ham_action, FilterAction::Untouched);
        assert_eq!(state.watch_folder_ids.len(), 1);
        assert!(state.spam_folder_id.is_some());
        assert!(state.unsure_folder_id.is_some());
        assert_eq!(state.ham_training_folder_ids.len(), 1);
        assert_eq!(state.spam_training_folder_ids.len(), 1);
        assert!(!state.is_dirty());
    }

    #[test]
    fn test_state_not_dirty_initially() {
        let config = make_test_config();
        let state = ManagerState::from_config(&config);
        assert!(!state.is_dirty());
    }

    #[test]
    fn test_set_spam_threshold_marks_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_threshold(85.0);
        assert!(state.is_dirty());
        assert_eq!(state.spam_threshold, 85.0);
    }

    #[test]
    fn test_set_spam_threshold_clamps_to_valid_range() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_threshold(150.0);
        assert_eq!(state.spam_threshold, 100.0);

        state.set_spam_threshold(-10.0);
        assert_eq!(state.spam_threshold, 0.0);
    }

    #[test]
    fn test_set_same_threshold_not_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        // Setting the same value should not mark dirty
        state.set_spam_threshold(90.0);
        assert!(!state.is_dirty());
    }

    #[test]
    fn test_set_unsure_threshold_marks_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_unsure_threshold(20.0);
        assert!(state.is_dirty());
        assert_eq!(state.unsure_threshold, 20.0);
    }

    #[test]
    fn test_set_unsure_threshold_clamps() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_unsure_threshold(200.0);
        assert_eq!(state.unsure_threshold, 100.0);

        state.set_unsure_threshold(-5.0);
        assert_eq!(state.unsure_threshold, 0.0);
    }

    #[test]
    fn test_set_spam_action_marks_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_action(FilterAction::Copy);
        assert!(state.is_dirty());
        assert_eq!(state.spam_action, FilterAction::Copy);
    }

    #[test]
    fn test_set_same_action_not_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        // Setting same action should not mark dirty
        state.set_spam_action(FilterAction::Move);
        assert!(!state.is_dirty());
    }

    #[test]
    fn test_set_unsure_action() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_unsure_action(FilterAction::Untouched);
        assert!(state.is_dirty());
        assert_eq!(state.unsure_action, FilterAction::Untouched);
    }

    #[test]
    fn test_set_ham_action() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_ham_action(FilterAction::Move);
        assert!(state.is_dirty());
        assert_eq!(state.ham_action, FilterAction::Move);
    }

    #[test]
    fn test_set_filter_enabled() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        // Disable filtering
        state.set_filter_enabled(false);
        assert!(state.is_dirty());
        assert!(!state.filter_enabled);
    }

    #[test]
    fn test_set_filter_enabled_same_not_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        // Setting same value should not mark dirty
        state.set_filter_enabled(true);
        assert!(!state.is_dirty());
    }

    // ─── Folder Selection Tests ──────────────────────────────────────────

    #[test]
    fn test_set_watch_folders() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        let new_folders = vec![
            make_folder_id("STORE01", "FOLDER_A"),
            make_folder_id("STORE01", "FOLDER_B"),
        ];
        state.set_watch_folders(new_folders.clone());
        assert!(state.is_dirty());
        assert_eq!(state.watch_folder_ids, new_folders);
    }

    #[test]
    fn test_set_spam_folder() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        let new_folder = make_folder_id("STORE02", "SPAM_NEW");
        state.set_spam_folder(Some(new_folder.clone()));
        assert!(state.is_dirty());
        assert_eq!(state.spam_folder_id, Some(new_folder));
    }

    #[test]
    fn test_set_unsure_folder() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        let new_folder = make_folder_id("STORE02", "UNSURE_NEW");
        state.set_unsure_folder(Some(new_folder.clone()));
        assert!(state.is_dirty());
        assert_eq!(state.unsure_folder_id, Some(new_folder));
    }

    #[test]
    fn test_set_ham_training_folders() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        let folders = vec![
            make_folder_id("STORE01", "HAM_A"),
            make_folder_id("STORE01", "HAM_B"),
        ];
        state.set_ham_training_folders(folders.clone());
        assert!(state.is_dirty());
        assert_eq!(state.ham_training_folder_ids, folders);
    }

    #[test]
    fn test_set_spam_training_folders() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        let folders = vec![make_folder_id("STORE01", "SPAM_T")];
        state.set_spam_training_folders(folders.clone());
        assert!(state.is_dirty());
        assert_eq!(state.spam_training_folder_ids, folders);
    }

    // ─── Apply to Config Tests ───────────────────────────────────────────

    #[test]
    fn test_apply_to_config() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        // Make changes
        state.set_spam_threshold(80.0);
        state.set_unsure_threshold(25.0);
        state.set_spam_action(FilterAction::Copy);
        state.set_filter_enabled(false);

        // Apply to a fresh config
        let mut target_config = AppConfig::default();
        state.apply_to_config(&mut target_config);

        assert!(!target_config.filter.enabled);
        assert_eq!(target_config.filter.spam_threshold, 80.0);
        assert_eq!(target_config.filter.unsure_threshold, 25.0);
        assert_eq!(target_config.filter.spam_action, FilterAction::Copy);
        assert_eq!(target_config.filter.unsure_action, FilterAction::Move);
        assert_eq!(target_config.filter.ham_action, FilterAction::Untouched);
        assert_eq!(target_config.filter.watch_folder_ids.len(), 1);
        assert!(target_config.filter.spam_folder_id.is_some());
        assert!(target_config.filter.unsure_folder_id.is_some());
        assert_eq!(target_config.training.ham_folder_ids.len(), 1);
        assert_eq!(target_config.training.spam_folder_ids.len(), 1);
    }

    #[test]
    fn test_apply_preserves_folder_selections() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        let new_watch = vec![
            make_folder_id("S1", "W1"),
            make_folder_id("S1", "W2"),
        ];
        state.set_watch_folders(new_watch.clone());

        let mut target_config = AppConfig::default();
        state.apply_to_config(&mut target_config);

        assert_eq!(target_config.filter.watch_folder_ids, new_watch);
    }

    // ─── ManagerDialog Tests ─────────────────────────────────────────────

    #[test]
    fn test_dialog_new() {
        let config = make_test_config();
        let stats = ManagerStats::with_values(500, 300, 42);
        let dialog = ManagerDialog::new(&config, stats.clone(), None);

        assert_eq!(dialog.stats().ham_trained, 500);
        assert_eq!(dialog.stats().spam_trained, 300);
        assert_eq!(dialog.stats().session_classified, 42);
        assert!(dialog.state().filter_enabled);
    }

    #[test]
    fn test_dialog_apply_changes_when_dirty() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        // Modify state
        dialog.state_mut().set_spam_threshold(75.0);

        // Apply via ConfigChain
        let temp = std::env::temp_dir().join("spambayes_mgr_test_apply");
        let _ = std::fs::create_dir_all(&temp);
        let mut chain = ConfigChain::from_parts(make_test_config(), temp.clone(), "test_profile");
        let saved = dialog.apply_changes(&mut chain);

        assert!(saved);
        assert_eq!(chain.config().filter.spam_threshold, 75.0);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_dialog_apply_changes_not_dirty() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let dialog = ManagerDialog::new(&config, stats, None);

        // No changes made — apply should return false
        let temp = std::env::temp_dir().join("spambayes_mgr_test_no_apply");
        let _ = std::fs::create_dir_all(&temp);
        let mut chain = ConfigChain::from_parts(make_test_config(), temp.clone(), "test_profile");
        let saved = dialog.apply_changes(&mut chain);

        assert!(!saved);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_dialog_show_train_progress() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let dialog = ManagerDialog::new(&config, stats, None);

        let req = dialog.show_train_progress();
        assert_eq!(req.ham_folder_ids.len(), 1);
        assert_eq!(req.spam_folder_ids.len(), 1);
        assert_eq!(req.ham_folder_ids[0], make_folder_id("STORE01", "HAM01"));
        assert_eq!(req.spam_folder_ids[0], make_folder_id("STORE01", "SPAMTRAIN01"));
    }

    #[test]
    fn test_dialog_show_filter_now_progress() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let dialog = ManagerDialog::new(&config, stats, None);

        let req = dialog.show_filter_now_progress();
        assert_eq!(req.folder_ids.len(), 1);
        assert_eq!(req.folder_ids[0], make_folder_id("STORE01", "INBOX01"));
    }

    #[test]
    fn test_dialog_on_folder_select_watch() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        let new_folders = vec![
            make_folder_id("S1", "F1"),
            make_folder_id("S1", "F2"),
        ];
        dialog.on_folder_select(FolderTarget::Watch, new_folders.clone());

        assert_eq!(dialog.state().watch_folder_ids, new_folders);
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_on_folder_select_spam() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        let folder = make_folder_id("S2", "SPAM_NEW");
        dialog.on_folder_select(FolderTarget::Spam, vec![folder.clone()]);

        assert_eq!(dialog.state().spam_folder_id, Some(folder));
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_on_folder_select_unsure() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        let folder = make_folder_id("S2", "UNSURE_NEW");
        dialog.on_folder_select(FolderTarget::Unsure, vec![folder.clone()]);

        assert_eq!(dialog.state().unsure_folder_id, Some(folder));
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_on_folder_select_ham_training() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        let folders = vec![
            make_folder_id("S1", "H1"),
            make_folder_id("S1", "H2"),
        ];
        dialog.on_folder_select(FolderTarget::HamTraining, folders.clone());

        assert_eq!(dialog.state().ham_training_folder_ids, folders);
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_on_folder_select_spam_training() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        let folders = vec![make_folder_id("S1", "ST1")];
        dialog.on_folder_select(FolderTarget::SpamTraining, folders.clone());

        assert_eq!(dialog.state().spam_training_folder_ids, folders);
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_update_stats() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats, None);

        let new_stats = ManagerStats {
            ham_trained: 1000,
            spam_trained: 500,
            session_ham_trained: 10,
            session_spam_trained: 5,
            session_classified: 100,
            session_ham_classified: 60,
            session_unsure_classified: 15,
            session_spam_classified: 25,
            total_ham_classified: 5000,
            total_unsure_classified: 300,
            total_spam_classified: 2000,
            correctly_classified: 0,
            false_positives: 0,
            false_negatives: 0,
            manually_classified_good: 0,
            manually_classified_spam: 0,
            last_reset_date: None,
        };
        dialog.update_stats(new_stats.clone());

        assert_eq!(dialog.stats(), &new_stats);
    }

    // ─── Timer Constant Tests (Req 3.4) ─────────────────────────────────

    #[test]
    fn test_stats_timer_id_is_distinct() {
        // The timer ID must not conflict with any dialog control IDs.
        assert_eq!(IDC_STATS_TIMER, 3100);
        // Ensure it doesn't overlap with the highest control ID (IDC_RESET_STATS = 3044).
        assert!(IDC_STATS_TIMER as u16 > IDC_RESET_STATS);
    }

    #[test]
    fn test_stats_refresh_interval() {
        // The refresh interval should be 2 seconds (2000ms) per the design.
        assert_eq!(STATS_REFRESH_MS, 2000);
    }
}
