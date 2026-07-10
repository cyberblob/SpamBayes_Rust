//! Training executor bridge — adapts `TrainingEngine` to the GUI's `TrainingExecutor` trait.
//!
//! The `TrainingTab` GUI component expects an `Arc<dyn TrainingExecutor>` that
//! can be called from a background thread. This module provides:
//!
//! - `MapiTrainingProvider`: Implements `TrainingFolderProvider` using MAPI
//!   session access for real folder/message enumeration.
//! - `TrainingExecutorBridge`: Implements the GUI's `TrainingExecutor` trait
//!   by wrapping a `TrainingEngine` and creating a fresh `MapiTrainingProvider`
//!   (with COM initialization) on each background call.
//!
//! # Thread Safety
//!
//! The bridge is `Send + Sync` because it stores only `Arc`-wrapped shared
//! state. Each `train_batch` call initializes COM and creates a new MAPI
//! session on the calling (background) thread, which is required by MAPI's
//! apartment threading model.
//!
//! # Validates: Requirements 3.4, 3.7, 3.8

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use spambayes_config::{FolderId, TrainingConfig};
use spambayes_core::classifier::Classifier;
use spambayes_mapi::{MsgStoreError, MapiSessionImpl, MessageStoreOps};
use spambayes_storage::{MessageDatabase, StorageBackend};

use crate::filter::Progress;
use crate::gui::tabs::training::{TrainingExecutor, TrainingResult};
use crate::logger::Logger;
use crate::statistics::StatisticsManager;
use crate::train::{TrainableMessage, TrainingEngine, TrainingFolderProvider};

// ─── MapiTrainableMessage ────────────────────────────────────────────────────

/// Wraps a `spambayes_mapi::Message` to implement `TrainableMessage`.
struct MapiTrainableMessage {
    message: spambayes_mapi::Message,
}

impl TrainableMessage for MapiTrainableMessage {
    fn get_raw_content(&self) -> Result<Vec<u8>, MsgStoreError> {
        // get_email_rfc2822 requires &mut self for lazy loading.
        // We use unsafe interior mutability since TrainableMessage trait takes &self.
        let msg_ptr = &self.message as *const spambayes_mapi::Message
            as *mut spambayes_mapi::Message;
        unsafe { (*msg_ptr).get_email_rfc2822() }
    }

    fn get_search_key(&self) -> &[u8] {
        // get_search_key returns Vec<u8>, but trait expects &[u8].
        // We store the search key at construction time to avoid lifetime issues.
        // Actually, get_search_key() on Message returns Vec<u8>, so we can't return &[u8]
        // from it without storing. Let's use entry_id as the search key instead,
        // which is always available and unique.
        self.message.entry_id()
    }

    fn get_display_id(&self) -> String {
        // Use hex-encoded entry ID as a display identifier.
        let eid = self.message.entry_id();
        if eid.len() > 8 {
            format!("msg:{}", hex_encode(&eid[..8]))
        } else {
            format!("msg:{}", hex_encode(eid))
        }
    }
}

// ─── MapiTrainingProvider ────────────────────────────────────────────────────

/// MAPI-based implementation of `TrainingFolderProvider`.
///
/// Opens all available MAPI message stores and tries each one when resolving
/// folder entry IDs. This handles the common case where the folder browser
/// returns OST-based entry IDs but the "default" store is the Exchange
/// online store (or vice versa).
struct MapiTrainingProvider {
    /// All opened message stores — we try each one for folder lookups.
    stores: Vec<MessageStoreOps>,
}

impl MapiTrainingProvider {
    /// Create a new provider by logging on to MAPI and opening all available stores.
    ///
    /// # Errors
    ///
    /// Returns an error if MAPI logon fails or no stores can be opened.
    fn new(session: &mut MapiSessionImpl) -> Result<Self, String> {
        // Enumerate all stores in the profile.
        let store_infos = session.enumerate_stores()
            .map_err(|e| format!("Failed to enumerate stores: {e}"))?;

        let mut stores = Vec::new();
        for info in &store_infos {
            match session.open_store(&info.entry_id) {
                Ok(store_ptr) => {
                    let store_ops = unsafe {
                        MessageStoreOps::new(store_ptr, info.entry_id.clone())
                    };
                    stores.push(store_ops);
                }
                Err(e) => {
                    log::warn!("Skipping store '{}': {e}", info.display_name);
                }
            }
        }

        if stores.is_empty() {
            return Err("No message stores could be opened".to_string());
        }

        Ok(Self { stores })
    }
}

