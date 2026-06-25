//! Python `SpamBayes` configuration migration.
//!
//! Detects an existing Python `SpamBayes` INI configuration file and applies
//! it seamlessly so that folder IDs, thresholds, and other settings carry
//! over without requiring user modification.
//!
//! # Python Config Locations
//!
//! The Python `SpamBayes` Outlook add-in stores its per-profile INI file as:
//!   `%APPDATA%\SpamBayes\{ProfileName}.ini`
//!
//! The Rust add-in stores its config as:
//!   `%LOCALAPPDATA%\SpamBayes\{profile_name}.ini`
//!
//! Migration detects Python configs in both directories (they may overlap
//! if the user has files in either location).

use std::path::{Path, PathBuf};

use crate::errors::ConfigError;
use crate::options::AppConfig;

/// Known candidate filenames for the Python `SpamBayes` INI configuration.
///
/// The Python add-in uses the MAPI profile name as the filename. Common
/// profile names include "Outlook", "Microsoft Outlook", or custom names.
/// We also check for generic names like "default".
const COMMON_PROFILE_NAMES: &[&str] = &[
    "Outlook",
    "Microsoft Outlook",
    "default",
    "Default",
];

/// Attempt to detect an existing Python `SpamBayes` INI config file.
///
/// Searches the following locations (in order):
/// 1. The Rust data directory itself (`data_dir`) — in case both use the same dir
/// 2. The `%APPDATA%\SpamBayes` directory (where Python `SpamBayes` stores its config)
///
/// Within each directory, searches for:
/// 1. A file matching the given `profile_name` (e.g., `default.ini`)
/// 2. Common Python `SpamBayes` profile names (e.g., `Outlook.ini`)
/// 3. Any `.ini` file that contains a `[Filter]` section (heuristic detection)
///
/// Returns `None` if no Python config is found.
#[must_use]
pub fn detect_python_config(data_dir: &Path, profile_name: &str) -> Option<PathBuf> {
    // Build list of directories to search
    let mut search_dirs: Vec<PathBuf> = vec![data_dir.to_path_buf()];

    // Add %APPDATA%\SpamBayes (Python's preferred location)
    if let Ok(appdata) = std::env::var("APPDATA") {
        let python_dir = PathBuf::from(appdata).join("SpamBayes");
        if python_dir != data_dir && python_dir.is_dir() {
            search_dirs.push(python_dir);
        }
    }

    detect_python_config_in_dirs(&search_dirs, data_dir, profile_name)
}

