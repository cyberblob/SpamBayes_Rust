//! Layered configuration loading via INI file chaining.
//!
//! `ConfigChain` manages the ordered loading and merging of multiple INI files
//! (default config → profile-specific config), with later files overriding
//! earlier ones. Settings are always saved to the profile-specific file.

use std::path::PathBuf;

use crate::errors::ConfigError;
use crate::ini_parser::{IniFile, IniData, merge_ini_data};
use crate::options::AppConfig;
use crate::profile::{sanitize_profile_name, resolve_data_directory};

/// Manages layered configuration loading from chained INI files.
///
/// Resolution order (highest priority last):
/// 1. Built-in defaults (hardcoded in `AppConfig::default()`)
/// 2. `default_bayes_customize.ini` (if exists in data directory)
/// 3. `<profile>_bayes_customize.ini` (if exists in data directory)
/// 4. `<profile>.ini` (if exists — written by `AppConfig::save` / wizard)
///
/// # Backward Compatibility
///
/// Existing single-file configurations are handled gracefully. If a user
/// already has a `<profile>_bayes_customize.ini` file from a previous
/// version (before the chaining feature was added), it continues to work
/// without any migration. The file is loaded as the profile layer (layer 3)
/// in the chain, and since missing files are silently skipped, the absence
/// of a `default_bayes_customize.ini` is perfectly normal. Users do not
/// need to take any action — their existing config is picked up automatically.
///
/// The `<profile>.ini` file (layer 4) is produced by `AppConfig::save()` and
/// the configuration wizard. It takes highest priority so that wizard-saved
/// settings (like `filter.enabled = true`) are always respected.
pub struct ConfigChain {
    /// The resolved data directory for this profile.
    data_directory: PathBuf,
    /// The sanitized profile name.
    profile_name: String,
    /// The merged configuration.
    config: AppConfig,
}

impl ConfigChain {
    /// Load configuration using the chain resolution strategy.
    ///
    /// Resolution order:
    /// 1. Built-in defaults (`AppConfig::default()`)
    /// 2. `default_bayes_customize.ini` (if exists in data directory)
    /// 3. `<profile>_bayes_customize.ini` (if exists in data directory)
    /// 4. `<profile>.ini` (if exists — written by `AppConfig::save` / wizard)
    ///
    /// Missing files are silently skipped (not an error).
    /// Parse errors produce a warning on stderr and the file is skipped.
    pub fn load(profile_name: &str) -> Result<Self, ConfigError> {
        let sanitized = sanitize_profile_name(profile_name);
        let data_dir = resolve_data_directory(profile_name, None);

        // Start with empty merged INI data — AppConfig::from_ini_data will
        // apply defaults for any keys not present.
        let mut merged = IniData::new();

        // Layer 1: default_bayes_customize.ini
        let default_path = data_dir.join("default_bayes_customize.ini");
        match IniFile::read(&default_path) {
            Ok(data) => merge_ini_data(&mut merged, &data),
            Err(ConfigError::FileNotFound(_)) => { /* normal — silently skip */ }
            Err(ConfigError::ParseError { path, line, message }) => {
                eprintln!(
                    "Warning: parse error in {} at line {}: {}, skipping file",
                    path.display(), line, message
                );
            }
            Err(ConfigError::IoError { path, source }) => {
                eprintln!("Warning: I/O error reading {}: {}, skipping file", path.display(), source);
            }
            Err(e) => {
                eprintln!("Warning: error reading {}: {e}, skipping file", default_path.display());
            }
        }

        // Layer 2: <sanitized_profile>_bayes_customize.ini
        let profile_path = data_dir.join(format!("{sanitized}_bayes_customize.ini"));
        match IniFile::read(&profile_path) {
            Ok(data) => merge_ini_data(&mut merged, &data),
            Err(ConfigError::FileNotFound(_)) => { /* normal — silently skip */ }
            Err(ConfigError::ParseError { path, line, message }) => {
                eprintln!(
                    "Warning: parse error in {} at line {}: {}, skipping file",
                    path.display(), line, message
                );
            }
            Err(ConfigError::IoError { path, source }) => {
                eprintln!("Warning: I/O error reading {}: {}, skipping file", path.display(), source);
            }
            Err(e) => {
                eprintln!("Warning: error reading {}: {e}, skipping file", profile_path.display());
            }
        }

        // Layer 3: <sanitized_profile>.ini (written by AppConfig::save and the wizard)
        let simple_profile_path = data_dir.join(format!("{sanitized}.ini"));
        match IniFile::read(&simple_profile_path) {
            Ok(data) => merge_ini_data(&mut merged, &data),
            Err(ConfigError::FileNotFound(_)) => {
                // Profile-specific INI not found — try default.ini as fallback
                let default_ini_path = data_dir.join("default.ini");
                match IniFile::read(&default_ini_path) {
                    Ok(data) => merge_ini_data(&mut merged, &data),
                    Err(ConfigError::FileNotFound(_)) => { /* no config at all — use built-in defaults */ }
                    Err(e) => {
                        eprintln!("Warning: error reading {}: {e}, skipping file", default_ini_path.display());
                    }
                }
            }
            Err(ConfigError::ParseError { path, line, message }) => {
                eprintln!(
                    "Warning: parse error in {} at line {}: {}, skipping file",
                    path.display(), line, message
                );
            }
            Err(ConfigError::IoError { path, source }) => {
                eprintln!("Warning: I/O error reading {}: {}, skipping file", path.display(), source);
            }
            Err(e) => {
                eprintln!("Warning: error reading {}: {e}, skipping file", simple_profile_path.display());
            }
        }

        // Apply merged INI data to produce the final AppConfig
        let config = AppConfig::from_ini_data(&merged);

        Ok(ConfigChain {
            data_directory: data_dir,
            profile_name: sanitized,
            config,
        })
    }

