//! Folder Browser dialog — browse Outlook's MAPI folder hierarchy.
//!
//! This is a 1-to-1 replacement of `FolderBrowserDialog` and the inline
//! `browse_folder` method from the tkinter code. Uses a GTK4 `TreeView`
//! with `TreeStore` to display the folder tree.
//!
//! The dialog uses a `FolderProvider` trait to decouple folder data loading
//! from the GTK4 UI. This allows MAPI operations to be performed on a
//! COM-initialized background thread before showing the dialog, and also
//! enables testing with mock data.
//!
//! **Validates: Requirements 11.1, 11.2, 11.3, 11.4, 11.5, 11.6**

// TreeView/TreeStore are deprecated since GTK 4.10 in favor of ColumnView,
// but we use them intentionally per the design spec for folder hierarchy display.
#![allow(deprecated)]

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use spambayes_config::{EntryId, FolderId, StoreId};

use super::message_boxes;

// ─── Selection Mode ──────────────────────────────────────────────────────────

/// Folder selection mode for the browser dialog.
///
/// **Validates: Requirement 11.2**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// Single folder selection (for spam/unsure/good folder).
    Single,
    /// Multiple folder selection (for watch/training folders).
    Multi,
}

// ─── Folder Tree Data ────────────────────────────────────────────────────────

/// A node in the folder hierarchy tree.
///
/// Represents either a message store (top-level) or a folder within a store.
/// Children are nested sub-folders.
#[derive(Debug, Clone)]
pub struct FolderNode {
    /// Display name (e.g., "Inbox", "Personal Folders").
    pub name: String,
    /// Hex-encoded store entry ID.
    pub store_id: String,
    /// Hex-encoded folder entry ID.
    pub entry_id: String,
    /// Child folders.
    pub children: Vec<FolderNode>,
}

// ─── Folder Provider Trait ───────────────────────────────────────────────────

/// Trait abstracting folder hierarchy retrieval.
///
/// The real implementation loads from MAPI on a COM-initialized thread.
/// Test implementations can provide static data.
pub trait FolderProvider {
    /// Load the folder tree for all available message stores.
    ///
    /// Returns a list of top-level nodes (one per store), each containing
    /// the store's folder hierarchy. Returns an error string on failure.
    fn load_folder_tree(&self) -> Result<Vec<FolderNode>, String>;
}

// ─── NullFolderProvider ──────────────────────────────────────────────────────

/// A no-op folder provider that returns an empty folder tree.
///
/// Used as a placeholder until real MAPI wiring is connected.
/// The folder browser will display an empty tree when this provider is used.
pub struct NullFolderProvider;

impl FolderProvider for NullFolderProvider {
    fn load_folder_tree(&self) -> Result<Vec<FolderNode>, String> {
        Ok(Vec::new())
    }
}

// ─── TreeStore Column Indices ────────────────────────────────────────────────

/// Column 0: Folder display name (String).
const COL_NAME: u32 = 0;
/// Column 1: Store ID hex string (String).
const COL_STORE_ID: u32 = 1;
/// Column 2: Entry ID hex string (String).
const COL_ENTRY_ID: u32 = 2;

// ─── FolderBrowserDialog ─────────────────────────────────────────────────────

/// Inner state shared via `Rc` for closure access.
struct FolderBrowserInner {
    dialog: gtk4::Window,
    tree_view: gtk4::TreeView,
    #[allow(dead_code)]
    tree_store: gtk4::TreeStore,
    selection_mode: SelectionMode,
    /// Dialog result: Some(selections) on OK, None on Cancel.
    result: RefCell<Option<Option<Vec<(FolderId, String)>>>>,
}

/// The Folder Browser dialog.
///
/// Displays a tree of all Outlook message stores and their folder
/// hierarchies. Supports single-select and multi-select modes.
///
/// **Validates: Requirements 11.1, 11.2, 11.3, 11.4, 11.5, 11.6**
pub struct FolderBrowserDialog {
    inner: Rc<FolderBrowserInner>,
}

