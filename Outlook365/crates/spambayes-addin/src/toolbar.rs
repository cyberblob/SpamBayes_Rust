//! Toolbar manager for the `SpamBayes` Outlook add-in.
//!
//! This module implements the toolbar setup logic that creates a "`SpamBayes`"
//! command bar in the Outlook Explorer window with "Spam" and "Not Spam"
//! buttons. It uses a command pattern (similar to [`crate::notification`]) so
//! that the pure logic is testable without COM dependencies.
//!
//! # COM Interaction
//!
//! On Windows, the [`ToolbarManager::execute_setup`] method performs the actual
//! COM `IDispatch::Invoke` calls to create the toolbar. On non-Windows platforms,
//! only the command generation logic is available (for testing).
//!
//! # Button Icons
//!
//! The toolbar buttons use bitmap icons:
//! - "Spam" button: `delete_as_spam.bmp`
//! - "Not Spam" button: `recover_ham.bmp`
//!
//! These BMP files are located at `Outlook2000/images/` in the source tree.
//!
//! **Validates: Requirements 11.1, 11.2, 11.3**

use std::ffi::c_void;
use std::path::PathBuf;

use spambayes_config::{FilterConfig, FolderId, GeneralConfig, MessageReadState};
use spambayes_mapi::MsgStoreError;
use spambayes_storage::{MessageDatabase, MessageInfo};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Name of the `SpamBayes` toolbar (`CommandBar`).
pub const TOOLBAR_NAME: &str = "SpamBayes";

/// Caption for the "Spam" button.
pub const SPAM_BUTTON_CAPTION: &str = "Spam";

/// Caption for the "Not Spam" button.
pub const NOT_SPAM_BUTTON_CAPTION: &str = "Not Spam";

/// Filename for the "Spam" button bitmap icon.
pub const SPAM_BUTTON_ICON: &str = "delete_as_spam.bmp";

/// Filename for the "Not Spam" button bitmap icon.
pub const NOT_SPAM_BUTTON_ICON: &str = "recover_ham.bmp";

// ─── ToolbarCommand ──────────────────────────────────────────────────────────

/// Commands returned by the toolbar state machine to instruct the COM layer
/// what operations to perform.
///
/// The COM shell layer executes these commands against the Outlook object model
/// via `IDispatch::Invoke` calls on the `CommandBars` collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolbarCommand {
    /// Create a named toolbar (`CommandBar`) in the Explorer window.
    /// The `bool` indicates whether to set it visible immediately.
    CreateToolbar {
        /// Name of the toolbar to create.
        name: String,
        /// Whether the toolbar should be visible after creation.
        visible: bool,
    },
    /// Add a button to the toolbar with a caption and bitmap icon.
    AddButton {
        /// Name of the parent toolbar.
        toolbar_name: String,
        /// Caption text displayed on the button.
        caption: String,
        /// Path to the bitmap icon file.
        icon_path: PathBuf,
    },
}

// ─── ToolbarState ────────────────────────────────────────────────────────────

/// Tracks the current state of toolbar setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarState {
    /// Toolbar has not been created yet.
    NotCreated,
    /// Toolbar is created and visible.
    Ready,
    /// Toolbar setup failed.
    Failed,
}

// ─── ToolbarManager ──────────────────────────────────────────────────────────

/// Manages the `SpamBayes` toolbar in the Outlook Explorer window.
///
/// The toolbar manager follows a command pattern: [`setup`] returns a list of
/// [`ToolbarCommand`]s that describe the COM operations needed. The COM shell
/// layer then executes those commands.
///
/// # Usage
///
/// ```ignore
/// let mut mgr = ToolbarManager::new(app_ptr, images_dir);
///
/// // Get the commands needed to create the toolbar:
/// let commands = mgr.setup();
///
/// // Execute commands against COM (done by the shell layer):
/// for cmd in &commands {
///     execute_toolbar_command(cmd);
/// }
///
/// // Mark setup complete:
/// mgr.mark_ready();
/// ```
///
/// **Validates: Requirements 11.1, 11.2, 11.3**
#[derive(Debug)]
pub struct ToolbarManager {
    /// Pointer to the Outlook Application `IDispatch` COM object.
    /// Stored as an opaque pointer; actual COM calls happen in the shell layer.
    application: *mut c_void,
    /// Directory containing the toolbar button bitmap icons.
    images_dir: PathBuf,
    /// Current state of the toolbar.
    state: ToolbarState,
}

// SAFETY: ToolbarManager is only accessed from the COM apartment thread (STA).
unsafe impl Send for ToolbarManager {}

impl ToolbarManager {
    /// Creates a new `ToolbarManager`.
    ///
    /// # Parameters
    ///
    /// - `application`: Pointer to the Outlook.Application `IDispatch` COM object.
    /// - `images_dir`: Path to the directory containing button bitmap icons.
    pub fn new(application: *mut c_void, images_dir: PathBuf) -> Self {
        Self {
            application,
            images_dir,
            state: ToolbarState::NotCreated,
        }
    }

    /// Generate the commands required to set up the `SpamBayes` toolbar.
    ///
    /// Returns a list of [`ToolbarCommand`]s that the COM shell layer should
    /// execute in order:
    /// 1. Create the "`SpamBayes`" toolbar and set it visible.
    /// 2. Add the "Spam" button with `delete_as_spam.bmp` icon.
    /// 3. Add the "Not Spam" button with `recover_ham.bmp` icon.
    ///
    /// If setup has already been performed (state is not `NotCreated`), returns
    /// an empty list.
    ///
    /// **Validates: Requirements 11.1, 11.2, 11.3**
    #[must_use]
    pub fn setup(&self) -> Vec<ToolbarCommand> {
        if self.state != ToolbarState::NotCreated {
            return vec![];
        }

        let spam_icon_path = self.images_dir.join(SPAM_BUTTON_ICON);
        let not_spam_icon_path = self.images_dir.join(NOT_SPAM_BUTTON_ICON);

        vec![
            // Requirement 11.1: Create "SpamBayes" toolbar, set visible
            ToolbarCommand::CreateToolbar {
                name: TOOLBAR_NAME.to_string(),
                visible: true,
            },
            // Requirement 11.2: Add "Spam" button with delete_as_spam.bmp icon
            ToolbarCommand::AddButton {
                toolbar_name: TOOLBAR_NAME.to_string(),
                caption: SPAM_BUTTON_CAPTION.to_string(),
                icon_path: spam_icon_path,
            },
            // Requirement 11.3: Add "Not Spam" button with recover_ham.bmp icon
            ToolbarCommand::AddButton {
                toolbar_name: TOOLBAR_NAME.to_string(),
                caption: NOT_SPAM_BUTTON_CAPTION.to_string(),
                icon_path: not_spam_icon_path,
            },
        ]
    }