    /// Load using `BAYESCUSTOMIZE` environment variable override.
    ///
    /// The env value is a semicolon-separated list of INI file paths that are
    /// loaded in order, with later files overriding earlier ones. Missing files
    /// produce a warning on stderr and are skipped. If all files are missing or
    /// have errors, falls back to built-in defaults.
    pub fn load_from_env(env_value: &str) -> Result<Self, ConfigError> {
        let paths: Vec<&str> = env_value.split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        let mut merged = IniData::new();
        let mut first_valid_path: Option<PathBuf> = None;

        for path_str in &paths {
            let path = std::path::Path::new(path_str);
            match IniFile::read(path) {
                Ok(data) => {
                    if first_valid_path.is_none() {
                        first_valid_path = Some(path.to_path_buf());
                    }
                    merge_ini_data(&mut merged, &data);
                }
                Err(ConfigError::FileNotFound(p)) => {
                    eprintln!(
                        "Warning: BAYESCUSTOMIZE file not found: {}, skipping",
                        p.display()
                    );
                }
                Err(ConfigError::ParseError { path: p, line, message }) => {
                    eprintln!(
                        "Warning: parse error in {} at line {}: {}, skipping file",
                        p.display(), line, message
                    );
                }
                Err(ConfigError::IoError { path: p, source }) => {
                    eprintln!(
                        "Warning: I/O error reading {}: {}, skipping file",
                        p.display(), source
                    );
                }
                Err(e) => {
                    eprintln!(
                        "Warning: error reading {}: {e}, skipping file",
                        path.display()
                    );
                }
            }
        }

        // Determine data directory: parent of first valid file, or current dir as fallback.
        let data_directory = first_valid_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let config = AppConfig::from_ini_data(&merged);

        Ok(ConfigChain {
            data_directory,
            profile_name: String::new(),
            config,
        })
    }

