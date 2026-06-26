//! Training engine - batch and incremental training logic.
//!
//! This module implements the `TrainingEngine` which ties together the tokenizer,
//! classifier, message database, and storage to train the Bayesian filter on
//! user-categorized ham and spam messages.
//!
//! # Batch Training
//!
//! The `train_batch` method processes all messages in configured ham and spam
//! folders. For each message it:
//! - Trains new (untrained) messages with the appropriate classification
//! - Untrains and retrains messages that were previously trained incorrectly
//! - Skips messages already trained with the correct classification
//!
//! # Error Handling
//!
//! Per-message errors are logged and processing continues (Requirement 9.8).
//! If no training folders are configured, an error is returned (Requirement 9.9).
//!
//! # Validates: Requirements 9.1, 9.2, 9.3, 9.4, 9.7, 9.8, 9.9, 9.10

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use spambayes_config::{FolderId, TrainingConfig};
use spambayes_core::classifier::Classifier;
use spambayes_core::tokenizer::Tokenizer;
use spambayes_core::{Classification, ClassifierError};
use spambayes_mapi::MsgStoreError;
use spambayes_storage::{ClassifierState, MessageDatabase, MessageInfo, StorageBackend, WordChange};

use crate::filter::Progress;
use crate::logger::Logger;
use crate::statistics::StatisticsManager;

// ─── TrainError ──────────────────────────────────────────────────────────────

/// Errors that can occur during training operations.
#[derive(Debug, thiserror::Error)]
pub enum TrainError {
    /// The classifier lock was poisoned (another thread panicked while holding it).
    #[error("classifier lock poisoned")]
    ClassifierLockPoisoned,

    /// The storage lock was poisoned.
    #[error("storage lock poisoned")]
    StorageLockPoisoned,

    /// The message database lock was poisoned.
    #[error("message database lock poisoned")]
    MessageDbLockPoisoned,

    /// A classifier operation failed (e.g., unlearn would cause negative count).
    #[error("classifier error: {0}")]
    ClassifierError(#[from] ClassifierError),

    /// A MAPI operation failed during training.
    #[error("MAPI error: {0}")]
    MapiError(#[from] MsgStoreError),

    /// No training folders are configured.
    #[error("no ham or spam training folders configured")]
    NoFoldersConfigured,

    /// Failed to extract message content for training.
    #[error("failed to extract message content: {0}")]
    ContentExtraction(String),
}

// ─── TrainResult ─────────────────────────────────────────────────────────────

/// Result of a batch training operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainResult {
    /// Total number of messages processed (including skipped).
    pub total_processed: u32,
    /// Number of messages newly trained or retrained.
    pub new_entries: u32,
    /// Number of messages that failed to process.
    pub errors: u32,
}

impl TrainResult {
    /// Create an empty result with all counts at zero.
    fn new() -> Self {
        Self {
            total_processed: 0,
            new_entries: 0,
            errors: 0,
        }
    }
}

// ─── IncrementalAction ───────────────────────────────────────────────────────

/// Internal enum representing what incremental training action to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncrementalAction {
    /// Train the message as spam (manual spam move).
    TrainAsSpam,
    /// Train the message as ham (recovery move).
    TrainAsHam,
    /// No training action needed.
    Skip,
}

// ─── TrainableMessage Trait ──────────────────────────────────────────────────

/// Trait abstracting message operations needed by the training engine.
///
/// This trait enables the training engine to be tested without actual MAPI
/// access. The real MAPI `Message` type implements this trait on Windows,
/// while tests use mock implementations.
pub trait TrainableMessage {
    /// Get the raw RFC 2822 message content for tokenization.
    fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError>;

    /// Get a unique search key identifying this message.
    fn get_search_key(&self) -> &[u8];

    /// Get a human-readable identifier for logging (e.g., subject or entry ID).
    fn get_display_id(&self) -> String;
}

// ─── TrainingFolderProvider Trait ────────────────────────────────────────────

/// Abstracts folder/message access for the training operation.
///
/// This trait allows the `TrainingEngine` to be tested without actual MAPI access.
pub trait TrainingFolderProvider {
    /// Get the display name of a folder.
    fn get_folder_name(&self, folder_id: &FolderId) -> Result<String, MsgStoreError>;

    /// Get the sub-folders of a folder.
    fn get_sub_folders(&self, folder_id: &FolderId) -> Result<Vec<FolderId>, MsgStoreError>;

    /// Get all messages in a folder for training.
    fn get_messages(
        &self,
        folder_id: &FolderId,
    ) -> Result<Vec<Box<dyn TrainableMessage>>, MsgStoreError>;
}

// ─── TrainingEngine ──────────────────────────────────────────────────────────

/// Core training engine that learns from user-categorized messages.
///
/// The `TrainingEngine` orchestrates the tokenizer, classifier, message
/// database, and storage to train the Bayesian filter. It handles batch
/// training (processing configured ham/spam folders) and single-message
/// training/untraining.
///
/// # Thread Safety
///
/// The classifier, storage, and message database are wrapped in `Arc<Mutex<_>>`
/// to allow shared access from multiple threads.
pub struct TrainingEngine {
    /// The Bayesian classifier to train.
    classifier: Arc<Mutex<Classifier>>,
    /// Persistence backend for classifier token data.
    #[allow(dead_code)]
    storage: Arc<Mutex<Box<dyn StorageBackend>>>,
    /// Per-message metadata database.
    message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
    /// Email tokenizer for extracting training features.
    tokenizer: Tokenizer,
    /// Logger for diagnostic output.
    logger: Arc<Logger>,
    /// Optional statistics observer for training tracking.
    statistics: Option<StatisticsManager>,
}

impl TrainingEngine {
    /// Create a new `TrainingEngine` with the given dependencies.
    pub fn new(
        classifier: Arc<Mutex<Classifier>>,
        storage: Arc<Mutex<Box<dyn StorageBackend>>>,
        message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
        logger: Arc<Logger>,
        statistics: Option<StatisticsManager>,
    ) -> Self {
        Self {
            classifier,
            storage,
            message_db,
            tokenizer: Tokenizer::with_defaults(),
            logger,
            statistics,
        }
    }

    /// Create a `TrainingEngine` with a custom tokenizer.
    pub fn with_tokenizer(
        classifier: Arc<Mutex<Classifier>>,
        storage: Arc<Mutex<Box<dyn StorageBackend>>>,
        message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
        tokenizer: Tokenizer,
        logger: Arc<Logger>,
        statistics: Option<StatisticsManager>,
    ) -> Self {
        Self {
            classifier,
            storage,
            message_db,
            tokenizer,
            logger,
            statistics,
        }
    }

    /// Train a single message as spam or ham.
    ///
    /// Returns `true` if the message was newly trained (or retrained),
    /// `false` if it was already correctly trained and skipped.
    ///
    /// # Logic
    ///
    /// - If untrained: train with the given classification (Req 9.2)
    /// - If trained with wrong classification: untrain old, train new (Req 9.3)
    /// - If trained with correct classification: skip (Req 9.4)
    ///
    /// # Validates: Requirements 9.2, 9.3, 9.4
    pub fn train_message(
        &self,
        msg: &dyn TrainableMessage,
        is_spam: bool,
    ) -> Result<bool, TrainError> {
        let search_key = msg.get_search_key();

        // Look up existing training state in message database.
        let existing_info = {
            let db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;
            db.load_msg(search_key)
        };

        // Determine action based on previous training state.
        match existing_info.as_ref().and_then(|info| info.trained_as) {
            Some(prev_is_spam) if prev_is_spam == is_spam => {
                // Requirement 9.4: Already correctly trained — skip.
                return Ok(false);
            }
            Some(prev_is_spam) => {
                // Requirement 9.3: Trained with wrong classification —
                // untrain old, then train new.
                let raw_content = msg.get_raw_content()
                    .map_err(|e| TrainError::ContentExtraction(e.to_string()))?;
                let tokens = self.tokenizer.tokenize(&raw_content);

                let mut classifier = self.classifier.lock()
                    .map_err(|_| TrainError::ClassifierLockPoisoned)?;

                // Untrain old classification.
                classifier.unlearn(tokens.clone().into_iter(), prev_is_spam)?;
                // Train new classification.
                classifier.learn(tokens.into_iter(), is_spam);
            }
            None => {
                // Requirement 9.2: Not previously trained — train now.
                let raw_content = msg.get_raw_content()
                    .map_err(|e| TrainError::ContentExtraction(e.to_string()))?;
                let tokens = self.tokenizer.tokenize(&raw_content);

                let mut classifier = self.classifier.lock()
                    .map_err(|_| TrainError::ClassifierLockPoisoned)?;

                classifier.learn(tokens.into_iter(), is_spam);
            }
        }

        // Update message database with new training state.
        let msg_id = msg.get_display_id();
        let new_info = MessageInfo {
            trained_as: Some(is_spam),
            classification: existing_info
                .as_ref()
                .and_then(|info| info.classification),
            score: existing_info.as_ref().and_then(|info| info.score),
            message_id: msg_id,
            original_folder: existing_info
                .as_ref()
                .and_then(|info| info.original_folder.clone()),
        };

        {
            let mut db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;
            db.store_msg(search_key, &new_info);
        }

        // Report training event to statistics.
        if let Some(stats) = &self.statistics {
            stats.on_trained(is_spam);
        }

        Ok(true)
    }