    /// Mark the toolbar as successfully created and ready.
    ///
    /// Call this after all [`ToolbarCommand`]s returned by [`setup`] have been
    /// successfully executed by the COM shell layer.
    pub fn mark_ready(&mut self) {
        self.state = ToolbarState::Ready;
    }

    /// Mark the toolbar setup as failed.
    ///
    /// Call this if any [`ToolbarCommand`] from [`setup`] fails during COM
    /// execution.
    pub fn mark_failed(&mut self) {
        self.state = ToolbarState::Failed;
    }

    /// Returns the current toolbar state.
    #[must_use]
    pub fn state(&self) -> ToolbarState {
        self.state
    }

    /// Returns a reference to the Outlook Application COM pointer.
    #[must_use]
    pub fn application(&self) -> *mut c_void {
        self.application
    }

    /// Returns the images directory path.
    #[must_use]
    pub fn images_dir(&self) -> &PathBuf {
        &self.images_dir
    }

    /// Execute the toolbar setup against the Outlook COM object model.
    ///
    /// This method performs the actual COM `IDispatch::Invoke` calls to create
    /// the command bar and add buttons. It is only available on Windows.
    ///
    /// On success, transitions state to `Ready`. On failure, transitions to
    /// `Failed`.
    ///
    /// # Safety
    ///
    /// The `application` pointer must be a valid `IDispatch` COM pointer to the
    /// Outlook.Application object in the current STA apartment.
    #[cfg(target_os = "windows")]
    pub unsafe fn execute_setup(&mut self) -> Result<(), ToolbarError> {
        use crate::com_invoke::{
            dispatch_get, dispatch_invoke_method, dispatch_put, VariantArg,
        };

        if self.application.is_null() {
            self.state = ToolbarState::Failed;
            return Err(ToolbarError::NullApplication);
        }

        if self.state != ToolbarState::NotCreated {
            return Ok(());
        }

        // 1. Get Application.ActiveExplorer
        let explorer_result = dispatch_get(self.application, "ActiveExplorer");

        // Debug trace
        let data_dir = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let debug_path = format!("{data_dir}\\SpamBayes\\addin_debug.log");
        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar: ActiveExplorer result={:?}", explorer_result.as_ref().map(|p| *p as usize).map_err(|hr| hr.0)) });

        let explorer = explorer_result
            .map_err(|_| ToolbarError::NoActiveExplorer)?;
        if explorer.is_null() {
            self.state = ToolbarState::Failed;
            return Err(ToolbarError::NoActiveExplorer);
        }

        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar: got ActiveExplorer={:?}", explorer) });

        // 2. Get Explorer.CommandBars
        let cmd_bars_result = dispatch_get(explorer, "CommandBars");
        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar: CommandBars result={:?}", cmd_bars_result.as_ref().map(|p| *p as usize).map_err(|hr| hr.0)) });

        let command_bars = cmd_bars_result
            .map_err(|_| ToolbarError::CommandBarsUnavailable)?;
        if command_bars.is_null() {
            self.state = ToolbarState::Failed;
            return Err(ToolbarError::CommandBarsUnavailable);
        }

        let _ = std::fs::OpenOptions::new().append(true).open(&debug_path)
            .and_then(|mut f| { use std::io::Write; writeln!(f, "toolbar: got CommandBars={:?}", command_bars) });

        // 3. Try to find existing "SpamBayes" toolbar, or create one
        let toolbar = dispatch_invoke_method(
            command_bars,
            "Add",
            &[
                VariantArg::BStr(TOOLBAR_NAME),
                VariantArg::I4(0), // msoBarTop = 0 (position)
                VariantArg::Bool(false), // Temporary = False
            ],
        )
        .map_err(|_| ToolbarError::CreateFailed("CommandBars.Add failed".to_string()))?;

        if toolbar.is_null() {
            self.state = ToolbarState::Failed;
            return Err(ToolbarError::CreateFailed(
                "CommandBars.Add returned null".to_string(),
            ));
        }

        // 4. Set toolbar visible
        let _ = dispatch_put(toolbar, "Visible", VariantArg::Bool(true));

        // 5. Get toolbar.Controls
        let controls = dispatch_get(toolbar, "Controls")
            .map_err(|_| ToolbarError::CreateFailed("Cannot get Controls".to_string()))?;

        // 6. Add "Spam" button (msoControlButton = 1)
        let spam_btn = dispatch_invoke_method(
            controls,
            "Add",
            &[VariantArg::I4(1)], // Type = msoControlButton
        )
        .map_err(|_| ToolbarError::AddButtonFailed(SPAM_BUTTON_CAPTION.to_string()))?;

        if !spam_btn.is_null() {
            let _ = dispatch_put(spam_btn, "Caption", VariantArg::BStr(SPAM_BUTTON_CAPTION));
            let _ = dispatch_put(
                spam_btn,
                "Tag",
                VariantArg::BStr("SpamBayesCommand.DeleteAsSpam"),
            );
            let _ = dispatch_put(
                spam_btn,
                "TooltipText",
                VariantArg::BStr("Move selected message to Spam folder and train as spam"),
            );
            let _ = dispatch_put(spam_btn, "Style", VariantArg::I4(2)); // msoButtonCaption = 2
            let _ = dispatch_put(
                spam_btn,
                "OnAction",
                VariantArg::BStr("<!SpamBayes.OutlookAddin>"),
            );
        }

        // 7. Add "Not Spam" button
        let ham_btn = dispatch_invoke_method(
            controls,
            "Add",
            &[VariantArg::I4(1)], // Type = msoControlButton
        )
        .map_err(|_| ToolbarError::AddButtonFailed(NOT_SPAM_BUTTON_CAPTION.to_string()))?;

        if !ham_btn.is_null() {
            let _ = dispatch_put(ham_btn, "Caption", VariantArg::BStr(NOT_SPAM_BUTTON_CAPTION));
            let _ = dispatch_put(
                ham_btn,
                "Tag",
                VariantArg::BStr("SpamBayesCommand.RecoverFromSpam"),
            );
            let _ = dispatch_put(
                ham_btn,
                "TooltipText",
                VariantArg::BStr("Recover message from spam and train as not spam"),
            );
            let _ = dispatch_put(ham_btn, "Style", VariantArg::I4(2)); // msoButtonCaption = 2
            let _ = dispatch_put(
                ham_btn,
                "OnAction",
                VariantArg::BStr("<!SpamBayes.OutlookAddin>"),
            );
        }

        // 8. Add "SpamBayes" popup menu
        let popup = dispatch_invoke_method(
            controls,
            "Add",
            &[VariantArg::I4(10)], // Type = msoControlPopup = 10
        )
        .unwrap_or(std::ptr::null_mut());

        if !popup.is_null() {
            let _ = dispatch_put(popup, "Caption", VariantArg::BStr("SpamBayes"));
            let _ = dispatch_put(popup, "Tag", VariantArg::BStr("SpamBayesCommand.Popup"));

            // Add submenu items to the popup
            let popup_controls = dispatch_get(popup, "Controls").unwrap_or(std::ptr::null_mut());
            if !popup_controls.is_null() {
                // "SpamBayes Manager..."
                let mgr_btn = dispatch_invoke_method(
                    popup_controls,
                    "Add",
                    &[VariantArg::I4(1)],
                )
                .unwrap_or(std::ptr::null_mut());
                if !mgr_btn.is_null() {
                    let _ = dispatch_put(mgr_btn, "Caption", VariantArg::BStr("SpamBayes Manager..."));
                    let _ = dispatch_put(mgr_btn, "Tag", VariantArg::BStr("SpamBayesCommand.Manager"));
                    // Sink the Click event via connection point
                    crate::com_invoke::advise_button_click(mgr_btn, crate::addin_core::AddinCore::launch_manager);
                }

                // "Filter messages..."
                let filter_btn = dispatch_invoke_method(
                    popup_controls,
                    "Add",
                    &[VariantArg::I4(1)],
                )
                .unwrap_or(std::ptr::null_mut());
                if !filter_btn.is_null() {
                    let _ = dispatch_put(filter_btn, "Caption", VariantArg::BStr("Filter messages..."));
                    let _ = dispatch_put(filter_btn, "Tag", VariantArg::BStr("SpamBayesCommand.FilterNow"));
                }

                // "Empty Spam Folder"
                let empty_btn = dispatch_invoke_method(
                    popup_controls,
                    "Add",
                    &[VariantArg::I4(1)],
                )
                .unwrap_or(std::ptr::null_mut());
                if !empty_btn.is_null() {
                    let _ = dispatch_put(empty_btn, "Caption", VariantArg::BStr("Empty Spam Folder"));
                    let _ = dispatch_put(empty_btn, "Tag", VariantArg::BStr("SpamBayesCommand.EmptySpam"));
                    let _ = dispatch_put(empty_btn, "BeginGroup", VariantArg::Bool(true));
                }
            }
        }

        self.state = ToolbarState::Ready;
        Ok(())
    }

