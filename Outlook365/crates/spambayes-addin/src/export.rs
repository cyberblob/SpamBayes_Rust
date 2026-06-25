//! Training data export - exports ham/spam messages to a directory structure.
//!
//! This module exports user-categorized training messages (ham and spam) into
//! a standard directory layout for cross-validation testing tools. Messages are
//! distributed across numbered bucket subdirectories using a deterministic hash
//! of the message search key.
//!
//! # Directory Structure
//!
//! ```text
//! <export_directory>/
//! ├── Ham/
//! │   ├── 0/
//! │   ├── 1/
//! │   ├── ...
//! │   └── N-1/
//! └── Spam/
//!     ├── 0/
//!     ├── 1/
//!     ├── ...
//!     └── N-1/
//! ```
//!
//! # Key Differences from Python `export.py`
//!
//! - **No deletion**: If the target directory exists, new files are appended
//!   without deleting existing content (Requirement 16.5).
//! - **Deterministic hashing**: Bucket selection uses a hash of the message
//!   search key rather than random selection (Requirement 16.2).
//! - **Bucket numbering**: Directories are numbered 0 through N-1 (not 1..N).
//!
//! # Validates: Requirements 16.1, 16.2, 16.3, 16.4, 16.5

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use spambayes_config::FolderId;
use spambayes_mapi::MsgStoreError;

use crate::filter::Progress;
use crate::logger::Logger;
use crate::train::{TrainableMessage, TrainingFolderProvider};

/// Module name used in log messages.
const MODULE: &str = "Export";

// ─── ExportConfig ────────────────────────────────────────────────────────────

/// Configuration for a training data export operation.
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Number of bucket subdirectories (0 to num_buckets-1). Default: 10.
    pub num_buckets: u32,
    /// Root directory for the export output.
    pub export_directory: PathBuf,
    /// Folder IDs containing ham (good) messages.
    pub ham_folder_ids: Vec<FolderId>,
    /// Whether ham folder selection includes sub-folders.
    pub ham_include_sub: bool,
    /// Folder IDs containing spam messages.
    pub spam_folder_ids: Vec<FolderId>,
    /// Whether spam folder selection includes sub-folders.
    pub spam_include_sub: bool,
}

impl ExportConfig {
    /// Create a new `ExportConfig` with the given export directory and default bucket count.
    #[must_use]
    pub fn new(export_directory: PathBuf) -> Self {
        Self {
            num_buckets: 10,
            export_directory,
            ham_folder_ids: Vec::new(),
            ham_include_sub: false,
            spam_folder_ids: Vec::new(),
            spam_include_sub: false,
        }
    }
}

// ─── ExportResult ────────────────────────────────────────────────────────────

/// Result of a training data export operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportResult {
    /// Number of ham messages successfully exported.
    pub ham_exported: u32,
    /// Number of spam messages successfully exported.
    pub spam_exported: u32,
    /// Number of messages that failed to export.
    pub errors: u32,
    /// Whether the operation was cancelled by the user.
    pub cancelled: bool,
}

impl ExportResult {
    /// Create an empty result with all counts at zero.
    fn new() -> Self {
        Self {
            ham_exported: 0,
            spam_exported: 0,
            errors: 0,
            cancelled: false,
        }
    }
}

// ─── ExportEngine ────────────────────────────────────────────────────────────

/// Engine that exports training messages to a directory structure.
///
/// Uses the same `TrainingFolderProvider` and `TrainableMessage` traits
/// as the training engine, ensuring consistent message access patterns.
pub struct ExportEngine {
    /// Logger for diagnostic output.
    logger: Arc<Logger>,
}

impl ExportEngine {
    /// Create a new `ExportEngine` with the given logger.
    pub fn new(logger: Arc<Logger>) -> Self {
        Self { logger }
    }