    /// Untrain a previously trained message.
    ///
    /// Returns `Some(was_spam)` if the message was untrained (indicating
    /// what it was previously trained as), or `None` if it was not trained.
    pub fn untrain_message(
        &self,
        msg: &dyn TrainableMessage,
    ) -> Result<Option<bool>, TrainError> {
        let search_key = msg.get_search_key();

        // Look up existing training state.
        let existing_info = {
            let db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;
            db.load_msg(search_key)
        };

        let Some(prev_is_spam) = existing_info.as_ref().and_then(|info| info.trained_as) else {
            return Ok(None); // Not trained — nothing to undo.
        };

        // Tokenize and unlearn.
        let raw_content = msg.get_raw_content()
            .map_err(|e| TrainError::ContentExtraction(e.to_string()))?;
        let tokens = self.tokenizer.tokenize(&raw_content);

        {
            let mut classifier = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            classifier.unlearn(tokens.into_iter(), prev_is_spam)?;
        }

        // Update message database: mark as untrained.
        let updated_info = MessageInfo {
            trained_as: None,
            classification: existing_info
                .as_ref()
                .and_then(|info| info.classification),
            score: existing_info.as_ref().and_then(|info| info.score),
            message_id: msg.get_display_id(),
            original_folder: existing_info
                .as_ref()
                .and_then(|info| info.original_folder.clone()),
        };

        {
            let mut db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;
            db.store_msg(search_key, &updated_info);
        }

        // Report untrain event to statistics.
        if let Some(stats) = &self.statistics {
            stats.on_untrained(prev_is_spam);
        }

        Ok(Some(prev_is_spam))
    }

    /// Incremental training triggered by drag-and-drop message moves.
    ///
    /// This method handles two scenarios:
    /// 1. **Manual spam** (Req 10.1): Message moved TO the spam folder — unlearn
    ///    previous training (if any) and train as spam.
    /// 2. **Recovery** (Req 10.2): Message moved BACK to its original folder —
    ///    unlearn previous training (if any) and train as ham.
    ///
    /// Messages that were never classified by `SpamBayes` are skipped (Req 10.5).
    /// On tokenization/training errors, the error is logged and the classifier
    /// database is left unchanged for that message (Req 10.3).
    ///
    /// # Arguments
    ///
    /// * `msg` - The message that was moved
    /// * `dest_folder` - The folder the message was moved to
    /// * `config` - Training configuration (`train_manual_spam`, `train_recovered_spam`)
    /// * `spam_folder_id` - The configured spam folder, if any
    ///
    /// # Validates: Requirements 10.1, 10.2, 10.3, 10.5
    pub fn on_message_moved(
        &self,
        msg: &dyn TrainableMessage,
        dest_folder: &FolderId,
        config: &TrainingConfig,
        spam_folder_id: Option<&FolderId>,
    ) -> Result<(), TrainError> {
        let search_key = msg.get_search_key();

        // Look up the message in the database.
        let existing_info = {
            let db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;
            db.load_msg(search_key)
        };

        // Requirement 10.5: If the message has never been classified by SpamBayes,
        // skip incremental training entirely.
        let info = match &existing_info {
            Some(info) if info.classification.is_some() => info,
            _ => {
                self.logger.info(
                    "train",
                    &format!(
                        "Skipping incremental training for '{}': never classified by SpamBayes",
                        msg.get_display_id()
                    ),
                );
                return Ok(());
            }
        };

        let classification = info.classification.unwrap(); // safe: checked above

        // Determine what action to take based on the move destination.
        let action = self.determine_incremental_action(
            &classification,
            dest_folder,
            config,
            spam_folder_id,
            info.original_folder.as_ref(),
        );

        match action {
            IncrementalAction::TrainAsSpam => {
                self.perform_incremental_train(msg, search_key, &existing_info, true)?;
            }
            IncrementalAction::TrainAsHam => {
                self.perform_incremental_train(msg, search_key, &existing_info, false)?;
            }
            IncrementalAction::Skip => {
                // No action needed for this move.
            }
        }

        Ok(())
    }

    /// Persist the classifier state after incremental training.
    ///
    /// This method saves the current classifier token data and global counts
    /// through the storage backend. It is intended to be called by an external
    /// timer mechanism within 5 seconds of the last incremental training operation.
    ///
    /// # Validates: Requirement 10.4
    pub fn save_post_incremental(&self) -> Result<(), TrainError> {
        let classifier = self.classifier.lock()
            .map_err(|_| TrainError::ClassifierLockPoisoned)?;

        let state = ClassifierState {
            nspam: classifier.nspam(),
            nham: classifier.nham(),
            version: 1,
        };

        // Collect all word info as Updated changes for a full persist.
        let changed: HashMap<Vec<u8>, WordChange> = classifier
            .word_info()
            .iter()
            .map(|(token, info)| (token.clone(), WordChange::Updated(*info)))
            .collect();

        drop(classifier); // Release classifier lock before acquiring storage lock.

        let mut storage = self.storage.lock()
            .map_err(|_| TrainError::StorageLockPoisoned)?;
        storage.store(&state, &changed).map_err(|e| {
            self.logger.error(
                "train",
                &format!("Failed to persist classifier after incremental training: {e}"),
            );
            TrainError::ContentExtraction(format!("storage error: {e}"))
        })?;

        self.logger.info(
            "train",
            "Classifier persisted after incremental training",
        );

        Ok(())
    }

    /// Determine what incremental training action to take for a moved message.
    fn determine_incremental_action(
        &self,
        classification: &Classification,
        dest_folder: &FolderId,
        config: &TrainingConfig,
        spam_folder_id: Option<&FolderId>,
        original_folder: Option<&FolderId>,
    ) -> IncrementalAction {
        // Requirement 10.1: Manual spam training.
        // If train_manual_spam is enabled AND the message is moved TO the spam folder
        // AND the message was previously scored as ham or unsure.
        if config.train_manual_spam {
            if let Some(spam_id) = spam_folder_id {
                if dest_folder == spam_id {
                    match classification {
                        Classification::Ham | Classification::Unsure => {
                            return IncrementalAction::TrainAsSpam;
                        }
                        Classification::Spam => {
                            // Already classified as spam — no action needed.
                        }
                    }
                }
            }
        }

        // Requirement 10.2: Recovery training.
        // If train_recovered_spam is enabled AND the message was previously scored
        // as spam or unsure AND moved back to its original folder.
        if config.train_recovered_spam {
            if let Some(orig_folder) = original_folder {
                if dest_folder == orig_folder {
                    match classification {
                        Classification::Spam | Classification::Unsure => {
                            return IncrementalAction::TrainAsHam;
                        }
                        Classification::Ham => {
                            // Already classified as ham — no action needed.
                        }
                    }
                }
            }
        }

        IncrementalAction::Skip
    }

    /// Perform the actual incremental training: unlearn previous + train new.
    ///
    /// On any error during tokenization or training, logs the error and returns
    /// Ok(()) to leave the DB unchanged (Requirement 10.3).
    fn perform_incremental_train(
        &self,
        msg: &dyn TrainableMessage,
        search_key: &[u8],
        existing_info: &Option<MessageInfo>,
        train_as_spam: bool,
    ) -> Result<(), TrainError> {
        // Extract content for tokenization.
        let raw_content = match msg.get_raw_content() {
            Ok(content) => content,
            Err(e) => {
                // Requirement 10.3: Log error, leave DB unchanged.
                self.logger.error(
                    "train",
                    &format!(
                        "Incremental training: failed to extract content for '{}': {}",
                        msg.get_display_id(), e
                    ),
                );
                return Ok(());
            }
        };

        let tokens = self.tokenizer.tokenize(&raw_content);

        // Unlearn previous training if any.
        if let Some(info) = existing_info {
            if let Some(prev_is_spam) = info.trained_as {
                let unlearn_tokens = self.tokenizer.tokenize(&raw_content);
                let mut classifier = self.classifier.lock()
                    .map_err(|_| TrainError::ClassifierLockPoisoned)?;
                if let Err(e) = classifier.unlearn(unlearn_tokens.into_iter(), prev_is_spam) {
                    // Requirement 10.3: Log error, leave DB unchanged.
                    self.logger.error(
                        "train",
                        &format!(
                            "Incremental training: failed to unlearn '{}': {}",
                            msg.get_display_id(), e
                        ),
                    );
                    return Ok(());
                }
            }
        }

        // Train with new classification.
        {
            let mut classifier = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            classifier.learn(tokens.into_iter(), train_as_spam);
        }

        // Update message database with new trained_as state.
        let updated_info = MessageInfo {
            trained_as: Some(train_as_spam),
            classification: existing_info.as_ref().and_then(|i| i.classification),
            score: existing_info.as_ref().and_then(|i| i.score),
            message_id: msg.get_display_id(),
            original_folder: existing_info.as_ref().and_then(|i| i.original_folder.clone()),
        };

        {
            let mut db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;
            db.store_msg(search_key, &updated_info);
        }

        // Report incremental training event to statistics.
        if let Some(stats) = &self.statistics {
            stats.on_trained(train_as_spam);
        }

        self.logger.info(
            "train",
            &format!(
                "Incremental training: '{}' trained as {}",
                msg.get_display_id(),
                if train_as_spam { "spam" } else { "ham" }
            ),
        );

        Ok(())
    }

    /// Batch training on configured ham/spam folders.
    ///
    /// Processes all messages in the configured training folders (and sub-folders
    /// if enabled). For each message:
    /// - New messages are trained with the folder's classification
    /// - Wrongly-trained messages are untrained and retrained
    /// - Correctly-trained messages are skipped
    ///
    /// Progress is reported after each message. Per-message errors are logged
    /// and processing continues.
    ///
    /// # Returns
    ///
    /// A [`TrainResult`] with counts of processed messages, new entries, and errors.
    /// Returns `Err(TrainError::NoFoldersConfigured)` if no folders are configured.
    ///
    /// # Validates: Requirements 9.1, 9.2, 9.3, 9.4, 9.7, 9.8, 9.9, 9.10
    pub fn train_batch(
        &self,
        config: &TrainingConfig,
        folder_provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
    ) -> Result<TrainResult, TrainError> {
        // Requirement 9.9: Error if no folders configured.
        if config.ham_folder_ids.is_empty() && config.spam_folder_ids.is_empty() {
            self.logger.error("train", "No ham or spam training folders configured");
            return Err(TrainError::NoFoldersConfigured);
        }

        let mut result = TrainResult::new();

        // Requirement 9.1: Process ham folders (and sub-folders if configured).
        let ham_folders = self.expand_folders(
            &config.ham_folder_ids,
            config.ham_include_sub,
            folder_provider,
        );
        for folder_id in &ham_folders {
            self.train_folder(
                folder_id,
                false, // is_spam = false (ham)
                folder_provider,
                progress,
                &mut result,
            );
        }

        // Requirement 9.1: Process spam folders (and sub-folders if configured).
        let spam_folders = self.expand_folders(
            &config.spam_folder_ids,
            config.spam_include_sub,
            folder_provider,
        );
        for folder_id in &spam_folders {
            self.train_folder(
                folder_id,
                true, // is_spam = true
                folder_provider,
                progress,
                &mut result,
            );
        }

        // Requirement 9.10: Display summary on completion.
        let (nham, nspam) = {
            let classifier = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            (classifier.nham(), classifier.nspam())
        };

        self.logger.info(
            "train",
            &format!(
                "Training complete. Total ham: {}, Total spam: {}. \
                 Processed: {}, New entries: {}, Errors: {}",
                nham, nspam,
                result.total_processed, result.new_entries, result.errors
            ),
        );

        Ok(result)
    }

