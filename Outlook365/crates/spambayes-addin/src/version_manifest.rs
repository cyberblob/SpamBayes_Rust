//! Remote version manifest parsing and comparison.
//!
//! The version manifest is a JSON file hosted at a configurable URL that
//! contains information about the latest available release. The updater
//! fetches this manifest and compares it against the running version and
//! build number to determine if an update is available.

use serde::Deserialize;

// ─── Compile-time Version Constants ──────────────────────────────────────────

/// The semantic version of this build, embedded at compile time from Cargo.toml.
pub const CURRENT_VERSION: &str = env!("SPAMBAYES_VERSION");

/// The build number (Unix timestamp) of this build, embedded at compile time.
pub const CURRENT_BUILD_NUMBER: &str = env!("SPAMBAYES_BUILD_NUMBER");

/// The human-readable build ID (timestamp + epoch seconds).
pub const CURRENT_BUILD_ID: &str = env!("SPAMBAYES_BUILD_ID");

// ─── Version Manifest ────────────────────────────────────────────────────────

/// The remote version manifest describing the latest available release.
///
/// This is the JSON structure hosted at the update URL. It contains both
/// version and build number so we can detect updates even when only the
/// build changes (same version, new build).
///
/// Example JSON:
/// ```json
/// {
///   "version": "0.3.0-alpha.3",
///   "build_number": 1752500000,
///   "release_date": "2026-07-14",
///   "download_url": "https://github.com/cyberblob/SpamBayes_Rust/releases/latest",
///   "installer_url": "https://github.com/cyberblob/SpamBayes_Rust/releases/download/v0.3.0-alpha.3/SpamBayes_Outlook_Setup_0.3.0a3.exe",
///   "release_notes": "Bug fixes and performance improvements.",
///   "min_version": "0.2.0"
/// }
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct VersionManifest {
    /// The latest available semantic version (e.g., "0.3.0-alpha.3").
    pub version: String,
    /// The build number (Unix timestamp) of the latest release.
    /// Used for detecting rebuilds of the same version.
    pub build_number: u64,
    /// ISO 8601 date of the release (e.g., "2026-07-14").
    #[serde(default)]
    pub release_date: String,
    /// URL to the download/release page.
    #[serde(default)]
    pub download_url: String,
    /// Direct URL to the installer executable (optional).
    #[serde(default)]
    pub installer_url: String,
    /// Brief description of changes in this release.
    #[serde(default)]
    pub release_notes: String,
    /// Minimum version required to use this updater (for breaking schema changes).
    /// If the running version is below this, the updater should direct the user
    /// to download manually.
    #[serde(default)]
    pub min_version: String,
}

impl VersionManifest {
    /// Parse a version manifest from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

// ─── Version Comparison ──────────────────────────────────────────────────────

/// Represents the result of comparing the running version against the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// No update available — running the latest version and build.
    UpToDate,
    /// A newer version is available.
    NewVersionAvailable {
        current: String,
        latest: String,
        download_url: String,
        release_notes: String,
    },
    /// Same version but a newer build is available (rebuild/hotfix).
    NewBuildAvailable {
        version: String,
        current_build: u64,
        latest_build: u64,
        download_url: String,
    },
}

/// Compare the current running version/build against a remote manifest.
///
/// Returns the update status indicating whether an update is available
/// and what type of update it is.
pub fn check_update_status(manifest: &VersionManifest) -> UpdateStatus {
    let current_version = CURRENT_VERSION;
    let current_build: u64 = CURRENT_BUILD_NUMBER.parse().unwrap_or(0);

    // Compare version strings using semantic versioning rules.
    let version_cmp = compare_versions(current_version, &manifest.version);

    match version_cmp {
        std::cmp::Ordering::Less => {
            // Remote version is newer.
            UpdateStatus::NewVersionAvailable {
                current: current_version.to_string(),
                latest: manifest.version.clone(),
                download_url: if manifest.installer_url.is_empty() {
                    manifest.download_url.clone()
                } else {
                    manifest.installer_url.clone()
                },
                release_notes: manifest.release_notes.clone(),
            }
        }
        std::cmp::Ordering::Equal => {
            // Same version — check build number.
            if manifest.build_number > current_build {
                UpdateStatus::NewBuildAvailable {
                    version: current_version.to_string(),
                    current_build,
                    latest_build: manifest.build_number,
                    download_url: if manifest.installer_url.is_empty() {
                        manifest.download_url.clone()
                    } else {
                        manifest.installer_url.clone()
                    },
                }
            } else {
                UpdateStatus::UpToDate
            }
        }
        std::cmp::Ordering::Greater => {
            // Running version is newer than manifest (dev/pre-release build).
            UpdateStatus::UpToDate
        }
    }
}

/// Compare two semantic version strings.
///
/// Supports the Cargo/SemVer format: `MAJOR.MINOR.PATCH[-PRERELEASE]`
/// Pre-release identifiers: `alpha.N`, `beta.N`, `rc.N`
///
/// A version without a pre-release suffix is considered newer than the same
/// version with a pre-release suffix (e.g., `0.3.0` > `0.3.0-alpha.1`).
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let (a_parts, a_pre) = parse_version(a);
    let (b_parts, b_pre) = parse_version(b);

    // Compare numeric parts first.
    for i in 0..3 {
        let av = a_parts.get(i).copied().unwrap_or(0);
        let bv = b_parts.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }

    // Numeric parts are equal — compare pre-release identifiers.
    // No pre-release > any pre-release (final release is newer).
    match (&a_pre, &b_pre) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater, // a is final, b is pre-release
        (Some(_), None) => std::cmp::Ordering::Less,    // a is pre-release, b is final
        (Some(a_pre), Some(b_pre)) => compare_prerelease(a_pre, b_pre),
    }
}

