//! Auto-update checker for the SpamBayes Outlook add-in.
//!
//! This module handles:
//! - Periodic version checking against a remote manifest
//! - Respecting the configured check interval
//! - Persisting last-check timestamps to avoid redundant network calls
//! - Reporting update availability to the user via Windows message box
//!
//! The update check runs on a background thread spawned from `OnStartupComplete`
//! to avoid blocking Outlook's COM STA thread. Results are posted back via a
//! Windows message to the STA thread for UI display.
//!
//! **Design decisions:**
//! - Uses `ureq` for HTTP (synchronous, minimal dependencies, no async runtime needed)
//! - The DLL cannot replace itself while loaded — we notify the user and provide
//!   a download URL. The actual update is performed by the installer.
//! - Build number comparison catches hotfix rebuilds of the same version.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use spambayes_config::AppConfig;

use crate::logger::Logger;
use crate::version_manifest::{self, UpdateStatus, VersionManifest};

// ─── UpdateChecker ───────────────────────────────────────────────────────────

/// Manages the lifecycle of update checks.
///
/// Created during `OnStartupComplete` and holds a reference to the logger
/// and a snapshot of the update configuration.
pub struct UpdateChecker {
    /// URL to fetch the version manifest from.
    update_url: String,
    /// Check interval in seconds (converted from config hours).
    check_interval_secs: u64,
    /// Unix timestamp of the last successful check.
    last_check_timestamp: u64,
    /// Whether the user has already been notified about the current update.
    already_notified: bool,
    /// The data directory for persisting update state.
    data_dir: std::path::PathBuf,
    /// Profile name for config persistence.
    profile_name: String,
    /// Logger reference for diagnostic output.
    logger: Option<Arc<Logger>>,
}

/// Result of an update check operation.
#[derive(Debug, Clone)]
pub enum UpdateCheckResult {
    /// No update available.
    UpToDate,
    /// A new version is available.
    UpdateAvailable {
        current_version: String,
        latest_version: String,
        download_url: String,
        release_notes: String,
    },
    /// A new build of the same version is available.
    BuildUpdateAvailable {
        version: String,
        current_build: u64,
        latest_build: u64,
        download_url: String,
    },
    /// Check was skipped (not due yet, or disabled).
    Skipped,
    /// Check failed with an error.
    Error(String),
}

impl UpdateChecker {
    /// Create a new `UpdateChecker` from the current application config.
    pub fn new(
        config: &AppConfig,
        data_dir: &Path,
        profile_name: &str,
        logger: Option<Arc<Logger>>,
    ) -> Self {
        Self {
            update_url: config.update.update_url.clone(),
            check_interval_secs: u64::from(config.update.check_interval_hours) * 3600,
            last_check_timestamp: config.update.last_check_timestamp,
            already_notified: config.update.update_notified,
            data_dir: data_dir.to_path_buf(),
            profile_name: profile_name.to_string(),
            logger,
        }
    }

    /// Check if an update check is due based on the configured interval.
    ///
    /// Returns `true` if enough time has elapsed since the last check.
    pub fn is_check_due(&self) -> bool {
        let now = current_unix_timestamp();
        now.saturating_sub(self.last_check_timestamp) >= self.check_interval_secs
    }

    /// Perform an update check.
    ///
    /// This method:
    /// 1. Checks if a check is due (respects interval)
    /// 2. Fetches the remote version manifest via HTTP
    /// 3. Compares against the running version and build number
    /// 4. Updates the last-check timestamp on success
    ///
    /// Returns the check result. The caller is responsible for displaying
    /// any notification to the user.
    pub fn check_for_update(&mut self) -> UpdateCheckResult {
        // Respect check interval.
        if !self.is_check_due() {
            self.log_verbose("Update check skipped: not due yet");
            return UpdateCheckResult::Skipped;
        }

        self.log_info("Checking for updates...");

        // Fetch the manifest.
        let manifest = match self.fetch_manifest() {
            Ok(m) => m,
            Err(e) => {
                self.log_info(&format!("Update check failed: {e}"));
                return UpdateCheckResult::Error(e);
            }
        };

        // Update the last-check timestamp (even on "up to date" — we did check).
        let now = current_unix_timestamp();
        self.last_check_timestamp = now;

        // Compare versions.
        let status = version_manifest::check_update_status(&manifest);

        match status {
            UpdateStatus::UpToDate => {
                self.log_info("Update check: running the latest version");
                self.already_notified = false;
                UpdateCheckResult::UpToDate
            }
            UpdateStatus::NewVersionAvailable {
                current,
                latest,
                download_url,
                release_notes,
            } => {
                self.log_info(&format!(
                    "Update available: {current} → {latest}"
                ));
                UpdateCheckResult::UpdateAvailable {
                    current_version: current,
                    latest_version: latest,
                    download_url,
                    release_notes,
                }
            }
            UpdateStatus::NewBuildAvailable {
                version,
                current_build,
                latest_build,
                download_url,
            } => {
                self.log_info(&format!(
                    "Build update available: {version} build {current_build} → {latest_build}"
                ));
                UpdateCheckResult::BuildUpdateAvailable {
                    version,
                    current_build,
                    latest_build,
                    download_url,
                }
            }
        }
    }

    /// Persist the update check state back to the config file.
    ///
    /// This saves:
    /// - `last_check_timestamp` — so we don't re-check too soon
    /// - `update_notified` — so we don't nag the user repeatedly
    /// - `latest_known_version` / `latest_download_url` — cached for UI display
    pub fn save_state(&self, config: &mut AppConfig) {
        config.update.last_check_timestamp = self.last_check_timestamp;
        config.update.update_notified = self.already_notified;
    }

