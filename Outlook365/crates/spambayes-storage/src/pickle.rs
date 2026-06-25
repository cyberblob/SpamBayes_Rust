//! Python pickle format importer for `SpamBayes` database migration.
//!
//! Implements a minimal pickle virtual machine capable of parsing the
//! specific format used by the Python `SpamBayes` classifier:
//!
//! - `Classifier.__getstate__()` returns `(PICKLE_VERSION=5, wordinfo_dict, nspam, nham)`
//! - `WordInfo.__getstate__()` returns `(spamcount, hamcount)`
//! - Supports pickle protocols 0, 1, and 2
//!
//! # Usage
//!
//! ```no_run
//! use std::path::Path;
//! use spambayes_storage::pickle::PickleImporter;
//!
//! let (state, tokens) = PickleImporter::import_pickle(Path::new("classifier.pkl")).unwrap();
//! println!("nspam={}, nham={}, tokens={}", state.nspam, state.nham, tokens.len());
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use spambayes_core::WordInfo;

use crate::traits::ClassifierState;
use crate::StorageError;

/// Expected pickle version from Python `SpamBayes` `Classifier.__getstate__()`.
const SPAMBAYES_PICKLE_VERSION: i64 = 5;

/// Importer for Python `SpamBayes` pickle database files.
///
/// Reads a pickle file produced by `pickle.dump(classifier, file, protocol)`
/// and extracts the classifier state and token data.
pub struct PickleImporter;

impl PickleImporter {
    /// Import a Python `SpamBayes` pickle file.
    ///
    /// Reads the pickle file at `path`, parses it using a minimal pickle VM,
    /// and extracts the classifier state (nspam, nham) and token map.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Corrupted` if the file cannot be parsed or
    /// does not contain valid `SpamBayes` classifier data.
    /// Returns `StorageError::Io` if the file cannot be read.
    pub fn import_pickle(
        path: &Path,
    ) -> Result<(ClassifierState, HashMap<Vec<u8>, WordInfo>), StorageError> {
        let data = fs::read(path)?;
        Self::parse_pickle(&data)
    }

    /// Parse pickle bytes into classifier state and token map.
    ///
    /// This is the core parsing logic, separated from file I/O for testability.
    fn parse_pickle(
        data: &[u8],
    ) -> Result<(ClassifierState, HashMap<Vec<u8>, WordInfo>), StorageError> {
        if data.is_empty() {
            return Err(StorageError::Corrupted(
                "pickle file is empty".to_string(),
            ));
        }

        let mut vm = PickleVm::new(data);
        let value = vm.execute()?;

        Self::extract_classifier_state(&value)
    }

    /// Extract `ClassifierState` and token map from the parsed pickle value.
    ///
    /// Expects the top-level object to have state `(5, {tokens}, nspam, nham)`.
    fn extract_classifier_state(
        value: &PickleValue,
    ) -> Result<(ClassifierState, HashMap<Vec<u8>, WordInfo>), StorageError> {
        // The classifier is an object whose state is (version, wordinfo, nspam, nham)
        let state_value = match value {
            PickleValue::Object { state, .. } => state.as_deref(),
            PickleValue::Tuple(_) => Some(value),
            _ => None,
        };

        let state_value = state_value.ok_or_else(|| {
            StorageError::Corrupted(
                "pickle does not contain a valid classifier object".to_string(),
            )
        })?;

        let items = match state_value {
            PickleValue::Tuple(items) => items.as_slice(),
            _ => {
                return Err(StorageError::Corrupted(
                    "classifier state is not a tuple".to_string(),
                ));
            }
        };

        if items.len() != 4 {
            return Err(StorageError::Corrupted(format!(
                "classifier state tuple has {} elements, expected 4",
                items.len()
            )));
        }

        // items[0] = PICKLE_VERSION (should be 5)
        let version = items[0].as_int().ok_or_else(|| {
            StorageError::Corrupted("pickle version is not an integer".to_string())
        })?;

        if version != SPAMBAYES_PICKLE_VERSION {
            return Err(StorageError::Corrupted(format!(
                "unsupported pickle version {version}, expected {SPAMBAYES_PICKLE_VERSION}"
            )));
        }

        // items[1] = wordinfo dict
        let wordinfo_dict = items[1].as_dict().ok_or_else(|| {
            StorageError::Corrupted("wordinfo is not a dictionary".to_string())
        })?;

        // items[2] = nspam
        let nspam = items[2].as_int().ok_or_else(|| {
            StorageError::Corrupted("nspam is not an integer".to_string())
        })? as u64;

        // items[3] = nham
        let nham = items[3].as_int().ok_or_else(|| {
            StorageError::Corrupted("nham is not an integer".to_string())
        })? as u64;

        // Extract token entries from the wordinfo dict
        let mut tokens = HashMap::with_capacity(wordinfo_dict.len());
        for (key, val) in wordinfo_dict {
            let token_key = key.as_bytes().ok_or_else(|| {
                StorageError::Corrupted("token key is not a string or bytes".to_string())
            })?;

            let word_info = Self::extract_word_info(val)?;
            tokens.insert(token_key, word_info);
        }

        let state = ClassifierState {
            nspam,
            nham,
            version: SPAMBAYES_PICKLE_VERSION as u32,
        };

        Ok((state, tokens))
    }

