//! Memory-mapped dbm-compatible storage backend.
//!
//! Provides [`MmapDbmBackend`], a persistent key-value store for classifier
//! token data using a custom binary format with memory-mapped file I/O.
//!
//! # File Format
//!
//! ```text
//! [Magic: 4 bytes "SBDB"]
//! [Version: u32 LE]
//! [nspam: u64 LE]
//! [nham: u64 LE]
//! [num_entries: u64 LE]
//! For each entry:
//!   [key_len: u32 LE]
//!   [key: key_len bytes]
//!   [spam_count: u32 LE]
//!   [ham_count: u32 LE]
//! ```
//!
//! On store, the full database is rewritten atomically via [`safe_write`](crate::safe_write)
//! to guarantee crash safety. Only keys present in the `changed` map are merged
//! into the in-memory data before serialization.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use spambayes_core::WordInfo;

use crate::safe_write::safe_write;
use crate::traits::{ClassifierState, StorageBackend, WordChange};
use crate::StorageError;

/// Magic bytes identifying a `SpamBayes` database file.
const MAGIC: &[u8; 4] = b"SBDB";

/// Size of the file header: magic(4) + version(4) + nspam(8) + nham(8) + `num_entries(8)` = 32.
const HEADER_SIZE: usize = 32;

/// Memory-mapped dbm-compatible storage backend.
///
/// Stores classifier token data in a custom binary format. Uses `memmap2` for
/// efficient reads and [`safe_write`] for crash-safe persistence.
///
/// Tracks dirty keys so that only changed tokens need to be merged before
/// a full serialization pass.
pub struct MmapDbmBackend {
    /// Path to the database file on disk.
    path: PathBuf,
    /// In-memory token data loaded from the file.
    data: HashMap<Vec<u8>, WordInfo>,
    /// Set of keys that have been modified since the last save.
    dirty_keys: HashSet<Vec<u8>>,
    /// Whether the backend has been closed.
    closed: bool,
}

impl MmapDbmBackend {
    /// Create a new backend targeting the given file path.
    ///
    /// The file is not read until [`StorageBackend::load`] is called.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            data: HashMap::new(),
            dirty_keys: HashSet::new(),
            closed: false,
        }
    }

    /// Returns the path to the backing database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the number of token entries currently held in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if there are no token entries in memory.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns a reference to the in-memory token data.
    ///
    /// This is populated after [`StorageBackend::load`] is called.
    #[must_use]
    pub fn data(&self) -> &HashMap<Vec<u8>, WordInfo> {
        &self.data
    }

    /// Parse the database file contents (from a memory-mapped region or byte slice).
    ///
    /// Returns the classifier state and a map of all token entries.
    /// Returns `Err(StorageError::Corrupted)` if the data is malformed.
    fn parse(bytes: &[u8]) -> Result<(ClassifierState, HashMap<Vec<u8>, WordInfo>), StorageError> {
        if bytes.len() < HEADER_SIZE {
            return Err(StorageError::Corrupted(
                "file too short for header".to_string(),
            ));
        }

        // Validate magic.
        if &bytes[0..4] != MAGIC {
            return Err(StorageError::Corrupted(format!(
                "invalid magic: expected SBDB, got {:?}",
                &bytes[0..4]
            )));
        }

        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let nspam = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let nham = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let num_entries = u64::from_le_bytes(bytes[24..32].try_into().unwrap());

        let state = ClassifierState {
            nspam,
            nham,
            version,
        };

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

            // Read spam_count + ham_count (8 bytes).
            if offset + 8 > bytes.len() {
                return Err(StorageError::Corrupted(
                    "truncated entry: missing word counts".to_string(),
                ));
            }
            let spam_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let ham_count =
                u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            offset += 8;

            data.insert(key, WordInfo { spam_count, ham_count });
        }

        Ok((state, data))
    }

    /// Serialize the current state and token data into the binary format.
    fn serialize(state: &ClassifierState, data: &HashMap<Vec<u8>, WordInfo>) -> Vec<u8> {
        // Estimate capacity: header + entries * (4 + avg_key_len + 8).
        let estimated = HEADER_SIZE + data.len() * 20;
        let mut buf = Vec::with_capacity(estimated);

        // Header.
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&state.version.to_le_bytes());
        buf.extend_from_slice(&state.nspam.to_le_bytes());
        buf.extend_from_slice(&state.nham.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u64).to_le_bytes());

        // Entries.
        for (key, info) in data {
            buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
            buf.extend_from_slice(key);
            buf.extend_from_slice(&info.spam_count.to_le_bytes());
            buf.extend_from_slice(&info.ham_count.to_le_bytes());
        }

        buf
    }
}

