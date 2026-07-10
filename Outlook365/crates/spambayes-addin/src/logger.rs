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
