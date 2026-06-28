//! Profile name sanitization and data directory resolution for filesystem-safe
//! config file naming.

use std::path::PathBuf;

/// Characters that are invalid in Windows filenames.
const INVALID_FILENAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Sanitize a profile name for use as a filename component.
///
/// - Trims leading/trailing whitespace
/// - Replaces spaces with underscores
/// - Replaces invalid filename chars (`\ / : * ? " < > |`) with underscores
/// - Converts to lowercase for case-insensitive matching
/// - If result is empty after sanitization, returns "default"
///
/// # Examples
///
/// ```
/// use spambayes_config::sanitize_profile_name;
///
/// assert_eq!(sanitize_profile_name("My Profile"), "my_profile");
/// assert_eq!(sanitize_profile_name("  Spaces  "), "spaces");
/// assert_eq!(sanitize_profile_name("Has:Special*Chars"), "has_special_chars");
/// assert_eq!(sanitize_profile_name("***"), "default");
/// ```
pub fn sanitize_profile_name(name: &str) -> String {
    let trimmed = name.trim();

    let sanitized: String = trimmed
        .chars()
        .map(|c| {
            if c == ' ' || INVALID_FILENAME_CHARS.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_lowercase();

    // Strip leading/trailing underscores that may have been introduced by
    // trimming whitespace or replacing edge characters.
    let result = sanitized.trim_matches('_').to_string();

    if result.is_empty() {
        "default".to_string()
    } else {
        result
    }
}

/// Resolve the data directory for a given profile name.
///
/// Default: `%LOCALAPPDATA%\SpamBayes\`
/// Override: if `data_directory_override` is `Some` and non-empty, that path is used directly.
///
/// The directory is created if it doesn't already exist.
///
/// Note: Uses `%LOCALAPPDATA%` (not `%APPDATA%`) to match the addin's data directory.
/// Profile name is no longer used as a subdirectory — all config lives in the
/// single `%LOCALAPPDATA%\SpamBayes\` directory with profile-named INI files.
///
/// # Panics
///
/// Panics if `LOCALAPPDATA` is not set and no override is provided.
///
/// # Examples
///
/// ```no_run
/// use spambayes_config::resolve_data_directory;
///
/// // Uses %LOCALAPPDATA%\SpamBayes\
/// let dir = resolve_data_directory("My Profile", None);
///
/// // Uses the override path directly
/// let dir = resolve_data_directory("My Profile", Some(r"C:\Custom\Path"));
/// ```
pub fn resolve_data_directory(profile_name: &str, data_directory_override: Option<&str>) -> PathBuf {
    let _ = profile_name; // Profile name no longer used for subdirectory
    let path = match data_directory_override {
        Some(override_dir) if !override_dir.is_empty() => PathBuf::from(override_dir),
        _ => {
            let local_appdata = std::env::var("LOCALAPPDATA")
                .expect("LOCALAPPDATA environment variable is not set");
            PathBuf::from(local_appdata).join("SpamBayes")
        }
    };

    // Create the directory if it doesn't exist.
    if !path.exists() {
        let _ = std::fs::create_dir_all(&path);
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mutex to serialize tests that modify the LOCALAPPDATA environment variable.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn spaces_replaced_with_underscores() {
        assert_eq!(sanitize_profile_name("My Profile"), "my_profile");
    }

    #[test]
    fn converts_to_lowercase() {
        assert_eq!(sanitize_profile_name("MyProfile"), "myprofile");
        assert_eq!(sanitize_profile_name("LOUD"), "loud");
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(sanitize_profile_name("  hello  "), "hello");
        assert_eq!(sanitize_profile_name("\t tabs \t"), "tabs");
    }

    #[test]
    fn replaces_invalid_chars() {
        assert_eq!(sanitize_profile_name("a\\b/c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn empty_input_returns_default() {
        assert_eq!(sanitize_profile_name(""), "default");
        assert_eq!(sanitize_profile_name("   "), "default");
    }

    #[test]
    fn all_invalid_chars_returns_default() {
        assert_eq!(sanitize_profile_name("***"), "default");
        assert_eq!(sanitize_profile_name(":::"), "default");
        assert_eq!(sanitize_profile_name("???"), "default");
    }

    #[test]
    fn case_insensitive_matching() {
        // Requirement 3.4: "MyProfile" and "myprofile" should produce the same result
        assert_eq!(
            sanitize_profile_name("MyProfile"),
            sanitize_profile_name("myprofile")
        );
    }

    #[test]
    fn special_chars_replaced_with_underscores() {
        // Requirement 3.3: special characters replaced with underscores
        assert_eq!(sanitize_profile_name("User:Admin"), "user_admin");
    }

    #[test]
    fn already_clean_name_unchanged() {
        assert_eq!(sanitize_profile_name("myprofile"), "myprofile");
    }

    #[test]
    fn mixed_valid_and_invalid() {
        assert_eq!(sanitize_profile_name("Work Profile (Main)"), "work_profile_(main)");
    }

    #[test]
    fn leading_trailing_underscores_trimmed() {
        // If the trimmed input starts/ends with invalid chars, the resulting
        // underscores at the edges are stripped.
        assert_eq!(sanitize_profile_name("*hello*"), "hello");
    }

    // ─── resolve_data_directory tests ───────────────────────────────────

    #[test]
    fn resolve_data_directory_default_uses_localappdata() {
        // Validates: Requirements 5.1
        let _lock = ENV_MUTEX.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fake_localappdata = tmp.path().to_str().unwrap().to_string();

        // Temporarily override LOCALAPPDATA
        let original = std::env::var("LOCALAPPDATA").ok();
        std::env::set_var("LOCALAPPDATA", &fake_localappdata);

        let result = resolve_data_directory("My Profile", None);

        // Restore LOCALAPPDATA
        match original {
            Some(val) => std::env::set_var("LOCALAPPDATA", val),
            None => std::env::remove_var("LOCALAPPDATA"),
        }

        // Should be <LOCALAPPDATA>/SpamBayes (flat, no profile subdirectory)
        let expected = PathBuf::from(&fake_localappdata)
            .join("SpamBayes");
        assert_eq!(result, expected);
    }

    #[test]
    fn resolve_data_directory_override_path_used_when_provided() {
        // Validates: Requirements 5.3, 5.4
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().join("custom_dir");

        let result = resolve_data_directory(
            "Ignored Profile",
            Some(override_path.to_str().unwrap()),
        );

        assert_eq!(result, override_path);
    }

    #[test]
    fn resolve_data_directory_creates_directory_if_missing() {
        // Validates: Requirements 5.2
        let tmp = tempfile::tempdir().unwrap();
        let new_dir = tmp.path().join("brand_new_subdir").join("nested");

        assert!(!new_dir.exists());

        let result = resolve_data_directory("anything", Some(new_dir.to_str().unwrap()));

        assert_eq!(result, new_dir);
        assert!(new_dir.exists(), "directory should have been created");
    }

    #[test]
    fn resolve_data_directory_empty_override_falls_back_to_default() {
        // Validates: Requirements 5.1
        let _lock = ENV_MUTEX.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fake_localappdata = tmp.path().to_str().unwrap().to_string();

        let original = std::env::var("LOCALAPPDATA").ok();
        std::env::set_var("LOCALAPPDATA", &fake_localappdata);

        // Empty string override should behave as if no override was provided
        let result = resolve_data_directory("TestProfile", Some(""));

        match original {
            Some(val) => std::env::set_var("LOCALAPPDATA", val),
            None => std::env::remove_var("LOCALAPPDATA"),
        }

        let expected = PathBuf::from(&fake_localappdata)
            .join("SpamBayes");
        assert_eq!(result, expected);
    }
}
