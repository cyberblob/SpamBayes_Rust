//! INI file reader/writer compatible with Python `ConfigParser` output.
//!
//! Supports:
//! - Sections in `[brackets]`
//! - Key-value pairs with ` = ` separator (matching Python `ConfigParser` output)
//! - Comments starting with `#` or `;`
//! - Multi-line values (continuation lines start with whitespace)
//! - Safe file writing (write to temp file, then rename)

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use indexmap::IndexMap;

use crate::ConfigError;

/// Ordered map of key-value pairs within a section.
pub type SectionData = IndexMap<String, String>;

/// Ordered map of section names to their key-value pairs.
pub type IniData = IndexMap<String, SectionData>;

/// INI file reader/writer compatible with Python `ConfigParser` output.
pub struct IniFile;

impl IniFile {
    /// Parse an INI file, returning sections with key-value pairs.
    ///
    /// # Format
    ///
    /// - Sections are delimited by `[section_name]`
    /// - Key-value pairs use `key = value` or `key: value` separators
    /// - Lines starting with `#` or `;` are comments
    /// - Continuation lines (starting with whitespace) append to the previous value
    /// - Empty lines and lines before any section header are ignored
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::FileNotFound` if the file does not exist.
    /// Returns `ConfigError::IoError` for other I/O failures.
    /// Returns `ConfigError::ParseError` for malformed content.
    pub fn read(path: &Path) -> Result<IniData, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::FileNotFound(path.to_path_buf()));
        }

        let file = fs::File::open(path).map_err(|e| ConfigError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;

        let reader = BufReader::new(file);
        let mut data = IniData::new();
        let mut current_section: Option<String> = None;
        let mut current_key: Option<String> = None;

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = line_result.map_err(|e| ConfigError::IoError {
                path: path.to_path_buf(),
                source: e,
            })?;

            let line_number = line_num + 1; // 1-indexed for error messages

            // Skip empty lines
            if line.trim().is_empty() {
                // Empty line resets continuation
                current_key = None;
                continue;
            }

            // Skip comment lines
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }

            // Check for continuation line (starts with whitespace)
            if (line.starts_with(' ') || line.starts_with('\t')) && current_key.is_some() {
                if let Some(ref section_name) = current_section {
                    if let Some(ref key) = current_key {
                        if let Some(section) = data.get_mut(section_name) {
                            if let Some(value) = section.get_mut(key) {
                                // Append continuation line with a newline separator
                                value.push('\n');
                                value.push_str(trimmed);
                            }
                        }
                    }
                }
                continue;
            }

            // Check for section header
            if trimmed.starts_with('[') {
                if let Some(end) = trimmed.find(']') {
                    let section_name = trimmed[1..end].trim().to_string();
                    data.entry(section_name.clone())
                        .or_default();
                    current_section = Some(section_name);
                    current_key = None;
                } else {
                    return Err(ConfigError::ParseError {
                        path: path.to_path_buf(),
                        line: line_number,
                        message: "unclosed section header bracket".to_string(),
                    });
                }
                continue;
            }

            // Must be a key-value pair
            if current_section.is_none() {
                // Key-value pair before any section — ignore (matches Python ConfigParser
                // behavior of raising MissingSectionHeaderError, but we're lenient)
                continue;
            }

            // Parse key = value or key: value
            let (key, value) = if let Some(eq_pos) = trimmed.find('=') {
                let k = trimmed[..eq_pos].trim().to_string();
                let v = trimmed[eq_pos + 1..].trim().to_string();
                (k, v)
            } else if let Some(colon_pos) = trimmed.find(':') {
                let k = trimmed[..colon_pos].trim().to_string();
                let v = trimmed[colon_pos + 1..].trim().to_string();
                (k, v)
            } else {
                return Err(ConfigError::ParseError {
                    path: path.to_path_buf(),
                    line: line_number,
                    message: format!("expected key = value or key: value, got: {trimmed}"),
                });
            };

            if let Some(ref section_name) = current_section {
                data.get_mut(section_name)
                    .expect("section must exist")
                    .insert(key.clone(), value);
                current_key = Some(key);
            }
        }

        Ok(data)
    }

    /// Write sections to an INI file using safe file writing (temp + rename).
    ///
    /// The output format matches Python's `ConfigParser`:
    /// - Section headers as `[section_name]`
    /// - Key-value pairs as `key = value`
    /// - Multi-line values use continuation lines indented with a tab
    /// - Blank line between sections
    ///
    /// # Safe Writing Strategy
    ///
    /// 1. Write content to a temporary file in the same directory as the target
    /// 2. Atomically rename the temp file to the target path
    ///
    /// This prevents corruption if the process is interrupted during writing.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::IoError` if writing or renaming fails.
    pub fn write(path: &Path, data: &IniData) -> Result<(), ConfigError> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| ConfigError::IoError {
                    path: path.to_path_buf(),
                    source: e,
                })?;
            }
        }

        // Create temp file in the same directory for atomic rename
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let temp_path = parent.join(format!(
            ".{}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("config")
        ));

        // Write to temp file
        {
            let mut file =
                fs::File::create(&temp_path).map_err(|e| ConfigError::IoError {
                    path: temp_path.clone(),
                    source: e,
                })?;

            let mut first_section = true;
            for (section_name, section_data) in data {
                // Blank line between sections (not before the first)
                if !first_section {
                    writeln!(file).map_err(|e| ConfigError::IoError {
                        path: temp_path.clone(),
                        source: e,
                    })?;
                }
                first_section = false;

                // Write section header
                writeln!(file, "[{section_name}]").map_err(|e| ConfigError::IoError {
                    path: temp_path.clone(),
                    source: e,
                })?;

                // Write key-value pairs
                for (key, value) in section_data {
                    if value.contains('\n') {
                        // Multi-line value: first line after =, continuation lines indented
                        let mut lines = value.lines();
                        if let Some(first_line) = lines.next() {
                            writeln!(file, "{key} = {first_line}").map_err(|e| {
                                ConfigError::IoError {
                                    path: temp_path.clone(),
                                    source: e,
                                }
                            })?;
                        }
                        for continuation in lines {
                            writeln!(file, "\t{continuation}").map_err(|e| {
                                ConfigError::IoError {
                                    path: temp_path.clone(),
                                    source: e,
                                }
                            })?;
                        }
                    } else {
                        writeln!(file, "{key} = {value}").map_err(|e| {
                            ConfigError::IoError {
                                path: temp_path.clone(),
                                source: e,
                            }
                        })?;
                    }
                }
            }

            // Flush to disk
            file.flush().map_err(|e| ConfigError::IoError {
                path: temp_path.clone(),
                source: e,
            })?;
        }

        // Atomic rename (on Windows, we need to remove the target first if it exists)
        if path.exists() {
            fs::remove_file(path).map_err(|e| ConfigError::IoError {
                path: path.to_path_buf(),
                source: e,
            })?;
        }

        fs::rename(&temp_path, path).map_err(|e| ConfigError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_temp_ini(content: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn test_read_basic_sections_and_values() {
        let content = "[General]\nverbose = True\nlog_file = spam.log\n\n[Filter]\nspam_threshold = 90\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        assert_eq!(data.len(), 2);
        assert_eq!(data["General"]["verbose"], "True");
        assert_eq!(data["General"]["log_file"], "spam.log");
        assert_eq!(data["Filter"]["spam_threshold"], "90");
    }

    #[test]
    fn test_read_comments_ignored() {
        let content = "# This is a comment\n; Another comment\n[Section]\nkey = value\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        assert_eq!(data.len(), 1);
        assert_eq!(data["Section"]["key"], "value");
    }

    #[test]
    fn test_read_multiline_values() {
        let content = "[Section]\nkey = first line\n  second line\n\tthird line\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        assert_eq!(data["Section"]["key"], "first line\nsecond line\nthird line");
    }

    #[test]
    fn test_read_empty_value() {
        let content = "[Section]\nempty_key =\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        assert_eq!(data["Section"]["empty_key"], "");
    }

    #[test]
    fn test_read_colon_separator() {
        let content = "[Section]\nkey: value\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        assert_eq!(data["Section"]["key"], "value");
    }

    #[test]
    fn test_read_preserves_section_order() {
        let content = "[Zebra]\na = 1\n[Alpha]\nb = 2\n[Middle]\nc = 3\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        let sections: Vec<&String> = data.keys().collect();
        assert_eq!(sections, vec!["Zebra", "Alpha", "Middle"]);
    }

    #[test]
    fn test_read_preserves_key_order() {
        let content = "[Section]\nzebra = 1\nalpha = 2\nmiddle = 3\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        let keys: Vec<&String> = data["Section"].keys().collect();
        assert_eq!(keys, vec!["zebra", "alpha", "middle"]);
    }

    #[test]
    fn test_read_file_not_found() {
        let result = IniFile::read(Path::new("/nonexistent/path.ini"));
        assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
    }

    #[test]
    fn test_read_unclosed_section() {
        let content = "[Unclosed\nkey = value\n";
        let file = create_temp_ini(content);

        let result = IniFile::read(file.path());
        assert!(matches!(result, Err(ConfigError::ParseError { .. })));
    }

    #[test]
    fn test_write_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ini");

        let mut data = IniData::new();
        let mut section = SectionData::new();
        section.insert("key1".to_string(), "value1".to_string());
        section.insert("key2".to_string(), "value2".to_string());
        data.insert("Section".to_string(), section);

        IniFile::write(&path, &data).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[Section]"));
        assert!(content.contains("key1 = value1"));
        assert!(content.contains("key2 = value2"));
    }

    #[test]
    fn test_write_multiline_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ini");

        let mut data = IniData::new();
        let mut section = SectionData::new();
        section.insert("multi".to_string(), "line1\nline2\nline3".to_string());
        data.insert("Section".to_string(), section);

        IniFile::write(&path, &data).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("multi = line1"));
        assert!(content.contains("\tline2"));
        assert!(content.contains("\tline3"));
    }

    #[test]
    fn test_write_multiple_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ini");

        let mut data = IniData::new();
        let mut s1 = SectionData::new();
        s1.insert("a".to_string(), "1".to_string());
        data.insert("First".to_string(), s1);

        let mut s2 = SectionData::new();
        s2.insert("b".to_string(), "2".to_string());
        data.insert("Second".to_string(), s2);

        IniFile::write(&path, &data).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[First]"));
        assert!(content.contains("[Second]"));
        // Sections separated by blank line
        assert!(content.contains("\n\n[Second]"));
    }

    #[test]
    fn test_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.ini");

        let mut data = IniData::new();
        let mut section = SectionData::new();
        section.insert("simple".to_string(), "value".to_string());
        section.insert("empty".to_string(), String::new());
        section.insert("multi".to_string(), "line1\nline2".to_string());
        data.insert("TestSection".to_string(), section);

        IniFile::write(&path, &data).unwrap();
        let read_back = IniFile::read(&path).unwrap();

        assert_eq!(data, read_back);
    }

    #[test]
    fn test_write_overwrites_existing_safely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ini");

        // Write initial content
        fs::write(&path, "old content").unwrap();

        let mut data = IniData::new();
        let mut section = SectionData::new();
        section.insert("key".to_string(), "new_value".to_string());
        data.insert("New".to_string(), section);

        IniFile::write(&path, &data).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[New]"));
        assert!(content.contains("key = new_value"));
        assert!(!content.contains("old content"));
    }

    #[test]
    fn test_read_python_configparser_format() {
        // Test with actual Python ConfigParser output format
        let content = "\
[Tokenizer]
replace_nonascii_chars = True
record_header_absence = True
crack_images = True
image_size = True
detect_calendar = True

[Filter]
spam_threshold = 90.0
unsure_threshold = 15.0
";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        assert_eq!(data["Tokenizer"]["replace_nonascii_chars"], "True");
        assert_eq!(data["Tokenizer"]["crack_images"], "True");
        assert_eq!(data["Filter"]["spam_threshold"], "90.0");
        assert_eq!(data["Filter"]["unsure_threshold"], "15.0");
    }

    #[test]
    fn test_read_whitespace_around_equals() {
        let content = "[Section]\nkey=value\nkey2 =value2\nkey3= value3\nkey4 = value4\n";
        let file = create_temp_ini(content);

        let data = IniFile::read(file.path()).unwrap();

        // All should be trimmed properly
        assert_eq!(data["Section"]["key"], "value");
        assert_eq!(data["Section"]["key2"], "value2");
        assert_eq!(data["Section"]["key3"], "value3");
        assert_eq!(data["Section"]["key4"], "value4");
    }
}