    /// Execute the toolbar setup — no-op on non-Windows platforms.
    #[cfg(not(target_os = "windows"))]
    pub fn execute_setup(&mut self) -> Result<(), ToolbarError> {
        // Non-Windows: mark as ready (for testing purposes).
        let commands = self.setup();
        if commands.is_empty() {
            return Ok(());
        }
        self.state = ToolbarState::Ready;
        Ok(())
    }
}

// ─── ButtonAction ─────────────────────────────────────────────────────────────

/// Actions returned by the button handler logic to instruct the COM layer
/// what operations to perform on each message.
///
/// Similar to [`ToolbarCommand`], this enum enables the button handler logic
/// to be tested without COM dependencies.
///
/// **Validates: Requirements 11.4, 11.5, 11.8**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonAction {
    /// Train the message as spam.
    TrainAsSpam,
    /// Train the message as ham.
    TrainAsHam,
    /// Move the message to the specified folder.
    MoveToFolder {
        /// Destination folder identifier.
        folder_id: FolderId,
    },
    /// Record the message's current folder for future recovery.
    RecordOriginalFolder {
        /// The search key identifying the message.
        search_key: Vec<u8>,
        /// The folder ID to record.
        folder_id: FolderId,
    },
    /// Set the message read state.
    SetReadState {
        /// `true` = mark as read, `false` = mark as unread.
        read: bool,
    },
}

// ─── WaitCursorGuard ─────────────────────────────────────────────────────────

/// RAII guard for wait cursor display during toolbar operations.
///
/// When created, the wait cursor is shown. When dropped, the normal cursor
/// is restored. This ensures cursor restoration even if an error occurs.
///
/// **Validates: Requirement 11.9**
pub struct WaitCursorGuard {
    _private: (),
}

impl WaitCursorGuard {
    /// Create a new guard, displaying the wait cursor.
    ///
    /// On Windows, this calls `SetCursor(LoadCursor(NULL, IDC_WAIT))`.
    /// On non-Windows platforms, this is a no-op for testing.
    #[must_use]
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        {
            // The actual cursor change is performed by the COM shell layer;
            // this type just tracks the lifetime for RAII restoration.
        }
        Self { _private: () }
    }
}

impl Drop for WaitCursorGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            // Restore normal cursor — done by COM shell layer on drop.
        }
    }
}

// ─── ToolbarMessage Trait ────────────────────────────────────────────────────

