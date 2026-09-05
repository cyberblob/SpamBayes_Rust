//! Wizard folder creation service.
//!
//! Creates the SpamBayes destination folders (Spam / Unsure) that the
//! configuration wizard collects from the user. This is the piece that was
//! previously missing: the GTK wizard used to gather folder *names* but never
//! created the folders, leaving the user to make them by hand.
//!
//! # Where are folders created?
//!
//! "Create the spam folder" is ambiguous until we decide *which store* and
//! *which parent folder*. This is mail-system dependent, so we mirror the
//! proven strategy from the Python reference (`Outlook2000/config_wizard.py`,
//! `_CreateFolder`):
//!
//! 1. **Store selection** — Use the store that owns the watched/receive
//!    folders (where mail is actually delivered). If that is unknown, fall
//!    back to the default message store.
//! 2. **Parent selection** — Within that store, prefer creating the folder
//!    *under the existing "Junk Email" folder* (works for Exchange /
//!    Outlook.com / Microsoft 365 where a server-managed Junk folder exists).
//!    If no Junk folder is found, fall back to the store's **root (IPM
//!    subtree)** folder (works for POP3 / IMAP / PST personal stores).
//!
//! Folder creation uses `OPEN_IF_EXISTS` semantics, so re-running the wizard
//! or naming an existing folder is safe and idempotent.
//!
//! # Threading
//!
//! All MAPI/COM work must happen on a COM-initialized (STA) thread. The GTK
//! wizard runs on the GTK thread, which is *not* COM-initialized, so callers
//! use [`create_wizard_folders`] which spins up a short-lived COM thread to
//! perform the work and returns plain data (`FolderId`s) back to the caller.
//!
//! **Validates: Requirements 13.3, 13.4, 13.11**

#![cfg(target_os = "windows")]

use spambayes_config::{EntryId, FolderId, StoreId};
use spambayes_mapi::session::MapiSession;
use spambayes_mapi::store::MessageStoreOps;

use super::wizard_window::FolderLocation;

// ─── Diagnostic Logging ──────────────────────────────────────────────────────

/// Append a timestamped diagnostic line to `%LOCALAPPDATA%\SpamBayes\addin_debug.log`.
///
/// The GTK manager process does not initialize the `log` crate backend, so
/// ordinary `log::info!` calls from here are discarded. This writes directly to
/// the same file the add-in uses, so wizard folder-creation steps are
/// diagnosable alongside the add-in's own log.
pub(crate) fn debug_log(msg: &str) {
    use std::io::Write;

    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("TEMP"))
        .unwrap_or_default();
    if base.is_empty() {
        return;
    }
    let path = std::path::Path::new(&base)
        .join("SpamBayes")
        .join("addin_debug.log");

    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[wizard_folder_creator t={ts}] {msg}");
    }
}

// ─── Result Types ────────────────────────────────────────────────────────────

/// A resolved folder: its ID plus a display name resolved from a fresh MAPI
/// hierarchy read (so callers need not rely on a possibly-stale cached tree).
#[derive(Debug, Clone)]
pub struct ResolvedFolder {
    /// The folder's entry/store ID (tree-consistent form).
    pub id: FolderId,
    /// Display name for the folder (typically the folder's own name).
    pub display_name: String,
}

/// The folders resolved/created by the wizard.
#[derive(Debug, Clone)]
pub struct CreatedFolders {
    /// The spam destination folder.
    pub spam: ResolvedFolder,
    /// The unsure destination folder.
    pub unsure: ResolvedFolder,
    /// The folder to watch for incoming mail (the delivery store's Inbox), if
    /// one could be resolved. `None` when no Inbox was found — in that case the
    /// caller should leave the existing watch-folder configuration untouched.
    pub watch: Option<ResolvedFolder>,
}

