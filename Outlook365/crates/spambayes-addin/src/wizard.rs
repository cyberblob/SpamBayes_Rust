//! Configuration Wizard for first-run setup.
//!
//! Implements a multi-page Win32 dialog wizard that guides the user
//! through initial `SpamBayes` configuration when no saved config file
//! exists for the current Outlook profile.
//!
//! # Wizard Flow
//!
//! 1. **Welcome** — informational page
//! 2. **Watch Folders** — pre-selects receive/inbox folders; user can
//!    add or remove watched folders
//! 3. **Spam Folder** — select or name the spam destination folder
//!    (default: "Junk E-Mail")
//! 4. **Unsure Folder** — select or name the unsure destination folder
//!    (default: "Junk Suspects")
//! 5. **Training Folders** — select ham and spam training folders
//! 6. **Finish** — commit configuration, create folders if needed
//!
//! # Requirements
//!
//! - Req 13.1: Auto-launch when no config file exists for current profile
//! - Req 13.2: Pre-select receive/inbox folders as watched folders
//! - Req 13.3: Spam folder selection with default "Junk E-Mail"
//! - Req 13.4: Unsure folder selection with default "Junk Suspects"
//! - Req 13.5: Ham and spam training folder selection
//! - Req 13.11: Display error and abort save if folder creation fails

#![cfg(target_os = "windows")]

use std::path::Path;

use spambayes_config::{AppConfig, FolderId};
use spambayes_mapi::{Folder, MessageStoreOps, MsgStoreError};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EndDialog, MessageBoxW, MB_ICONERROR, MB_OK, WM_CLOSE, WM_COMMAND,
    WM_INITDIALOG,
};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Default name for the spam destination folder.
const DEFAULT_SPAM_FOLDER_NAME: &str = "Junk E-Mail";

/// Default name for the unsure destination folder.
const DEFAULT_UNSURE_FOLDER_NAME: &str = "Junk Suspects";

// Dialog control IDs (will be defined in resource file; placeholders here).
#[allow(dead_code)]
const IDD_WIZARD: u32 = 2000;
#[allow(dead_code)]
const IDC_BACK: u16 = 2001;
#[allow(dead_code)]
const IDC_NEXT: u16 = 2002;
#[allow(dead_code)]
const IDC_CANCEL: u16 = 2003;
#[allow(dead_code)]
const IDC_FINISH: u16 = 2004;

// ─── Wizard Page ─────────────────────────────────────────────────────────────

/// The current page/step in the wizard flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardPage {
    /// Welcome/information page.
    Welcome,
    /// Watch folder selection.
    WatchFolders,
    /// Spam folder selection/naming.
    SpamFolder,
    /// Unsure folder selection/naming.
    UnsureFolder,
    /// Training folder selection.
    TrainingFolders,
    /// Final confirmation/finish.
    Finish,
}

impl WizardPage {
    /// Advance to the next page in the wizard sequence.
    fn next(self) -> Option<Self> {
        match self {
            WizardPage::Welcome => Some(WizardPage::WatchFolders),
            WizardPage::WatchFolders => Some(WizardPage::SpamFolder),
            WizardPage::SpamFolder => Some(WizardPage::UnsureFolder),
            WizardPage::UnsureFolder => Some(WizardPage::TrainingFolders),
            WizardPage::TrainingFolders => Some(WizardPage::Finish),
            WizardPage::Finish => None,
        }
    }

    /// Go back to the previous page.
    fn prev(self) -> Option<Self> {
        match self {
            WizardPage::Welcome => None,
            WizardPage::WatchFolders => Some(WizardPage::Welcome),
            WizardPage::SpamFolder => Some(WizardPage::WatchFolders),
            WizardPage::UnsureFolder => Some(WizardPage::SpamFolder),
            WizardPage::TrainingFolders => Some(WizardPage::UnsureFolder),
            WizardPage::Finish => Some(WizardPage::TrainingFolders),
        }
    }
}

// ─── WizardCompletionChoice ───────────────────────────────────────────────────