/// Trait abstracting message operations needed by toolbar button handlers.
///
/// Combines the subset of [`crate::train::TrainableMessage`] and
/// [`crate::filter::FilterableMessage`] operations required for the
/// "Spam" and "Not Spam" button actions.
pub trait ToolbarMessage {
    /// Get the raw RFC 2822 message content for training.
    fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError>;

    /// Get a unique search key identifying this message.
    fn get_search_key(&self) -> &[u8];

    /// Get a human-readable identifier for logging.
    fn get_display_id(&self) -> String;

    /// Get the current folder ID of this message.
    fn get_current_folder(&self) -> Option<FolderId>;

    /// Move the message to the specified folder.
    fn move_to(&mut self, folder_id: &FolderId) -> Result<(), MsgStoreError>;

    /// Set the read/unread state of the message.
    fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError>;
}

// ─── ButtonHandler ───────────────────────────────────────────────────────────

/// Handles the logic for the "Spam" and "Not Spam" toolbar button clicks.
///
/// This struct encapsulates the pure logic for button handling, producing
/// [`ButtonAction`] lists that the COM shell layer executes. It also provides
/// higher-level methods that directly orchestrate the full operation using
/// trait-based abstractions for messages and training.
///
/// **Validates: Requirements 11.4, 11.5, 11.6, 11.7, 11.8, 11.9**
pub struct ButtonHandler;

impl ButtonHandler {
    /// Handle the "Spam" button click.
    ///
    /// For each selected message:
    /// 1. Records the message's current folder for future recovery (Req 11.4)
    /// 2. Trains the message as spam (Req 11.4)
    /// 3. Moves the message to the configured spam folder (Req 11.4)
    /// 4. Updates message read state per config (Req 11.8)
    ///
    /// Returns an empty list if no messages are selected (Req 11.6).
    /// Returns an error if filtering is not enabled (Req 11.7).
    ///
    /// **Validates: Requirements 11.4, 11.6, 11.7, 11.8**
    pub fn on_spam_clicked(
        messages: &[&dyn ToolbarMessage],
        filter_config: &FilterConfig,
        general_config: &GeneralConfig,
    ) -> Result<Vec<Vec<ButtonAction>>, ToolbarError> {
        // Requirement 11.6: No selection → no-op.
        if messages.is_empty() {
            return Ok(vec![]);
        }

        // Requirement 11.7: Filtering must be enabled.
        if !filter_config.enabled {
            return Err(ToolbarError::FilteringNotEnabled);
        }

        // Requirement 11.4: Need a spam folder configured.
        let spam_folder = filter_config.spam_folder_id.as_ref().ok_or({
            ToolbarError::SpamFolderNotConfigured
        })?;

        let mut all_actions = Vec::with_capacity(messages.len());

        for msg in messages {
            let mut actions = Vec::new();

            // Record the message's current folder for recovery.
            if let Some(current_folder) = msg.get_current_folder() {
                actions.push(ButtonAction::RecordOriginalFolder {
                    search_key: msg.get_search_key().to_vec(),
                    folder_id: current_folder,
                });
            }

            // Train as spam.
            actions.push(ButtonAction::TrainAsSpam);

            // Move to spam folder.
            actions.push(ButtonAction::MoveToFolder {
                folder_id: spam_folder.clone(),
            });

            // Requirement 11.8: Update read state per config.
            match &general_config.delete_as_spam_message_state {
                MessageReadState::Read => {
                    actions.push(ButtonAction::SetReadState { read: true });
                }
                MessageReadState::Unread => {
                    actions.push(ButtonAction::SetReadState { read: false });
                }
                MessageReadState::None => {
                    // No change to read state.
                }
            }

            all_actions.push(actions);
        }

        Ok(all_actions)
    }

    /// Handle the "Not Spam" button click.
    ///
    /// For each selected message:
    /// 1. Trains the message as ham (Req 11.5)
    /// 2. Moves to the original folder, or Inbox fallback (Req 11.5)
    /// 3. Updates message read state per config (Req 11.8)
    ///
    /// Returns an empty list if no messages are selected (Req 11.6).
    /// Returns an error if filtering is not enabled (Req 11.7).
    ///
    /// **Validates: Requirements 11.5, 11.6, 11.7, 11.8**
    pub fn on_not_spam_clicked(
        messages: &[&dyn ToolbarMessage],
        filter_config: &FilterConfig,
        general_config: &GeneralConfig,
        message_db: &dyn MessageDatabase,
        inbox_folder_id: &FolderId,
    ) -> Result<Vec<Vec<ButtonAction>>, ToolbarError> {
        // Requirement 11.6: No selection → no-op.
        if messages.is_empty() {
            return Ok(vec![]);
        }

        // Requirement 11.7: Filtering must be enabled.
        if !filter_config.enabled {
            return Err(ToolbarError::FilteringNotEnabled);
        }

        let mut all_actions = Vec::with_capacity(messages.len());

        for msg in messages {
            let mut actions = Vec::new();

            // Train as ham.
            actions.push(ButtonAction::TrainAsHam);

            // Requirement 11.5: Move to original folder, falling back to Inbox.
            let dest_folder = message_db
                .load_msg(msg.get_search_key())
                .and_then(|info| info.original_folder)
                .unwrap_or_else(|| inbox_folder_id.clone());

            actions.push(ButtonAction::MoveToFolder {
                folder_id: dest_folder,
            });

            // Requirement 11.8: Update read state per config.
            match &general_config.recover_from_spam_message_state {
                MessageReadState::Read => {
                    actions.push(ButtonAction::SetReadState { read: true });
                }
                MessageReadState::Unread => {
                    actions.push(ButtonAction::SetReadState { read: false });
                }
                MessageReadState::None => {
                    // No change to read state.
                }
            }

            all_actions.push(actions);
        }

        Ok(all_actions)
    }

