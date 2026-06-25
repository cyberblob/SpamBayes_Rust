//! Timer-based filtering state machine.
//!
//! This module implements the timer state machine that controls when messages
//! are processed after receiving new message events. The state machine is a
//! logical abstraction — it does not use real Windows timers. Instead, it
//! provides methods that the COM shell layer calls when timers fire.
//!
//! # State Machine Phases
//!
//! - **Idle**: No pending messages, no timers active. Waiting for a new message event.
//! - **`WaitingStartDelay`**: A new message arrived; waiting for the start delay to elapse.
//!   If another message arrives during this phase, the delay restarts.
//! - **Processing**: The start delay fired; processing one message per interval tick.
//!
//! # Timer Configuration Validation
//!
//! Both `timer_start_delay` and `timer_interval` must be within the valid range
//! of 0.4 to 60 seconds. If either value is outside this range, timer-based
//! filtering is disabled and a diagnostic is logged.
//!
//! # Receive Folder Filtering
//!
//! When `timer_only_receive_folders` is enabled, timer-based filtering only
//! applies to folders that directly receive new mail from the server. Messages
//! in non-receive watched folders are processed immediately without delay.
//!
//! **Validates: Requirements 7.1, 7.2, 7.3, 7.4, 7.5, 7.6**

use std::collections::VecDeque;

use spambayes_config::{EntryId, FilterConfig, StoreId};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Minimum valid timer value in seconds.
const TIMER_MIN_SECS: f64 = 0.4;

/// Maximum valid timer value in seconds.
const TIMER_MAX_SECS: f64 = 60.0;

// ─── MessageRef ──────────────────────────────────────────────────────────────

/// A reference to a pending message in the filter queue.
///
/// Contains the folder and message entry IDs needed to locate and process
/// the message later via the MAPI store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    /// The store ID of the folder containing the message.
    pub store_id: StoreId,
    /// The entry ID of the folder containing the message.
    pub folder_entry_id: EntryId,
    /// The entry ID of the message itself.
    pub message_entry_id: EntryId,
}

// ─── TimerPhase ──────────────────────────────────────────────────────────────

/// The current phase of the timer state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerPhase {
    /// No pending messages, no timers active.
    Idle,
    /// Waiting for the start delay to elapse after a new message event.
    WaitingStartDelay,
    /// Processing messages one per interval tick.
    Processing,
}

// ─── TimerCommand ────────────────────────────────────────────────────────────

/// Commands returned by the state machine to instruct the COM layer
/// what timer operations to perform.
#[derive(Debug, Clone, PartialEq)]
pub enum TimerCommand {
    /// Start (or restart) the start-delay timer with the given duration in ms.
    StartDelayTimer(u32),
    /// Start the interval timer with the given duration in ms.
    StartIntervalTimer(u32),
    /// Stop all active timers.
    StopTimer,
    /// Process this message immediately (no timer delay).
    ProcessImmediately(MessageRef),
}

// ─── TimerFilterState ────────────────────────────────────────────────────────

/// Timer-based filtering state machine.
///
/// Manages the queue of pending messages and the timer phase transitions.
/// The COM layer calls methods on this struct when events occur (new message,
/// timer fired) and receives [`TimerCommand`]s indicating what timer operations
/// to perform.
///
/// # Usage
///
/// ```ignore
/// let mut state = TimerFilterState::new(&config);
///
/// // When a new message arrives:
/// let commands = state.on_new_message(msg_ref, is_receive_folder);
/// // Execute the returned commands (start/restart timers, or process immediately)
///
/// // When the start delay timer fires:
/// let commands = state.on_start_delay_fired();
/// // Execute the returned commands (start interval timer, process first message)
///
/// // When the interval timer ticks:
/// let commands = state.on_interval_tick();
/// // Execute the returned commands (process next message, or stop timer)
/// ```
#[derive(Debug)]
pub struct TimerFilterState {
    /// Current phase of the state machine.
    phase: TimerPhase,
    /// Queue of messages waiting to be processed.
    pending: VecDeque<MessageRef>,
    /// Start delay in milliseconds (converted from config seconds).
    start_delay_ms: u32,
    /// Interval between processing ticks in milliseconds.
    interval_ms: u32,
    /// Whether the timer configuration is valid.
    timer_valid: bool,
    /// Whether to apply timer only to receive folders.
    only_receive_folders: bool,
    /// Diagnostic messages logged during operation.
    diagnostics: Vec<String>,
}

