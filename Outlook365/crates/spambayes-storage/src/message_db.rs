//! Memory-mapped message metadata database.
//!
//! Provides [`MmapMessageDb`], a persistent store for per-message classification
//! metadata using a custom binary format with atomic file writes.
//!
//! # File Format
//!
//! ```text
//! [Magic: 4 bytes "SBMM"]
//! [Version: u32 LE]
//! [num_entries: u64 LE]
//! For each entry:
//!   [key_len: u32 LE]
//!   [key: key_len bytes]
//!   [trained_as: u8]       — 0=untrained, 1=ham, 2=spam
//!   [classification: u8]   — 0=none, 1=ham, 2=spam, 3=unsure
//!   [score: f64 LE]        — NaN sentinel for None
//!   [msg_id_len: u32 LE]
//!   [message_id: msg_id_len bytes UTF-8]
//! ```
//!
//! On store, the full database is rewritten atomically via [`safe_write`](crate::safe_write)
//! to guarantee crash safety.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use spambayes_core::Classification;

use crate::safe_write::safe_write;
use crate::traits::{MessageDatabase, MessageInfo};
use crate::StorageError;

/// Magic bytes identifying a `SpamBayes` message database file.
const MAGIC: &[u8; 4] = b"SBMM";

/// Current file format version.
const VERSION: u32 = 1;

/// Size of the file header: magic(4) + version(4) + `num_entries(8)` = 16.
const HEADER_SIZE: usize = 16;

/// Sentinel value for `score` field when `None`.
const SCORE_NONE_SENTINEL: f64 = f64::NAN;

/// Memory-mapped message metadata database.
///
/// Stores per-message classification metadata (trained status, classification,
/// score, message ID) indexed by `PR_SEARCH_KEY`. Uses [`safe_write`] for
/// crash-safe persistence.
pub struct MmapMessageDb {
    /// Path to the database file on disk.
    path: PathBuf,
    /// In-memory message data loaded from the file.
    data: HashMap<Vec<u8>, MessageInfo>,
}

impl MmapMessageDb {
    /// Create a new message database targeting the given file path.
    ///
    /// The file is not read until [`load_from_file`](Self::load_from_file) is called.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            data: HashMap::new(),
        }
    }

    /// Returns the path to the backing database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the number of message entries currently held in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if there are no message entries in memory.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Load message data from the backing file.
    ///
    /// If the file does not exist, initializes an empty database.
    /// If the file is corrupted, logs the error and initializes empty.
    pub fn load_from_file(&mut self) -> Result<(), StorageError> {
        if !self.path.exists() {
            self.data = HashMap::new();
            return Ok(());
        }

        let result = (|| -> Result<HashMap<Vec<u8>, MessageInfo>, StorageError> {
            let file = fs::File::open(&self.path)?;
            let metadata = file.metadata()?;

            if metadata.len() == 0 {
                return Ok(HashMap::new());
            }

            // SAFETY: The file is opened read-only and we hold the File handle.
            // The mmap is only used within this scope for parsing.
            let mmap = unsafe { Mmap::map(&file) }?;
            Self::parse(&mmap)
        })();

        match result {
            Ok(data) => {
                self.data = data;
                Ok(())
            }
            Err(StorageError::Corrupted(msg) | StorageError::Deserialize(msg)) => {
                eprintln!(
                    "spambayes-storage: corrupted message database at '{}': {}. Initializing empty.",
                    self.path.display(),
                    msg
                );
                self.data = HashMap::new();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Save all message data to the backing file atomically.
    pub fn save_to_file(&self) -> Result<(), StorageError> {
        let bytes = Self::serialize(&self.data);
        safe_write(&self.path, &bytes)?;
        Ok(())
    }

    /// Parse the database file contents from a byte slice.
    ///
    /// Returns a map of search keys to message info.
    fn parse(bytes: &[u8]) -> Result<HashMap<Vec<u8>, MessageInfo>, StorageError> {
        if bytes.len() < HEADER_SIZE {
            return Err(StorageError::Corrupted(
                "file too short for header".to_string(),
            ));
        }

        // Validate magic.
        if &bytes[0..4] != MAGIC {
            return Err(StorageError::Corrupted(format!(
                "invalid magic: expected SBMM, got {:?}",
                &bytes[0..4]
            )));
        }

        let _version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let num_entries = u64::from_le_bytes(bytes[8..16].try_into().unwrap());

        let mut data = HashMap::with_capacity(num_entries as usize);
        let mut offset = HEADER_SIZE;

        for _ in 0..num_entries {
            // Read key_len (4 bytes).
            if offset + 4 > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing key_len".to_string(),
                ));
            }
            let key_len =
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            // Read key (key_len bytes).
            if offset + key_len > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing key data".to_string(),
                ));
            }
            let key = bytes[offset..offset + key_len].to_vec();
            offset += key_len;

            // Read trained_as (1 byte).
            if offset + 1 > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing trained_as".to_string(),
                ));
            }
            let trained_as = decode_trained_as(bytes[offset])?;
            offset += 1;

            // Read classification (1 byte).
            if offset + 1 > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing classification".to_string(),
                ));
            }
            let classification = decode_classification(bytes[offset])?;
            offset += 1;

            // Read score (8 bytes f64 LE).
            if offset + 8 > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing score".to_string(),
                ));
            }
            let score_bits = f64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            let score = if score_bits.is_nan() {
                None
            } else {
                Some(score_bits)
            };
            offset += 8;

            // Read message_id (length-prefixed UTF-8).
            if offset + 4 > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing msg_id_len".to_string(),
                ));
            }
            let msg_id_len =
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + msg_id_len > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing message_id data".to_string(),
                ));
            }
            let message_id = String::from_utf8(bytes[offset..offset + msg_id_len].to_vec())
                .map_err(|e| StorageError::Deserialize(format!("invalid UTF-8 in message_id: {e}")))?;
            offset += msg_id_len;

            data.insert(
                key,
                MessageInfo {
                    trained_as,
                    classification,
                    score,
                    message_id,
                    original_folder: None,
                },
            );
        }

        Ok(data)
    }

    /// Serialize the message data into the binary format.
    fn serialize(data: &HashMap<Vec<u8>, MessageInfo>) -> Vec<u8> {
        // Estimate capacity: header + entries * (4 + avg_key + 1 + 1 + 8 + 4 + avg_id).
        let estimated = HEADER_SIZE + data.len() * 40;
        let mut buf = Vec::with_capacity(estimated);

        // Header.
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u64).to_le_bytes());

        // Entries.
        for (key, info) in data {
            // Key (length-prefixed).
            buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
            buf.extend_from_slice(key);

            // trained_as: u8.
            buf.push(encode_trained_as(info.trained_as));

            // classification: u8.
            buf.push(encode_classification(info.classification));

            // score: f64 LE (NaN sentinel for None).
            let score_bits = info.score.unwrap_or(SCORE_NONE_SENTINEL);
            buf.extend_from_slice(&score_bits.to_le_bytes());

            // message_id (length-prefixed UTF-8).
            let id_bytes = info.message_id.as_bytes();
            buf.extend_from_slice(&(id_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(id_bytes);
        }

        buf
    }
}

