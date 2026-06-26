//! Native GTK4 GUI module for the SpamBayes Outlook add-in.
//!
//! This module provides a 1-to-1 replacement of the Python/tkinter GUI,
//! including:
//! - Manager window with 7 tabs (General, Filtering, Training, Statistics,
//!   Notifications, Calendar, Advanced)
//! - Configuration Wizard (4-page setup)
//! - Filter Now dialog
//! - Folder browser dialog
//! - Progress dialog
//! - Message box utility functions
//! - Show Clues dialog
//!
//! The GTK4 runtime runs on a dedicated thread and communicates with the
//! COM STA thread via crossbeam channels.
//!
//! **Validates: Requirement 14.1**

pub mod gtk_runtime;
pub mod manager_window;
pub mod wizard_window;
pub mod filter_now_dialog;
pub mod folder_browser;
pub mod mapi_folder_provider;
pub mod message_boxes;
pub mod progress_dialog;
pub mod clues_dialog;
pub mod tabs;