impl FolderBrowserDialog {
    /// Create a new folder browser dialog.
    ///
    /// Loads folder data from the provider and builds the TreeView.
    /// If loading fails, shows an error message and the dialog will
    /// return `None` immediately when `run()` is called.
    ///
    /// # Arguments
    /// * `parent` - Parent window for modality
    /// * `provider` - Folder data source (MAPI or mock)
    /// * `mode` - Single or multi-select
    /// * `preselected` - Folder IDs to pre-select when opened
    ///
    /// **Validates: Requirements 11.1, 11.2, 11.3, 11.6**
    pub fn new(
        parent: Option<&gtk4::Window>,
        provider: &dyn FolderProvider,
        mode: SelectionMode,
        preselected: &[FolderId],
    ) -> Self {
        // Load folder data from the provider
        let folder_tree = match provider.load_folder_tree() {
            Ok(tree) => tree,
            Err(err) => {
                // Requirement 11.6: Show error and close gracefully
                message_boxes::report_error(
                    parent,
                    "SpamBayes",
                    &format!("Failed to load folder list:\n\n{err}"),
                );
                // Return a dialog that will immediately yield None
                return Self::new_error_state(parent, mode);
            }
        };

        // Build the dialog window
        let title = match mode {
            SelectionMode::Single => "Select Folder",
            SelectionMode::Multi => "Select Folders",
        };

        let dialog = gtk4::Window::builder()
            .title(title)
            .default_width(400)
            .default_height(500)
            .modal(true)
            .resizable(true)
            .build();

        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        // Create the TreeStore with 3 string columns: name, store_id, entry_id
        let tree_store = gtk4::TreeStore::new(&[
            glib::Type::STRING, // COL_NAME
            glib::Type::STRING, // COL_STORE_ID
            glib::Type::STRING, // COL_ENTRY_ID
        ]);

        // Populate the tree store from the loaded data
        Self::populate_tree_store(&tree_store, &folder_tree);

        // Create the TreeView
        let tree_view = gtk4::TreeView::builder()
            .model(&tree_store)
            .headers_visible(false)
            .enable_search(true)
            .build();

        // Add a text column for the folder name
        let renderer = gtk4::CellRendererText::new();
        let column = gtk4::TreeViewColumn::new();
        column.pack_start(&renderer, true);
        column.add_attribute(&renderer, "text", COL_NAME as i32);
        tree_view.append_column(&column);

        // Configure selection mode
        let selection = tree_view.selection();
        match mode {
            SelectionMode::Single => {
                selection.set_mode(gtk4::SelectionMode::Single);
            }
            SelectionMode::Multi => {
                selection.set_mode(gtk4::SelectionMode::Multiple);
            }
        }

        // Pre-select configured folders (Requirement 11.3)
        if !preselected.is_empty() {
            Self::preselect_folders(&tree_store, &tree_view, preselected);
        }

        // Expand all nodes by default for visibility
        tree_view.expand_all();

        // Layout: TreeView in a ScrolledWindow + button bar
        let scrolled = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Automatic)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .build();
        scrolled.set_child(Some(&tree_view));

        // Button bar
        let ok_btn = gtk4::Button::builder()
            .label("OK")
            .width_request(80)
            .build();
        let cancel_btn = gtk4::Button::builder()
            .label("Cancel")
            .width_request(80)
            .build();