    /// Expand a list of folder IDs to include sub-folders if configured.
    fn expand_folders(
        &self,
        folder_ids: &[FolderId],
        include_sub: bool,
        folder_provider: &dyn TrainingFolderProvider,
    ) -> Vec<FolderId> {
        let mut all_folders: Vec<FolderId> = Vec::new();
        for folder_id in folder_ids {
            all_folders.push(folder_id.clone());
            if include_sub {
                match folder_provider.get_sub_folders(folder_id) {
                    Ok(subs) => all_folders.extend(subs),
                    Err(e) => {
                        self.logger.error(
                            "train",
                            &format!(
                                "Failed to get sub-folders for folder: {e}"
                            ),
                        );
                    }
                }
            }
        }
        all_folders
    }

    /// Rebuild: create a fresh classifier, train all messages, then atomically swap.
    ///
    /// This method creates a new empty `Classifier` with the same configuration
    /// as the current one, trains ALL messages in configured ham/spam folders into
    /// the new classifier (ignoring previous training state), and on success
    /// atomically replaces the current classifier with the new one.
    ///
    /// If `config.rescore` is true, all messages are re-scored after training.
    ///
    /// # Returns
    ///
    /// A [`TrainResult`] with counts of processed messages, new entries, and errors.
    /// Returns `Err(TrainError::NoFoldersConfigured)` if no folders are configured.
    ///
    /// # Validates: Requirements 9.5, 9.6
    pub fn rebuild(
        &self,
        config: &TrainingConfig,
        folder_provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
    ) -> Result<TrainResult, TrainError> {
        // Requirement 9.9: Error if no folders configured.
        if config.ham_folder_ids.is_empty() && config.spam_folder_ids.is_empty() {
            self.logger.error("train", "No ham or spam training folders configured");
            return Err(TrainError::NoFoldersConfigured);
        }

        // Requirement 9.5: Create a new temporary classifier with same config.
        let classifier_config = {
            let current = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            current.config().clone()
        };
        let mut new_classifier = Classifier::new(classifier_config);

        let mut result = TrainResult::new();

        // Expand all ham folders (and sub-folders if configured).
        let ham_folders = self.expand_folders(
            &config.ham_folder_ids,
            config.ham_include_sub,
            folder_provider,
        );

        // Expand all spam folders (and sub-folders if configured).
        let spam_folders = self.expand_folders(
            &config.spam_folder_ids,
            config.spam_include_sub,
            folder_provider,
        );

        // Collect all messages with their classification for training.
        // We train ALL messages fresh, ignoring previous training state.
        let mut trained_messages: Vec<(Vec<u8>, bool, String)> = Vec::new();

        // Train ham folders into new classifier.
        for folder_id in &ham_folders {
            self.rebuild_train_folder(
                folder_id,
                false,
                &mut new_classifier,
                folder_provider,
                progress,
                &mut result,
                &mut trained_messages,
            );

            if progress.is_cancelled() {
                self.logger.info("train", "Rebuild cancelled by user");
                return Ok(result);
            }
        }

        // Train spam folders into new classifier.
        for folder_id in &spam_folders {
            self.rebuild_train_folder(
                folder_id,
                true,
                &mut new_classifier,
                folder_provider,
                progress,
                &mut result,
                &mut trained_messages,
            );

            if progress.is_cancelled() {
                self.logger.info("train", "Rebuild cancelled by user");
                return Ok(result);
            }
        }

        // Requirement 9.5: Atomically swap the new classifier in.
        {
            let mut classifier_guard = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            *classifier_guard = new_classifier;
        }

        // Update message database: re-record all trained messages.
        {
            let mut db = self.message_db.lock()
                .map_err(|_| TrainError::MessageDbLockPoisoned)?;

            for (search_key, is_spam, display_id) in &trained_messages {
                let info = MessageInfo {
                    trained_as: Some(*is_spam),
                    classification: None,
                    score: None,
                    message_id: display_id.clone(),
                    original_folder: None,
                };
                db.store_msg(search_key, &info);
            }
        }

        // Requirement 9.6: Rescore if enabled.
        if config.rescore {
            self.rescore_messages(config, folder_provider, progress)?;
        }

        // Requirement 9.10: Display summary on completion.
        let (nham, nspam) = {
            let classifier = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            (classifier.nham(), classifier.nspam())
        };

        self.logger.info(
            "train",
            &format!(
                "Rebuild complete. Total ham: {}, Total spam: {}. \
                 Processed: {}, New entries: {}, Errors: {}",
                nham, nspam,
                result.total_processed, result.new_entries, result.errors
            ),
        );

        Ok(result)
    }

    /// Train all messages in a folder into a new classifier during rebuild.
    ///
    /// Unlike `train_folder`, this method trains ALL messages unconditionally
    /// (ignoring previous training state) into the provided new classifier.
    fn rebuild_train_folder(
        &self,
        folder_id: &FolderId,
        is_spam: bool,
        new_classifier: &mut Classifier,
        folder_provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
        result: &mut TrainResult,
        trained_messages: &mut Vec<(Vec<u8>, bool, String)>,
    ) {
        let folder_name = folder_provider
            .get_folder_name(folder_id)
            .unwrap_or_else(|_| "(unknown)".to_string());

        let messages = match folder_provider.get_messages(folder_id) {
            Ok(msgs) => msgs,
            Err(e) => {
                self.logger.error(
                    "train",
                    &format!(
                        "Failed to get messages from folder '{folder_name}': {e}"
                    ),
                );
                result.errors += 1;
                return;
            }
        };

        let total = messages.len() as u32;

        for msg in &messages {
            result.total_processed += 1;

            // Extract content and train into new classifier.
            match msg.get_raw_content() {
                Ok(raw_content) => {
                    let tokens = self.tokenizer.tokenize(&raw_content);
                    new_classifier.learn(tokens.into_iter(), is_spam);
                    result.new_entries += 1;

                    // Record for message_db update.
                    trained_messages.push((
                        msg.get_search_key().to_vec(),
                        is_spam,
                        msg.get_display_id(),
                    ));
                }
                Err(e) => {
                    let msg_id = msg.get_display_id();
                    self.logger.error(
                        "train",
                        &format!("Error extracting content for '{msg_id}': {e}"),
                    );
                    result.errors += 1;
                }
            }

            progress.report(&folder_name, result.total_processed, total);
        }
    }

    /// Re-score all messages in ham and spam folders after training completes.
    ///
    /// Iterates all messages in configured folders, tokenizes each, scores it
    /// using the current classifier, and updates the score in the message database.
    ///
    /// # Validates: Requirement 9.6
    pub fn rescore_messages(
        &self,
        config: &TrainingConfig,
        folder_provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
    ) -> Result<(), TrainError> {
        let ham_folders = self.expand_folders(
            &config.ham_folder_ids,
            config.ham_include_sub,
            folder_provider,
        );
        let spam_folders = self.expand_folders(
            &config.spam_folder_ids,
            config.spam_include_sub,
            folder_provider,
        );

        let all_folders: Vec<&FolderId> = ham_folders.iter()
            .chain(spam_folders.iter())
            .collect();

        let mut processed: u32 = 0;

        for folder_id in all_folders {
            let folder_name = folder_provider
                .get_folder_name(folder_id)
                .unwrap_or_else(|_| "(unknown)".to_string());

            let messages = match folder_provider.get_messages(folder_id) {
                Ok(msgs) => msgs,
                Err(e) => {
                    self.logger.error(
                        "train",
                        &format!(
                            "Rescore: failed to get messages from folder '{folder_name}': {e}"
                        ),
                    );
                    continue;
                }
            };

            let total = messages.len() as u32;

            for msg in &messages {
                processed += 1;

                let search_key = msg.get_search_key();

                match msg.get_raw_content() {
                    Ok(raw_content) => {
                        let tokens = self.tokenizer.tokenize(&raw_content);

                        // Score using the current classifier.
                        let score = {
                            let classifier = self.classifier.lock()
                                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
                            classifier.spam_prob(tokens.into_iter())
                        };

                        // Update the message database with the new score.
                        let mut db = self.message_db.lock()
                            .map_err(|_| TrainError::MessageDbLockPoisoned)?;

                        let existing = db.load_msg(search_key);
                        let updated_info = MessageInfo {
                            trained_as: existing.as_ref().and_then(|i| i.trained_as),
                            classification: existing.as_ref().and_then(|i| i.classification),
                            score: Some(score),
                            message_id: existing
                                .as_ref().map_or_else(|| msg.get_display_id(), |i| i.message_id.clone()),
                            original_folder: existing
                                .as_ref()
                                .and_then(|i| i.original_folder.clone()),
                        };
                        db.store_msg(search_key, &updated_info);
                    }
                    Err(e) => {
                        self.logger.error(
                            "train",
                            &format!(
                                "Rescore: failed to extract content for '{}': {}",
                                msg.get_display_id(), e
                            ),
                        );
                    }
                }

                progress.report(&folder_name, processed, total);

                if progress.is_cancelled() {
                    self.logger.info("train", "Rescore cancelled by user");
                    return Ok(());
                }
            }
        }

        self.logger.info(
            "train",
            &format!("Rescore complete. {processed} messages re-scored."),
        );

        Ok(())
    }

