//! Filter engine - core scoring and classification logic.
//!
//! This module implements the `FilterEngine` which ties together the tokenizer,
//! classifier, and configuration to produce message classifications (ham, spam,
//! or unsure) based on configurable probability thresholds.
//!
//! # Classification Thresholds
//!
//! - **Spam**: score >= `spam_threshold` (default 90%)
//! - **Unsure**: score >= `unsure_threshold` (default 15%) and < `spam_threshold`
//! - **Ham**: score < `unsure_threshold`
//!
//! The classifier produces a raw probability in `0.0..=1.0`; the filter engine
//! converts this to a percentage (0–100%) before comparing against thresholds.
//!
//! # Filter Actions
//!
//! After classification, the engine performs configured actions on messages:
//! - **Move**: Move the message to the configured destination folder
//! - **Copy**: Copy the message to the configured destination folder
//! - **Untouched**: Leave the message in its current folder
//!
//! The engine also handles saving spam score information, setting cleanup
//! timestamps on spam messages, and marking messages as read per configuration.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use spambayes_config::{FilterAction, FilterConfig, FilterNowConfig, FolderId, GeneralConfig};
use spambayes_core::classifier::Classifier;
use spambayes_core::tokenizer::Tokenizer;
use spambayes_core::Classification;
use spambayes_mapi::MsgStoreError;
use spambayes_storage::{MessageDatabase, StorageBackend};

use crate::statistics::StatisticsManager;
use crate::logger::Logger;

// ─── FilterError ─────────────────────────────────────────────────────────────

/// Errors that can occur during filter operations.
#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    /// The classifier lock was poisoned (another thread panicked while holding it).
    #[error("classifier lock poisoned")]
    ClassifierLockPoisoned,

    /// The storage lock was poisoned.
    #[error("storage lock poisoned")]
    StorageLockPoisoned,

    /// The message database lock was poisoned.
    #[error("message database lock poisoned")]
    MessageDbLockPoisoned,

    /// Failed to extract message content for scoring.
    #[error("failed to extract message content: {0}")]
    ContentExtraction(String),

    /// The filter is disabled in configuration.
    #[error("filter is disabled")]
    Disabled,

    /// The destination folder is unavailable or not configured.
    #[error("destination folder unavailable: {0}")]
    DestinationFolderUnavailable(String),

    /// A MAPI operation failed during a filter action.
    #[error("MAPI error during filter action: {0}")]
    MapiError(#[from] MsgStoreError),

    /// Failed to save spam score after retries.
    #[error("failed to save spam score after {0} retries")]
    ScoreSaveFailed(u32),

    /// No folders are configured for the Filter Now operation.
    #[error("no folders configured for Filter Now — at least one folder must be specified")]
    NoFoldersConfigured,
}

// ─── FilterableMessage Trait ─────────────────────────────────────────────────

/// Field value type for message properties.
///
/// Abstracts over the field types that can be stored on a message,
/// allowing the filter to work with both real MAPI messages and test mocks.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// A string field value.
    String(String),
    /// A floating-point field value.
    Float(f64),
    /// An integer field value.
    Integer(i64),
}

/// Trait abstracting message operations needed by the filter engine.
///
/// This trait enables the filter engine to be tested without actual MAPI
/// access. The real MAPI `Message` type can implement this trait on Windows,
/// while tests use mock implementations.
pub trait FilterableMessage {
    /// Get a named field value from the message.
    fn get_field(&self, name: &str) -> Option<FieldValue>;

    /// Set a named field value on the message.
    fn set_field(&mut self, name: &str, value: FieldValue) -> Result<(), MsgStoreError>;

    /// Save the message (persist property changes).
    fn save(&mut self) -> Result<(), MsgStoreError>;

    /// Move the message to a destination folder identified by ID.
    fn move_to(&mut self, folder_id: &str) -> Result<(), MsgStoreError>;

    /// Copy the message to a destination folder identified by ID.
    fn copy_to(&self, folder_id: &str) -> Result<(), MsgStoreError>;

    /// Set the read/unread state of the message.
    fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError>;
}

// ─── Progress Trait ──────────────────────────────────────────────────────────

/// Trait for reporting progress during batch operations (Filter Now).
///
/// Implementers display progress to the user and check for cancellation.
pub trait Progress {
    /// Report progress: the current folder name, the count processed so far,
    /// and the total messages in this folder.
    fn report(&mut self, folder_name: &str, current: u32, total: u32);

    /// Check whether the user has requested cancellation.
    fn is_cancelled(&self) -> bool;
}

// ─── FilterNowResult ─────────────────────────────────────────────────────────

/// Result of a Filter Now batch operation, tracking counts per classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterNowResult {
    /// Number of messages classified as spam.
    pub spam_count: u32,
    /// Number of messages classified as unsure.
    pub unsure_count: u32,
    /// Number of messages classified as ham.
    pub ham_count: u32,
    /// Number of messages that failed to process.
    pub error_count: u32,
    /// Whether the operation was cancelled by the user.
    pub cancelled: bool,
}

impl FilterNowResult {
    /// Create an empty result with all counts at zero.
    fn new() -> Self {
        Self {
            spam_count: 0,
            unsure_count: 0,
            ham_count: 0,
            error_count: 0,
            cancelled: false,
        }
    }
}

// ─── FilterNowMessage Trait ──────────────────────────────────────────────────

/// Extended message trait for Filter Now operations.
///
/// Adds read-state checking and raw content access needed for batch filtering.
pub trait FilterNowMessage: FilterableMessage {
    /// Check whether the message is marked as read.
    fn get_read_state(&self) -> bool;

    /// Get the raw RFC 2822 message content for scoring.
    fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError>;
}

// ─── FolderProvider Trait ────────────────────────────────────────────────────

/// Abstracts folder/message access for the Filter Now operation.
///
/// This trait allows the `FilterEngine` to be tested without actual MAPI access.
pub trait FolderProvider {
    /// Get the display name of a folder.
    fn get_folder_name(&self, folder_id: &FolderId) -> Result<String, MsgStoreError>;

    /// Get the sub-folders of a folder.
    fn get_sub_folders(&self, folder_id: &FolderId) -> Result<Vec<FolderId>, MsgStoreError>;

    /// Get all messages in a folder.
    fn get_messages(
        &self,
        folder_id: &FolderId,
    ) -> Result<Vec<Box<dyn FilterNowMessage>>, MsgStoreError>;

    /// Get the total number of messages in a folder.
    fn message_count(&self, folder_id: &FolderId) -> Result<u32, MsgStoreError>;
}

// ─── Spam Auto-Cleanup ───────────────────────────────────────────────────────

/// The field name used to store the cleanup timestamp on spam messages.
const CLEANUP_FIELD: &str = "SpamBayesCleanupTimestamp";

/// Trait for messages involved in the spam auto-cleanup operation.
///
/// Provides the minimal interface needed: reading a field and deleting.
pub trait CleanupMessage {
    /// Get a named field value from the message.
    fn get_field(&self, name: &str) -> Option<FieldValue>;

    /// Delete this message from its folder.
    fn delete(&mut self) -> Result<(), MsgStoreError>;
}

/// Abstracts folder access for the spam auto-cleanup operation.
///
/// This trait allows the `FilterEngine`'s cleanup to be tested without
/// actual MAPI access.
pub trait CleanupProvider {
    /// Get all messages in the spam folder for cleanup scanning.
    fn get_spam_folder_messages(
        &self,
        folder_id: &FolderId,
    ) -> Result<Vec<Box<dyn CleanupMessage>>, MsgStoreError>;
}

/// Result of a spam auto-cleanup operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupResult {
    /// Number of messages deleted.
    pub deleted_count: u32,
    /// Number of messages skipped (no timestamp or within retention).
    pub skipped_count: u32,
    /// Number of messages that failed to delete.
    pub error_count: u32,
}

// ─── FilterResult ────────────────────────────────────────────────────────────

/// Result of filtering a message, including the classification and score.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterResult {
    /// The classification decision (Ham, Spam, or Unsure).
    pub classification: Classification,
    /// The spam probability as a percentage (0.0–100.0).
    pub score_pct: f64,
}

// ─── FilterEngine ────────────────────────────────────────────────────────────