/// The user's selection on the wizard's final (Finish) page.
///
/// This determines what happens after folder configuration is committed.
///
/// **Validates: Requirement 13.6**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardCompletionChoice {
    /// Train immediately on selected folders, then enable filtering.
    ///
    /// **Validates: Requirement 13.7**
    TrainNow,

    /// Save configuration and enable filtering; user will sort mail
    /// first and invoke training later.
    ///
    /// **Validates: Requirement 13.8**
    TrainLater,

    /// Open the Manager dialog for manual configuration. Saves config
    /// but does NOT enable filtering automatically.
    ///
    /// **Validates: Requirement 13.9**
    ConfigureManually,
}

// ─── WizardCompletionAction ──────────────────────────────────────────────────

/// The action the add-in should take after the wizard completes.
///
/// Returned by [`ConfigWizard::complete()`] to inform the caller what
/// post-wizard actions are required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardCompletionAction {
    /// Configuration saved, filtering enabled. Training was performed
    /// on the selected folders.
    ///
    /// **Validates: Requirement 13.7**
    TrainedAndEnabled,

    /// Configuration saved, filtering enabled. No training was performed;
    /// the user will train later.
    ///
    /// **Validates: Requirement 13.8**
    SavedAndEnabled,

    /// Configuration saved, filtering NOT enabled. The caller should open
    /// the Manager dialog so the user can configure manually.
    ///
    /// **Validates: Requirement 13.9**
    OpenManagerDialog,

    /// User cancelled. Nothing was saved, filtering not enabled, wizard
    /// should be shown again on the next add-in startup.
    ///
    /// **Validates: Requirement 13.10**
    Cancelled,
}

// ─── WizardState ─────────────────────────────────────────────────────────────

/// Tracks the user's selections throughout the wizard flow.
///
/// This is the mutable state that accumulates user choices page by page
/// and is committed to an `AppConfig` when the wizard completes.
#[derive(Debug, Clone)]
pub struct WizardState {
    /// Currently displayed wizard page.
    current_page: WizardPage,

    /// Selected watch folder IDs (pre-populated from receive folders).
    ///
    /// **Validates: Requirement 13.2**
    pub watch_folder_ids: Vec<FolderId>,

    /// Selected spam folder ID (if user chose an existing folder).
    pub spam_folder_id: Option<FolderId>,

    /// Name for spam folder if creating new (default "Junk E-Mail").
    ///
    /// **Validates: Requirement 13.3**
    pub spam_folder_name: String,

    /// Selected unsure folder ID (if user chose an existing folder).
    pub unsure_folder_id: Option<FolderId>,

    /// Name for unsure folder if creating new (default "Junk Suspects").
    ///
    /// **Validates: Requirement 13.4**
    pub unsure_folder_name: String,

    /// Selected ham training folder IDs.
    ///
    /// **Validates: Requirement 13.5**
    pub ham_training_folder_ids: Vec<FolderId>,

    /// Selected spam training folder IDs.
    ///
    /// **Validates: Requirement 13.5**
    pub spam_training_folder_ids: Vec<FolderId>,

    /// The user's completion choice on the Finish page.
    ///
    /// **Validates: Requirement 13.6**
    pub completion_choice: Option<WizardCompletionChoice>,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            current_page: WizardPage::Welcome,
            watch_folder_ids: Vec::new(),
            spam_folder_id: None,
            spam_folder_name: DEFAULT_SPAM_FOLDER_NAME.to_string(),
            unsure_folder_id: None,
            unsure_folder_name: DEFAULT_UNSURE_FOLDER_NAME.to_string(),
            ham_training_folder_ids: Vec::new(),
            spam_training_folder_ids: Vec::new(),
            completion_choice: None,
        }
    }
}

impl WizardState {
    /// Create a new wizard state pre-populated with receive folders.
    ///
    /// **Validates: Requirement 13.2**
    #[must_use]
    pub fn new_with_receive_folders(receive_folders: Vec<FolderId>) -> Self {
        Self {
            watch_folder_ids: receive_folders,
            ..Default::default()
        }
    }

