//! Notification sound state machine.
//!
//! This module implements the notification sound logic that plays a WAV file
//! after a batch of messages has been classified. It uses an accumulate timer
//! to batch notifications together: when the timer fires, exactly one sound is
//! played based on the highest-priority classification in the batch.
//!
//! # Priority Order
//!
//! Ham (highest) > Unsure > Spam (lowest).
//!
//! If the highest-priority classification has no configured sound file, the
//! system falls through to the next lower-priority classification that does.
//!
//! # Timer Behavior
//!
//! Each new classification starts (or restarts) the accumulate timer. When the
//! timer fires without further interruption, the notification is played and the
//! pending batch is cleared.
//!
//! # Accumulate Delay Validation
//!
//! The delay must be within 0.5 to 60.0 seconds. If outside that range, a
//! diagnostic is logged but the timer still functions with the configured value.
//!
//! **Validates: Requirements 15.1, 15.2, 15.3, 15.4, 15.5, 15.6, 15.7**

use std::path::{Path, PathBuf};

use spambayes_config::NotificationConfig;
use spambayes_core::Classification;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Minimum valid accumulate delay in seconds.
const ACCUMULATE_MIN_SECS: f64 = 0.5;

/// Maximum valid accumulate delay in seconds.
const ACCUMULATE_MAX_SECS: f64 = 60.0;

// ─── NotificationCommand ─────────────────────────────────────────────────────

/// Commands returned by the notification state machine to instruct the COM
/// layer what operations to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationCommand {
    /// Start (or restart) the accumulate timer with the given duration in ms.
    StartAccumulateTimer(u32),
    /// Stop the accumulate timer.
    StopAccumulateTimer,
    /// Play the WAV sound file at the given path.
    PlaySound(PathBuf),
}

// ─── NotificationManager ─────────────────────────────────────────────────────

/// Notification sound state machine.
///
/// Manages pending classifications and determines which sound to play when
/// the accumulate timer fires. The COM shell layer calls methods on this
/// struct and receives [`NotificationCommand`]s indicating what to do.
///
/// # Usage
///
/// ```ignore
/// let mut mgr = NotificationManager::new(&config);
///
/// // When a message is classified:
/// let commands = mgr.record_classification(Classification::Ham);
/// // Execute the returned commands (start/restart timer)
///
/// // When the accumulate timer fires:
/// let commands = mgr.on_accumulate_timer_fire();
/// // Execute the returned commands (play sound)
/// ```
#[derive(Debug)]
pub struct NotificationManager {
    /// Whether sound notifications are enabled.
    enabled: bool,
    /// Path to the ham notification sound.
    ham_sound: String,
    /// Path to the unsure notification sound.
    unsure_sound: String,
    /// Path to the spam notification sound.
    spam_sound: String,
    /// Accumulate delay in milliseconds.
    accumulate_delay_ms: u32,
    /// Whether the accumulate timer is currently active.
    timer_active: bool,
    /// Pending classifications accumulated since the last notification.
    pending_classifications: Vec<Classification>,
    /// Diagnostic messages generated during initialization or operation.
    diagnostics: Vec<String>,
}

impl NotificationManager {
    /// Create a new `NotificationManager` from the notification configuration.
    ///
    /// Validates the accumulate delay and logs a diagnostic if it is outside
    /// the valid range (0.5 to 60.0 seconds). The timer still operates with
    /// the configured value even if out of range.
    #[must_use]
    pub fn new(config: &NotificationConfig) -> Self {
        let mut diagnostics = Vec::new();

        if config.notify_accumulate_delay < ACCUMULATE_MIN_SECS
            || config.notify_accumulate_delay > ACCUMULATE_MAX_SECS
        {
            diagnostics.push(format!(
                "Accumulate delay {:.2}s is outside valid range ({:.1}-{:.1}s)",
                config.notify_accumulate_delay, ACCUMULATE_MIN_SECS, ACCUMULATE_MAX_SECS
            ));
        }

        let accumulate_delay_ms = (config.notify_accumulate_delay * 1000.0) as u32;

        Self {
            enabled: config.notify_sound_enabled,
            ham_sound: config.notify_ham_sound.clone(),
            unsure_sound: config.notify_unsure_sound.clone(),
            spam_sound: config.notify_spam_sound.clone(),
            accumulate_delay_ms,
            timer_active: false,
            pending_classifications: Vec::new(),
            diagnostics,
        }
    }