/// Core filter engine that scores and classifies email messages.
///
/// The `FilterEngine` orchestrates the tokenizer, classifier, and threshold
/// configuration to produce spam/ham/unsure classifications for incoming
/// messages.
///
/// # Thread Safety
///
/// The classifier, storage, and message database are wrapped in `Arc<Mutex<_>>`
/// to allow shared access from multiple threads (e.g., the filter timer and
/// the UI thread).
#[allow(dead_code)]
pub struct FilterEngine {
    /// Filter configuration (thresholds, actions, timer settings).
    config: FilterConfig,
    /// The Bayesian classifier used for scoring.
    classifier: Arc<Mutex<Classifier>>,
    /// Persistence backend for classifier token data.
    storage: Arc<Mutex<Box<dyn StorageBackend>>>,
    /// Per-message metadata database.
    message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
    /// Email tokenizer for extracting scoring features.
    tokenizer: Tokenizer,
    /// Optional statistics observer for classification tracking.
    statistics: Option<StatisticsManager>,
    /// Optional logger for verbose diagnostics.
    logger: Option<Arc<Logger>>,
}

impl FilterEngine {
    /// Create a new `FilterEngine` with the given dependencies.
    pub fn new(
        config: FilterConfig,
        classifier: Arc<Mutex<Classifier>>,
        storage: Arc<Mutex<Box<dyn StorageBackend>>>,
        message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
        statistics: Option<StatisticsManager>,
    ) -> Self {
        Self {
            config,
            classifier,
            storage,
            message_db,
            tokenizer: Tokenizer::with_defaults(),
            statistics,
            logger: None,
        }
    }

    /// Create a `FilterEngine` with a custom tokenizer.
    pub fn with_tokenizer(
        config: FilterConfig,
        classifier: Arc<Mutex<Classifier>>,
        storage: Arc<Mutex<Box<dyn StorageBackend>>>,
        message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
        tokenizer: Tokenizer,
        statistics: Option<StatisticsManager>,
    ) -> Self {
        Self {
            config,
            classifier,
            storage,
            message_db,
            tokenizer,
            statistics,
            logger: None,
        }
    }

    /// Set the logger for verbose diagnostics.
    pub fn set_logger(&mut self, logger: Arc<Logger>) {
        self.logger = Some(logger);
    }

