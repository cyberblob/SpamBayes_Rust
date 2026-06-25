//! Typed configuration option definitions with defaults.
//!
//! All default values match the Python `SpamBayes` implementation to ensure
//! seamless migration from existing installations.

use std::path::Path;

use crate::errors::ConfigError;
use crate::folder_id::{format_folder_id_list, parse_folder_id_list, FolderId};
use crate::ini_parser::{IniData, IniFile, SectionData};

// ─── FilterAction ────────────────────────────────────────────────────────────

/// Action to take on a message after classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterAction {
    /// Move the message to the target folder.
    Move,
    /// Copy the message to the target folder (original stays in place).
    Copy,
    /// Leave the message untouched in its current folder.
    Untouched,
}

impl FilterAction {
    /// Parse from INI string value (Python format).
    #[must_use]
    pub fn from_ini_str(s: &str) -> Option<Self> {
        match s.trim() {
            "Moved" | "Move" => Some(FilterAction::Move),
            "Copied" | "Copy" => Some(FilterAction::Copy),
            "Untouched" => Some(FilterAction::Untouched),
            _ => None,
        }
    }

    /// Serialize to INI string value (Python format).
    #[must_use]
    pub fn to_ini_str(&self) -> &'static str {
        match self {
            FilterAction::Move => "Moved",
            FilterAction::Copy => "Copied",
            FilterAction::Untouched => "Untouched",
        }
    }
}

// ─── MessageReadState ────────────────────────────────────────────────────────

/// How the message 'read' flag should be modified after an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageReadState {
    /// Do not change the read state.
    None,
    /// Mark the message as read.
    Read,
    /// Mark the message as unread.
    Unread,
}

impl MessageReadState {
    /// Parse from INI string value.
    #[must_use]
    pub fn from_ini_str(s: &str) -> Option<Self> {
        match s.trim() {
            "None" => Some(MessageReadState::None),
            "Read" => Some(MessageReadState::Read),
            "Unread" => Some(MessageReadState::Unread),
            _ => None,
        }
    }

    /// Serialize to INI string value.
    #[must_use]
    pub fn to_ini_str(&self) -> &'static str {
        match self {
            MessageReadState::None => "None",
            MessageReadState::Read => "Read",
            MessageReadState::Unread => "Unread",
        }
    }
}

// ─── AppConfig ───────────────────────────────────────────────────────────────

/// Top-level application configuration loaded from INI files.
#[derive(Clone, Debug, Default)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub filter: FilterConfig,
    pub filter_now: FilterNowConfig,
    pub training: TrainingConfig,
    pub notification: NotificationConfig,
    pub experimental: ExperimentalConfig,
}

// ─── GeneralConfig ───────────────────────────────────────────────────────────

/// General application settings.
#[derive(Clone, Debug)]
pub struct GeneralConfig {
    /// The name of the custom field used to store the spam score.
    /// Default: `"Spam"`.
    pub field_score_name: String,
    /// The directory where `SpamBayes` data files are stored.
    /// Empty string means use the default location.
    pub data_directory: String,
    /// How the 'read' flag is modified when "Delete as Spam" is used.
    pub delete_as_spam_message_state: MessageReadState,
    /// How the 'read' flag is modified when "Recover from Spam" is used.
    pub recover_from_spam_message_state: MessageReadState,
    /// Verbosity level for debug output (0 = minimal, higher = more verbose).
    pub verbose: u32,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            field_score_name: "Spam".to_string(),
            data_directory: String::new(),
            delete_as_spam_message_state: MessageReadState::None,
            recover_from_spam_message_state: MessageReadState::None,
            verbose: 0,
        }
    }
}

// ─── FilterConfig ────────────────────────────────────────────────────────────