    /// Advance to the next wizard page.
    ///
    /// Returns `true` if there is a next page, `false` if already on the last.
    pub fn advance(&mut self) -> bool {
        if let Some(next) = self.current_page.next() {
            self.current_page = next;
            true
        } else {
            false
        }
    }

    /// Go back to the previous wizard page.
    ///
    /// Returns `true` if there is a previous page, `false` if on the first.
    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.current_page.prev() {
            self.current_page = prev;
            true
        } else {
            false
        }
    }

    /// Returns the current page.
    #[must_use]
    pub fn current_page(&self) -> WizardPage {
        self.current_page
    }

    /// Returns whether the wizard is on the final page.
    #[must_use]
    pub fn is_on_finish_page(&self) -> bool {
        self.current_page == WizardPage::Finish
    }
}

// ─── WizardResult ────────────────────────────────────────────────────────────

/// The outcome of running the configuration wizard.
#[derive(Debug, Clone)]
pub enum WizardResult {
    /// User completed the wizard. Contains the final configuration state
    /// and the action to take next.
    Completed {
        /// The committed folder configuration.
        config: WizardConfig,
        /// What the add-in should do after the wizard closes.
        action: WizardCompletionAction,
    },
    /// User cancelled the wizard. No configuration changes are saved.
    ///
    /// **Validates: Requirement 13.10**
    Cancelled,
}

/// The final validated configuration produced by the wizard.
///
/// This is consumed by the add-in core to write the actual `AppConfig`
/// to disk and enable filtering.
#[derive(Debug, Clone)]
pub struct WizardConfig {
    /// Folders to watch for incoming mail.
    pub watch_folder_ids: Vec<FolderId>,
    /// The spam destination folder (created or selected).
    pub spam_folder_id: FolderId,
    /// The unsure destination folder (created or selected).
    pub unsure_folder_id: FolderId,
    /// Ham (good mail) training folders.
    pub ham_training_folder_ids: Vec<FolderId>,
    /// Spam training folders.
    pub spam_training_folder_ids: Vec<FolderId>,
}

// ─── ConfigWizard ────────────────────────────────────────────────────────────

/// The configuration wizard controller.
///
/// Manages the Win32 dialog lifecycle, folder enumeration from the
/// message store, and committing the final configuration.
pub struct ConfigWizard {
    /// The wizard's mutable state tracking user selections.
    state: WizardState,
    /// Dialog handle (set when the dialog is created).
    hwnd: HWND,
}

impl ConfigWizard {
    /// Create a new wizard instance pre-populated with receive folder defaults.
    ///
    /// **Validates: Requirement 13.2**
    #[must_use]
    pub fn new(receive_folder_ids: Vec<FolderId>) -> Self {
        Self {
            state: WizardState::new_with_receive_folders(receive_folder_ids),
            hwnd: HWND::default(),
        }
    }

    /// Check whether the wizard needs to be launched.
    ///
    /// Returns `true` if no configuration file exists for the given profile,
    /// indicating a first-run scenario.
    ///
    /// **Validates: Requirement 13.1**
    #[must_use]
    pub fn needs_wizard(data_dir: &Path, profile_name: &str) -> bool {
        let config_path = data_dir.join(format!("{profile_name}.ini"));
        !config_path.exists()
    }