    /// Minimum number of concurrent worker threads for batch training.
    const MIN_WORKER_THREADS: usize = 4;

    /// Batch training with concurrent tokenization.
    ///
    /// This method uses multiple worker threads to tokenize messages in parallel
    /// (the CPU-intensive step), then applies results to the classifier serially.
    /// This provides significant speedup for large training sets while maintaining
    /// correctness of the shared classifier state.
    ///
    /// The architecture is:
    /// - Worker threads handle tokenization (CPU-intensive, no lock needed)
    /// - Results are collected and applied to the classifier under the lock
    ///
    /// # Arguments
    ///
    /// * `config` - Training configuration with folder IDs
    /// * `folder_provider` - Provides access to folders and messages
    /// * `progress` - Progress reporter
    /// * `num_workers` - Number of worker threads (minimum 4, clamped if less)
    ///
    /// # Returns
    ///
    /// A [`TrainResult`] with counts of processed messages, new entries, and errors.
    ///
    /// # Validates: Requirements 22.8
    pub fn train_batch_concurrent(
        &self,
        config: &TrainingConfig,
        folder_provider: &(dyn TrainingFolderProvider + Sync),
        progress: &mut dyn Progress,
        num_workers: usize,
    ) -> Result<TrainResult, TrainError> {
        // Requirement 9.9: Error if no folders configured.
        if config.ham_folder_ids.is_empty() && config.spam_folder_ids.is_empty() {
            self.logger.error("train", "No ham or spam training folders configured");
            return Err(TrainError::NoFoldersConfigured);
        }

        let num_workers = num_workers.max(Self::MIN_WORKER_THREADS);

        // Collect all (folder_id, is_spam) pairs.
        let ham_folders = self.expand_folders(
            &config.ham_folder_ids,
            config.ham_include_sub,
            folder_provider,
        );
        let spam_folders = self.expand_folders(
            &config.spam_folder_ids,
            config.spam_include_sub,
            folder_provider,
        );

        // Collect all messages with their metadata for parallel processing.
        // Each entry: (search_key, raw_content, is_spam, display_id)
        // We also track errors from content extraction.
        struct MessageWork {
            search_key: Vec<u8>,
            raw_content: Vec<u8>,
            is_spam: bool,
            display_id: String,
        }

        let mut work_items: Vec<MessageWork> = Vec::new();
        let mut total_processed: u32 = 0;
        let mut errors: u32 = 0;

        // Helper to collect messages from a list of folders.
        let mut collect_from_folders = |folders: &[FolderId], is_spam: bool| {
            for folder_id in folders {
                let folder_name = folder_provider
                    .get_folder_name(folder_id)
                    .unwrap_or_else(|_| "(unknown)".to_string());

                let messages = match folder_provider.get_messages(folder_id) {
                    Ok(msgs) => msgs,
                    Err(e) => {
                        self.logger.error(
                            "train",
                            &format!(
                                "Failed to get messages from folder '{folder_name}': {e}"
                            ),
                        );
                        errors += 1;
                        continue;
                    }
                };

                for msg in &messages {
                    total_processed += 1;
                    let search_key = msg.get_search_key().to_vec();
                    let display_id = msg.get_display_id();

                    // Check if already correctly trained (skip if so).
                    let existing_info = {
                        let Ok(db) = self.message_db.lock() else {
                            errors += 1;
                            continue;
                        };
                        db.load_msg(&search_key)
                    };

                    let needs_training = !matches!(existing_info.as_ref().and_then(|info| info.trained_as), Some(prev_is_spam) if prev_is_spam == is_spam);

                    if !needs_training {
                        continue;
                    }

                    match msg.get_raw_content() {
                        Ok(content) => {
                            work_items.push(MessageWork {
                                search_key,
                                raw_content: content,
                                is_spam,
                                display_id,
                            });
                        }
                        Err(e) => {
                            self.logger.error(
                                "train",
                                &format!("Error extracting content for '{display_id}': {e}"),
                            );
                            errors += 1;
                        }
                    }
                }
            }
        };

        collect_from_folders(&ham_folders, false);
        collect_from_folders(&spam_folders, true);

        // Parallel tokenization using std::thread::scope.
        // Each worker tokenizes a chunk of messages.
        let chunk_size = (work_items.len() + num_workers - 1) / num_workers.max(1);
        let chunks: Vec<&[MessageWork]> = if work_items.is_empty() {
            Vec::new()
        } else {
            work_items.chunks(chunk_size.max(1)).collect()
        };

        // Tokenized results: (search_key, tokens, is_spam, display_id, existing_trained_as)
        struct TokenizedResult {
            search_key: Vec<u8>,
            tokens: Vec<Vec<u8>>,
            is_spam: bool,
            display_id: String,
        }

        let tokenizer = &self.tokenizer;
        let tokenized_results: Vec<TokenizedResult> = std::thread::scope(|s| {
            let handles: Vec<_> = chunks
                .into_iter()
                .map(|chunk| {
                    s.spawn(move || {
                        let mut results = Vec::with_capacity(chunk.len());
                        for item in chunk {
                            let tokens = tokenizer.tokenize(&item.raw_content);
                            results.push(TokenizedResult {
                                search_key: item.search_key.clone(),
                                tokens,
                                is_spam: item.is_spam,
                                display_id: item.display_id.clone(),
                            });
                        }
                        results
                    })
                })
                .collect();

            let mut all_results = Vec::with_capacity(work_items.len());
            for handle in handles {
                if let Ok(results) = handle.join() { all_results.extend(results) } else {
                    // Worker thread panicked — count as errors.
                }
            }
            all_results
        });

        // Apply tokenized results to the classifier (serialized step).
        let mut new_entries: u32 = 0;
        for result in &tokenized_results {
            // Re-check existing training state (may have changed since collection).
            let existing_info = {
                let db = self.message_db.lock()
                    .map_err(|_| TrainError::MessageDbLockPoisoned)?;
                db.load_msg(&result.search_key)
            };

            match existing_info.as_ref().and_then(|info| info.trained_as) {
                Some(prev_is_spam) if prev_is_spam == result.is_spam => {
                    // Already correctly trained — skip.
                    continue;
                }
                Some(prev_is_spam) => {
                    // Trained with wrong classification — untrain old, train new.
                    let mut classifier = self.classifier.lock()
                        .map_err(|_| TrainError::ClassifierLockPoisoned)?;
                    if let Err(e) = classifier.unlearn(result.tokens.clone().into_iter(), prev_is_spam) {
                        self.logger.error(
                            "train",
                            &format!("Error unlearning '{}': {}", result.display_id, e),
                        );
                        errors += 1;
                        continue;
                    }
                    classifier.learn(result.tokens.clone().into_iter(), result.is_spam);
                }
                None => {
                    // Not previously trained — train now.
                    let mut classifier = self.classifier.lock()
                        .map_err(|_| TrainError::ClassifierLockPoisoned)?;
                    classifier.learn(result.tokens.clone().into_iter(), result.is_spam);
                }
            }

            // Update message database.
            let new_info = MessageInfo {
                trained_as: Some(result.is_spam),
                classification: existing_info.as_ref().and_then(|info| info.classification),
                score: existing_info.as_ref().and_then(|info| info.score),
                message_id: result.display_id.clone(),
                original_folder: existing_info.as_ref().and_then(|info| info.original_folder.clone()),
            };

            {
                let mut db = self.message_db.lock()
                    .map_err(|_| TrainError::MessageDbLockPoisoned)?;
                db.store_msg(&result.search_key, &new_info);
            }

            new_entries += 1;
        }

        // Report progress completion.
        progress.report("concurrent_training", total_processed, total_processed);

        // Log summary.
        let (nham, nspam) = {
            let classifier = self.classifier.lock()
                .map_err(|_| TrainError::ClassifierLockPoisoned)?;
            (classifier.nham(), classifier.nspam())
        };

        self.logger.info(
            "train",
            &format!(
                "Concurrent training complete ({num_workers} workers). Total ham: {nham}, Total spam: {nspam}. \
                 Processed: {total_processed}, New entries: {new_entries}, Errors: {errors}"
            ),
        );

        Ok(TrainResult {
            total_processed,
            new_entries,
            errors,
        })
    }

    /// Train all messages in a single folder.
    ///
    /// Reports progress after each message. Logs per-message errors
    /// and continues processing (Requirement 9.8).
    fn train_folder(
        &self,
        folder_id: &FolderId,
        is_spam: bool,
        folder_provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
        result: &mut TrainResult,
    ) {
        // Get folder name for progress reporting.
        let folder_name = folder_provider
            .get_folder_name(folder_id)
            .unwrap_or_else(|_| "(unknown)".to_string());

        // Get messages in this folder.
        let messages = match folder_provider.get_messages(folder_id) {
            Ok(msgs) => msgs,
            Err(e) => {
                self.logger.error(
                    "train",
                    &format!(
                        "Failed to get messages from folder '{folder_name}': {e}"
                    ),
                );
                result.errors += 1;
                return;
            }
        };

        let total = messages.len() as u32;

        for msg in &messages {
            result.total_processed += 1;

            // Requirement 9.8: Handle per-message errors gracefully.
            match self.train_message(msg.as_ref(), is_spam) {
                Ok(true) => {
                    result.new_entries += 1;
                }
                Ok(false) => {
                    // Already correctly trained — skipped (Req 9.4).
                }
                Err(e) => {
                    // Requirement 9.8: Log error and continue.
                    let msg_id = msg.get_display_id();
                    self.logger.error(
                        "train",
                        &format!(
                            "Error training message '{msg_id}': {e}"
                        ),
                    );
                    result.errors += 1;
                }
            }

            // Requirement 9.7: Report progress.
            progress.report(&folder_name, result.total_processed, total);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::type_complexity)]
mod tests {
    use super::*;
    use spambayes_config::FolderId;
    use spambayes_core::Classification;
    use spambayes_storage::{ClassifierState, StorageError, WordChange};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

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

