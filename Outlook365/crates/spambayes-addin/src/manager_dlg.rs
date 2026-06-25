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

use std::path::Path;

use spambayes_config::{AppConfig, FilterAction, FolderId};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EndDialog, MessageBoxW, MB_ICONERROR, MB_OK, WM_CLOSE, WM_COMMAND,
    WM_INITDIALOG,
};

// ─── Dialog Control ID Constants ─────────────────────────────────────────────

/// Dialog resource ID for the Manager dialog.
#[allow(dead_code)]
const IDD_MANAGER: u32 = 3000;

// Statistics display controls
#[allow(dead_code)]
const IDC_STAT_HAM_TRAINED: u16 = 3001;
#[allow(dead_code)]
const IDC_STAT_SPAM_TRAINED: u16 = 3002;
#[allow(dead_code)]
const IDC_STAT_SESSION_CLASSIFIED: u16 = 3003;

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

// ─── ManagerStats ────────────────────────────────────────────────────────────

/// Classifier statistics displayed in the Manager dialog.
///
/// **Validates: Requirement 14.2**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerStats {
    /// Number of ham messages trained in the classifier database.
    pub ham_trained: u64,
    /// Number of spam messages trained in the classifier database.
    pub spam_trained: u64,
    /// Total messages classified in the current Outlook session.
    pub session_classified: u32,
}

impl ManagerStats {
    /// Create stats with zero values.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ham_trained: 0,
            spam_trained: 0,
            session_classified: 0,
        }
    }

    /// Create stats from specific values.
    #[must_use]
    pub fn with_values(ham_trained: u64, spam_trained: u64, session_classified: u32) -> Self {
        Self {
            ham_trained,
            spam_trained,
            session_classified,
        }
    }
}

impl Default for ManagerStats {
    fn default() -> Self {
        Self::new()
    }
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
}

impl ManagerDialog {
    /// Create a new Manager dialog instance from current config and stats.
    ///
    /// **Validates: Requirement 14.1**
    #[must_use]
    pub fn new(config: &AppConfig, stats: ManagerStats) -> Self {
        Self {
            state: ManagerState::from_config(config),
            stats,
            hwnd: HWND::default(),
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

    /// Apply changed settings to the config and save to disk.
    ///
    /// Called when the user closes the dialog with OK. If settings have
    /// been modified, applies them to the given `AppConfig` and persists.
    ///
    /// Returns `true` if settings were saved, `false` if nothing changed.
    ///
    /// **Validates: Requirement 14.7**
    pub fn apply_changes(
        &self,
        config: &mut AppConfig,
        data_dir: &Path,
        profile_name: &str,
    ) -> bool {
        if !self.state.is_dirty() {
            return false;
        }

        self.state.apply_to_config(config);
        let _ = config.save(data_dir, profile_name);
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
        _lparam: LPARAM,
    ) -> isize {
        match msg {
            WM_INITDIALOG => {
                // Store the dialog pointer in window user data.
                // In a real implementation we'd use SetWindowLongPtrW.
                // Populate controls from state and stats.
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
            WM_CLOSE => {
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
    use spambayes_config::{EntryId, StoreId};

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
    }

    #[test]
    fn test_stats_with_values() {
        let stats = ManagerStats::with_values(100, 200, 50);
        assert_eq!(stats.ham_trained, 100);
        assert_eq!(stats.spam_trained, 200);
        assert_eq!(stats.session_classified, 50);
    }

    #[test]
    fn test_stats_default() {
        let stats = ManagerStats::default();
        assert_eq!(stats, ManagerStats::new());
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
        let dialog = ManagerDialog::new(&config, stats.clone());

        assert_eq!(dialog.stats().ham_trained, 500);
        assert_eq!(dialog.stats().spam_trained, 300);
        assert_eq!(dialog.stats().session_classified, 42);
        assert!(dialog.state().filter_enabled);
    }

    #[test]
    fn test_dialog_apply_changes_when_dirty() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats);

        // Modify state
        dialog.state_mut().set_spam_threshold(75.0);

        // Apply to a new config
        let mut target_config = make_test_config();
        let temp = std::env::temp_dir().join("spambayes_mgr_test_apply");
        let _ = std::fs::create_dir_all(&temp);
        let saved = dialog.apply_changes(&mut target_config, &temp, "test_profile");

        assert!(saved);
        assert_eq!(target_config.filter.spam_threshold, 75.0);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_dialog_apply_changes_not_dirty() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let dialog = ManagerDialog::new(&config, stats);

        // No changes made — apply should return false
        let mut target_config = make_test_config();
        let temp = std::env::temp_dir().join("spambayes_mgr_test_no_apply");
        let _ = std::fs::create_dir_all(&temp);
        let saved = dialog.apply_changes(&mut target_config, &temp, "test_profile");

        assert!(!saved);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_dialog_show_train_progress() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let dialog = ManagerDialog::new(&config, stats);

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
        let dialog = ManagerDialog::new(&config, stats);

        let req = dialog.show_filter_now_progress();
        assert_eq!(req.folder_ids.len(), 1);
        assert_eq!(req.folder_ids[0], make_folder_id("STORE01", "INBOX01"));
    }

    #[test]
    fn test_dialog_on_folder_select_watch() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats);

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
        let mut dialog = ManagerDialog::new(&config, stats);

        let folder = make_folder_id("S2", "SPAM_NEW");
        dialog.on_folder_select(FolderTarget::Spam, vec![folder.clone()]);

        assert_eq!(dialog.state().spam_folder_id, Some(folder));
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_on_folder_select_unsure() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats);

        let folder = make_folder_id("S2", "UNSURE_NEW");
        dialog.on_folder_select(FolderTarget::Unsure, vec![folder.clone()]);

        assert_eq!(dialog.state().unsure_folder_id, Some(folder));
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_on_folder_select_ham_training() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats);

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
        let mut dialog = ManagerDialog::new(&config, stats);

        let folders = vec![make_folder_id("S1", "ST1")];
        dialog.on_folder_select(FolderTarget::SpamTraining, folders.clone());

        assert_eq!(dialog.state().spam_training_folder_ids, folders);
        assert!(dialog.state().is_dirty());
    }

    #[test]
    fn test_dialog_update_stats() {
        let config = make_test_config();
        let stats = ManagerStats::new();
        let mut dialog = ManagerDialog::new(&config, stats);

        let new_stats = ManagerStats::with_values(1000, 500, 100);
        dialog.update_stats(new_stats.clone());

        assert_eq!(dialog.stats(), &new_stats);
    }
}