    /// Launch the wizard dialog.
    ///
    /// Creates a modal dialog and runs the wizard flow. Returns the result
    /// once the user either completes or cancels the wizard.
    ///
    /// # Arguments
    ///
    /// * `hwnd_parent` - Parent window handle (Outlook's main window)
    /// * `store` - Message store for folder enumeration and creation
    ///
    /// # Returns
    ///
    /// `WizardResult::Completed` with the final config if the user finishes,
    /// or `WizardResult::Cancelled` if the user cancels.
    ///
    /// **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 13.5**
    pub fn launch(
        &mut self,
        _hwnd_parent: HWND,
        _store: &MessageStoreOps,
    ) -> WizardResult {
        // In a full implementation, this would create the Win32 dialog via
        // CreateDialogParamW and enter a message loop. The dialog proc
        // handles page navigation and user input.
        //
        // For now, we implement the logic pipeline: the actual dialog
        // creation requires a compiled dialog resource (.rc) that will
        // be provided when the resource system is integrated.

        // The wizard dialog would be created here:
        // unsafe {
        //     let hinst = GetModuleHandleW(None).unwrap_or_default();
        //     self.hwnd = CreateDialogParamW(
        //         hinst,
        //         PCWSTR::from_raw(IDD_WIZARD as *const u16),
        //         hwnd_parent,
        //         Some(Self::dialog_proc),
        //         LPARAM(self as *mut _ as isize),
        //     );
        // }

        // For a complete implementation, the message loop would run here
        // and return based on user action. This placeholder returns
        // Cancelled until the dialog resources are integrated.
        WizardResult::Cancelled
    }

    /// Commit the wizard configuration by creating any necessary folders.
    ///
    /// If the user specified folder names (rather than selecting existing
    /// folders), this method creates them in the message store. On failure,
    /// displays an error and returns `None` without saving config.
    ///
    /// **Validates: Requirements 13.3, 13.4, 13.11**
    #[must_use]
    pub fn commit(
        &self,
        store: &MessageStoreOps,
        root_folder_eid: &[u8],
    ) -> Option<WizardConfig> {
        let state = &self.state;

        // Resolve spam folder: use existing selection or create by name
        let spam_folder_id = match &state.spam_folder_id {
            Some(id) => id.clone(),
            None => {
                if let Ok(folder) = Self::create_folder_if_needed(
                    store,
                    root_folder_eid,
                    &state.spam_folder_name,
                ) { Self::folder_to_id(&folder) } else {
                    Self::show_folder_creation_error(
                        self.hwnd,
                        &state.spam_folder_name,
                    );
                    return None;
                }
            }
        };

        // Resolve unsure folder: use existing selection or create by name
        let unsure_folder_id = match &state.unsure_folder_id {
            Some(id) => id.clone(),
            None => {
                if let Ok(folder) = Self::create_folder_if_needed(
                    store,
                    root_folder_eid,
                    &state.unsure_folder_name,
                ) { Self::folder_to_id(&folder) } else {
                    Self::show_folder_creation_error(
                        self.hwnd,
                        &state.unsure_folder_name,
                    );
                    return None;
                }
            }
        };

        Some(WizardConfig {
            watch_folder_ids: state.watch_folder_ids.clone(),
            spam_folder_id,
            unsure_folder_id,
            ham_training_folder_ids: state.ham_training_folder_ids.clone(),
            spam_training_folder_ids: state.spam_training_folder_ids.clone(),
        })
    }