/// Configuration for the real-time message filter.
#[derive(Clone, Debug)]
pub struct FilterConfig {
    /// Whether filtering is enabled.
    pub enabled: bool,
    /// Score at or above which a message is classified as spam.
    /// Default: `90.0`.
    pub spam_threshold: f64,
    /// Score at or above which a message is classified as unsure.
    /// Default: `15.0`.
    pub unsure_threshold: f64,
    /// Folder to move/copy spam messages to.
    pub spam_folder_id: Option<FolderId>,
    /// Folder to move/copy unsure messages to.
    pub unsure_folder_id: Option<FolderId>,
    /// Folder to move/copy ham (good) messages to.
    pub ham_folder_id: Option<FolderId>,
    /// Action to take for spam messages.
    pub spam_action: FilterAction,
    /// Action to take for unsure messages.
    pub unsure_action: FilterAction,
    /// Action to take for ham (good) messages.
    pub ham_action: FilterAction,
    /// Whether to mark spam messages as read when filtered.
    pub spam_mark_as_read: bool,
    /// Whether to mark unsure messages as read when filtered.
    pub unsure_mark_as_read: bool,
    /// Whether to mark ham messages as read when filtered.
    pub ham_mark_as_read: bool,
    /// Whether to save spam score info in each filtered message.
    /// Default: `true`.
    pub save_spam_info: bool,
    /// Folders to watch for new incoming messages.
    pub watch_folder_ids: Vec<FolderId>,
    /// Whether to use a timer for background filtering.
    /// Default: `true`.
    pub timer_enabled: bool,
    /// Delay in seconds before the filter timer starts after a new message.
    /// Default: `2.0`.
    pub timer_start_delay: f64,
    /// Interval in seconds between timer checks for new messages.
    /// Default: `1.0`.
    pub timer_interval: f64,
    /// Whether the timer should only apply to receive (Inbox-style) folders.
    /// Default: `true`.
    pub timer_only_receive_folders: bool,
    /// Whether automatic spam cleanup is enabled.
    /// Default: `false`.
    pub spam_auto_cleanup_enabled: bool,
    /// Number of days to keep spam before automatic deletion.
    /// Default: `30`.
    pub spam_auto_cleanup_days: u32,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            spam_threshold: 90.0,
            unsure_threshold: 15.0,
            spam_folder_id: None,
            unsure_folder_id: None,
            ham_folder_id: None,
            spam_action: FilterAction::Move,
            unsure_action: FilterAction::Move,
            ham_action: FilterAction::Untouched,
            spam_mark_as_read: false,
            unsure_mark_as_read: false,
            ham_mark_as_read: false,
            save_spam_info: true,
            watch_folder_ids: Vec::new(),
            timer_enabled: true,
            timer_start_delay: 2.0,
            timer_interval: 1.0,
            timer_only_receive_folders: true,
            spam_auto_cleanup_enabled: false,
            spam_auto_cleanup_days: 30,
        }
    }
}

// ─── FilterNowConfig ─────────────────────────────────────────────────────────

/// Configuration for the "Filter Now" batch operation.
#[derive(Clone, Debug)]
pub struct FilterNowConfig {
    /// Folders to filter during a "Filter Now" operation.
    pub folder_ids: Vec<FolderId>,
    /// Whether to include sub-folders of the nominated folders.
    pub include_sub: bool,
    /// Whether to only filter unread messages.
    pub only_unread: bool,
    /// Whether to only filter messages that have never been scored.
    pub only_unseen: bool,
    /// Whether to perform all filter actions (move/copy) or just score.
    /// Default: `true`.
    pub action_all: bool,
}

impl Default for FilterNowConfig {
    fn default() -> Self {
        Self {
            folder_ids: Vec::new(),
            include_sub: false,
            only_unread: false,
            only_unseen: false,
            action_all: true,
        }
    }
}

// ─── TrainingConfig ──────────────────────────────────────────────────────────

/// Configuration for training operations.
#[derive(Clone, Debug)]
pub struct TrainingConfig {
    /// Folders containing known ham (good) messages for training.
    pub ham_folder_ids: Vec<FolderId>,
    /// Whether ham folder selection includes sub-folders.
    pub ham_include_sub: bool,
    /// Folders containing known spam messages for training.
    pub spam_folder_ids: Vec<FolderId>,
    /// Whether spam folder selection includes sub-folders.
    pub spam_include_sub: bool,
    /// Whether to automatically train messages recovered from spam.
    /// Default: `true`.
    pub train_recovered_spam: bool,
    /// Whether to automatically train messages manually moved to spam.
    /// Default: `true`.
    pub train_manual_spam: bool,
    /// Whether to rescore messages after training.
    /// Default: `true`.
    pub rescore: bool,
    /// Whether to rebuild the entire database during training.
    /// Default: `true`.
    pub rebuild: bool,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            ham_folder_ids: Vec::new(),
            ham_include_sub: false,
            spam_folder_ids: Vec::new(),
            spam_include_sub: false,
            train_recovered_spam: true,
            train_manual_spam: true,
            rescore: true,
            rebuild: true,
        }
    }
}

// ─── NotificationConfig ──────────────────────────────────────────────────────

/// Configuration for sound notifications.
#[derive(Clone, Debug)]
pub struct NotificationConfig {
    /// Whether sound notifications are enabled.
    /// Default: `false`.
    pub notify_sound_enabled: bool,
    /// Path to the WAV file for ham (good) message notifications.
    pub notify_ham_sound: String,
    /// Path to the WAV file for unsure message notifications.
    pub notify_unsure_sound: String,
    /// Path to the WAV file for spam message notifications.
    pub notify_spam_sound: String,
    /// Delay in seconds to accumulate messages before playing notification.
    /// Default: `10.0`.
    pub notify_accumulate_delay: f64,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            notify_sound_enabled: false,
            notify_ham_sound: String::new(),
            notify_unsure_sound: String::new(),
            notify_spam_sound: String::new(),
            notify_accumulate_delay: 10.0,
        }
    }
}