    /// Extract `WordInfo` from a pickle value representing a `WordInfo` object.
    ///
    /// Expects the value to be an object with state `(spamcount, hamcount)`.
    fn extract_word_info(value: &PickleValue) -> Result<WordInfo, StorageError> {
        let state_value = match value {
            PickleValue::Object { state, .. } => state.as_deref(),
            PickleValue::Tuple(_) => Some(value),
            _ => None,
        };

        let state_value = state_value.ok_or_else(|| {
            StorageError::Corrupted(
                "WordInfo is not an object with tuple state".to_string(),
            )
        })?;

        let items = match state_value {
            PickleValue::Tuple(items) => items.as_slice(),
            _ => {
                return Err(StorageError::Corrupted(
                    "WordInfo state is not a tuple".to_string(),
                ));
            }
        };

        if items.len() != 2 {
            return Err(StorageError::Corrupted(format!(
                "WordInfo state has {} elements, expected 2",
                items.len()
            )));
        }

        let spam_count = items[0].as_int().ok_or_else(|| {
            StorageError::Corrupted("WordInfo spamcount is not an integer".to_string())
        })? as u32;

        let ham_count = items[1].as_int().ok_or_else(|| {
            StorageError::Corrupted("WordInfo hamcount is not an integer".to_string())
        })? as u32;

        Ok(WordInfo { spam_count, ham_count })
    }
}


// ─── Pickle Value Representation ─────────────────────────────────────────────

/// Represents a value parsed from a Python pickle stream.
///
/// This is a simplified representation that captures the types relevant
/// to `SpamBayes` classifier data.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum PickleValue {
    /// Python None
    None,
    /// Integer value (covers Python int)
    Int(i64),
    /// Unicode string
    String(String),
    /// Raw bytes
    Bytes(Vec<u8>),
    /// Tuple of values
    Tuple(Vec<PickleValue>),
    /// Dictionary (list of key-value pairs to preserve insertion order)
    Dict(Vec<(PickleValue, PickleValue)>),
    /// Python object with class name and state (from BUILD opcode)
    Object {
        class: String,
        state: Option<Box<PickleValue>>,
    },
    /// Boolean
    Bool(bool),
}

impl PickleValue {
    /// Try to extract an integer value.
    fn as_int(&self) -> Option<i64> {
        match self {
            PickleValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Try to extract as a dictionary (list of key-value pairs).
    fn as_dict(&self) -> Option<&[(PickleValue, PickleValue)]> {
        match self {
            PickleValue::Dict(items) => Some(items),
            _ => None,
        }
    }

    /// Try to extract as bytes (from String or Bytes).
    fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            PickleValue::String(s) => Some(s.as_bytes().to_vec()),
            PickleValue::Bytes(b) => Some(b.clone()),
            _ => None,
        }
    }
}

// ─── Pickle Virtual Machine ──────────────────────────────────────────────────

/// Minimal Python pickle virtual machine.
///
/// Supports protocols 0, 1, and 2 with the opcodes used by `SpamBayes`:
/// GLOBAL, REDUCE, NEWOBJ, BUILD, MARK, TUPLE, TUPLE2, TUPLE3,
/// `EMPTY_TUPLE`, DICT, `EMPTY_DICT`, SETITEM, SETITEMS, INT, BININT1,
/// BININT2, BININT, LONG1, BINUNICODE, `SHORT_BINSTRING`, BINSTRING,
/// UNICODE, NONE, PUT, BINPUT, `LONG_BINPUT`, GET, BINGET, `LONG_BINGET`,
/// PROTO, STOP, BOOL (TRUE/FALSE), POP, DUP, BINBYTES, `SHORT_BINBYTES`.
struct PickleVm<'a> {
    data: &'a [u8],
    pos: usize,
    stack: Vec<PickleValue>,
    memo: HashMap<u32, PickleValue>,
    /// Mark stack for MARK/TUPLE/DICT/SETITEMS operations
    metastack: Vec<Vec<PickleValue>>,
}