impl TimerFilterState {
    /// Create a new `TimerFilterState` from the filter configuration.
    ///
    /// Validates timer values and disables timer-based filtering if either
    /// `timer_start_delay` or `timer_interval` is outside the valid range
    /// (0.4 to 60 seconds).
    ///
    /// **Validates: Requirement 7.5**
    #[must_use]
    pub fn new(config: &FilterConfig) -> Self {
        let mut diagnostics = Vec::new();
        let timer_valid = Self::validate_timer_config(config, &mut diagnostics);

        let start_delay_ms = (config.timer_start_delay * 1000.0) as u32;
        let interval_ms = (config.timer_interval * 1000.0) as u32;

        Self {
            phase: TimerPhase::Idle,
            pending: VecDeque::new(),
            start_delay_ms,
            interval_ms,
            timer_valid,
            only_receive_folders: config.timer_only_receive_folders,
            diagnostics,
        }
    }

    /// Validate timer configuration values are within the valid range.
    ///
    /// Returns `true` if both values are valid, `false` otherwise.
    /// Logs diagnostic messages for invalid values.
    fn validate_timer_config(config: &FilterConfig, diagnostics: &mut Vec<String>) -> bool {
        let mut valid = true;

        if config.timer_start_delay < TIMER_MIN_SECS
            || config.timer_start_delay > TIMER_MAX_SECS
        {
            diagnostics.push(format!(
                "Timer start delay {:.2}s is outside valid range ({:.1}-{:.1}s), \
                 disabling timer-based filtering",
                config.timer_start_delay, TIMER_MIN_SECS, TIMER_MAX_SECS
            ));
            valid = false;
        }

        if config.timer_interval < TIMER_MIN_SECS
            || config.timer_interval > TIMER_MAX_SECS
        {
            diagnostics.push(format!(
                "Timer interval {:.2}s is outside valid range ({:.1}-{:.1}s), \
                 disabling timer-based filtering",
                config.timer_interval, TIMER_MIN_SECS, TIMER_MAX_SECS
            ));
            valid = false;
        }

        valid
    }

    /// Handle a new message event from a folder.
    ///
    /// If `timer_only_receive_folders` is enabled and this is NOT a receive
    /// folder, the message is returned for immediate processing (no timer delay).
    ///
    /// If timer-based filtering is disabled (invalid config), the message is
    /// also returned for immediate processing.
    ///
    /// Otherwise, the message is queued and the start delay timer is
    /// started/restarted.
    ///
    /// **Validates: Requirements 7.1, 7.2, 7.6**
    pub fn on_new_message(
        &mut self,
        msg_ref: MessageRef,
        is_receive_folder: bool,
    ) -> Vec<TimerCommand> {
        // If timer is invalid, process immediately
        if !self.timer_valid {
            return vec![TimerCommand::ProcessImmediately(msg_ref)];
        }

        // If timer_only_receive_folders is enabled and this is NOT a receive
        // folder, process immediately without timer delay (Req 7.6)
        if self.only_receive_folders && !is_receive_folder {
            return vec![TimerCommand::ProcessImmediately(msg_ref)];
        }

        // Enqueue the message
        self.pending.push_back(msg_ref);

        // Cancel existing timer and restart start delay (Req 7.1, 7.2)
        self.phase = TimerPhase::WaitingStartDelay;
        vec![TimerCommand::StartDelayTimer(self.start_delay_ms)]
    }

    /// Handle the start delay timer firing.
    ///
    /// Transitions to the Processing phase and processes the first pending
    /// message. If more messages remain, starts the interval timer.
    ///
    /// **Validates: Requirement 7.3**
    pub fn on_start_delay_fired(&mut self) -> Vec<TimerCommand> {
        if self.phase != TimerPhase::WaitingStartDelay {
            // Spurious timer fire — ignore
            return vec![];
        }

        self.phase = TimerPhase::Processing;

        let mut commands = Vec::new();

        // Process the first pending message
        if let Some(msg_ref) = self.pending.pop_front() {
            commands.push(TimerCommand::ProcessImmediately(msg_ref));
        }

        // If more messages remain, start the interval timer
        if self.pending.is_empty() {
            // No more messages — return to idle (Req 7.4)
            self.phase = TimerPhase::Idle;
            commands.push(TimerCommand::StopTimer);
        } else {
            commands.push(TimerCommand::StartIntervalTimer(self.interval_ms));
        }

        commands
    }