// ─── ExperimentalConfig ──────────────────────────────────────────────────────

/// Experimental options that may change or be removed in future versions.
///
/// These are preserved for migration compatibility with older Python configs.
#[derive(Clone, Debug)]
pub struct ExperimentalConfig {
    /// Obsolete timer start delay (migrated to Filter section).
    pub timer_start_delay: u32,
    /// Obsolete timer interval (migrated to Filter section).
    pub timer_interval: u32,
    /// Obsolete `timer_only_receive_folders` (migrated to Filter section).
    pub timer_only_receive_folders: bool,
}

impl Default for ExperimentalConfig {
    fn default() -> Self {
        Self {
            timer_start_delay: 0,
            timer_interval: 1000,
            timer_only_receive_folders: true,
        }
    }
}

// ─── Parsing helpers ─────────────────────────────────────────────────────────

/// Parse a boolean from Python INI format ("True"/"False").
/// Returns None for invalid values.
fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "True" | "true" | "1" | "yes" => Some(true),
        "False" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Format a boolean to Python INI format.
fn format_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Parse an f64, returning None for invalid values.
fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Parse a u32, returning None for invalid values.
fn parse_u32(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok()
}

/// Get a string value from a section, returning None if not present.
fn get_value<'a>(section: Option<&'a SectionData>, key: &str) -> Option<&'a str> {
    section.and_then(|s| s.get(key).map(std::string::String::as_str))
}

// ─── AppConfig impl ──────────────────────────────────────────────────────────

impl AppConfig {
    /// Load configuration from INI files in the given data directory.
    ///
    /// Reads `{data_dir}/{profile_name}.ini` as the main config file.
    /// If the file does not exist, returns defaults.
    /// Then applies `{data_dir}/bayes_customize.ini` as an overlay
    /// (overlay values override main values).
    ///
    /// Invalid or missing values use defaults with a warning printed to stderr.
    pub fn load(data_dir: &Path, profile_name: &str) -> Result<Self, ConfigError> {
        let config_path = data_dir.join(format!("{profile_name}.ini"));

        // Start with defaults
        let mut config = AppConfig::default();

        // Read main config file (if it exists)
        let main_data = match IniFile::read(&config_path) {
            Ok(data) => Some(data),
            Err(ConfigError::FileNotFound(_)) => None,
            Err(e) => return Err(e),
        };

        // Read bayes_customize.ini overlay (if it exists)
        let overlay_path = data_dir.join("bayes_customize.ini");
        let overlay_data = match IniFile::read(&overlay_path) {
            Ok(data) => Some(data),
            Err(ConfigError::FileNotFound(_)) => None,
            Err(e) => return Err(e),
        };

        // Helper: get value from overlay first, then main config
        // This implements the overlay precedence requirement
        let get = |section: &str, key: &str| -> Option<String> {
            if let Some(ref overlay) = overlay_data {
                if let Some(val) = get_value(overlay.get(section), key) {
                    return Some(val.to_string());
                }
            }
            if let Some(ref main) = main_data {
                if let Some(val) = get_value(main.get(section), key) {
                    return Some(val.to_string());
                }
            }
            None
        };

        // ── General section ──
        if let Some(v) = get("General", "field_score_name") {
            config.general.field_score_name = v;
        }
        if let Some(v) = get("General", "data_directory") {
            config.general.data_directory = v;
        }
        if let Some(v) = get("General", "delete_as_spam_message_state") {
            match MessageReadState::from_ini_str(&v) {
                Some(state) => config.general.delete_as_spam_message_state = state,
                None => eprintln!(
                    "Warning: invalid value for [General] delete_as_spam_message_state: {v:?}, using default"
                ),
            }
        }
        if let Some(v) = get("General", "recover_from_spam_message_state") {
            match MessageReadState::from_ini_str(&v) {
                Some(state) => config.general.recover_from_spam_message_state = state,
                None => eprintln!(
                    "Warning: invalid value for [General] recover_from_spam_message_state: {v:?}, using default"
                ),
            }
        }
        if let Some(v) = get("General", "verbose") {
            match parse_u32(&v) {
                Some(n) => config.general.verbose = n,
                None => eprintln!(
                    "Warning: invalid value for [General] verbose: {v:?}, using default"
                ),
            }
        }

        // ── Filter section ──
        Self::load_filter_section(&mut config.filter, &get);

        // ── Filter_Now section ──
        Self::load_filter_now_section(&mut config.filter_now, &get);

        // ── Training section ──
        Self::load_training_section(&mut config.training, &get);

        // ── Notification section ──
        Self::load_notification_section(&mut config.notification, &get);

        // ── Experimental section ──
        Self::load_experimental_section(&mut config.experimental, &get);

        Ok(config)
    }