/// Error describing which folder failed to be created.
#[derive(Debug, Clone)]
pub struct FolderCreationError {
    /// The display name of the folder that could not be created.
    pub folder_name: String,
    /// A human-readable reason (for logging / diagnostics).
    pub reason: String,
}

// ─── Public Entry Point ──────────────────────────────────────────────────────

/// Create the wizard's spam and unsure folders on a dedicated COM thread.
///
/// `store_id_hint` is the hex-encoded store entry ID of the delivery store
/// (typically taken from the wizard's watched/receive folders). When `None`,
/// the default message store is used. `location` selects the parent folder
/// within that store (under the Junk Email folder, or the mailbox root).
///
/// Returns the created [`CreatedFolders`] on success, or a
/// [`FolderCreationError`] identifying the folder that failed.
///
/// This function blocks the calling (GTK) thread until the COM thread
/// finishes, which is acceptable because the wizard's Finish action is a
/// discrete, user-initiated step.
///
/// **Validates: Requirements 13.3, 13.4, 13.11**
pub fn create_wizard_folders(
    store_id_hint: Option<String>,
    location: FolderLocation,
    spam_folder_name: String,
    unsure_folder_name: String,
) -> Result<CreatedFolders, FolderCreationError> {
    // MAPI is apartment-bound; do all COM work on a dedicated STA thread.
    let handle = std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
            .ok();
        }

        let result = create_folders_inner(
            store_id_hint.as_deref(),
            location,
            &spam_folder_name,
            &unsure_folder_name,
        );

        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }

        result
    });

    handle.join().unwrap_or_else(|_| {
        Err(FolderCreationError {
            folder_name: "SpamBayes folders".to_string(),
            reason: "The folder-creation thread panicked.".to_string(),
        })
    })
}

// ─── Core Logic (runs on COM thread) ─────────────────────────────────────────