/// Internal detection logic that searches given directories.
///
/// Separated from `detect_python_config` to allow testing without
/// depending on the real `%APPDATA%` environment variable.
fn detect_python_config_in_dirs(
    search_dirs: &[PathBuf],
    data_dir: &Path,
    profile_name: &str,
) -> Option<PathBuf> {
    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }

        let is_rust_dir = dir.as_path() == data_dir;

        // 1. Try common profile names (skip the current profile_name itself in
        //    the Rust data_dir — that would be our own file, not a Python one)
        for &name in COMMON_PROFILE_NAMES {
            // Skip the Rust profile's own filename in its own directory
            if is_rust_dir && name.eq_ignore_ascii_case(profile_name) {
                continue;
            }
            let candidate = dir.join(format!("{name}.ini"));
            if candidate.is_file() && looks_like_spambayes_config(&candidate) {
                return Some(candidate);
            }
        }

        // 2. Also check for profile_name in the *other* directory (APPDATA)
        if !is_rust_dir {
            let candidate = dir.join(format!("{profile_name}.ini"));
            if candidate.is_file() && looks_like_spambayes_config(&candidate) {
                return Some(candidate);
            }
        }

        // 3. Scan all .ini files in the directory as a fallback heuristic
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("ini") {
                    // Skip our own Rust config file and known non-config INIs
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if is_rust_dir && stem.eq_ignore_ascii_case(profile_name) {
                        continue;
                    }
                    // Skip bayes_customize.ini (overlay, not main config)
                    if stem.eq_ignore_ascii_case("bayes_customize") {
                        continue;
                    }
                    if stem.eq_ignore_ascii_case("default_configuration") {
                        continue;
                    }
                    // Skip files already checked in the common-names pass
                    let already_checked = COMMON_PROFILE_NAMES
                        .iter()
                        .any(|&n| stem.eq_ignore_ascii_case(n));
                    if already_checked {
                        continue;
                    }
                    if looks_like_spambayes_config(&path) {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

/// Migrate a Python `SpamBayes` INI config file, returning the loaded `AppConfig`.
///
/// This reads the Python INI file using the same parser that `AppConfig::load`
/// uses. Since the Python and Rust formats are compatible (same section names,
/// same key names, same folder ID tuple format), the file is parsed directly.
///
/// The function does NOT copy the file — it reads from `source` and produces
/// an `AppConfig` struct. The caller can then use this config and optionally
/// save it to the Rust config location.
///
/// Folder ID references (hex tuples like `('AABB', 'CCDD')`) are preserved
/// exactly as they appear in the Python config.
///
/// # Errors
///
/// Returns `ConfigError` if the source file cannot be read or parsed.
pub fn migrate_python_config(source: &Path) -> Result<AppConfig, ConfigError> {
    // The Python INI and our Rust INI share the same format.
    // We can load it by treating source's parent as data_dir and the stem as profile_name.
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");

    AppConfig::load(parent, stem)
}

/// Detect and migrate Python `SpamBayes` config on startup.
///
/// This is the main entry point for the migration flow, intended to be called
/// from `addin_core.rs` when no Rust-specific config file exists yet.
///
/// Returns `Some(AppConfig)` if a Python config was found and successfully migrated,
/// or `None` if no Python config was detected.
///
/// # Arguments
///
/// * `data_dir` — The Rust add-in's data directory (e.g., `%LOCALAPPDATA%\SpamBayes`)
/// * `profile_name` — The Rust profile name (e.g., `"default"`)
#[must_use]
pub fn try_migrate(data_dir: &Path, profile_name: &str) -> Option<AppConfig> {
    let python_config_path = detect_python_config(data_dir, profile_name)?;

    migrate_python_config(&python_config_path).ok()
}

/// Check if a file looks like a `SpamBayes` config by searching for characteristic
/// sections (`[Filter]` or `[General]` with SpamBayes-specific keys).
///
/// This is a lightweight heuristic — it reads the first few KB of the file
/// and checks for known section headers.
fn looks_like_spambayes_config(path: &Path) -> bool {
    // Read up to 4KB for a quick heuristic check
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };

    // Truncate to first 4KB for efficiency on large files
    let sample = if content.len() > 4096 {
        &content[..4096]
    } else {
        &content
    };

    // Must have at least one SpamBayes-specific section
    let has_filter_section = sample.contains("[Filter]");
    let has_general_section = sample.contains("[General]");
    let has_training_section = sample.contains("[Training]");

    // Require at least one recognizable section
    if !(has_filter_section || has_general_section || has_training_section) {
        return false;
    }

    // Extra confidence: check for SpamBayes-specific keys
    let has_spam_threshold = sample.contains("spam_threshold");
    let has_folder_id = sample.contains("spam_folder_id") || sample.contains("watch_folder_ids");
    let has_field_score = sample.contains("field_score_name");

    // At least one SpamBayes-specific key should be present
    has_spam_threshold || has_folder_id || has_field_score
}

/// Check if the Rust-specific config file already exists.
///
/// Returns `true` if `{data_dir}/{profile_name}.ini` exists on disk.
#[must_use]
pub fn rust_config_exists(data_dir: &Path, profile_name: &str) -> bool {
    let config_path = data_dir.join(format!("{profile_name}.ini"));
    config_path.is_file()
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temporary directory with a fake `SpamBayes` INI file.
    fn create_test_ini(dir: &Path, filename: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(filename), content).unwrap();
    }

    const SAMPLE_PYTHON_CONFIG: &str = "\
[General]
field_score_name = Spam
verbose = 0

[Filter]
enabled = True
spam_threshold = 90.0
unsure_threshold = 15.0
spam_folder_id = ('0000000038A1BB1005E5101AA1BB08002B2A56C20000454D534D44422E444C4C00000000', 'AABBCCDD')
watch_folder_ids = [('1122334455', '6677889900')]
spam_action = Moved

[Training]
ham_folder_ids = [('AABB', 'CCDD')]
spam_folder_ids = [('EEFF', '0011')]

[Filter_Now]
only_unread = True
";

    #[test]
    fn test_detect_python_config_in_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        // Place a Python config with a different profile name
        create_test_ini(data_dir, "Outlook.ini", SAMPLE_PYTHON_CONFIG);

        let search_dirs = vec![data_dir.to_path_buf()];
        let result = detect_python_config_in_dirs(&search_dirs, data_dir, "default");
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("Outlook.ini"));
    }

    #[test]
    fn test_detect_skips_own_profile() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        // Place a config with the SAME profile name — should be skipped
        create_test_ini(data_dir, "default.ini", SAMPLE_PYTHON_CONFIG);

        let search_dirs = vec![data_dir.to_path_buf()];
        let result = detect_python_config_in_dirs(&search_dirs, data_dir, "default");
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_skips_non_spambayes_ini() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        // Place a random INI file that doesn't look like SpamBayes
        let non_sb_content = "[SomeApp]\nkey = value\n";
        create_test_ini(data_dir, "Outlook.ini", non_sb_content);

        let search_dirs = vec![data_dir.to_path_buf()];
        let result = detect_python_config_in_dirs(&search_dirs, data_dir, "default");
        assert!(result.is_none());
    }

    #[test]
    fn test_migrate_python_config_preserves_folder_ids() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        create_test_ini(data_dir, "Outlook.ini", SAMPLE_PYTHON_CONFIG);

        let config = migrate_python_config(&data_dir.join("Outlook.ini")).unwrap();

        // Verify folder IDs are preserved
        assert!(config.filter.spam_folder_id.is_some());
        let spam_folder = config.filter.spam_folder_id.unwrap();
        assert_eq!(
            spam_folder.store_id.0,
            "0000000038A1BB1005E5101AA1BB08002B2A56C20000454D534D44422E444C4C00000000"
        );
        assert_eq!(spam_folder.entry_id.0, "AABBCCDD");

        // Verify watch folders
        assert_eq!(config.filter.watch_folder_ids.len(), 1);
        assert_eq!(config.filter.watch_folder_ids[0].store_id.0, "1122334455");
        assert_eq!(config.filter.watch_folder_ids[0].entry_id.0, "6677889900");

        // Verify training folders
        assert_eq!(config.training.ham_folder_ids.len(), 1);
        assert_eq!(config.training.ham_folder_ids[0].store_id.0, "AABB");
        assert_eq!(config.training.ham_folder_ids[0].entry_id.0, "CCDD");

        assert_eq!(config.training.spam_folder_ids.len(), 1);
        assert_eq!(config.training.spam_folder_ids[0].store_id.0, "EEFF");
        assert_eq!(config.training.spam_folder_ids[0].entry_id.0, "0011");
    }

    #[test]
    fn test_migrate_python_config_preserves_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        create_test_ini(data_dir, "Outlook.ini", SAMPLE_PYTHON_CONFIG);

        let config = migrate_python_config(&data_dir.join("Outlook.ini")).unwrap();

        assert_eq!(config.filter.spam_threshold, 90.0);
        assert_eq!(config.filter.unsure_threshold, 15.0);
    }

    #[test]
    fn test_try_migrate_no_python_config() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        // Empty directory — no Python config to find.
        // Use detect_python_config_in_dirs directly to avoid %APPDATA% interference.
        let search_dirs = vec![data_dir.to_path_buf()];
        let result = detect_python_config_in_dirs(&search_dirs, data_dir, "default");
        assert!(result.is_none());
    }

    #[test]
    fn test_try_migrate_success() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        create_test_ini(data_dir, "Outlook.ini", SAMPLE_PYTHON_CONFIG);

        let search_dirs = vec![data_dir.to_path_buf()];
        let detected = detect_python_config_in_dirs(&search_dirs, data_dir, "default");
        assert!(detected.is_some());

        let config = migrate_python_config(&detected.unwrap()).unwrap();
        assert_eq!(config.filter.spam_threshold, 90.0);
        assert!(config.filter.spam_folder_id.is_some());
    }

    #[test]
    fn test_rust_config_exists() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        assert!(!rust_config_exists(data_dir, "default"));

        fs::write(data_dir.join("default.ini"), "[General]\n").unwrap();
        assert!(rust_config_exists(data_dir, "default"));
    }

    #[test]
    fn test_looks_like_spambayes_config_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ini");
        fs::write(&path, SAMPLE_PYTHON_CONFIG).unwrap();

        assert!(looks_like_spambayes_config(&path));
    }

    #[test]
    fn test_looks_like_spambayes_config_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ini");
        fs::write(&path, "[RandomApp]\nfoo = bar\n").unwrap();

        assert!(!looks_like_spambayes_config(&path));
    }

    #[test]
    fn test_detect_in_separate_appdata_dir() {
        let rust_dir = tempfile::tempdir().unwrap();
        let appdata_dir = tempfile::tempdir().unwrap();

        // Place config only in the "appdata" directory
        create_test_ini(appdata_dir.path(), "Outlook.ini", SAMPLE_PYTHON_CONFIG);

        let search_dirs = vec![
            rust_dir.path().to_path_buf(),
            appdata_dir.path().to_path_buf(),
        ];
        let result =
            detect_python_config_in_dirs(&search_dirs, rust_dir.path(), "default");
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("Outlook.ini"));
    }

    #[test]
    fn test_detect_finds_profile_name_in_appdata() {
        let rust_dir = tempfile::tempdir().unwrap();
        let appdata_dir = tempfile::tempdir().unwrap();

        // Place config with the same profile_name but in a DIFFERENT directory
        // This simulates finding `default.ini` in %APPDATA%\SpamBayes
        create_test_ini(appdata_dir.path(), "default.ini", SAMPLE_PYTHON_CONFIG);

        let search_dirs = vec![
            rust_dir.path().to_path_buf(),
            appdata_dir.path().to_path_buf(),
        ];
        let result =
            detect_python_config_in_dirs(&search_dirs, rust_dir.path(), "default");
        assert!(result.is_some());
    }
}