impl MessageDatabase for MmapMessageDb {
    fn load_msg(&self, search_key: &[u8]) -> Option<MessageInfo> {
        self.data.get(search_key).cloned()
    }

    fn store_msg(&mut self, search_key: &[u8], info: &MessageInfo) {
        self.data.insert(search_key.to_vec(), info.clone());
    }

    fn remove_msg(&mut self, search_key: &[u8]) {
        self.data.remove(search_key);
    }
}

// ─── Encoding/Decoding Helpers ───────────────────────────────────────────────

/// Encode `trained_as` to a u8 value.
/// - `None` → 0 (untrained)
/// - `Some(false)` → 1 (ham)
/// - `Some(true)` → 2 (spam)
fn encode_trained_as(trained_as: Option<bool>) -> u8 {
    match trained_as {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}

/// Decode a u8 value to `trained_as`.
fn decode_trained_as(byte: u8) -> Result<Option<bool>, StorageError> {
    match byte {
        0 => Ok(None),
        1 => Ok(Some(false)),
        2 => Ok(Some(true)),
        other => Err(StorageError::Corrupted(format!(
            "invalid trained_as value: {other}"
        ))),
    }
}

/// Encode `Classification` to a u8 value.
/// - `None` → 0
/// - `Some(Ham)` → 1
/// - `Some(Spam)` → 2
/// - `Some(Unsure)` → 3
fn encode_classification(classification: Option<Classification>) -> u8 {
    match classification {
        None => 0,
        Some(Classification::Ham) => 1,
        Some(Classification::Spam) => 2,
        Some(Classification::Unsure) => 3,
    }
}

/// Decode a u8 value to `Option<Classification>`.
fn decode_classification(byte: u8) -> Result<Option<Classification>, StorageError> {
    match byte {
        0 => Ok(None),
        1 => Ok(Some(Classification::Ham)),
        2 => Ok(Some(Classification::Spam)),
        3 => Ok(Some(Classification::Unsure)),
        other => Err(StorageError::Corrupted(format!(
            "invalid classification value: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper to create a database with a temp directory path.
    fn temp_db() -> (MmapMessageDb, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("messages.db");
        let db = MmapMessageDb::new(path);
        (db, dir)
    }

    #[test]
    fn new_empty_database_has_no_entries() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();
        assert!(db.is_empty());
        assert_eq!(db.len(), 0);
    }

    #[test]
    fn store_and_load_single_message() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info = MessageInfo {
            trained_as: Some(true),
            classification: Some(Classification::Spam),
            score: Some(0.95),
            message_id: "msg-001@example.com".to_string(),
            original_folder: None,
        };

        db.store_msg(b"search_key_1", &info);
        db.save_to_file().unwrap();

        // Reload from disk.
        db.load_from_file().unwrap();
        let loaded = db.load_msg(b"search_key_1").unwrap();

        assert_eq!(loaded.trained_as, Some(true));
        assert_eq!(loaded.classification, Some(Classification::Spam));
        assert!((loaded.score.unwrap() - 0.95).abs() < f64::EPSILON);
        assert_eq!(loaded.message_id, "msg-001@example.com");
    }

    #[test]
    fn store_multiple_messages_and_reload() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info1 = MessageInfo {
            trained_as: Some(false),
            classification: Some(Classification::Ham),
            score: Some(0.1),
            message_id: "ham-msg@example.com".to_string(),
            original_folder: None,
        };
        let info2 = MessageInfo {
            trained_as: None,
            classification: Some(Classification::Unsure),
            score: Some(0.5),
            message_id: "unsure-msg@example.com".to_string(),
            original_folder: None,
        };
        let info3 = MessageInfo {
            trained_as: Some(true),
            classification: None,
            score: None,
            message_id: "no-score@example.com".to_string(),
            original_folder: None,
        };

        db.store_msg(b"key_a", &info1);
        db.store_msg(b"key_b", &info2);
        db.store_msg(b"key_c", &info3);
        db.save_to_file().unwrap();

        // Reload and verify all entries.
        db.load_from_file().unwrap();
        assert_eq!(db.len(), 3);

        let loaded1 = db.load_msg(b"key_a").unwrap();
        assert_eq!(loaded1.trained_as, Some(false));
        assert_eq!(loaded1.classification, Some(Classification::Ham));

        let loaded2 = db.load_msg(b"key_b").unwrap();
        assert_eq!(loaded2.trained_as, None);
        assert_eq!(loaded2.classification, Some(Classification::Unsure));

        let loaded3 = db.load_msg(b"key_c").unwrap();
        assert_eq!(loaded3.trained_as, Some(true));
        assert_eq!(loaded3.classification, None);
        assert!(loaded3.score.is_none());
    }

    #[test]
    fn remove_message_removes_entry() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info = MessageInfo {
            trained_as: Some(true),
            classification: Some(Classification::Spam),
            score: Some(0.99),
            message_id: "to-remove@example.com".to_string(),
            original_folder: None,
        };

        db.store_msg(b"remove_me", &info);
        assert_eq!(db.len(), 1);

        db.remove_msg(b"remove_me");
        assert_eq!(db.len(), 0);
        assert!(db.load_msg(b"remove_me").is_none());
    }

    #[test]
    fn remove_nonexistent_key_is_noop() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();
        db.remove_msg(b"does_not_exist");
        assert!(db.is_empty());
    }

    #[test]
    fn update_existing_message() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info1 = MessageInfo {
            trained_as: None,
            classification: Some(Classification::Unsure),
            score: Some(0.5),
            message_id: "update-me@example.com".to_string(),
            original_folder: None,
        };
        db.store_msg(b"update_key", &info1);

        // Update with new data.
        let info2 = MessageInfo {
            trained_as: Some(true),
            classification: Some(Classification::Spam),
            score: Some(0.98),
            message_id: "update-me@example.com".to_string(),
            original_folder: None,
        };
        db.store_msg(b"update_key", &info2);

        let loaded = db.load_msg(b"update_key").unwrap();
        assert_eq!(loaded.trained_as, Some(true));
        assert_eq!(loaded.classification, Some(Classification::Spam));
        assert!((loaded.score.unwrap() - 0.98).abs() < f64::EPSILON);
    }

    #[test]
    fn corrupted_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.db");

        // Write garbage data.
        fs::write(&path, b"this is not a valid message database").unwrap();

        let mut db = MmapMessageDb::new(path);
        db.load_from_file().unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn wrong_magic_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad_magic.db");

        let mut data = vec![0u8; HEADER_SIZE];
        data[0..4].copy_from_slice(b"XXXX");
        fs::write(&path, &data).unwrap();

        let mut db = MmapMessageDb::new(path);
        db.load_from_file().unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn truncated_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("truncated.db");

        // Write partial header.
        fs::write(&path, b"SBMM\x01\x00").unwrap();

        let mut db = MmapMessageDb::new(path);
        db.load_from_file().unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn empty_file_treated_as_fresh() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.db");

        fs::write(&path, b"").unwrap();

        let mut db = MmapMessageDb::new(path);
        db.load_from_file().unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn binary_keys_round_trip() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        // Use binary search keys (like real PR_SEARCH_KEY values).
        let binary_key = vec![0x00, 0xFF, 0xAB, 0xCD, 0x12, 0x34, 0x56, 0x78];
        let info = MessageInfo {
            trained_as: Some(false),
            classification: Some(Classification::Ham),
            score: Some(0.05),
            message_id: "binary-key-msg@test.com".to_string(),
            original_folder: None,
        };

        db.store_msg(&binary_key, &info);
        db.save_to_file().unwrap();

        db.load_from_file().unwrap();
        let loaded = db.load_msg(&binary_key).unwrap();
        assert_eq!(loaded.message_id, "binary-key-msg@test.com");
    }

    #[test]
    fn score_none_round_trips() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info = MessageInfo {
            trained_as: None,
            classification: None,
            score: None,
            message_id: "no-score@example.com".to_string(),
            original_folder: None,
        };

        db.store_msg(b"no_score_key", &info);
        db.save_to_file().unwrap();

        db.load_from_file().unwrap();
        let loaded = db.load_msg(b"no_score_key").unwrap();
        assert!(loaded.score.is_none());
        assert!(loaded.trained_as.is_none());
        assert!(loaded.classification.is_none());
    }

    #[test]
    fn score_zero_round_trips_correctly() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info = MessageInfo {
            trained_as: Some(false),
            classification: Some(Classification::Ham),
            score: Some(0.0),
            message_id: "zero-score@example.com".to_string(),
            original_folder: None,
        };

        db.store_msg(b"zero_score", &info);
        db.save_to_file().unwrap();

        db.load_from_file().unwrap();
        let loaded = db.load_msg(b"zero_score").unwrap();
        assert_eq!(loaded.score, Some(0.0));
    }

    #[test]
    fn unicode_message_id_round_trips() {
        let (mut db, _dir) = temp_db();
        db.load_from_file().unwrap();

        let info = MessageInfo {
            trained_as: None,
            classification: None,
            score: None,
            message_id: "café-日本語@example.com".to_string(),
            original_folder: None,
        };

        db.store_msg(b"unicode_key", &info);
        db.save_to_file().unwrap();

        db.load_from_file().unwrap();
        let loaded = db.load_msg(b"unicode_key").unwrap();
        assert_eq!(loaded.message_id, "café-日本語@example.com");
    }

    #[test]
    fn invalid_trained_as_byte_detected() {
        // Build a valid file but with an invalid trained_as byte.
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes()); // 1 entry

        // Key.
        let key = b"test_key";
        buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buf.extend_from_slice(key);

        // Invalid trained_as = 99.
        buf.push(99);
        // classification = 0 (valid).
        buf.push(0);
        // score = NaN.
        buf.extend_from_slice(&f64::NAN.to_le_bytes());
        // message_id = "".
        buf.extend_from_slice(&0u32.to_le_bytes());

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("invalid_trained.db");
        fs::write(&path, &buf).unwrap();

        let mut db = MmapMessageDb::new(path);
        // Should gracefully handle corrupt data.
        db.load_from_file().unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn invalid_classification_byte_detected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes()); // 1 entry

        // Key.
        let key = b"test_key";
        buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buf.extend_from_slice(key);

        // trained_as = 0 (valid).
        buf.push(0);
        // Invalid classification = 99.
        buf.push(99);
        // score = NaN.
        buf.extend_from_slice(&f64::NAN.to_le_bytes());
        // message_id = "".
        buf.extend_from_slice(&0u32.to_le_bytes());

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("invalid_class.db");
        fs::write(&path, &buf).unwrap();

        let mut db = MmapMessageDb::new(path);
        // Should gracefully handle corrupt data.
        db.load_from_file().unwrap();
        assert!(db.is_empty());
    }

    #[test]
    fn persist_and_reload_survives_process_restart() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("persist.db");

        // First "process": create and save.
        {
            let mut db = MmapMessageDb::new(path.clone());
            db.load_from_file().unwrap();

            let info = MessageInfo {
                trained_as: Some(true),
                classification: Some(Classification::Spam),
                score: Some(0.87),
                message_id: "persist-test@example.com".to_string(),
                original_folder: None,
            };
            db.store_msg(b"persist_key", &info);
            db.save_to_file().unwrap();
        }

        // Second "process": load from same file.
        {
            let mut db = MmapMessageDb::new(path);
            db.load_from_file().unwrap();
            assert_eq!(db.len(), 1);

            let loaded = db.load_msg(b"persist_key").unwrap();
            assert_eq!(loaded.trained_as, Some(true));
            assert_eq!(loaded.classification, Some(Classification::Spam));
            assert!((loaded.score.unwrap() - 0.87).abs() < f64::EPSILON);
            assert_eq!(loaded.message_id, "persist-test@example.com");
        }
    }
}