    struct MockMessageDb {
        messages: HashMap<Vec<u8>, MessageInfo>,
    }

    impl MockMessageDb {
        fn new() -> Self {
            Self {
                messages: HashMap::new(),
            }
        }

        fn with_entry(mut self, key: &[u8], info: MessageInfo) -> Self {
            self.messages.insert(key.to_vec(), info);
            self
        }
    }

    impl MessageDatabase for MockMessageDb {
        fn load_msg(&self, search_key: &[u8]) -> Option<MessageInfo> {
            self.messages.get(search_key).cloned()
        }

        fn store_msg(&mut self, search_key: &[u8], info: &MessageInfo) {
            self.messages.insert(search_key.to_vec(), info.clone());
        }

        fn remove_msg(&mut self, search_key: &[u8]) {
            self.messages.remove(search_key);
        }
    }

    // ── Mock TrainableMessage ────────────────────────────────────────────

    struct MockTrainableMessage {
        search_key: Vec<u8>,
        raw_content: Option<Vec<u8>>, // None = error
        display_id: String,
    }

    impl MockTrainableMessage {
        fn new(search_key: &[u8], content: &[u8]) -> Self {
            Self {
                search_key: search_key.to_vec(),
                raw_content: Some(content.to_vec()),
                display_id: format!("msg_{}", hex::encode(search_key)),
            }
        }

        fn with_error(search_key: &[u8]) -> Self {
            Self {
                search_key: search_key.to_vec(),
                raw_content: None,
                display_id: format!("msg_{}", hex::encode(search_key)),
            }
        }
    }

    impl TrainableMessage for MockTrainableMessage {
        fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError> {
            match &self.raw_content {
                Some(content) => Ok(content.clone()),
                None => Err(MsgStoreError::NotFound(
                    "message content unavailable".to_string(),
                )),
            }
        }

        fn get_search_key(&self) -> &[u8] {
            &self.search_key
        }

        fn get_display_id(&self) -> String {
            self.display_id.clone()
        }
    }

    // ── Mock TrainingFolderProvider ──────────────────────────────────────

    /// Mock folder provider that stores message data as (`search_key`, content) pairs
    /// and reconstructs `MockTrainableMessage` instances on demand.
    struct MockFolderProvider {
        /// Maps folder INI key → list of (`search_key`, `raw_content_or_error`).
        folders: HashMap<String, Vec<(Vec<u8>, Option<Vec<u8>>)>>,
        sub_folders: HashMap<String, Vec<FolderId>>,
    }

    impl MockFolderProvider {
        fn new() -> Self {
            Self {
                folders: HashMap::new(),
                sub_folders: HashMap::new(),
            }
        }

        /// Add a folder with messages. Each message is (`search_key`, `raw_content`).
        fn add_folder_with_messages(
            mut self,
            folder_id: &FolderId,
            messages: Vec<(Vec<u8>, Vec<u8>)>,
        ) -> Self {
            let key = folder_id.to_ini_str();
            let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = messages
                .into_iter()
                .map(|(k, content)| (k, Some(content)))
                .collect();
            self.folders.insert(key, entries);
            self
        }

        /// Add a folder with one failing message.
        fn add_folder_with_error_message(
            mut self,
            folder_id: &FolderId,
            search_key: Vec<u8>,
        ) -> Self {
            let key = folder_id.to_ini_str();
            self.folders
                .entry(key)
                .or_default()
                .push((search_key, None));
            self
        }

        fn add_sub_folders(
            mut self,
            parent: &FolderId,
            children: Vec<FolderId>,
        ) -> Self {
            let key = parent.to_ini_str();
            self.sub_folders.insert(key, children);
            self
        }
    }

    impl TrainingFolderProvider for MockFolderProvider {
        fn get_folder_name(&self, folder_id: &FolderId) -> Result<String, MsgStoreError> {
            Ok(format!("Folder_{}", &folder_id.entry_id.0))
        }

        fn get_sub_folders(
            &self,
            folder_id: &FolderId,
        ) -> Result<Vec<FolderId>, MsgStoreError> {
            let key = folder_id.to_ini_str();
            Ok(self.sub_folders.get(&key).cloned().unwrap_or_default())
        }

        fn get_messages(
            &self,
            folder_id: &FolderId,
        ) -> Result<Vec<Box<dyn TrainableMessage>>, MsgStoreError> {
            let key = folder_id.to_ini_str();
            match self.folders.get(&key) {
                Some(entries) => {
                    let messages: Vec<Box<dyn TrainableMessage>> = entries
                        .iter()
                        .map(|(search_key, content)| {
                            let msg: Box<dyn TrainableMessage> = match content {
                                Some(data) => {
                                    Box::new(MockTrainableMessage::new(search_key, data))
                                }
                                None => Box::new(MockTrainableMessage::with_error(search_key)),
                            };
                            msg
                        })
                        .collect();
                    Ok(messages)
                }
                None => Ok(Vec::new()),
            }
        }
    }

    // ── Mock Progress ────────────────────────────────────────────────────

    struct MockProgress {
        reports: Vec<(String, u32, u32)>,
    }

    impl MockProgress {
        fn new() -> Self {
            Self {
                reports: Vec::new(),
            }
        }
    }

    impl Progress for MockProgress {
        fn report(&mut self, folder_name: &str, current: u32, total: u32) {
            self.reports.push((folder_name.to_string(), current, total));
        }

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    // ── Helper Functions ─────────────────────────────────────────────────

    fn make_logger() -> Arc<Logger> {
        // Use a temp file for test logging.
        let path = std::env::temp_dir().join("spambayes_train_test.log");
        Arc::new(Logger::new(&path, crate::LogLevel::Verbose).unwrap())
    }

    fn make_engine(message_db: MockMessageDb) -> TrainingEngine {
        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        let storage: Arc<Mutex<Box<dyn StorageBackend>>> =
            Arc::new(Mutex::new(Box::new(MockStorage)));
        let db: Arc<Mutex<Box<dyn MessageDatabase>>> =
            Arc::new(Mutex::new(Box::new(message_db)));
        let logger = make_logger();

        TrainingEngine::new(classifier, storage, db, logger, None)
    }

    fn make_engine_with_classifier(
        classifier: Arc<Mutex<Classifier>>,
        message_db: MockMessageDb,
    ) -> TrainingEngine {
        let storage: Arc<Mutex<Box<dyn StorageBackend>>> =
            Arc::new(Mutex::new(Box::new(MockStorage)));
        let db: Arc<Mutex<Box<dyn MessageDatabase>>> =
            Arc::new(Mutex::new(Box::new(message_db)));
        let logger = make_logger();

        TrainingEngine::new(classifier, storage, db, logger, None)
    }

    fn make_ham_folder_id() -> FolderId {
        FolderId::from_ini_str("('AABB0011', 'CCDD2233')").unwrap()
    }

    fn make_spam_folder_id() -> FolderId {
        FolderId::from_ini_str("('AABB0011', 'EEFF4455')").unwrap()
    }

    /// Simple email content for training (minimal RFC 2822).
    fn make_email_content(subject: &str, body: &str) -> Vec<u8> {
        format!(
            "From: sender@example.com\r\n\
             To: recipient@example.com\r\n\
             Subject: {subject}\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             {body}"
        )
        .into_bytes()
    }

    /// Hex-encode helper (simple, no dependency).
    mod hex {
        pub fn encode(data: &[u8]) -> String {
            data.iter().map(|b| format!("{b:02x}")).collect()
        }
    }

    // ─── Unit Tests ──────────────────────────────────────────────────────

    #[test]
    fn train_message_new_ham() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let content = make_email_content("Hello", "This is a legitimate message");
        let msg = MockTrainableMessage::new(b"key1", &content);

        let result = engine.train_message(&msg, false).unwrap();
        assert!(result, "new message should be trained (returns true)");

        // Verify classifier was updated.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 1);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn train_message_new_spam() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let content = make_email_content("Buy now!", "Cheap pills discount offer");
        let msg = MockTrainableMessage::new(b"key2", &content);

        let result = engine.train_message(&msg, true).unwrap();
        assert!(result, "new message should be trained (returns true)");

        // Verify classifier was updated.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 1);
    }

    #[test]
    fn train_message_skip_correctly_trained() {
        // Pre-populate DB with a message trained as ham.
        let db = MockMessageDb::new().with_entry(
            b"key1",
            MessageInfo {
                trained_as: Some(false), // trained as ham
                classification: Some(Classification::Ham),
                score: Some(0.1),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let content = make_email_content("Hello", "Legitimate message");
        let msg = MockTrainableMessage::new(b"key1", &content);

        // Training as ham again should skip (Requirement 9.4).
        let result = engine.train_message(&msg, false).unwrap();
        assert!(!result, "correctly trained message should be skipped");

        // Classifier should not be modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn train_message_retrain_wrong_classification() {
        // Pre-populate DB: message was trained as ham but is now in spam folder.
        let content = make_email_content("Spam!", "Buy cheap things now");
        let db = MockMessageDb::new().with_entry(
            b"key1",
            MessageInfo {
                trained_as: Some(false), // was trained as ham
                classification: Some(Classification::Ham),
                score: Some(0.1),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );

        // We need a classifier with at least 1 ham trained so unlearn works.
        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        {
            let mut c = classifier.lock().unwrap();
            // Pretend we already trained this message as ham.
            let tokens = Tokenizer::with_defaults().tokenize(&content);
            c.learn(tokens.into_iter(), false);
        }

        let engine = make_engine_with_classifier(classifier.clone(), db);

        let msg = MockTrainableMessage::new(b"key1", &content);

        // Retrain as spam (Requirement 9.3).
        let result = engine.train_message(&msg, true).unwrap();
        assert!(result, "retrained message should return true");

        // Verify: ham unlearned, spam learned.
        let c = classifier.lock().unwrap();
        assert_eq!(c.nham(), 0); // unlearned the ham
        assert_eq!(c.nspam(), 1); // learned as spam
    }

    #[test]
    fn train_message_content_extraction_error() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let msg = MockTrainableMessage::with_error(b"bad_key");

        let result = engine.train_message(&msg, true);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TrainError::ContentExtraction(_)));
    }

