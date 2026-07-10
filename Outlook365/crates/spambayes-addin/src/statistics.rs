//! Statistics tracking for the SpamBayes add-in.
//!
//! Provides session-level counters (reset on each Outlook start) and
//! lifetime counters (persisted to a JSON file across sessions).
//!
//! **Validates: Requirements 1.1, 1.2**

use serde::{Deserialize, Serialize};
use spambayes_core::Classification;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ─── Session Statistics ──────────────────────────────────────────────────────

/// Counters tracking activity within the current Outlook session.
///
/// These are reset to zero each time the add-in initializes.
///
/// **Validates: Requirement 1.2**
#[derive(Default, Clone, Debug)]
pub struct SessionStats {
    pub ham_classified: u32,
    pub unsure_classified: u32,
    pub spam_classified: u32,
    pub ham_trained: u32,
    pub spam_trained: u32,
    /// Messages where the user confirmed the classification was correct.
    pub correctly_classified: u32,
    /// Ham/unsure messages the user reclassified as spam (false negatives).
    pub false_negatives: u32,
    /// Spam/unsure messages the user reclassified as ham (false positives).
    pub false_positives: u32,
}

// ─── Lifetime Statistics ─────────────────────────────────────────────────────

/// Counters that accumulate across all Outlook sessions and are persisted to disk.
///
/// Uses `u64` to avoid overflow for users who process large volumes of email
/// over the lifetime of the installation.
///
/// **Validates: Requirement 1.1**
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct LifetimeStats {
    pub total_ham_trained: u64,
    pub total_spam_trained: u64,
    pub total_ham_classified: u64,
    pub total_unsure_classified: u64,
    pub total_spam_classified: u64,
    /// Lifetime count of messages confirmed as correctly classified.
    #[serde(default)]
    pub correctly_classified: u64,
    /// Lifetime count of false negatives (spam that was classified as ham/unsure).
    #[serde(default)]
    pub false_negatives: u64,
    /// Lifetime count of false positives (ham that was classified as spam/unsure).
    #[serde(default)]
    pub false_positives: u64,
}

// ─── Stats File ──────────────────────────────────────────────────────────────

/// Wrapper struct for the on-disk JSON format.
///
/// The `version` field provides forward-compatible versioning so future
/// changes to the statistics format can be handled gracefully.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsFile {
    pub version: u32,
    pub lifetime: LifetimeStats,
}

impl Default for StatsFile {
    fn default() -> Self {
        Self {
            version: 1,
            lifetime: LifetimeStats::default(),
        }
    }
}

// ─── Statistics Manager ──────────────────────────────────────────────────────

/// Internal mutable state protected by the mutex.
struct StatisticsInner {
    session: SessionStats,
    lifetime: LifetimeStats,
    file_path: PathBuf,
    dirty_count: u32,
    save_interval: u32,
}

/// Thread-safe statistics manager.
///
/// Tracks both session and lifetime counters, auto-saving to disk when the
/// dirty count reaches the configured save interval.
///
/// **Validates: Requirements 1.3, 1.4, 1.5, 1.6, 4.1, 4.4**
#[derive(Clone)]
pub struct StatisticsManager {
    inner: Arc<Mutex<StatisticsInner>>,
}

