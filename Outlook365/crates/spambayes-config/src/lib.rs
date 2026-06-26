#![warn(clippy::pedantic)]
// ── Pedantic allow-list (documented exceptions) ──────────────────────────────
// doc_markdown: Domain-specific identifiers (SpamBayes, FolderId, etc.) are
// product names and type references that don't benefit from backtick escaping.
#![allow(clippy::doc_markdown)]
// module_name_repetitions: Config type names use crate prefix for clarity.
#![allow(clippy::module_name_repetitions)]
// struct_excessive_bools: Configuration structs mirror Python INI sections with
// many boolean flags — this is inherent to the domain model.
#![allow(clippy::struct_excessive_bools)]
// too_many_lines: Config loading/saving functions are naturally long due to
// mapping many INI keys to typed fields.
#![allow(clippy::too_many_lines)]
// missing_errors_doc: Config load/save error conditions are self-evident.
#![allow(clippy::missing_errors_doc)]
// missing_panics_doc: Panics in config code are documented inline.
#![allow(clippy::missing_panics_doc)]

//! `SpamBayes` Config - Configuration system.
//!
//! Provides INI file parsing compatible with the Python configparser format,
//! typed option definitions with defaults, and `FolderId` types.
//!
//! # Overview
//!
//! This crate implements the configuration layer for the `SpamBayes` Outlook
//! add-in, providing:
//!
//! - **Typed folder IDs** (`FolderId`, `StoreId`, `EntryId`) that prevent
//!   accidental mixing of store and entry identifiers at compile time
//! - **Configuration structs** with defaults matching the Python `SpamBayes`
//!   implementation for seamless migration
//! - **INI format compatibility** for reading/writing Python configparser output
//!
//! # Examples
//!
//! ```
//! use spambayes_config::{AppConfig, FolderId, StoreId, EntryId, FilterAction};
//!
//! // Create a default config (all values match Python SpamBayes defaults)
//! let config = AppConfig::default();
//! assert_eq!(config.filter.spam_threshold, 90.0);
//! assert_eq!(config.filter.spam_action, FilterAction::Move);
//!
//! // Parse a folder ID from INI format
//! let folder = FolderId::from_ini_str("('0123ABCD', 'FEDC9876')").unwrap();
//! assert_eq!(folder.store_id, StoreId::new("0123ABCD"));
//! ```

pub mod config_chain;
pub mod errors;
pub mod folder_id;
pub mod ini_parser;
pub mod migration;
pub mod options;
pub mod profile;

// Re-export primary types for convenience.
pub use errors::ConfigError;
pub use folder_id::{EntryId, FolderId, StoreId};
pub use folder_id::{format_folder_id_list, parse_folder_id_list};
pub use ini_parser::{IniData, IniFile, SectionData, merge_ini_data};
pub use options::{
    AppConfig, CalendarConfig, CalendarSpamAction, ExperimentalConfig, FilterAction, FilterConfig,
    FilterNowConfig, GeneralConfig, MessageReadState, NotificationConfig, TrainingConfig,
};
pub use migration::{detect_python_config, migrate_python_config, rust_config_exists, try_migrate};
pub use profile::sanitize_profile_name;
pub use profile::resolve_data_directory;
pub use config_chain::ConfigChain;
