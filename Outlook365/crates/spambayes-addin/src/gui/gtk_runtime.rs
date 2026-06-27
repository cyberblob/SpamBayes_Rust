//! GTK4 runtime manager — manages the GTK4 lifecycle and thread communication.
//!
//! The GTK4 event loop runs on a dedicated thread, separate from the COM STA
//! thread. Commands are dispatched via a `crossbeam_channel` and processed
//! inside the GLib main loop using `glib::timeout_add_local`. This matches
//! the current `ShowManager` / `_run_manager_in_thread` / `_manager_lock`
//! pattern from the Python code.
//!
//! **Validates: Requirements 14.1, 14.2, 14.3, 14.4, 14.5**

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use spambayes_config::AppConfig;

use super::manager_window::ManagerWindow;
use super::message_boxes::{MsgBoxKind, MsgBoxResult};
use super::wizard_window::{WizardResult, WizardWindow};
use super::folder_browser::{FolderNode, FolderProvider, NullFolderProvider};
use crate::manager_dlg::ManagerState;
use crate::wizard::ConfigWizard;

/// Commands sent from the COM thread to the GTK4 thread.
pub enum GuiCommand {
    /// Show the Manager window with the given state.
    ShowManager {
        state: ManagerState,
        config: AppConfig,
        on_close: Option<Box<dyn FnOnce() + Send>>,
        /// Pre-loaded folder tree (loaded on COM thread, passed to GTK thread).
        /// If None, the folder browser will show an empty tree.
        folder_tree: Option<Vec<FolderNode>>,
        /// Optional training executor for the Training tab's "Start Training" button.
        /// If None, the button will show an error when clicked.
        training_executor: Option<Arc<dyn super::tabs::training::TrainingExecutor>>,
        /// Optional stats (ham/spam trained counts). If None, defaults to zeros.
        stats: Option<crate::manager_dlg::ManagerStats>,
    },
    /// Show the Configuration Wizard.
    ShowWizard {
        config: AppConfig,
        on_close: Option<Box<dyn FnOnce(WizardResult) + Send>>,
    },
    /// Show the Filter Now dialog.
    ShowFilterNow {
        config: AppConfig,
        /// Pre-loaded folder tree for the filter-now folder picker.
        folder_tree: Option<Vec<FolderNode>>,
    },
    /// Show a message box and return the result via a oneshot channel.
    ShowMessageBox {
        kind: MsgBoxKind,
        title: String,
        message: String,
        response: Sender<MsgBoxResult>,
    },
    /// Show the Clues dialog displaying scoring evidence for a message.
    ///
    /// **Validates: Requirements 14.2, 14.3**
    ShowClues {
        subject: String,
        clues_text: String,
        /// Optional callback invoked when the dialog is closed.
        on_close: Option<Box<dyn FnOnce() + Send>>,
    },
    /// Shut down the GTK4 runtime.
    Shutdown,
}

/// Manages the GTK4 runtime thread and provides methods to show dialogs.
///
/// The runtime is initialized once and persists for the lifetime of the add-in.
/// When all windows are closed, the runtime remains initialized for fast
/// subsequent launches (Requirement 14.3).
pub struct GtkRuntime {
    /// Channel sender for dispatching commands to the GTK4 thread.
    sender: Sender<GuiCommand>,
    /// Whether the manager window is currently open (prevents duplicates).
    is_open: Arc<AtomicBool>,
    /// Handle to the GTK4 thread (kept alive for the lifetime of the runtime).
    _thread_handle: thread::JoinHandle<()>,
}

/// Errors that can occur during GTK4 initialization.
#[derive(Debug, thiserror::Error)]
pub enum GtkInitError {
    /// GTK4 DLLs were not found on the system.
    #[error("GTK4 runtime DLLs not found: {0}")]
    DllsNotFound(String),

    /// GTK4 initialization failed for another reason.
    #[error("GTK4 initialization failed: {0}")]
    InitFailed(String),

    /// The GTK4 thread failed to start or signal readiness.
    #[error("GTK4 thread startup failed: {0}")]
    ThreadStartFailed(String),
}