        let button_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk4::Align::End)
            .margin_top(8)
            .build();
        button_box.append(&ok_btn);
        button_box.append(&cancel_btn);

        // Main container
        let vbox = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        vbox.append(&scrolled);
        vbox.append(&button_box);

        dialog.set_child(Some(&vbox));

        let inner = Rc::new(FolderBrowserInner {
            dialog,
            tree_view,
            tree_store,
            selection_mode: mode,
            result: RefCell::new(None),
        });

        // Connect OK button
        let inner_ok = Rc::clone(&inner);
        ok_btn.connect_clicked(move |_| {
            let selections = Self::read_selection(&inner_ok);
            if selections.is_empty() {
                // No selection — treat as cancel for single-select,
                // allow empty for multi-select
                if inner_ok.selection_mode == SelectionMode::Single {
                    // Requirement 11.5: no selection → None
                    *inner_ok.result.borrow_mut() = Some(None);
                } else {
                    *inner_ok.result.borrow_mut() = Some(Some(selections));
                }
            } else {
                *inner_ok.result.borrow_mut() = Some(Some(selections));
            }
            inner_ok.dialog.close();
        });

        // Connect Cancel button
        let inner_cancel = Rc::clone(&inner);
        cancel_btn.connect_clicked(move |_| {
            // Requirement 11.5: Cancel → None
            *inner_cancel.result.borrow_mut() = Some(None);
            inner_cancel.dialog.close();
        });

        // Handle window close (X button / Escape) as Cancel
        let inner_close = Rc::clone(&inner);
        inner.dialog.connect_close_request(move |_| {
            if inner_close.result.borrow().is_none() {
                *inner_close.result.borrow_mut() = Some(None);
            }
            glib::Propagation::Proceed
        });

        // Allow double-click to select in single-select mode
        if mode == SelectionMode::Single {
            let inner_dbl = Rc::clone(&inner);
            inner.tree_view.connect_row_activated(move |_, _, _| {
                let selections = Self::read_selection(&inner_dbl);
                if !selections.is_empty() {
                    *inner_dbl.result.borrow_mut() = Some(Some(selections));
                    inner_dbl.dialog.close();
                }
            });
        }

        Self { inner }
    }

    /// Show the dialog and block until closed.
    ///
    /// Returns the selected folders on OK, or `None` on Cancel / no selection.
    ///
    /// **Validates: Requirements 11.4, 11.5**
    pub fn run(&self) -> Option<Vec<(FolderId, String)>> {
        // If the dialog was created in error state, return None immediately
        if self.inner.result.borrow().is_some() {
            return self.inner.result.borrow().clone().unwrap_or(None);
        }

        // Present the dialog
        self.inner.dialog.present();

        // Spin the GLib main context until the dialog is closed
        let context = glib::MainContext::default();
        while self.inner.result.borrow().is_none() {
            context.iteration(true);
        }

        self.inner.result.borrow().clone().unwrap_or(None)
    }

    // ─── Internal Helpers ────────────────────────────────────────────────

    /// Create an error-state dialog that returns None immediately.
    fn new_error_state(parent: Option<&gtk4::Window>, mode: SelectionMode) -> Self {
        let dialog = gtk4::Window::builder()
            .title("Select Folder")
            .modal(true)
            .build();

        if let Some(parent_win) = parent {
            dialog.set_transient_for(Some(parent_win));
        }

        let tree_store = gtk4::TreeStore::new(&[
            glib::Type::STRING,
            glib::Type::STRING,
            glib::Type::STRING,
        ]);

        let tree_view = gtk4::TreeView::builder()
            .model(&tree_store)
            .build();

        let inner = Rc::new(FolderBrowserInner {
            dialog,
            tree_view,
            tree_store,
            selection_mode: mode,
            result: RefCell::new(Some(None)), // Pre-set to None (error state)
        });

        Self { inner }
    }

    /// Populate the `TreeStore` from the folder tree data.
    ///
    /// **Validates: Requirement 11.1**
    fn populate_tree_store(store: &gtk4::TreeStore, nodes: &[FolderNode]) {
        for node in nodes {
            Self::insert_node(store, None, node);
        }
    }

    /// Recursively insert a `FolderNode` and its children into the `TreeStore`.
    fn insert_node(
        store: &gtk4::TreeStore,
        parent_iter: Option<&gtk4::TreeIter>,
        node: &FolderNode,
    ) {
        let iter = store.append(parent_iter);
        store.set(
            &iter,
            &[
                (COL_NAME, &node.name),
                (COL_STORE_ID, &node.store_id),
                (COL_ENTRY_ID, &node.entry_id),
            ],
        );

        for child in &node.children {
            Self::insert_node(store, Some(&iter), child);
        }
    }

    /// Pre-select folders that match the preselected list.
    ///
    /// Walks the tree store and selects any row whose (store_id, entry_id)
    /// matches an entry in the preselected list.
    ///
    /// **Validates: Requirement 11.3**
    fn preselect_folders(
        store: &gtk4::TreeStore,
        tree_view: &gtk4::TreeView,
        preselected: &[FolderId],
    ) {
        let selection = tree_view.selection();

        // Walk the entire tree store looking for matches
        if let Some(iter) = store.iter_first() {
            Self::preselect_walk(store, tree_view, &selection, &iter, preselected);
        }
    }

    /// Recursive helper to walk tree and select matching nodes.
    fn preselect_walk(
        store: &gtk4::TreeStore,
        tree_view: &gtk4::TreeView,
        selection: &gtk4::TreeSelection,
        iter: &gtk4::TreeIter,
        preselected: &[FolderId],
    ) {
        loop {
            // Check this node
            let store_id_val: String = store.get(iter, COL_STORE_ID as i32);
            let entry_id_val: String = store.get(iter, COL_ENTRY_ID as i32);

            // Compare case-insensitively since hex strings may vary in case
            for pre in preselected {
                if pre.store_id.0.eq_ignore_ascii_case(&store_id_val)
                    && pre.entry_id.0.eq_ignore_ascii_case(&entry_id_val)
                {
                    selection.select_iter(iter);
                    // Expand the path to make the selection visible
                    let path = store.path(iter);
                    tree_view.expand_to_path(&path);
                    break;
                }
            }

            // Recurse into children
            if let Some(child_iter) = store.iter_children(Some(iter)) {
                Self::preselect_walk(store, tree_view, selection, &child_iter, preselected);
            }

            // Move to next sibling
            if !store.iter_next(iter) {
                break;
            }
        }
    }

    /// Read the current selection from the TreeView and return folder IDs.
    ///
    /// **Validates: Requirement 11.4**
    fn read_selection(inner: &FolderBrowserInner) -> Vec<(FolderId, String)> {
        let selection = inner.tree_view.selection();
        let (paths, model) = selection.selected_rows();

        let mut results = Vec::new();
        for path in &paths {
            if let Some(iter) = model.iter(path) {
                let name: String = model.get(&iter, COL_NAME as i32);
                let store_id: String = model.get(&iter, COL_STORE_ID as i32);
                let entry_id: String = model.get(&iter, COL_ENTRY_ID as i32);

                // Skip store-level nodes with empty entry_id (they represent
                // the store root, not a selectable folder) — but if they have
                // an entry_id they are the IPM subtree root and are valid
                if entry_id.is_empty() {
                    continue;
                }

                let folder_id = FolderId::new(
                    StoreId::new(store_id),
                    EntryId::new(entry_id),
                );
                results.push((folder_id, name));
            }
        }

        results
    }
}
