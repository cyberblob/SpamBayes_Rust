//! Typed folder ID structs for compile-time safety.
//!
//! These types prevent accidentally mixing store IDs and entry IDs at the type level.
//! They serialize to/from the Python tuple format `('hex_store_id', 'hex_entry_id')`
//! used in `SpamBayes` INI configuration files.

/// Newtype wrapper for message store IDs (hex string internally for INI compatibility).
///
/// Store IDs identify which Outlook message store (mailbox) a folder belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StoreId(pub String);

/// Newtype wrapper for entry IDs (hex string internally for INI compatibility).
///
/// Entry IDs identify a specific folder within a message store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntryId(pub String);

/// A typed folder identifier combining a store ID and entry ID.
///
/// This prevents accidentally swapping `store_id` and `entry_id` at compile time.
/// Serializes to/from the Python tuple format for INI compatibility:
/// `('hex_store_id', 'hex_entry_id')`
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FolderId {
    /// The message store this folder belongs to.
    pub store_id: StoreId,
    /// The entry ID of the folder within the store.
    pub entry_id: EntryId,
}

impl StoreId {
    /// Create a new `StoreId` from a hex string.
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    /// Convert the hex string to raw bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        hex_decode(&self.0)
    }
}

impl EntryId {
    /// Create a new `EntryId` from a hex string.
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    /// Convert the hex string to raw bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        hex_decode(&self.0)
    }
}

impl FolderId {
    /// Create a new `FolderId` from store and entry IDs.
    #[must_use]
    pub fn new(store_id: StoreId, entry_id: EntryId) -> Self {
        Self { store_id, entry_id }
    }

    /// Parse from Python tuple format: `('hex_store_id', 'hex_entry_id')`
    ///
    /// Accepts both single-quoted and double-quoted variants.
    /// Returns `None` if the string does not match the expected format.
    ///
    /// # Examples
    /// ```
    /// use spambayes_config::FolderId;
    ///
    /// let id = FolderId::from_ini_str("('0123ABCD', 'FEDC9876')").unwrap();
    /// assert_eq!(id.store_id.0, "0123ABCD");
    /// assert_eq!(id.entry_id.0, "FEDC9876");
    /// ```
    #[must_use]
    pub fn from_ini_str(s: &str) -> Option<Self> {
        let s = s.trim();

        // Must start with '(' and end with ')'
        if !s.starts_with('(') || !s.ends_with(')') {
            return None;
        }

        // Strip outer parentheses
        let inner = &s[1..s.len() - 1];

        // Split on comma — expect exactly two parts
        let parts: Vec<&str> = inner.splitn(2, ',').collect();
        if parts.len() != 2 {
            return None;
        }

        let store_hex = strip_quotes(parts[0].trim())?;
        let entry_hex = strip_quotes(parts[1].trim())?;

        // Validate that both are valid hex strings
        if !is_hex_string(store_hex) || !is_hex_string(entry_hex) {
            return None;
        }

        Some(FolderId {
            store_id: StoreId(store_hex.to_string()),
            entry_id: EntryId(entry_hex.to_string()),
        })
    }

    /// Serialize to Python tuple format for INI file writing.
    ///
    /// Produces: `('hex_store_id', 'hex_entry_id')`
    ///
    /// # Examples
    /// ```
    /// use spambayes_config::{FolderId, StoreId, EntryId};
    ///
    /// let id = FolderId::new(StoreId::new("0123ABCD"), EntryId::new("FEDC9876"));
    /// assert_eq!(id.to_ini_str(), "('0123ABCD', 'FEDC9876')");
    /// ```
    #[must_use]
    pub fn to_ini_str(&self) -> String {
        format!("('{}', '{}')", self.store_id.0, self.entry_id.0)
    }

    /// Convert the store ID to raw bytes for MAPI calls.
    #[must_use]
    pub fn store_id_bytes(&self) -> Vec<u8> {
        self.store_id.to_bytes()
    }

    /// Convert the entry ID to raw bytes for MAPI calls.
    #[must_use]
    pub fn entry_id_bytes(&self) -> Vec<u8> {
        self.entry_id.to_bytes()
    }
}

/// Parse a list of folder IDs from INI format: `[('id1', 'id2'), ('id3', 'id4')]`
///
/// Returns an empty Vec if the input is empty or `[]`.
#[must_use]
pub fn parse_folder_id_list(s: &str) -> Vec<FolderId> {
    let s = s.trim();
    if s.is_empty() || s == "[]" {
        return Vec::new();
    }

    // Strip outer brackets if present
    let inner = if s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    };

    let mut results = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, ch) in inner.char_indices() {
        match ch {
            '(' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let tuple_str = &inner[start..=i];
                    if let Some(folder_id) = FolderId::from_ini_str(tuple_str) {
                        results.push(folder_id);
                    }
                }
            }
            _ => {}
        }
    }

    results
}