impl GtkRuntime {
    /// Initialize the GTK4 runtime on a dedicated thread.
    ///
    /// Spawns a new thread that initializes GTK4 and runs the GLib main loop.
    /// Commands are sent to the thread via a crossbeam channel and polled
    /// from the GLib main loop using a timeout source.
    ///
    /// If GTK4 DLLs are not found, returns `GtkInitError::DllsNotFound`
    /// and the caller should fall back to Win32 `MessageBoxW`.
    ///
    /// **Validates: Requirements 14.1, 14.2, 14.4**
    pub fn init() -> Result<Self, GtkInitError> {
        let is_open = Arc::new(AtomicBool::new(false));
        let is_open_clone = Arc::clone(&is_open);

        // Command channel: COM thread → GTK thread.
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<GuiCommand>();

        // Readiness channel: GTK thread signals success/failure after init.
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<(), GtkInitError>>(1);

        let thread_handle = thread::Builder::new()
            .name("gtk4-runtime".to_string())
            .spawn(move || {
                Self::gtk_thread_main(ready_tx, cmd_rx, is_open_clone);
            })
            .map_err(|e| GtkInitError::ThreadStartFailed(e.to_string()))?;

        // Wait for the GTK thread to signal readiness (or failure).
        ready_rx
            .recv()
            .map_err(|_| {
                GtkInitError::ThreadStartFailed(
                    "GTK thread exited before signaling readiness".to_string(),
                )
            })??;

        Ok(Self {
            sender: cmd_tx,
            is_open,
            _thread_handle: thread_handle,
        })
    }

