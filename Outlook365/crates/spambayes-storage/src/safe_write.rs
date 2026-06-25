//! Safe file writing utility for crash-safe persistence.
//!
//! Implements the write-to-temp-then-rename pattern to prevent data corruption
//! if the process crashes mid-write. On Windows, uses the `ReplaceFile` API
//! for atomic replacement of existing files.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::StorageError;

/// Atomically write `data` to `target`.
///
/// Strategy:
/// 1. Write data to a temporary file in the same directory as `target`.
/// 2. Flush and sync the temporary file to disk.
/// 3. If `target` already exists (Windows): use `ReplaceFile` for atomic swap.
///    If `target` doesn't exist or on non-Windows: use `std::fs::rename`.
/// 4. Clean up the temp file on any error.
pub fn safe_write(target: &Path, data: &[u8]) -> Result<(), StorageError> {
    let temp_path = temp_path_for(target);

    // Write data to temp file, cleaning up on failure.
    let write_result = write_and_sync(&temp_path, data);
    if let Err(e) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }

    // Atomically move temp file to target.
    let rename_result = atomic_replace(target, &temp_path);
    if let Err(e) = rename_result {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }

    Ok(())
}

/// Generate a temporary file path in the same directory as `target`.
///
/// Uses the process ID and a simple counter for uniqueness.
fn temp_path_for(target: &Path) -> std::path::PathBuf {
    let pid = std::process::id();
    let counter = next_counter();
    let file_name = target
        .file_name().map_or_else(|| "file".to_string(), |n| n.to_string_lossy().to_string());

    let temp_name = format!(".{file_name}.{pid}.{counter}.tmp");
    target.with_file_name(temp_name)
}

/// Simple atomic counter for temp file uniqueness within a process.
fn next_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Write data to a file and sync to disk.
fn write_and_sync(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    let mut file = File::create(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

/// Atomically replace `target` with `source`.
///
/// On Windows, uses `ReplaceFile` when the target already exists for a true
/// atomic swap. Falls back to `std::fs::rename` when the target doesn't exist
/// or on non-Windows platforms.
fn atomic_replace(target: &Path, source: &Path) -> Result<(), StorageError> {
    #[cfg(windows)]
    {
        if target.exists() {
            return replace_file_windows(target, source);
        }
    }

    // Target doesn't exist or non-Windows: simple rename.
    fs::rename(source, target)?;
    Ok(())
}

/// Windows-specific atomic file replacement using the `ReplaceFile` API.
#[cfg(windows)]
fn replace_file_windows(target: &Path, source: &Path) -> Result<(), StorageError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

    // Convert paths to null-terminated wide strings for the Win32 API.
    let target_wide: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();

    let result = unsafe {
        ReplaceFileW(
            PCWSTR(target_wide.as_ptr()),
            PCWSTR(source_wide.as_ptr()),
            PCWSTR::null(), // no backup
            REPLACE_FILE_FLAGS(0),
            None, // reserved
            None, // reserved
        )
    };

    match result {
        Ok(()) => Ok(()),
        Err(e) => Err(StorageError::Io(std::io::Error::other(
            format!("ReplaceFile failed: {e}"),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_new_file_creates_with_correct_content() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("output.dat");

        let data = b"hello, world!";
        safe_write(&target, data).unwrap();

        let contents = fs::read(&target).unwrap();
        assert_eq!(contents, data);
    }

    #[test]
    fn write_existing_file_replaces_atomically() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("output.dat");

        // Create initial file.
        fs::write(&target, b"original content").unwrap();

        // Overwrite with new content.
        let new_data = b"replaced content";
        safe_write(&target, new_data).unwrap();

        let contents = fs::read(&target).unwrap();
        assert_eq!(contents, new_data);
    }

    #[test]
    fn temp_file_cleaned_up_on_success() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("output.dat");

        safe_write(&target, b"data").unwrap();

        // Only the target file should exist, no .tmp files.
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name().to_str().unwrap(), "output.dat");
    }

    #[test]
    fn write_empty_data() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("empty.dat");

        safe_write(&target, b"").unwrap();

        let contents = fs::read(&target).unwrap();
        assert_eq!(contents, b"");
    }

    #[test]
    fn write_large_data() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("large.dat");

        let data = vec![0xAB_u8; 1024 * 1024]; // 1 MB
        safe_write(&target, &data).unwrap();

        let contents = fs::read(&target).unwrap();
        assert_eq!(contents, data);
    }
}