    /// Get the merged configuration.
    #[must_use]
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Get mutable reference to config (for modifications).
    pub fn config_mut(&mut self) -> &mut AppConfig {
        &mut self.config
    }

    /// Save modified settings to the profile-specific file.
    ///
    /// Only writes settings that differ from built-in defaults (sparse save).
    /// Uses atomic write (temp file + rename) via `IniFile::write()`.
    /// Never writes to `default_bayes_customize.ini`.
    pub fn save(&self) -> Result<(), ConfigError> {
        let sparse_data = self.config.to_sparse_ini_data();
        let profile_path = self.profile_config_path();
        IniFile::write(&profile_path, &sparse_data)
    }

    /// Get the data directory path.
    #[must_use]
    pub fn data_directory(&self) -> &PathBuf {
        &self.data_directory
    }

    /// Get the sanitized profile name.
    #[must_use]
    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    /// Get the profile config file path.
    ///
    /// Returns `<data_directory>/<profile_name>_bayes_customize.ini`.
    #[must_use]
    pub fn profile_config_path(&self) -> PathBuf {
        self.data_directory
            .join(format!("{}_bayes_customize.ini", self.profile_name))
    }

    /// Get the default config file path.
    ///
    /// Returns `<data_directory>/default_bayes_customize.ini`.
    #[must_use]
    pub fn default_config_path(&self) -> PathBuf {
        self.data_directory.join("default_bayes_customize.ini")
    }

    /// Create a `ConfigChain` from an existing config, data directory, and profile name.
    ///
    /// Useful for constructing a chain when the caller already has a resolved
    /// config and knows the save location. The config is used as-is without
    /// loading any files from disk.
    #[must_use]
    pub fn from_parts(config: AppConfig, data_directory: PathBuf, profile_name: &str) -> Self {
        Self {
            data_directory,
            profile_name: profile_name.to_string(),
            config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// Mutex to serialize tests that modify the LOCALAPPDATA environment variable,
    /// since env vars are process-global.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper: set LOCALAPPDATA to a tempdir, run closure, then restore.
    fn with_fake_appdata<F, R>(tmp: &std::path::Path, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _lock = ENV_MUTEX.lock().unwrap();
        let original = std::env::var("LOCALAPPDATA").ok();
        std::env::set_var("LOCALAPPDATA", tmp.to_str().unwrap());

        let result = f();

        match original {
            Some(val) => std::env::set_var("LOCALAPPDATA", val),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        result
    }

    // ─── Test: load with no files → built-in defaults ───────────────────

    #[test]
    fn load_no_files_returns_builtin_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            ConfigChain::load("TestProfile").unwrap()
        });

        let defaults = AppConfig::default();
        assert_eq!(chain.config().filter.spam_threshold, defaults.filter.spam_threshold);
        assert_eq!(chain.config().filter.unsure_threshold, defaults.filter.unsure_threshold);
        assert_eq!(chain.config().filter.enabled, defaults.filter.enabled);
        assert_eq!(chain.config().general.field_score_name, defaults.general.field_score_name);
    }

    // ─── Test: load with only default file → defaults overridden ────────

    #[test]
    fn load_only_default_file_overrides_builtin_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            // Create data directory and default config file
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();
            fs::write(
                data_dir.join("default_bayes_customize.ini"),
                "[Filter]\nspam_threshold = 50.0\n",
            ).unwrap();

