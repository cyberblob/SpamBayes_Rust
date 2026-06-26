//! Integration test: multi-profile scenario.
//!
//! Verifies that two profiles with different settings can be loaded
//! independently, and that saving changes to one profile does not
//! affect the other.

use std::fs;
use std::sync::Mutex;

use spambayes_config::ConfigChain;

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

/// Two profiles ("Work" and "Personal") with different settings can be
/// loaded independently. Saving changes to one does not affect the other.
#[test]
fn multi_profile_isolation() {
    let tmp = tempfile::tempdir().unwrap();

    with_fake_appdata(tmp.path(), || {
        // ─── Setup: create profile config files ─────────────────────────
        let work_dir = tmp.path().join("SpamBayes").join("work");
        let personal_dir = tmp.path().join("SpamBayes").join("personal");
        fs::create_dir_all(&work_dir).unwrap();
        fs::create_dir_all(&personal_dir).unwrap();

        // Work profile: spam_threshold = 70.0
        fs::write(
            work_dir.join("work_bayes_customize.ini"),
            "[Filter]\nspam_threshold = 70.0\n",
        )
        .unwrap();

        // Personal profile: spam_threshold = 95.0
        fs::write(
            personal_dir.join("personal_bayes_customize.ini"),
            "[Filter]\nspam_threshold = 95.0\n",
        )
        .unwrap();

        // ─── Step 1: Load each profile and verify correct settings ──────
        let work_chain = ConfigChain::load("Work").unwrap();
        assert_eq!(
            work_chain.config().filter.spam_threshold, 70.0,
            "Work profile should have spam_threshold = 70.0"
        );

        let personal_chain = ConfigChain::load("Personal").unwrap();
        assert_eq!(
            personal_chain.config().filter.spam_threshold, 95.0,
            "Personal profile should have spam_threshold = 95.0"
        );

        // Both should have default unsure_threshold since neither overrides it
        assert_eq!(
            work_chain.config().filter.unsure_threshold, 15.0,
            "Work profile should have default unsure_threshold"
        );
        assert_eq!(
            personal_chain.config().filter.unsure_threshold, 15.0,
            "Personal profile should have default unsure_threshold"
        );

        // ─── Step 2: Modify Work's unsure_threshold and save ────────────
        let mut work_chain = work_chain;
        work_chain.config_mut().filter.unsure_threshold = 25.0;
        work_chain.save().unwrap();

        // ─── Step 3: Reload Personal and verify it is unaffected ────────
        let personal_reloaded = ConfigChain::load("Personal").unwrap();
        assert_eq!(
            personal_reloaded.config().filter.spam_threshold, 95.0,
            "Personal spam_threshold should be unchanged after Work save"
        );
        assert_eq!(
            personal_reloaded.config().filter.unsure_threshold, 15.0,
            "Personal unsure_threshold should still be default after Work save"
        );

        // ─── Step 4: Reload Work and verify the change persisted ────────
        let work_reloaded = ConfigChain::load("Work").unwrap();
        assert_eq!(
            work_reloaded.config().filter.spam_threshold, 70.0,
            "Work spam_threshold should persist after reload"
        );
        assert_eq!(
            work_reloaded.config().filter.unsure_threshold, 25.0,
            "Work unsure_threshold should be the saved value"
        );
    });
}