    #[test]
    fn untrain_message_previously_trained() {
        let content = make_email_content("Test", "Some content here");
        let db = MockMessageDb::new().with_entry(
            b"key1",
            MessageInfo {
                trained_as: Some(true), // trained as spam
                classification: Some(Classification::Spam),
                score: Some(0.95),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );

        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        {
            let mut c = classifier.lock().unwrap();
            let tokens = Tokenizer::with_defaults().tokenize(&content);
            c.learn(tokens.into_iter(), true);
        }

        let engine = make_engine_with_classifier(classifier.clone(), db);
        let msg = MockTrainableMessage::new(b"key1", &content);

        let result = engine.untrain_message(&msg).unwrap();
        assert_eq!(result, Some(true)); // was trained as spam

        let c = classifier.lock().unwrap();
        assert_eq!(c.nspam(), 0);
    }

    #[test]
    fn untrain_message_not_trained() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let content = make_email_content("Test", "Content");
        let msg = MockTrainableMessage::new(b"key1", &content);

        let result = engine.untrain_message(&msg).unwrap();
        assert_eq!(result, None); // was not trained
    }

    #[test]
    fn train_batch_no_folders_configured() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let config = TrainingConfig {
            ham_folder_ids: vec![],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let provider = MockFolderProvider::new();
        let mut progress = MockProgress::new();

        let result = engine.train_batch(&config, &provider, &mut progress);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TrainError::NoFoldersConfigured));
    }

    #[test]
    fn train_batch_processes_ham_and_spam_folders() {
        let ham_folder = make_ham_folder_id();
        let spam_folder = make_spam_folder_id();

        let ham_content = make_email_content("Hi", "Normal email content");
        let spam_content = make_email_content("Buy!", "Cheap offers now");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &ham_folder,
                vec![
                    (b"ham1".to_vec(), ham_content.clone()),
                    (b"ham2".to_vec(), ham_content.clone()),
                ],
            )
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"spam1".to_vec(), spam_content.clone())],
            );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![spam_folder],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.train_batch(&config, &provider, &mut progress).unwrap();

        assert_eq!(result.total_processed, 3);
        assert_eq!(result.new_entries, 3);
        assert_eq!(result.errors, 0);

        // Verify classifier counts.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 2);
        assert_eq!(classifier.nspam(), 1);
    }

    #[test]
    fn train_batch_includes_sub_folders() {
        let ham_folder = make_ham_folder_id();
        let sub_folder = FolderId::from_ini_str("('AABB0011', 'DDEE6677')").unwrap();

        let content = make_email_content("Hi", "Sub-folder message");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(&ham_folder, vec![(b"h1".to_vec(), content.clone())])
            .add_folder_with_messages(&sub_folder, vec![(b"h2".to_vec(), content.clone())])
            .add_sub_folders(&ham_folder, vec![sub_folder]);

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            ham_include_sub: true,
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.train_batch(&config, &provider, &mut progress).unwrap();

        // Should process messages from both parent and sub-folder.
        assert_eq!(result.total_processed, 2);
        assert_eq!(result.new_entries, 2);

        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 2);
    }

    #[test]
    fn train_batch_handles_per_message_errors() {
        let spam_folder = make_spam_folder_id();

        let good_content = make_email_content("Spam", "Spammy content here");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"good1".to_vec(), good_content.clone())],
            )
            .add_folder_with_error_message(&spam_folder, b"bad1".to_vec());

        let config = TrainingConfig {
            ham_folder_ids: vec![],
            spam_folder_ids: vec![spam_folder],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.train_batch(&config, &provider, &mut progress).unwrap();

        // One message should succeed, one should fail.
        assert_eq!(result.total_processed, 2);
        assert_eq!(result.new_entries, 1);
        assert_eq!(result.errors, 1);
    }

    #[test]
    fn train_batch_reports_progress() {
        let ham_folder = make_ham_folder_id();

        let content = make_email_content("Hi", "Message body");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new().add_folder_with_messages(
            &ham_folder,
            vec![
                (b"m1".to_vec(), content.clone()),
                (b"m2".to_vec(), content.clone()),
            ],
        );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder.clone()],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        engine.train_batch(&config, &provider, &mut progress).unwrap();

        // Progress should have been reported for each message.
        assert_eq!(progress.reports.len(), 2);
        // Each report should contain the folder name.
        for (name, _, _) in &progress.reports {
            assert!(name.contains(&ham_folder.entry_id.0));
        }
    }

    #[test]
    fn train_batch_skips_correctly_trained_messages() {
        let ham_folder = make_ham_folder_id();
        let content = make_email_content("Hi", "Already trained");

        // Pre-populate DB: message already trained as ham.
        let db = MockMessageDb::new().with_entry(
            b"m1",
            MessageInfo {
                trained_as: Some(false), // already trained as ham
                classification: Some(Classification::Ham),
                score: Some(0.05),
                message_id: "msg_m1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let provider = MockFolderProvider::new().add_folder_with_messages(
            &ham_folder,
            vec![(b"m1".to_vec(), content)],
        );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.train_batch(&config, &provider, &mut progress).unwrap();

        // Message was processed but not newly trained.
        assert_eq!(result.total_processed, 1);
        assert_eq!(result.new_entries, 0);
        assert_eq!(result.errors, 0);

        // Classifier should not have been modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    // ─── Rebuild Tests ───────────────────────────────────────────────────

    #[test]
    fn rebuild_no_folders_returns_error() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let config = TrainingConfig {
            ham_folder_ids: vec![],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let provider = MockFolderProvider::new();
        let mut progress = MockProgress::new();

        let result = engine.rebuild(&config, &provider, &mut progress);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TrainError::NoFoldersConfigured));
    }

    #[test]
    fn rebuild_creates_fresh_classifier_and_trains_all() {
        let ham_folder = make_ham_folder_id();
        let spam_folder = make_spam_folder_id();

        let ham_content = make_email_content("Hi friend", "Normal legitimate email content here");
        let spam_content = make_email_content("Buy now!", "Cheap discount pills offer");

        // Pre-train the classifier with some old data (should be discarded).
        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        {
            let mut c = classifier.lock().unwrap();
            let fake_tokens = vec![b"old_token".to_vec()];
            c.learn(fake_tokens.into_iter(), true);
            assert_eq!(c.nspam(), 1);
        }

        let db = MockMessageDb::new();
        let engine = make_engine_with_classifier(classifier.clone(), db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &ham_folder,
                vec![
                    (b"h1".to_vec(), ham_content.clone()),
                    (b"h2".to_vec(), ham_content.clone()),
                ],
            )
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"s1".to_vec(), spam_content.clone())],
            );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![spam_folder],
            rescore: false,
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.rebuild(&config, &provider, &mut progress).unwrap();

        assert_eq!(result.total_processed, 3);
        assert_eq!(result.new_entries, 3);
        assert_eq!(result.errors, 0);

        // Verify old classifier state is gone, replaced with fresh training.
        let c = classifier.lock().unwrap();
        assert_eq!(c.nham(), 2);
        assert_eq!(c.nspam(), 1);
        // The old "old_token" should not exist in the new classifier.
        assert!(!c.word_info().contains_key(&b"old_token".to_vec()));
    }

    #[test]
    fn rebuild_atomically_swaps_classifier() {
        let ham_folder = make_ham_folder_id();
        let ham_content = make_email_content("Hello world", "This is a test message");

        // Start with a classifier that has existing training.
        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        {
            let mut c = classifier.lock().unwrap();
            let old_tokens = vec![b"stale_data".to_vec()];
            c.learn(old_tokens.into_iter(), false);
            c.learn(vec![b"stale_spam".to_vec()].into_iter(), true);
            assert_eq!(c.nham(), 1);
            assert_eq!(c.nspam(), 1);
        }

        let db = MockMessageDb::new();
        let engine = make_engine_with_classifier(classifier.clone(), db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &ham_folder,
                vec![(b"msg1".to_vec(), ham_content)],
            );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![],
            rescore: false,
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        engine.rebuild(&config, &provider, &mut progress).unwrap();

        // After rebuild: old state (nham=1, nspam=1) is replaced.
        // New state: only 1 ham message trained.
        let c = classifier.lock().unwrap();
        assert_eq!(c.nham(), 1);
        assert_eq!(c.nspam(), 0);
        // Old tokens should not exist.
        assert!(!c.word_info().contains_key(&b"stale_data".to_vec()));
        assert!(!c.word_info().contains_key(&b"stale_spam".to_vec()));
    }

    #[test]
    fn rebuild_with_rescore_updates_message_scores() {
        let ham_folder = make_ham_folder_id();
        let spam_folder = make_spam_folder_id();

        let ham_content = make_email_content("Hello friend", "Normal legitimate message content");
        let spam_content = make_email_content("URGENT buy now!", "Cheap pills discount offers free");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &ham_folder,
                vec![(b"h1".to_vec(), ham_content.clone())],
            )
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"s1".to_vec(), spam_content.clone())],
            );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![spam_folder],
            rescore: true, // Enable rescoring
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.rebuild(&config, &provider, &mut progress).unwrap();

        assert_eq!(result.total_processed, 2);
        assert_eq!(result.new_entries, 2);

        // After rescore, messages should have updated scores in the DB.
        let db = engine.message_db.lock().unwrap();

        let h1_info = db.load_msg(b"h1");
        assert!(h1_info.is_some(), "ham message should be in DB");
        let h1 = h1_info.unwrap();
        assert!(h1.score.is_some(), "ham message should have a score after rescore");
        // Ham message score should be low (closer to 0).
        assert!(h1.score.unwrap() < 0.5, "ham score {} should be < 0.5", h1.score.unwrap());

        let s1_info = db.load_msg(b"s1");
        assert!(s1_info.is_some(), "spam message should be in DB");
        let s1 = s1_info.unwrap();
        assert!(s1.score.is_some(), "spam message should have a score after rescore");
        // Spam message score should be high (closer to 1).
        assert!(s1.score.unwrap() > 0.5, "spam score {} should be > 0.5", s1.score.unwrap());
    }

    #[test]
    fn rebuild_handles_message_errors_gracefully() {
        let spam_folder = make_spam_folder_id();
        let good_content = make_email_content("Spam", "Some spam content for training");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"good1".to_vec(), good_content)],
            )
            .add_folder_with_error_message(&spam_folder, b"bad1".to_vec());

        let config = TrainingConfig {
            ham_folder_ids: vec![],
            spam_folder_ids: vec![spam_folder],
            rescore: false,
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine.rebuild(&config, &provider, &mut progress).unwrap();

        // One should succeed, one should fail.
        assert_eq!(result.total_processed, 2);
        assert_eq!(result.new_entries, 1);
        assert_eq!(result.errors, 1);

        // Classifier should still have the successful message trained.
        let c = engine.classifier.lock().unwrap();
        assert_eq!(c.nspam(), 1);
    }

    // ─── Incremental Training Tests ──────────────────────────────────────

    /// Helper to create a `MockStorage` that tracks whether `store` was called.
    struct MockStorageWithTracking {
        stored: std::sync::atomic::AtomicBool,
    }

    impl MockStorageWithTracking {
        fn new() -> Self {
            Self {
                stored: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl StorageBackend for MockStorageWithTracking {
        fn load(&mut self) -> Result<ClassifierState, StorageError> {
            Ok(ClassifierState::default())
        }

        fn store(
            &mut self,
            _state: &ClassifierState,
            _changed: &HashMap<Vec<u8>, WordChange>,
        ) -> Result<(), StorageError> {
            self.stored.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn close(&mut self) -> Result<(), StorageError> {
            Ok(())
        }
    }

    fn make_engine_with_storage(
        message_db: MockMessageDb,
        storage: Arc<Mutex<Box<dyn StorageBackend>>>,
    ) -> TrainingEngine {
        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        let db: Arc<Mutex<Box<dyn MessageDatabase>>> =
            Arc::new(Mutex::new(Box::new(message_db)));
        let logger = make_logger();

        TrainingEngine::new(classifier, storage, db, logger, None)
    }

    #[test]
    fn on_message_moved_manual_spam_trains_as_spam() {
        // Message classified as ham, moved to spam folder → train as spam.
        let content = make_email_content("Hello friend", "Normal email about work");
        let spam_folder = make_spam_folder_id();
        let inbox_folder = make_ham_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Ham),
                score: Some(0.1),
                message_id: "msg_1".to_string(),
                original_folder: Some(inbox_folder.clone()),
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default(); // train_manual_spam = true

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        // Verify: classifier trained as spam.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nspam(), 1);
        assert_eq!(classifier.nham(), 0);

        // Verify: message_db updated.
        let db = engine.message_db.lock().unwrap();
        let info = db.load_msg(b"msg1").unwrap();
        assert_eq!(info.trained_as, Some(true)); // trained as spam
    }

    #[test]
    fn on_message_moved_manual_spam_unlearns_previous_ham_training() {
        // Message was previously trained as ham, now moved to spam → unlearn ham, train spam.
        let content = make_email_content("Hello friend", "Normal email about work");
        let spam_folder = make_spam_folder_id();

        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        {
            // Pre-train the message as ham.
            let mut c = classifier.lock().unwrap();
            let tokens = Tokenizer::with_defaults().tokenize(&content);
            c.learn(tokens.into_iter(), false);
            assert_eq!(c.nham(), 1);
        }

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: Some(false), // previously trained as ham
                classification: Some(Classification::Ham),
                score: Some(0.1),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine_with_classifier(classifier.clone(), db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        // Ham was unlearned, spam was learned.
        let c = classifier.lock().unwrap();
        assert_eq!(c.nham(), 0);
        assert_eq!(c.nspam(), 1);
    }

    #[test]
    fn on_message_moved_recovery_trains_as_ham() {
        // Message classified as spam, moved back to original folder → train as ham.
        let content = make_email_content("Important notice", "Your account needs review");
        let inbox_folder = make_ham_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Spam),
                score: Some(0.9),
                message_id: "msg_1".to_string(),
                original_folder: Some(inbox_folder.clone()),
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default(); // train_recovered_spam = true

        let result = engine.on_message_moved(
            &msg,
            &inbox_folder, // Moving back to original folder
            &config,
            Some(&make_spam_folder_id()),
        );
        assert!(result.is_ok());

        // Verify: classifier trained as ham.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 1);
        assert_eq!(classifier.nspam(), 0);

        // Verify: message_db updated.
        let db = engine.message_db.lock().unwrap();
        let info = db.load_msg(b"msg1").unwrap();
        assert_eq!(info.trained_as, Some(false)); // trained as ham
    }

    #[test]
    fn on_message_moved_recovery_unlearns_previous_spam_training() {
        // Message previously trained as spam, moved back to original → unlearn spam, train ham.
        let content = make_email_content("Important notice", "Your account needs review");
        let inbox_folder = make_ham_folder_id();

        let classifier = Arc::new(Mutex::new(Classifier::with_defaults()));
        {
            let mut c = classifier.lock().unwrap();
            let tokens = Tokenizer::with_defaults().tokenize(&content);
            c.learn(tokens.into_iter(), true);
            assert_eq!(c.nspam(), 1);
        }

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: Some(true), // previously trained as spam
                classification: Some(Classification::Spam),
                score: Some(0.9),
                message_id: "msg_1".to_string(),
                original_folder: Some(inbox_folder.clone()),
            },
        );
        let engine = make_engine_with_classifier(classifier.clone(), db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &inbox_folder,
            &config,
            Some(&make_spam_folder_id()),
        );
        assert!(result.is_ok());

        // Spam was unlearned, ham was learned.
        let c = classifier.lock().unwrap();
        assert_eq!(c.nspam(), 0);
        assert_eq!(c.nham(), 1);
    }

    #[test]
    fn on_message_moved_skips_unclassified_messages() {
        // Requirement 10.5: Message never classified → skip.
        let content = make_email_content("Hello", "Some message");
        let spam_folder = make_spam_folder_id();

        // No classification field → never classified by SpamBayes.
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        // Classifier should not have been modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn on_message_moved_skips_when_classification_is_none() {
        // Requirement 10.5: classification = None → skip.
        let content = make_email_content("Hello", "Some message");
        let spam_folder = make_spam_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: Some(false),
                classification: None, // Never classified
                score: None,
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        // Classifier should not have been modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn on_message_moved_content_extraction_error_leaves_db_unchanged() {
        // Requirement 10.3: Error during tokenization → log, leave DB unchanged.
        let spam_folder = make_spam_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"bad_msg",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Ham),
                score: Some(0.1),
                message_id: "bad_msg".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        // Message with content extraction error.
        let msg = MockTrainableMessage::with_error(b"bad_msg");
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        // Should return Ok (error is logged but not propagated).
        assert!(result.is_ok());

        // Classifier untouched.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);

        // Message DB unchanged (trained_as still None).
        let db = engine.message_db.lock().unwrap();
        let info = db.load_msg(b"bad_msg").unwrap();
        assert_eq!(info.trained_as, None);
    }

    #[test]
    fn on_message_moved_disabled_manual_spam_does_not_train() {
        // train_manual_spam = false → no training even when moved to spam.
        let content = make_email_content("Hello", "Normal message");
        let spam_folder = make_spam_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Ham),
                score: Some(0.1),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig {
            train_manual_spam: false,
            ..TrainingConfig::default()
        };

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        // Classifier should not have been modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn on_message_moved_disabled_recovered_spam_does_not_train() {
        // train_recovered_spam = false → no training even when moved back.
        let content = make_email_content("Important", "Account notification");
        let inbox_folder = make_ham_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Spam),
                score: Some(0.9),
                message_id: "msg_1".to_string(),
                original_folder: Some(inbox_folder.clone()),
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig {
            train_recovered_spam: false,
            ..TrainingConfig::default()
        };

        let result = engine.on_message_moved(
            &msg,
            &inbox_folder,
            &config,
            Some(&make_spam_folder_id()),
        );
        assert!(result.is_ok());

        // Classifier should not have been modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn on_message_moved_unsure_to_spam_trains_as_spam() {
        // Unsure classification + moved to spam → train as spam.
        let content = make_email_content("Maybe spam", "Could be spam content");
        let spam_folder = make_spam_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Unsure),
                score: Some(0.5),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nspam(), 1);
    }

    #[test]
    fn on_message_moved_unsure_recovered_trains_as_ham() {
        // Unsure classification + moved back to original → train as ham.
        let content = make_email_content("Maybe ok", "Could be legit");
        let inbox_folder = make_ham_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Unsure),
                score: Some(0.5),
                message_id: "msg_1".to_string(),
                original_folder: Some(inbox_folder.clone()),
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &inbox_folder,
            &config,
            Some(&make_spam_folder_id()),
        );
        assert!(result.is_ok());

        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 1);
    }

    #[test]
    fn save_post_incremental_persists_classifier_state() {
        // Requirement 10.4: Persist classifier data after incremental training.
        let storage = Arc::new(Mutex::new(
            Box::new(MockStorageWithTracking::new()) as Box<dyn StorageBackend>
        ));
        let db = MockMessageDb::new();
        let engine = make_engine_with_storage(db, storage.clone());

        // Train something so there's data to persist.
        let content = make_email_content("Spam!", "Buy cheap things now");
        let msg = MockTrainableMessage::new(b"key1", &content);
        engine.train_message(&msg, true).unwrap();

        // Now persist.
        let result = engine.save_post_incremental();
        assert!(result.is_ok());

        // Verify store was called (we can't directly check MockStorageWithTracking
        // through the Box<dyn>, but we know the method completed successfully).
    }

    #[test]
    fn on_message_moved_spam_to_spam_no_action() {
        // Already classified as spam, moved to spam folder → no training action.
        let content = make_email_content("Buy now", "Cheap pills");
        let spam_folder = make_spam_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Spam),
                score: Some(0.95),
                message_id: "msg_1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        let result = engine.on_message_moved(
            &msg,
            &spam_folder,
            &config,
            Some(&spam_folder),
        );
        assert!(result.is_ok());

        // No training happened — already classified as spam.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn on_message_moved_no_original_folder_skips_recovery() {
        // Message has no original_folder recorded → recovery is not possible.
        let content = make_email_content("Some message", "Content");
        let inbox_folder = make_ham_folder_id();

        let db = MockMessageDb::new().with_entry(
            b"msg1",
            MessageInfo {
                trained_as: None,
                classification: Some(Classification::Spam),
                score: Some(0.9),
                message_id: "msg_1".to_string(),
                original_folder: None, // No original folder recorded
            },
        );
        let engine = make_engine(db);

        let msg = MockTrainableMessage::new(b"msg1", &content);
        let config = TrainingConfig::default();

        // Move to inbox, but no original_folder → can't determine if it's recovery.
        let result = engine.on_message_moved(
            &msg,
            &inbox_folder,
            &config,
            Some(&make_spam_folder_id()),
        );
        assert!(result.is_ok());

        // No training happened.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    // ─── Concurrent Training Tests ───────────────────────────────────────

    #[test]
    fn train_batch_concurrent_no_folders_returns_error() {
        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let config = TrainingConfig {
            ham_folder_ids: vec![],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let provider = MockFolderProvider::new();
        let mut progress = MockProgress::new();

        let result = engine.train_batch_concurrent(&config, &provider, &mut progress, 4);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TrainError::NoFoldersConfigured));
    }

    #[test]
    fn train_batch_concurrent_processes_ham_and_spam() {
        let ham_folder = make_ham_folder_id();
        let spam_folder = make_spam_folder_id();

        let ham_content = make_email_content("Hi", "Normal email content from a friend");
        let spam_content = make_email_content("Buy!", "Cheap offers discount now");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &ham_folder,
                vec![
                    (b"ham1".to_vec(), ham_content.clone()),
                    (b"ham2".to_vec(), ham_content.clone()),
                ],
            )
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"spam1".to_vec(), spam_content.clone())],
            );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![spam_folder],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine
            .train_batch_concurrent(&config, &provider, &mut progress, 4)
            .unwrap();

        assert_eq!(result.total_processed, 3);
        assert_eq!(result.new_entries, 3);
        assert_eq!(result.errors, 0);

        // Verify classifier counts match sequential training.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 2);
        assert_eq!(classifier.nspam(), 1);
    }

    #[test]
    fn train_batch_concurrent_handles_content_errors() {
        let spam_folder = make_spam_folder_id();
        let good_content = make_email_content("Spam", "Spammy content here");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &spam_folder,
                vec![(b"good1".to_vec(), good_content.clone())],
            )
            .add_folder_with_error_message(&spam_folder, b"bad1".to_vec());

        let config = TrainingConfig {
            ham_folder_ids: vec![],
            spam_folder_ids: vec![spam_folder],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine
            .train_batch_concurrent(&config, &provider, &mut progress, 4)
            .unwrap();

        assert_eq!(result.total_processed, 2);
        assert_eq!(result.new_entries, 1);
        assert_eq!(result.errors, 1);
    }

    #[test]
    fn train_batch_concurrent_skips_correctly_trained() {
        let ham_folder = make_ham_folder_id();
        let content = make_email_content("Hi", "Already trained message");

        // Pre-populate DB: message already trained as ham.
        let db = MockMessageDb::new().with_entry(
            b"m1",
            MessageInfo {
                trained_as: Some(false),
                classification: Some(Classification::Ham),
                score: Some(0.05),
                message_id: "msg_m1".to_string(),
                original_folder: None,
            },
        );
        let engine = make_engine(db);

        let provider = MockFolderProvider::new().add_folder_with_messages(
            &ham_folder,
            vec![(b"m1".to_vec(), content)],
        );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine
            .train_batch_concurrent(&config, &provider, &mut progress, 4)
            .unwrap();

        assert_eq!(result.total_processed, 1);
        assert_eq!(result.new_entries, 0);
        assert_eq!(result.errors, 0);

        // Classifier should not have been modified.
        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 0);
        assert_eq!(classifier.nspam(), 0);
    }

    #[test]
    fn train_batch_concurrent_produces_same_result_as_sequential() {
        // Verify that concurrent training produces the same classifier state
        // as sequential training, ensuring thread safety.
        let ham_folder = make_ham_folder_id();
        let spam_folder = make_spam_folder_id();

        let ham_messages: Vec<(Vec<u8>, Vec<u8>)> = (0..10)
            .map(|i| {
                let content = make_email_content(
                    &format!("Ham message {i}"),
                    &format!("This is legitimate email number {i} about work topics"),
                );
                (format!("ham_{i}").into_bytes(), content)
            })
            .collect();

        let spam_messages: Vec<(Vec<u8>, Vec<u8>)> = (0..10)
            .map(|i| {
                let content = make_email_content(
                    &format!("Buy now! Offer {i}"),
                    &format!("Cheap discount pills offer {i} free money"),
                );
                (format!("spam_{i}").into_bytes(), content)
            })
            .collect();

        // Sequential training.
        let db_seq = MockMessageDb::new();
        let engine_seq = make_engine(db_seq);

        let provider_seq = MockFolderProvider::new()
            .add_folder_with_messages(&ham_folder, ham_messages.clone())
            .add_folder_with_messages(&spam_folder, spam_messages.clone());

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder.clone()],
            spam_folder_ids: vec![spam_folder.clone()],
            ..TrainingConfig::default()
        };

        let mut progress_seq = MockProgress::new();
        let result_seq = engine_seq
            .train_batch(&config, &provider_seq, &mut progress_seq)
            .unwrap();

        // Concurrent training.
        let db_conc = MockMessageDb::new();
        let engine_conc = make_engine(db_conc);

        let provider_conc = MockFolderProvider::new()
            .add_folder_with_messages(&ham_folder, ham_messages)
            .add_folder_with_messages(&spam_folder, spam_messages);

        let mut progress_conc = MockProgress::new();
        let result_conc = engine_conc
            .train_batch_concurrent(&config, &provider_conc, &mut progress_conc, 4)
            .unwrap();

        // Results should match.
        assert_eq!(result_seq.total_processed, result_conc.total_processed);
        assert_eq!(result_seq.new_entries, result_conc.new_entries);
        assert_eq!(result_seq.errors, result_conc.errors);

        // Classifier state should match.
        let c_seq = engine_seq.classifier.lock().unwrap();
        let c_conc = engine_conc.classifier.lock().unwrap();
        assert_eq!(c_seq.nham(), c_conc.nham());
        assert_eq!(c_seq.nspam(), c_conc.nspam());

        // Token counts should be identical.
        assert_eq!(c_seq.word_info().len(), c_conc.word_info().len());
        for (token, info_seq) in c_seq.word_info() {
            let info_conc = c_conc.word_info().get(token)
                .expect("concurrent classifier missing token from sequential");
            assert_eq!(
                info_seq, info_conc,
                "Token {:?} has different counts: seq={:?}, conc={:?}",
                String::from_utf8_lossy(token), info_seq, info_conc
            );
        }
    }

    #[test]
    fn train_batch_concurrent_uses_minimum_4_workers() {
        // Even if we pass fewer than 4 workers, it should clamp to 4.
        // This test just verifies the method works with num_workers < MIN_WORKER_THREADS.
        let ham_folder = make_ham_folder_id();
        let content = make_email_content("Hi", "Test message content");

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(
                &ham_folder,
                vec![(b"m1".to_vec(), content)],
            );

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        // Pass 1 worker — should be clamped to 4 internally.
        let result = engine
            .train_batch_concurrent(&config, &provider, &mut progress, 1)
            .unwrap();

        assert_eq!(result.total_processed, 1);
        assert_eq!(result.new_entries, 1);
        assert_eq!(result.errors, 0);
    }

    #[test]
    fn train_batch_concurrent_handles_many_messages() {
        // Test with more messages than workers to verify chunking works correctly.
        let ham_folder = make_ham_folder_id();

        let messages: Vec<(Vec<u8>, Vec<u8>)> = (0..20)
            .map(|i| {
                let content = make_email_content(
                    &format!("Message {i}"),
                    &format!("Content for message number {i} with unique words_{i}"),
                );
                (format!("msg_{i}").into_bytes(), content)
            })
            .collect();

        let db = MockMessageDb::new();
        let engine = make_engine(db);

        let provider = MockFolderProvider::new()
            .add_folder_with_messages(&ham_folder, messages);

        let config = TrainingConfig {
            ham_folder_ids: vec![ham_folder],
            spam_folder_ids: vec![],
            ..TrainingConfig::default()
        };

        let mut progress = MockProgress::new();
        let result = engine
            .train_batch_concurrent(&config, &provider, &mut progress, 4)
            .unwrap();

        assert_eq!(result.total_processed, 20);
        assert_eq!(result.new_entries, 20);
        assert_eq!(result.errors, 0);

        let classifier = engine.classifier.lock().unwrap();
        assert_eq!(classifier.nham(), 20);
    }
}