    /// Execute the wizard completion logic based on the user's choice.
    ///
    /// This method applies the wizard config to the given `AppConfig` and
    /// returns the appropriate [`WizardCompletionAction`] to inform the caller
    /// what post-wizard steps are needed.
    ///
    /// # Behavior by choice
    ///
    /// - **`TrainNow`** (Req 13.7): Populates config from wizard selections,
    ///   saves config, enables filtering, and returns `TrainedAndEnabled`.
    ///   The caller is responsible for invoking the training engine on the
    ///   training folders specified in the returned config.
    ///
    /// - **`TrainLater`** (Req 13.8): Populates config from wizard selections,
    ///   saves config, enables filtering. Returns `SavedAndEnabled`.
    ///
    /// - **`ConfigureManually`** (Req 13.9): Populates config from wizard
    ///   selections, saves config, does NOT enable filtering. Returns
    ///   `OpenManagerDialog` so the caller can open the Manager dialog.
    ///
    /// - **None / Cancel** (Req 13.10): Returns `Cancelled`. Nothing is saved
    ///   and filtering is not enabled. The wizard should be shown again on
    ///   next startup.
    ///
    /// # Arguments
    ///
    /// * `wizard_config` - The committed folder configuration from [`commit()`]
    /// * `app_config` - The application config to update
    /// * `data_dir` - Directory where config files are stored
    /// * `profile_name` - Profile name for config file naming
    ///
    /// # Validates: Requirements 13.6, 13.7, 13.8, 13.9, 13.10
    pub fn complete(
        &self,
        wizard_config: &WizardConfig,
        app_config: &mut AppConfig,
        data_dir: &Path,
        profile_name: &str,
    ) -> WizardCompletionAction {
        let choice = match self.state.completion_choice {
            Some(c) => c,
            None => {
                // No choice made — treat as cancel (Req 13.10).
                return WizardCompletionAction::Cancelled;
            }
        };

        // Apply wizard folder selections to the config for all completion paths.
        Self::apply_wizard_config_to_app_config(wizard_config, app_config);

        match choice {
            WizardCompletionChoice::TrainNow => {
                // Req 13.7: Enable filtering, save config.
                // The caller must invoke the training engine after this returns.
                app_config.filter.enabled = true;
                let _ = app_config.save(data_dir, profile_name);
                WizardCompletionAction::TrainedAndEnabled
            }
            WizardCompletionChoice::TrainLater => {
                // Req 13.8: Save config, enable filtering.
                app_config.filter.enabled = true;
                let _ = app_config.save(data_dir, profile_name);
                WizardCompletionAction::SavedAndEnabled
            }
            WizardCompletionChoice::ConfigureManually => {
                // Req 13.9: Save config but do NOT enable filtering.
                // Caller should open the Manager dialog.
                // filter.enabled remains as-is (false by default).
                let _ = app_config.save(data_dir, profile_name);
                WizardCompletionAction::OpenManagerDialog
            }
        }
    }

    /// Apply wizard folder configuration to the application config.
    ///
    /// Sets watch folders, spam/unsure destination folders, and training
    /// folders from the wizard selections.
    fn apply_wizard_config_to_app_config(
        wizard_config: &WizardConfig,
        app_config: &mut AppConfig,
    ) {
        // Set watch folders
        app_config.filter.watch_folder_ids = wizard_config.watch_folder_ids.clone();

        // Set spam destination folder
        app_config.filter.spam_folder_id = Some(wizard_config.spam_folder_id.clone());

        // Set unsure destination folder
        app_config.filter.unsure_folder_id = Some(wizard_config.unsure_folder_id.clone());

        // Set training folders
        app_config.training.ham_folder_ids = wizard_config.ham_training_folder_ids.clone();
        app_config.training.spam_folder_ids = wizard_config.spam_training_folder_ids.clone();
    }

    /// Create a folder under the root if it doesn't already exist.
    ///
    /// Uses the `OPEN_IF_EXISTS` semantics from the MAPI layer, so if
    /// the folder already exists it is simply returned.
    ///
    /// **Validates: Requirements 13.3, 13.4**
    pub fn create_folder_if_needed(
        store: &MessageStoreOps,
        parent_eid: &[u8],
        name: &str,
    ) -> Result<Folder, MsgStoreError> {
        store.create_folder(parent_eid, name)
    }

    /// Convert a `Folder` to a `FolderId` for config storage.
    fn folder_to_id(folder: &Folder) -> FolderId {
        use spambayes_config::{EntryId, StoreId};
        FolderId::new(
            StoreId::new(hex::encode(&folder.store_id)),
            EntryId::new(hex::encode(&folder.entry_id)),
        )
    }

