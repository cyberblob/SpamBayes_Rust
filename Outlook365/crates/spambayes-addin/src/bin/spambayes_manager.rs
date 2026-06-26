//! SpamBayes Manager — GTK4 native launcher.
//!
//! Standalone executable that opens the SpamBayes Manager window using
//! the GTK4 runtime. This can be launched independently of Outlook for
//! configuration management.
//!
//! **Validates: Requirements 14.1, 14.2, 14.3**

#![windows_subsystem = "windows"]

use std::path::PathBuf;

use spambayes_config::AppConfig;

fn get_data_directory() -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let path = PathBuf::from(local_app_data).join("SpamBayes");
        if path.is_dir() || std::fs::create_dir_all(&path).is_ok() {
            return path;
        }
    }
    std::env::temp_dir()
}

fn main() {
    // Initialize COM apartment (required for MAPI in a standalone process).
    // Must happen before any MAPI or COM calls.
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        ).ok();
    }

    // Load configuration from %LOCALAPPDATA%\SpamBayes\default.ini
    let data_dir = get_data_directory();
    let profile_name = "default";

    let config = match AppConfig::load(&data_dir, profile_name) {
        Ok(c) => c,
        Err(_) => AppConfig::default(),
    };

    // Load folder tree from MAPI on a background thread while GTK initializes.
    // This runs concurrently so it doesn't delay the window appearing.
    let folder_tree_handle = std::thread::spawn(|| {
        use spambayes_addin::gui::mapi_folder_provider::MapiFolderProvider;
        use spambayes_addin::gui::folder_browser::FolderProvider;

        // COM must be initialized on this thread for MAPI access.
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            ).ok();
        }

        let provider = MapiFolderProvider::load();
        let tree = provider.load_folder_tree().ok();

        unsafe { windows::Win32::System::Com::CoUninitialize(); }
        tree
    });

    // Initialize GTK4 runtime (runs concurrently with MAPI load above).
    let runtime = match spambayes_addin::gui::gtk_runtime::GtkRuntime::init() {
        Ok(rt) => rt,
        Err(e) => {
            spambayes_addin::gui::gtk_runtime::fallback_message_box(
                "SpamBayes Manager",
                &format!(
                    "Failed to initialize GTK4 runtime:\n{e}\n\n\
                     Please ensure GTK4 libraries are installed."
                ),
            );
            return;
        }
    };

    // Build manager state from config.
    let state = spambayes_addin::manager_dlg::ManagerState::from_config(&config);

    // Wait for the folder tree to finish loading (should be done by now
    // since GTK init also takes time).
    let folder_tree = folder_tree_handle.join().unwrap_or(None);

    // Show manager or wizard if first-run.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    runtime.show_manager_or_wizard_if_first_run(
        state,
        config,
        &data_dir,
        profile_name,
        folder_tree,
        move || {
            let _ = done_tx.send(());
        },
    );

    // Wait for the manager window to close before exiting.
    let _ = done_rx.recv();

    // Shut down GTK4 runtime gracefully.
    runtime.shutdown();

    // Uninitialize COM apartment.
    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }
}