impl StatisticsManager {
    /// Create a new statistics manager, loading lifetime stats from file if it exists.
    ///
    /// Session counters always start at zero (Requirement 1.6).
    /// If the file is missing or corrupt, lifetime counters start at zero.
    /// If the file does not exist yet, it is created immediately with zeros
    /// so that external tools (e.g., the Manager GUI) always have a valid file
    /// to read.
    pub fn new(data_directory: &Path, save_interval: u32) -> Self {
        let file_path = data_directory.join("spambayes_stats.json");

        let (lifetime, file_existed) = Self::load_from_file(&file_path);

        let mut inner = StatisticsInner {
            session: SessionStats::default(),
            lifetime,
            file_path,
            dirty_count: 0,
            save_interval,
        };

        // Ensure the file exists on disk even when starting fresh.
        if !file_existed {
            Self::save_inner(&mut inner);
        }

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Attempt to load lifetime statistics from the JSON file.
    /// Returns (stats, file_existed): default zeros if missing or corrupt,
    /// and a bool indicating whether a valid file was found on disk.
    fn load_from_file(file_path: &Path) -> (LifetimeStats, bool) {
        match std::fs::read_to_string(file_path) {
            Ok(contents) => match serde_json::from_str::<StatsFile>(&contents) {
                Ok(stats_file) => (stats_file.lifetime, true),
                Err(e) => {
                    eprintln!(
                        "Warning: statistics file is corrupt ({}), starting fresh: {}",
                        file_path.display(),
                        e
                    );
                    (LifetimeStats::default(), false)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist yet — normal for first run.
                (LifetimeStats::default(), false)
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not read statistics file ({}): {}",
                    file_path.display(),
                    e
                );
                (LifetimeStats::default(), false)
            }
        }
    }

    /// Record a classification event.
    ///
    /// Increments the appropriate session and lifetime counter, then triggers
    /// an auto-save if `dirty_count` reaches `save_interval`.
    ///
    /// **Validates: Requirement 1.5**
    pub fn on_classified(&self, classification: Classification) {
        let should_save = {
            let mut inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    eprintln!("Error: statistics lock poisoned in on_classified: {}", e);
                    return;
                }
            };

            match classification {
                Classification::Ham => {
                    inner.session.ham_classified += 1;
                    inner.lifetime.total_ham_classified += 1;
                }
                Classification::Spam => {
                    inner.session.spam_classified += 1;
                    inner.lifetime.total_spam_classified += 1;
                }
                Classification::Unsure => {
                    inner.session.unsure_classified += 1;
                    inner.lifetime.total_unsure_classified += 1;
                }
            }

            inner.dirty_count += 1;

            if inner.dirty_count >= inner.save_interval {
                // Save while still holding the lock (file I/O is infrequent).
                Self::save_inner(&mut inner);
                false
            } else {
                false
            }
        };

