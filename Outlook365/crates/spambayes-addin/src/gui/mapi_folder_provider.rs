//! MAPI-backed folder provider for the folder browser dialog.
//!
//! Loads the complete Outlook folder hierarchy on construction (which must
//! happen on a COM-initialized thread), then returns the cached tree from
//! `load_folder_tree()`. This allows the GTK4 thread to access folder data
//! without needing direct MAPI access.
//!
//! **Validates: Requirement 11.1 (folder hierarchy display)**

use spambayes_mapi::session::MapiSession;
use spambayes_mapi::store::{FolderTreeNode, MessageStoreOps};

use super::folder_browser::{FolderNode, FolderProvider};

// ─── MapiFolderProvider ──────────────────────────────────────────────────────

/// A `FolderProvider` that loads the full Outlook folder hierarchy from MAPI.
///
/// Must be constructed on a COM-initialized thread (STA). After construction,
/// the cached tree can be accessed from any thread.
pub struct MapiFolderProvider {
    /// Pre-loaded folder tree (one entry per message store).
    tree: Result<Vec<FolderNode>, String>,
}

impl MapiFolderProvider {
    /// Load the folder hierarchy from Outlook via MAPI.
    ///
    /// This function:
    /// 1. Creates a MAPI session and logs on
    /// 2. Enumerates all message stores (mailboxes)
    /// 3. For each store, opens it and recursively walks the folder hierarchy
    /// 4. Converts everything into `FolderNode` trees
    ///
    /// # Requirements
    ///
    /// - Must be called from a COM-initialized thread (CoInitializeEx already called)
    /// - Outlook must be running or a MAPI profile must be available
    ///
    /// The provider will cache any errors and return them from `load_folder_tree()`.
    pub fn load() -> Self {
        let tree = Self::load_inner();
        Self { tree }
    }

    /// Create a provider from a pre-loaded tree (useful for testing or
    /// when the tree has already been built elsewhere).
    pub fn from_tree(tree: Vec<FolderNode>) -> Self {
        Self { tree: Ok(tree) }
    }

    /// Return the cached folder tree (consuming self).
    ///
    /// This is useful for extracting the tree to send across threads
    /// (e.g., in a `GuiCommand`).
    pub fn into_tree(self) -> Result<Vec<FolderNode>, String> {
        self.tree
    }

    fn load_inner() -> Result<Vec<FolderNode>, String> {
        // Initialize MAPI and log on
        let mut session = MapiSession::initialize_and_logon()
            .map_err(|e| format!("Failed to connect to Outlook: {e}"))?;

        // Enumerate all message stores
        let stores = session
            .enumerate_stores()
            .map_err(|e| format!("Failed to enumerate mailboxes: {e}"))?;

        if stores.is_empty() {
            return Err("No mailboxes found in Outlook profile.".to_string());
        }

        let mut root_nodes = Vec::new();

        for store_info in &stores {
            // Open the store
            let store_ptr = match session.open_store(&store_info.entry_id) {
                Ok(ptr) => ptr,
                Err(e) => {
                    log::warn!(
                        "Skipping store '{}': {e}",
                        store_info.display_name
                    );
                    continue;
                }
            };

            // Create MessageStoreOps to access the folder hierarchy
            let store_ops = unsafe {
                MessageStoreOps::new(store_ptr, store_info.entry_id.clone())
            };

            // Get the hierarchical folder tree for this store
            let children = match store_ops.get_folder_hierarchy() {
                Ok(nodes) => nodes.into_iter().map(Self::convert_node).collect(),
                Err(e) => {
                    log::warn!(
                        "Skipping store '{}': cannot read folders: {e}",
                        store_info.display_name
                    );
                    continue;
                }
            };

            let store_id_hex = hex_encode(&store_info.entry_id);

            // Get root folder entry ID for the store node
            let root_entry_id_hex = match store_ops.get_root_folder() {
                Ok(root) => hex_encode(&root.entry_id),
                Err(_) => String::new(),
            };

            // The top-level node represents the store itself
            let store_node = FolderNode {
                name: store_info.display_name.clone(),
                store_id: store_id_hex,
                entry_id: root_entry_id_hex,
                children,
            };

            root_nodes.push(store_node);
        }

        if root_nodes.is_empty() {
            return Err("Could not open any mailboxes.".to_string());
        }

        Ok(root_nodes)
    }

    /// Convert a `FolderTreeNode` (from the mapi crate) into a `FolderNode`
    /// (for the GUI folder browser).
    fn convert_node(node: FolderTreeNode) -> FolderNode {
        FolderNode {
            name: node.name,
            store_id: node.store_id_hex,
            entry_id: node.entry_id_hex,
            children: node.children.into_iter().map(Self::convert_node).collect(),
        }
    }
}

impl FolderProvider for MapiFolderProvider {
    fn load_folder_tree(&self) -> Result<Vec<FolderNode>, String> {
        self.tree.clone()
    }
}

// ─── Hex Encoding Helper ─────────────────────────────────────────────────────

/// Encode bytes to a lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