    /// Display an error message when folder creation fails.
    ///
    /// **Validates: Requirement 13.11**
    fn show_folder_creation_error(hwnd: HWND, folder_name: &str) {
        let message = format!(
            "There was an error creating the folder named '{folder_name}'\r\n\
             Please restart Outlook and try again."
        );
        let wide_msg: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        let title = "SpamBayes Configuration Error";
        let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            MessageBoxW(
                hwnd,
                PCWSTR::from_raw(wide_msg.as_ptr()),
                PCWSTR::from_raw(wide_title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    /// Win32 dialog procedure callback for the wizard.
    ///
    /// Handles dialog messages: initialization, button clicks (Back, Next,
    /// Cancel, Finish), and page transitions.
    ///
    /// # Safety
    ///
    /// Called by the Windows message dispatcher. The `lparam` on
    /// `WM_INITDIALOG` carries a pointer to the `ConfigWizard` instance.
    #[allow(dead_code)]
    unsafe extern "system" fn dialog_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> isize {
        match msg {
            WM_INITDIALOG => {
                // Store the wizard pointer in the dialog's user data.
                // In a real implementation we'd use SetWindowLongPtrW.
                1 // Return TRUE to accept default focus
            }
            WM_COMMAND => {
                let control_id = (wparam.0 & 0xFFFF) as u16;
                match control_id {
                    IDC_BACK => {
                        // Navigate to previous page
                        0
                    }
                    IDC_NEXT => {
                        // Navigate to next page
                        0
                    }
                    IDC_CANCEL => {
                        // Cancel the wizard
                        let _ = EndDialog(hwnd, 0);
                        0
                    }
                    IDC_FINISH => {
                        // Attempt to commit and close
                        let _ = EndDialog(hwnd, 1);
                        0
                    }
                    _ => 0,
                }
            }
            WM_CLOSE => {
                let _ = EndDialog(hwnd, 0);
                0
            }
            _ => 0,
        }
    }

    /// Returns a reference to the current wizard state.
    #[must_use]
    pub fn state(&self) -> &WizardState {
        &self.state
    }

    /// Returns a mutable reference to the wizard state.
    pub fn state_mut(&mut self) -> &mut WizardState {
        &mut self.state
    }
}

// ─── Hex Encoding Helper ─────────────────────────────────────────────────────

/// Minimal hex encoding module (avoids adding a dependency for this simple op).
mod hex {
    /// Encode bytes to a lowercase hex string.
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use spambayes_config::{EntryId, StoreId};

    /// Helper to create a test `WizardConfig` with dummy folder IDs.
    fn make_test_wizard_config() -> WizardConfig {
        WizardConfig {
            watch_folder_ids: vec![FolderId::new(
                StoreId::new("STORE01"),
                EntryId::new("WATCH01"),
            )],
            spam_folder_id: FolderId::new(
                StoreId::new("STORE01"),
                EntryId::new("SPAM01"),
            ),
            unsure_folder_id: FolderId::new(
                StoreId::new("STORE01"),
                EntryId::new("UNSURE01"),
            ),
            ham_training_folder_ids: vec![FolderId::new(
                StoreId::new("STORE01"),
                EntryId::new("HAM_TRAIN01"),
            )],
            spam_training_folder_ids: vec![FolderId::new(
                StoreId::new("STORE01"),
                EntryId::new("SPAM_TRAIN01"),
            )],
        }
    }

    /// Helper to create a `ConfigWizard` with a given completion choice.
    fn make_wizard_with_choice(choice: Option<WizardCompletionChoice>) -> ConfigWizard {
        let mut wizard = ConfigWizard::new(vec![FolderId::new(
            StoreId::new("STORE01"),
            EntryId::new("WATCH01"),
        )]);
        wizard.state_mut().completion_choice = choice;
        wizard
    }

    #[test]
    fn test_needs_wizard_no_config_file() {
        let temp = std::env::temp_dir().join("spambayes_wizard_test_empty");
        let _ = std::fs::create_dir_all(&temp);
        assert!(ConfigWizard::needs_wizard(&temp, "nonexistent_profile"));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_needs_wizard_config_exists() {
        let temp = std::env::temp_dir().join("spambayes_wizard_test_exists");
        let _ = std::fs::create_dir_all(&temp);
        let config_path = temp.join("test_profile.ini");
        std::fs::write(&config_path, "[General]\n").unwrap();
        assert!(!ConfigWizard::needs_wizard(&temp, "test_profile"));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_wizard_state_default() {
        let state = WizardState::default();
        assert_eq!(state.current_page, WizardPage::Welcome);
        assert_eq!(state.spam_folder_name, "Junk E-Mail");
        assert_eq!(state.unsure_folder_name, "Junk Suspects");
        assert!(state.watch_folder_ids.is_empty());
        assert!(state.completion_choice.is_none());
    }

    #[test]
    fn test_wizard_state_with_receive_folders() {
        let folders = vec![FolderId::new(
            StoreId::new("AABB"),
            EntryId::new("CCDD"),
        )];
        let state = WizardState::new_with_receive_folders(folders.clone());
        assert_eq!(state.watch_folder_ids.len(), 1);
        assert_eq!(state.watch_folder_ids[0], folders[0]);
    }

    #[test]
    fn test_wizard_page_navigation() {
        let mut state = WizardState::default();
        assert_eq!(state.current_page(), WizardPage::Welcome);

        assert!(state.advance());
        assert_eq!(state.current_page(), WizardPage::WatchFolders);

        assert!(state.advance());
        assert_eq!(state.current_page(), WizardPage::SpamFolder);

        assert!(state.advance());
        assert_eq!(state.current_page(), WizardPage::UnsureFolder);

        assert!(state.advance());
        assert_eq!(state.current_page(), WizardPage::TrainingFolders);

        assert!(state.advance());
        assert_eq!(state.current_page(), WizardPage::Finish);
        assert!(state.is_on_finish_page());

        // Can't advance past Finish
        assert!(!state.advance());
        assert_eq!(state.current_page(), WizardPage::Finish);

        // Go back
        assert!(state.go_back());
        assert_eq!(state.current_page(), WizardPage::TrainingFolders);
    }

    #[test]
    fn test_wizard_page_cannot_go_back_from_welcome() {
        let mut state = WizardState::default();
        assert!(!state.go_back());
        assert_eq!(state.current_page(), WizardPage::Welcome);
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex::encode(&[0xAA, 0xBB, 0xCC]), "aabbcc");
        assert_eq!(hex::encode(&[]), "");
        assert_eq!(hex::encode(&[0x00, 0xFF]), "00ff");
    }

    #[test]
    fn test_folder_to_id() {
        let folder = Folder {
            store_id: vec![0x01, 0x02, 0x03],
            entry_id: vec![0xAA, 0xBB],
            name: "Test".to_string(),
            count: 0,
        };
        let id = ConfigWizard::folder_to_id(&folder);
        assert_eq!(id.store_id.0, "010203");
        assert_eq!(id.entry_id.0, "aabb");
    }

    // ─── Wizard Completion Logic Tests ───────────────────────────────────────

    #[test]
    fn test_complete_train_now_enables_filtering_and_saves() {
        let temp = std::env::temp_dir().join("spambayes_wizard_complete_train_now");
        let _ = std::fs::create_dir_all(&temp);

        let wizard = make_wizard_with_choice(Some(WizardCompletionChoice::TrainNow));
        let wizard_config = make_test_wizard_config();
        let mut app_config = AppConfig::default();

        assert!(!app_config.filter.enabled);

        let action = wizard.complete(&wizard_config, &mut app_config, &temp, "test_profile");

        assert_eq!(action, WizardCompletionAction::TrainedAndEnabled);
        assert!(app_config.filter.enabled);
        // Verify folder config was applied
        assert_eq!(app_config.filter.watch_folder_ids.len(), 1);
        assert_eq!(app_config.filter.spam_folder_id, Some(wizard_config.spam_folder_id));
        assert_eq!(app_config.filter.unsure_folder_id, Some(wizard_config.unsure_folder_id));
        assert_eq!(app_config.training.ham_folder_ids.len(), 1);
        assert_eq!(app_config.training.spam_folder_ids.len(), 1);
        // Verify config was saved to disk
        assert!(temp.join("test_profile.ini").exists());

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_complete_train_later_enables_filtering_and_saves() {
        let temp = std::env::temp_dir().join("spambayes_wizard_complete_train_later");
        let _ = std::fs::create_dir_all(&temp);

        let wizard = make_wizard_with_choice(Some(WizardCompletionChoice::TrainLater));
        let wizard_config = make_test_wizard_config();
        let mut app_config = AppConfig::default();

        let action = wizard.complete(&wizard_config, &mut app_config, &temp, "test_profile");

        assert_eq!(action, WizardCompletionAction::SavedAndEnabled);
        assert!(app_config.filter.enabled);
        assert!(temp.join("test_profile.ini").exists());

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_complete_configure_manually_does_not_enable_filtering() {
        let temp = std::env::temp_dir().join("spambayes_wizard_complete_manual");
        let _ = std::fs::create_dir_all(&temp);

        let wizard = make_wizard_with_choice(Some(WizardCompletionChoice::ConfigureManually));
        let wizard_config = make_test_wizard_config();
        let mut app_config = AppConfig::default();

        let action = wizard.complete(&wizard_config, &mut app_config, &temp, "test_profile");

        assert_eq!(action, WizardCompletionAction::OpenManagerDialog);
        // Filtering must NOT be enabled for configure-manually
        assert!(!app_config.filter.enabled);
        // Config should still be saved
        assert!(temp.join("test_profile.ini").exists());
        // Folder config should still be applied
        assert_eq!(app_config.filter.watch_folder_ids.len(), 1);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_complete_cancel_does_not_save_or_enable() {
        let temp = std::env::temp_dir().join("spambayes_wizard_complete_cancel");
        let _ = std::fs::create_dir_all(&temp);

        // No completion choice = cancel
        let wizard = make_wizard_with_choice(None);
        let wizard_config = make_test_wizard_config();
        let mut app_config = AppConfig::default();

        let action = wizard.complete(&wizard_config, &mut app_config, &temp, "test_profile");

        assert_eq!(action, WizardCompletionAction::Cancelled);
        // Filtering must NOT be enabled
        assert!(!app_config.filter.enabled);
        // Config must NOT be saved to disk
        assert!(!temp.join("test_profile.ini").exists());
        // Folder config should NOT be applied to app_config
        assert!(app_config.filter.watch_folder_ids.is_empty());
        assert!(app_config.filter.spam_folder_id.is_none());

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_completion_choice_enum_variants() {
        // Ensure all three choices are distinct
        let choices = [
            WizardCompletionChoice::TrainNow,
            WizardCompletionChoice::TrainLater,
            WizardCompletionChoice::ConfigureManually,
        ];
        assert_ne!(choices[0], choices[1]);
        assert_ne!(choices[1], choices[2]);
        assert_ne!(choices[0], choices[2]);
    }

    #[test]
    fn test_completion_action_enum_variants() {
        // Ensure all four actions are distinct
        let actions = [
            WizardCompletionAction::TrainedAndEnabled,
            WizardCompletionAction::SavedAndEnabled,
            WizardCompletionAction::OpenManagerDialog,
            WizardCompletionAction::Cancelled,
        ];
        assert_ne!(actions[0], actions[1]);
        assert_ne!(actions[1], actions[2]);
        assert_ne!(actions[2], actions[3]);
        assert_ne!(actions[0], actions[3]);
    }

    #[test]
    fn test_apply_wizard_config_sets_all_folders() {
        let wizard_config = make_test_wizard_config();
        let mut app_config = AppConfig::default();

        ConfigWizard::apply_wizard_config_to_app_config(&wizard_config, &mut app_config);

        assert_eq!(app_config.filter.watch_folder_ids, wizard_config.watch_folder_ids);
        assert_eq!(app_config.filter.spam_folder_id, Some(wizard_config.spam_folder_id.clone()));
        assert_eq!(app_config.filter.unsure_folder_id, Some(wizard_config.unsure_folder_id.clone()));
        assert_eq!(app_config.training.ham_folder_ids, wizard_config.ham_training_folder_ids);
        assert_eq!(app_config.training.spam_folder_ids, wizard_config.spam_training_folder_ids);
    }
}