        // `should_save` is currently unused but keeps the pattern extensible
        // if we later move I/O outside the lock.
        let _ = should_save;
    }

    /// Record a training event.
    ///
    /// **Validates: Requirements 1.3, 1.4**
    pub fn on_trained(&self, is_spam: bool) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in on_trained: {}", e);
                return;
            }
        };

        if is_spam {
            inner.session.spam_trained += 1;
            inner.lifetime.total_spam_trained += 1;
        } else {
            inner.session.ham_trained += 1;
            inner.lifetime.total_ham_trained += 1;
        }
    }

    /// Record an untrain event, decrementing the lifetime training counter.
    ///
    /// Uses saturating subtraction to floor at zero.
    pub fn on_untrained(&self, was_spam: bool) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in on_untrained: {}", e);
                return;
            }
        };

        if was_spam {
            inner.lifetime.total_spam_trained = inner.lifetime.total_spam_trained.saturating_sub(1);
        } else {
            inner.lifetime.total_ham_trained = inner.lifetime.total_ham_trained.saturating_sub(1);
        }
    }

    /// Record a user correction event for accuracy tracking.
    ///
    /// Called when the user manually reclassifies a message (via Spam/Not Spam
    /// buttons or drag-to-folder). The `original_classification` is what
    /// SpamBayes assigned, and `user_says_spam` is the user's judgment.
    ///
    /// - If the original classification matches the user's judgment, it counts
    ///   as "correctly classified."
    /// - If the message was classified as ham/unsure but the user says spam,
    ///   it's a false negative.
    /// - If the message was classified as spam/unsure but the user says ham,
    ///   it's a false positive.
    ///
    /// **Validates: Requirements 4.2, 4.4**
    pub fn on_correction(&self, original_classification: Classification, user_says_spam: bool) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in on_correction: {}", e);
                return;
            }
        };

        match (original_classification, user_says_spam) {
            // User confirms spam was correctly classified as spam.
            (Classification::Spam, true) => {
                inner.session.correctly_classified += 1;
                inner.lifetime.correctly_classified += 1;
            }
            // User confirms ham was correctly classified as ham.
            (Classification::Ham, false) => {
                inner.session.correctly_classified += 1;
                inner.lifetime.correctly_classified += 1;
            }
            // Classified as ham or unsure, but user says it's spam → false negative.
            (Classification::Ham | Classification::Unsure, true) => {
                inner.session.false_negatives += 1;
                inner.lifetime.false_negatives += 1;
            }
            // Classified as spam or unsure, but user says it's ham → false positive.
            (Classification::Spam | Classification::Unsure, false) => {
                inner.session.false_positives += 1;
                inner.lifetime.false_positives += 1;
            }
        }

        inner.dirty_count += 1;
        if inner.dirty_count >= inner.save_interval {
            Self::save_inner(&mut inner);
        }
    }

    /// Get a snapshot of the current session statistics.
    pub fn session_stats(&self) -> SessionStats {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in session_stats: {}", e);
                return SessionStats::default();
            }
        };
        inner.session.clone()
    }

    /// Get a snapshot of the current lifetime statistics.
    pub fn lifetime_stats(&self) -> LifetimeStats {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in lifetime_stats: {}", e);
                return LifetimeStats::default();
            }
        };
        inner.lifetime.clone()
    }

    /// Reset all lifetime statistics to zero and save immediately.
    pub fn reset_lifetime(&self) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in reset_lifetime: {}", e);
                return;
            }
        };
        inner.lifetime = LifetimeStats::default();
        Self::save_inner(&mut inner);
    }

    /// Reset session statistics to zero.
    pub fn reset_session(&self) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in reset_session: {}", e);
                return;
            }
        };
        inner.session = SessionStats::default();
    }

    /// Force save to disk (e.g., called on shutdown).
    pub fn save(&self) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Error: statistics lock poisoned in save: {}", e);
                return;
            }
        };
        Self::save_inner(&mut inner);
    }

    /// Maximum number of retry attempts when the file is locked by another process.
    const SAVE_MAX_RETRIES: u32 = 5;

    /// Initial delay between retries (doubles on each attempt).
    const SAVE_INITIAL_DELAY_MS: u64 = 50;

    /// Internal save implementation. Writes atomically via temp file + rename.
    /// Resets `dirty_count` on success. Logs warnings on failure.
    ///
    /// If the file is held open by another process (e.g., the Manager GUI reading
    /// stats), this retries with exponential backoff up to ~1.5 seconds total
    /// before giving up. The in-memory counters remain correct regardless and
    /// will be persisted on the next successful save.
    fn save_inner(inner: &mut StatisticsInner) {
        let stats_file = StatsFile {
            version: 1,
            lifetime: inner.lifetime.clone(),
        };

        let json = match serde_json::to_string_pretty(&stats_file) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("Warning: failed to serialize statistics: {}", e);
                return;
            }
        };

        let temp_path = inner.file_path.with_extension("json.tmp");

        // Write to temp file with retry on sharing violations.
        if !Self::retry_io(Self::SAVE_MAX_RETRIES, Self::SAVE_INITIAL_DELAY_MS, || {
            std::fs::write(&temp_path, json.as_bytes())
        }) {
            eprintln!(
                "Warning: failed to write statistics temp file after {} retries ({})",
                Self::SAVE_MAX_RETRIES,
                temp_path.display(),
            );
            return;
        }

        // Rename temp → final with retry on sharing violations.
        if !Self::retry_io(Self::SAVE_MAX_RETRIES, Self::SAVE_INITIAL_DELAY_MS, || {
            std::fs::rename(&temp_path, &inner.file_path)
        }) {
            eprintln!(
                "Warning: failed to rename statistics file after {} retries ({} -> {})",
                Self::SAVE_MAX_RETRIES,
                temp_path.display(),
                inner.file_path.display(),
            );
            // Try to clean up the temp file.
            let _ = std::fs::remove_file(&temp_path);
            return;
        }

        inner.dirty_count = 0;
    }

    /// Retry an I/O operation that may fail with a sharing violation or
    /// permission error because another process holds the file open.
    ///
    /// Uses exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms (total ~1.55s).
    /// Returns `true` if the operation eventually succeeded, `false` if all
    /// retries were exhausted.
    fn retry_io<F>(max_retries: u32, initial_delay_ms: u64, mut op: F) -> bool
    where
        F: FnMut() -> std::io::Result<()>,
    {
        let mut delay_ms = initial_delay_ms;

        for attempt in 0..=max_retries {
            match op() {
                Ok(()) => return true,
                Err(ref e) if Self::is_sharing_violation(e) && attempt < max_retries => {
                    // File is locked — wait and retry.
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = delay_ms.saturating_mul(2);
                }
                Err(_) => {
                    // Non-retryable error or final attempt — give up.
                    return false;
                }
            }
        }
        false
    }

    /// Check whether an I/O error indicates a sharing violation (file in use).
    ///
    /// On Windows this is `ERROR_SHARING_VIOLATION` (OS error 32) or
    /// `ERROR_LOCK_VIOLATION` (OS error 33). On other platforms we check
    /// for `PermissionDenied` as the closest equivalent.
    fn is_sharing_violation(err: &std::io::Error) -> bool {
        #[cfg(windows)]
        {
            // ERROR_SHARING_VIOLATION = 32, ERROR_LOCK_VIOLATION = 33
            matches!(err.raw_os_error(), Some(32) | Some(33))
        }
        #[cfg(not(windows))]
        {
            err.kind() == std::io::ErrorKind::PermissionDenied
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use spambayes_core::Classification;
    use std::fs;
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    /// Helper: create a StatisticsManager pointing at a fresh temp directory.
    fn manager_in_tmp(save_interval: u32) -> (StatisticsManager, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let mgr = StatisticsManager::new(dir.path(), save_interval);
        (mgr, dir)
    }

    /// Validates: Requirements 1.1, 1.2, 1.6, 2.5
    /// new() with no existing file → all counters are zero.
    #[test]
    fn test_new_no_existing_file_gives_zeros() {
        let (mgr, _dir) = manager_in_tmp(10);

        let session = mgr.session_stats();
        assert_eq!(session.ham_classified, 0);
        assert_eq!(session.unsure_classified, 0);
        assert_eq!(session.spam_classified, 0);
        assert_eq!(session.ham_trained, 0);
        assert_eq!(session.spam_trained, 0);

        let lifetime = mgr.lifetime_stats();
        assert_eq!(lifetime.total_ham_trained, 0);
        assert_eq!(lifetime.total_spam_trained, 0);
        assert_eq!(lifetime.total_ham_classified, 0);
        assert_eq!(lifetime.total_unsure_classified, 0);
        assert_eq!(lifetime.total_spam_classified, 0);
    }

    /// Validates: Requirements 2.1, 2.2, 2.5
    /// new() with valid stats file → lifetime loaded correctly, session zero.
    #[test]
    fn test_new_with_valid_stats_file() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let stats_path = dir.path().join("spambayes_stats.json");

        let json = r#"{
            "version": 1,
            "lifetime": {
                "total_ham_trained": 100,
                "total_spam_trained": 200,
                "total_ham_classified": 300,
                "total_unsure_classified": 40,
                "total_spam_classified": 500
            }
        }"#;
        fs::write(&stats_path, json).unwrap();

        let mgr = StatisticsManager::new(dir.path(), 10);

        let lifetime = mgr.lifetime_stats();
        assert_eq!(lifetime.total_ham_trained, 100);
        assert_eq!(lifetime.total_spam_trained, 200);
        assert_eq!(lifetime.total_ham_classified, 300);
        assert_eq!(lifetime.total_unsure_classified, 40);
        assert_eq!(lifetime.total_spam_classified, 500);

        // Session counters always start at zero.
        let session = mgr.session_stats();
        assert_eq!(session.ham_classified, 0);
        assert_eq!(session.spam_classified, 0);
    }

    /// Validates: Requirements 2.5, 2.7
    /// new() with corrupt file → starts at zeros (logs warning).
    #[test]
    fn test_new_with_corrupt_file_gives_zeros() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let stats_path = dir.path().join("spambayes_stats.json");

        fs::write(&stats_path, "NOT VALID JSON {{{{").unwrap();

        let mgr = StatisticsManager::new(dir.path(), 10);

        let lifetime = mgr.lifetime_stats();
        assert_eq!(lifetime.total_ham_trained, 0);
        assert_eq!(lifetime.total_spam_trained, 0);
        assert_eq!(lifetime.total_ham_classified, 0);
        assert_eq!(lifetime.total_unsure_classified, 0);
        assert_eq!(lifetime.total_spam_classified, 0);
    }

    /// Validates: Requirements 1.5
    /// on_classified increments the correct session and lifetime counters.
    #[test]
    fn test_on_classified_increments_correct_counters() {
        let (mgr, _dir) = manager_in_tmp(100); // high interval to avoid auto-save

        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Spam);
        mgr.on_classified(Classification::Unsure);
        mgr.on_classified(Classification::Unsure);
        mgr.on_classified(Classification::Unsure);

        let session = mgr.session_stats();
        assert_eq!(session.ham_classified, 2);
        assert_eq!(session.spam_classified, 1);
        assert_eq!(session.unsure_classified, 3);

        let lifetime = mgr.lifetime_stats();
        assert_eq!(lifetime.total_ham_classified, 2);
        assert_eq!(lifetime.total_spam_classified, 1);
        assert_eq!(lifetime.total_unsure_classified, 3);
    }

    /// Validates: Requirements 1.3, 1.4
    /// on_trained increments the correct session and lifetime counters.
    #[test]
    fn test_on_trained_increments_correct_counters() {
        let (mgr, _dir) = manager_in_tmp(100);

        mgr.on_trained(true); // spam
        mgr.on_trained(true);
        mgr.on_trained(false); // ham

        let session = mgr.session_stats();
        assert_eq!(session.spam_trained, 2);
        assert_eq!(session.ham_trained, 1);

        let lifetime = mgr.lifetime_stats();
        assert_eq!(lifetime.total_spam_trained, 2);
        assert_eq!(lifetime.total_ham_trained, 1);
    }

    /// Validates: Requirements 2.3, 2.6
    /// Auto-save triggers when dirty_count reaches save_interval.
    #[test]
    fn test_auto_save_triggers_at_save_interval() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let stats_path = dir.path().join("spambayes_stats.json");
        let mgr = StatisticsManager::new(dir.path(), 3);

        // File should exist immediately (created on init with zeros).
        assert!(stats_path.exists(), "stats file should be created on init");

        // Verify it starts with zeros.
        let contents = fs::read_to_string(&stats_path).unwrap();
        let saved: StatsFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.lifetime.total_ham_classified, 0);

        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Spam);

        // Still below save_interval — file should still show zeros (not updated).
        let contents = fs::read_to_string(&stats_path).unwrap();
        let saved: StatsFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.lifetime.total_ham_classified, 0);

        // Third classification hits the save_interval of 3.
        mgr.on_classified(Classification::Unsure);

        // Verify contents are valid JSON with correct counts.
        let contents = fs::read_to_string(&stats_path).unwrap();
        let saved: StatsFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.lifetime.total_ham_classified, 1);
        assert_eq!(saved.lifetime.total_spam_classified, 1);
        assert_eq!(saved.lifetime.total_unsure_classified, 1);
    }

    /// Validates: Requirements 2.1, 2.2, 2.6
    /// save() creates a valid JSON file with correct format.
    #[test]
    fn test_save_creates_valid_json_file() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let stats_path = dir.path().join("spambayes_stats.json");
        let mgr = StatisticsManager::new(dir.path(), 100);

        mgr.on_classified(Classification::Ham);
        mgr.on_trained(true);
        mgr.save();

        assert!(stats_path.exists());
        let contents = fs::read_to_string(&stats_path).unwrap();
        let saved: StatsFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.version, 1);
        assert_eq!(saved.lifetime.total_ham_classified, 1);
        assert_eq!(saved.lifetime.total_spam_trained, 1);
    }

    /// Validates: Requirement 2.6
    /// Atomic write: after save() no .tmp file remains, and the JSON is complete.
    #[test]
    fn test_atomic_write_no_partial_files() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let stats_path = dir.path().join("spambayes_stats.json");
        let temp_path = dir.path().join("spambayes_stats.json.tmp");
        let mgr = StatisticsManager::new(dir.path(), 100);

        mgr.on_classified(Classification::Spam);
        mgr.save();

        // The final file should exist and be valid.
        assert!(stats_path.exists());
        // The temp file should NOT remain after a successful save.
        assert!(!temp_path.exists(), ".tmp file should not remain after save");

        // Verify the file is complete (parseable JSON).
        let contents = fs::read_to_string(&stats_path).unwrap();
        let saved: StatsFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.lifetime.total_spam_classified, 1);
    }

    /// Validates: Requirements 3.5
    /// reset_lifetime zeros all lifetime counters and saves immediately.
    #[test]
    fn test_reset_lifetime_zeros_all_and_saves() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let stats_path = dir.path().join("spambayes_stats.json");
        let mgr = StatisticsManager::new(dir.path(), 100);

        // Accumulate some stats.
        mgr.on_classified(Classification::Ham);
        mgr.on_classified(Classification::Spam);
        mgr.on_trained(true);
        mgr.on_trained(false);

        mgr.reset_lifetime();

        // In-memory lifetime should be zeros.
        let lifetime = mgr.lifetime_stats();
        assert_eq!(lifetime.total_ham_trained, 0);
        assert_eq!(lifetime.total_spam_trained, 0);
        assert_eq!(lifetime.total_ham_classified, 0);
        assert_eq!(lifetime.total_unsure_classified, 0);
        assert_eq!(lifetime.total_spam_classified, 0);

        // File should exist (immediate save).
        assert!(stats_path.exists());
        let contents = fs::read_to_string(&stats_path).unwrap();
        let saved: StatsFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(saved.lifetime.total_ham_classified, 0);
        assert_eq!(saved.lifetime.total_spam_classified, 0);
    }

    /// Validates: Requirement 4.4
    /// Thread safety: concurrent on_classified calls produce correct totals.
    #[test]
    fn test_thread_safety_concurrent_classified() {
        let (mgr, _dir) = manager_in_tmp(1000); // high interval — no auto-save interference

        let num_threads = 8;
        let calls_per_thread = 100;
        let mgr_arc = Arc::new(mgr);

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let m = Arc::clone(&mgr_arc);
                thread::spawn(move || {
                    for _ in 0..calls_per_thread {
                        m.on_classified(Classification::Ham);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        let expected_total = num_threads * calls_per_thread;

        let session = mgr_arc.session_stats();
        assert_eq!(
            session.ham_classified, expected_total as u32,
            "session ham count should equal total concurrent calls"
        );

        let lifetime = mgr_arc.lifetime_stats();
        assert_eq!(
            lifetime.total_ham_classified, expected_total as u64,
            "lifetime ham count should equal total concurrent calls"
        );
    }
}