    /// Classify raw message bytes: tokenize → score → classify.
    ///
    /// This method accepts raw RFC 2822 message content and produces a
    /// classification based on the configured thresholds. It does not
    /// require MAPI access, making it suitable for testing and batch
    /// operations.
    ///
    /// # Arguments
    ///
    /// * `message_bytes` - Raw RFC 2822 message content.
    ///
    /// # Returns
    ///
    /// A [`FilterResult`] containing the classification and percentage score,
    /// or a [`FilterError`] if scoring fails.
    ///
    /// # Validates: Requirements 6.1, 6.2, 6.3, 6.4
    pub fn classify_raw(&self, message_bytes: &[u8]) -> Result<FilterResult, FilterError> {
        // Step 1: Tokenize the message content.
        let tokens = self.tokenizer.tokenize(message_bytes);

        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "classify_raw: tokenized {} bytes into {} tokens",
                message_bytes.len(), tokens.len()
            ));
        }

        // Step 2: Score using the classifier.
        let classifier = self
            .classifier
            .lock()
            .map_err(|_| FilterError::ClassifierLockPoisoned)?;

        let probability = classifier.spam_prob(tokens.into_iter());

        // Step 3: Convert probability (0.0–1.0) to percentage (0–100%).
        let score_pct = probability * 100.0;

        // Step 4: Classify based on thresholds.
        let classification = self.classify_score(score_pct);

        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "classify_raw: score={:.2}%, classification={:?} (thresholds: spam>={:.1}, unsure>={:.1})",
                score_pct, classification, self.config.spam_threshold, self.config.unsure_threshold
            ));
        }

        Ok(FilterResult {
            classification,
            score_pct,
        })
    }

    /// Classify a score percentage against the configured thresholds.
    ///
    /// - `score_pct >= spam_threshold` → Spam
    /// - `score_pct >= unsure_threshold` → Unsure
    /// - otherwise → Ham
    ///
    /// # Validates: Requirements 6.2, 6.3, 6.4
    #[must_use]
    pub fn classify_score(&self, score_pct: f64) -> Classification {
        if score_pct >= self.config.spam_threshold {
            Classification::Spam
        } else if score_pct >= self.config.unsure_threshold {
            Classification::Unsure
        } else {
            Classification::Ham
        }
    }

    /// Returns whether the filter is currently enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Returns a reference to the classifier Arc for inspection.
    pub fn classifier(&self) -> &Arc<Mutex<Classifier>> {
        &self.classifier
    }

    /// Returns the current spam threshold (as a percentage).
    #[must_use]
    pub fn spam_threshold(&self) -> f64 {
        self.config.spam_threshold
    }

    /// Returns the current unsure threshold (as a percentage).
    #[must_use]
    pub fn unsure_threshold(&self) -> f64 {
        self.config.unsure_threshold
    }

    /// Returns a reference to the filter configuration.
    #[must_use]
    pub fn config(&self) -> &FilterConfig {
        &self.config
    }

    /// Update the filter configuration.
    pub fn set_config(&mut self, config: FilterConfig) {
        self.config = config;
    }

    /// Filter a message: classify it, save score info, and perform the configured action.
    ///
    /// This method orchestrates the full filter pipeline:
    /// 1. Tokenize and score the raw message bytes
    /// 2. Optionally save spam probability to the message's score field
    /// 3. Set cleanup timestamp on spam messages (for auto-deletion tracking)
    /// 4. Mark the message as read if configured
    /// 5. Perform the configured action (move, copy, or leave untouched)
    ///
    /// # Arguments
    ///
    /// * `message` - A mutable reference to a filterable message
    /// * `message_bytes` - Raw RFC 2822 message content for tokenization
    /// * `general_config` - General configuration (for `field_score_name`)
    ///
    /// # Returns
    ///
    /// A [`FilterResult`] with the classification and score on success,
    /// or a [`FilterError`] if the operation fails.
    ///
    /// # Validates: Requirements 6.5, 6.6, 6.7, 6.8, 6.9, 6.10, 6.11, 6.12, 6.13
    pub fn filter_message(
        &self,
        message: &mut dyn FilterableMessage,
        message_bytes: &[u8],
        general_config: &GeneralConfig,
    ) -> Result<FilterResult, FilterError> {
        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "filter_message: processing {} bytes of message content",
                message_bytes.len()
            ));
        }

        // Step 1: Score and classify the message.
        let result = self.classify_raw(message_bytes)?;

        // Notify the statistics observer of the classification result.
        if let Some(stats) = &self.statistics {
            stats.on_classified(result.classification);
        }

        // Step 2: Determine action and destination based on classification.
        let (action, folder_id, mark_as_read) = match result.classification {
            Classification::Spam => (
                &self.config.spam_action,
                &self.config.spam_folder_id,
                self.config.spam_mark_as_read,
            ),
            Classification::Unsure => (
                &self.config.unsure_action,
                &self.config.unsure_folder_id,
                self.config.unsure_mark_as_read,
            ),
            Classification::Ham => (
                &self.config.ham_action,
                &self.config.ham_folder_id,
                self.config.ham_mark_as_read,
            ),
        };

        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "filter_message: classification={:?}, score={:.2}%, action={:?}, mark_as_read={}",
                result.classification, result.score_pct, action, mark_as_read
            ));
        }

        // Step 3: Mark as read if configured for this classification.
        if mark_as_read {
            if let Err(e) = message.set_read_state(true) {
                if let Some(logger) = &self.logger {
                    logger.verbose("filter", &format!(
                        "filter_message: failed to mark as read: {e}"
                    ));
                }
            }
        }

        // Step 4: Perform the filter action (move, copy, or leave untouched).
        // The move/copy MUST happen BEFORE saving properties. Saving properties
        // to a message in an Exchange-managed folder (e.g. "Junk Email") modifies
        // it in-place, which can trigger server-side rule re-evaluation and
        // bounce the message back. After the move, property saves target the
        // message in its new (SpamBayes-owned) destination folder.
        match action {
            FilterAction::Untouched => {
                if let Some(logger) = &self.logger {
                    logger.verbose("filter", "filter_message: action=Untouched, leaving in place");
                }
            }
            FilterAction::Move => {
                match folder_id {
                    Some(fid) => {
                        let folder_str = fid.to_ini_str();
                        if let Some(logger) = &self.logger {
                            logger.verbose("filter", &format!(
                                "filter_message: moving to folder '{}'", folder_str
                            ));
                        }
                        if let Err(e) = message.move_to(&folder_str) {
                            if let Some(logger) = &self.logger {
                                logger.log(crate::LogLevel::Error, "filter", &format!(
                                    "filter_message: move failed: {e}"
                                ));
                            }
                            // Leave message in place per requirement 6.8
                        }
                    }
                    None => {
                        if let Some(logger) = &self.logger {
                            logger.log(crate::LogLevel::Error, "filter", &format!(
                                "filter_message: no folder configured for {:?} move action",
                                result.classification
                            ));
                        }
                    }
                }
            }
            FilterAction::Copy => {
                match folder_id {
                    Some(fid) => {
                        let folder_str = fid.to_ini_str();
                        if let Some(logger) = &self.logger {
                            logger.verbose("filter", &format!(
                                "filter_message: copying to folder '{}'", folder_str
                            ));
                        }
                        if let Err(e) = message.copy_to(&folder_str) {
                            if let Some(logger) = &self.logger {
                                logger.log(crate::LogLevel::Error, "filter", &format!(
                                    "filter_message: copy failed: {e}"
                                ));
                            }
                            // Leave message in place per requirement 6.8
                        }
                    }
                    None => {
                        if let Some(logger) = &self.logger {
                            logger.log(crate::LogLevel::Error, "filter", &format!(
                                "filter_message: no folder configured for {:?} copy action",
                                result.classification
                            ));
                        }
                    }
                }
            }
        }

        // Step 5: Post-move property saves — these now target the message in
        // its destination folder (safe from Exchange re-evaluation).

        // Set cleanup timestamp on spam messages (for auto-deletion tracking).
        if result.classification == Classification::Spam {
            self.set_cleanup_timestamp_if_missing(message);
        }

        // Save the spam score if configured.
        if self.config.save_spam_info {
            self.save_score_with_retry(message, &general_config.field_score_name, result.score_pct);
        }

        Ok(result)
    }

    /// Save the spam score to the message with retry logic for `ObjectChanged` errors.
    ///
    /// Retries up to 3 times if an `ObjectChanged` error occurs. Skips saving
    /// silently if the store is read-only or the provider is unavailable.
    ///
    /// # Validates: Requirements 6.9, 6.10, 6.11
    fn save_score_with_retry(
        &self,
        message: &mut dyn FilterableMessage,
        field_name: &str,
        score_pct: f64,
    ) {
        const MAX_RETRIES: u32 = 3;

        for attempt in 0..MAX_RETRIES {
            // Set the field value.
            if let Err(e) = message.set_field(field_name, FieldValue::Float(score_pct)) {
                match &e {
                    MsgStoreError::ReadOnly(_) | MsgStoreError::ProviderUnavailable(_) => {
                        // Skip saving — requirement 6.11
                        return;
                    }
                    _ => {
                        eprintln!(
                            "Warning: failed to set score field (attempt {}): {}",
                            attempt + 1,
                            e
                        );
                        return;
                    }
                }
            }

            // Try to save.
            match message.save() {
                Ok(()) => return,
                Err(MsgStoreError::ObjectChanged) => {
                    // Retry — requirement 6.10
                    if attempt < MAX_RETRIES - 1 {
                        continue;
                    }
                    eprintln!(
                        "Warning: failed to save spam score after {MAX_RETRIES} retries (ObjectChanged)"
                    );
                }
                Err(MsgStoreError::ReadOnly(_) | MsgStoreError::ProviderUnavailable(_)) => {
                    // Skip saving — requirement 6.11
                    return;
                }
                Err(e) => {
                    eprintln!("Warning: failed to save spam score: {e}");
                    return;
                }
            }
        }
    }

    /// Set a cleanup timestamp on a spam message if one is not already present.
    ///
    /// The timestamp is set as seconds since UNIX epoch. This is used for
    /// auto-deletion tracking (`spam_auto_cleanup` feature).
    ///
    /// # Validates: Requirement 6.12
    fn set_cleanup_timestamp_if_missing(&self, message: &mut dyn FilterableMessage) {
        // Only set if not already present.
        if message.get_field(CLEANUP_FIELD).is_some() {
            return;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);

        if let Err(e) = message.set_field(CLEANUP_FIELD, FieldValue::Integer(timestamp)) {
            eprintln!("Warning: failed to set cleanup timestamp: {e}");
            return;
        }

        if let Err(e) = message.save() {
            eprintln!("Warning: failed to save cleanup timestamp: {e}");
        }
    }

    /// Auto-cleanup: delete old spam messages.
    ///
    /// Scans the spam folder and deletes messages whose age (determined by the
    /// `SpamBayesCleanupTimestamp` field) exceeds the configured retention period.
    /// Messages without the timestamp field are skipped. If deletion of a
    /// message fails, the error is logged and processing continues.
    ///
    /// # Arguments
    ///
    /// * `provider` - Abstraction for accessing messages in the spam folder.
    ///
    /// # Returns
    ///
    /// A [`CleanupResult`] with counts of deleted, skipped, and errored messages,
    /// or a [`FilterError`] if the operation cannot proceed.
    ///
    /// # Validates: Requirements 18.1, 18.2, 18.3, 18.4
    pub fn cleanup_old_spam(
        &self,
        provider: &mut dyn CleanupProvider,
    ) -> Result<CleanupResult, FilterError> {
        // Requirement 18.1: Return early if cleanup is disabled.
        if !self.config.spam_auto_cleanup_enabled {
            if let Some(logger) = &self.logger {
                logger.verbose("filter", "cleanup_old_spam: disabled, skipping");
            }
            return Ok(CleanupResult {
                deleted_count: 0,
                skipped_count: 0,
                error_count: 0,
            });
        }

        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "cleanup_old_spam: running (retention={} days)",
                self.config.spam_auto_cleanup_days
            ));
        }

        // Requirement 18.1: Must have a spam folder configured.
        let spam_folder_id = match &self.config.spam_folder_id {
            Some(fid) => fid.clone(),
            None => {
                return Err(FilterError::DestinationFolderUnavailable(
                    "spam folder not configured for auto-cleanup".to_string(),
                ));
            }
        };

        // Get all messages from the spam folder.
        let mut messages = provider.get_spam_folder_messages(&spam_folder_id)?;

        if let Some(logger) = &self.logger {
            logger.log(crate::LogLevel::Info, "filter", &format!(
                "cleanup_old_spam: found {} messages in spam folder", messages.len()
            ));
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);

        let mut result = CleanupResult {
            deleted_count: 0,
            skipped_count: 0,
            error_count: 0,
        };

        for msg in &mut messages {
            // Determine the message age. Try SpamBayesCleanupTimestamp first
            // (set when the filter engine moves a message to spam), then fall
            // back to ReceivedTime (always available on Outlook messages).
            // This ensures messages that were moved to spam before the cleanup
            // feature was enabled (or on Exchange stores where custom properties
            // can't be saved) are still eligible for deletion.
            let timestamp = if let Some(FieldValue::Integer(ts)) = msg.get_field(CLEANUP_FIELD) {
                if let Some(logger) = &self.logger {
                    logger.verbose("filter", &format!(
                        "cleanup_old_spam: using CleanupTimestamp={}", ts
                    ));
                }
                ts
            } else if let Some(FieldValue::Integer(ts)) = msg.get_field("ReceivedTime") {
                if let Some(logger) = &self.logger {
                    logger.verbose("filter", &format!(
                        "cleanup_old_spam: using ReceivedTime fallback={}", ts
                    ));
                }
                ts
            } else {
                // No timestamp available at all — skip.
                if let Some(logger) = &self.logger {
                    logger.verbose("filter", "cleanup_old_spam: no timestamp or ReceivedTime, skipping");
                }
                result.skipped_count += 1;
                continue;
            };

            // Requirement 18.2: Calculate age in whole days.
            let age_days = (now_secs - timestamp) / 86400;

            if age_days >= i64::from(self.config.spam_auto_cleanup_days) {
                // Message has exceeded retention period — delete it.
                if let Some(logger) = &self.logger {
                    logger.verbose("filter", &format!(
                        "cleanup_old_spam: deleting message (age={} days)", age_days
                    ));
                }
                match msg.delete() {
                    Ok(()) => result.deleted_count += 1,
                    Err(e) => {
                        // Requirement 18.4: Log error and continue.
                        if let Some(logger) = &self.logger {
                            logger.log(crate::LogLevel::Error, "filter", &format!(
                                "cleanup_old_spam: delete failed: {e}"
                            ));
                        }
                        result.error_count += 1;
                    }
                }
            }
            // If age < retention, message is kept (not counted anywhere special).
        }

        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "cleanup_old_spam: complete — deleted={}, skipped={}, errors={}",
                result.deleted_count, result.skipped_count, result.error_count
            ));
        }

        Ok(result)
    }

    /// Process all messages in folders (Filter Now operation).
    ///
    /// Iterates through the configured folders (and sub-folders if enabled),
    /// scoring and optionally filtering each message. Supports skip logic
    /// for read and previously-scored messages, reports progress, and
    /// honours cancellation requests.
    ///
    /// # Arguments
    ///
    /// * `filter_now_config` - Configuration for the Filter Now operation
    /// * `general_config` - General configuration (for `field_score_name`)
    /// * `folder_provider` - Abstraction for folder/message access
    /// * `progress` - Progress reporter and cancellation checker
    ///
    /// # Returns
    ///
    /// A [`FilterNowResult`] with counts of spam, unsure, ham, and errors,
    /// plus a cancelled flag. Returns [`FilterError::NoFoldersConfigured`]
    /// if no folders are specified.
    ///
    /// # Validates: Requirements 8.1, 8.2, 8.3, 8.4, 8.5, 8.6, 8.7
    pub fn filter_now(
        &self,
        filter_now_config: &FilterNowConfig,
        general_config: &GeneralConfig,
        folder_provider: &dyn FolderProvider,
        progress: &mut dyn Progress,
    ) -> Result<FilterNowResult, FilterError> {
        // Requirement 8.7: Error if no folders configured.
        if filter_now_config.folder_ids.is_empty() {
            return Err(FilterError::NoFoldersConfigured);
        }

        if let Some(logger) = &self.logger {
            logger.verbose("filter", &format!(
                "filter_now: starting ({} folders, include_sub={}, only_unread={}, only_unseen={})",
                filter_now_config.folder_ids.len(),
                filter_now_config.include_sub,
                filter_now_config.only_unread,
                filter_now_config.only_unseen,
            ));
        }

        let mut result = FilterNowResult::new();

        // Requirement 8.1: Expand folder list to include sub-folders if configured.
        let mut all_folders: Vec<FolderId> = Vec::new();
        for folder_id in &filter_now_config.folder_ids {
            all_folders.push(folder_id.clone());
            if filter_now_config.include_sub {
                if let Ok(subs) = folder_provider.get_sub_folders(folder_id) {
                    all_folders.extend(subs);
                }
            }
        }

        // Process each folder.
        for folder_id in &all_folders {
            // Get folder name for progress reporting.
            let folder_name = folder_provider
                .get_folder_name(folder_id)
                .unwrap_or_else(|_| "(unknown)".to_string());

            // Get messages in this folder.
            let messages = match folder_provider.get_messages(folder_id) {
                Ok(msgs) => msgs,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to get messages from folder '{folder_name}': {e}"
                    );
                    result.error_count += 1;
                    continue;
                }
            };

            let total = messages.len() as u32;
            let mut count: u32 = 0;

            for mut msg in messages {
                // Requirement 8.6: Check for cancellation before each message.
                if progress.is_cancelled() {
                    result.cancelled = true;
                    return Ok(result);
                }

                count += 1;

                // Requirement 8.2: Skip read messages if only_unread is enabled.
                if filter_now_config.only_unread && msg.get_read_state() {
                    // Requirement 8.5: Report progress even for skipped messages.
                    progress.report(&folder_name, count, total);
                    continue;
                }

                // Requirement 8.3: Skip messages that already have a score if only_unseen.
                if filter_now_config.only_unseen
                    && msg.get_field(&general_config.field_score_name).is_some() {
                        progress.report(&folder_name, count, total);
                        continue;
                    }

                // Get raw content for scoring.
                let raw_content = match msg.get_raw_content() {
                    Ok(content) => content,
                    Err(e) => {
                        eprintln!("Warning: failed to get message content: {e}");
                        result.error_count += 1;
                        progress.report(&folder_name, count, total);
                        continue;
                    }
                };

                // Requirement 8.4: If action_all, perform full filter pipeline;
                // otherwise just score and save info.
                if filter_now_config.action_all {
                    // Full filter: score + actions (move/copy).
                    match self.filter_message(msg.as_mut(), &raw_content, general_config) {
                        Ok(filter_result) => match filter_result.classification {
                            Classification::Spam => result.spam_count += 1,
                            Classification::Unsure => result.unsure_count += 1,
                            Classification::Ham => result.ham_count += 1,
                        },
                        Err(e) => {
                            eprintln!("Warning: failed to filter message: {e}");
                            result.error_count += 1;
                        }
                    }
                } else {
                    // Score only: classify and save score without move/copy.
                    match self.classify_raw(&raw_content) {
                        Ok(filter_result) => {
                            // Save score to message if save_spam_info is enabled.
                            if self.config.save_spam_info {
                                self.save_score_with_retry(
                                    msg.as_mut(),
                                    &general_config.field_score_name,
                                    filter_result.score_pct,
                                );
                            }
                            match filter_result.classification {
                                Classification::Spam => result.spam_count += 1,
                                Classification::Unsure => result.unsure_count += 1,
                                Classification::Ham => result.ham_count += 1,
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: failed to score message: {e}");
                            result.error_count += 1;
                        }
                    }
                }

                // Requirement 8.5: Report progress (folder name, count/total).
                progress.report(&folder_name, count, total);
            }
        }

        Ok(result)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test assertions comparing exact threshold values