    /// Record a classification and start/restart the accumulate timer.
    ///
    /// If notifications are disabled, this is a no-op (returns no commands).
    /// Otherwise the classification is appended to the pending batch and the
    /// accumulate timer is started or restarted.
    ///
    /// **Validates: Requirements 15.5, 15.6**
    pub fn record_classification(
        &mut self,
        classification: Classification,
    ) -> Vec<NotificationCommand> {
        if !self.enabled {
            return vec![];
        }

        self.pending_classifications.push(classification);
        self.restart_accumulate_timer()
    }

    /// Start or restart the accumulate timer.
    ///
    /// Returns the command to start the timer with the configured delay.
    ///
    /// **Validates: Requirements 15.5, 15.6**
    pub fn restart_accumulate_timer(&mut self) -> Vec<NotificationCommand> {
        self.timer_active = true;
        vec![NotificationCommand::StartAccumulateTimer(
            self.accumulate_delay_ms,
        )]
    }

    /// Handle the accumulate timer firing.
    ///
    /// Determines which sound to play based on the highest-priority
    /// classification present in the pending batch, with fall-through when
    /// the highest-priority sound is not configured.
    ///
    /// Priority order: Ham > Unsure > Spam.
    ///
    /// After determining the sound (or finding none configured), clears
    /// the pending classifications.
    ///
    /// **Validates: Requirements 15.1, 15.2, 15.3, 15.4, 15.7**
    pub fn on_accumulate_timer_fire(&mut self) -> Vec<NotificationCommand> {
        self.timer_active = false;

        if self.pending_classifications.is_empty() {
            return vec![];
        }

        let has_ham = self
            .pending_classifications.contains(&Classification::Ham);
        let has_unsure = self
            .pending_classifications.contains(&Classification::Unsure);
        let has_spam = self
            .pending_classifications.contains(&Classification::Spam);

        // Clear pending classifications regardless of outcome
        self.pending_classifications.clear();

        // Determine the sound to play using priority + fall-through logic.
        // Priority: Ham > Unsure > Spam.
        // Fall-through: if a classification is present but its sound is not
        // configured, try the next lower priority that IS present and configured.
        let sound_path = self.select_sound(has_ham, has_unsure, has_spam);

        match sound_path {
            Some(path) => vec![NotificationCommand::PlaySound(path)],
            None => vec![],
        }
    }

    /// Select the appropriate sound file based on priority and fall-through.
    ///
    /// Iterates through classifications in priority order (Ham, Unsure, Spam).
    /// For each classification present in the batch, checks if a sound is
    /// configured. Returns the first configured sound found.
    ///
    /// **Validates: Requirement 15.7**
    fn select_sound(
        &self,
        has_ham: bool,
        has_unsure: bool,
        has_spam: bool,
    ) -> Option<PathBuf> {
        // Priority order: Ham > Unsure > Spam
        // Only consider a classification if it is present in the batch.
        // Fall-through: if present but no sound configured, try next.
        if has_ham && !self.ham_sound.is_empty() {
            return Some(PathBuf::from(&self.ham_sound));
        }
        if has_unsure && !self.unsure_sound.is_empty() {
            return Some(PathBuf::from(&self.unsure_sound));
        }
        if has_spam && !self.spam_sound.is_empty() {
            return Some(PathBuf::from(&self.spam_sound));
        }
        None
    }