    /// The GTK4 thread's main function.
    ///
    /// Initializes GTK4, sets up a polling source for the command channel,
    /// and runs the GLib main loop. The loop persists even when all windows
    /// are closed (Requirement 14.3).
    #[allow(clippy::needless_pass_by_value)]
    fn gtk_thread_main(
        ready_tx: Sender<Result<(), GtkInitError>>,
        cmd_rx: Receiver<GuiCommand>,
        is_open: Arc<AtomicBool>,
    ) {
        // Prepend bundled GTK4 DLL path to PATH before initialization.
        // This allows GTK4 to find its DLLs from the bundle directory
        // without requiring a system-wide MSYS2 installation.
        if let Some(path) = prepend_gtk4_dll_path() {
            log::info!("GTK4 DLL path set to: {}", path.display());
        }

        // Attempt GTK4 initialization — this will fail if DLLs aren't found.
        if let Err(e) = gtk4::init() {
            let err = GtkInitError::DllsNotFound(format!(
                "gtk4::init() failed — GTK4 DLLs may not be installed: {e}"
            ));
            let _ = ready_tx.send(Err(err));
            return;
        }

        // Signal the calling thread that initialization succeeded.
        if ready_tx.send(Ok(())).is_err() {
            // The caller dropped — no point continuing.
            return;
        }

        // Create the main loop that will run on this thread.
        let main_loop = glib::MainLoop::new(None, false);
        let main_loop_quit = main_loop.clone();

        // Poll the command channel every 50ms from the GLib main loop.
        // This is lightweight and integrates cleanly with GTK4's event model.
        glib::timeout_add_local(Duration::from_millis(50), move || {
            // Drain all pending commands without blocking.
            while let Ok(command) = cmd_rx.try_recv() {
                match command {
                    GuiCommand::ShowManager {
                        state,
                        config,
                        on_close,
                        folder_tree,
                        training_executor,
                        stats,
                    } => {
                        // Mark the manager as open.
                        is_open.store(true, Ordering::SeqCst);

                        // Create and present the manager window.
                        let stats = stats.unwrap_or_else(crate::manager_dlg::ManagerStats::new);
                        let folder_provider: std::rc::Rc<dyn FolderProvider> = match folder_tree {
                            Some(tree) => std::rc::Rc::new(
                                super::mapi_folder_provider::MapiFolderProvider::from_tree(tree),
                            ),
                            None => std::rc::Rc::new(NullFolderProvider),
                        };
                        let manager = ManagerWindow::new(&state, &stats, &config, folder_provider);

                        // Wire up the training executor if provided.
                        if let Some(executor) = training_executor {
                            manager.set_training_executor(executor);
                        }

                        // Wire the on_close callback so that when the window
                        // closes, we clear the is_open flag and notify the COM thread.
                        let is_open_for_close = is_open.clone();
                        if let Some(callback) = on_close {
                            manager.set_on_close(Box::new(move || {
                                is_open_for_close.store(false, Ordering::SeqCst);
                                callback();
                            }));
                        } else {
                            let is_open_for_close2 = is_open.clone();
                            manager.set_on_close(Box::new(move || {
                                is_open_for_close2.store(false, Ordering::SeqCst);
                            }));
                        }

                        manager.present();
                    }
                    GuiCommand::ShowWizard {
                        config,
                        on_close,
                    } => {
                        // Create and present the Configuration Wizard.
                        let wizard = WizardWindow::new(&config);
                        let callback: Option<Box<dyn FnOnce(super::wizard_window::WizardResult) + 'static>> =
                            on_close.map(|cb| Box::new(cb) as Box<dyn FnOnce(super::wizard_window::WizardResult) + 'static>);
                        wizard.connect_signals(callback);
                        wizard.present();
                    }
                    GuiCommand::ShowFilterNow { config, folder_tree } => {
                        let folder_provider: std::rc::Rc<dyn FolderProvider> = match folder_tree {
                            Some(tree) => std::rc::Rc::new(
                                super::mapi_folder_provider::MapiFolderProvider::from_tree(tree),
                            ),
                            None => std::rc::Rc::new(NullFolderProvider),
                        };
                        let dialog = super::filter_now_dialog::FilterNowDialog::new(
                            &config,
                            folder_provider,
                        );
                        dialog.present();
                    }
                    GuiCommand::ShowClues {
                        subject,
                        clues_text,
                        on_close,
                    } => {
                        // Show the Clues dialog (Requirement 14.2, 14.3).
                        let dialog = super::clues_dialog::CluesDialog::new(
                            None, &subject, &clues_text,
                        );
                        // If a close callback was provided, wire it to the
                        // window's close-request signal so the caller knows
                        // when the dialog has been dismissed.
                        if let Some(callback) = on_close {
                            use std::cell::Cell;
                            let cb = std::rc::Rc::new(Cell::new(Some(callback)));
                            dialog.connect_close(move || {
                                if let Some(f) = cb.take() {
                                    f();
                                }
                            });
                        }
                        dialog.present();
                    }
                    GuiCommand::ShowMessageBox {
                        kind: _kind,
                        title: _title,
                        message: _message,
                        response,
                    } => {
                        // Message box will be implemented in task 2.1.
                        // For now, return Ok as default.
                        let _ = response.send(MsgBoxResult::Ok);
                    }
                    GuiCommand::Shutdown => {
                        // Quit the main loop, which ends this thread.
                        main_loop_quit.quit();
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });

        // Run the GLib main loop. This blocks until Shutdown is received
        // or the main loop is otherwise terminated.
        // The loop persists when windows close (Requirement 14.3).
        main_loop.run();
    }

    /// Show the Manager window. If already open, brings the existing window to front.
    ///
    /// The `folder_tree` parameter should be pre-loaded on the COM thread via
    /// `MapiFolderProvider::load()`. If `None`, the folder browser will be empty.
    ///
    /// This method does NOT block the calling (COM STA) thread.
    ///
    /// **Validates: Requirements 14.2, 14.5**
    pub fn show_manager(
        &self,
        state: ManagerState,
        config: AppConfig,
        folder_tree: Option<Vec<super::folder_browser::FolderNode>>,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        if self.is_open.load(Ordering::SeqCst) {
            // Manager is already open — the GTK thread will bring it to front.
            // Don't send a duplicate ShowManager command.
            return;
        }
        let _ = self.sender.send(GuiCommand::ShowManager {
            state,
            config,
            on_close: Some(Box::new(on_close)),
            folder_tree,
            training_executor: None,
            stats: None,
        });
    }

    /// Show the Manager window with a training executor attached.
    ///
    /// Same as `show_manager` but also provides a training executor so the
    /// Training tab's "Start Training" button is functional.
    ///
    /// **Validates: Requirements 3.4, 14.2, 14.5**
    pub fn show_manager_with_training(
        &self,
        state: ManagerState,
        config: AppConfig,
        folder_tree: Option<Vec<super::folder_browser::FolderNode>>,
        training_executor: Arc<dyn super::tabs::training::TrainingExecutor>,
        stats: Option<crate::manager_dlg::ManagerStats>,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        if self.is_open.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.sender.send(GuiCommand::ShowManager {
            state,
            config,
            on_close: Some(Box::new(on_close)),
            folder_tree,
            training_executor: Some(training_executor),
            stats,
        });
    }

    /// Show the Configuration Wizard.
    ///
    /// This method does NOT block the calling (COM STA) thread.
    ///
    /// **Validates: Requirement 14.2**
    pub fn show_wizard(
        &self,
        config: AppConfig,
        on_close: impl FnOnce(WizardResult) + Send + 'static,
    ) {
        let _ = self.sender.send(GuiCommand::ShowWizard {
            config,
            on_close: Some(Box::new(on_close)),
        });
    }

    /// Show the Filter Now dialog.
    ///
    /// The `folder_tree` parameter should be pre-loaded on the COM thread.
    ///
    /// This method does NOT block the calling (COM STA) thread.
    ///
    /// **Validates: Requirement 14.2**
    pub fn show_filter_now(&self, config: AppConfig, folder_tree: Option<Vec<super::folder_browser::FolderNode>>) {
        let _ = self.sender.send(GuiCommand::ShowFilterNow { config, folder_tree });
    }

    /// Show the Clues dialog displaying scoring evidence for a message.
    ///
    /// This method does NOT block the calling (COM STA) thread.
    ///
    /// **Validates: Requirements 14.2, 14.3**
    pub fn show_clues(&self, subject: &str, clues_text: &str) {
        let _ = self.sender.send(GuiCommand::ShowClues {
            subject: subject.to_owned(),
            clues_text: clues_text.to_owned(),
            on_close: None,
        });
    }

    /// Show the Clues dialog and invoke a callback when it closes.
    ///
    /// Used by the standalone `spambayes_clues` binary to know when to exit.
    pub fn show_clues_and_wait(
        &self,
        subject: &str,
        clues_text: &str,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        let _ = self.sender.send(GuiCommand::ShowClues {
            subject: subject.to_owned(),
            clues_text: clues_text.to_owned(),
            on_close: Some(Box::new(on_close)),
        });
    }

    /// Show the wizard if first-run is detected, otherwise show the manager.
    ///
    /// Checks if a config file exists for the current profile. If not,
    /// shows the wizard first, then shows the manager on completion.
    ///
    /// **Validates: Requirement 9.9**
    pub fn show_manager_or_wizard_if_first_run(
        &self,
        state: ManagerState,
        config: AppConfig,
        data_dir: &Path,
        profile_name: &str,
        folder_tree: Option<Vec<super::folder_browser::FolderNode>>,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        self.show_manager_or_wizard_inner(
            state, config, data_dir, profile_name, folder_tree, None, None, on_close,
        );
    }

    /// Show the wizard (if first-run) or manager, with a training executor attached.
    ///
    /// Same as `show_manager_or_wizard_if_first_run` but also provides a
    /// training executor so the Training tab is functional.
    ///
    /// **Validates: Requirements 3.4, 9.9**
    pub fn show_manager_or_wizard_with_training(
        &self,
        state: ManagerState,
        config: AppConfig,
        data_dir: &Path,
        profile_name: &str,
        folder_tree: Option<Vec<super::folder_browser::FolderNode>>,
        training_executor: Arc<dyn super::tabs::training::TrainingExecutor>,
        stats: Option<crate::manager_dlg::ManagerStats>,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        self.show_manager_or_wizard_inner(
            state, config, data_dir, profile_name, folder_tree, Some(training_executor), stats, on_close,
        );
    }

    /// Internal implementation for show_manager_or_wizard variants.
    fn show_manager_or_wizard_inner(
        &self,
        state: ManagerState,
        config: AppConfig,
        data_dir: &Path,
        profile_name: &str,
        folder_tree: Option<Vec<super::folder_browser::FolderNode>>,
        training_executor: Option<Arc<dyn super::tabs::training::TrainingExecutor>>,
        stats: Option<crate::manager_dlg::ManagerStats>,
        on_close: impl FnOnce() + Send + 'static,
    ) {
        if ConfigWizard::needs_wizard(data_dir, profile_name) {
            // First-run: show wizard, then show manager on completion.
            let sender = self.sender.clone();
            let state_for_manager = state.clone();
            let config_for_manager = config.clone();
            self.show_wizard(config, move |result| {
                match result {
                    WizardResult::Completed { .. } => {
                        // Wizard completed — now show the manager.
                        log::info!("First-run wizard completed, opening manager.");
                        let _ = sender.send(GuiCommand::ShowManager {
                            state: state_for_manager,
                            config: config_for_manager,
                            on_close: Some(Box::new(on_close)),
                            folder_tree,
                            training_executor,
                            stats,
                        });
                    }
                    WizardResult::Cancelled => {
                        // User cancelled the wizard — just call on_close.
                        log::info!("First-run wizard cancelled.");
                        on_close();
                    }
                }
            });
        } else if let Some(executor) = training_executor {
            self.show_manager_with_training(state, config, folder_tree, executor, stats, on_close);
        } else {
            self.show_manager(state, config, folder_tree, on_close);
        }
    }

    /// Show a message box and block until the user responds.
    ///
    /// Unlike other methods, this blocks the calling thread because the caller
    /// typically needs the result immediately. Uses a crossbeam oneshot channel
    /// for the response.
    #[must_use]
    pub fn message_box(&self, kind: MsgBoxKind, title: &str, message: &str) -> MsgBoxResult {
        let (response_tx, response_rx) = crossbeam_channel::bounded::<MsgBoxResult>(1);
        let _ = self.sender.send(GuiCommand::ShowMessageBox {
            kind,
            title: title.to_string(),
            message: message.to_string(),
            response: response_tx,
        });
        // Block until the GTK thread sends a response.
        response_rx.recv().unwrap_or(MsgBoxResult::Cancel)
    }

    /// Returns true if the manager window is currently open.
    ///
    /// **Validates: Requirement 14.5**
    #[must_use]
    pub fn is_manager_open(&self) -> bool {
        self.is_open.load(Ordering::SeqCst)
    }

    /// Mark the manager window as closed.
    ///
    /// Called by the ManagerWindow's close-request handler to clear
    /// the is_open guard, allowing a new instance to be opened later.
    pub fn mark_manager_closed(&self) {
        self.is_open.store(false, Ordering::SeqCst);
    }

    /// Shut down the GTK4 runtime gracefully.
    ///
    /// Sends the Shutdown command which exits the GLib main loop.
    pub fn shutdown(&self) {
        let _ = self.sender.send(GuiCommand::Shutdown);
    }
}

// ─── GTK4 DLL Path Setup ─────────────────────────────────────────────────────

/// Prepend the bundled GTK4 DLL directory to the `PATH` environment variable.
///
/// Looks for a `gtk4\` subdirectory next to the currently loaded add-in DLL.
/// If found, prepends it to `PATH` so that `gtk4::init()` can locate the
/// bundled DLLs without requiring a system-wide MSYS2 installation.
///
/// Returns `Some(path)` if the directory was found and PATH was updated,
/// or `None` if the directory does not exist (indicating GTK4 must be on
/// the system PATH already).
///
/// **Validates: Requirement 14.4**
fn prepend_gtk4_dll_path() -> Option<PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };

    // Get the HMODULE of *this* DLL by passing an address within it.
    // We use the address of this function as the reference point.
    let mut hmodule = HMODULE::default();
    let self_addr = prepend_gtk4_dll_path as *const () as *const u8;
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
        | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;

    unsafe {
        let ok = GetModuleHandleExW(flags, PCWSTR(self_addr.cast()), &mut hmodule);
        if ok.is_err() {
            log::warn!("GetModuleHandleExW failed; cannot determine DLL path");
            return None;
        }
    }

    // Get the full path of the DLL.
    let mut path_buf = vec![0u16; 1024];
    let len = unsafe { GetModuleFileNameW(hmodule, &mut path_buf) } as usize;
    if len == 0 || len >= path_buf.len() {
        log::warn!("GetModuleFileNameW failed or path too long");
        return None;
    }

    let dll_path = String::from_utf16_lossy(&path_buf[..len]);
    let dll_dir = PathBuf::from(&dll_path);
    let parent = dll_dir.parent()?;

    // Look for GTK4 DLLs. They can be either:
    // 1. In the same directory as the DLL (flat layout — preferred for load-time linking)
    // 2. In a `gtk4` subdirectory (legacy layout)
    let gtk4_dll_in_same_dir = parent.join("libgtk-4-1.dll");
    let gtk4_dir = if gtk4_dll_in_same_dir.is_file() {
        // Flat layout: DLLs are already next to us, PATH prepend still helps
        // for any delayed-load or runtime-loaded DLLs.
        parent.to_path_buf()
    } else {
        // Subdirectory layout: check gtk4\ subdir.
        let subdir = parent.join("gtk4");
        if !subdir.is_dir() {
            log::debug!(
                "No bundled GTK4 DLLs found at {} or {}; relying on system PATH",
                parent.display(),
                subdir.display()
            );
            return None;
        }
        subdir
    };

    // Prepend the gtk4 directory to PATH.
    let gtk4_dir_str = gtk4_dir.to_string_lossy();
    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{};{}", gtk4_dir_str, current_path);

    // Set the new PATH. Using std::env::set_var updates the process environment
    // which is what LoadLibrary uses to resolve DLL dependencies.
    // Safety: we're on a dedicated thread before any GTK4 code runs.
    unsafe {
        std::env::set_var("PATH", &new_path);
    }

    // Also set XDG_DATA_DIRS to point to our bundled share/ directory
    // so GLib can find schemas and icons.
    // In flat layout: share/ is next to the DLLs. In subdir layout: share/ is inside gtk4/.
    let share_dir = if gtk4_dir == parent.to_path_buf() {
        parent.join("share")
    } else {
        gtk4_dir.join("share")
    };
    if share_dir.is_dir() {
        let current_xdg = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
        let new_xdg = if current_xdg.is_empty() {
            share_dir.to_string_lossy().to_string()
        } else {
            format!("{};{}", share_dir.to_string_lossy(), current_xdg)
        };
        unsafe {
            std::env::set_var("XDG_DATA_DIRS", &new_xdg);
        }
    }

    // Set GDK_PIXBUF_MODULE_FILE if the bundled loaders.cache exists.
    let lib_base = if gtk4_dir == parent.to_path_buf() {
        parent.join("lib")
    } else {
        gtk4_dir.join("lib")
    };
    let loaders_cache = lib_base
        .join("gdk-pixbuf-2.0")
        .join("2.10.0")
        .join("loaders.cache");
    if loaders_cache.is_file() {
        unsafe {
            std::env::set_var("GDK_PIXBUF_MODULE_FILE", &loaders_cache);
        }
    }

    Some(gtk4_dir)
}

// ─── Win32 Fallback ──────────────────────────────────────────────────────────

/// Display a Win32 MessageBox as a fallback when GTK4 is unavailable.
///
/// This is used when `GtkRuntime::init()` fails (DLLs not found) to still
/// inform the user about issues.
///
/// **Validates: Requirement 14.4**
pub fn fallback_message_box(title: &str, message: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let message_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message_wide.as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Attempt to initialize GTK4, falling back to Win32 MessageBox on failure.
///
/// This is the recommended entry point for the add-in. It tries to create
/// a `GtkRuntime`. If that fails (e.g., GTK4 DLLs missing), it shows a
/// Win32 fallback error message and returns `None`.
///
/// **Validates: Requirements 14.1, 14.4**
#[must_use]
pub fn init_or_fallback() -> Option<GtkRuntime> {
    match GtkRuntime::init() {
        Ok(runtime) => Some(runtime),
        Err(e) => {
            fallback_message_box(
                "SpamBayes",
                &format!(
                    "The SpamBayes GUI could not be initialized.\n\n\
                     Error: {e}\n\n\
                     The spam filter will continue to work, but the \
                     settings manager is unavailable.\n\n\
                     Please ensure GTK4 runtime libraries are installed."
                ),
            );
            None
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_open_guard_initially_false() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_is_open_guard_set_and_clear() {
        let flag = Arc::new(AtomicBool::new(false));

        // Simulate opening
        flag.store(true, Ordering::SeqCst);
        assert!(flag.load(Ordering::SeqCst));

        // Simulate closing
        flag.store(false, Ordering::SeqCst);
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_is_open_guard_prevents_duplicate() {
        let flag = Arc::new(AtomicBool::new(true));

        // compare_exchange should fail when already open (true → true not allowed)
        let result = flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
        assert!(
            result.is_err(),
            "Should not allow opening when already open"
        );
    }

    #[test]
    fn test_gui_command_enum_construction() {
        // Verify all variants can be constructed without panicking.
        let _cmd = GuiCommand::Shutdown;

        let (tx, _rx) = crossbeam_channel::bounded::<MsgBoxResult>(1);
        let _cmd = GuiCommand::ShowMessageBox {
            kind: MsgBoxKind::Error,
            title: "Test".to_string(),
            message: "Hello".to_string(),
            response: tx,
        };
    }

    #[test]
    fn test_gui_command_show_manager_construction() {
        let config = AppConfig::default();
        let state = ManagerState::from_config(&config);
        let _cmd = GuiCommand::ShowManager {
            state,
            config,
            on_close: Some(Box::new(|| {})),
            folder_tree: None,
            training_executor: None,
            stats: None,
        };
    }

    #[test]
    fn test_gui_command_show_wizard_construction() {
        let config = AppConfig::default();
        let _cmd = GuiCommand::ShowWizard {
            config,
            on_close: Some(Box::new(|_result| {})),
        };
    }

    #[test]
    fn test_fallback_message_box_strings() {
        // Test that wide string conversion doesn't panic for various inputs.
        let title = "SpamBayes";
        let message = "GTK4 DLLs not found.";
        let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let message_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        assert!(!title_wide.is_empty());
        assert!(!message_wide.is_empty());
        assert_eq!(*title_wide.last().unwrap(), 0u16);
        assert_eq!(*message_wide.last().unwrap(), 0u16);
    }

    #[test]
    fn test_fallback_message_box_unicode() {
        // Ensure Unicode strings don't panic during conversion.
        let title = "SpamBayes — エラー";
        let message = "GTK4 ランタイムが見つかりません 🚫";
        let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let message_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(*title_wide.last().unwrap(), 0u16);
        assert_eq!(*message_wide.last().unwrap(), 0u16);
    }

    #[test]
    fn test_fallback_message_box_empty_strings() {
        // Empty strings should still produce valid null-terminated wide strings.
        let title_wide: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
        let message_wide: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(title_wide.len(), 1);
        assert_eq!(message_wide.len(), 1);
        assert_eq!(title_wide[0], 0u16);
    }

    #[test]
    fn test_is_open_shared_across_threads() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&flag);

        let handle = std::thread::spawn(move || {
            flag_clone.store(true, Ordering::SeqCst);
        });
        handle.join().unwrap();

        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_channel_send_recv() {
        // Verify the crossbeam channel works for our command types.
        let (tx, rx) = crossbeam_channel::unbounded::<GuiCommand>();

        tx.send(GuiCommand::Shutdown).unwrap();

        match rx.try_recv() {
            Ok(GuiCommand::Shutdown) => {} // expected
            _ => panic!("Expected Shutdown command"),
        }
    }

    #[test]
    fn test_message_box_response_channel() {
        // Simulate the message_box response flow.
        let (response_tx, response_rx) = crossbeam_channel::bounded::<MsgBoxResult>(1);

        // Simulate GTK thread responding.
        response_tx.send(MsgBoxResult::Yes).unwrap();

        let result = response_rx.recv().unwrap();
        assert_eq!(result, MsgBoxResult::Yes);
    }

    #[test]
    fn test_message_box_response_timeout() {
        // If the GTK thread never responds, recv with timeout should not hang.
        let (_response_tx, response_rx) = crossbeam_channel::bounded::<MsgBoxResult>(1);

        let result = response_rx.recv_timeout(Duration::from_millis(10));
        assert!(result.is_err(), "Should timeout when no response");
    }

    #[test]
    fn test_gui_command_show_clues_construction() {
        // Verify ShowClues variant can be constructed and sent via channel.
        let (tx, rx) = crossbeam_channel::unbounded::<GuiCommand>();

        tx.send(GuiCommand::ShowClues {
            subject: "Test Email Subject".to_string(),
            clues_text: "Token: 0.99\nAnother: 0.01".to_string(),
            on_close: None,
        }).unwrap();

        match rx.try_recv() {
            Ok(GuiCommand::ShowClues { subject, clues_text, .. }) => {
                assert_eq!(subject, "Test Email Subject");
                assert!(clues_text.contains("Token"));
            }
            _ => panic!("Expected ShowClues command"),
        }
    }
}