impl<'a> PickleVm<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            stack: Vec::new(),
            memo: HashMap::new(),
            metastack: Vec::new(),
        }
    }

    /// Execute the pickle bytecode and return the final value.
    #[allow(clippy::match_same_arms)] // Pickle opcodes with same behavior are intentionally separate for documentation
    fn execute(&mut self) -> Result<PickleValue, StorageError> {
        loop {
            if self.pos >= self.data.len() {
                return Err(StorageError::Corrupted(
                    "unexpected end of pickle data".to_string(),
                ));
            }

            let opcode = self.data[self.pos];
            self.pos += 1;

            match opcode {
                b'.' => {
                    // STOP
                    return self.stack.pop().ok_or_else(|| {
                        StorageError::Corrupted("empty stack at STOP".to_string())
                    });
                }
                0x80 => self.op_proto()?,
                b'(' => self.op_mark(),
                b't' => self.op_tuple()?,
                0x85 => self.op_tuple1()?,
                0x86 => self.op_tuple2()?,
                0x87 => self.op_tuple3()?,
                b')' => self.stack.push(PickleValue::Tuple(Vec::new())),
                b'}' => self.stack.push(PickleValue::Dict(Vec::new())),
                b'd' => self.op_dict()?,
                b's' => self.op_setitem()?,
                b'u' => self.op_setitems()?,
                b'c' => self.op_global()?,
                b'R' => self.op_reduce()?,
                0x81 => self.op_newobj()?,
                b'b' => self.op_build()?,
                b'N' => self.stack.push(PickleValue::None),
                b'K' => self.op_binint1()?,
                b'M' => self.op_binint2()?,
                b'J' => self.op_binint()?,
                b'I' => self.op_int()?,
                b'L' => self.op_long()?,
                0x8a => self.op_long1()?,
                b'X' => self.op_binunicode()?,
                b'V' => self.op_unicode()?,
                b'T' => self.op_binstring()?,
                b'U' => self.op_short_binstring()?,
                b'B' => self.op_binbytes()?,
                b'C' => self.op_short_binbytes()?,
                b'p' => self.op_put()?,
                b'q' => self.op_binput()?,
                b'r' => self.op_long_binput()?,
                b'g' => self.op_get()?,
                b'h' => self.op_binget()?,
                b'j' => self.op_long_binget()?,
                b'0' => { self.stack.pop(); } // POP
                b'2' => self.op_dup()?,
                0x88 => self.stack.push(PickleValue::Bool(true)),  // NEWTRUE
                0x89 => self.stack.push(PickleValue::Bool(false)), // NEWFALSE
                b'l' => self.op_list()?,
                b']' => self.stack.push(PickleValue::Tuple(Vec::new())), // EMPTY_LIST (treat as tuple)
                b'a' => self.op_append()?,
                b'e' => self.op_appends()?,
                _ => {
                    return Err(StorageError::Corrupted(format!(
                        "unsupported pickle opcode 0x{:02x} at position {}",
                        opcode,
                        self.pos - 1
                    )));
                }
            }
        }
    }

    // ─── Helper methods ──────────────────────────────────────────────────

    fn read_byte(&mut self) -> Result<u8, StorageError> {
        if self.pos >= self.data.len() {
            return Err(StorageError::Corrupted(
                "unexpected end of data".to_string(),
            ));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_u16_le(&mut self) -> Result<u16, StorageError> {
        if self.pos + 2 > self.data.len() {
            return Err(StorageError::Corrupted(
                "unexpected end of data reading u16".to_string(),
            ));
        }
        let val = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(val)
    }

    fn read_i32_le(&mut self) -> Result<i32, StorageError> {
        if self.pos + 4 > self.data.len() {
            return Err(StorageError::Corrupted(
                "unexpected end of data reading i32".to_string(),
            ));
        }
        let val = i32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(val)
    }

    fn read_u32_le(&mut self) -> Result<u32, StorageError> {
        if self.pos + 4 > self.data.len() {
            return Err(StorageError::Corrupted(
                "unexpected end of data reading u32".to_string(),
            ));
        }
        let val = u32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(val)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], StorageError> {
        if self.pos + n > self.data.len() {
            return Err(StorageError::Corrupted(format!(
                "unexpected end of data reading {n} bytes"
            )));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Read a newline-terminated line (for protocol 0 text-based opcodes).
    fn read_line(&mut self) -> Result<&'a [u8], StorageError> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != b'\n' {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(StorageError::Corrupted(
                "unterminated line in pickle".to_string(),
            ));
        }
        let line = &self.data[start..self.pos];
        self.pos += 1; // skip the newline
        Ok(line)
    }

    /// Pop items from the stack back to the last mark.
    fn pop_mark(&mut self) -> Result<Vec<PickleValue>, StorageError> {
        let saved = self.metastack.pop().ok_or_else(|| {
            StorageError::Corrupted("no mark on metastack".to_string())
        })?;
        let items = std::mem::replace(&mut self.stack, saved);
        Ok(items)
    }

    fn stack_pop(&mut self) -> Result<PickleValue, StorageError> {
        self.stack.pop().ok_or_else(|| {
            StorageError::Corrupted("stack underflow".to_string())
        })
    }

    // ─── Opcode implementations ──────────────────────────────────────────

    fn op_proto(&mut self) -> Result<(), StorageError> {
        // PROTO: one byte protocol version, informational only
        self.read_byte()?;
        Ok(())
    }

    fn op_mark(&mut self) {
        // Push current stack onto metastack, start fresh
        let current = std::mem::take(&mut self.stack);
        self.metastack.push(current);
    }

    fn op_tuple(&mut self) -> Result<(), StorageError> {
        let items = self.pop_mark()?;
        self.stack.push(PickleValue::Tuple(items));
        Ok(())
    }

    fn op_tuple1(&mut self) -> Result<(), StorageError> {
        let a = self.stack_pop()?;
        self.stack.push(PickleValue::Tuple(vec![a]));
        Ok(())
    }

    fn op_tuple2(&mut self) -> Result<(), StorageError> {
        let b = self.stack_pop()?;
        let a = self.stack_pop()?;
        self.stack.push(PickleValue::Tuple(vec![a, b]));
        Ok(())
    }

    fn op_tuple3(&mut self) -> Result<(), StorageError> {
        let c = self.stack_pop()?;
        let b = self.stack_pop()?;
        let a = self.stack_pop()?;
        self.stack.push(PickleValue::Tuple(vec![a, b, c]));
        Ok(())
    }

    fn op_dict(&mut self) -> Result<(), StorageError> {
        let items = self.pop_mark()?;
        let mut dict = Vec::with_capacity(items.len() / 2);
        let mut iter = items.into_iter();
        while let Some(key) = iter.next() {
            let val = iter.next().ok_or_else(|| {
                StorageError::Corrupted("odd number of items in DICT".to_string())
            })?;
            dict.push((key, val));
        }
        self.stack.push(PickleValue::Dict(dict));
        Ok(())
    }

    fn op_setitem(&mut self) -> Result<(), StorageError> {
        let val = self.stack_pop()?;
        let key = self.stack_pop()?;
        let dict = self.stack.last_mut().ok_or_else(|| {
            StorageError::Corrupted("SETITEM: no dict on stack".to_string())
        })?;
        match dict {
            PickleValue::Dict(items) => items.push((key, val)),
            _ => {
                return Err(StorageError::Corrupted(
                    "SETITEM: top of stack is not a dict".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn op_setitems(&mut self) -> Result<(), StorageError> {
        let items = self.pop_mark()?;
        let dict = self.stack.last_mut().ok_or_else(|| {
            StorageError::Corrupted("SETITEMS: no dict on stack".to_string())
        })?;
        match dict {
            PickleValue::Dict(dict_items) => {
                let mut iter = items.into_iter();
                while let Some(key) = iter.next() {
                    let val = iter.next().ok_or_else(|| {
                        StorageError::Corrupted(
                            "odd number of items in SETITEMS".to_string(),
                        )
                    })?;
                    dict_items.push((key, val));
                }
            }
            _ => {
                return Err(StorageError::Corrupted(
                    "SETITEMS: top of stack is not a dict".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn op_global(&mut self) -> Result<(), StorageError> {
        // GLOBAL: "module\nname\n" — read two lines
        let module = self.read_line()?;
        let name = self.read_line()?;
        let class_name = format!(
            "{}.{}",
            String::from_utf8_lossy(module),
            String::from_utf8_lossy(name)
        );
        self.stack.push(PickleValue::Object {
            class: class_name,
            state: None,
        });
        Ok(())
    }

    fn op_reduce(&mut self) -> Result<(), StorageError> {
        // REDUCE: pop args tuple and callable, push result object
        let _args = self.stack_pop()?;
        let callable = self.stack_pop()?;

        // For our purposes, REDUCE creates an instance.
        // The callable might be copy_reg._reconstructor or a class reference.
        let class_name = match &callable {
            PickleValue::Object { class, .. } => class.clone(),
            _ => "unknown".to_string(),
        };

        self.stack.push(PickleValue::Object {
            class: class_name,
            state: None,
        });
        Ok(())
    }

    fn op_newobj(&mut self) -> Result<(), StorageError> {
        // NEWOBJ (protocol 2): pop args and class, push new instance
        let _args = self.stack_pop()?;
        let cls = self.stack_pop()?;

        let class_name = match &cls {
            PickleValue::Object { class, .. } => class.clone(),
            _ => "unknown".to_string(),
        };

        self.stack.push(PickleValue::Object {
            class: class_name,
            state: None,
        });
        Ok(())
    }

    fn op_build(&mut self) -> Result<(), StorageError> {
        // BUILD: pop state, set it on the object at TOS
        let state = self.stack_pop()?;
        let obj = self.stack.last_mut().ok_or_else(|| {
            StorageError::Corrupted("BUILD: empty stack".to_string())
        })?;

        if let PickleValue::Object { state: s, .. } = obj {
            // If state is a tuple, store as-is for extraction
            *s = Some(Box::new(state));
        } else {
            // Some pickles BUILD on non-objects; wrap it
            // This shouldn't happen for SpamBayes but handle gracefully
        }
        Ok(())
    }

    fn op_binint1(&mut self) -> Result<(), StorageError> {
        let val = i64::from(self.read_byte()?);
        self.stack.push(PickleValue::Int(val));
        Ok(())
    }

    fn op_binint2(&mut self) -> Result<(), StorageError> {
        let val = i64::from(self.read_u16_le()?);
        self.stack.push(PickleValue::Int(val));
        Ok(())
    }

    fn op_binint(&mut self) -> Result<(), StorageError> {
        let val = i64::from(self.read_i32_le()?);
        self.stack.push(PickleValue::Int(val));
        Ok(())
    }

    fn op_int(&mut self) -> Result<(), StorageError> {
        // INT: decimal integer terminated by newline
        let line = self.read_line()?;
        let s = std::str::from_utf8(line).map_err(|_| {
            StorageError::Corrupted("INT: invalid UTF-8".to_string())
        })?;

        // Handle Python booleans encoded as INT in protocol 0
        if s == "00" || s == "01" {
            self.stack.push(PickleValue::Bool(s == "01"));
            return Ok(());
        }

        let val: i64 = s.trim().parse().map_err(|_| {
            StorageError::Corrupted(format!("INT: cannot parse '{s}'"))
        })?;
        self.stack.push(PickleValue::Int(val));
        Ok(())
    }

    fn op_long(&mut self) -> Result<(), StorageError> {
        // LONG: long integer terminated by 'L\n'
        let line = self.read_line()?;
        let s = std::str::from_utf8(line).map_err(|_| {
            StorageError::Corrupted("LONG: invalid UTF-8".to_string())
        })?;
        // Strip trailing 'L' if present
        let s = s.trim().trim_end_matches('L');
        let val: i64 = s.parse().map_err(|_| {
            StorageError::Corrupted(format!("LONG: cannot parse '{s}'"))
        })?;
        self.stack.push(PickleValue::Int(val));
        Ok(())
    }

    fn op_long1(&mut self) -> Result<(), StorageError> {
        // LONG1: 1-byte length + little-endian signed integer bytes
        let n = self.read_byte()? as usize;
        let bytes = self.read_bytes(n)?;
        let val = bytes_to_long(bytes);
        self.stack.push(PickleValue::Int(val));
        Ok(())
    }

    fn op_binunicode(&mut self) -> Result<(), StorageError> {
        // BINUNICODE: 4-byte length + UTF-8 data
        let len = self.read_u32_le()? as usize;
        let bytes = self.read_bytes(len)?;
        let s = String::from_utf8_lossy(bytes).into_owned();
        self.stack.push(PickleValue::String(s));
        Ok(())
    }

    fn op_unicode(&mut self) -> Result<(), StorageError> {
        // UNICODE (protocol 0): raw-unicode-escape encoded, newline terminated
        let line = self.read_line()?;
        let s = String::from_utf8_lossy(line).into_owned();
        self.stack.push(PickleValue::String(s));
        Ok(())
    }

    fn op_binstring(&mut self) -> Result<(), StorageError> {
        // BINSTRING: 4-byte length + raw bytes (Python 2 str)
        let len = self.read_i32_le()? as usize;
        let bytes = self.read_bytes(len)?.to_vec();
        self.stack.push(PickleValue::Bytes(bytes));
        Ok(())
    }

    fn op_short_binstring(&mut self) -> Result<(), StorageError> {
        // SHORT_BINSTRING: 1-byte length + raw bytes (Python 2 str)
        let len = self.read_byte()? as usize;
        let bytes = self.read_bytes(len)?.to_vec();
        self.stack.push(PickleValue::Bytes(bytes));
        Ok(())
    }

    fn op_binbytes(&mut self) -> Result<(), StorageError> {
        // BINBYTES: 4-byte length + raw bytes (Python 3 bytes)
        let len = self.read_u32_le()? as usize;
        let bytes = self.read_bytes(len)?.to_vec();
        self.stack.push(PickleValue::Bytes(bytes));
        Ok(())
    }

    fn op_short_binbytes(&mut self) -> Result<(), StorageError> {
        // SHORT_BINBYTES: 1-byte length + raw bytes (Python 3 bytes)
        let len = self.read_byte()? as usize;
        let bytes = self.read_bytes(len)?.to_vec();
        self.stack.push(PickleValue::Bytes(bytes));
        Ok(())
    }

    fn op_put(&mut self) -> Result<(), StorageError> {
        // PUT (protocol 0): decimal index on a line
        let line = self.read_line()?;
        let idx: u32 = std::str::from_utf8(line)
            .map_err(|_| StorageError::Corrupted("PUT: invalid UTF-8".to_string()))?
            .trim()
            .parse()
            .map_err(|_| StorageError::Corrupted("PUT: invalid index".to_string()))?;
        if let Some(val) = self.stack.last() {
            self.memo.insert(idx, val.clone());
        }
        Ok(())
    }

    fn op_binput(&mut self) -> Result<(), StorageError> {
        // BINPUT: 1-byte memo index
        let idx = u32::from(self.read_byte()?);
        if let Some(val) = self.stack.last() {
            self.memo.insert(idx, val.clone());
        }
        Ok(())
    }

    fn op_long_binput(&mut self) -> Result<(), StorageError> {
        // LONG_BINPUT: 4-byte memo index
        let idx = self.read_u32_le()?;
        if let Some(val) = self.stack.last() {
            self.memo.insert(idx, val.clone());
        }
        Ok(())
    }

    fn op_get(&mut self) -> Result<(), StorageError> {
        // GET (protocol 0): decimal index on a line
        let line = self.read_line()?;
        let idx: u32 = std::str::from_utf8(line)
            .map_err(|_| StorageError::Corrupted("GET: invalid UTF-8".to_string()))?
            .trim()
            .parse()
            .map_err(|_| StorageError::Corrupted("GET: invalid index".to_string()))?;
        let val = self.memo.get(&idx).ok_or_else(|| {
            StorageError::Corrupted(format!("GET: memo index {idx} not found"))
        })?;
        self.stack.push(val.clone());
        Ok(())
    }

    fn op_binget(&mut self) -> Result<(), StorageError> {
        // BINGET: 1-byte memo index
        let idx = u32::from(self.read_byte()?);
        let val = self.memo.get(&idx).ok_or_else(|| {
            StorageError::Corrupted(format!("BINGET: memo index {idx} not found"))
        })?;
        self.stack.push(val.clone());
        Ok(())
    }

    fn op_long_binget(&mut self) -> Result<(), StorageError> {
        // LONG_BINGET: 4-byte memo index
        let idx = self.read_u32_le()?;
        let val = self.memo.get(&idx).ok_or_else(|| {
            StorageError::Corrupted(format!("LONG_BINGET: memo index {idx} not found"))
        })?;
        self.stack.push(val.clone());
        Ok(())
    }

    fn op_dup(&mut self) -> Result<(), StorageError> {
        let val = self.stack.last().ok_or_else(|| {
            StorageError::Corrupted("DUP: empty stack".to_string())
        })?;
        let cloned = val.clone();
        self.stack.push(cloned);
        Ok(())
    }

    fn op_list(&mut self) -> Result<(), StorageError> {
        // LIST: create list from items on stack back to mark
        let items = self.pop_mark()?;
        self.stack.push(PickleValue::Tuple(items)); // Treat list as tuple
        Ok(())
    }

    fn op_append(&mut self) -> Result<(), StorageError> {
        // APPEND: pop value, append to list on TOS
        let val = self.stack_pop()?;
        let list = self.stack.last_mut().ok_or_else(|| {
            StorageError::Corrupted("APPEND: empty stack".to_string())
        })?;
        if let PickleValue::Tuple(items) = list {
            items.push(val);
        }
        Ok(())
    }

    fn op_appends(&mut self) -> Result<(), StorageError> {
        // APPENDS: pop items from mark, append all to list below mark
        let items = self.pop_mark()?;
        let list = self.stack.last_mut().ok_or_else(|| {
            StorageError::Corrupted("APPENDS: empty stack".to_string())
        })?;
        if let PickleValue::Tuple(existing) = list {
            existing.extend(items);
        }
        Ok(())
    }
}

// ─── Utility Functions ───────────────────────────────────────────────────────

/// Convert little-endian bytes to a signed i64 (for LONG1 opcode).
fn bytes_to_long(bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut result: i64 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        result |= i64::from(b) << (i * 8);
    }
    // Sign-extend if the high bit of the last byte is set
    if bytes.last().unwrap() & 0x80 != 0 {
        let shift = bytes.len() * 8;
        if shift < 64 {
            result |= !0i64 << shift;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Protocol 2 pickle of a Classifier with:
    /// - nspam=42, nham=100
    /// - wordinfo: {"hello": WordInfo(5, 10), "world": WordInfo(3, 7)}
    ///
    /// Generated by Python:
    /// ```python
    /// pickle.dump(classifier, buf, 2)
    /// ```
    fn protocol2_fixture() -> Vec<u8> {
        vec![
            0x80, 0x02,                          // PROTO 2
            b'c',                                // GLOBAL
            b'_', b'_', b'm', b'a', b'i', b'n', b'_', b'_', b'\n', // __main__\n
            b'C', b'l', b'a', b's', b's', b'i', b'f', b'i', b'e', b'r', b'\n', // Classifier\n
            b'q', 0x00,                          // BINPUT 0
            b')', 0x81,                          // EMPTY_TUPLE + NEWOBJ
            b'q', 0x01,                          // BINPUT 1
            b'(',                                // MARK
            b'K', 0x05,                          // BININT1 5 (PICKLE_VERSION)
            b'}',                                // EMPTY_DICT
            b'q', 0x02,                          // BINPUT 2
            b'(',                                // MARK (for SETITEMS)
            b'X', 0x05, 0x00, 0x00, 0x00,       // BINUNICODE len=5
            b'h', b'e', b'l', b'l', b'o',       // "hello"
            b'q', 0x03,                          // BINPUT 3
            b'c',                                // GLOBAL
            b'_', b'_', b'm', b'a', b'i', b'n', b'_', b'_', b'\n', // __main__\n
            b'W', b'o', b'r', b'd', b'I', b'n', b'f', b'o', b'\n', // WordInfo\n
            b'q', 0x04,                          // BINPUT 4
            b')', 0x81,                          // EMPTY_TUPLE + NEWOBJ
            b'q', 0x05,                          // BINPUT 5
            b'K', 0x05,                          // BININT1 5 (spamcount)
            b'K', 0x0a,                          // BININT1 10 (hamcount)
            0x86,                                // TUPLE2
            b'q', 0x06,                          // BINPUT 6
            b'b',                                // BUILD
            b'X', 0x05, 0x00, 0x00, 0x00,       // BINUNICODE len=5
            b'w', b'o', b'r', b'l', b'd',       // "world"
            b'q', 0x07,                          // BINPUT 7
            b'h', 0x04,                          // BINGET 4 (WordInfo class)
            b')', 0x81,                          // EMPTY_TUPLE + NEWOBJ
            b'q', 0x08,                          // BINPUT 8
            b'K', 0x03,                          // BININT1 3 (spamcount)
            b'K', 0x07,                          // BININT1 7 (hamcount)
            0x86,                                // TUPLE2
            b'q', 0x09,                          // BINPUT 9
            b'b',                                // BUILD
            b'u',                                // SETITEMS (back to MARK)
            b'K', 0x2a,                          // BININT1 42 (nspam)
            b'K', 0x64,                          // BININT1 100 (nham)
            b't',                                // TUPLE (back to MARK)
            b'q', 0x0a,                          // BINPUT 10
            b'b',                                // BUILD
            b'.',                                // STOP
        ]
    }

    #[test]
    fn import_valid_protocol2_pickle() {
        let data = protocol2_fixture();
        let (state, tokens) = PickleImporter::parse_pickle(&data).unwrap();

        assert_eq!(state.nspam, 42);
        assert_eq!(state.nham, 100);
        assert_eq!(state.version, 5);
        assert_eq!(tokens.len(), 2);

        let hello = tokens.get(b"hello".as_slice()).unwrap();
        assert_eq!(hello.spam_count, 5);
        assert_eq!(hello.ham_count, 10);

        let world = tokens.get(b"world".as_slice()).unwrap();
        assert_eq!(world.spam_count, 3);
        assert_eq!(world.ham_count, 7);
    }

    #[test]
    fn import_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("classifier.pkl");
        fs::write(&path, protocol2_fixture()).unwrap();

        let (state, tokens) = PickleImporter::import_pickle(&path).unwrap();
        assert_eq!(state.nspam, 42);
        assert_eq!(state.nham, 100);
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn corrupted_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.pkl");
        fs::write(&path, b"").unwrap();

        let result = PickleImporter::import_pickle(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::Corrupted(msg) => {
                assert!(msg.contains("empty"), "msg: {msg}");
            }
            other => panic!("expected Corrupted, got: {other:?}"),
        }
    }

    #[test]
    fn corrupted_garbage_data() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("garbage.pkl");
        fs::write(&path, b"this is not a pickle file at all").unwrap();

        let result = PickleImporter::import_pickle(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::Corrupted(_) => {}
            other => panic!("expected Corrupted, got: {other:?}"),
        }
    }

    #[test]
    fn corrupted_truncated_pickle() {
        // Take first 20 bytes of a valid pickle (truncated mid-stream)
        let data = protocol2_fixture();
        let truncated = &data[..20];

        let result = PickleImporter::parse_pickle(truncated);
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::Corrupted(_) => {}
            other => panic!("expected Corrupted, got: {other:?}"),
        }
    }

    #[test]
    fn wrong_pickle_version() {
        // Create a pickle with version 3 instead of 5
        let mut data = protocol2_fixture();
        // The BININT1 for version is at position after MARK opcode:
        // PROTO(2) + GLOBAL(22) + BINPUT(2) + EMPTY_TUPLE(1) + NEWOBJ(1) + BINPUT(2) + MARK(1) = 31
        // Then BININT1 opcode + value: position 31 is 'K', 32 is 0x05
        // Find and patch the version byte
        for i in 0..data.len() - 1 {
            if data[i] == b'K' && data[i + 1] == 0x05 {
                data[i + 1] = 0x03; // Change to version 3
                break;
            }
        }

        let result = PickleImporter::parse_pickle(&data);
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::Corrupted(msg) => {
                assert!(msg.contains("unsupported pickle version"), "msg: {msg}");
            }
            other => panic!("expected Corrupted, got: {other:?}"),
        }
    }

    #[test]
    fn file_not_found() {
        let result = PickleImporter::import_pickle(Path::new("/nonexistent/path/classifier.pkl"));
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::Io(_) => {}
            other => panic!("expected Io error, got: {other:?}"),
        }
    }

    #[test]
    fn binint2_larger_values() {
        // Test with BININT2 (M opcode) for values > 255
        // Classifier with nspam=5000, nham=10000, one token "big" with counts (300, 500)
        let data: Vec<u8> = vec![
            0x80, 0x02,                          // PROTO 2
            b'c',
            b'_', b'_', b'm', b'a', b'i', b'n', b'_', b'_', b'\n',
            b'C', b'l', b'a', b's', b's', b'i', b'f', b'i', b'e', b'r', b'\n',
            b'q', 0x00,
            b')', 0x81, b'q', 0x01,
            b'(',                                // MARK
            b'K', 0x05,                          // PICKLE_VERSION = 5
            b'}', b'q', 0x02,                    // EMPTY_DICT
            b'X', 0x03, 0x00, 0x00, 0x00,       // BINUNICODE "big"
            b'b', b'i', b'g',
            b'q', 0x03,
            b'c',
            b'_', b'_', b'm', b'a', b'i', b'n', b'_', b'_', b'\n',
            b'W', b'o', b'r', b'd', b'I', b'n', b'f', b'o', b'\n',
            b'q', 0x04,
            b')', 0x81, b'q', 0x05,
            b'M', 0x2c, 0x01,                   // BININT2 300
            b'M', 0xf4, 0x01,                   // BININT2 500
            0x86, b'q', 0x06,                    // TUPLE2
            b'b',                                // BUILD
            b's',                                // SETITEM
            b'M', 0x88, 0x13,                   // BININT2 5000 (nspam)
            b'M', 0x10, 0x27,                   // BININT2 10000 (nham)
            b't', b'q', 0x07,                    // TUPLE
            b'b',                                // BUILD
            b'.',                                // STOP
        ];

        let (state, tokens) = PickleImporter::parse_pickle(&data).unwrap();
        assert_eq!(state.nspam, 5000);
        assert_eq!(state.nham, 10000);
        assert_eq!(tokens.len(), 1);

        let big = tokens.get(b"big".as_slice()).unwrap();
        assert_eq!(big.spam_count, 300);
        assert_eq!(big.ham_count, 500);
    }

    #[test]
    fn import_real_python_pickle() {
        // Use Python to generate a real pickle and verify we can parse it.
        // This test uses the actual bytes generated by Python 3 with protocol 2.
        // Generated by: pickle.dump(classifier, buf, 2) where classifier has
        // nspam=42, nham=100, wordinfo={"hello": WI(5,10), "world": WI(3,7)}
        let real_pickle: Vec<u8> = vec![
            0x80, 0x02, 0x63, 0x5f, 0x5f, 0x6d, 0x61, 0x69, 0x6e, 0x5f,
            0x5f, 0x0a, 0x43, 0x6c, 0x61, 0x73, 0x73, 0x69, 0x66, 0x69,
            0x65, 0x72, 0x0a, 0x71, 0x00, 0x29, 0x81, 0x71, 0x01, 0x28,
            0x4b, 0x05, 0x7d, 0x71, 0x02, 0x28, 0x58, 0x05, 0x00, 0x00,
            0x00, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x71, 0x03, 0x63, 0x5f,
            0x5f, 0x6d, 0x61, 0x69, 0x6e, 0x5f, 0x5f, 0x0a, 0x57, 0x6f,
            0x72, 0x64, 0x49, 0x6e, 0x66, 0x6f, 0x0a, 0x71, 0x04, 0x29,
            0x81, 0x71, 0x05, 0x4b, 0x05, 0x4b, 0x0a, 0x86, 0x71, 0x06,
            0x62, 0x58, 0x05, 0x00, 0x00, 0x00, 0x77, 0x6f, 0x72, 0x6c,
            0x64, 0x71, 0x07, 0x68, 0x04, 0x29, 0x81, 0x71, 0x08, 0x4b,
            0x03, 0x4b, 0x07, 0x86, 0x71, 0x09, 0x62, 0x75, 0x4b, 0x2a,
            0x4b, 0x64, 0x74, 0x71, 0x0a, 0x62, 0x2e,
        ];

        let (state, tokens) = PickleImporter::parse_pickle(&real_pickle).unwrap();
        assert_eq!(state.nspam, 42);
        assert_eq!(state.nham, 100);
        assert_eq!(state.version, 5);
        assert_eq!(tokens.len(), 2);

        let hello = tokens.get(b"hello".as_slice()).unwrap();
        assert_eq!(hello.spam_count, 5);
        assert_eq!(hello.ham_count, 10);

        let world = tokens.get(b"world".as_slice()).unwrap();
        assert_eq!(world.spam_count, 3);
        assert_eq!(world.ham_count, 7);
    }

    #[test]
    fn bytes_to_long_converts_correctly() {
        assert_eq!(bytes_to_long(&[]), 0);
        assert_eq!(bytes_to_long(&[0x01]), 1);
        assert_eq!(bytes_to_long(&[0xff]), -1); // sign extended
        assert_eq!(bytes_to_long(&[0x00, 0x01]), 256);
        assert_eq!(bytes_to_long(&[0xe8, 0x03]), 1000);
    }

    #[test]
    fn import_protocol0_pickle() {
        // Protocol 0 pickle generated by Python 3 with:
        // Classifier(nspam=42, nham=100, wordinfo={"hello": WordInfo(5, 10)})
        let data: Vec<u8> = vec![
            99, 99, 111, 112, 121, 95, 114, 101, 103, 10, 95, 114, 101, 99,
            111, 110, 115, 116, 114, 117, 99, 116, 111, 114, 10, 112, 48, 10,
            40, 99, 95, 95, 109, 97, 105, 110, 95, 95, 10, 67, 108, 97, 115,
            115, 105, 102, 105, 101, 114, 10, 112, 49, 10, 99, 95, 95, 98,
            117, 105, 108, 116, 105, 110, 95, 95, 10, 111, 98, 106, 101, 99,
            116, 10, 112, 50, 10, 78, 116, 112, 51, 10, 82, 112, 52, 10, 40,
            73, 53, 10, 40, 100, 112, 53, 10, 86, 104, 101, 108, 108, 111,
            10, 112, 54, 10, 103, 48, 10, 40, 99, 95, 95, 109, 97, 105, 110,
            95, 95, 10, 87, 111, 114, 100, 73, 110, 102, 111, 10, 112, 55,
            10, 103, 50, 10, 78, 116, 112, 56, 10, 82, 112, 57, 10, 40, 73,
            53, 10, 73, 49, 48, 10, 116, 112, 49, 48, 10, 98, 115, 73, 52,
            50, 10, 73, 49, 48, 48, 10, 116, 112, 49, 49, 10, 98, 46,
        ];

        let (state, tokens) = PickleImporter::parse_pickle(&data).unwrap();
        assert_eq!(state.nspam, 42);
        assert_eq!(state.nham, 100);
        assert_eq!(state.version, 5);
        assert_eq!(tokens.len(), 1);

        let hello = tokens.get(b"hello".as_slice()).unwrap();
        assert_eq!(hello.spam_count, 5);
        assert_eq!(hello.ham_count, 10);
    }
}