    /// Play a WAV file using the Windows `PlaySound` API.
    ///
    /// On non-Windows platforms, this is a no-op (for testing).
    #[cfg(target_os = "windows")]
    pub fn play_sound(path: &Path) {
        use std::os::windows::ffi::OsStrExt;

        use windows::core::PCWSTR;
        use windows::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_NODEFAULT};

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let _ = PlaySoundW(PCWSTR(wide.as_ptr()), None, SND_FILENAME | SND_NODEFAULT);
        }
    }

    /// Play a WAV file — no-op on non-Windows platforms.
    #[cfg(not(target_os = "windows"))]
    pub fn play_sound(_path: &Path) {
        // No-op for testing on non-Windows.
    }

    /// Returns whether sound notifications are enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns whether the accumulate timer is currently active.
    #[must_use]
    pub fn is_timer_active(&self) -> bool {
        self.timer_active
    }

    /// Returns the number of pending classifications in the current batch.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending_classifications.len()
    }

    /// Returns any diagnostic messages generated during initialization.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Returns the configured accumulate delay in milliseconds.
    #[must_use]
    pub fn accumulate_delay_ms(&self) -> u32 {
        self.accumulate_delay_ms
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a default `NotificationConfig` with sounds enabled.
    fn enabled_config() -> NotificationConfig {
        NotificationConfig {
            notify_sound_enabled: true,
            notify_ham_sound: "C:\\sounds\\ham.wav".to_string(),
            notify_unsure_sound: "C:\\sounds\\unsure.wav".to_string(),
            notify_spam_sound: "C:\\sounds\\spam.wav".to_string(),
            notify_accumulate_delay: 10.0,
        }
    }

    /// Helper to create a config with notifications disabled.
    fn disabled_config() -> NotificationConfig {
        NotificationConfig {
            notify_sound_enabled: false,
            ..enabled_config()
        }
    }

    // ── Initialization Tests ─────────────────────────────────────────────

    #[test]
    fn new_manager_starts_with_no_pending() {
        let mgr = NotificationManager::new(&enabled_config());

        assert!(mgr.is_enabled());
        assert!(!mgr.is_timer_active());
        assert_eq!(mgr.pending_count(), 0);
        assert!(mgr.diagnostics().is_empty());
    }

    #[test]
    fn new_manager_converts_delay_to_ms() {
        let config = NotificationConfig {
            notify_accumulate_delay: 10.0,
            ..enabled_config()
        };
        let mgr = NotificationManager::new(&config);

        assert_eq!(mgr.accumulate_delay_ms(), 10000);
    }

    #[test]
    fn new_manager_logs_diagnostic_for_low_delay() {
        let config = NotificationConfig {
            notify_accumulate_delay: 0.3,
            ..enabled_config()
        };
        let mgr = NotificationManager::new(&config);

        assert_eq!(mgr.diagnostics().len(), 1);
        assert!(mgr.diagnostics()[0].contains("Accumulate delay"));
        assert!(mgr.diagnostics()[0].contains("outside valid range"));
    }

    #[test]
    fn new_manager_logs_diagnostic_for_high_delay() {
        let config = NotificationConfig {
            notify_accumulate_delay: 61.0,
            ..enabled_config()
        };
        let mgr = NotificationManager::new(&config);

        assert_eq!(mgr.diagnostics().len(), 1);
        assert!(mgr.diagnostics()[0].contains("outside valid range"));
    }

    #[test]
    fn boundary_delay_values_are_valid() {
        // Minimum boundary: 0.5s
        let config = NotificationConfig {
            notify_accumulate_delay: 0.5,
            ..enabled_config()
        };
        let mgr = NotificationManager::new(&config);
        assert!(mgr.diagnostics().is_empty());
        assert_eq!(mgr.accumulate_delay_ms(), 500);

        // Maximum boundary: 60.0s
        let config = NotificationConfig {
            notify_accumulate_delay: 60.0,
            ..enabled_config()
        };
        let mgr = NotificationManager::new(&config);
        assert!(mgr.diagnostics().is_empty());
        assert_eq!(mgr.accumulate_delay_ms(), 60000);
    }

    // ── Disabled Notifications ───────────────────────────────────────────

    #[test]
    fn disabled_notifications_ignore_classifications() {
        let mut mgr = NotificationManager::new(&disabled_config());

        let commands = mgr.record_classification(Classification::Ham);

        assert!(commands.is_empty());
        assert_eq!(mgr.pending_count(), 0);
        assert!(!mgr.is_timer_active());
    }

    // ── Record Classification (Req 15.5, 15.6) ──────────────────────────

    #[test]
    fn record_classification_starts_timer() {
        let mut mgr = NotificationManager::new(&enabled_config());

        let commands = mgr.record_classification(Classification::Ham);

        assert_eq!(mgr.pending_count(), 1);
        assert!(mgr.is_timer_active());
        assert_eq!(
            commands,
            vec![NotificationCommand::StartAccumulateTimer(10000)]
        );
    }

    #[test]
    fn record_second_classification_restarts_timer() {
        let mut mgr = NotificationManager::new(&enabled_config());

        mgr.record_classification(Classification::Ham);
        let commands = mgr.record_classification(Classification::Spam);

        assert_eq!(mgr.pending_count(), 2);
        assert!(mgr.is_timer_active());
        // Timer is restarted with the full delay
        assert_eq!(
            commands,
            vec![NotificationCommand::StartAccumulateTimer(10000)]
        );
    }

    // ── Timer Fire — Priority Logic (Req 15.1, 15.2, 15.3, 15.4) ────────

    #[test]
    fn timer_fire_plays_ham_when_ham_present() {
        let mut mgr = NotificationManager::new(&enabled_config());

        mgr.record_classification(Classification::Ham);
        mgr.record_classification(Classification::Spam);
        mgr.record_classification(Classification::Unsure);

        let commands = mgr.on_accumulate_timer_fire();

        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\ham.wav"
            ))]
        );
        assert_eq!(mgr.pending_count(), 0);
        assert!(!mgr.is_timer_active());
    }

    #[test]
    fn timer_fire_plays_unsure_when_no_ham() {
        let mut mgr = NotificationManager::new(&enabled_config());

        mgr.record_classification(Classification::Unsure);
        mgr.record_classification(Classification::Spam);

        let commands = mgr.on_accumulate_timer_fire();

        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\unsure.wav"
            ))]
        );
    }

    #[test]
    fn timer_fire_plays_spam_when_only_spam() {
        let mut mgr = NotificationManager::new(&enabled_config());

        mgr.record_classification(Classification::Spam);
        mgr.record_classification(Classification::Spam);

        let commands = mgr.on_accumulate_timer_fire();

        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\spam.wav"
            ))]
        );
    }

    // ── Fall-Through Logic (Req 15.7) ────────────────────────────────────

    #[test]
    fn fall_through_ham_to_unsure() {
        let config = NotificationConfig {
            notify_ham_sound: String::new(), // No ham sound configured
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        mgr.record_classification(Classification::Ham);
        mgr.record_classification(Classification::Unsure);

        let commands = mgr.on_accumulate_timer_fire();

        // Ham is highest priority but no sound configured → fall through to unsure
        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\unsure.wav"
            ))]
        );
    }

    #[test]
    fn fall_through_ham_to_spam_when_no_unsure_present() {
        let config = NotificationConfig {
            notify_ham_sound: String::new(), // No ham sound
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        mgr.record_classification(Classification::Ham);
        mgr.record_classification(Classification::Spam);

        let commands = mgr.on_accumulate_timer_fire();

        // Ham present but no sound → unsure not present → fall through to spam
        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\spam.wav"
            ))]
        );
    }

    #[test]
    fn fall_through_ham_and_unsure_to_spam() {
        let config = NotificationConfig {
            notify_ham_sound: String::new(),   // No ham sound
            notify_unsure_sound: String::new(), // No unsure sound
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        mgr.record_classification(Classification::Ham);
        mgr.record_classification(Classification::Unsure);
        mgr.record_classification(Classification::Spam);

        let commands = mgr.on_accumulate_timer_fire();

        // Falls through all the way to spam
        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\spam.wav"
            ))]
        );
    }

    #[test]
    fn fall_through_no_sound_configured_at_all() {
        let config = NotificationConfig {
            notify_ham_sound: String::new(),
            notify_unsure_sound: String::new(),
            notify_spam_sound: String::new(),
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        mgr.record_classification(Classification::Ham);

        let commands = mgr.on_accumulate_timer_fire();

        // No sound configured for any classification → no PlaySound command
        assert!(commands.is_empty());
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn fall_through_only_considers_present_classifications() {
        // Unsure sound is configured but unsure is NOT present in the batch
        let config = NotificationConfig {
            notify_ham_sound: String::new(), // No ham sound
            notify_unsure_sound: "C:\\sounds\\unsure.wav".to_string(),
            notify_spam_sound: "C:\\sounds\\spam.wav".to_string(),
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        // Only ham and spam in batch — no unsure
        mgr.record_classification(Classification::Ham);
        mgr.record_classification(Classification::Spam);

        let commands = mgr.on_accumulate_timer_fire();

        // Ham present but no ham sound → unsure NOT present → spam present with sound
        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\spam.wav"
            ))]
        );
    }

    // ── Timer Fire Edge Cases ────────────────────────────────────────────

    #[test]
    fn timer_fire_with_no_pending_is_noop() {
        let mut mgr = NotificationManager::new(&enabled_config());

        let commands = mgr.on_accumulate_timer_fire();

        assert!(commands.is_empty());
    }

    #[test]
    fn timer_fire_clears_pending_for_next_batch() {
        let mut mgr = NotificationManager::new(&enabled_config());

        mgr.record_classification(Classification::Ham);
        mgr.on_accumulate_timer_fire();

        // Second batch — only spam
        mgr.record_classification(Classification::Spam);
        let commands = mgr.on_accumulate_timer_fire();

        // Should play spam, not ham from previous batch
        assert_eq!(
            commands,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\spam.wav"
            ))]
        );
    }

    // ── Full Lifecycle Tests ─────────────────────────────────────────────

    #[test]
    fn full_lifecycle_single_classification() {
        let mut mgr = NotificationManager::new(&enabled_config());

        // 1. Message classified as unsure → starts timer
        let cmds = mgr.record_classification(Classification::Unsure);
        assert_eq!(
            cmds,
            vec![NotificationCommand::StartAccumulateTimer(10000)]
        );
        assert!(mgr.is_timer_active());

        // 2. Timer fires → play unsure sound
        let cmds = mgr.on_accumulate_timer_fire();
        assert_eq!(
            cmds,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\unsure.wav"
            ))]
        );
        assert!(!mgr.is_timer_active());
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn full_lifecycle_batch_with_restart() {
        let config = NotificationConfig {
            notify_accumulate_delay: 5.0,
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        // 1. First message → spam
        let cmds = mgr.record_classification(Classification::Spam);
        assert_eq!(
            cmds,
            vec![NotificationCommand::StartAccumulateTimer(5000)]
        );

        // 2. Second message → ham (restarts timer)
        let cmds = mgr.record_classification(Classification::Ham);
        assert_eq!(
            cmds,
            vec![NotificationCommand::StartAccumulateTimer(5000)]
        );
        assert_eq!(mgr.pending_count(), 2);

        // 3. Timer fires → play ham (highest priority)
        let cmds = mgr.on_accumulate_timer_fire();
        assert_eq!(
            cmds,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\ham.wav"
            ))]
        );
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn out_of_range_delay_still_functions() {
        let config = NotificationConfig {
            notify_accumulate_delay: 0.2, // Too low
            ..enabled_config()
        };
        let mut mgr = NotificationManager::new(&config);

        // Should still work despite diagnostic
        assert!(!mgr.diagnostics().is_empty());

        let cmds = mgr.record_classification(Classification::Ham);
        assert_eq!(
            cmds,
            vec![NotificationCommand::StartAccumulateTimer(200)]
        );

        let cmds = mgr.on_accumulate_timer_fire();
        assert_eq!(
            cmds,
            vec![NotificationCommand::PlaySound(PathBuf::from(
                "C:\\sounds\\ham.wav"
            ))]
        );
    }
}