    /// Export training data according to the given configuration.
    ///
    /// Creates the directory structure (Ham/0..N-1, Spam/0..N-1) under the
    /// configured export directory, then writes each message as an RFC 2822
    /// text file. Existing files are not deleted (append mode).
    ///
    /// # Arguments
    ///
    /// * `config` - Export configuration (directories, folders, bucket count)
    /// * `provider` - Folder/message access abstraction
    /// * `progress` - Progress reporter and cancellation checker
    ///
    /// # Returns
    ///
    /// An [`ExportResult`] with counts of exported messages and errors.
    ///
    /// # Validates: Requirements 16.1, 16.2, 16.3, 16.4, 16.5
    pub fn export(
        &self,
        config: &ExportConfig,
        provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
    ) -> ExportResult {
        let mut result = ExportResult::new();

        // Create directory structure (Req 16.5: don't delete existing).
        if let Err(e) = self.create_directory_structure(config) {
            self.logger.error(
                MODULE,
                &format!("Failed to create export directory structure: {e}"),
            );
            return result;
        }

        self.logger.info(
            MODULE,
            &format!(
                "Exporting to '{}' with {} buckets",
                config.export_directory.display(),
                config.num_buckets
            ),
        );

        // Export ham messages (Req 16.1).
        let ham_dir = config.export_directory.join("Ham");
        let ham_count = self.export_folders(
            &config.ham_folder_ids,
            config.ham_include_sub,
            &ham_dir,
            config.num_buckets,
            provider,
            progress,
            &mut result,
            true, // is_ham
        );
        result.ham_exported = ham_count;

        if result.cancelled {
            return result;
        }

        // Export spam messages (Req 16.1).
        let spam_dir = config.export_directory.join("Spam");
        let spam_count = self.export_folders(
            &config.spam_folder_ids,
            config.spam_include_sub,
            &spam_dir,
            config.num_buckets,
            provider,
            progress,
            &mut result,
            false, // is_spam
        );
        result.spam_exported = spam_count;

        self.logger.info(
            MODULE,
            &format!(
                "Export complete: {} ham, {} spam exported, {} errors",
                result.ham_exported, result.spam_exported, result.errors
            ),
        );

        result
    }

    /// Create the export directory structure without deleting existing files.
    ///
    /// Creates `Ham/0..N-1` and `Spam/0..N-1` subdirectories.
    ///
    /// # Validates: Requirement 16.5
    fn create_directory_structure(
        &self,
        config: &ExportConfig,
    ) -> Result<(), std::io::Error> {
        for category in &["Ham", "Spam"] {
            for bucket in 0..config.num_buckets {
                let dir = config
                    .export_directory
                    .join(category)
                    .join(bucket.to_string());
                fs::create_dir_all(&dir)?;
            }
        }
        Ok(())
    }

    /// Export messages from a set of folders into the given root directory.
    ///
    /// Expands sub-folders if `include_sub` is true, iterates messages,
    /// and writes each as an RFC 2822 file in the appropriate bucket.
    ///
    /// Returns the number of successfully exported messages.
    #[allow(clippy::too_many_arguments)]
    fn export_folders(
        &self,
        folder_ids: &[FolderId],
        include_sub: bool,
        root_dir: &Path,
        num_buckets: u32,
        provider: &dyn TrainingFolderProvider,
        progress: &mut dyn Progress,
        result: &mut ExportResult,
        _is_ham: bool,
    ) -> u32 {
        let mut exported: u32 = 0;

        // Expand folder list (include sub-folders if configured).
        let all_folders = self.expand_folders(folder_ids, include_sub, provider);

        for folder_id in &all_folders {
            // Check cancellation.
            if progress.is_cancelled() {
                result.cancelled = true;
                return exported;
            }

            let folder_name = provider
                .get_folder_name(folder_id)
                .unwrap_or_else(|_| "(unknown)".to_string());

            self.logger.verbose(MODULE, &format!("Exporting folder: {folder_name}"));

            // Get messages from this folder.
            let messages = match provider.get_messages(folder_id) {
                Ok(msgs) => msgs,
                Err(e) => {
                    // Req 16.4: Log error and continue.
                    self.logger.error(
                        MODULE,
                        &format!("Failed to get messages from folder '{folder_name}': {e}"),
                    );
                    result.errors += 1;
                    continue;
                }
            };

            let total = messages.len() as u32;

            for (idx, msg) in messages.iter().enumerate() {
                // Check cancellation.
                if progress.is_cancelled() {
                    result.cancelled = true;
                    return exported;
                }

                // Report progress.
                progress.report(&folder_name, (idx as u32) + 1, total);

                // Export individual message.
                match self.export_message(msg.as_ref(), root_dir, num_buckets) {
                    Ok(()) => exported += 1,
                    Err(e) => {
                        // Req 16.4: Log per-message error and continue.
                        self.logger.error(
                            MODULE,
                            &format!(
                                "Failed to export message '{}' from folder '{}': {}",
                                msg.get_display_id(),
                                folder_name,
                                e
                            ),
                        );
                        result.errors += 1;
                    }
                }
            }
        }

        exported
    }

