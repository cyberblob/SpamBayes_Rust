//! `SpamBayes` Manager — shared state types.
//!
//! Provides the data model used by the GTK4 Manager window: classifier
//! statistics, editable filter/folder state, and formatting helpers.
//!
//! # Requirements
//!
//! - Req 14.2: Display classifier statistics
//! - Req 14.3: Allow changing filter settings (thresholds, actions)
//! - Req 14.5: Enable/disable filtering via checkbox
//! - Req 14.6: Select folders via MAPI folder picker
//! - Req 14.7: Save changed settings on dialog close

use spambayes_config::{AppConfig, FilterAction, FolderId};

use crate::statistics::StatisticsManager;

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
            correctly_classified: lifetime.correctly_classified,
            false_positives: lifetime.false_positives,
            false_negatives: lifetime.false_negatives,
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

    /// Ham (good) destination folder.
    pub ham_folder_id: Option<FolderId>,

    /// Ham training folder IDs.
    pub ham_training_folder_ids: Vec<FolderId>,

    /// Spam training folder IDs.
    pub spam_training_folder_ids: Vec<FolderId>,

    /// Whether spam auto-cleanup is enabled.
    ///
    /// **Validates: Requirement 18.1**
    pub spam_auto_cleanup_enabled: bool,

    /// Number of days to keep spam before automatic deletion.
    ///
    /// **Validates: Requirement 18.2**
    pub spam_auto_cleanup_days: u32,

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
            ham_folder_id: config.filter.ham_folder_id.clone(),
            ham_training_folder_ids: config.training.ham_folder_ids.clone(),
            spam_training_folder_ids: config.training.spam_folder_ids.clone(),
            spam_auto_cleanup_enabled: config.filter.spam_auto_cleanup_enabled,
            spam_auto_cleanup_days: config.filter.spam_auto_cleanup_days,
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
        config.filter.ham_folder_id = self.ham_folder_id.clone();
        config.training.ham_folder_ids = self.ham_training_folder_ids.clone();
        config.training.spam_folder_ids = self.spam_training_folder_ids.clone();
        config.filter.spam_auto_cleanup_enabled = self.spam_auto_cleanup_enabled;
        config.filter.spam_auto_cleanup_days = self.spam_auto_cleanup_days;
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

    /// Set spam auto-cleanup enabled and mark dirty.
    ///
    /// **Validates: Requirement 18.1**
    pub fn set_spam_auto_cleanup_enabled(&mut self, enabled: bool) {
        if self.spam_auto_cleanup_enabled != enabled {
            self.spam_auto_cleanup_enabled = enabled;
            self.dirty = true;
        }
    }

    /// Set spam auto-cleanup retention days and mark dirty.
    ///
    /// **Validates: Requirement 18.2**
    pub fn set_spam_auto_cleanup_days(&mut self, days: u32) {
        let clamped = days.clamp(1, 365);
        if self.spam_auto_cleanup_days != clamped {
            self.spam_auto_cleanup_days = clamped;
            self.dirty = true;
        }
    }

    // ─── Validation Helpers ──────────────────────────────────────────────

    /// Check whether the threshold values are valid.
    ///
    /// Returns `true` if both thresholds are in [0, 100] and the unsure
    /// threshold does not exceed the spam threshold.
    ///
    /// **Validates: Requirements 5.1, 5.2, 5.3**
    #[must_use]
    pub fn is_threshold_valid(&self) -> bool {
        self.spam_threshold >= 0.0
            && self.spam_threshold <= 100.0
            && self.unsure_threshold >= 0.0
            && self.unsure_threshold <= 100.0
            && self.unsure_threshold < self.spam_threshold
    }
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
        assert_eq!(stats.session_ham_trained, 0);
        assert_eq!(stats.session_spam_trained, 0);
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

        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Spam);
        mgr.on_classified(Classification::Unsure);
        mgr.on_trained(false); // ham
        mgr.on_trained(true); // spam
        mgr.on_trained(true); // spam

        let stats = ManagerStats::from_statistics(&mgr);

        assert_eq!(stats.ham_trained, 1);
        assert_eq!(stats.spam_trained, 2);
        assert_eq!(stats.session_ham_trained, 1);
        assert_eq!(stats.session_spam_trained, 2);
        assert_eq!(stats.session_ham_classified, 2);
        assert_eq!(stats.session_unsure_classified, 1);
        assert_eq!(stats.session_spam_classified, 1);
        assert_eq!(stats.session_classified, 4);
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

        state.set_filter_enabled(false);
        assert!(state.is_dirty());
        assert!(!state.filter_enabled);
    }

    #[test]
    fn test_set_filter_enabled_same_not_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

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

        state.set_spam_threshold(80.0);
        state.set_unsure_threshold(25.0);
        state.set_spam_action(FilterAction::Copy);
        state.set_filter_enabled(false);

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

    // ─── Threshold Validation Tests ──────────────────────────────────────

    #[test]
    fn test_is_threshold_valid_invalid_unsure_exceeds_spam() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);
        state.spam_threshold = 50.0;
        state.unsure_threshold = 60.0;
        assert!(!state.is_threshold_valid());
    }

    #[test]
    fn test_is_threshold_valid_out_of_range() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);
        state.spam_threshold = 101.0;
        state.unsure_threshold = 15.0;
        assert!(!state.is_threshold_valid());
    }

    #[test]
    fn test_is_threshold_valid_valid_state() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);
        state.spam_threshold = 90.0;
        state.unsure_threshold = 15.0;
        assert!(state.is_threshold_valid());
    }

    // ─── Spam Auto-Cleanup Tests (Req 18) ────────────────────────────────

    #[test]
    fn test_state_from_config_loads_cleanup_defaults() {
        let config = AppConfig::default();
        let state = ManagerState::from_config(&config);

        assert!(!state.spam_auto_cleanup_enabled);
        assert_eq!(state.spam_auto_cleanup_days, 30);
    }

    #[test]
    fn test_state_from_config_loads_cleanup_enabled() {
        let mut config = make_test_config();
        config.filter.spam_auto_cleanup_enabled = true;
        config.filter.spam_auto_cleanup_days = 14;

        let state = ManagerState::from_config(&config);

        assert!(state.spam_auto_cleanup_enabled);
        assert_eq!(state.spam_auto_cleanup_days, 14);
    }

    #[test]
    fn test_apply_to_config_writes_cleanup_fields() {
        let mut config = make_test_config();
        config.filter.spam_auto_cleanup_enabled = true;
        config.filter.spam_auto_cleanup_days = 7;

        let state = ManagerState::from_config(&config);
        let mut target = AppConfig::default();
        state.apply_to_config(&mut target);

        assert!(target.filter.spam_auto_cleanup_enabled);
        assert_eq!(target.filter.spam_auto_cleanup_days, 7);
    }

    #[test]
    fn test_apply_to_config_writes_cleanup_disabled() {
        let config = make_test_config();
        let state = ManagerState::from_config(&config);

        let mut target = AppConfig::default();
        target.filter.spam_auto_cleanup_enabled = true;
        target.filter.spam_auto_cleanup_days = 99;

        state.apply_to_config(&mut target);

        assert!(!target.filter.spam_auto_cleanup_enabled);
        assert_eq!(target.filter.spam_auto_cleanup_days, 30);
    }

    #[test]
    fn test_set_cleanup_enabled_marks_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_enabled(true);
        assert!(state.is_dirty());
        assert!(state.spam_auto_cleanup_enabled);
    }

    #[test]
    fn test_set_cleanup_enabled_same_not_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_enabled(false);
        assert!(!state.is_dirty());
    }

    #[test]
    fn test_set_cleanup_days_marks_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_days(14);
        assert!(state.is_dirty());
        assert_eq!(state.spam_auto_cleanup_days, 14);
    }

    #[test]
    fn test_set_cleanup_days_same_not_dirty() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_days(30);
        assert!(!state.is_dirty());
    }

    #[test]
    fn test_set_cleanup_days_clamps_to_minimum() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_days(0);
        assert_eq!(state.spam_auto_cleanup_days, 1);
    }

    #[test]
    fn test_set_cleanup_days_clamps_to_maximum() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_days(500);
        assert_eq!(state.spam_auto_cleanup_days, 365);
    }

    #[test]
    fn test_set_cleanup_days_boundary_values() {
        let config = make_test_config();
        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_days(1);
        assert_eq!(state.spam_auto_cleanup_days, 1);

        state.set_spam_auto_cleanup_days(365);
        assert_eq!(state.spam_auto_cleanup_days, 365);
    }

    #[test]
    fn test_cleanup_roundtrip_through_config() {
        let mut config = make_test_config();
        config.filter.spam_auto_cleanup_enabled = false;
        config.filter.spam_auto_cleanup_days = 30;

        let mut state = ManagerState::from_config(&config);

        state.set_spam_auto_cleanup_enabled(true);
        state.set_spam_auto_cleanup_days(7);

        let mut saved_config = AppConfig::default();
        state.apply_to_config(&mut saved_config);

        let reloaded_state = ManagerState::from_config(&saved_config);
        assert!(reloaded_state.spam_auto_cleanup_enabled);
        assert_eq!(reloaded_state.spam_auto_cleanup_days, 7);
    }
}