mod tests {
    use super::*;
    use spambayes_config::{FilterNowConfig, FolderId};
    use spambayes_storage::{ClassifierState, MessageInfo, StorageError, WordChange};
    use std::cell::RefCell;
    use std::collections::HashMap;

    // ── Mock StorageBackend ──────────────────────────────────────────────

    struct MockStorage;

    impl StorageBackend for MockStorage {
        fn load(&mut self) -> Result<ClassifierState, StorageError> {
            Ok(ClassifierState::default())
        }

        fn store(
            &mut self,
            _state: &ClassifierState,
            _changed: &HashMap<Vec<u8>, WordChange>,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), StorageError> {
            Ok(())
        }
    }

    // ── Mock MessageDatabase ─────────────────────────────────────────────

    struct MockMessageDb;

    impl MessageDatabase for MockMessageDb {
        fn load_msg(&self, _search_key: &[u8]) -> Option<MessageInfo> {
            None
        }

        fn store_msg(&mut self, _search_key: &[u8], _info: &MessageInfo) {}

        fn remove_msg(&mut self, _search_key: &[u8]) {}
    }

    // ── Mock FilterableMessage ───────────────────────────────────────────

    /// Tracks all operations performed on the message for test assertions.
    #[derive(Debug)]
    struct MockMessage {
        fields: RefCell<HashMap<String, FieldValue>>,
        moved_to: RefCell<Option<String>>,
        copied_to: RefCell<Option<String>>,
        read_state: RefCell<Option<bool>>,
        save_count: RefCell<u32>,
        save_error: RefCell<Option<MsgStoreError>>,
        set_field_error: RefCell<Option<MsgStoreError>>,
    }

    impl MockMessage {
        fn new() -> Self {
            Self {
                fields: RefCell::new(HashMap::new()),
                moved_to: RefCell::new(None),
                copied_to: RefCell::new(None),
                read_state: RefCell::new(None),
                save_count: RefCell::new(0),
                save_error: RefCell::new(None),
                set_field_error: RefCell::new(None),
            }
        }

        fn with_field(self, name: &str, value: FieldValue) -> Self {
            self.fields.borrow_mut().insert(name.to_string(), value);
            self
        }

        #[allow(dead_code)]
        fn set_save_error(&self, err: MsgStoreError) {
            *self.save_error.borrow_mut() = Some(err);
        }

        fn set_field_error(&self, err: MsgStoreError) {
            *self.set_field_error.borrow_mut() = Some(err);
        }
    }

    impl FilterableMessage for MockMessage {
        fn get_field(&self, name: &str) -> Option<FieldValue> {
            self.fields.borrow().get(name).cloned()
        }

        fn set_field(&mut self, name: &str, value: FieldValue) -> Result<(), MsgStoreError> {
            if let Some(err) = self.set_field_error.borrow_mut().take() {
                return Err(err);
            }
            self.fields.borrow_mut().insert(name.to_string(), value);
            Ok(())
        }

        fn save(&mut self) -> Result<(), MsgStoreError> {
            *self.save_count.borrow_mut() += 1;
            if let Some(err) = self.save_error.borrow_mut().take() {
                return Err(err);
            }
            Ok(())
        }

