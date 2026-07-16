//! Centralized logging for the `SpamBayes` add-in.
//!
//! Provides a thread-safe [`Logger`] that writes timestamped, level-tagged
//! entries to a file. The default output location is
//! `%LOCALAPPDATA%\SpamBayes\addin_debug.log`.
//!
//! ## Verbosity Levels
//!
//! The verbosity setting in the Manager GUI maps directly to [`LogLevel`]:
//!
//! | Value | Level     | Description                              |
//! |-------|-----------|------------------------------------------|
//! | 0     | `Error`   | Minimal logging — errors only            |
//! | 1     | `Info`    | Application flow logging                 |
//! | 2     | `Verbose` | Debugging verbose logging (all messages) |
//!
//! **Validates: Requirements 17.2, 17.3, 17.6, 17.7, 21.5**

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::LogLevel;

/// Centralized logger accessible from all subsystems.
///
/// Writes formatted log entries to a file, filtering by the configured
/// [`LogLevel`]. Each entry includes a timestamp, severity level, and
/// the originating module name.
///
/// Thread-safe via an internal [`Mutex`] around the buffered writer.
/// The log level uses an [`AtomicU8`] so it can be updated at runtime
/// without acquiring the write lock.
///
/// Flushes after every write to ensure entries are visible even if
/// the process crashes.
///
/// **Validates: Requirements 17.2, 17.3, 17.6, 17.7, 21.5**
pub struct Logger {
    file: Mutex<BufWriter<File>>,
    level: AtomicU8,
}

impl Logger {
    /// Creates a new `Logger` that writes to the given path with the
    /// specified minimum log level.
    ///
    /// The file is opened in append mode and created if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the file cannot be opened or created.
    pub fn new(path: &Path, level: LogLevel) -> Result<Self, io::Error> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self {
            file: Mutex::new(BufWriter::new(file)),
            level: AtomicU8::new(level as u8),
        })
    }

    /// Returns the default log file path:
    /// `%LOCALAPPDATA%\SpamBayes\addin_debug.log`.
    ///
    /// Falls back to `%TEMP%\addin_debug.log` if `LOCALAPPDATA` is not set.
    #[must_use]
    pub fn default_path() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| {
                std::env::var("TEMP")
                    .or_else(|_| std::env::var("TMP"))
                    .unwrap_or_else(|_| ".".to_string())
            });
        PathBuf::from(base).join("SpamBayes").join("addin_debug.log")
    }

    /// Updates the log level at runtime.
    ///
    /// This is lock-free and can be called from any thread. Messages
    /// logged after this call will use the new level for filtering.
    pub fn set_level(&self, level: LogLevel) {
        self.level.store(level as u8, Ordering::Relaxed);
    }

    /// Returns the current log level.
    #[must_use]
    pub fn current_level(&self) -> LogLevel {
        LogLevel::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Converts a config verbosity value (u32) to a [`LogLevel`].
    ///
    /// - `0` → `Error` (minimal logging)
    /// - `1` → `Info` (application flow)
    /// - `2+` → `Verbose` (debugging)
    #[must_use]
    pub fn verbosity_to_level(verbose: u32) -> LogLevel {
        match verbose {
            0 => LogLevel::Error,
            1 => LogLevel::Info,
            _ => LogLevel::Verbose,
        }
    }

    /// Logs a message if `level` is at or below the configured threshold.
    ///
    /// Each entry is formatted as:
    /// ```text
    /// [YYYY-MM-DD HH:MM:SS] [LEVEL] [module] message
    /// ```
    ///
    /// The writer is flushed after every entry for crash diagnostics.
    pub fn log(&self, level: LogLevel, module: &str, message: &str) {
        // Higher numeric value = more verbose. Only log if the message
        // level is <= the configured threshold.
        let current_level = self.level.load(Ordering::Relaxed);
        if (level as u8) > current_level {
            return;
        }

        let timestamp = format_timestamp();
        let level_str = match level {
            LogLevel::Error => "ERROR",
            LogLevel::Info => "INFO",
            LogLevel::Verbose => "VERBOSE",
        };

        let entry = format!("[{timestamp}] [{level_str}] [{module}] {message}\n");

        if let Ok(mut writer) = self.file.lock() {
            // Best-effort write; we don't propagate errors from logging.
            let _ = writer.write_all(entry.as_bytes());
            let _ = writer.flush();
        }
    }

    /// Convenience method to log at [`LogLevel::Error`].
    pub fn error(&self, module: &str, message: &str) {
        self.log(LogLevel::Error, module, message);
    }

    /// Convenience method to log at [`LogLevel::Info`].
    pub fn info(&self, module: &str, message: &str) {
        self.log(LogLevel::Info, module, message);
    }

    /// Convenience method to log at [`LogLevel::Verbose`].
    pub fn verbose(&self, module: &str, message: &str) {
        self.log(LogLevel::Verbose, module, message);
    }
}