/// Perform folder creation. Must run on a COM-initialized thread.
fn create_folders_inner(
    store_id_hint: Option<&str>,
    location: FolderLocation,
    spam_folder_name: &str,
    unsure_folder_name: &str,
) -> Result<CreatedFolders, FolderCreationError> {
    let mut session = MapiSession::initialize_and_logon().map_err(|e| FolderCreationError {
        folder_name: "SpamBayes folders".to_string(),
        reason: format!("Could not connect to Outlook (MAPI logon failed): {e}"),
    })?;

    // Resolve the delivery store's binary entry ID.
    let store_eid = resolve_delivery_store(&mut session, store_id_hint).ok_or_else(|| {
        FolderCreationError {
            folder_name: "SpamBayes folders".to_string(),
            reason: "Could not locate a message store to create folders in.".to_string(),
        }
    })?;

    // Open the store.
    let store_ptr = session
        .open_store(&store_eid)
        .map_err(|e| FolderCreationError {
            folder_name: "SpamBayes folders".to_string(),
            reason: format!("Could not open the message store: {e}"),
        })?;
    let store_ops = unsafe { MessageStoreOps::new(store_ptr, store_eid.clone()) };

    debug_log(&format!(
        "start: location={location:?}, spam='{spam_folder_name}', unsure='{unsure_folder_name}', \
         store_id_hint={store_id_hint:?}, store={}",
        hex_encode(&store_eid)
    ));

    // Load the store hierarchy once. It is used both to resolve the parent
    // folder and to detect folders that already exist (so we don't attempt to
    // re-create them, which some stores reject with E_INVALIDARG).
    let hierarchy = store_ops.get_folder_hierarchy().unwrap_or_default();
    debug_log(&format!(
        "hierarchy loaded: {} top-level folder(s): [{}]",
        hierarchy.len(),
        hierarchy
            .iter()
            .map(|n| n.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // The store root's entry ID. Used both to detect "parent is the mailbox
    // root" (whose direct children are the top-level hierarchy nodes) and as
    // the fallback parent if creating under the chosen location fails.
    let root_eid = store_ops.get_root_folder().map(|f| f.entry_id).unwrap_or_default();
    let root_eid_hex = hex_encode(&root_eid);

    // Determine the parent folder entry ID based on the user's choice.
    let parent_eid = resolve_parent_folder_eid(&store_ops, &hierarchy, location);
    let parent_eid_hex = hex_encode(&parent_eid);
    let store_id_hex = hex_encode(&store_eid);

    // Whether we have a distinct root to fall back to. (When the user chose
    // MailboxRoot, or Junk resolution already fell back to root, the parent is
    // the root and there is nothing further to fall back to.)
    let root_differs_from_parent = !root_eid.is_empty() && parent_eid != root_eid;

    // Resolve (reuse existing or create) both folders under the parent, with a
    // fallback to the mailbox root if creation under the chosen parent fails.
    // This mirrors the Python reference (config_wizard._CreateFolder), which
    // creates under Junk Email but falls back to the root on any error — some
    // Exchange / Outlook.com stores reject subfolders under the special
    // server-managed Junk Email folder.
    let spam = resolve_or_create_with_fallback(
        &store_ops,
        &hierarchy,
        &parent_eid,
        &parent_eid_hex,
        &root_eid,
        &root_eid_hex,
        root_differs_from_parent,
        &store_id_hex,
        spam_folder_name,
    )?;
    let unsure = resolve_or_create_with_fallback(
        &store_ops,
        &hierarchy,
        &parent_eid,
        &parent_eid_hex,
        &root_eid,
        &root_eid_hex,
        root_differs_from_parent,
        &store_id_hex,
        unsure_folder_name,
    )?;

    // Resolve the watched folder: the delivery store's Inbox. This is the
    // folder the add-in monitors for incoming mail. We locate it by name in
    // the hierarchy already loaded above (the untyped GetReceiveFolder vtable
    // slot is not wired for calling).
    let watch = find_inbox_node(&hierarchy).map(|(eid_hex, display_name)| {
        debug_log(&format!("watch folder: Inbox found (eid={eid_hex})"));
        ResolvedFolder {
            id: FolderId::new(
                StoreId::new(store_id_hex.clone()),
                EntryId::new(eid_hex),
            ),
            display_name,
        }
    });
    if watch.is_none() {
        debug_log("watch folder: no Inbox found in store; leaving watch config unchanged");
    }

    Ok(CreatedFolders {
        spam,
        unsure,
        watch,
    })
}

/// Resolve/create the folder under the chosen parent, and if that fails, retry
/// under the mailbox root.
///
/// Mirrors the Python reference behaviour: prefer the chosen location (e.g. the
/// Junk Email folder) but degrade gracefully to the mailbox root when the store
/// refuses subfolder creation there (observed as `E_INVALIDARG` / `0x80070057`
/// on some Exchange / Outlook.com mailboxes).
#[allow(clippy::too_many_arguments)]
fn resolve_or_create_with_fallback(
    store_ops: &MessageStoreOps,
    hierarchy: &[spambayes_mapi::store::FolderTreeNode],
    parent_eid: &[u8],
    parent_eid_hex: &str,
    root_eid: &[u8],
    root_eid_hex: &str,
    root_differs_from_parent: bool,
    store_id_hex: &str,
    name: &str,
) -> Result<ResolvedFolder, FolderCreationError> {
    match resolve_or_create(
        store_ops,
        hierarchy,
        parent_eid,
        parent_eid_hex,
        root_eid_hex,
        store_id_hex,
        name,
    ) {
        Ok(id) => Ok(id),
        Err(primary_err) => {
            if root_differs_from_parent {
                log::warn!(
                    "Wizard: creating '{name}' under the chosen location failed ({}); \
                     retrying at the mailbox root.",
                    primary_err.reason
                );
                resolve_or_create(
                    store_ops,
                    hierarchy,
                    root_eid,
                    root_eid_hex,
                    root_eid_hex,
                    store_id_hex,
                    name,
                )
            } else {
                Err(primary_err)
            }
        }
    }
}

/// Reuse an existing folder if one with `name` already exists directly under
/// the parent, otherwise create it.
///
/// Checking for an existing folder first avoids the `E_INVALIDARG`
/// (`0x80070057`) some stores return from `CreateFolder` when the folder is
/// already present, and makes re-running the wizard idempotent.
fn resolve_or_create(
    store_ops: &MessageStoreOps,
    hierarchy: &[spambayes_mapi::store::FolderTreeNode],
    parent_eid: &[u8],
    parent_eid_hex: &str,
    root_eid_hex: &str,
    store_id_hex: &str,
    name: &str,
) -> Result<ResolvedFolder, FolderCreationError> {
    // Only reuse a folder that is already in the CORRECT place: a direct child
    // of the chosen parent. We deliberately do NOT reuse a same-named folder
    // found elsewhere in the store — doing so would silently adopt a folder in
    // the wrong location (e.g. a leftover under "Deleted Items" from an earlier
    // run), permanently pointing the config at the wrong folder.
    if let Some(existing_eid_hex) =
        find_child_by_name(hierarchy, parent_eid_hex, root_eid_hex, name)
    {
        debug_log(&format!("reuse: '{name}' found directly under chosen parent (eid={existing_eid_hex})"));
        return Ok(ResolvedFolder {
            id: FolderId::new(
                StoreId::new(store_id_hex.to_string()),
                EntryId::new(existing_eid_hex),
            ),
            display_name: name.to_string(),
        });
    }

    // Otherwise create it under the chosen parent. CreateFolder uses
    // OPEN_IF_EXISTS, so if a folder with this name already exists *directly
    // under this parent* it is opened rather than duplicated.
    debug_log(&format!(
        "create: '{name}' not found; calling CreateFolder under parent eid={parent_eid_hex}"
    ));
    let folder = store_ops
        .create_folder(parent_eid, name)
        .map_err(|e| {
            debug_log(&format!("create FAILED: '{name}' under parent eid={parent_eid_hex}: {e}"));
            FolderCreationError {
                folder_name: name.to_string(),
                reason: format!("MAPI CreateFolder failed: {e}"),
            }
        })?;

    let created_eid_hex = hex_encode(&folder.entry_id);
    debug_log(&format!("create OK: '{name}' eid={created_eid_hex}"));

    // IMPORTANT: CreateFolder returns a *long-term* entry ID (prefixed
    // 00000000...), whereas the folder hierarchy table returns *short-term*
    // entry IDs (e.g. ef000000...). Folder-name resolution compares entry IDs
    // by value, so the long-term ID from CreateFolder would never match the
    // tree and the folder would display as "(unknown folder)".
    //
    // To store a tree-consistent ID, reload the hierarchy and look the folder
    // up by name under the same parent. Fall back to the CreateFolder ID only
    // if the re-lookup fails.
    let effective_eid_hex = store_ops
        .get_folder_hierarchy()
        .ok()
        .and_then(|fresh| find_child_by_name(&fresh, parent_eid_hex, root_eid_hex, name))
        .map_or_else(
            || {
                debug_log(&format!(
                    "create: could not re-resolve '{name}' from reloaded hierarchy; \
                     using CreateFolder entry ID (may not resolve by name)"
                ));
                created_eid_hex
            },
            |tree_eid_hex| {
                debug_log(&format!(
                    "create: re-resolved '{name}' from hierarchy to tree eid={tree_eid_hex}"
                ));
                tree_eid_hex
            },
        );

    Ok(ResolvedFolder {
        id: FolderId::new(
            StoreId::new(store_id_hex.to_string()),
            EntryId::new(effective_eid_hex),
        ),
        display_name: name.to_string(),
    })
}

// ─── Store & Parent Resolution ───────────────────────────────────────────────

/// Resolve the binary entry ID of the store to create folders in.
///
/// Priority:
/// 1. The store identified by `store_id_hint` (from watched/receive folders),
///    matched against enumerated stores by hex entry ID.
/// 2. The default message store.
/// 3. The first enumerated store.
fn resolve_delivery_store(
    session: &mut MapiSession,
    store_id_hint: Option<&str>,
) -> Option<Vec<u8>> {
    let stores = session.enumerate_stores().ok()?;
    if stores.is_empty() {
        return None;
    }

    // 1. Match the hint (delivery store owning the watch folders).
    if let Some(hint) = store_id_hint {
        let hint_lc = hint.to_ascii_lowercase();
        if let Some(s) = stores
            .iter()
            .find(|s| hex_encode(&s.entry_id) == hint_lc)
        {
            return Some(s.entry_id.clone());
        }
    }

    // 2. Prefer the default store.
    if let Some(s) = stores.iter().find(|s| s.is_default) {
        return Some(s.entry_id.clone());
    }

    // 3. Fall back to the first store.
    stores.first().map(|s| s.entry_id.clone())
}

/// Resolve the parent folder entry ID under which to create the folders.
///
/// When `location` is [`FolderLocation::UnderJunkEmail`], prefers an existing
/// "Junk Email"-style folder and falls back to the store root if none exists.
/// When [`FolderLocation::MailboxRoot`], always uses the store root. Mirrors
/// (and now makes user-selectable) the Python `_CreateFolder` strategy.
fn resolve_parent_folder_eid(
    store_ops: &MessageStoreOps,
    hierarchy: &[spambayes_mapi::store::FolderTreeNode],
    location: FolderLocation,
) -> Vec<u8> {
    // Only look for the Junk Email folder if the user chose that location.
    if location == FolderLocation::UnderJunkEmail {
        if let Some(eid_hex) = find_junk_folder_eid(hierarchy) {
            if let Some(eid) = hex_decode(&eid_hex) {
                log::info!("Wizard: creating SpamBayes folders under the Junk Email folder.");
                return eid;
            }
        }
        log::info!(
            "Wizard: no Junk Email folder found; falling back to the mailbox root."
        );
    }

    // Mailbox root (either chosen explicitly, or as the Junk-Email fallback).
    if let Ok(root) = store_ops.get_root_folder() {
        log::info!("Wizard: creating SpamBayes folders at the mailbox root.");
        return root.entry_id;
    }

    // Last resort: empty parent (create_folder will surface a clear error).
    log::warn!("Wizard: could not resolve a parent folder; folder creation may fail.");
    Vec::new()
}

/// Depth-first search for a folder whose name looks like the Junk Email folder.
fn find_junk_folder_eid(nodes: &[spambayes_mapi::store::FolderTreeNode]) -> Option<String> {
    for node in nodes {
        if is_junk_folder_name(&node.name) {
            return Some(node.entry_id_hex.clone());
        }
        if let Some(found) = find_junk_folder_eid(&node.children) {
            return Some(found);
        }
    }
    None
}

/// Find a folder named `name` that is a **direct child** of the parent folder
/// identified by `parent_eid_hex`, returning its hex entry ID if present.
///
/// When the parent is the store root (`parent_eid_hex == root_eid_hex`), the
/// direct children are the top-level hierarchy nodes (the tree returned by
/// `get_folder_hierarchy` is rooted at the store root's children).
///
/// Folder-name comparison is case-insensitive, matching Outlook's behaviour.
fn find_child_by_name(
    hierarchy: &[spambayes_mapi::store::FolderTreeNode],
    parent_eid_hex: &str,
    root_eid_hex: &str,
    name: &str,
) -> Option<String> {
    let target = name.trim().to_ascii_lowercase();

    // Parent is the mailbox root: search the top-level nodes directly.
    if parent_eid_hex == root_eid_hex && !root_eid_hex.is_empty() {
        return match_child(hierarchy, &target);
    }

    // Otherwise locate the parent node in the tree and search its children.
    let parent = find_node_by_eid(hierarchy, parent_eid_hex)?;
    match_child(&parent.children, &target)
}

/// Return the hex entry ID of the first child whose name matches `target`
/// (already lowercased/trimmed).
fn match_child(
    nodes: &[spambayes_mapi::store::FolderTreeNode],
    target: &str,
) -> Option<String> {
    nodes
        .iter()
        .find(|n| n.name.trim().to_ascii_lowercase() == target)
        .map(|n| n.entry_id_hex.clone())
}

/// Depth-first search for the tree node whose entry-ID hex equals `eid_hex`.
fn find_node_by_eid<'a>(
    nodes: &'a [spambayes_mapi::store::FolderTreeNode],
    eid_hex: &str,
) -> Option<&'a spambayes_mapi::store::FolderTreeNode> {
    for node in nodes {
        if node.entry_id_hex == eid_hex {
            return Some(node);
        }
        if let Some(found) = find_node_by_eid(&node.children, eid_hex) {
            return Some(found);
        }
    }
    None
}

/// Depth-first search for the mailbox's Inbox folder, returning its hex entry
/// ID and display name.
///
/// The Inbox is the natural folder to watch for incoming mail. On
/// Outlook.com/Exchange and standard PST/IMAP stores it is a top-level folder
/// named "Inbox"; the search is depth-first and case-insensitive so it also
/// works if the store nests it.
fn find_inbox_node(nodes: &[spambayes_mapi::store::FolderTreeNode]) -> Option<(String, String)> {
    for node in nodes {
        if node.name.trim().eq_ignore_ascii_case("inbox") {
            return Some((node.entry_id_hex.clone(), node.name.clone()));
        }
        if let Some(found) = find_inbox_node(&node.children) {
            return Some(found);
        }
    }
    None
}

/// Whether a folder name matches the well-known Junk Email folder.
///
/// Outlook localizes and varies the spelling ("Junk Email", "Junk E-Mail",
/// "Junk E-mail"), so we normalize before comparing.
fn is_junk_folder_name(name: &str) -> bool {
    let normalized: String = name
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .collect();
    normalized == "junkemail"
}

// ─── Hex Helpers ─────────────────────────────────────────────────────────────

/// Encode bytes to a lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode a lowercase/uppercase hex string to bytes. Returns `None` if the
/// input is not valid hex.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_name_variants_match() {
        assert!(is_junk_folder_name("Junk Email"));
        assert!(is_junk_folder_name("Junk E-Mail"));
        assert!(is_junk_folder_name("Junk E-mail"));
        assert!(is_junk_folder_name("junk email"));
    }

    #[test]
    fn non_junk_names_do_not_match() {
        assert!(!is_junk_folder_name("Inbox"));
        assert!(!is_junk_folder_name("Junk Suspects"));
        assert!(!is_junk_folder_name("SpamBayes Junk E-Mail"));
        assert!(!is_junk_folder_name("SpamBayes Junk Suspects"));
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = vec![0x00, 0x01, 0xAB, 0xFF];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "0001abff");
        assert_eq!(hex_decode(&hex), Some(bytes));
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert_eq!(hex_decode("abc"), None);
    }

    #[test]
    fn find_junk_folder_searches_recursively() {
        use spambayes_mapi::store::FolderTreeNode;
        let tree = vec![FolderTreeNode {
            name: "Root".to_string(),
            store_id_hex: "aa".to_string(),
            entry_id_hex: "01".to_string(),
            children: vec![FolderTreeNode {
                name: "Junk Email".to_string(),
                store_id_hex: "aa".to_string(),
                entry_id_hex: "02".to_string(),
                children: vec![],
            }],
        }];
        assert_eq!(find_junk_folder_eid(&tree), Some("02".to_string()));
    }

    // ─── Existing-folder detection ───────────────────────────────────────

    fn sample_hierarchy() -> Vec<spambayes_mapi::store::FolderTreeNode> {
        use spambayes_mapi::store::FolderTreeNode;
        // Simulates get_folder_hierarchy(): rooted at the store root's
        // children, so "Inbox" and "Junk Email" are top-level nodes.
        vec![
            FolderTreeNode {
                name: "Inbox".to_string(),
                store_id_hex: "aa".to_string(),
                entry_id_hex: "10".to_string(),
                children: vec![],
            },
            FolderTreeNode {
                name: "Junk Email".to_string(),
                store_id_hex: "aa".to_string(),
                entry_id_hex: "20".to_string(),
                children: vec![FolderTreeNode {
                    name: "SpamBayes Junk E-Mail".to_string(),
                    store_id_hex: "aa".to_string(),
                    entry_id_hex: "21".to_string(),
                    children: vec![],
                }],
            },
        ]
    }

    #[test]
    fn finds_existing_child_under_named_parent() {
        // Parent is the Junk Email folder (eid 20); its child already exists.
        let found = find_child_by_name(
            &sample_hierarchy(),
            "20",   // parent = Junk Email
            "00",   // root eid (different)
            "SpamBayes Junk E-Mail",
        );
        assert_eq!(found, Some("21".to_string()));
    }

    #[test]
    fn existing_child_match_is_case_insensitive() {
        let found = find_child_by_name(
            &sample_hierarchy(),
            "20",
            "00",
            "spambayes junk e-mail",
        );
        assert_eq!(found, Some("21".to_string()));
    }

    #[test]
    fn returns_none_when_child_missing() {
        let found = find_child_by_name(
            &sample_hierarchy(),
            "20",
            "00",
            "Junk Suspects",
        );
        assert_eq!(found, None);
    }

    #[test]
    fn finds_existing_top_level_folder_when_parent_is_root() {
        // Parent is the store root; its direct children are the top-level
        // nodes, so "Junk Email" should be found there.
        let found = find_child_by_name(
            &sample_hierarchy(),
            "00",   // parent = root
            "00",   // root eid (same → search top-level)
            "Junk Email",
        );
        assert_eq!(found, Some("20".to_string()));
    }

    #[test]
    fn root_search_ignores_deeper_folders() {
        // "SpamBayes Junk E-Mail" is nested under Junk Email, not a direct
        // child of root, so a root-level search must not match it.
        let found = find_child_by_name(
            &sample_hierarchy(),
            "00",
            "00",
            "SpamBayes Junk E-Mail",
        );
        assert_eq!(found, None);
    }

    // ─── Inbox (watch folder) detection ──────────────────────────────────

    #[test]
    fn find_inbox_matches_top_level() {
        // sample_hierarchy() has "Inbox" at eid=10.
        assert_eq!(
            find_inbox_node(&sample_hierarchy()),
            Some(("10".to_string(), "Inbox".to_string()))
        );
    }

    #[test]
    fn find_inbox_is_case_insensitive() {
        use spambayes_mapi::store::FolderTreeNode;
        let tree = vec![FolderTreeNode {
            name: "INBOX".to_string(),
            store_id_hex: "aa".to_string(),
            entry_id_hex: "99".to_string(),
            children: vec![],
        }];
        assert_eq!(
            find_inbox_node(&tree),
            Some(("99".to_string(), "INBOX".to_string()))
        );
    }

    #[test]
    fn find_inbox_returns_none_when_absent() {
        use spambayes_mapi::store::FolderTreeNode;
        let tree = vec![FolderTreeNode {
            name: "Junk Email".to_string(),
            store_id_hex: "aa".to_string(),
            entry_id_hex: "20".to_string(),
            children: vec![],
        }];
        assert_eq!(find_inbox_node(&tree), None);
    }
}