        fn move_to(&mut self, folder_id: &str) -> Result<(), MsgStoreError> {
            *self.moved_to.borrow_mut() = Some(folder_id.to_string());
            Ok(())
        }

        fn copy_to(&self, folder_id: &str) -> Result<(), MsgStoreError> {
            *self.copied_to.borrow_mut() = Some(folder_id.to_string());
            Ok(())
        }

        fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError> {
            *self.read_state.borrow_mut() = Some(read);
            Ok(())
        }
    }

    /// Mock that returns `NotFound` when move/copy is called.
    struct MockMessageMoveError {
        inner: MockMessage,
    }

    impl MockMessageMoveError {
        fn new() -> Self {
            Self { inner: MockMessage::new() }
        }
    }

    impl FilterableMessage for MockMessageMoveError {
        fn get_field(&self, name: &str) -> Option<FieldValue> {
            self.inner.get_field(name)
        }
        fn set_field(&mut self, name: &str, value: FieldValue) -> Result<(), MsgStoreError> {
            self.inner.set_field(name, value)
        }
        fn save(&mut self) -> Result<(), MsgStoreError> {
            self.inner.save()
        }
        fn move_to(&mut self, _folder_id: &str) -> Result<(), MsgStoreError> {
            Err(MsgStoreError::NotFound("folder does not exist".to_string()))
        }
        fn copy_to(&self, _folder_id: &str) -> Result<(), MsgStoreError> {
            Err(MsgStoreError::NotFound("folder does not exist".to_string()))
        }
        fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError> {
            self.inner.set_read_state(read)
        }
    }

    /// Mock that returns `ObjectChanged` on save N times then succeeds.
    struct MockMessageObjectChanged {
        inner: MockMessage,
        failures_remaining: RefCell<u32>,
    }

    impl MockMessageObjectChanged {
        fn new(fail_count: u32) -> Self {
            Self {
                inner: MockMessage::new(),
                failures_remaining: RefCell::new(fail_count),
            }
        }
    }

    impl FilterableMessage for MockMessageObjectChanged {
        fn get_field(&self, name: &str) -> Option<FieldValue> {
            self.inner.get_field(name)
        }
        fn set_field(&mut self, name: &str, value: FieldValue) -> Result<(), MsgStoreError> {
            self.inner.set_field(name, value)
        }
        fn save(&mut self) -> Result<(), MsgStoreError> {
            let mut remaining = self.failures_remaining.borrow_mut();
            if *remaining > 0 {
                *remaining -= 1;
                Err(MsgStoreError::ObjectChanged)
            } else {
                Ok(())
            }
        }
        fn move_to(&mut self, folder_id: &str) -> Result<(), MsgStoreError> {
            self.inner.move_to(folder_id)
        }
        fn copy_to(&self, folder_id: &str) -> Result<(), MsgStoreError> {
            self.inner.copy_to(folder_id)
        }
        fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError> {
            self.inner.set_read_state(read)
        }
    }

    // ── Helper ───────────────────────────────────────────────────────────

    fn make_engine(config: FilterConfig) -> FilterEngine {
        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        let storage: Arc<Mutex<Box<dyn StorageBackend>>> =
            Arc::new(Mutex::new(Box::new(MockStorage)));
        let message_db: Arc<Mutex<Box<dyn MessageDatabase>>> =
            Arc::new(Mutex::new(Box::new(MockMessageDb)));

        FilterEngine::new(config, classifier, storage, message_db, None)
    }

    fn default_general_config() -> GeneralConfig {
        GeneralConfig::default()
    }

    fn make_spam_folder_id() -> FolderId {
        FolderId::from_ini_str("('AABB0011', 'CCDD2233')").unwrap()
    }

    fn make_unsure_folder_id() -> FolderId {
        FolderId::from_ini_str("('AABB0011', 'EEFF4455')").unwrap()
    }

    // ── Classification Threshold Tests ───────────────────────────────────