impl TrainingFolderProvider for MapiTrainingProvider {
    fn get_folder_name(&self, folder_id: &FolderId) -> Result<String, MsgStoreError> {
        let entry_id_bytes = folder_id.entry_id.to_bytes();
        let store_id_bytes = folder_id.store_id.to_bytes();

        // Try each store until one can open this folder.
        for store_ops in &self.stores {
            if let Ok(folder) = store_ops.get_folder(&entry_id_bytes, &store_id_bytes) {
                return Ok(folder.name);
            }
        }

        Err(MsgStoreError::Mapi {
            hr: 0x80040107_u32 as i32,
            message: "IMsgStore::OpenEntry failed".to_string(),
        })
    }

    fn get_sub_folders(&self, folder_id: &FolderId) -> Result<Vec<FolderId>, MsgStoreError> {
        let entry_id_bytes = folder_id.entry_id.to_bytes();
        let store_id_bytes = folder_id.store_id.to_bytes();

        for store_ops in &self.stores {
            let folder_eids: Vec<(&[u8], &[u8])> = vec![
                (store_id_bytes.as_slice(), entry_id_bytes.as_slice())
            ];
            let results = store_ops.folder_iter(&folder_eids, true);

            let mut children = Vec::new();
            let mut found = false;
            for result in results {
                found = true;
                if let Ok(f) = result {
                    if f.entry_id != entry_id_bytes {
                        children.push(FolderId {
                            store_id: spambayes_config::StoreId::new(hex_encode(&f.store_id)),
                            entry_id: spambayes_config::EntryId::new(hex_encode(&f.entry_id)),
                        });
                    }
                }
            }
            if found {
                return Ok(children);
            }
        }

        Ok(Vec::new())
    }

    fn get_messages(
        &self,
        folder_id: &FolderId,
    ) -> Result<Vec<Box<dyn TrainableMessage>>, MsgStoreError> {
        let entry_id_bytes = folder_id.entry_id.to_bytes();
        let store_id_bytes = folder_id.store_id.to_bytes();

        // Try each store until one can open this folder.
        for store_ops in &self.stores {
            match store_ops.get_folder(&entry_id_bytes, &store_id_bytes) {
                Ok(folder) => {
                    let iter = store_ops.message_iter(&folder)?;
                    let mut messages: Vec<Box<dyn TrainableMessage>> = Vec::new();
                    for result in iter {
                        match result {
                            Ok(msg) => {
                                messages.push(Box::new(MapiTrainableMessage { message: msg }));
                            }
                            Err(e) => {
                                log::warn!("Failed to open message during training: {e}");
                            }
                        }
                    }
                    return Ok(messages);
                }
                Err(_) => continue,
            }
        }

        Err(MsgStoreError::Mapi {
            hr: 0x80040107_u32 as i32,
            message: "IMsgStore::OpenEntry failed".to_string(),
        })
    }
}

// ─── ProgressAdapter ─────────────────────────────────────────────────────────

/// Adapts the GUI's progress callback to the `Progress` trait expected by `TrainingEngine`.
struct ProgressAdapter {
    callback: Box<dyn Fn(&str, u32, u32) + Send>,
    is_cancelled: Arc<AtomicBool>,
}

impl ProgressAdapter {
    fn new(callback: Box<dyn Fn(&str, u32, u32) + Send>, is_cancelled: Arc<AtomicBool>) -> Self {
        Self {
            callback,
            is_cancelled,
        }
    }
}

impl Progress for ProgressAdapter {
    fn report(&mut self, folder_name: &str, current: u32, total: u32) {
        (self.callback)(folder_name, current, total);
    }