    /// Mark that the user has been notified about the current available update.
    pub fn mark_notified(&mut self) {
        self.already_notified = true;
    }

    /// Returns `true` if the user has already been notified about the current update.
    pub fn was_notified(&self) -> bool {
        self.already_notified
    }

    /// Fetch the version manifest from the configured URL.
    fn fetch_manifest(&self) -> Result<VersionManifest, String> {
        let response = ureq::get(&self.update_url)
            .timeout(std::time::Duration::from_secs(15))
            .call()
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if response.status() != 200 {
            return Err(format!("HTTP {}", response.status()));
        }

        let body = response
            .into_string()
            .map_err(|e| format!("Failed to read response body: {e}"))?;

        VersionManifest::from_json(&body)
            .map_err(|e| format!("Failed to parse version manifest: {e}"))
    }

    fn log_info(&self, msg: &str) {
        if let Some(ref logger) = self.logger {
            logger.info("updater", msg);
        }
    }

    fn log_verbose(&self, msg: &str) {
        if let Some(ref logger) = self.logger {
            logger.verbose("updater", msg);
        }
    }
}

// ─── Update Notification (Windows UI) ────────────────────────────────────────

/// Display an update notification to the user via a Windows message box.
///
/// This should be called on the STA thread (from a timer callback or
/// `OnStartupComplete` continuation).
///
/// Returns `true` if the user clicked "Yes" (wants to open the download page).
#[cfg(target_os = "windows")]
pub fn show_update_notification(
    current_version: &str,
    latest_version: &str,
    release_notes: &str,
    download_url: &str,
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONINFORMATION, MB_YESNO,
    };

    let message = if release_notes.is_empty() {
        format!(
            "A new version of SpamBayes is available!\n\n\
             Current version: {current_version}\n\
             Latest version: {latest_version}\n\n\
             Would you like to open the download page?"
        )
    } else {
        format!(
            "A new version of SpamBayes is available!\n\n\
             Current version: {current_version}\n\
             Latest version: {latest_version}\n\n\
             What's new: {release_notes}\n\n\
             Would you like to open the download page?"
        )
    };

    let title = "SpamBayes Update Available";

    let msg_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(msg_wide.as_ptr()),
            windows::core::PCWSTR(title_wide.as_ptr()),
            MB_YESNO | MB_ICONINFORMATION,
        )
    };

    if result == IDYES {
        // Open the download URL in the default browser.
        open_url(download_url);
        true
    } else {
        false
    }
}

/// Display a build-update notification (same version, newer build).
#[cfg(target_os = "windows")]
pub fn show_build_update_notification(
    version: &str,
    current_build: u64,
    latest_build: u64,
    download_url: &str,
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONINFORMATION, MB_YESNO,
    };

    let message = format!(
        "A newer build of SpamBayes {version} is available.\n\n\
         Your build: {current_build}\n\
         Latest build: {latest_build}\n\n\
         This is a maintenance update with bug fixes.\n\
         Would you like to open the download page?"
    );

    let title = "SpamBayes Build Update";

    let msg_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(msg_wide.as_ptr()),
            windows::core::PCWSTR(title_wide.as_ptr()),
            MB_YESNO | MB_ICONINFORMATION,
        )
    };

    if result == IDYES {
        open_url(download_url);
        true
    } else {
        false
    }
}

/// Open a URL in the system default browser.
#[cfg(target_os = "windows")]
fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let operation: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let url_wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        ShellExecuteW(
            None,
            windows::core::PCWSTR(operation.as_ptr()),
            windows::core::PCWSTR(url_wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Stub for non-Windows builds (unit testing).
#[cfg(not(target_os = "windows"))]
pub fn show_update_notification(
    _current_version: &str,
    _latest_version: &str,
    _release_notes: &str,
    _download_url: &str,
) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn show_build_update_notification(
    _version: &str,
    _current_build: u64,
    _latest_build: u64,
    _download_url: &str,
) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
fn open_url(_url: &str) {}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Get the current time as a Unix timestamp (seconds since epoch).
fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_check_due_first_time() {
        let config = AppConfig::default();
        let dir = std::path::PathBuf::from(".");
        let checker = UpdateChecker::new(&config, &dir, "default", None);
        // last_check_timestamp is 0, so check should always be due.
        assert!(checker.is_check_due());
    }

    #[test]
    fn test_is_check_due_recent_check() {
        let mut config = AppConfig::default();
        // Set last check to "just now".
        config.update.last_check_timestamp = current_unix_timestamp();
        config.update.check_interval_hours = 24;

        let dir = std::path::PathBuf::from(".");
        let checker = UpdateChecker::new(&config, &dir, "default", None);
        // Should NOT be due — we just checked.
        assert!(!checker.is_check_due());
    }

    #[test]
    fn test_is_check_due_expired() {
        let mut config = AppConfig::default();
        // Set last check to 25 hours ago.
        let now = current_unix_timestamp();
        config.update.last_check_timestamp = now.saturating_sub(25 * 3600);
        config.update.check_interval_hours = 24;

        let dir = std::path::PathBuf::from(".");
        let checker = UpdateChecker::new(&config, &dir, "default", None);
        assert!(checker.is_check_due());
    }

    #[test]
    fn test_save_state_updates_config() {
        let mut config = AppConfig::default();
        let dir = std::path::PathBuf::from(".");
        let mut checker = UpdateChecker::new(&config, &dir, "default", None);

        checker.last_check_timestamp = 12345;
        checker.mark_notified();
        checker.save_state(&mut config);

        assert_eq!(config.update.last_check_timestamp, 12345);
        assert!(config.update.update_notified);
    }
}
