//! SpamBayes Manager — GTK4 native launcher.
//!
//! Standalone executable that opens the SpamBayes Manager window using
//! the GTK4 runtime. This can be launched independently of Outlook for
//! configuration management.
//!
//! **Validates: Requirements 14.1, 14.2, 14.3**

#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

/// Load the classifier database and create a training executor bridge.
///
/// Returns `None` if the classifier cannot be loaded (training will be
/// unavailable but the manager can still open for configuration).
/// On success, returns the executor and (nham, nspam) counts.
fn create_training_executor(
    data_dir: &PathBuf,
) -> Option<(Arc<dyn spambayes_addin::gui::tabs::TrainingExecutor>, u64, u64)> {
    use spambayes_core::classifier::Classifier;
    use spambayes_storage::{MmapDbmBackend, MmapMessageDb, StorageBackend, MessageDatabase};
    use spambayes_addin::training_bridge::TrainingExecutorBridge;
    use spambayes_addin::logger::Logger;

    let db_path = data_dir.join("spambayes.db");
    let msg_db_path = data_dir.join("spambayes_msg.db");

    // Load or create the classifier database.
    let (classifier, storage): (
        Arc<Mutex<Classifier>>,
        Arc<Mutex<Box<dyn StorageBackend>>>,
    ) = if db_path.exists() {
        let mut backend = MmapDbmBackend::new(&db_path);
        match backend.load() {
            Ok(state) => {
                let config = spambayes_core::ClassifierConfig::default();
                let word_data = backend.data().clone();
                let classifier = Classifier::from_state(
                    config,
                    state.nspam,
                    state.nham,
                    word_data,
                );
                (
                    Arc::new(Mutex::new(classifier)),
                    Arc::new(Mutex::new(Box::new(backend) as Box<dyn StorageBackend>)),
                )
            }
            Err(e) => {
                log::error!("Failed to load classifier database: {e}");
                // Fall through to empty classifier
                let config = spambayes_core::ClassifierConfig::default();
                let classifier = Classifier::new(config);
                let backend = MmapDbmBackend::new(&db_path);
                (
                    Arc::new(Mutex::new(classifier)),
                    Arc::new(Mutex::new(Box::new(backend) as Box<dyn StorageBackend>)),
                )
            }
        }
    } else {
        // Try migration from Python database.
        if let Some((state, tokens)) = spambayes_storage::try_migrate_classifier(data_dir) {
            let config = spambayes_core::ClassifierConfig::default();
            let classifier = Classifier::from_state(config, state.nspam, state.nham, tokens);
            let backend = MmapDbmBackend::new(&db_path);
            (
                Arc::new(Mutex::new(classifier)),
                Arc::new(Mutex::new(Box::new(backend) as Box<dyn StorageBackend>)),
            )
        } else {
            // No database found — initialize empty.
            let config = spambayes_core::ClassifierConfig::default();
            let classifier = Classifier::new(config);
            let backend = MmapDbmBackend::new(&db_path);
            (
                Arc::new(Mutex::new(classifier)),
                Arc::new(Mutex::new(Box::new(backend) as Box<dyn StorageBackend>)),
            )
        }
    };

    // Create message database.
    let message_db: Arc<Mutex<Box<dyn MessageDatabase>>> =
        Arc::new(Mutex::new(Box::new(MmapMessageDb::new(msg_db_path))));

    // Create logger.
    let log_path = data_dir.join("spambayes.log");
    let logger = Logger::new(&log_path, spambayes_addin::LogLevel::Info)
        .or_else(|_| Logger::new(&Logger::default_path(), spambayes_addin::LogLevel::Info))
        .ok()?;
    let logger = Arc::new(logger);

    // Read classifier counts before creating the bridge.
    let (nham, nspam) = {
        let c = classifier.lock().ok()?;
        (c.nham(), c.nspam())
    };

    // Create the training executor bridge.
    let bridge = TrainingExecutorBridge::new(
        classifier,
        storage,
        message_db,
        logger,
        None, // No statistics in standalone mode
    );

    Some((Arc::new(bridge), nham, nspam))
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

    // Show a lightweight splash window immediately while heavy initialization
    // (GTK4 DLL loading, classifier DB, MAPI folder tree) proceeds.
    let splash = spambayes_addin::splash_window::show();

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

    // Create the training executor (loads classifier database).
    let training_executor = create_training_executor(&data_dir);

    // Initialize GTK4 runtime (runs concurrently with MAPI load above).
    let runtime = match spambayes_addin::gui::gtk_runtime::GtkRuntime::init() {
        Ok(rt) => rt,
        Err(e) => {
            // Close splash before showing error dialog.
            if let Some(ref s) = splash {
                s.close();
            }
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

    // Show manager (with training executor) or wizard if first-run.
    // Close the splash now — all heavy loading is done and the GTK4 window
    // will appear immediately.
    if let Some(ref s) = splash {
        s.close();
    }
    drop(splash);

    let (done_tx, done_rx) = std::sync::mpsc::channel();

    if let Some((executor, nham, nspam)) = training_executor {
        // Create a StatisticsManager to load lifetime classification stats
        // from the persistent stats file (spambayes_stats.json).
        let stats_mgr = spambayes_addin::statistics::StatisticsManager::new(&data_dir, 10);
        let mut stats = spambayes_addin::manager_dlg::ManagerStats::from_statistics(&stats_mgr);
        // Populate manual classification counts from the classifier DB
        // (nham = messages trained as good, nspam = messages trained as spam).
        stats.manually_classified_good = nham;
        stats.manually_classified_spam = nspam;
        stats.ham_trained = nham;
        stats.spam_trained = nspam;
        runtime.show_manager_or_wizard_with_training(
            state,
            config,
            &data_dir,
            profile_name,
            folder_tree,
            executor,
            Some(stats),
            move || {
                let _ = done_tx.send(());
            },
        );
    } else {
        // Training unavailable — show manager without training support.
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
    }

    // Wait for the manager window to close before exiting.
    let _ = done_rx.recv();

    // Shut down GTK4 runtime gracefully.
    runtime.shutdown();

    // Uninitialize COM apartment.
    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }
}