    fn load_filter_section(
        filter: &mut FilterConfig,
        get: &dyn Fn(&str, &str) -> Option<String>,
    ) {
        if let Some(v) = get("Filter", "enabled") {
            match parse_bool(&v) {
                Some(b) => filter.enabled = b,
                None => eprintln!("Warning: invalid value for [Filter] enabled: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "spam_threshold") {
            match parse_f64(&v) {
                Some(f) => filter.spam_threshold = f,
                None => eprintln!("Warning: invalid value for [Filter] spam_threshold: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "unsure_threshold") {
            match parse_f64(&v) {
                Some(f) => filter.unsure_threshold = f,
                None => eprintln!("Warning: invalid value for [Filter] unsure_threshold: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "spam_folder_id") {
            if !v.is_empty() {
                filter.spam_folder_id = FolderId::from_ini_str(&v);
                if filter.spam_folder_id.is_none() && !v.trim().is_empty() && v.trim() != "None" {
                    eprintln!("Warning: invalid value for [Filter] spam_folder_id: {v:?}, using default");
                }
            }
        }
        if let Some(v) = get("Filter", "unsure_folder_id") {
            if !v.is_empty() {
                filter.unsure_folder_id = FolderId::from_ini_str(&v);
                if filter.unsure_folder_id.is_none() && !v.trim().is_empty() && v.trim() != "None" {
                    eprintln!("Warning: invalid value for [Filter] unsure_folder_id: {v:?}, using default");
                }
            }
        }
        if let Some(v) = get("Filter", "ham_folder_id") {
            if !v.is_empty() {
                filter.ham_folder_id = FolderId::from_ini_str(&v);
                if filter.ham_folder_id.is_none() && !v.trim().is_empty() && v.trim() != "None" {
                    eprintln!("Warning: invalid value for [Filter] ham_folder_id: {v:?}, using default");
                }
            }
        }
        if let Some(v) = get("Filter", "spam_action") {
            match FilterAction::from_ini_str(&v) {
                Some(a) => filter.spam_action = a,
                None => eprintln!("Warning: invalid value for [Filter] spam_action: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "unsure_action") {
            match FilterAction::from_ini_str(&v) {
                Some(a) => filter.unsure_action = a,
                None => eprintln!("Warning: invalid value for [Filter] unsure_action: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "ham_action") {
            match FilterAction::from_ini_str(&v) {
                Some(a) => filter.ham_action = a,
                None => eprintln!("Warning: invalid value for [Filter] ham_action: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "spam_mark_as_read") {
            match parse_bool(&v) {
                Some(b) => filter.spam_mark_as_read = b,
                None => eprintln!("Warning: invalid value for [Filter] spam_mark_as_read: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "unsure_mark_as_read") {
            match parse_bool(&v) {
                Some(b) => filter.unsure_mark_as_read = b,
                None => eprintln!("Warning: invalid value for [Filter] unsure_mark_as_read: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "ham_mark_as_read") {
            match parse_bool(&v) {
                Some(b) => filter.ham_mark_as_read = b,
                None => eprintln!("Warning: invalid value for [Filter] ham_mark_as_read: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "save_spam_info") {
            match parse_bool(&v) {
                Some(b) => filter.save_spam_info = b,
                None => eprintln!("Warning: invalid value for [Filter] save_spam_info: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "watch_folder_ids") {
            filter.watch_folder_ids = parse_folder_id_list(&v);
        }
        if let Some(v) = get("Filter", "timer_enabled") {
            match parse_bool(&v) {
                Some(b) => filter.timer_enabled = b,
                None => eprintln!("Warning: invalid value for [Filter] timer_enabled: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "timer_start_delay") {
            match parse_f64(&v) {
                Some(f) => filter.timer_start_delay = f,
                None => eprintln!("Warning: invalid value for [Filter] timer_start_delay: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "timer_interval") {
            match parse_f64(&v) {
                Some(f) => filter.timer_interval = f,
                None => eprintln!("Warning: invalid value for [Filter] timer_interval: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "timer_only_receive_folders") {
            match parse_bool(&v) {
                Some(b) => filter.timer_only_receive_folders = b,
                None => eprintln!("Warning: invalid value for [Filter] timer_only_receive_folders: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "spam_auto_cleanup_enabled") {
            match parse_bool(&v) {
                Some(b) => filter.spam_auto_cleanup_enabled = b,
                None => eprintln!("Warning: invalid value for [Filter] spam_auto_cleanup_enabled: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter", "spam_auto_cleanup_days") {
            match parse_u32(&v) {
                Some(n) => filter.spam_auto_cleanup_days = n,
                None => eprintln!("Warning: invalid value for [Filter] spam_auto_cleanup_days: {v:?}, using default"),
            }
        }
    }

    fn load_filter_now_section(
        filter_now: &mut FilterNowConfig,
        get: &dyn Fn(&str, &str) -> Option<String>,
    ) {
        if let Some(v) = get("Filter_Now", "folder_ids") {
            filter_now.folder_ids = parse_folder_id_list(&v);
        }
        if let Some(v) = get("Filter_Now", "include_sub") {
            match parse_bool(&v) {
                Some(b) => filter_now.include_sub = b,
                None => eprintln!("Warning: invalid value for [Filter_Now] include_sub: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter_Now", "only_unread") {
            match parse_bool(&v) {
                Some(b) => filter_now.only_unread = b,
                None => eprintln!("Warning: invalid value for [Filter_Now] only_unread: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter_Now", "only_unseen") {
            match parse_bool(&v) {
                Some(b) => filter_now.only_unseen = b,
                None => eprintln!("Warning: invalid value for [Filter_Now] only_unseen: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Filter_Now", "action_all") {
            match parse_bool(&v) {
                Some(b) => filter_now.action_all = b,
                None => eprintln!("Warning: invalid value for [Filter_Now] action_all: {v:?}, using default"),
            }
        }
    }

    fn load_training_section(
        training: &mut TrainingConfig,
        get: &dyn Fn(&str, &str) -> Option<String>,
    ) {
        if let Some(v) = get("Training", "ham_folder_ids") {
            training.ham_folder_ids = parse_folder_id_list(&v);
        }
        if let Some(v) = get("Training", "ham_include_sub") {
            match parse_bool(&v) {
                Some(b) => training.ham_include_sub = b,
                None => eprintln!("Warning: invalid value for [Training] ham_include_sub: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Training", "spam_folder_ids") {
            training.spam_folder_ids = parse_folder_id_list(&v);
        }
        if let Some(v) = get("Training", "spam_include_sub") {
            match parse_bool(&v) {
                Some(b) => training.spam_include_sub = b,
                None => eprintln!("Warning: invalid value for [Training] spam_include_sub: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Training", "train_recovered_spam") {
            match parse_bool(&v) {
                Some(b) => training.train_recovered_spam = b,
                None => eprintln!("Warning: invalid value for [Training] train_recovered_spam: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Training", "train_manual_spam") {
            match parse_bool(&v) {
                Some(b) => training.train_manual_spam = b,
                None => eprintln!("Warning: invalid value for [Training] train_manual_spam: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Training", "rescore") {
            match parse_bool(&v) {
                Some(b) => training.rescore = b,
                None => eprintln!("Warning: invalid value for [Training] rescore: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Training", "rebuild") {
            match parse_bool(&v) {
                Some(b) => training.rebuild = b,
                None => eprintln!("Warning: invalid value for [Training] rebuild: {v:?}, using default"),
            }
        }
    }

    fn load_notification_section(
        notification: &mut NotificationConfig,
        get: &dyn Fn(&str, &str) -> Option<String>,
    ) {
        if let Some(v) = get("Notification", "notify_sound_enabled") {
            match parse_bool(&v) {
                Some(b) => notification.notify_sound_enabled = b,
                None => eprintln!("Warning: invalid value for [Notification] notify_sound_enabled: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Notification", "notify_ham_sound") {
            notification.notify_ham_sound = v;
        }
        if let Some(v) = get("Notification", "notify_unsure_sound") {
            notification.notify_unsure_sound = v;
        }
        if let Some(v) = get("Notification", "notify_spam_sound") {
            notification.notify_spam_sound = v;
        }
        if let Some(v) = get("Notification", "notify_accumulate_delay") {
            match parse_f64(&v) {
                Some(f) => notification.notify_accumulate_delay = f,
                None => eprintln!("Warning: invalid value for [Notification] notify_accumulate_delay: {v:?}, using default"),
            }
        }
    }

    fn load_experimental_section(
        experimental: &mut ExperimentalConfig,
        get: &dyn Fn(&str, &str) -> Option<String>,
    ) {
        if let Some(v) = get("Experimental", "timer_start_delay") {
            match parse_u32(&v) {
                Some(n) => experimental.timer_start_delay = n,
                None => eprintln!("Warning: invalid value for [Experimental] timer_start_delay: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Experimental", "timer_interval") {
            match parse_u32(&v) {
                Some(n) => experimental.timer_interval = n,
                None => eprintln!("Warning: invalid value for [Experimental] timer_interval: {v:?}, using default"),
            }
        }
        if let Some(v) = get("Experimental", "timer_only_receive_folders") {
            match parse_bool(&v) {
                Some(b) => experimental.timer_only_receive_folders = b,
                None => eprintln!("Warning: invalid value for [Experimental] timer_only_receive_folders: {v:?}, using default"),
            }
        }
    }

    /// Save the current configuration to disk.
    ///
    /// Writes to `{data_dir}/{profile_name}.ini` using safe file writing
    /// (temp file + rename) via `IniFile::write()`.
    pub fn save(&self, data_dir: &Path, profile_name: &str) -> Result<(), ConfigError> {
        let config_path = data_dir.join(format!("{profile_name}.ini"));
        let data = self.to_ini_data();
        IniFile::write(&config_path, &data)
    }

    /// Serialize all config values to `IniData` for writing.
    fn to_ini_data(&self) -> IniData {
        let mut data = IniData::new();

        // ── General section ──
        let mut general = SectionData::new();
        general.insert("field_score_name".to_string(), self.general.field_score_name.clone());
        general.insert("data_directory".to_string(), self.general.data_directory.clone());
        general.insert(
            "delete_as_spam_message_state".to_string(),
            self.general.delete_as_spam_message_state.to_ini_str().to_string(),
        );
        general.insert(
            "recover_from_spam_message_state".to_string(),
            self.general.recover_from_spam_message_state.to_ini_str().to_string(),
        );
        general.insert("verbose".to_string(), self.general.verbose.to_string());
        data.insert("General".to_string(), general);

        // ── Filter section ──
        let mut filter = SectionData::new();
        filter.insert("enabled".to_string(), format_bool(self.filter.enabled).to_string());
        filter.insert("spam_threshold".to_string(), format!("{:.1}", self.filter.spam_threshold));
        filter.insert("unsure_threshold".to_string(), format!("{:.1}", self.filter.unsure_threshold));
        filter.insert(
            "spam_folder_id".to_string(),
            self.filter.spam_folder_id.as_ref().map_or_else(String::new, super::folder_id::FolderId::to_ini_str),
        );
        filter.insert(
            "unsure_folder_id".to_string(),
            self.filter.unsure_folder_id.as_ref().map_or_else(String::new, super::folder_id::FolderId::to_ini_str),
        );
        filter.insert(
            "ham_folder_id".to_string(),
            self.filter.ham_folder_id.as_ref().map_or_else(String::new, super::folder_id::FolderId::to_ini_str),
        );
        filter.insert("spam_action".to_string(), self.filter.spam_action.to_ini_str().to_string());
        filter.insert("unsure_action".to_string(), self.filter.unsure_action.to_ini_str().to_string());
        filter.insert("ham_action".to_string(), self.filter.ham_action.to_ini_str().to_string());
        filter.insert("spam_mark_as_read".to_string(), format_bool(self.filter.spam_mark_as_read).to_string());
        filter.insert("unsure_mark_as_read".to_string(), format_bool(self.filter.unsure_mark_as_read).to_string());
        filter.insert("ham_mark_as_read".to_string(), format_bool(self.filter.ham_mark_as_read).to_string());
        filter.insert("save_spam_info".to_string(), format_bool(self.filter.save_spam_info).to_string());
        filter.insert("watch_folder_ids".to_string(), format_folder_id_list(&self.filter.watch_folder_ids));
        filter.insert("timer_enabled".to_string(), format_bool(self.filter.timer_enabled).to_string());
        filter.insert("timer_start_delay".to_string(), format!("{:.1}", self.filter.timer_start_delay));
        filter.insert("timer_interval".to_string(), format!("{:.1}", self.filter.timer_interval));
        filter.insert("timer_only_receive_folders".to_string(), format_bool(self.filter.timer_only_receive_folders).to_string());
        filter.insert("spam_auto_cleanup_enabled".to_string(), format_bool(self.filter.spam_auto_cleanup_enabled).to_string());
        filter.insert("spam_auto_cleanup_days".to_string(), self.filter.spam_auto_cleanup_days.to_string());
        data.insert("Filter".to_string(), filter);

        // ── Filter_Now section ──
        let mut filter_now = SectionData::new();
        filter_now.insert("folder_ids".to_string(), format_folder_id_list(&self.filter_now.folder_ids));
        filter_now.insert("include_sub".to_string(), format_bool(self.filter_now.include_sub).to_string());
        filter_now.insert("only_unread".to_string(), format_bool(self.filter_now.only_unread).to_string());
        filter_now.insert("only_unseen".to_string(), format_bool(self.filter_now.only_unseen).to_string());
        filter_now.insert("action_all".to_string(), format_bool(self.filter_now.action_all).to_string());
        data.insert("Filter_Now".to_string(), filter_now);

        // ── Training section ──
        let mut training = SectionData::new();
        training.insert("ham_folder_ids".to_string(), format_folder_id_list(&self.training.ham_folder_ids));
        training.insert("ham_include_sub".to_string(), format_bool(self.training.ham_include_sub).to_string());
        training.insert("spam_folder_ids".to_string(), format_folder_id_list(&self.training.spam_folder_ids));
        training.insert("spam_include_sub".to_string(), format_bool(self.training.spam_include_sub).to_string());
        training.insert("train_recovered_spam".to_string(), format_bool(self.training.train_recovered_spam).to_string());
        training.insert("train_manual_spam".to_string(), format_bool(self.training.train_manual_spam).to_string());
        training.insert("rescore".to_string(), format_bool(self.training.rescore).to_string());
        training.insert("rebuild".to_string(), format_bool(self.training.rebuild).to_string());
        data.insert("Training".to_string(), training);

        // ── Notification section ──
        let mut notification = SectionData::new();
        notification.insert("notify_sound_enabled".to_string(), format_bool(self.notification.notify_sound_enabled).to_string());
        notification.insert("notify_ham_sound".to_string(), self.notification.notify_ham_sound.clone());
        notification.insert("notify_unsure_sound".to_string(), self.notification.notify_unsure_sound.clone());
        notification.insert("notify_spam_sound".to_string(), self.notification.notify_spam_sound.clone());
        notification.insert("notify_accumulate_delay".to_string(), format!("{:.1}", self.notification.notify_accumulate_delay));
        data.insert("Notification".to_string(), notification);

        // ── Experimental section ──
        let mut experimental = SectionData::new();
        experimental.insert("timer_start_delay".to_string(), self.experimental.timer_start_delay.to_string());
        experimental.insert("timer_interval".to_string(), self.experimental.timer_interval.to_string());
        experimental.insert("timer_only_receive_folders".to_string(), format_bool(self.experimental.timer_only_receive_folders).to_string());
        data.insert("Experimental".to_string(), experimental);

        data
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_app_config_defaults() {
        let config = AppConfig::default();
        assert_eq!(config.general.field_score_name, "Spam");
        assert!(!config.filter.enabled);
        assert_eq!(config.filter.spam_threshold, 90.0);
        assert_eq!(config.filter.unsure_threshold, 15.0);
        assert_eq!(config.filter.spam_action, FilterAction::Move);
        assert_eq!(config.filter.unsure_action, FilterAction::Move);
        assert_eq!(config.filter.ham_action, FilterAction::Untouched);
        assert!(config.filter.save_spam_info);
        assert!(config.filter.timer_enabled);
        assert_eq!(config.filter.timer_start_delay, 2.0);
        assert_eq!(config.filter.timer_interval, 1.0);
        assert!(config.filter.timer_only_receive_folders);
        assert_eq!(config.filter.spam_auto_cleanup_days, 30);
        assert!(!config.filter.spam_auto_cleanup_enabled);
    }

    #[test]
    fn test_filter_now_defaults() {
        let config = FilterNowConfig::default();
        assert!(config.folder_ids.is_empty());
        assert!(!config.include_sub);
        assert!(!config.only_unread);
        assert!(!config.only_unseen);
        assert!(config.action_all);
    }

    #[test]
    fn test_training_defaults() {
        let config = TrainingConfig::default();
        assert!(config.ham_folder_ids.is_empty());
        assert!(config.spam_folder_ids.is_empty());
        assert!(config.train_recovered_spam);
        assert!(config.train_manual_spam);
        assert!(config.rescore);
        assert!(config.rebuild);
    }

    #[test]
    fn test_notification_defaults() {
        let config = NotificationConfig::default();
        assert!(!config.notify_sound_enabled);
        assert!(config.notify_ham_sound.is_empty());
        assert_eq!(config.notify_accumulate_delay, 10.0);
    }

    #[test]
    fn test_filter_action_ini_roundtrip() {
        assert_eq!(FilterAction::from_ini_str("Moved"), Some(FilterAction::Move));
        assert_eq!(FilterAction::from_ini_str("Copied"), Some(FilterAction::Copy));
        assert_eq!(FilterAction::from_ini_str("Untouched"), Some(FilterAction::Untouched));
        assert_eq!(FilterAction::from_ini_str("Invalid"), None);

        assert_eq!(FilterAction::Move.to_ini_str(), "Moved");
        assert_eq!(FilterAction::Copy.to_ini_str(), "Copied");
        assert_eq!(FilterAction::Untouched.to_ini_str(), "Untouched");
    }

    #[test]
    fn test_message_read_state_ini_roundtrip() {
        assert_eq!(MessageReadState::from_ini_str("None"), Some(MessageReadState::None));
        assert_eq!(MessageReadState::from_ini_str("Read"), Some(MessageReadState::Read));
        assert_eq!(MessageReadState::from_ini_str("Unread"), Some(MessageReadState::Unread));
        assert_eq!(MessageReadState::from_ini_str("Invalid"), None);

        assert_eq!(MessageReadState::None.to_ini_str(), "None");
        assert_eq!(MessageReadState::Read.to_ini_str(), "Read");
        assert_eq!(MessageReadState::Unread.to_ini_str(), "Unread");
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig::load(dir.path(), "nonexistent_profile").unwrap();
        let default = AppConfig::default();
        assert_eq!(config.general.field_score_name, default.general.field_score_name);
        assert_eq!(config.filter.spam_threshold, default.filter.spam_threshold);
        assert_eq!(config.filter.enabled, default.filter.enabled);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        use crate::folder_id::{EntryId, StoreId};

        let dir = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.general.field_score_name = "MyScore".to_string();
        config.general.verbose = 2;
        config.filter.enabled = true;
        config.filter.spam_threshold = 85.0;
        config.filter.unsure_threshold = 20.0;
        config.filter.spam_action = FilterAction::Copy;
        config.filter.spam_mark_as_read = true;
        config.filter.watch_folder_ids = vec![
            FolderId::new(StoreId::new("AABB"), EntryId::new("CCDD")),
        ];
        config.filter.spam_folder_id = Some(FolderId::new(
            StoreId::new("1122"),
            EntryId::new("3344"),
        ));
        config.training.rebuild = false;
        config.notification.notify_sound_enabled = true;
        config.notification.notify_ham_sound = "C:\\sounds\\ham.wav".to_string();

        config.save(dir.path(), "test_profile").unwrap();
        let loaded = AppConfig::load(dir.path(), "test_profile").unwrap();

        assert_eq!(loaded.general.field_score_name, "MyScore");
        assert_eq!(loaded.general.verbose, 2);
        assert!(loaded.filter.enabled);
        assert_eq!(loaded.filter.spam_threshold, 85.0);
        assert_eq!(loaded.filter.unsure_threshold, 20.0);
        assert_eq!(loaded.filter.spam_action, FilterAction::Copy);
        assert!(loaded.filter.spam_mark_as_read);
        assert_eq!(loaded.filter.watch_folder_ids.len(), 1);
        assert_eq!(loaded.filter.watch_folder_ids[0].store_id.0, "AABB");
        assert!(loaded.filter.spam_folder_id.is_some());
        let spam_fid = loaded.filter.spam_folder_id.unwrap();
        assert_eq!(spam_fid.store_id.0, "1122");
        assert_eq!(spam_fid.entry_id.0, "3344");
        assert!(!loaded.training.rebuild);
        assert!(loaded.notification.notify_sound_enabled);
        assert_eq!(loaded.notification.notify_ham_sound, "C:\\sounds\\ham.wav");
    }

    #[test]
    fn test_load_with_invalid_values_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let ini_content = "\
[General]
verbose = not_a_number
field_score_name = CustomField

[Filter]
enabled = maybe
spam_threshold = abc
unsure_threshold = 25.0
spam_action = InvalidAction
";
        std::fs::write(dir.path().join("bad_profile.ini"), ini_content).unwrap();
        let config = AppConfig::load(dir.path(), "bad_profile").unwrap();

        // Valid values should be loaded
        assert_eq!(config.general.field_score_name, "CustomField");
        assert_eq!(config.filter.unsure_threshold, 25.0);

        // Invalid values should fall back to defaults
        assert_eq!(config.general.verbose, 0); // default
        assert!(!config.filter.enabled); // default (parse failed)
        assert_eq!(config.filter.spam_threshold, 90.0); // default
        assert_eq!(config.filter.spam_action, FilterAction::Move); // default
    }

    #[test]
    fn test_load_overlay_overrides_main() {
        let dir = tempfile::tempdir().unwrap();
        let main_content = "\
[Filter]
spam_threshold = 85.0
unsure_threshold = 20.0
enabled = True
";
        let overlay_content = "\
[Filter]
spam_threshold = 95.0
";
        std::fs::write(dir.path().join("overlay_test.ini"), main_content).unwrap();
        std::fs::write(dir.path().join("bayes_customize.ini"), overlay_content).unwrap();

        let config = AppConfig::load(dir.path(), "overlay_test").unwrap();

        // Overlay value takes precedence
        assert_eq!(config.filter.spam_threshold, 95.0);
        // Main values still loaded for non-overlay keys
        assert_eq!(config.filter.unsure_threshold, 20.0);
        assert!(config.filter.enabled);
    }
}
