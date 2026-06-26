//! Integration test: sparse save round-trip
//!
//! Verifies that modifying a single setting and performing a sparse save
//! produces a minimal profile file that persists across reloads.

use std::fs;
use std::sync::Mutex;

use spambayes_config::{AppConfig, ConfigChain};

/// Mutex to serialize tests that modify the APPDATA environment variable,
/// since env vars are process-global.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Helper: set APPDATA to a tempdir, run closure, then restore.
fn with_fake_appdata<F, R>(tmp: &std::path::Path, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _lock = ENV_MUTEX.lock().unwrap();
    let original = std::env::var("APPDATA").ok();
    unsafe { std::env::set_var("APPDATA", tmp.to_str().unwrap()) };

    let result = f();

    match original {
        Some(val) => unsafe { std::env::set_var("APPDATA", val) },
        None => unsafe { std::env::remove_var("APPDATA") },
    }
    result
}

#[test]
fn sparse_save_round_trip_persists_single_change() {
    let tmp = tempfile::tempdir().unwrap();

    with_fake_appdata(tmp.path(), || {
        let profile = "RoundTripTest";

        // ── Step 1: Load fresh config chain (no files on disk → built-in defaults) ──
        let mut chain = ConfigChain::load(profile).unwrap();

        // Verify initial values match AppConfig::default()
        let defaults = AppConfig::default();
        assert_eq!(chain.config().filter.spam_threshold, defaults.filter.spam_threshold);
        assert_eq!(chain.config().filter.unsure_threshold, defaults.filter.unsure_threshold);
        assert_eq!(chain.config().filter.enabled, defaults.filter.enabled);
        assert_eq!(chain.config().general.field_score_name, defaults.general.field_score_name);

        // ── Step 2: Change ONE setting ──
        chain.config_mut().filter.spam_threshold = 55.0;

        // ── Step 3: Save (sparse) ──
        chain.save().unwrap();

        // ── Step 4: Verify the file is minimal (only the one change) ──
        let profile_path = chain.profile_config_path();
        let raw_content = fs::read_to_string(&profile_path)
            .expect("profile config file should exist after save");

        // The file should contain the changed value
        assert!(
            raw_content.contains("spam_threshold"),
            "sparse file should contain the changed key; got:\n{raw_content}"
        );
        assert!(
            raw_content.contains("55"),
            "sparse file should contain the new value; got:\n{raw_content}"
        );

        // The file should NOT contain unchanged default values
        assert!(
            !raw_content.contains("unsure_threshold"),
            "sparse file should not contain unchanged defaults; got:\n{raw_content}"
        );
        assert!(
            !raw_content.contains("enabled"),
            "sparse file should not contain unchanged defaults; got:\n{raw_content}"
        );
        assert!(
            !raw_content.contains("field_score_name"),
            "sparse file should not contain unchanged defaults; got:\n{raw_content}"
        );

        // ── Step 5: Reload a fresh ConfigChain for the same profile ──
        let reloaded = ConfigChain::load(profile).unwrap();

        // The changed setting persists
        assert_eq!(
            reloaded.config().filter.spam_threshold, 55.0,
            "changed setting should persist after reload"
        );

        // Other settings remain at their default values
        assert_eq!(
            reloaded.config().filter.unsure_threshold,
            defaults.filter.unsure_threshold,
            "unchanged settings should remain at defaults after reload"
        );
        assert_eq!(
            reloaded.config().filter.enabled,
            defaults.filter.enabled,
            "unchanged settings should remain at defaults after reload"
        );
        assert_eq!(
            reloaded.config().general.field_score_name,
            defaults.general.field_score_name,
            "unchanged settings should remain at defaults after reload"
        );
    });
}