impl StorageBackend for MmapDbmBackend {
    fn load(&mut self) -> Result<ClassifierState, StorageError> {
        self.closed = false;
        self.dirty_keys.clear();

        // If the file doesn't exist, return an empty state.
        if !self.path.exists() {
            self.data = HashMap::new();
            return Ok(ClassifierState::default());
        }

        // Attempt to memory-map and parse the file.
        let result = (|| -> Result<(ClassifierState, HashMap<Vec<u8>, WordInfo>), StorageError> {
            let file = fs::File::open(&self.path)?;
            let metadata = file.metadata()?;

            if metadata.len() == 0 {
                // Empty file — treat as fresh database.
                return Ok((ClassifierState::default(), HashMap::new()));
            }

            // SAFETY: The file is opened read-only and we hold the File handle.
            // The mmap is only used within this scope for parsing.
            let mmap = unsafe { Mmap::map(&file) }?;
            Self::parse(&mmap)
        })();

        match result {
            Ok((state, data)) => {
                self.data = data;
                Ok(state)
            }
            Err(StorageError::Corrupted(msg)) => {
                // Log error and initialize empty database.
                eprintln!(
                    "spambayes-storage: corrupted database at '{}': {}. Initializing empty.",
                    self.path.display(),
                    msg
                );
                self.data = HashMap::new();
                Ok(ClassifierState::default())
            }
            Err(e) => Err(e),
        }
    }

    fn store(
        &mut self,
        state: &ClassifierState,
        changed: &HashMap<Vec<u8>, WordChange>,
    ) -> Result<(), StorageError> {
        if self.closed {
            return Err(StorageError::Io(std::io::Error::other(
                "backend is closed",
            )));
        }

        // Apply changes to in-memory data and track dirty keys.
        for (key, change) in changed {
            match change {
                WordChange::Updated(info) => {
                    self.data.insert(key.clone(), *info);
                    self.dirty_keys.insert(key.clone());
                }
                WordChange::Removed => {
                    self.data.remove(key);
                    self.dirty_keys.insert(key.clone());
                }
            }
        }

        // Serialize and write atomically.
        let bytes = Self::serialize(state, &self.data);
        safe_write(&self.path, &bytes)?;

        // Clear dirty tracking after successful write.
        self.dirty_keys.clear();

        Ok(())
    }