    #[test]
    fn classify_score_spam_at_threshold() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // Exactly at spam threshold (90%) → Spam
        assert_eq!(engine.classify_score(90.0), Classification::Spam);
    }

    #[test]
    fn classify_score_spam_above_threshold() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // Above spam threshold → Spam
        assert_eq!(engine.classify_score(95.0), Classification::Spam);
        assert_eq!(engine.classify_score(100.0), Classification::Spam);
    }

    #[test]
    fn classify_score_unsure_at_threshold() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // Exactly at unsure threshold (15%) → Unsure
        assert_eq!(engine.classify_score(15.0), Classification::Unsure);
    }

    #[test]
    fn classify_score_unsure_between_thresholds() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // Between unsure and spam thresholds → Unsure
        assert_eq!(engine.classify_score(50.0), Classification::Unsure);
        assert_eq!(engine.classify_score(89.9), Classification::Unsure);
    }

    #[test]
    fn classify_score_ham_below_unsure_threshold() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // Below unsure threshold (15%) → Ham
        assert_eq!(engine.classify_score(14.9), Classification::Ham);
        assert_eq!(engine.classify_score(0.0), Classification::Ham);
        assert_eq!(engine.classify_score(10.0), Classification::Ham);
    }

    #[test]
    fn classify_score_custom_thresholds() {
        let config = FilterConfig {
            spam_threshold: 80.0,
            unsure_threshold: 20.0,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Custom thresholds: spam >= 80, unsure >= 20
        assert_eq!(engine.classify_score(80.0), Classification::Spam);
        assert_eq!(engine.classify_score(79.9), Classification::Unsure);
        assert_eq!(engine.classify_score(20.0), Classification::Unsure);
        assert_eq!(engine.classify_score(19.9), Classification::Ham);
    }

    #[test]
    fn classify_raw_untrained_classifier_returns_neutral() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // An untrained classifier returns 0.5 (50%) for any message,
        // which is between unsure (15%) and spam (90%) → Unsure.
        let msg = b"Subject: Hello\r\n\r\nThis is a test message.";
        let result = engine.classify_raw(msg).unwrap();

        assert_eq!(result.classification, Classification::Unsure);
        assert!((result.score_pct - 50.0).abs() < 1.0);
    }

    #[test]
    fn classify_raw_empty_message() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        // Empty message produces no significant tokens → neutral score (50%)
        let result = engine.classify_raw(b"").unwrap();
        assert_eq!(result.classification, Classification::Unsure);
    }

    #[test]
    fn filter_result_score_percentage_range() {
        let config = FilterConfig::default();
        let engine = make_engine(config);

        let msg = b"Subject: Test\r\n\r\nBody content here.";
        let result = engine.classify_raw(msg).unwrap();

        // Score should be between 0 and 100 percent
        assert!(result.score_pct >= 0.0);
        assert!(result.score_pct <= 100.0);
    }

    #[test]
    fn engine_accessors() {
        let config = FilterConfig {
            enabled: true,
            spam_threshold: 85.0,
            unsure_threshold: 20.0,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        assert!(engine.is_enabled());
        assert_eq!(engine.spam_threshold(), 85.0);
        assert_eq!(engine.unsure_threshold(), 20.0);
    }

    #[test]
    fn set_config_updates_thresholds() {
        let config = FilterConfig::default();
        let mut engine = make_engine(config);

        assert_eq!(engine.spam_threshold(), 90.0);

        let new_config = FilterConfig {
            spam_threshold: 75.0,
            unsure_threshold: 25.0,
            ..FilterConfig::default()
        };
        engine.set_config(new_config);

        assert_eq!(engine.spam_threshold(), 75.0);
        assert_eq!(engine.unsure_threshold(), 25.0);
    }

    // ── Filter Action Tests ──────────────────────────────────────────────

    #[test]
    fn filter_message_untouched_action_leaves_message_in_place() {
        let config = FilterConfig {
            unsure_action: FilterAction::Untouched,
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nBody", &general)
            .unwrap();

        // Untrained classifier → Unsure (50%), action is Untouched
        assert_eq!(result.classification, Classification::Unsure);
        assert_eq!(*msg.moved_to.borrow(), None);
        assert_eq!(*msg.copied_to.borrow(), None);
    }

    #[test]
    fn filter_message_move_action_moves_to_configured_folder() {
        let spam_folder = make_spam_folder_id();
        let config = FilterConfig {
            spam_threshold: 0.0,
            spam_action: FilterAction::Move,
            spam_folder_id: Some(spam_folder.clone()),
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Spam\r\n\r\nBuy now!", &general)
            .unwrap();

        assert_eq!(result.classification, Classification::Spam);
        assert_eq!(*msg.moved_to.borrow(), Some(spam_folder.to_ini_str()));
    }

    #[test]
    fn filter_message_copy_action_copies_to_configured_folder() {
        let unsure_folder = make_unsure_folder_id();
        let config = FilterConfig {
            unsure_action: FilterAction::Copy,
            unsure_folder_id: Some(unsure_folder.clone()),
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nHello", &general)
            .unwrap();

        assert_eq!(result.classification, Classification::Unsure);
        assert_eq!(*msg.copied_to.borrow(), Some(unsure_folder.to_ini_str()));
    }

    #[test]
    fn filter_message_saves_score_when_enabled() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general)
            .unwrap();

        let saved = msg.fields.borrow().get("Spam").cloned();
        assert!(saved.is_some());
        if let Some(FieldValue::Float(score)) = saved {
            assert!((score - result.score_pct).abs() < 0.01);
        } else {
            panic!("Expected Float field value");
        }
    }

    #[test]
    fn filter_message_no_score_when_disabled() {
        let config = FilterConfig {
            save_spam_info: false,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general)
            .unwrap();

        assert!(msg.fields.borrow().get("Spam").is_none());
    }

    #[test]
    fn filter_message_marks_as_read_when_configured() {
        let config = FilterConfig {
            unsure_mark_as_read: true,
            unsure_action: FilterAction::Untouched,
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general)
            .unwrap();

        assert_eq!(*msg.read_state.borrow(), Some(true));
    }

    #[test]
    fn filter_message_no_mark_as_read_when_not_configured() {
        let config = FilterConfig {
            unsure_mark_as_read: false,
            unsure_action: FilterAction::Untouched,
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general)
            .unwrap();

        assert_eq!(*msg.read_state.borrow(), None);
    }

    #[test]
    fn filter_message_sets_cleanup_timestamp_on_spam() {
        let spam_folder = make_spam_folder_id();
        let config = FilterConfig {
            spam_threshold: 0.0,
            spam_action: FilterAction::Move,
            spam_folder_id: Some(spam_folder),
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        engine
            .filter_message(&mut msg, b"Subject: Spam\r\n\r\nBuy!", &general)
            .unwrap();

        let ts = msg.fields.borrow().get("SpamBayesCleanupTimestamp").cloned();
        assert!(ts.is_some());
        if let Some(FieldValue::Integer(val)) = ts {
            assert!(val > 0);
        } else {
            panic!("Expected Integer cleanup timestamp");
        }
    }

    #[test]
    fn filter_message_preserves_existing_cleanup_timestamp() {
        let spam_folder = make_spam_folder_id();
        let config = FilterConfig {
            spam_threshold: 0.0,
            spam_action: FilterAction::Move,
            spam_folder_id: Some(spam_folder),
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new()
            .with_field("SpamBayesCleanupTimestamp", FieldValue::Integer(12345));
        let general = default_general_config();

        engine
            .filter_message(&mut msg, b"Subject: Spam\r\n\r\nBuy!", &general)
            .unwrap();

        let ts = msg.fields.borrow().get("SpamBayesCleanupTimestamp").cloned();
        assert_eq!(ts, Some(FieldValue::Integer(12345)));
    }

    #[test]
    fn filter_message_logs_and_leaves_when_move_fails() {
        let spam_folder = make_spam_folder_id();
        let config = FilterConfig {
            spam_threshold: 0.0,
            spam_action: FilterAction::Move,
            spam_folder_id: Some(spam_folder),
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessageMoveError::new();
        let general = default_general_config();

        // Should succeed — logs error and leaves message in place
        let result = engine
            .filter_message(&mut msg, b"Subject: Spam\r\n\r\nBuy!", &general);
        assert!(result.is_ok());
    }

    #[test]
    fn filter_message_leaves_in_place_when_no_folder_configured() {
        let config = FilterConfig {
            spam_threshold: 0.0,
            spam_action: FilterAction::Move,
            spam_folder_id: None,
            save_spam_info: false,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Spam\r\n\r\nBuy!", &general);
        assert!(result.is_ok());
        assert_eq!(*msg.moved_to.borrow(), None);
    }

    #[test]
    fn save_score_retries_on_object_changed() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessageObjectChanged::new(2);
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general);
        assert!(result.is_ok());
        // Score field should be set (succeeded on 3rd try)
        let score = msg.inner.fields.borrow().get("Spam").cloned();
        assert!(score.is_some());
    }

    #[test]
    fn save_score_skips_on_read_only() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        msg.set_field_error(MsgStoreError::ReadOnly("test".to_string()));
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general);
        assert!(result.is_ok());
    }

    #[test]
    fn save_score_skips_on_provider_unavailable() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        msg.set_field_error(MsgStoreError::ProviderUnavailable("Hotmail".to_string()));
        let general = default_general_config();

        let result = engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general);
        assert!(result.is_ok());
    }

    #[test]
    fn filter_message_uses_custom_score_field_name() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut msg = MockMessage::new();
        let general = GeneralConfig {
            field_score_name: "MyScore".to_string(),
            ..GeneralConfig::default()
        };

        engine
            .filter_message(&mut msg, b"Subject: Test\r\n\r\nContent", &general)
            .unwrap();

        assert!(msg.fields.borrow().get("MyScore").is_some());
        assert!(msg.fields.borrow().get("Spam").is_none());
    }

    // ── Filter Now Tests ─────────────────────────────────────────────────

    // ── Mock FilterNowMessage ────────────────────────────────────────────

    /// A mock message for Filter Now operations that extends `MockMessage`
    /// with read state and raw content access.
    struct MockFilterNowMessage {
        fields: RefCell<HashMap<String, FieldValue>>,
        moved_to: RefCell<Option<String>>,
        copied_to: RefCell<Option<String>>,
        read_state_val: bool,
        save_count: RefCell<u32>,
        raw_content: Vec<u8>,
        content_error: bool,
    }

    impl MockFilterNowMessage {
        fn new(read: bool, content: &[u8]) -> Self {
            Self {
                fields: RefCell::new(HashMap::new()),
                moved_to: RefCell::new(None),
                copied_to: RefCell::new(None),
                read_state_val: read,
                save_count: RefCell::new(0),
                raw_content: content.to_vec(),
                content_error: false,
            }
        }

        fn with_field(self, name: &str, value: FieldValue) -> Self {
            self.fields.borrow_mut().insert(name.to_string(), value);
            self
        }

        fn with_content_error(mut self) -> Self {
            self.content_error = true;
            self
        }
    }

    impl FilterableMessage for MockFilterNowMessage {
        fn get_field(&self, name: &str) -> Option<FieldValue> {
            self.fields.borrow().get(name).cloned()
        }

        fn set_field(&mut self, name: &str, value: FieldValue) -> Result<(), MsgStoreError> {
            self.fields.borrow_mut().insert(name.to_string(), value);
            Ok(())
        }

        fn save(&mut self) -> Result<(), MsgStoreError> {
            *self.save_count.borrow_mut() += 1;
            Ok(())
        }

        fn move_to(&mut self, folder_id: &str) -> Result<(), MsgStoreError> {
            *self.moved_to.borrow_mut() = Some(folder_id.to_string());
            Ok(())
        }

        fn copy_to(&self, folder_id: &str) -> Result<(), MsgStoreError> {
            *self.copied_to.borrow_mut() = Some(folder_id.to_string());
            Ok(())
        }

        fn set_read_state(&mut self, _read: bool) -> Result<(), MsgStoreError> {
            Ok(())
        }
    }

    impl FilterNowMessage for MockFilterNowMessage {
        fn get_read_state(&self) -> bool {
            self.read_state_val
        }

        fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError> {
            if self.content_error {
                Err(MsgStoreError::NotFound("content unavailable".to_string()))
            } else {
                Ok(self.raw_content.clone())
            }
        }
    }

    // Better mock that uses RefCell to allow message consumption
    struct MockFolderProviderWithMessages {
        folders: HashMap<String, String>,
        sub_folders: HashMap<String, Vec<FolderId>>,
        messages: RefCell<HashMap<String, Vec<Box<dyn FilterNowMessage>>>>,
    }

    impl MockFolderProviderWithMessages {
        fn new() -> Self {
            Self {
                folders: HashMap::new(),
                sub_folders: HashMap::new(),
                messages: RefCell::new(HashMap::new()),
            }
        }

        fn add_folder(mut self, folder_id: &FolderId, name: &str) -> Self {
            self.folders.insert(folder_id.to_ini_str(), name.to_string());
            self
        }

        fn add_sub_folders(mut self, parent: &FolderId, children: Vec<FolderId>) -> Self {
            self.sub_folders.insert(parent.to_ini_str(), children);
            self
        }

        fn add_messages(
            self,
            folder_id: &FolderId,
            msgs: Vec<Box<dyn FilterNowMessage>>,
        ) -> Self {
            self.messages
                .borrow_mut()
                .insert(folder_id.to_ini_str(), msgs);
            self
        }
    }

    impl FolderProvider for MockFolderProviderWithMessages {
        fn get_folder_name(&self, folder_id: &FolderId) -> Result<String, MsgStoreError> {
            self.folders
                .get(&folder_id.to_ini_str())
                .cloned()
                .ok_or_else(|| MsgStoreError::NotFound("folder not found".to_string()))
        }

        fn get_sub_folders(&self, folder_id: &FolderId) -> Result<Vec<FolderId>, MsgStoreError> {
            Ok(self
                .sub_folders
                .get(&folder_id.to_ini_str())
                .cloned()
                .unwrap_or_default())
        }

        fn get_messages(
            &self,
            folder_id: &FolderId,
        ) -> Result<Vec<Box<dyn FilterNowMessage>>, MsgStoreError> {
            let mut map = self.messages.borrow_mut();
            Ok(map.remove(&folder_id.to_ini_str()).unwrap_or_default())
        }

        fn message_count(&self, folder_id: &FolderId) -> Result<u32, MsgStoreError> {
            Ok(self
                .messages
                .borrow()
                .get(&folder_id.to_ini_str())
                .map_or(0, |m| m.len() as u32))
        }
    }

    // ── Mock Progress ────────────────────────────────────────────────────

    struct MockProgress {
        reports: RefCell<Vec<(String, u32, u32)>>,
        cancel_after: Option<u32>,
        report_count: RefCell<u32>,
    }

    impl MockProgress {
        fn new() -> Self {
            Self {
                reports: RefCell::new(Vec::new()),
                cancel_after: None,
                report_count: RefCell::new(0),
            }
        }

        fn cancel_after(mut self, n: u32) -> Self {
            self.cancel_after = Some(n);
            self
        }
    }

    impl Progress for MockProgress {
        fn report(&mut self, folder_name: &str, current: u32, total: u32) {
            self.reports
                .borrow_mut()
                .push((folder_name.to_string(), current, total));
            *self.report_count.borrow_mut() += 1;
        }

        fn is_cancelled(&self) -> bool {
            if let Some(limit) = self.cancel_after {
                *self.report_count.borrow() >= limit
            } else {
                false
            }
        }
    }

    // ── Helper for Filter Now tests ──────────────────────────────────────

    fn make_filter_now_folder_id() -> FolderId {
        FolderId::from_ini_str("('AA00BB11', 'CC22DD33')").unwrap()
    }

    fn make_sub_folder_id() -> FolderId {
        FolderId::from_ini_str("('AA00BB11', 'EE44FF55')").unwrap()
    }

    // ── Filter Now Unit Tests ────────────────────────────────────────────

    #[test]
    fn filter_now_no_folders_returns_error() {
        let config = FilterConfig::default();
        let engine = make_engine(config);
        let filter_now_config = FilterNowConfig {
            folder_ids: Vec::new(),
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let provider = MockFolderProviderWithMessages::new();
        let mut progress = MockProgress::new();

        let result = engine.filter_now(&filter_now_config, &general, &provider, &mut progress);

        assert!(result.is_err());
        match result.unwrap_err() {
            FilterError::NoFoldersConfigured => {}
            other => panic!("Expected NoFoldersConfigured, got: {other:?}"),
        }
    }

    #[test]
    fn filter_now_processes_all_messages() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Msg1\r\n\r\nBody1",
            )),
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Msg2\r\n\r\nBody2",
            )),
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Msg3\r\n\r\nBody3",
            )),
        ];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "Inbox")
            .add_messages(&folder, msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder],
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // All 3 messages should be processed (untrained → unsure)
        assert_eq!(result.unsure_count, 3);
        assert_eq!(result.spam_count, 0);
        assert_eq!(result.ham_count, 0);
        assert_eq!(result.error_count, 0);
        assert!(!result.cancelled);
    }

    #[test]
    fn filter_now_respects_only_unread_skip() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![
            // read=true → should be skipped with only_unread
            Box::new(MockFilterNowMessage::new(
                true,
                b"Subject: Read\r\n\r\nAlready read",
            )),
            // read=false → should be processed
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Unread\r\n\r\nNot read",
            )),
        ];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "Inbox")
            .add_messages(&folder, msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder],
            only_unread: true,
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // Only 1 message should be processed (the unread one)
        assert_eq!(result.unsure_count, 1);
        assert_eq!(result.spam_count, 0);
        assert_eq!(result.ham_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn filter_now_respects_only_unseen_skip() {
        let config = FilterConfig {
            save_spam_info: true,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![
            // Has a score field → should be skipped with only_unseen
            Box::new(
                MockFilterNowMessage::new(false, b"Subject: Scored\r\n\r\nAlready scored")
                    .with_field("Spam", FieldValue::Float(50.0)),
            ),
            // No score field → should be processed
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Fresh\r\n\r\nNever scored",
            )),
        ];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "Inbox")
            .add_messages(&folder, msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder],
            only_unseen: true,
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // Only 1 message should be processed (the one without a score)
        assert_eq!(result.unsure_count, 1);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn filter_now_action_all_false_scores_only_no_move() {
        let spam_folder = make_spam_folder_id();
        let config = FilterConfig {
            spam_threshold: 0.0, // everything is spam
            spam_action: FilterAction::Move,
            spam_folder_id: Some(spam_folder),
            save_spam_info: true,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![Box::new(MockFilterNowMessage::new(
            false,
            b"Subject: Test\r\n\r\nBody",
        ))];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "Inbox")
            .add_messages(&folder, msgs);

        // action_all = false → score only, no move/copy
        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder.clone()],
            action_all: false,
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // Message was classified as spam (threshold=0) but NOT moved
        assert_eq!(result.spam_count, 1);
        assert_eq!(result.error_count, 0);
        assert!(!result.cancelled);
    }

    #[test]
    fn filter_now_cancellation_stops_and_reports_partial() {
        let config = FilterConfig {
            save_spam_info: false,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Msg1\r\n\r\nBody1",
            )),
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Msg2\r\n\r\nBody2",
            )),
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: Msg3\r\n\r\nBody3",
            )),
        ];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "Inbox")
            .add_messages(&folder, msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder],
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        // Cancel after 1 progress report (i.e., after processing the first message)
        let mut progress = MockProgress::cancel_after(MockProgress::new(), 1);

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // Should have processed 1 message, then cancelled
        assert!(result.cancelled);
        assert_eq!(result.unsure_count, 1);
        // Remaining messages not processed
        assert_eq!(result.unsure_count + result.spam_count + result.ham_count, 1);
    }

    #[test]
    fn filter_now_reports_progress_correctly() {
        let config = FilterConfig {
            save_spam_info: false,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: A\r\n\r\nBody",
            )),
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: B\r\n\r\nBody",
            )),
        ];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "TestFolder")
            .add_messages(&folder, msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder],
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        let reports = progress.reports.borrow();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0], ("TestFolder".to_string(), 1, 2));
        assert_eq!(reports[1], ("TestFolder".to_string(), 2, 2));
    }

    #[test]
    fn filter_now_handles_message_errors_gracefully() {
        let config = FilterConfig {
            save_spam_info: false,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let folder = make_filter_now_folder_id();

        let msgs: Vec<Box<dyn FilterNowMessage>> = vec![
            // This message will fail to get content
            Box::new(MockFilterNowMessage::new(false, b"").with_content_error()),
            // This one should process normally
            Box::new(MockFilterNowMessage::new(
                false,
                b"Subject: OK\r\n\r\nBody",
            )),
        ];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&folder, "Inbox")
            .add_messages(&folder, msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![folder],
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // One error, one success
        assert_eq!(result.error_count, 1);
        assert_eq!(result.unsure_count, 1);
        assert!(!result.cancelled);
    }

    #[test]
    fn filter_now_includes_sub_folders() {
        let config = FilterConfig {
            save_spam_info: false,
            unsure_action: FilterAction::Untouched,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let parent_folder = make_filter_now_folder_id();
        let sub_folder = make_sub_folder_id();

        let parent_msgs: Vec<Box<dyn FilterNowMessage>> = vec![Box::new(
            MockFilterNowMessage::new(false, b"Subject: Parent\r\n\r\nBody"),
        )];
        let sub_msgs: Vec<Box<dyn FilterNowMessage>> = vec![Box::new(
            MockFilterNowMessage::new(false, b"Subject: Child\r\n\r\nBody"),
        )];

        let provider = MockFolderProviderWithMessages::new()
            .add_folder(&parent_folder, "Parent")
            .add_folder(&sub_folder, "SubFolder")
            .add_sub_folders(&parent_folder, vec![sub_folder.clone()])
            .add_messages(&parent_folder, parent_msgs)
            .add_messages(&sub_folder, sub_msgs);

        let filter_now_config = FilterNowConfig {
            folder_ids: vec![parent_folder],
            include_sub: true,
            ..FilterNowConfig::default()
        };
        let general = default_general_config();
        let mut progress = MockProgress::new();

        let result = engine
            .filter_now(&filter_now_config, &general, &provider, &mut progress)
            .unwrap();

        // Should process messages from both parent and sub-folder
        assert_eq!(result.unsure_count, 2);
        assert_eq!(result.error_count, 0);
    }

    // ── Spam Auto-Cleanup Tests ──────────────────────────────────────────

    /// Mock message for cleanup operations.
    struct MockCleanupMessage {
        fields: HashMap<String, FieldValue>,
        deleted: RefCell<bool>,
        delete_error: Option<MsgStoreError>,
    }

    impl MockCleanupMessage {
        fn new() -> Self {
            Self {
                fields: HashMap::new(),
                deleted: RefCell::new(false),
                delete_error: None,
            }
        }

        fn with_timestamp(mut self, ts: i64) -> Self {
            self.fields.insert(
                "SpamBayesCleanupTimestamp".to_string(),
                FieldValue::Integer(ts),
            );
            self
        }

        fn with_delete_error(mut self, err: MsgStoreError) -> Self {
            self.delete_error = Some(err);
            self
        }
    }

    impl CleanupMessage for MockCleanupMessage {
        fn get_field(&self, name: &str) -> Option<FieldValue> {
            self.fields.get(name).cloned()
        }

        fn delete(&mut self) -> Result<(), MsgStoreError> {
            if let Some(err) = self.delete_error.take() {
                return Err(err);
            }
            *self.deleted.borrow_mut() = true;
            Ok(())
        }
    }

    /// Mock provider for cleanup that returns a configurable list of messages.
    struct MockCleanupProvider {
        messages: RefCell<Option<Vec<Box<dyn CleanupMessage>>>>,
    }

    impl MockCleanupProvider {
        fn new(messages: Vec<Box<dyn CleanupMessage>>) -> Self {
            Self {
                messages: RefCell::new(Some(messages)),
            }
        }
    }

    impl CleanupProvider for MockCleanupProvider {
        fn get_spam_folder_messages(
            &self,
            _folder_id: &FolderId,
        ) -> Result<Vec<Box<dyn CleanupMessage>>, MsgStoreError> {
            Ok(self.messages.borrow_mut().take().unwrap_or_default())
        }
    }

    /// Helper: get the current time as seconds since epoch.
    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }

    #[test]
    fn cleanup_disabled_returns_zero_result() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: false,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut provider = MockCleanupProvider::new(vec![]);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_no_spam_folder_returns_error() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_folder_id: None,
            ..FilterConfig::default()
        };
        let engine = make_engine(config);
        let mut provider = MockCleanupProvider::new(vec![]);

        let result = engine.cleanup_old_spam(&mut provider);

        assert!(result.is_err());
        match result.unwrap_err() {
            FilterError::DestinationFolderUnavailable(msg) => {
                assert!(msg.contains("spam folder"));
            }
            other => panic!("Expected DestinationFolderUnavailable, got: {other:?}"),
        }
    }

    #[test]
    fn cleanup_skips_messages_without_timestamp() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new()), // no timestamp
            Box::new(MockCleanupMessage::new()), // no timestamp
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 2);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_keeps_messages_within_retention() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Message timestamped 10 days ago — within 30-day retention
        let ten_days_ago = now_secs() - (10 * 86400);
        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(ten_days_ago)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_deletes_messages_beyond_retention() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Message timestamped 31 days ago — beyond 30-day retention
        let thirty_one_days_ago = now_secs() - (31 * 86400);
        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(thirty_one_days_ago)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 1);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_deletes_at_exact_retention_boundary() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Message timestamped exactly 30 days ago — at boundary, should be deleted
        let exactly_thirty_days = now_secs() - (30 * 86400);
        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(exactly_thirty_days)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 1);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_logs_and_continues_on_delete_error() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        let old_timestamp = now_secs() - (60 * 86400);
        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            // First message will fail to delete
            Box::new(
                MockCleanupMessage::new()
                    .with_timestamp(old_timestamp)
                    .with_delete_error(MsgStoreError::NotFound("already gone".to_string())),
            ),
            // Second message should still be processed and deleted
            Box::new(MockCleanupMessage::new().with_timestamp(old_timestamp)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 1);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 1);
    }

    #[test]
    fn cleanup_mixed_messages() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        let old_timestamp = now_secs() - (45 * 86400); // 45 days ago — expired
        let recent_timestamp = now_secs() - (5 * 86400); // 5 days ago — within retention

        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            // Expired message → deleted
            Box::new(MockCleanupMessage::new().with_timestamp(old_timestamp)),
            // No timestamp → skipped
            Box::new(MockCleanupMessage::new()),
            // Recent message → kept
            Box::new(MockCleanupMessage::new().with_timestamp(recent_timestamp)),
            // Expired but delete fails → error
            Box::new(
                MockCleanupMessage::new()
                    .with_timestamp(old_timestamp)
                    .with_delete_error(MsgStoreError::ProviderUnavailable(
                        "offline".to_string(),
                    )),
            ),
            // Another expired message → deleted
            Box::new(MockCleanupMessage::new().with_timestamp(old_timestamp)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 2);
        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.error_count, 1);
    }

    // ─── Cleanup Config-Driven Behavior Tests ────────────────────────────

    #[test]
    fn cleanup_respects_custom_retention_period() {
        // User sets 7 days via the Manager GUI checkbox/spinner
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 7,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Message is 8 days old — should be deleted with 7-day retention
        let eight_days_ago = now_secs() - (8 * 86400);
        // Message is 6 days old — should be kept with 7-day retention
        let six_days_ago = now_secs() - (6 * 86400);

        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(eight_days_ago)),
            Box::new(MockCleanupMessage::new().with_timestamp(six_days_ago)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 1);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_with_max_retention_365_days() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 365,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Message 366 days old — should be deleted
        let old = now_secs() - (366 * 86400);
        // Message 364 days old — should be kept
        let recent = now_secs() - (364 * 86400);

        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(old)),
            Box::new(MockCleanupMessage::new().with_timestamp(recent)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 1);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_with_minimum_retention_1_day() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 1,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        // Message 2 days old — should be deleted
        let two_days_ago = now_secs() - (2 * 86400);
        // Message 12 hours old — should be kept (less than 1 full day)
        let twelve_hours_ago = now_secs() - (12 * 3600);

        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(two_days_ago)),
            Box::new(MockCleanupMessage::new().with_timestamp(twelve_hours_ago)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 1);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_empty_folder_returns_zero_counts() {
        let config = FilterConfig {
            spam_auto_cleanup_enabled: true,
            spam_auto_cleanup_days: 30,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        let messages: Vec<Box<dyn CleanupMessage>> = vec![];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }

    #[test]
    fn cleanup_enabled_flag_controls_execution() {
        // Even with a valid folder and old messages, disabled = no-op
        let config = FilterConfig {
            spam_auto_cleanup_enabled: false,
            spam_auto_cleanup_days: 1,
            spam_folder_id: Some(make_spam_folder_id()),
            ..FilterConfig::default()
        };
        let engine = make_engine(config);

        let very_old = now_secs() - (9999 * 86400);
        let messages: Vec<Box<dyn CleanupMessage>> = vec![
            Box::new(MockCleanupMessage::new().with_timestamp(very_old)),
        ];
        let mut provider = MockCleanupProvider::new(messages);

        let result = engine.cleanup_old_spam(&mut provider).unwrap();

        // Disabled — nothing deleted
        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 0);
        assert_eq!(result.error_count, 0);
    }
}