impl LogLevel {
    /// Converts a raw `u8` to a `LogLevel`, defaulting to `Error` for
    /// unrecognized values.
    #[must_use]
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Error,
            1 => Self::Info,
            _ => Self::Verbose,
        }
    }
}

/// Formats the current system time as `YYYY-MM-DD HH:MM:SS`.
fn format_timestamp() -> String {
    let now = SystemTime::now();
    let duration = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    let total_secs = duration.as_secs();

    // Convert to date/time components (simplified UTC calculation).
    let secs_per_day: u64 = 86400;
    let days = total_secs / secs_per_day;
    let day_secs = total_secs % secs_per_day;

    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    // Calculate year, month, day from days since epoch (1970-01-01).
    let (year, month, day) = days_to_date(days);

    format!(
        "{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}"
    )
}

/// Converts days since Unix epoch to (year, month, day).
fn days_to_date(days_since_epoch: u64) -> (u64, u64, u64) {
    // Algorithm based on Howard Hinnant's civil_from_days.
    let z = days_since_epoch + 719468;
    let era = z / 146097;
    let doe = z - era * 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    // ─── days_to_date Tests ──────────────────────────────────────────────

    #[test]
    fn days_to_date_unix_epoch() {
        // Day 0 = 1970-01-01
        let (y, m, d) = days_to_date(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn days_to_date_known_date_2000_01_01() {
        // 2000-01-01 is day 10957 since epoch
        let (y, m, d) = days_to_date(10957);
        assert_eq!((y, m, d), (2000, 1, 1));
    }

    #[test]
    fn days_to_date_known_date_2024_02_29() {
        // 2024 is a leap year. 2024-02-29 is day 19782
        // Days from 1970-01-01 to 2024-01-01 = 19723
        // Jan has 31 days, Feb 1-29 = 28 more days → 19723 + 31 + 28 = 19782
        let (y, m, d) = days_to_date(19782);
        assert_eq!((y, m, d), (2024, 2, 29));
    }

    #[test]
    fn days_to_date_known_date_2025_07_16() {
        // 2025-07-16: days from 1970-01-01 to 2025-01-01 = 20089
        // Jan=31, Feb=28, Mar=31, Apr=30, May=31, Jun=30, Jul 1-16=15
        // 20089 + 31 + 28 + 31 + 30 + 31 + 30 + 15 = 20285
        let (y, m, d) = days_to_date(20285);
        assert_eq!((y, m, d), (2025, 7, 16));
    }

    #[test]
    fn days_to_date_end_of_year() {
        // 1970-12-31 is day 364
        let (y, m, d) = days_to_date(364);
        assert_eq!((y, m, d), (1970, 12, 31));
    }

    #[test]
    fn days_to_date_leap_year_boundary() {
        // 1972 is first leap year after epoch. 1972-03-01
        // Days: 1970=365, 1971=365, 1972 Jan=31, Feb=29 → 365+365+31+29 = 790
        let (y, m, d) = days_to_date(790);
        assert_eq!((y, m, d), (1972, 3, 1));
    }

    // ─── LogLevel::from_u8 Tests ─────────────────────────────────────────

    #[test]
    fn log_level_from_u8_error() {
        assert_eq!(LogLevel::from_u8(0), LogLevel::Error);
    }

    #[test]
    fn log_level_from_u8_info() {
        assert_eq!(LogLevel::from_u8(1), LogLevel::Info);
    }

    #[test]
    fn log_level_from_u8_verbose() {
        assert_eq!(LogLevel::from_u8(2), LogLevel::Verbose);
    }

    #[test]
    fn log_level_from_u8_unknown_defaults_to_verbose() {
        // Any value >= 2 maps to Verbose
        assert_eq!(LogLevel::from_u8(3), LogLevel::Verbose);
        assert_eq!(LogLevel::from_u8(255), LogLevel::Verbose);
    }

    // ─── verbosity_to_level Tests ────────────────────────────────────────

    #[test]
    fn verbosity_to_level_zero_is_error() {
        assert_eq!(Logger::verbosity_to_level(0), LogLevel::Error);
    }

    #[test]
    fn verbosity_to_level_one_is_info() {
        assert_eq!(Logger::verbosity_to_level(1), LogLevel::Info);
    }

    #[test]
    fn verbosity_to_level_two_is_verbose() {
        assert_eq!(Logger::verbosity_to_level(2), LogLevel::Verbose);
    }

    #[test]
    fn verbosity_to_level_high_value_is_verbose() {
        assert_eq!(Logger::verbosity_to_level(100), LogLevel::Verbose);
    }

    // ─── Logger Level Filtering Tests ────────────────────────────────────

    #[test]
    fn logger_filters_verbose_when_level_is_error() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_filter");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_filter.log");
        let _ = std::fs::remove_file(&log_path);

        let logger = Logger::new(&log_path, LogLevel::Error).unwrap();

        // Log at all levels
        logger.error("test", "error message");
        logger.info("test", "info message");
        logger.verbose("test", "verbose message");

        // Read back
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("error message"), "error should be logged");
        assert!(!contents.contains("info message"), "info should be filtered");
        assert!(!contents.contains("verbose message"), "verbose should be filtered");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logger_passes_info_and_error_when_level_is_info() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_info");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_info.log");
        let _ = std::fs::remove_file(&log_path);

        let logger = Logger::new(&log_path, LogLevel::Info).unwrap();

        logger.error("mod", "err_msg");
        logger.info("mod", "info_msg");
        logger.verbose("mod", "verbose_msg");

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("err_msg"));
        assert!(contents.contains("info_msg"));
        assert!(!contents.contains("verbose_msg"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logger_passes_all_when_level_is_verbose() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_verbose");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_verbose.log");
        let _ = std::fs::remove_file(&log_path);

        let logger = Logger::new(&log_path, LogLevel::Verbose).unwrap();

        logger.error("m", "e");
        logger.info("m", "i");
        logger.verbose("m", "v");

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("[ERROR]"));
        assert!(contents.contains("[INFO]"));
        assert!(contents.contains("[VERBOSE]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logger_set_level_changes_filtering_at_runtime() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_setlevel");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_setlevel.log");
        let _ = std::fs::remove_file(&log_path);

        let logger = Logger::new(&log_path, LogLevel::Error).unwrap();

        logger.info("m", "before_upgrade");
        logger.set_level(LogLevel::Info);
        logger.info("m", "after_upgrade");

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(!contents.contains("before_upgrade"));
        assert!(contents.contains("after_upgrade"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logger_current_level_reflects_set_level() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_current");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_current.log");

        let logger = Logger::new(&log_path, LogLevel::Error).unwrap();
        assert_eq!(logger.current_level(), LogLevel::Error);

        logger.set_level(LogLevel::Verbose);
        assert_eq!(logger.current_level(), LogLevel::Verbose);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── Log Entry Format Tests ──────────────────────────────────────────

    #[test]
    fn log_entry_contains_module_name() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_module");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_module.log");
        let _ = std::fs::remove_file(&log_path);

        let logger = Logger::new(&log_path, LogLevel::Verbose).unwrap();
        logger.info("my_module", "hello world");

        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("[my_module]"));
        assert!(contents.contains("hello world"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_entry_has_timestamp_format() {
        let dir = std::env::temp_dir().join("spambayes_logger_test_ts");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test_ts.log");
        let _ = std::fs::remove_file(&log_path);

        let logger = Logger::new(&log_path, LogLevel::Verbose).unwrap();
        logger.info("t", "x");

        let contents = std::fs::read_to_string(&log_path).unwrap();
        // Should match pattern: [YYYY-MM-DD HH:MM:SS]
        let line = contents.lines().next().unwrap();
        assert!(line.starts_with('['));
        // Check timestamp portion length: [YYYY-MM-DD HH:MM:SS] = 21 chars
        let ts_end = line.find(']').unwrap();
        let ts = &line[1..ts_end];
        assert_eq!(ts.len(), 19, "timestamp should be 19 chars: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], " ");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── default_path Tests ──────────────────────────────────────────────

    #[test]
    fn default_path_ends_with_expected_filename() {
        let path = Logger::default_path();
        assert!(path.ends_with("addin_debug.log"));
    }

    #[test]
    fn default_path_contains_spambayes_dir() {
        let path = Logger::default_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("SpamBayes"),
            "default path should contain 'SpamBayes': {path_str}"
        );
    }
}