    /// Export a single message as an RFC 2822 file in the appropriate bucket.
    ///
    /// The bucket is determined by hashing the message's search key.
    /// The filename is the hex-encoded search key with a `.txt` extension.
    ///
    /// # Validates: Requirements 16.2, 16.3
    fn export_message(
        &self,
        msg: &dyn TrainableMessage,
        root_dir: &Path,
        num_buckets: u32,
    ) -> Result<(), ExportError> {
        // Get the raw RFC 2822 content (Req 16.3).
        let content = msg.get_raw_content().map_err(ExportError::MessageRead)?;

        // Get the search key for bucket hashing and filename.
        let search_key = msg.get_search_key();

        // Determine bucket using hash of search key (Req 16.2).
        let bucket = hash_to_bucket(search_key, num_buckets);

        // Build the output path: root_dir/<bucket>/<hex_search_key>.txt
        let filename = hex_encode(search_key) + ".txt";
        let output_path = root_dir.join(bucket.to_string()).join(&filename);

        // Write the message content (Req 16.3: RFC 2822 formatted text file).
        // Req 16.5: We don't delete existing files; we just write new ones.
        fs::write(&output_path, &content).map_err(ExportError::FileWrite)?;

        Ok(())
    }

    /// Expand a list of folder IDs to include sub-folders if configured.
    fn expand_folders(
        &self,
        folder_ids: &[FolderId],
        include_sub: bool,
        provider: &dyn TrainingFolderProvider,
    ) -> Vec<FolderId> {
        let mut all_folders = Vec::new();

        for folder_id in folder_ids {
            all_folders.push(folder_id.clone());

            if include_sub {
                match provider.get_sub_folders(folder_id) {
                    Ok(subs) => all_folders.extend(subs),
                    Err(e) => {
                        self.logger.error(
                            MODULE,
                            &format!("Failed to get sub-folders: {e}"),
                        );
                    }
                }
            }
        }

        all_folders
    }
}

// ─── ExportError ─────────────────────────────────────────────────────────────

/// Internal error type for export operations.
#[derive(Debug)]
enum ExportError {
    /// Failed to read message content.
    MessageRead(MsgStoreError),
    /// Failed to write file to disk.
    FileWrite(std::io::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::MessageRead(e) => write!(f, "failed to read message: {e}"),
            ExportError::FileWrite(e) => write!(f, "failed to write file: {e}"),
        }
    }
}

// ─── Helper Functions ────────────────────────────────────────────────────────

/// Hash a search key to determine the bucket subdirectory (0 to num_buckets-1).
///
/// Uses the standard library `DefaultHasher` for a deterministic, uniform
/// distribution across buckets.
///
/// # Validates: Requirement 16.2
fn hash_to_bucket(search_key: &[u8], num_buckets: u32) -> u32 {
    let mut hasher = DefaultHasher::new();
    search_key.hash(&mut hasher);
    let hash_value = hasher.finish();
    (hash_value % u64::from(num_buckets)) as u32
}