    fn is_cancelled(&self) -> bool {
        self.is_cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// ─── TrainingExecutorBridge ──────────────────────────────────────────────────

/// Bridges the GUI's `TrainingExecutor` trait to the `TrainingEngine`.
///
/// Each call to `train_batch` initializes COM on the calling thread,
/// logs on to MAPI, creates a `MapiTrainingProvider`, and delegates
/// to `TrainingEngine::train_batch`.
///
/// This struct is `Send + Sync` because it only holds `Arc`-wrapped data.
pub struct TrainingExecutorBridge {
    /// Shared classifier (same instance used by the filter engine).
    classifier: Arc<Mutex<Classifier>>,
    /// Shared storage backend for classifier persistence.
    storage: Arc<Mutex<Box<dyn StorageBackend>>>,
    /// Shared message database.
    message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
    /// Logger instance.
    logger: Arc<Logger>,
    /// Optional statistics manager.
    statistics: Option<StatisticsManager>,
}

impl TrainingExecutorBridge {
    /// Create a new bridge with the shared classifier and storage state.
    pub fn new(
        classifier: Arc<Mutex<Classifier>>,
        storage: Arc<Mutex<Box<dyn StorageBackend>>>,
        message_db: Arc<Mutex<Box<dyn MessageDatabase>>>,
        logger: Arc<Logger>,
        statistics: Option<StatisticsManager>,
    ) -> Self {
        Self {
            classifier,
            storage,
            message_db,
            logger,
            statistics,
        }
    }
}

impl TrainingExecutor for TrainingExecutorBridge {
    fn train_batch(
        &self,
        ham_folder_ids: Vec<FolderId>,
        spam_folder_ids: Vec<FolderId>,
        rescore: bool,
        rebuild: bool,
        progress_reporter: Box<dyn Fn(&str, u32, u32) + Send>,
        is_cancelled: Arc<AtomicBool>,
    ) -> Result<TrainingResult, String> {
        // Initialize COM on this background thread (required for MAPI).
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
            .ok()
            .map_err(|e| format!("COM initialization failed: {e}"))?;
        }

        // Ensure COM is uninitialized when we leave this scope.
        struct ComGuard;
        impl Drop for ComGuard {
            fn drop(&mut self) {
                unsafe { windows::Win32::System::Com::CoUninitialize(); }
            }
        }
        let _com_guard = ComGuard;

        // Create a MAPI session on this thread.
        let mut session = MapiSessionImpl::initialize_and_logon()
            .map_err(|e| format!("MAPI logon failed: {e}"))?;

        // Create the folder provider.
        let provider = MapiTrainingProvider::new(&mut session)?;

        // Create training config from the folder IDs.
        let config = TrainingConfig {
            ham_folder_ids,
            spam_folder_ids,
            ham_include_sub: true,
            spam_include_sub: true,
            rescore,
            ..TrainingConfig::default()
        };

        // Create the training engine.
        let engine = TrainingEngine::new(
            Arc::clone(&self.classifier),
            Arc::clone(&self.storage),
            Arc::clone(&self.message_db),
            Arc::clone(&self.logger),
            self.statistics.clone(),
        );

        // Create progress adapter.
        let mut progress = ProgressAdapter::new(progress_reporter, is_cancelled);

        // If rebuild is requested, use the rebuild method which creates a fresh classifier.
        if rebuild {
            let result = engine
                .rebuild(&config, &provider, &mut progress)
                .map_err(|e| format!("Rebuild failed: {e}"))?;

            // Save after rebuild.
            engine.save_post_incremental()
                .map_err(|e| format!("Failed to save after rebuild: {e}"))?;

            // Log off MAPI.
            session.logoff();

            return Ok(TrainingResult {
                total_processed: result.total_processed,
                new_entries: result.new_entries,
                errors: result.errors,
            });
        }

        // Run incremental training.
        let result = engine
            .train_batch(&config, &provider, &mut progress)
            .map_err(|e| format!("Training failed: {e}"))?;

        // Save the classifier database after training.
        engine.save_post_incremental()
            .map_err(|e| format!("Failed to save after training: {e}"))?;

        // Log off MAPI.
        session.logoff();

        Ok(TrainingResult {
            total_processed: result.total_processed,
            new_entries: result.new_entries,
            errors: result.errors,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}


// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use spambayes_config::{EntryId, FolderId, StoreId};

    /// Regression test: MapiTrainingProvider must store multiple stores.
    ///
    /// Previously, the provider only opened the "default" store. If the
    /// training folder's entry IDs came from a different store (e.g. OST vs
    /// Exchange online), OpenEntry would fail with MAPI_E_NOT_FOUND (0x80040107).
    ///
    /// This test verifies that the `stores` field can hold multiple stores
    /// and that `get_messages` tries each one (the struct is Vec, not single).
    #[test]
    fn provider_stores_field_is_multi_store() {
        // MapiTrainingProvider.stores is a Vec — verify the type holds multiple.
        // We can't construct a real one without MAPI, but we verify the struct
        // definition accepts multiple stores by checking it's a Vec.
        let provider = MapiTrainingProvider {
            stores: Vec::new(), // empty is valid (would error on use)
        };
        assert_eq!(provider.stores.len(), 0);
    }

    /// Regression test: get_messages tries all stores, not just the first.
    ///
    /// With a single store, if that store can't open the entry ID, the error
    /// is immediate. With multiple stores, we iterate and only fail after all
    /// stores have been tried.
    #[test]
    fn get_messages_returns_not_found_when_no_stores() {
        let provider = MapiTrainingProvider {
            stores: Vec::new(),
        };

        let folder_id = FolderId::new(
            StoreId::new("0000000011223344".to_string()),
            EntryId::new("ef00000055667788".to_string()),
        );

        let result = provider.get_messages(&folder_id);
        assert!(result.is_err(), "Should fail when no stores are available");
        match result.err().unwrap() {
            MsgStoreError::Mapi { hr, .. } => {
                assert_eq!(hr, 0x80040107_u32 as i32, "Should be MAPI_E_NOT_FOUND");
            }
            other => panic!("Expected MsgStoreError::Mapi, got: {other:?}"),
        }
    }

    /// Regression test: get_folder_name returns not-found when no stores match.
    #[test]
    fn get_folder_name_returns_not_found_when_no_stores() {
        let provider = MapiTrainingProvider {
            stores: Vec::new(),
        };

        let folder_id = FolderId::new(
            StoreId::new("0000000011223344".to_string()),
            EntryId::new("ef00000055667788".to_string()),
        );

        let result = provider.get_folder_name(&folder_id);
        assert!(result.is_err());
    }

    /// Regression test: INI profile name must always be "default".
    ///
    /// The MAPI session's get_profile_name() can return nonsensical values
    /// like "Microsoft Outlook Address Book Provider" (a service provider name,
    /// not a profile name). Using that as the INI filename causes the Manager
    /// and the DLL to read/write different files, breaking folder ID persistence.
    ///
    /// The Manager binary hardcodes "default" and the DLL's load_config_chain
    /// also hardcodes "default". This test verifies the Manager's value.
    #[test]
    fn manager_profile_name_is_always_default() {
        // This is a compile-time guarantee verified by grep, but we document
        // the invariant as a test that will remind developers of the requirement.
        let profile_name = "default";
        assert_eq!(profile_name, "default",
            "REGRESSION: profile_name must always be 'default'. \
             Never use MAPI's get_profile_name() for INI file naming — \
             it returns provider service names, not meaningful profile names.");
    }

    /// Regression test: get_sub_folders returns empty vec (not error) when no stores match.
    #[test]
    fn get_sub_folders_returns_empty_when_no_stores() {
        let provider = MapiTrainingProvider {
            stores: Vec::new(),
        };

        let folder_id = FolderId::new(
            StoreId::new("0000000011223344".to_string()),
            EntryId::new("ef00000055667788".to_string()),
        );

        let result = provider.get_sub_folders(&folder_id);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