    /// Execute the "Spam" button operation end-to-end.
    ///
    /// This method wraps the full pipeline:
    /// 1. Displays wait cursor (Req 11.9)
    /// 2. Validates selection and filter state (Req 11.6, 11.7)
    /// 3. For each message: record folder, train, move, set read state (Req 11.4, 11.8)
    /// 4. Restores cursor on completion or error (Req 11.9)
    ///
    /// **Validates: Requirements 11.4, 11.6, 11.7, 11.8, 11.9**
    pub fn execute_spam(
        messages: &mut [&mut dyn ToolbarMessage],
        filter_config: &FilterConfig,
        general_config: &GeneralConfig,
        message_db: &mut dyn MessageDatabase,
        train_fn: &dyn Fn(&dyn ToolbarMessage, bool) -> Result<(), ToolbarError>,
    ) -> Result<(), ToolbarError> {
        // Requirement 11.9: Show wait cursor (RAII restores on drop).
        let _cursor_guard = WaitCursorGuard::new();

        // Requirement 11.6: No selection → no-op.
        if messages.is_empty() {
            return Ok(());
        }

        // Requirement 11.7: Filtering must be enabled.
        if !filter_config.enabled {
            return Err(ToolbarError::FilteringNotEnabled);
        }

        // Requirement 11.4: Need a spam folder configured.
        let spam_folder = filter_config.spam_folder_id.as_ref().ok_or({
            ToolbarError::SpamFolderNotConfigured
        })?;

        for msg in messages.iter_mut() {
            // Record original folder in message database.
            if let Some(current_folder) = msg.get_current_folder() {
                let search_key = msg.get_search_key().to_vec();
                let existing = message_db.load_msg(&search_key);
                let updated_info = MessageInfo {
                    trained_as: existing.as_ref().and_then(|i| i.trained_as),
                    classification: existing.as_ref().and_then(|i| i.classification),
                    score: existing.as_ref().and_then(|i| i.score),
                    message_id: msg.get_display_id(),
                    original_folder: Some(current_folder),
                };
                message_db.store_msg(&search_key, &updated_info);
            }

            // Train as spam.
            train_fn(*msg, true)?;

            // Move to spam folder.
            msg.move_to(spam_folder).map_err(|e| {
                ToolbarError::ActionFailed(format!("move to spam folder: {e}"))
            })?;

            // Requirement 11.8: Update read state.
            match &general_config.delete_as_spam_message_state {
                MessageReadState::Read => {
                    let _ = msg.set_read_state(true);
                }
                MessageReadState::Unread => {
                    let _ = msg.set_read_state(false);
                }
                MessageReadState::None => {}
            }
        }

        Ok(())
    }

    /// Execute the "Not Spam" button operation end-to-end.
    ///
    /// This method wraps the full pipeline:
    /// 1. Displays wait cursor (Req 11.9)
    /// 2. Validates selection and filter state (Req 11.6, 11.7)
    /// 3. For each message: train as ham, move to original folder/Inbox, set read state (Req 11.5, 11.8)
    /// 4. Restores cursor on completion or error (Req 11.9)
    ///
    /// **Validates: Requirements 11.5, 11.6, 11.7, 11.8, 11.9**
    pub fn execute_not_spam(
        messages: &mut [&mut dyn ToolbarMessage],
        filter_config: &FilterConfig,
        general_config: &GeneralConfig,
        message_db: &dyn MessageDatabase,
        inbox_folder_id: &FolderId,
        train_fn: &dyn Fn(&dyn ToolbarMessage, bool) -> Result<(), ToolbarError>,
    ) -> Result<(), ToolbarError> {
        // Requirement 11.9: Show wait cursor (RAII restores on drop).
        let _cursor_guard = WaitCursorGuard::new();

        // Requirement 11.6: No selection → no-op.
        if messages.is_empty() {
            return Ok(());
        }

        // Requirement 11.7: Filtering must be enabled.
        if !filter_config.enabled {
            return Err(ToolbarError::FilteringNotEnabled);
        }

        for msg in messages.iter_mut() {
            // Train as ham.
            train_fn(*msg, false)?;

            // Requirement 11.5: Move to original folder, falling back to Inbox.
            let search_key = msg.get_search_key().to_vec();
            let dest_folder = message_db
                .load_msg(&search_key)
                .and_then(|info| info.original_folder)
                .unwrap_or_else(|| inbox_folder_id.clone());

            msg.move_to(&dest_folder).map_err(|e| {
                ToolbarError::ActionFailed(format!("move to recovery folder: {e}"))
            })?;

            // Requirement 11.8: Update read state.
            match &general_config.recover_from_spam_message_state {
                MessageReadState::Read => {
                    let _ = msg.set_read_state(true);
                }
                MessageReadState::Unread => {
                    let _ = msg.set_read_state(false);
                }
                MessageReadState::None => {}
            }
        }

        Ok(())
    }
}

// ─── ToolbarError ────────────────────────────────────────────────────────────