/// Hex-encode a byte slice to a lowercase hex string.
///
/// Used to derive unique filenames from message search keys.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_to_bucket_deterministic() {
        let key = b"test_search_key_123";
        let bucket1 = hash_to_bucket(key, 10);
        let bucket2 = hash_to_bucket(key, 10);
        assert_eq!(bucket1, bucket2, "Same key should always map to same bucket");
    }

    #[test]
    fn test_hash_to_bucket_range() {
        let num_buckets = 10;
        for i in 0..100 {
            let key = format!("key_{i}");
            let bucket = hash_to_bucket(key.as_bytes(), num_buckets);
            assert!(
                bucket < num_buckets,
                "Bucket {bucket} should be < {num_buckets}"
            );
        }
    }

    #[test]
    fn test_hash_to_bucket_single_bucket() {
        let key = b"any_key";
        let bucket = hash_to_bucket(key, 1);
        assert_eq!(bucket, 0, "With 1 bucket, result must be 0");
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0xAB, 0xCD, 0x01, 0xFF]), "abcd01ff");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00]), "00");
    }

    #[test]
    fn test_export_config_default_buckets() {
        let config = ExportConfig::new(PathBuf::from("/tmp/export"));
        assert_eq!(config.num_buckets, 10);
        assert!(config.ham_folder_ids.is_empty());
        assert!(config.spam_folder_ids.is_empty());
    }

    #[test]
    fn test_export_result_new() {
        let result = ExportResult::new();
        assert_eq!(result.ham_exported, 0);
        assert_eq!(result.spam_exported, 0);
        assert_eq!(result.errors, 0);
        assert!(!result.cancelled);
    }

    // Integration test with mock provider
    use std::cell::RefCell;

    struct MockMessage {
        search_key: Vec<u8>,
        content: Vec<u8>,
        display_id: String,
    }

    impl TrainableMessage for MockMessage {
        fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError> {
            Ok(self.content.clone())
        }

        fn get_search_key(&self) -> &[u8] {
            &self.search_key
        }

        fn get_display_id(&self) -> String {
            self.display_id.clone()
        }
    }

    struct MockProvider {
        folder_name: String,
        messages: Vec<MockMessage>,
    }

    impl TrainingFolderProvider for MockProvider {
        fn get_folder_name(&self, _folder_id: &FolderId) -> Result<String, MsgStoreError> {
            Ok(self.folder_name.clone())
        }

        fn get_sub_folders(&self, _folder_id: &FolderId) -> Result<Vec<FolderId>, MsgStoreError> {
            Ok(Vec::new())
        }

        fn get_messages(
            &self,
            _folder_id: &FolderId,
        ) -> Result<Vec<Box<dyn TrainableMessage>>, MsgStoreError> {
            let msgs: Vec<Box<dyn TrainableMessage>> = self
                .messages
                .iter()
                .map(|m| {
                    Box::new(MockMessage {
                        search_key: m.search_key.clone(),
                        content: m.content.clone(),
                        display_id: m.display_id.clone(),
                    }) as Box<dyn TrainableMessage>
                })
                .collect();
            Ok(msgs)
        }
    }

    struct MockProgress {
        cancelled: bool,
        reports: RefCell<Vec<(String, u32, u32)>>,
    }

    impl MockProgress {
        fn new() -> Self {
            Self {
                cancelled: false,
                reports: RefCell::new(Vec::new()),
            }
        }
    }

    impl Progress for MockProgress {
        fn report(&mut self, folder_name: &str, current: u32, total: u32) {
            self.reports
                .borrow_mut()
                .push((folder_name.to_string(), current, total));
        }

        fn is_cancelled(&self) -> bool {
            self.cancelled
        }
    }

    #[test]
    fn test_export_creates_directory_structure() {
        use spambayes_config::{EntryId, StoreId};

        let temp_dir = std::env::temp_dir().join("spambayes_export_test_dirs");
        let _ = fs::remove_dir_all(&temp_dir); // Clean up from previous runs.

        let config = ExportConfig {
            num_buckets: 3,
            export_directory: temp_dir.clone(),
            ham_folder_ids: vec![FolderId::new(
                StoreId::new("AABB"),
                EntryId::new("CCDD"),
            )],
            ham_include_sub: false,
            spam_folder_ids: vec![FolderId::new(
                StoreId::new("AABB"),
                EntryId::new("EEFF"),
            )],
            spam_include_sub: false,
        };

        let logger = Arc::new(
            Logger::new(
                &std::env::temp_dir().join("spambayes_export_test.log"),
                crate::LogLevel::Verbose,
            )
            .unwrap(),
        );

        let provider = MockProvider {
            folder_name: "TestFolder".to_string(),
            messages: vec![MockMessage {
                search_key: vec![0xAA, 0xBB, 0xCC],
                content: b"From: test@example.com\r\nSubject: Test\r\n\r\nBody".to_vec(),
                display_id: "Test Message".to_string(),
            }],
        };

        let engine = ExportEngine::new(logger);
        let mut progress = MockProgress::new();
        let result = engine.export(&config, &provider, &mut progress);

        // Check directory structure was created.
        for category in &["Ham", "Spam"] {
            for bucket in 0..3u32 {
                let dir = temp_dir.join(category).join(bucket.to_string());
                assert!(dir.exists(), "Directory {dir:?} should exist");
            }
        }

        // At least one message was exported.
        assert!(result.ham_exported > 0 || result.spam_exported > 0);
        assert_eq!(result.errors, 0);
        assert!(!result.cancelled);

        // Clean up.
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_export_appends_without_deleting() {
        use spambayes_config::{EntryId, StoreId};

        let temp_dir = std::env::temp_dir().join("spambayes_export_test_append");
        let _ = fs::remove_dir_all(&temp_dir);

        // Create a pre-existing file in the export directory.
        let pre_existing_path = temp_dir.join("Ham").join("0").join("pre_existing.txt");
        fs::create_dir_all(pre_existing_path.parent().unwrap()).unwrap();
        fs::write(&pre_existing_path, b"existing content").unwrap();

        let config = ExportConfig {
            num_buckets: 3,
            export_directory: temp_dir.clone(),
            ham_folder_ids: vec![FolderId::new(
                StoreId::new("AABB"),
                EntryId::new("CCDD"),
            )],
            ham_include_sub: false,
            spam_folder_ids: Vec::new(),
            spam_include_sub: false,
        };

        let logger = Arc::new(
            Logger::new(
                &std::env::temp_dir().join("spambayes_export_test_append.log"),
                crate::LogLevel::Verbose,
            )
            .unwrap(),
        );

        let provider = MockProvider {
            folder_name: "Inbox".to_string(),
            messages: vec![MockMessage {
                search_key: vec![0x01, 0x02, 0x03],
                content: b"From: sender@test.com\r\nSubject: Hello\r\n\r\nHello world".to_vec(),
                display_id: "Hello Message".to_string(),
            }],
        };

        let engine = ExportEngine::new(logger);
        let mut progress = MockProgress::new();
        let result = engine.export(&config, &provider, &mut progress);

        // Pre-existing file should still exist (Req 16.5).
        assert!(
            pre_existing_path.exists(),
            "Pre-existing file should not be deleted"
        );
        let content = fs::read_to_string(&pre_existing_path).unwrap();
        assert_eq!(content, "existing content");

        // New message should be exported.
        assert_eq!(result.ham_exported, 1);
        assert_eq!(result.errors, 0);

        // Clean up.
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