            ConfigChain::load("TestProfile").unwrap()
        });

        assert_eq!(chain.config().filter.spam_threshold, 50.0);
        // Unmodified fields retain built-in defaults
        assert_eq!(chain.config().filter.unsure_threshold, 15.0);
    }

    // ─── Test: load with both files → profile overrides default ─────────

    #[test]
    fn load_both_files_profile_overrides_default() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();

            // Default sets spam_threshold to 50 and unsure_threshold to 25
            fs::write(
                data_dir.join("default_bayes_customize.ini"),
                "[Filter]\nspam_threshold = 50.0\nunsure_threshold = 25.0\n",
            ).unwrap();

            // Profile overrides spam_threshold to 80 only
            fs::write(
                data_dir.join("testprofile_bayes_customize.ini"),
                "[Filter]\nspam_threshold = 80.0\n",
            ).unwrap();

            ConfigChain::load("TestProfile").unwrap()
        });

        // Profile overrides the default file's value
        assert_eq!(chain.config().filter.spam_threshold, 80.0);
        // Default file's value persists where profile doesn't override
        assert_eq!(chain.config().filter.unsure_threshold, 25.0);
    }

    // ─── Test: load with only profile file → profile values + built-in defaults ─

    #[test]
    fn load_only_profile_file_uses_profile_plus_builtin_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();

            // Only the profile file exists
            fs::write(
                data_dir.join("testprofile_bayes_customize.ini"),
                "[Filter]\nunsure_threshold = 30.0\n",
            ).unwrap();

            ConfigChain::load("TestProfile").unwrap()
        });

        // Profile value applied
        assert_eq!(chain.config().filter.unsure_threshold, 30.0);
        // Built-in defaults for everything else
        assert_eq!(chain.config().filter.spam_threshold, 90.0);
        assert_eq!(chain.config().filter.enabled, false);
    }

    // ─── Test: parse error in default file → skip, log warning ──────────

    #[test]
    fn load_parse_error_in_default_file_skips_and_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();

            // Malformed INI: unclosed section header triggers ParseError
            fs::write(
                data_dir.join("default_bayes_customize.ini"),
                "[Section\nno_closing_bracket\n",
            ).unwrap();

            ConfigChain::load("TestProfile").unwrap()
        });

        // Parse error skipped — falls back to built-in defaults
        let defaults = AppConfig::default();
        assert_eq!(chain.config().filter.spam_threshold, defaults.filter.spam_threshold);
        assert_eq!(chain.config().filter.unsure_threshold, defaults.filter.unsure_threshold);
    }

    // ─── Test: sparse save: only non-default values written ─────────────

    #[test]
    fn save_writes_only_non_default_values() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();

            let mut chain = ConfigChain::load("TestProfile").unwrap();
            // Modify one value away from default
            chain.config_mut().filter.spam_threshold = 75.0;
            chain.save().unwrap();
            chain
        });

        // Read back the profile config file
        let profile_path = chain.profile_config_path();
        let content = fs::read_to_string(&profile_path).unwrap();

        // Should contain only the changed value
        assert!(content.contains("spam_threshold = 75.0"), "file should contain changed value");
        // Should NOT contain default values that weren't changed
        assert!(!content.contains("unsure_threshold"), "file should not contain default values");
        assert!(!content.contains("enabled"), "file should not contain default values");
    }

    // ─── Test: atomic write creates valid file ──────────────────────────

    #[test]
    fn save_creates_valid_readable_file() {
        let tmp = tempfile::tempdir().unwrap();
        with_fake_appdata(tmp.path(), || {
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();

            let mut chain = ConfigChain::load("TestProfile").unwrap();
            chain.config_mut().filter.spam_threshold = 42.0;
            chain.config_mut().general.field_score_name = "CustomName".to_string();
            chain.save().unwrap();

            // Verify: the saved file can be read back and parsed
            let profile_path = chain.profile_config_path();
            assert!(profile_path.exists(), "profile config file should exist");

            let read_data = IniFile::read(&profile_path).unwrap();
            // Values should be present and parseable
            assert_eq!(
                read_data.get("Filter").and_then(|s| s.get("spam_threshold")),
                Some(&"42.0".to_string())
            );
            assert_eq!(
                read_data.get("General").and_then(|s| s.get("field_score_name")),
                Some(&"CustomName".to_string())
            );

            // No temp files should be left behind
            let dir_entries: Vec<_> = fs::read_dir(&data_dir).unwrap().collect();
            for entry in &dir_entries {
                let name = entry.as_ref().unwrap().file_name();
                let name_str = name.to_str().unwrap();
                assert!(
                    !name_str.starts_with('.') || !name_str.ends_with(".tmp"),
                    "temp file should not remain: {name_str}"
                );
            }
        });
    }

    // ─── Test: BAYESCUSTOMIZE override ──────────────────────────────────

    #[test]
    fn load_from_env_loads_specified_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file1 = tmp.path().join("first.ini");
        let file2 = tmp.path().join("second.ini");

        fs::write(&file1, "[Filter]\nspam_threshold = 60.0\n").unwrap();
        fs::write(&file2, "[Filter]\nspam_threshold = 70.0\nunsure_threshold = 20.0\n").unwrap();

        let env_value = format!(
            "{};{}",
            file1.to_str().unwrap(),
            file2.to_str().unwrap()
        );

        let chain = ConfigChain::load_from_env(&env_value).unwrap();

        // Second file overrides first for spam_threshold
        assert_eq!(chain.config().filter.spam_threshold, 70.0);
        // Second file's unsure_threshold applied
        assert_eq!(chain.config().filter.unsure_threshold, 20.0);
        // Built-in defaults for unset values
        assert_eq!(chain.config().filter.enabled, false);
    }

    // ─── Test: BAYESCUSTOMIZE with missing file → warning + defaults ────

    #[test]
    fn load_from_env_missing_file_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_path = tmp.path().join("nonexistent.ini");

        let env_value = missing_path.to_str().unwrap().to_string();
        let chain = ConfigChain::load_from_env(&env_value).unwrap();

        // All values should be built-in defaults
        let defaults = AppConfig::default();
        assert_eq!(chain.config().filter.spam_threshold, defaults.filter.spam_threshold);
        assert_eq!(chain.config().filter.unsure_threshold, defaults.filter.unsure_threshold);
        assert_eq!(chain.config().filter.enabled, defaults.filter.enabled);
        assert_eq!(chain.config().general.field_score_name, defaults.general.field_score_name);
    }

    // ─── Test: config() accessor returns expected reference ─────────────

    #[test]
    fn config_accessor_returns_merged_config() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            let data_dir = tmp.path().join("SpamBayes").join("testprofile");
            fs::create_dir_all(&data_dir).unwrap();

            fs::write(
                data_dir.join("testprofile_bayes_customize.ini"),
                "[Filter]\nenabled = True\n",
            ).unwrap();

            ConfigChain::load("TestProfile").unwrap()
        });

        assert_eq!(chain.config().filter.enabled, true);
    }

    // ─── Test: profile_config_path and default_config_path ──────────────

    #[test]
    fn path_accessors_return_correct_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = with_fake_appdata(tmp.path(), || {
            ConfigChain::load("My Profile").unwrap()
        });

        let data_dir = tmp.path().join("SpamBayes").join("my_profile");
        assert_eq!(
            chain.profile_config_path(),
            data_dir.join("my_profile_bayes_customize.ini")
        );
        assert_eq!(
            chain.default_config_path(),
            data_dir.join("default_bayes_customize.ini")
        );
    }

    // ─── Test: BAYESCUSTOMIZE with mix of valid and missing files ────────

    #[test]
    fn load_from_env_skips_missing_uses_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let valid_file = tmp.path().join("valid.ini");
        let missing_file = tmp.path().join("missing.ini");

        fs::write(&valid_file, "[General]\nfield_score_name = Custom\n").unwrap();

        let env_value = format!(
            "{};{}",
            missing_file.to_str().unwrap(),
            valid_file.to_str().unwrap()
        );

        let chain = ConfigChain::load_from_env(&env_value).unwrap();

        // Valid file's value applied
        assert_eq!(chain.config().general.field_score_name, "Custom");
    }
}