    /// Handle an interval timer tick.
    ///
    /// Processes one pending message per tick. When no messages remain,
    /// stops the timer and returns to Idle.
    ///
    /// **Validates: Requirements 7.3, 7.4**
    pub fn on_interval_tick(&mut self) -> Vec<TimerCommand> {
        if self.phase != TimerPhase::Processing {
            // Spurious tick — ignore
            return vec![];
        }

        let mut commands = Vec::new();

        // Process one message per tick
        if let Some(msg_ref) = self.pending.pop_front() {
            commands.push(TimerCommand::ProcessImmediately(msg_ref));
        }

        // If queue is now empty, stop timer and return to idle (Req 7.4)
        if self.pending.is_empty() {
            self.phase = TimerPhase::Idle;
            commands.push(TimerCommand::StopTimer);
        }

        commands
    }

    /// Returns whether the timer configuration is valid.
    ///
    /// If `false`, timer-based filtering is disabled and messages should be
    /// processed immediately.
    #[must_use]
    pub fn is_timer_valid(&self) -> bool {
        self.timer_valid
    }

    /// Returns the current phase of the state machine.
    #[must_use]
    pub fn phase(&self) -> TimerPhase {
        self.phase
    }

    /// Returns the number of pending messages in the queue.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns any diagnostic messages generated during initialization.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Returns the configured start delay in milliseconds.
    #[must_use]
    pub fn start_delay_ms(&self) -> u32 {
        self.start_delay_ms
    }

    /// Returns the configured interval in milliseconds.
    #[must_use]
    pub fn interval_ms(&self) -> u32 {
        self.interval_ms
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a `MessageRef` with simple test values.
    fn msg_ref(id: &str) -> MessageRef {
        MessageRef {
            store_id: StoreId::new("STORE01"),
            folder_entry_id: EntryId::new("FOLDER01"),
            message_entry_id: EntryId::new(id),
        }
    }

    /// Helper to create a default valid `FilterConfig` for timer tests.
    fn timer_config() -> FilterConfig {
        FilterConfig {
            timer_enabled: true,
            timer_start_delay: 2.0,
            timer_interval: 1.0,
            timer_only_receive_folders: true,
            ..FilterConfig::default()
        }
    }

    // ── Initialization Tests ─────────────────────────────────────────────

    #[test]
    fn new_state_starts_idle() {
        let config = timer_config();
        let state = TimerFilterState::new(&config);

        assert_eq!(state.phase(), TimerPhase::Idle);
        assert_eq!(state.pending_count(), 0);
        assert!(state.is_timer_valid());
        assert!(state.diagnostics().is_empty());
    }

    #[test]
    fn new_state_converts_seconds_to_ms() {
        let config = FilterConfig {
            timer_start_delay: 2.0,
            timer_interval: 1.0,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);

        assert_eq!(state.start_delay_ms(), 2000);
        assert_eq!(state.interval_ms(), 1000);
    }

    // ── Validation Tests (Req 7.5) ──────────────────────────────────────

    #[test]
    fn invalid_start_delay_too_low_disables_timer() {
        let config = FilterConfig {
            timer_start_delay: 0.3,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);

        assert!(!state.is_timer_valid());
        assert_eq!(state.diagnostics().len(), 1);
        assert!(state.diagnostics()[0].contains("start delay"));
    }

    #[test]
    fn invalid_start_delay_too_high_disables_timer() {
        let config = FilterConfig {
            timer_start_delay: 61.0,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);

        assert!(!state.is_timer_valid());
        assert!(state.diagnostics()[0].contains("start delay"));
    }

    #[test]
    fn invalid_interval_too_low_disables_timer() {
        let config = FilterConfig {
            timer_interval: 0.1,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);

        assert!(!state.is_timer_valid());
        assert!(state.diagnostics()[0].contains("interval"));
    }

    #[test]
    fn invalid_interval_too_high_disables_timer() {
        let config = FilterConfig {
            timer_interval: 100.0,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);

        assert!(!state.is_timer_valid());
        assert!(state.diagnostics()[0].contains("interval"));
    }

    #[test]
    fn both_invalid_produces_two_diagnostics() {
        let config = FilterConfig {
            timer_start_delay: 0.1,
            timer_interval: 100.0,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);

        assert!(!state.is_timer_valid());
        assert_eq!(state.diagnostics().len(), 2);
    }

    #[test]
    fn boundary_values_are_valid() {
        // Exactly at min boundary
        let config = FilterConfig {
            timer_start_delay: 0.4,
            timer_interval: 0.4,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);
        assert!(state.is_timer_valid());

        // Exactly at max boundary
        let config = FilterConfig {
            timer_start_delay: 60.0,
            timer_interval: 60.0,
            ..timer_config()
        };
        let state = TimerFilterState::new(&config);
        assert!(state.is_timer_valid());
    }

    // ── New Message Tests (Req 7.1, 7.2) ────────────────────────────────

    #[test]
    fn on_new_message_receive_folder_starts_delay() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        let commands = state.on_new_message(msg_ref("MSG01"), true);

        assert_eq!(state.phase(), TimerPhase::WaitingStartDelay);
        assert_eq!(state.pending_count(), 1);
        assert_eq!(commands, vec![TimerCommand::StartDelayTimer(2000)]);
    }