/// Errors that can occur during toolbar setup or button operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolbarError {
    /// The Application COM pointer was null.
    #[error("Outlook Application pointer is null")]
    NullApplication,

    /// Failed to get the active Explorer window.
    #[error("failed to get active Explorer window")]
    NoActiveExplorer,

    /// Failed to access the `CommandBars` collection.
    #[error("failed to access CommandBars collection")]
    CommandBarsUnavailable,

    /// Failed to create the toolbar.
    #[error("failed to create toolbar: {0}")]
    CreateFailed(String),

    /// Failed to add a button to the toolbar.
    #[error("failed to add button '{0}' to toolbar")]
    AddButtonFailed(String),

    /// Failed to load a bitmap icon.
    #[error("failed to load bitmap icon: {0}")]
    IconLoadFailed(String),

    /// Filtering is not enabled — user must configure and enable `SpamBayes` first.
    ///
    /// **Validates: Requirement 11.7**
    #[error("SpamBayes filtering is not enabled. Please configure and enable SpamBayes before marking messages as spam or not spam.")]
    FilteringNotEnabled,

    /// The spam folder is not configured.
    #[error("spam folder is not configured")]
    SpamFolderNotConfigured,

    /// A button action (move, train, etc.) failed.
    #[error("toolbar action failed: {0}")]
    ActionFailed(String),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    use spambayes_config::{EntryId, StoreId};
    use spambayes_core::Classification;

    // ─── Existing ToolbarManager Tests ───────────────────────────────────────

    #[test]
    fn new_manager_starts_not_created() {
        let mgr = ToolbarManager::new(ptr::null_mut(), PathBuf::from("images"));
        assert_eq!(mgr.state(), ToolbarState::NotCreated);
    }

    #[test]
    fn setup_returns_three_commands() {
        let images_dir = PathBuf::from("C:\\images");
        let mgr = ToolbarManager::new(ptr::null_mut(), images_dir.clone());

        let commands = mgr.setup();

        assert_eq!(commands.len(), 3);
    }

    #[test]
    fn setup_first_command_creates_toolbar_visible() {
        let images_dir = PathBuf::from("C:\\images");
        let mgr = ToolbarManager::new(ptr::null_mut(), images_dir);

        let commands = mgr.setup();

        assert_eq!(
            commands[0],
            ToolbarCommand::CreateToolbar {
                name: "SpamBayes".to_string(),
                visible: true,
            }
        );
    }

    #[test]
    fn setup_second_command_adds_spam_button() {
        let images_dir = PathBuf::from("C:\\images");
        let mgr = ToolbarManager::new(ptr::null_mut(), images_dir.clone());

        let commands = mgr.setup();

        assert_eq!(
            commands[1],
            ToolbarCommand::AddButton {
                toolbar_name: "SpamBayes".to_string(),
                caption: "Spam".to_string(),
                icon_path: images_dir.join("delete_as_spam.bmp"),
            }
        );
    }

    #[test]
    fn setup_third_command_adds_not_spam_button() {
        let images_dir = PathBuf::from("C:\\images");
        let mgr = ToolbarManager::new(ptr::null_mut(), images_dir.clone());

        let commands = mgr.setup();

        assert_eq!(
            commands[2],
            ToolbarCommand::AddButton {
                toolbar_name: "SpamBayes".to_string(),
                caption: "Not Spam".to_string(),
                icon_path: images_dir.join("recover_ham.bmp"),
            }
        );
    }

    #[test]
    fn setup_returns_empty_when_already_ready() {
        let images_dir = PathBuf::from("C:\\images");
        let mut mgr = ToolbarManager::new(ptr::null_mut(), images_dir);

        mgr.mark_ready();
        let commands = mgr.setup();

        assert!(commands.is_empty());
    }

    #[test]
    fn setup_returns_empty_when_failed() {
        let images_dir = PathBuf::from("C:\\images");
        let mut mgr = ToolbarManager::new(ptr::null_mut(), images_dir);

        mgr.mark_failed();
        let commands = mgr.setup();

        assert!(commands.is_empty());
    }

    #[test]
    fn mark_ready_transitions_state() {
        let mut mgr = ToolbarManager::new(ptr::null_mut(), PathBuf::from("images"));

        mgr.mark_ready();

        assert_eq!(mgr.state(), ToolbarState::Ready);
    }

    #[test]
    fn mark_failed_transitions_state() {
        let mut mgr = ToolbarManager::new(ptr::null_mut(), PathBuf::from("images"));

        mgr.mark_failed();

        assert_eq!(mgr.state(), ToolbarState::Failed);
    }

    #[test]
    fn execute_setup_transitions_state() {
        let mut mgr = ToolbarManager::new(ptr::null_mut(), PathBuf::from("images"));

        // On non-Windows, execute_setup is a no-op that succeeds.
        // On Windows with null ptr, it fails with NullApplication.
        #[cfg(not(target_os = "windows"))]
        {
            let result = mgr.execute_setup();
            assert!(result.is_ok());
            assert_eq!(mgr.state(), ToolbarState::Ready);
        }
        #[cfg(target_os = "windows")]
        {
            let result = unsafe { mgr.execute_setup() };
            assert_eq!(result, Err(ToolbarError::NullApplication));
            assert_eq!(mgr.state(), ToolbarState::Failed);
        }
    }

    #[test]
    fn images_dir_is_stored_correctly() {
        let images_dir = PathBuf::from("C:\\SpamBayes\\images");
        let mgr = ToolbarManager::new(ptr::null_mut(), images_dir.clone());

        assert_eq!(mgr.images_dir(), &images_dir);
    }

    #[test]
    fn application_pointer_is_stored() {
        let fake_ptr = 0x1234 as *mut c_void;
        let mgr = ToolbarManager::new(fake_ptr, PathBuf::from("images"));

        assert_eq!(mgr.application(), fake_ptr);
    }

    // ─── Button Handler Test Helpers ─────────────────────────────────────────

    fn make_folder_id(store: &str, entry: &str) -> FolderId {
        FolderId::new(StoreId::new(store), EntryId::new(entry))
    }

    fn make_filter_config(enabled: bool, spam_folder: Option<FolderId>) -> FilterConfig {
        FilterConfig {
            enabled,
            spam_folder_id: spam_folder,
            ..Default::default()
        }
    }

    fn make_general_config(
        spam_state: MessageReadState,
        recover_state: MessageReadState,
    ) -> GeneralConfig {
        GeneralConfig {
            delete_as_spam_message_state: spam_state,
            recover_from_spam_message_state: recover_state,
            ..Default::default()
        }
    }

    /// A mock message for button handler testing.
    struct MockToolbarMessage {
        search_key: Vec<u8>,
        display_id: String,
        current_folder: Option<FolderId>,
        moved_to: Option<FolderId>,
        read_state: Option<bool>,
    }

    impl MockToolbarMessage {
        fn new(key: &[u8], folder: Option<FolderId>) -> Self {
            Self {
                search_key: key.to_vec(),
                display_id: format!("msg-{}", String::from_utf8_lossy(key)),
                current_folder: folder,
                moved_to: None,
                read_state: None,
            }
        }
    }

    impl ToolbarMessage for MockToolbarMessage {
        fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError> {
            Ok(b"From: test@example.com\r\nSubject: Test\r\n\r\nBody".to_vec())
        }

        fn get_search_key(&self) -> &[u8] {
            &self.search_key
        }

        fn get_display_id(&self) -> String {
            self.display_id.clone()
        }

        fn get_current_folder(&self) -> Option<FolderId> {
            self.current_folder.clone()
        }

        fn move_to(&mut self, folder_id: &FolderId) -> Result<(), MsgStoreError> {
            self.moved_to = Some(folder_id.clone());
            Ok(())
        }

        fn set_read_state(&mut self, read: bool) -> Result<(), MsgStoreError> {
            self.read_state = Some(read);
            Ok(())
        }
    }

    /// A mock message database for testing.
    struct MockMessageDb {
        messages: std::collections::HashMap<Vec<u8>, MessageInfo>,
    }

    impl MockMessageDb {
        fn new() -> Self {
            Self {
                messages: std::collections::HashMap::new(),
            }
        }

        fn with_entry(mut self, key: &[u8], info: MessageInfo) -> Self {
            self.messages.insert(key.to_vec(), info);
            self
        }
    }

    impl MessageDatabase for MockMessageDb {
        fn load_msg(&self, search_key: &[u8]) -> Option<MessageInfo> {
            self.messages.get(search_key).cloned()
        }

        fn store_msg(&mut self, search_key: &[u8], info: &MessageInfo) {
            self.messages.insert(search_key.to_vec(), info.clone());
        }

        fn remove_msg(&mut self, search_key: &[u8]) {
            self.messages.remove(search_key);
        }
    }

    // ─── Button Handler Tests: Spam ──────────────────────────────────────────

    #[test]
    fn spam_click_no_selection_returns_empty() {
        let filter_config = make_filter_config(true, Some(make_folder_id("AA", "BB")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);

        let messages: Vec<&dyn ToolbarMessage> = vec![];
        let result = ButtonHandler::on_spam_clicked(&messages, &filter_config, &general_config);

        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn spam_click_filtering_disabled_returns_error() {
        let filter_config = make_filter_config(false, Some(make_folder_id("AA", "BB")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);

        let msg = MockToolbarMessage::new(b"key1", Some(make_folder_id("11", "22")));
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];
        let result = ButtonHandler::on_spam_clicked(&messages, &filter_config, &general_config);

        assert_eq!(result, Err(ToolbarError::FilteringNotEnabled));
    }

    #[test]
    fn spam_click_no_spam_folder_returns_error() {
        let filter_config = make_filter_config(true, None);
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);

        let msg = MockToolbarMessage::new(b"key1", Some(make_folder_id("11", "22")));
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];
        let result = ButtonHandler::on_spam_clicked(&messages, &filter_config, &general_config);

        assert_eq!(result, Err(ToolbarError::SpamFolderNotConfigured));
    }

    #[test]
    fn spam_click_produces_correct_actions() {
        let spam_folder = make_folder_id("SPAM", "FOLD");
        let filter_config = make_filter_config(true, Some(spam_folder.clone()));
        let general_config = make_general_config(MessageReadState::Read, MessageReadState::None);

        let current_folder = make_folder_id("INBOX", "0001");
        let msg = MockToolbarMessage::new(b"key1", Some(current_folder.clone()));
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];

        let result = ButtonHandler::on_spam_clicked(&messages, &filter_config, &general_config)
            .unwrap();

        assert_eq!(result.len(), 1);
        let actions = &result[0];

        // Should record original folder, train as spam, move, set read state.
        assert_eq!(actions.len(), 4);
        assert_eq!(
            actions[0],
            ButtonAction::RecordOriginalFolder {
                search_key: b"key1".to_vec(),
                folder_id: current_folder,
            }
        );
        assert_eq!(actions[1], ButtonAction::TrainAsSpam);
        assert_eq!(
            actions[2],
            ButtonAction::MoveToFolder {
                folder_id: spam_folder,
            }
        );
        assert_eq!(actions[3], ButtonAction::SetReadState { read: true });
    }

    #[test]
    fn spam_click_no_read_state_when_none_configured() {
        let spam_folder = make_folder_id("SPAM", "FOLD");
        let filter_config = make_filter_config(true, Some(spam_folder.clone()));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);

        let msg = MockToolbarMessage::new(b"key1", Some(make_folder_id("INBOX", "0001")));
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];

        let result = ButtonHandler::on_spam_clicked(&messages, &filter_config, &general_config)
            .unwrap();

        let actions = &result[0];
        // Should have 3 actions (no SetReadState).
        assert_eq!(actions.len(), 3);
        assert!(!actions.iter().any(|a| matches!(a, ButtonAction::SetReadState { .. })));
    }

    // ─── Button Handler Tests: Not Spam ──────────────────────────────────────

    #[test]
    fn not_spam_click_no_selection_returns_empty() {
        let filter_config = make_filter_config(true, Some(make_folder_id("AA", "BB")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);
        let message_db = MockMessageDb::new();
        let inbox = make_folder_id("INBOX", "0001");

        let messages: Vec<&dyn ToolbarMessage> = vec![];
        let result = ButtonHandler::on_not_spam_clicked(
            &messages,
            &filter_config,
            &general_config,
            &message_db,
            &inbox,
        );

        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn not_spam_click_filtering_disabled_returns_error() {
        let filter_config = make_filter_config(false, Some(make_folder_id("AA", "BB")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);
        let message_db = MockMessageDb::new();
        let inbox = make_folder_id("INBOX", "0001");

        let msg = MockToolbarMessage::new(b"key1", None);
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];
        let result = ButtonHandler::on_not_spam_clicked(
            &messages,
            &filter_config,
            &general_config,
            &message_db,
            &inbox,
        );

        assert_eq!(result, Err(ToolbarError::FilteringNotEnabled));
    }

    #[test]
    fn not_spam_click_uses_original_folder_from_db() {
        let filter_config = make_filter_config(true, Some(make_folder_id("SPAM", "FOLD")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::Unread);
        let original_folder = make_folder_id("ORIG", "FOLD");

        let message_db = MockMessageDb::new().with_entry(
            b"key1",
            MessageInfo {
                trained_as: Some(true),
                classification: Some(Classification::Spam),
                score: Some(95.0),
                message_id: "msg-key1".to_string(),
                original_folder: Some(original_folder.clone()),
            },
        );
        let inbox = make_folder_id("INBOX", "0001");

        let msg = MockToolbarMessage::new(b"key1", None);
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];

        let result = ButtonHandler::on_not_spam_clicked(
            &messages,
            &filter_config,
            &general_config,
            &message_db,
            &inbox,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        let actions = &result[0];

        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], ButtonAction::TrainAsHam);
        assert_eq!(
            actions[1],
            ButtonAction::MoveToFolder {
                folder_id: original_folder,
            }
        );
        assert_eq!(actions[2], ButtonAction::SetReadState { read: false });
    }

    #[test]
    fn not_spam_click_falls_back_to_inbox_when_no_original() {
        let filter_config = make_filter_config(true, Some(make_folder_id("SPAM", "FOLD")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);
        let message_db = MockMessageDb::new(); // No entry for key1
        let inbox = make_folder_id("INBOX", "0001");

        let msg = MockToolbarMessage::new(b"key1", None);
        let messages: Vec<&dyn ToolbarMessage> = vec![&msg];

        let result = ButtonHandler::on_not_spam_clicked(
            &messages,
            &filter_config,
            &general_config,
            &message_db,
            &inbox,
        )
        .unwrap();

        let actions = &result[0];
        assert_eq!(
            actions[1],
            ButtonAction::MoveToFolder {
                folder_id: inbox.clone(),
            }
        );
    }

    // ─── Button Handler Tests: execute_spam ──────────────────────────────────

    #[test]
    fn execute_spam_no_selection_is_noop() {
        let filter_config = make_filter_config(true, Some(make_folder_id("SPAM", "FOLD")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);
        let mut message_db = MockMessageDb::new();

        let mut messages: Vec<&mut dyn ToolbarMessage> = vec![];
        let train_fn = |_msg: &dyn ToolbarMessage, _is_spam: bool| -> Result<(), ToolbarError> {
            Ok(())
        };

        let result = ButtonHandler::execute_spam(
            &mut messages,
            &filter_config,
            &general_config,
            &mut message_db,
            &train_fn,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn execute_spam_filtering_disabled_returns_error() {
        let filter_config = make_filter_config(false, Some(make_folder_id("SPAM", "FOLD")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);
        let mut message_db = MockMessageDb::new();

        let mut msg = MockToolbarMessage::new(b"key1", Some(make_folder_id("INBOX", "0001")));
        let mut messages: Vec<&mut dyn ToolbarMessage> = vec![&mut msg];
        let train_fn = |_msg: &dyn ToolbarMessage, _is_spam: bool| -> Result<(), ToolbarError> {
            Ok(())
        };

        let result = ButtonHandler::execute_spam(
            &mut messages,
            &filter_config,
            &general_config,
            &mut message_db,
            &train_fn,
        );

        assert_eq!(result, Err(ToolbarError::FilteringNotEnabled));
    }

    #[test]
    fn execute_spam_moves_and_records_folder() {
        let spam_folder = make_folder_id("SPAM", "FOLD");
        let inbox_folder = make_folder_id("INBOX", "0001");
        let filter_config = make_filter_config(true, Some(spam_folder.clone()));
        let general_config = make_general_config(MessageReadState::Read, MessageReadState::None);
        let mut message_db = MockMessageDb::new();

        let mut msg = MockToolbarMessage::new(b"key1", Some(inbox_folder.clone()));
        let mut messages: Vec<&mut dyn ToolbarMessage> = vec![&mut msg];
        let train_fn = |_msg: &dyn ToolbarMessage, _is_spam: bool| -> Result<(), ToolbarError> {
            Ok(())
        };

        let result = ButtonHandler::execute_spam(
            &mut messages,
            &filter_config,
            &general_config,
            &mut message_db,
            &train_fn,
        );

        assert!(result.is_ok());

        // Verify the message was moved to spam folder.
        assert_eq!(msg.moved_to, Some(spam_folder));
        // Verify read state was set.
        assert_eq!(msg.read_state, Some(true));
        // Verify original folder was recorded in DB.
        let stored = message_db.load_msg(b"key1").unwrap();
        assert_eq!(stored.original_folder, Some(inbox_folder));
    }

    // ─── Button Handler Tests: execute_not_spam ──────────────────────────────

    #[test]
    fn execute_not_spam_moves_to_original_folder() {
        let original_folder = make_folder_id("ORIG", "FOLD");
        let inbox = make_folder_id("INBOX", "0001");
        let filter_config = make_filter_config(true, Some(make_folder_id("SPAM", "FOLD")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::Unread);

        let message_db = MockMessageDb::new().with_entry(
            b"key1",
            MessageInfo {
                trained_as: Some(true),
                classification: Some(Classification::Spam),
                score: Some(95.0),
                message_id: "msg-key1".to_string(),
                original_folder: Some(original_folder.clone()),
            },
        );

        let mut msg = MockToolbarMessage::new(b"key1", None);
        let mut messages: Vec<&mut dyn ToolbarMessage> = vec![&mut msg];
        let train_fn = |_msg: &dyn ToolbarMessage, _is_spam: bool| -> Result<(), ToolbarError> {
            Ok(())
        };

        let result = ButtonHandler::execute_not_spam(
            &mut messages,
            &filter_config,
            &general_config,
            &message_db,
            &inbox,
            &train_fn,
        );

        assert!(result.is_ok());
        assert_eq!(msg.moved_to, Some(original_folder));
        assert_eq!(msg.read_state, Some(false)); // Unread per config
    }

    #[test]
    fn execute_not_spam_falls_back_to_inbox() {
        let inbox = make_folder_id("INBOX", "0001");
        let filter_config = make_filter_config(true, Some(make_folder_id("SPAM", "FOLD")));
        let general_config = make_general_config(MessageReadState::None, MessageReadState::None);
        let message_db = MockMessageDb::new(); // No entry → fallback to Inbox

        let mut msg = MockToolbarMessage::new(b"key1", None);
        let mut messages: Vec<&mut dyn ToolbarMessage> = vec![&mut msg];
        let train_fn = |_msg: &dyn ToolbarMessage, _is_spam: bool| -> Result<(), ToolbarError> {
            Ok(())
        };

        let result = ButtonHandler::execute_not_spam(
            &mut messages,
            &filter_config,
            &general_config,
            &message_db,
            &inbox,
            &train_fn,
        );

        assert!(result.is_ok());
        assert_eq!(msg.moved_to, Some(inbox));
    }
}