/// Serialize a list of folder IDs to INI format.
#[must_use]
pub fn format_folder_id_list(ids: &[FolderId]) -> String {
    if ids.is_empty() {
        return "[]".to_string();
    }
    let items: Vec<String> = ids.iter().map(FolderId::to_ini_str).collect();
    format!("[{}]", items.join(", "))
}

// ─── Helper functions ────────────────────────────────────────────────────────

/// Strip surrounding quotes (single or double) from a string.
fn strip_quotes(s: &str) -> Option<&str> {
    if s.len() < 2 {
        return None;
    }
    let first = s.as_bytes()[0];
    let last = s.as_bytes()[s.len() - 1];
    if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

/// Check if a string contains only valid hexadecimal characters.
fn is_hex_string(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Decode a hex string to bytes. Returns empty vec for invalid input.
fn hex_decode(hex: &str) -> Vec<u8> {
    let hex = hex.as_bytes();
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.chunks(2) {
        if chunk.len() == 2 {
            let high = hex_nibble(chunk[0]);
            let low = hex_nibble(chunk[1]);
            if let (Some(h), Some(l)) = (high, low) {
                bytes.push((h << 4) | l);
            }
        }
    }
    bytes
}

/// Convert a single hex ASCII character to its numeric value.
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_id_roundtrip() {
        let id = FolderId::new(StoreId::new("0123ABCD"), EntryId::new("FEDC9876"));
        let serialized = id.to_ini_str();
        let parsed = FolderId::from_ini_str(&serialized).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_from_ini_str_valid() {
        let id = FolderId::from_ini_str("('0123ABCD', 'FEDC9876')").unwrap();
        assert_eq!(id.store_id.0, "0123ABCD");
        assert_eq!(id.entry_id.0, "FEDC9876");
    }

    #[test]
    fn test_from_ini_str_double_quotes() {
        let id = FolderId::from_ini_str("(\"0123ABCD\", \"FEDC9876\")").unwrap();
        assert_eq!(id.store_id.0, "0123ABCD");
        assert_eq!(id.entry_id.0, "FEDC9876");
    }

    #[test]
    fn test_from_ini_str_invalid() {
        assert!(FolderId::from_ini_str("").is_none());
        assert!(FolderId::from_ini_str("not a tuple").is_none());
        assert!(FolderId::from_ini_str("('ZZZZ', 'GGGG')").is_none());
        assert!(FolderId::from_ini_str("('only_one')").is_none());
    }

    #[test]
    fn test_from_ini_str_with_whitespace() {
        let id = FolderId::from_ini_str("  ( '0123ABCD' , 'FEDC9876' )  ").unwrap();
        assert_eq!(id.store_id.0, "0123ABCD");
        assert_eq!(id.entry_id.0, "FEDC9876");
    }

    #[test]
    fn test_store_id_bytes() {
        let store = StoreId::new("0123ABCD");
        assert_eq!(store.to_bytes(), vec![0x01, 0x23, 0xAB, 0xCD]);
    }

    #[test]
    fn test_entry_id_bytes() {
        let entry = EntryId::new("FEDC9876");
        assert_eq!(entry.to_bytes(), vec![0xFE, 0xDC, 0x98, 0x76]);
    }

    #[test]
    fn test_parse_folder_id_list_empty() {
        assert!(parse_folder_id_list("").is_empty());
        assert!(parse_folder_id_list("[]").is_empty());
    }

    #[test]
    fn test_parse_folder_id_list_multiple() {
        let input = "[('AABB', 'CCDD'), ('1122', '3344')]";
        let ids = parse_folder_id_list(input);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].store_id.0, "AABB");
        assert_eq!(ids[0].entry_id.0, "CCDD");
        assert_eq!(ids[1].store_id.0, "1122");
        assert_eq!(ids[1].entry_id.0, "3344");
    }

    #[test]
    fn test_format_folder_id_list_roundtrip() {
        let ids = vec![
            FolderId::new(StoreId::new("AABB"), EntryId::new("CCDD")),
            FolderId::new(StoreId::new("1122"), EntryId::new("3344")),
        ];
        let serialized = format_folder_id_list(&ids);
        let parsed = parse_folder_id_list(&serialized);
        assert_eq!(ids, parsed);
    }
}