/// Parse a version string into (numeric_parts, optional_prerelease).
///
/// Examples:
/// - "0.3.0" → ([0, 3, 0], None)
/// - "0.3.0-alpha.1" → ([0, 3, 0], Some("alpha.1"))
/// - "1.0.0-rc.2" → ([1, 0, 0], Some("rc.2"))
fn parse_version(v: &str) -> (Vec<u64>, Option<String>) {
    let (numeric_str, prerelease) = if let Some(idx) = v.find('-') {
        (&v[..idx], Some(v[idx + 1..].to_string()))
    } else {
        (v, None)
    };

    let parts: Vec<u64> = numeric_str
        .split('.')
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();

    (parts, prerelease)
}

/// Compare pre-release identifiers.
///
/// Ordering: alpha < beta < rc
/// Within the same type: alpha.1 < alpha.2
fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    let (a_type, a_num) = split_prerelease(a);
    let (b_type, b_num) = split_prerelease(b);

    let type_order = prerelease_type_order(&a_type).cmp(&prerelease_type_order(&b_type));
    if type_order != std::cmp::Ordering::Equal {
        return type_order;
    }

    // Same pre-release type — compare numeric suffix.
    a_num.cmp(&b_num)
}

/// Split a pre-release string into (type, number).
///
/// Examples:
/// - "alpha.1" → ("alpha", 1)
/// - "beta.3" → ("beta", 3)
/// - "rc.2" → ("rc", 2)
fn split_prerelease(s: &str) -> (String, u64) {
    if let Some(idx) = s.rfind('.') {
        let type_str = &s[..idx];
        let num_str = &s[idx + 1..];
        let num = num_str.parse::<u64>().unwrap_or(0);
        (type_str.to_lowercase(), num)
    } else {
        // No dot — try to split at the boundary between letters and digits.
        let digit_start = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
        let type_str = &s[..digit_start];
        let num_str = &s[digit_start..];
        let num = num_str.parse::<u64>().unwrap_or(0);
        (type_str.to_lowercase(), num)
    }
}

/// Map pre-release type names to a sort order.
fn prerelease_type_order(type_name: &str) -> u32 {
    match type_name {
        "alpha" | "a" => 0,
        "beta" | "b" => 1,
        "rc" | "candidate" => 2,
        _ => 3, // Unknown pre-release types sort after rc
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions_basic() {
        assert_eq!(compare_versions("0.3.0", "0.3.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("0.3.0", "0.3.1"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("0.3.1", "0.3.0"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("0.2.9", "0.3.0"), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_compare_versions_prerelease() {
        // Final release is greater than any pre-release of same version.
        assert_eq!(compare_versions("0.3.0", "0.3.0-alpha.1"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("0.3.0-alpha.1", "0.3.0"), std::cmp::Ordering::Less);

        // alpha < beta < rc
        assert_eq!(compare_versions("0.3.0-alpha.1", "0.3.0-beta.1"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("0.3.0-beta.1", "0.3.0-rc.1"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("0.3.0-rc.1", "0.3.0-alpha.1"), std::cmp::Ordering::Greater);

        // Same type, different number
        assert_eq!(compare_versions("0.3.0-alpha.1", "0.3.0-alpha.2"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("0.3.0-alpha.2", "0.3.0-alpha.1"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_compare_versions_different_major_with_prerelease() {
        // Higher major version always wins regardless of pre-release.
        assert_eq!(compare_versions("1.0.0-alpha.1", "0.9.9"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_parse_manifest_json() {
        let json = r#"{
            "version": "0.3.0-alpha.3",
            "build_number": 1752500000,
            "release_date": "2026-07-14",
            "download_url": "https://github.com/cyberblob/SpamBayes_Rust/releases/latest",
            "installer_url": "https://github.com/cyberblob/SpamBayes_Rust/releases/download/v0.3.0-alpha.3/Setup.exe",
            "release_notes": "Bug fixes.",
            "min_version": "0.2.0"
        }"#;

        let manifest = VersionManifest::from_json(json).unwrap();
        assert_eq!(manifest.version, "0.3.0-alpha.2");
        assert_eq!(manifest.build_number, 1752500000);
        assert_eq!(manifest.release_date, "2026-07-14");
        assert!(!manifest.download_url.is_empty());
        assert!(!manifest.installer_url.is_empty());
        assert_eq!(manifest.release_notes, "Bug fixes.");
        assert_eq!(manifest.min_version, "0.2.0");
    }

    #[test]
    fn test_parse_manifest_minimal_json() {
        // Only required fields.
        let json = r#"{"version": "0.3.0-alpha.3", "build_number": 12345}"#;
        let manifest = VersionManifest::from_json(json).unwrap();
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.build_number, 12345);
        assert!(manifest.download_url.is_empty());
        assert!(manifest.installer_url.is_empty());
        assert!(manifest.release_notes.is_empty());
    }
}