    #[test]
    fn on_new_message_restarts_delay_on_second_message() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        state.on_new_message(msg_ref("MSG01"), true);
        let commands = state.on_new_message(msg_ref("MSG02"), true);

        // Delay should be restarted (Req 7.2)
        assert_eq!(state.phase(), TimerPhase::WaitingStartDelay);
        assert_eq!(state.pending_count(), 2);
        assert_eq!(commands, vec![TimerCommand::StartDelayTimer(2000)]);
    }

    // ── Non-Receive Folder Tests (Req 7.6) ──────────────────────────────

    #[test]
    fn non_receive_folder_processes_immediately_when_option_enabled() {
        let config = FilterConfig {
            timer_only_receive_folders: true,
            ..timer_config()
        };
        let mut state = TimerFilterState::new(&config);

        let commands = state.on_new_message(msg_ref("MSG01"), false);

        // Should process immediately, no timer delay
        assert_eq!(state.phase(), TimerPhase::Idle);
        assert_eq!(state.pending_count(), 0);
        assert_eq!(
            commands,
            vec![TimerCommand::ProcessImmediately(msg_ref("MSG01"))]
        );
    }

    #[test]
    fn non_receive_folder_uses_timer_when_option_disabled() {
        let config = FilterConfig {
            timer_only_receive_folders: false,
            ..timer_config()
        };
        let mut state = TimerFilterState::new(&config);

        let commands = state.on_new_message(msg_ref("MSG01"), false);

        // Should use timer since option is disabled
        assert_eq!(state.phase(), TimerPhase::WaitingStartDelay);
        assert_eq!(commands, vec![TimerCommand::StartDelayTimer(2000)]);
    }

    // ── Invalid Config Immediate Processing (Req 7.5) ───────────────────

    #[test]
    fn invalid_config_processes_immediately() {
        let config = FilterConfig {
            timer_start_delay: 0.1, // invalid
            ..timer_config()
        };
        let mut state = TimerFilterState::new(&config);

        let commands = state.on_new_message(msg_ref("MSG01"), true);

        assert_eq!(state.phase(), TimerPhase::Idle);
        assert_eq!(state.pending_count(), 0);
        assert_eq!(
            commands,
            vec![TimerCommand::ProcessImmediately(msg_ref("MSG01"))]
        );
    }

    // ── Start Delay Fired Tests (Req 7.3) ───────────────────────────────

    #[test]
    fn on_start_delay_fired_processes_first_message() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        state.on_new_message(msg_ref("MSG01"), true);
        let commands = state.on_start_delay_fired();

        // Should process the first message and stop (only one pending)
        assert_eq!(state.phase(), TimerPhase::Idle);
        assert_eq!(state.pending_count(), 0);
        assert_eq!(
            commands,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG01")),
                TimerCommand::StopTimer,
            ]
        );
    }

    #[test]
    fn on_start_delay_fired_with_multiple_starts_interval() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        state.on_new_message(msg_ref("MSG01"), true);
        state.on_new_message(msg_ref("MSG02"), true);
        state.on_new_message(msg_ref("MSG03"), true);

        let commands = state.on_start_delay_fired();

        // Should process first message and start interval timer
        assert_eq!(state.phase(), TimerPhase::Processing);
        assert_eq!(state.pending_count(), 2); // MSG02, MSG03 still pending
        assert_eq!(
            commands,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG01")),
                TimerCommand::StartIntervalTimer(1000),
            ]
        );
    }

    #[test]
    fn on_start_delay_fired_ignored_when_idle() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        // Spurious fire when idle
        let commands = state.on_start_delay_fired();

        assert_eq!(state.phase(), TimerPhase::Idle);
        assert!(commands.is_empty());
    }

    // ── Interval Tick Tests (Req 7.3, 7.4) ──────────────────────────────

    #[test]
    fn on_interval_tick_processes_one_message() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        state.on_new_message(msg_ref("MSG01"), true);
        state.on_new_message(msg_ref("MSG02"), true);
        state.on_new_message(msg_ref("MSG03"), true);

        // Fire start delay → processes MSG01
        state.on_start_delay_fired();

        // First interval tick → processes MSG02
        let commands = state.on_interval_tick();

        assert_eq!(state.phase(), TimerPhase::Processing);
        assert_eq!(state.pending_count(), 1); // MSG03 still pending
        assert_eq!(
            commands,
            vec![TimerCommand::ProcessImmediately(msg_ref("MSG02"))]
        );
    }

    #[test]
    fn on_interval_tick_stops_when_queue_empty() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        state.on_new_message(msg_ref("MSG01"), true);
        state.on_new_message(msg_ref("MSG02"), true);

        // Fire start delay → processes MSG01, starts interval
        state.on_start_delay_fired();

        // First interval tick → processes MSG02, queue empty → stops
        let commands = state.on_interval_tick();

        assert_eq!(state.phase(), TimerPhase::Idle);
        assert_eq!(state.pending_count(), 0);
        assert_eq!(
            commands,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG02")),
                TimerCommand::StopTimer,
            ]
        );
    }

    #[test]
    fn on_interval_tick_ignored_when_idle() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        let commands = state.on_interval_tick();

        assert_eq!(state.phase(), TimerPhase::Idle);
        assert!(commands.is_empty());
    }

    // ── Full Lifecycle Tests ─────────────────────────────────────────────

    #[test]
    fn full_lifecycle_single_message() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        // 1. New message arrives → starts delay
        let cmds = state.on_new_message(msg_ref("MSG01"), true);
        assert_eq!(cmds, vec![TimerCommand::StartDelayTimer(2000)]);
        assert_eq!(state.phase(), TimerPhase::WaitingStartDelay);

        // 2. Start delay fires → process message, stop timer
        let cmds = state.on_start_delay_fired();
        assert_eq!(
            cmds,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG01")),
                TimerCommand::StopTimer,
            ]
        );
        assert_eq!(state.phase(), TimerPhase::Idle);
    }

    #[test]
    fn full_lifecycle_multiple_messages_with_restart() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        // 1. First message → starts delay
        state.on_new_message(msg_ref("MSG01"), true);
        assert_eq!(state.phase(), TimerPhase::WaitingStartDelay);

        // 2. Second message arrives → restarts delay (Req 7.2)
        let cmds = state.on_new_message(msg_ref("MSG02"), true);
        assert_eq!(cmds, vec![TimerCommand::StartDelayTimer(2000)]);
        assert_eq!(state.pending_count(), 2);

        // 3. Start delay fires → process MSG01, start interval
        let cmds = state.on_start_delay_fired();
        assert_eq!(
            cmds,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG01")),
                TimerCommand::StartIntervalTimer(1000),
            ]
        );
        assert_eq!(state.phase(), TimerPhase::Processing);

        // 4. Interval tick → process MSG02, stop timer
        let cmds = state.on_interval_tick();
        assert_eq!(
            cmds,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG02")),
                TimerCommand::StopTimer,
            ]
        );
        assert_eq!(state.phase(), TimerPhase::Idle);
    }

    #[test]
    fn new_message_during_processing_phase_queues_it() {
        let config = timer_config();
        let mut state = TimerFilterState::new(&config);

        // Setup: get into Processing phase
        state.on_new_message(msg_ref("MSG01"), true);
        state.on_new_message(msg_ref("MSG02"), true);
        state.on_start_delay_fired(); // processes MSG01, starts interval

        assert_eq!(state.phase(), TimerPhase::Processing);
        assert_eq!(state.pending_count(), 1); // MSG02

        // New message arrives during processing
        let cmds = state.on_new_message(msg_ref("MSG03"), true);

        // Restarts the delay timer even during processing
        assert_eq!(state.phase(), TimerPhase::WaitingStartDelay);
        assert_eq!(state.pending_count(), 2); // MSG02, MSG03
        assert_eq!(cmds, vec![TimerCommand::StartDelayTimer(2000)]);
    }

    #[test]
    fn custom_timer_values() {
        let config = FilterConfig {
            timer_start_delay: 5.0,
            timer_interval: 0.5,
            ..timer_config()
        };
        let mut state = TimerFilterState::new(&config);

        let cmds = state.on_new_message(msg_ref("MSG01"), true);
        assert_eq!(cmds, vec![TimerCommand::StartDelayTimer(5000)]);

        state.on_new_message(msg_ref("MSG02"), true);
        let cmds = state.on_start_delay_fired();
        assert_eq!(
            cmds,
            vec![
                TimerCommand::ProcessImmediately(msg_ref("MSG01")),
                TimerCommand::StartIntervalTimer(500),
            ]
        );
    }
}