    fn close(&mut self) -> Result<(), StorageError> {
        self.closed = true;
        self.data.clear();
        self.dirty_keys.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper to create a backend with a temp directory path.
    fn temp_backend() -> (MmapDbmBackend, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let backend = MmapDbmBackend::new(path);
        (backend, dir)
    }

    #[test]
    fn new_empty_database_returns_default_state() {
        let (mut backend, _dir) = temp_backend();

        let state = backend.load().unwrap();
        assert_eq!(state, ClassifierState::default());
        assert!(backend.is_empty());
    }

    #[test]
    fn store_and_reload_state_and_tokens() {
        let (mut backend, _dir) = temp_backend();

        // Load fresh state.
        backend.load().unwrap();

        // Store some tokens.
        let state = ClassifierState {
            nspam: 100,
            nham: 200,
            version: 2,
        };
        let mut changed = HashMap::new();
        changed.insert(
            b"hello".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 5,
                ham_count: 10,
            }),
        );
        changed.insert(
            b"world".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 3,
                ham_count: 7,
            }),
        );

        backend.store(&state, &changed).unwrap();

        // Reload from disk.
        let loaded_state = backend.load().unwrap();
        assert_eq!(loaded_state.nspam, 100);
        assert_eq!(loaded_state.nham, 200);
        assert_eq!(loaded_state.version, 2);
        assert_eq!(backend.len(), 2);

        assert_eq!(
            backend.data.get(b"hello".as_slice()),
            Some(&WordInfo {
                spam_count: 5,
                ham_count: 10
            })
        );
        assert_eq!(
            backend.data.get(b"world".as_slice()),
            Some(&WordInfo {
                spam_count: 3,
                ham_count: 7
            })
        );
    }

    #[test]
    fn incremental_updates_only_changed_keys() {
        let (mut backend, _dir) = temp_backend();
        backend.load().unwrap();

        // First store: add two tokens.
        let state = ClassifierState {
            nspam: 10,
            nham: 20,
            version: 1,
        };
        let mut changed = HashMap::new();
        changed.insert(
            b"alpha".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 1,
                ham_count: 2,
            }),
        );
        changed.insert(
            b"beta".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 3,
                ham_count: 4,
            }),
        );
        backend.store(&state, &changed).unwrap();

        // Second store: update one, remove one, add one.
        let state2 = ClassifierState {
            nspam: 11,
            nham: 21,
            version: 1,
        };
        let mut changed2 = HashMap::new();
        changed2.insert(
            b"alpha".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 10,
                ham_count: 20,
            }),
        );
        changed2.insert(b"beta".to_vec(), WordChange::Removed);
        changed2.insert(
            b"gamma".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 5,
                ham_count: 6,
            }),
        );
        backend.store(&state2, &changed2).unwrap();

        // Reload and verify.
        let loaded_state = backend.load().unwrap();
        assert_eq!(loaded_state.nspam, 11);
        assert_eq!(loaded_state.nham, 21);
        assert_eq!(backend.len(), 2); // alpha + gamma, beta removed

        assert_eq!(
            backend.data.get(b"alpha".as_slice()),
            Some(&WordInfo {
                spam_count: 10,
                ham_count: 20
            })
        );
        assert!(!backend.data.contains_key(b"beta".as_slice()));
        assert_eq!(
            backend.data.get(b"gamma".as_slice()),
            Some(&WordInfo {
                spam_count: 5,
                ham_count: 6
            })
        );
    }

    #[test]
    fn corrupted_file_returns_empty_state() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.db");

        // Write garbage data.
        fs::write(&path, b"this is not a valid database file").unwrap();

        let mut backend = MmapDbmBackend::new(path);
        let state = backend.load().unwrap();

        // Should gracefully return default state.
        assert_eq!(state, ClassifierState::default());
        assert!(backend.is_empty());
    }

    #[test]
    fn corrupted_magic_returns_empty_state() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad_magic.db");

        // Write a header-sized block with wrong magic.
        let mut data = vec![0u8; HEADER_SIZE];
        data[0..4].copy_from_slice(b"XXXX");
        fs::write(&path, &data).unwrap();

        let mut backend = MmapDbmBackend::new(path);
        let state = backend.load().unwrap();
        assert_eq!(state, ClassifierState::default());
        assert!(backend.is_empty());
    }

    #[test]
    fn truncated_file_returns_empty_state() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("truncated.db");

        // Write just the magic + partial header.
        fs::write(&path, b"SBDB\x01\x00").unwrap();

        let mut backend = MmapDbmBackend::new(path);
        let state = backend.load().unwrap();
        assert_eq!(state, ClassifierState::default());
        assert!(backend.is_empty());
    }

    #[test]
    fn close_releases_resources() {
        let (mut backend, _dir) = temp_backend();
        backend.load().unwrap();

        let state = ClassifierState {
            nspam: 5,
            nham: 10,
            version: 1,
        };
        let mut changed = HashMap::new();
        changed.insert(
            b"token".to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 1,
                ham_count: 2,
            }),
        );
        backend.store(&state, &changed).unwrap();
        assert_eq!(backend.len(), 1);

        // Close should clear in-memory data.
        backend.close().unwrap();
        assert!(backend.is_empty());
        assert!(backend.closed);

        // Store after close should error.
        let result = backend.store(&state, &changed);
        assert!(result.is_err());
    }

    #[test]
    fn empty_file_treated_as_fresh_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.db");

        // Create an empty file.
        fs::write(&path, b"").unwrap();

        let mut backend = MmapDbmBackend::new(path);
        let state = backend.load().unwrap();
        assert_eq!(state, ClassifierState::default());
        assert!(backend.is_empty());
    }

    #[test]
    fn unicode_keys_round_trip() {
        let (mut backend, _dir) = temp_backend();
        backend.load().unwrap();

        let state = ClassifierState::default();
        let mut changed = HashMap::new();
        // UTF-8 keys with various character sets.
        changed.insert(
            "café".as_bytes().to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 2,
                ham_count: 3,
            }),
        );
        changed.insert(
            "日本語".as_bytes().to_vec(),
            WordChange::Updated(WordInfo {
                spam_count: 1,
                ham_count: 1,
            }),
        );

        backend.store(&state, &changed).unwrap();

        // Reload and verify.
        backend.load().unwrap();
        assert_eq!(backend.len(), 2);
        assert_eq!(
            backend.data.get("café".as_bytes()),
            Some(&WordInfo {
                spam_count: 2,
                ham_count: 3
            })
        );
        assert_eq!(
            backend.data.get("日本語".as_bytes()),
            Some(&WordInfo {
                spam_count: 1,
                ham_count: 1
            })
        );
    }
}
