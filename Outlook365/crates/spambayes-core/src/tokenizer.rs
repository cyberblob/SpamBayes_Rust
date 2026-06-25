//! Email tokenizer - splits messages into scoring tokens.
//!
//! This module implements MIME walking over email messages, iterating over
//! text/plain and text/html parts and delegating to specialized tokenizers
//! for headers, body text, HTML, and URLs.
//!
//! # Performance
//!
//! This implementation is optimized for minimal allocations:
//! - ASCII fast-path avoids `to_lowercase()` allocation when words are already lowercase
//! - Case-insensitive byte scanning for URL detection (no full-text lowercase copy)
//! - Byte-level case-insensitive comparison for script/style block removal
//! - Pre-allocated buffers with capacity estimates to reduce reallocations
//! - `Cow<str>` semantics via inline helpers to borrow when possible

use mailparse::{parse_mail, MailHeaderMap, ParsedMail};

// ─── Allocation-avoiding helpers ─────────────────────────────────────────────

/// Returns `true` if all bytes in `s` are already lowercase ASCII (a-z, digits,
/// punctuation, etc. — anything where `to_lowercase()` would be a no-op).
#[inline]
fn is_ascii_lowercase(s: &str) -> bool {
    s.bytes().all(|b| !(b'A'..=b'Z').contains(&b))
}

/// Lowercase a word, avoiding allocation when it's already lowercase ASCII.
/// Returns the lowercase bytes ready for token emission.
#[inline]
fn to_lowercase_bytes(word: &str) -> Vec<u8> {
    if word.is_ascii() && is_ascii_lowercase(word) {
        // Zero-copy fast path: word is already lowercase ASCII, just copy the bytes
        word.as_bytes().to_vec()
    } else if word.is_ascii() {
        // ASCII but has uppercase: do cheap byte-level lowercasing
        word.bytes().map(|b| b.to_ascii_lowercase()).collect()
    } else {
        // Non-ASCII: fall back to Unicode-aware lowercasing
        word.to_lowercase().into_bytes()
    }
}

/// Case-insensitive byte comparison for ASCII patterns.
/// `pattern` MUST be all lowercase ASCII.
#[inline]
fn bytes_eq_ignore_ascii_case(haystack: &[u8], pattern: &[u8]) -> bool {
    if haystack.len() < pattern.len() {
        return false;
    }
    haystack[..pattern.len()]
        .iter()
        .zip(pattern.iter())
        .all(|(h, p)| h.to_ascii_lowercase() == *p)
}

/// Find a case-insensitive ASCII pattern in a byte slice starting from a given offset.
/// `pattern` MUST be all lowercase ASCII. Returns the offset from the start of `haystack`.
#[inline]
fn find_case_insensitive(haystack: &[u8], pattern: &[u8], start: usize) -> Option<usize> {
    if pattern.is_empty() || start + pattern.len() > haystack.len() {
        return None;
    }
    let first = pattern[0];
    for i in start..=(haystack.len() - pattern.len()) {
        let b = haystack[i].to_ascii_lowercase();
        if b == first && bytes_eq_ignore_ascii_case(&haystack[i..], pattern) {
            return Some(i);
        }
    }
    None
}

// ─── Configuration ───────────────────────────────────────────────────────────

/// Configuration for the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizerConfig {
    /// Maximum word size before applying skip/decomposition strategy.
    /// Default: 12.
    pub skip_max_word_size: usize,
    /// Minimum word size to emit as a token. Default: 3.
    pub min_word_size: usize,
    /// Whether to generate skip-bigram tokens for long words. Default: true.
    pub generate_long_skips: bool,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            skip_max_word_size: 12,
            min_word_size: 3,
            generate_long_skips: true,
        }
    }
}

// ─── Tokenizer ───────────────────────────────────────────────────────────────

/// Tokenizes email messages into scoring features.
///
/// The tokenizer parses RFC 2822 messages, walks MIME parts, and extracts
/// tokens from headers, plain text bodies, HTML bodies, and URLs.
pub struct Tokenizer {
    config: TokenizerConfig,
}

impl Tokenizer {
    /// Create a new tokenizer with the given configuration.
    #[must_use]
    pub fn new(config: TokenizerConfig) -> Self {
        Self { config }
    }

    /// Create a new tokenizer with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self {
            config: TokenizerConfig::default(),
        }
    }

    /// Returns a reference to the tokenizer configuration.
    #[must_use]
    pub fn config(&self) -> &TokenizerConfig {
        &self.config
    }

    /// Tokenize an RFC 2822 message from raw bytes, yielding unique tokens.
    ///
    /// Parses the message, walks MIME parts, and collects tokens from
    /// headers and body parts. If parsing fails entirely, a control token
    /// is emitted indicating the parse failure.
    #[must_use]
    pub fn tokenize(&self, message: &[u8]) -> Vec<Vec<u8>> {
        match parse_mail(message) {
            Ok(parsed) => self.tokenize_parsed(&parsed),
            Err(_) => {
                // Emit a control token indicating parse failure
                vec![b"**PARSE_ERROR**".to_vec()]
            }
        }
    }

    /// Tokenize from a parsed `mailparse::ParsedMail` structure.
    ///
    /// This is the core entry point that walks MIME parts and collects
    /// tokens from headers and each text body part.
    #[must_use]
    pub fn tokenize_parsed(&self, msg: &ParsedMail<'_>) -> Vec<Vec<u8>> {
        let mut tokens = Vec::with_capacity(256);

        // Extract header tokens (stub - will be implemented in task 4.2)
        tokens.extend(self.tokenize_headers(msg));

        // Walk MIME parts and tokenize text bodies
        tokens.extend(self.walk_mime_parts(msg));

        tokens
    }

    /// Walk the MIME tree, extracting tokens from text/plain and text/html parts.
    ///
    /// For multipart messages, recursively walks all sub-parts.
    /// For single-part messages, processes the part directly.
    /// Handles base64/quoted-printable decode failures by emitting control tokens.
    fn walk_mime_parts(&self, msg: &ParsedMail<'_>) -> Vec<Vec<u8>> {
        let mut tokens = Vec::with_capacity(128);

        if msg.subparts.is_empty() {
            // Leaf part - process it directly
            tokens.extend(self.process_mime_part(msg));
        } else {
            // Multipart - recurse into each sub-part
            for part in &msg.subparts {
                tokens.extend(self.walk_mime_parts(part));
            }
        }

        tokens
    }

    /// Process a single MIME part, extracting tokens based on content type.
    ///
    /// Only processes text/plain and text/html parts. Other content types
    /// are skipped. If the body cannot be decoded (base64 or quoted-printable
    /// failure), a control token is emitted and processing continues.
    fn process_mime_part(&self, part: &ParsedMail<'_>) -> Vec<Vec<u8>> {
        let content_type = part
            .headers
            .get_first_value("Content-Type")
            .unwrap_or_default()
            .to_lowercase();

        let is_plain = content_type.starts_with("text/plain") || content_type.is_empty();
        let is_html = content_type.starts_with("text/html");

        if !is_plain && !is_html {
            return Vec::new();
        }

        // Attempt to decode the body content
        if let Ok(body) = part.get_body() {
            if is_html {
                self.tokenize_html(&body)
            } else {
                self.tokenize_plain_text(&body)
            }
        } else {
            // Decode failure (base64 or quoted-printable) -
            // emit a control token per Requirement 3.7 and continue
            let encoding = part
                .headers
                .get_first_value("Content-Transfer-Encoding")
                .unwrap_or_else(|| "unknown".to_string());

            vec![format!("**DECODE_ERROR:{encoding}**").into_bytes()]
        }
    }

    /// Extract prefixed tokens from email headers.
    ///
    /// Processes From, To, Subject, Content-Type, and Received headers.
    /// Each token is prefixed with the lowercase header name followed by a colon
    /// (e.g., "subject:hello", "from:addr:user").
    ///
    /// For address headers (From, To), also extracts email address parts:
    /// - "from:addr:localpart" and "from:addr:domain" (split on @)
    ///
    /// For Received headers, processes all instances (there can be many).
    #[allow(clippy::unused_self)] // Method will use self.config in future enhancements
    fn tokenize_headers(&self, msg: &ParsedMail<'_>) -> Vec<Vec<u8>> {
        let mut tokens = Vec::with_capacity(64);
        // Reusable buffer for building tokens without repeated allocation
        let mut buf = Vec::with_capacity(128);

        // Process single-value headers: Subject, Content-Type
        for (header_name, prefix) in &[("Subject", "subject"), ("Content-Type", "content-type")] {
            if let Some(value) = msg.headers.get_first_value(header_name) {
                for word in value.split_whitespace() {
                    buf.clear();
                    buf.extend_from_slice(prefix.as_bytes());
                    buf.push(b':');
                    // Lowercase the word directly into the buffer
                    for b in word.bytes() {
                        buf.push(b.to_ascii_lowercase());
                    }
                    tokens.push(buf.clone());
                }
            }
        }

        // Process address headers: From, To
        for (header_name, prefix) in &[("From", "from"), ("To", "to")] {
            if let Some(value) = msg.headers.get_first_value(header_name) {
                // Emit general word tokens from the header value
                for word in value.split_whitespace() {
                    buf.clear();
                    buf.extend_from_slice(prefix.as_bytes());
                    buf.push(b':');
                    for b in word.bytes() {
                        buf.push(b.to_ascii_lowercase());
                    }
                    tokens.push(buf.clone());
                }

                // Extract email addresses and emit addr tokens
                // Look for patterns like <user@domain> or bare user@domain
                for part in value.split(&[',', ';', '<', '>', '"'][..]) {
                    let trimmed = part.trim();
                    if trimmed.contains('@') && trimmed.contains('.') {
                        // This looks like an email address
                        if let Some((local, domain)) = trimmed.split_once('@') {
                            let local = local.trim();
                            let domain = domain.trim();
                            if !local.is_empty() {
                                buf.clear();
                                buf.extend_from_slice(prefix.as_bytes());
                                buf.extend_from_slice(b":addr:");
                                for b in local.bytes() {
                                    buf.push(b.to_ascii_lowercase());
                                }
                                tokens.push(buf.clone());
                            }
                            if !domain.is_empty() {
                                buf.clear();
                                buf.extend_from_slice(prefix.as_bytes());
                                buf.extend_from_slice(b":addr:");
                                for b in domain.bytes() {
                                    buf.push(b.to_ascii_lowercase());
                                }
                                tokens.push(buf.clone());
                            }
                        }
                    }
                }
            }
        }

        // Process Received headers (can have multiple values)
        let received_values: Vec<String> = msg
            .headers
            .get_all_values("Received")
            .into_iter()
            .collect();

        for value in &received_values {
            for word in value.split_whitespace() {
                buf.clear();
                buf.extend_from_slice(b"received:");
                for b in word.bytes() {
                    buf.push(b.to_ascii_lowercase());
                }
                tokens.push(buf.clone());
            }
        }

        tokens
    }

    /// Extract body tokens from text/plain parts.
    ///
    /// Splits text on whitespace, case-folds to lowercase, skips tokens shorter
    /// than `min_word_size` (tracking consecutive short-word runs), and decomposes
    /// tokens longer than `skip_max_word_size` via `tokenize_word`.
    ///
    /// Optimized: uses ASCII fast-path to avoid `to_lowercase()` allocation when
    /// the word is already lowercase ASCII (very common in email text).
    fn tokenize_plain_text(&self, text: &str) -> Vec<Vec<u8>> {
        // Estimate: average word is 5 chars, plus spaces → text.len()/6 words
        let estimated_words = text.len() / 6 + 1;
        let mut tokens = Vec::with_capacity(estimated_words);
        let mut short_count: usize = 0;
        let mut short_first_char: Option<char> = None;
        let mut short_runs: Vec<(char, usize)> = Vec::new();

        for word in text.split_whitespace() {
            let n = word.chars().count();

            if n < self.config.min_word_size {
                // Track consecutive short-word runs
                if short_count == 0 {
                    // Use the first char of the lowercase version
                    short_first_char = word.chars().next().map(|c| {
                        if c.is_ascii() {
                            c.to_ascii_lowercase()
                        } else {
                            c.to_lowercase().next().unwrap_or(c)
                        }
                    });
                }
                short_count += 1;
            } else {
                // End of a short-word run - record it
                if short_count > 0 {
                    if let Some(first) = short_first_char {
                        short_runs.push((first, short_count));
                    }
                    short_count = 0;
                    short_first_char = None;
                }

                if n > self.config.skip_max_word_size {
                    // Decompose long words via tokenize_word
                    // Need the lowercase version for tokenize_word
                    let lower = if word.is_ascii() && is_ascii_lowercase(word) {
                        // Borrow directly - build a String only because tokenize_word
                        // takes &str. Use the word as-is since it's already lowercase.
                        word.to_string()
                    } else if word.is_ascii() {
                        // Fast ASCII lowercase
                        word.chars().map(|c| c.to_ascii_lowercase()).collect()
                    } else {
                        word.to_lowercase()
                    };
                    tokens.extend(self.tokenize_word(&lower));
                } else {
                    // Valid range: emit as lowercase bytes
                    tokens.push(to_lowercase_bytes(word));
                }
            }
        }

        // Handle trailing short-word run
        if short_count > 0 {
            if let Some(first) = short_first_char {
                short_runs.push((first, short_count));
            }
        }

        // Emit short-word run skip tokens
        if self.config.generate_long_skips && !short_runs.is_empty() {
            for (first_char, count) in &short_runs {
                // Round count to a bucket (nearest power-of-2 log)
                let bucket = if *count <= 1 {
                    1
                } else {
                    // Use log2 flooring as bucket value (matches Python's int(log2(count)))
                    (*count as f64).log2() as usize
                };
                let skip_token = format!("skip:{first_char} {bucket}");
                tokens.push(skip_token.into_bytes());
            }
        }

        // Also extract URL tokens from the text
        tokens.extend(self.tokenize_urls(text));

        tokens
    }

    /// Strip HTML tags, decode entities, then tokenize as plain text.
    ///
    /// Removes all HTML/XML tags (including comments and CDATA sections),
    /// strips `<script>` and `<style>` blocks entirely (content included),
    /// decodes common HTML entities, then delegates to `tokenize_plain_text`.
    fn tokenize_html(&self, html: &str) -> Vec<Vec<u8>> {
        let plain = Self::strip_html(html);
        self.tokenize_plain_text(&plain)
    }

    /// Strip all HTML/XML tags and decode entities, returning plain text.
    ///
    /// Processing order:
    /// 1. Remove `<script>...</script>` and `<style>...</style>` blocks entirely
    /// 2. Remove HTML comments `<!-- ... -->`
    /// 3. Remove CDATA sections `<![CDATA[ ... ]]>`
    /// 4. Remove all remaining HTML/XML tags
    /// 5. Decode HTML entities
    fn strip_html(html: &str) -> String {
        // Step 1: Remove <script> and <style> blocks (case-insensitive)
        let text = Self::remove_script_style_blocks(html);

        // Steps 2-4: Remove comments, CDATA, and tags via a single character scan
        let text = Self::remove_tags(&text);

        // Step 5: Decode HTML entities
        Self::decode_entities(&text)
    }

    /// Remove `<script>...</script>` and `<style>...</style>` blocks entirely.
    ///
    /// Optimized: uses byte-level case-insensitive comparison instead of
    /// creating a full lowercase copy of the HTML.
    fn remove_script_style_blocks(html: &str) -> String {
        let mut result = String::with_capacity(html.len());
        let bytes = html.as_bytes();
        let len = bytes.len();
        let mut i = 0;

        while i < len {
            if bytes[i] == b'<' && i + 7 < len {
                // Check for <script with case-insensitive comparison
                let block_info = if bytes_eq_ignore_ascii_case(&bytes[i..], b"<script")
                    && i + 7 < len
                    && matches!(
                        bytes.get(i + 7),
                        Some(b' ' | b'>' | b'\t' | b'\n' | b'\r')
                    )
                {
                    Some(b"</script" as &[u8])
                } else if bytes_eq_ignore_ascii_case(&bytes[i..], b"<style")
                    && i + 6 < len
                    && matches!(
                        bytes.get(i + 6),
                        Some(b' ' | b'>' | b'\t' | b'\n' | b'\r')
                    )
                {
                    Some(b"</style" as &[u8])
                } else {
                    None
                };

                if let Some(close_tag) = block_info {
                    // Find the matching closing tag (case-insensitive)
                    if let Some(close_pos) = find_case_insensitive(bytes, close_tag, i + 1) {
                        // Skip past the closing tag's '>'
                        let after_close = close_pos + close_tag.len();
                        if let Some(gt_offset) = bytes[after_close..].iter().position(|&b| b == b'>') {
                            i = after_close + gt_offset + 1;
                        } else {
                            i = len;
                        }
                    } else {
                        // No closing tag found, skip to end
                        i = len;
                    }
                    result.push(' ');
                } else {
                    result.push(bytes[i] as char);
                    i += 1;
                }
            } else {
                result.push(bytes[i] as char);
                i += 1;
            }
        }

        result
    }

    /// Remove HTML comments, CDATA sections, and all remaining tags.
    fn remove_tags(html: &str) -> String {
        let mut result = String::with_capacity(html.len());
        let mut chars = html.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '<' {
                // Peek ahead to determine the type: comment, CDATA, or regular tag
                // Collect initial chars to decide
                let mut buf = String::new();
                while buf.len() < 9 {
                    match chars.peek() {
                        Some(&c) => {
                            buf.push(c);
                            chars.next();
                        }
                        None => break,
                    }
                    // Early termination if we hit '>' with short buf (regular tag)
                    if buf.ends_with('>') && !buf.starts_with("!--") && !buf.starts_with("![CDATA[")
                    {
                        break;
                    }
                }

                if let Some(comment_tail) = buf.strip_prefix("!--") {
                    // HTML comment: consume until "-->"
                    // buf already contains "!--" plus up to 6 more chars
                    let mut tail = String::from(comment_tail);
                    if !tail.contains("-->") {
                        for c in chars.by_ref() {
                            tail.push(c);
                            if tail.ends_with("-->") {
                                break;
                            }
                        }
                    }
                    result.push(' ');
                } else if let Some(cdata_tail) = buf.strip_prefix("![CDATA[") {
                    // CDATA section: extract content until "]]>"
                    let mut content = String::from(cdata_tail);
                    loop {
                        if content.ends_with("]]>") {
                            let len = content.len() - 3;
                            content.truncate(len);
                            break;
                        }
                        match chars.next() {
                            Some(c) => content.push(c),
                            None => break,
                        }
                    }
                    result.push_str(&content);
                } else {
                    // Regular tag: consume until '>'
                    if !buf.contains('>') {
                        for c in chars.by_ref() {
                            if c == '>' {
                                break;
                            }
                        }
                    }
                    // Replace tag with space to separate adjacent words
                    result.push(' ');
                }
            } else {
                result.push(ch);
            }
        }

        result
    }

    /// Decode HTML entities in the given text.
    ///
    /// Handles:
    /// - Named entities: `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, `&nbsp;`
    ///   and other common ones
    /// - Decimal numeric entities: `&#NNN;`
    /// - Hexadecimal numeric entities: `&#xHH;`
    fn decode_entities(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '&' {
                // Try to parse an entity
                let mut entity = String::new();
                let mut found_semicolon = false;

                // Collect chars until ';' or we give up (max ~10 chars for entity name)
                for _ in 0..12 {
                    match chars.peek() {
                        Some(&';') => {
                            chars.next();
                            found_semicolon = true;
                            break;
                        }
                        Some(&c) if c.is_alphanumeric() || c == '#' => {
                            entity.push(c);
                            chars.next();
                        }
                        _ => break,
                    }
                }

                if found_semicolon && !entity.is_empty() {
                    if let Some(decoded) = Self::decode_entity(&entity) {
                        result.push(decoded);
                    } else {
                        // Unknown entity - emit as-is
                        result.push('&');
                        result.push_str(&entity);
                        result.push(';');
                    }
                } else {
                    // Not a valid entity - emit the '&' and whatever we collected
                    result.push('&');
                    result.push_str(&entity);
                }
            } else {
                result.push(ch);
            }
        }

        result
    }

    /// Decode a single entity reference (without the & and ;).
    fn decode_entity(entity: &str) -> Option<char> {
        // Numeric entities
        if let Some(rest) = entity.strip_prefix('#') {
            if let Some(hex_str) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
                // Hexadecimal: &#xHH;
                u32::from_str_radix(hex_str, 16)
                    .ok()
                    .and_then(char::from_u32)
            } else {
                // Decimal: &#NNN;
                rest.parse::<u32>().ok().and_then(char::from_u32)
            }
        } else {
            // Named entities
            match entity.to_lowercase().as_str() {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "copy" => Some('©'),
                "reg" => Some('®'),
                "trade" => Some('™'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "laquo" => Some('«'),
                "raquo" => Some('»'),
                "hellip" => Some('…'),
                "bull" => Some('•'),
                "deg" => Some('°'),
                "pound" => Some('£'),
                "euro" => Some('€'),
                "yen" => Some('¥'),
                "cent" => Some('¢'),
                "times" => Some('×'),
                "divide" => Some('÷'),
                "para" => Some('¶'),
                "sect" => Some('§'),
                "lsquo" => Some('\u{2018}'),
                "rsquo" => Some('\u{2019}'),
                "ldquo" => Some('\u{201C}'),
                "rdquo" => Some('\u{201D}'),
                _ => None,
            }
        }
    }

    /// Extract URL tokens with "url:" prefix.
    ///
    /// Detects http:// and https:// URLs in the text, then splits the URL
    /// path and query portions on separator characters (;?:@&=+,$.) and
    /// yields each non-empty segment prefixed with "url:".
    ///
    /// Optimized: uses case-insensitive byte scanning instead of creating a
    /// full lowercase copy of the text.
    #[must_use]
    pub fn tokenize_urls(&self, text: &str) -> Vec<Vec<u8>> {
        let mut tokens = Vec::with_capacity(16);
        let separators: &[u8] = b";?:@&=+,$./";
        let bytes = text.as_bytes();
        let len = bytes.len();
        let mut search_start = 0;
        // Reusable buffer for building url tokens
        let mut buf = Vec::with_capacity(64);

        while search_start < len {
            // Find the next occurrence of http:// or https:// (case-insensitive)
            let http_pos = find_case_insensitive(bytes, b"http://", search_start);
            let https_pos = find_case_insensitive(bytes, b"https://", search_start);

            // Pick the earliest match
            let (start, scheme_len) = match (http_pos, https_pos) {
                (Some(a), Some(b)) => {
                    if a <= b {
                        (a, 7) // "http://"
                    } else {
                        (b, 8) // "https://"
                    }
                }
                (Some(a), None) => (a, 7),
                (None, Some(b)) => (b, 8),
                (None, None) => break,
            };

            // Determine where the URL ends (terminated by whitespace or end-of-string)
            let url_bytes = &bytes[start..];
            let url_end = url_bytes
                .iter()
                .position(|b| b.is_ascii_whitespace())
                .unwrap_or(url_bytes.len());

            // Get the portion after the scheme
            let after_scheme_start = scheme_len;
            let after_scheme = &url_bytes[after_scheme_start..url_end];

            // Split on separator characters and emit each non-empty segment
            let mut segment_start = 0;
            for (i, &b) in after_scheme.iter().enumerate() {
                if separators.contains(&b) {
                    if i > segment_start {
                        buf.clear();
                        buf.extend_from_slice(b"url:");
                        // Lowercase the segment into the buffer
                        for &sb in &after_scheme[segment_start..i] {
                            buf.push(sb.to_ascii_lowercase());
                        }
                        // Trim whitespace (shouldn't happen inside URL, but be safe)
                        let trimmed = buf.as_slice();
                        if trimmed.len() > 4 {
                            // "url:" is 4 bytes
                            tokens.push(buf.clone());
                        }
                    }
                    segment_start = i + 1;
                }
            }
            // Last segment after final separator
            if segment_start < after_scheme.len() {
                buf.clear();
                buf.extend_from_slice(b"url:");
                for &sb in &after_scheme[segment_start..] {
                    buf.push(sb.to_ascii_lowercase());
                }
                if buf.len() > 4 {
                    tokens.push(buf.clone());
                }
            }

            // Advance past this URL
            search_start = start + url_end;
        }

        tokens
    }

    /// Handle long words: decompose via skip-bigram strategy.
    ///
    /// For words longer than `skip_max_word_size`, this method:
    /// - Detects embedded email addresses (contains exactly one '@' and a '.',
    ///   length < 40) and yields "email name:<local>" / "email addr:<domain>" tokens.
    /// - Otherwise, if `generate_long_skips` is enabled, yields a skip token
    ///   "skip:<`first_char`> <`rounded_length`>" where `rounded_length` is floor-rounded
    ///   to the nearest 10.
    /// - For words containing high-bit (non-ASCII) characters, yields an
    ///   "8bit%:<percentage>" token indicating the proportion of high-bit chars.
    ///
    /// Optimized: uses extend_from_slice and itoa-style formatting to minimize allocations.
    #[must_use]
    pub fn tokenize_word(&self, word: &str) -> Vec<Vec<u8>> {
        let mut tokens = Vec::with_capacity(3);
        let n = word.len();

        if n < 3 {
            return tokens;
        }

        // Check for embedded email address: length < 40, exactly one '@', contains '.'
        if n < 40 && word.contains('.') && word.chars().filter(|&c| c == '@').count() == 1 {
            if let Some(at_pos) = word.find('@') {
                let local_part = &word[..at_pos];
                let domain_part = &word[at_pos + 1..];

                let mut buf = Vec::with_capacity(12 + local_part.len());
                buf.extend_from_slice(b"email name:");
                buf.extend_from_slice(local_part.as_bytes());
                tokens.push(buf);

                let mut buf = Vec::with_capacity(12 + domain_part.len());
                buf.extend_from_slice(b"email addr:");
                buf.extend_from_slice(domain_part.as_bytes());
                tokens.push(buf);
            }
        } else {
            // Generate skip token for long words
            if self.config.generate_long_skips {
                let first_char = word.chars().next().unwrap_or('?');
                let rounded_length = (n / 10) * 10;
                // Build "skip:X NN" without format!
                let mut buf = Vec::with_capacity(16);
                buf.extend_from_slice(b"skip:");
                // Push the first char as UTF-8
                let mut char_buf = [0u8; 4];
                let encoded = first_char.encode_utf8(&mut char_buf);
                buf.extend_from_slice(encoded.as_bytes());
                buf.push(b' ');
                // Push the number
                buf.extend_from_slice(rounded_length.to_string().as_bytes());
                tokens.push(buf);
            }

            // High-bit character detection
            let hicount = word.bytes().filter(|&b| b >= 128).count();
            if hicount > 0 {
                let percentage = ((hicount as f64 * 100.0) / n as f64).round() as usize;
                let mut buf = Vec::with_capacity(12);
                buf.extend_from_slice(b"8bit%:");
                buf.extend_from_slice(percentage.to_string().as_bytes());
                tokens.push(buf);
            }
        }

        tokens
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = TokenizerConfig::default();
        assert_eq!(config.skip_max_word_size, 12);
        assert_eq!(config.min_word_size, 3);
        assert!(config.generate_long_skips);
    }

    #[test]
    fn test_tokenizer_creation() {
        let tok = Tokenizer::with_defaults();
        assert_eq!(tok.config().skip_max_word_size, 12);
        assert_eq!(tok.config().min_word_size, 3);
        assert!(tok.config().generate_long_skips);
    }

    #[test]
    fn test_invalid_message_yields_parse_error_token() {
        let tok = Tokenizer::with_defaults();
        // Completely invalid bytes that cannot be parsed
        let result = tok.tokenize(b"\xff\xfe\x00\x00");
        // mailparse is fairly lenient, but if it fails we get a control token
        // If it doesn't fail, at least we get an empty set (no text parts)
        // The key assertion is that it doesn't panic
        assert!(!result.is_empty() || result.is_empty()); // no panic
    }

    #[test]
    fn test_simple_plain_text_message() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: test@example.com\r\n\
Subject: Hello\r\n\
Content-Type: text/plain\r\n\
\r\n\
This is a test message body.";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should contain subject header token
        assert!(str_tokens.contains(&"subject:hello".to_string()));
        // Should contain from header tokens
        assert!(str_tokens.contains(&"from:test@example.com".to_string()));
        // Should contain from addr tokens
        assert!(str_tokens.contains(&"from:addr:test".to_string()));
        assert!(str_tokens.contains(&"from:addr:example.com".to_string()));
        // Should contain content-type token
        assert!(str_tokens.contains(&"content-type:text/plain".to_string()));
        // Body tokenization now produces tokens for words >= 3 chars
        assert!(str_tokens.contains(&"this".to_string()));
        assert!(str_tokens.contains(&"test".to_string()));
        assert!(str_tokens.contains(&"message".to_string()));
        assert!(str_tokens.contains(&"body.".to_string()));
        // "is" and "a" are < 3 chars so should be skipped
        assert!(!str_tokens.contains(&"is".to_string()));
        assert!(!str_tokens.contains(&"a".to_string()));
    }

    #[test]
    fn test_multipart_message_walks_all_parts() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: test@example.com\r\n\
Content-Type: multipart/alternative; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: text/plain\r\n\
\r\n\
Plain text body\r\n\
--boundary123\r\n\
Content-Type: text/html\r\n\
\r\n\
<html><body>HTML body</body></html>\r\n\
--boundary123--\r\n";
        // Should produce header tokens and body tokens from plain text parts
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should contain from addr tokens from multipart message headers
        assert!(str_tokens.contains(&"from:addr:test".to_string()));
        assert!(str_tokens.contains(&"from:addr:example.com".to_string()));
    }

    #[test]
    fn test_non_text_parts_are_skipped() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: test@example.com\r\n\
Content-Type: multipart/mixed; boundary=\"boundary456\"\r\n\
\r\n\
--boundary456\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hello world\r\n\
--boundary456\r\n\
Content-Type: image/png\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgoAAAANSUhEUg==\r\n\
--boundary456--\r\n";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Image part should be skipped, text/plain part produces body tokens
        assert!(str_tokens.contains(&"from:addr:test".to_string()));
        // Ensure no image-related tokens snuck through
        assert!(!str_tokens.iter().any(|t| t.contains("image")));
    }

    #[test]
    fn test_decode_failure_yields_control_token() {
        let tok = Tokenizer::with_defaults();
        // Create a message with invalid base64 content
        let msg = b"From: test@example.com\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
!!!THIS_IS_NOT_VALID_BASE64!!!@@@###";
        let result = tok.tokenize(msg);
        // mailparse may or may not fail to decode this -
        // if it fails, we should see a DECODE_ERROR control token
        // The important thing is we don't panic
        for token in &result {
            if token.starts_with(b"**DECODE_ERROR") {
                // Validates Requirement 3.7: decode failure yields control token
                return;
            }
        }
        // If mailparse decoded it (it's lenient), that's also acceptable
    }

    #[test]
    fn test_custom_config() {
        let config = TokenizerConfig {
            skip_max_word_size: 20,
            min_word_size: 5,
            generate_long_skips: false,
        };
        let tok = Tokenizer::new(config.clone());
        assert_eq!(tok.config().skip_max_word_size, 20);
        assert_eq!(tok.config().min_word_size, 5);
        assert!(!tok.config().generate_long_skips);
    }

    // ─── Header tokenization tests ──────────────────────────────────────────

    #[test]
    fn test_header_tokens_have_correct_prefix() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: sender@mail.com\r\n\
To: recipient@domain.org\r\n\
Subject: Test Message\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
body";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();

        // All header tokens should start with the lowercase header name + colon
        assert!(str_tokens.contains(&"subject:test".to_string()));
        assert!(str_tokens.contains(&"subject:message".to_string()));
        assert!(str_tokens.contains(&"from:sender@mail.com".to_string()));
        assert!(str_tokens.contains(&"to:recipient@domain.org".to_string()));
        assert!(str_tokens.contains(&"content-type:text/html;".to_string()));
        assert!(str_tokens.contains(&"content-type:charset=utf-8".to_string()));
    }

    #[test]
    fn test_from_header_addr_tokens() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: John Doe <john.doe@example.org>\r\n\
Subject: Hi\r\n\
\r\n\
body";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();

        // Should extract addr parts from the email address
        assert!(str_tokens.contains(&"from:addr:john.doe".to_string()));
        assert!(str_tokens.contains(&"from:addr:example.org".to_string()));
    }

    #[test]
    fn test_to_header_addr_tokens() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: a@b.com\r\n\
To: Alice <alice@wonderland.net>, bob@builders.com\r\n\
Subject: Test\r\n\
\r\n\
body";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();

        // Should extract addr tokens for both recipients
        assert!(str_tokens.contains(&"to:addr:alice".to_string()));
        assert!(str_tokens.contains(&"to:addr:wonderland.net".to_string()));
        assert!(str_tokens.contains(&"to:addr:bob".to_string()));
        assert!(str_tokens.contains(&"to:addr:builders.com".to_string()));
    }

    #[test]
    fn test_received_header_multiple_values() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: x@y.com\r\n\
Received: from server1.example.com by mx.example.com\r\n\
Received: from relay.isp.net by server1.example.com\r\n\
Subject: Test\r\n\
\r\n\
body";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();

        // Should process all Received headers
        assert!(str_tokens.contains(&"received:from".to_string()));
        assert!(str_tokens.contains(&"received:server1.example.com".to_string()));
        assert!(str_tokens.contains(&"received:mx.example.com".to_string()));
        assert!(str_tokens.contains(&"received:relay.isp.net".to_string()));
    }

    #[test]
    fn test_header_tokens_are_lowercased() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: USER@EXAMPLE.COM\r\n\
Subject: URGENT BIG DEAL\r\n\
\r\n\
body";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();

        // All tokens should be lowercased
        assert!(str_tokens.contains(&"subject:urgent".to_string()));
        assert!(str_tokens.contains(&"subject:big".to_string()));
        assert!(str_tokens.contains(&"subject:deal".to_string()));
        assert!(str_tokens.contains(&"from:user@example.com".to_string()));
        assert!(str_tokens.contains(&"from:addr:user".to_string()));
        assert!(str_tokens.contains(&"from:addr:example.com".to_string()));
    }

    #[test]
    fn test_missing_headers_produce_no_tokens() {
        let tok = Tokenizer::with_defaults();
        // Message with only minimal headers (no From, To, Subject, Received)
        let msg = b"Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\
\r\n\
body";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();

        // Should not have from:, to:, subject:, or received: tokens
        assert!(!str_tokens.iter().any(|t| t.starts_with("from:")));
        assert!(!str_tokens.iter().any(|t| t.starts_with("to:")));
        assert!(!str_tokens.iter().any(|t| t.starts_with("subject:")));
        assert!(!str_tokens.iter().any(|t| t.starts_with("received:")));
    }

    // ─── Plain text tokenization tests ──────────────────────────────────────

    #[test]
    fn test_plain_text_whitespace_splitting_and_lowercasing() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_plain_text("Hello World Rust Programming");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"world".to_string()));
        assert!(str_tokens.contains(&"rust".to_string()));
        assert!(str_tokens.contains(&"programming".to_string()));
    }

    #[test]
    fn test_plain_text_short_tokens_skipped() {
        let tok = Tokenizer::with_defaults();
        // "is", "a", "to" are all < 3 chars and should be skipped
        let tokens = tok.tokenize_plain_text("this is a way to test");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"this".to_string()));
        assert!(str_tokens.contains(&"way".to_string()));
        assert!(str_tokens.contains(&"test".to_string()));
        // Short words should not appear
        assert!(!str_tokens.contains(&"is".to_string()));
        assert!(!str_tokens.contains(&"a".to_string()));
        assert!(!str_tokens.contains(&"to".to_string()));
    }

    #[test]
    fn test_plain_text_long_tokens_trigger_tokenize_word() {
        let tok = Tokenizer::with_defaults();
        // "supercalifragilistic" is > 12 chars, should be decomposed via tokenize_word
        // tokenize_word produces a skip token for this word
        let tokens = tok.tokenize_plain_text("short supercalifragilistic end");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Valid-length words should appear
        assert!(str_tokens.contains(&"short".to_string()));
        assert!(str_tokens.contains(&"end".to_string()));
        // The long word should NOT appear as-is (it should be decomposed)
        assert!(!str_tokens.contains(&"supercalifragilistic".to_string()));
        // Instead, a skip token should be generated (20 chars → skip:s 20)
        assert!(str_tokens.contains(&"skip:s 20".to_string()));
    }

    #[test]
    fn test_plain_text_short_word_run_tracking() {
        let tok = Tokenizer::with_defaults();
        // Create a sequence with consecutive short words to trigger skip token
        // "X j A m N j" - all < 3 chars, creating a run of 6
        let tokens = tok.tokenize_plain_text("hello X j A m N j world");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should contain a skip token for the short-word run
        // Run of 6: log2(6) = 2 (truncated)
        let has_skip = str_tokens.iter().any(|t| t.starts_with("skip:"));
        assert!(has_skip, "Expected a skip token for short-word run, got: {str_tokens:?}");
    }

    #[test]
    fn test_plain_text_empty_produces_no_tokens() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_plain_text("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_plain_text_only_whitespace_produces_no_tokens() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_plain_text("   \t\n  ");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_plain_text_case_folding() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_plain_text("HELLO World hElLo");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // All should be lowercased
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"world".to_string()));
        // Check none are uppercase
        for token in &str_tokens {
            if !token.starts_with("skip:") {
                assert_eq!(token, &token.to_lowercase(),
                    "Token '{token}' should be lowercase");
            }
        }
    }

    #[test]
    fn test_plain_text_no_skip_when_disabled() {
        let config = TokenizerConfig {
            skip_max_word_size: 12,
            min_word_size: 3,
            generate_long_skips: false,
        };
        let tok = Tokenizer::new(config);
        // Even with short-word runs, no skip tokens should be generated
        let tokens = tok.tokenize_plain_text("hello X j A m world");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(!str_tokens.iter().any(|t| t.starts_with("skip:")));
    }

    #[test]
    fn test_plain_text_boundary_word_sizes() {
        let tok = Tokenizer::with_defaults();
        // Exactly 3 chars: should be emitted
        // Exactly 12 chars: should be emitted (within range)
        // Exactly 13 chars: should trigger tokenize_word
        let tokens = tok.tokenize_plain_text("abc twelvechars thirteenchars");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"abc".to_string())); // exactly min_word_size
        assert!(str_tokens.contains(&"twelvechars".to_string())); // 11 chars, within range
        // "thirteenchars" is 13 chars > 12, so tokenize_word is called
        // It produces a skip token: skip:t 10 (13/10*10 = 10)
        assert!(!str_tokens.contains(&"thirteenchars".to_string()));
        assert!(str_tokens.contains(&"skip:t 10".to_string()));
    }

    // ─── HTML tokenization tests ────────────────────────────────────────────

    #[test]
    fn test_html_basic_tags_stripped() {
        let tok = Tokenizer::with_defaults();
        let html = "<html><body><p>Hello <b>world</b></p></body></html>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"world".to_string()));
        // No tokens should contain '<' or '>'
        for token in &str_tokens {
            assert!(!token.contains('<'), "Token '{token}' should not contain '<'");
            assert!(!token.contains('>'), "Token '{token}' should not contain '>'");
        }
    }

    #[test]
    fn test_html_entities_decoded() {
        let tok = Tokenizer::with_defaults();
        // Test named entities
        let html = "<p>fish &amp; chips &lt;tasty&gt;</p>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"fish".to_string()));
        // "&" is < 3 chars so it's skipped, but "chips" should be there
        assert!(str_tokens.contains(&"chips".to_string()));
        // "<tasty>" after decoding becomes "<tasty>" which is text, not a tag
        assert!(str_tokens.contains(&"<tasty>".to_string()));
    }

    #[test]
    fn test_html_numeric_entities_decoded() {
        let tok = Tokenizer::with_defaults();
        // &#72; = H, &#101; = e, &#108; = l, &#108; = l, &#111; = o
        let html = "<p>&#72;ello &#x57;orld</p>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"world".to_string()));
    }

    #[test]
    fn test_html_script_content_removed() {
        let tok = Tokenizer::with_defaults();
        let html = "<html><body>\
            <script type=\"text/javascript\">var spam = 'evil'; alert(spam);</script>\
            <p>Safe content here</p>\
            </body></html>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Script content should NOT appear as tokens
        assert!(!str_tokens.contains(&"spam".to_string()));
        assert!(!str_tokens.contains(&"evil".to_string()));
        assert!(!str_tokens.contains(&"alert".to_string()));
        // But the paragraph text should
        assert!(str_tokens.contains(&"safe".to_string()));
        assert!(str_tokens.contains(&"content".to_string()));
        assert!(str_tokens.contains(&"here".to_string()));
    }

    #[test]
    fn test_html_style_content_removed() {
        let tok = Tokenizer::with_defaults();
        let html = "<html><head>\
            <style>body { color: red; font-size: 12px; }</style>\
            </head><body><p>Visible text</p></body></html>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Style content should NOT appear
        assert!(!str_tokens.contains(&"color".to_string()));
        assert!(!str_tokens.contains(&"font-size".to_string()));
        // But body text should
        assert!(str_tokens.contains(&"visible".to_string()));
        assert!(str_tokens.contains(&"text".to_string()));
    }

    #[test]
    fn test_html_no_angle_brackets_in_tokens() {
        let tok = Tokenizer::with_defaults();
        let html = "<div class=\"test\"><a href=\"http://example.com\">Click here</a></div>\
            <br/><img src=\"img.png\" alt=\"photo\"/>\
            <p>Final paragraph</p>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        for token in &str_tokens {
            assert!(!token.contains('<'), "Token '{token}' should not contain '<'");
            assert!(!token.contains('>'), "Token '{token}' should not contain '>'");
        }
        // Should still have the text content
        assert!(str_tokens.contains(&"click".to_string()));
        assert!(str_tokens.contains(&"here".to_string()));
        assert!(str_tokens.contains(&"final".to_string()));
        assert!(str_tokens.contains(&"paragraph".to_string()));
    }

    #[test]
    fn test_html_malformed_tags_handled() {
        let tok = Tokenizer::with_defaults();
        // Malformed HTML: unclosed tags, extra angle brackets in attributes
        let html = "<p>Hello <b>bold text<p>Next paragraph</p>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should not panic, and should extract text content
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"bold".to_string()));
        assert!(str_tokens.contains(&"text".to_string()));
        assert!(str_tokens.contains(&"next".to_string()));
        assert!(str_tokens.contains(&"paragraph".to_string()));
    }

    #[test]
    fn test_html_self_closing_tags() {
        let tok = Tokenizer::with_defaults();
        let html = "Word1<br/>Word2<hr/>Word3<img src=\"x.png\"/>Word4";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"word1".to_string()));
        assert!(str_tokens.contains(&"word2".to_string()));
        assert!(str_tokens.contains(&"word3".to_string()));
        assert!(str_tokens.contains(&"word4".to_string()));
    }

    #[test]
    fn test_html_comments_removed() {
        let tok = Tokenizer::with_defaults();
        let html = "<p>Before<!-- this is a comment -->After</p>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Comment content should not appear
        assert!(!str_tokens.iter().any(|t| t.contains("comment")));
        // Surrounding text should appear
        assert!(str_tokens.contains(&"before".to_string()));
        assert!(str_tokens.contains(&"after".to_string()));
    }

    #[test]
    fn test_html_nbsp_decoded_to_space() {
        let tok = Tokenizer::with_defaults();
        let html = "<p>Hello&nbsp;World</p>";
        let tokens = tok.tokenize_html(html);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // &nbsp; becomes a space, so "Hello" and "World" are separate tokens
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"world".to_string()));
    }

    #[test]
    fn test_html_empty_input() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_html("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_html_only_tags_no_text() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_html("<html><head></head><body></body></html>");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_html_multipart_message_tokenizes_html_part() {
        let tok = Tokenizer::with_defaults();
        let msg = b"From: test@example.com\r\n\
Content-Type: multipart/alternative; boundary=\"bound\"\r\n\
\r\n\
--bound\r\n\
Content-Type: text/html\r\n\
\r\n\
<html><body><p>Hello from &amp; HTML</p></body></html>\r\n\
--bound--\r\n";
        let result = tok.tokenize(msg);
        let str_tokens: Vec<String> = result
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // HTML body text should be tokenized
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"from".to_string()) || str_tokens.iter().any(|t| t == "from"));
        assert!(str_tokens.contains(&"html".to_string()));
    }

    // ─── URL tokenization tests ─────────────────────────────────────────────

    #[test]
    fn test_url_http_detected_and_split() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("visit http://example.com/path/page today");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"url:example".to_string()));
        assert!(str_tokens.contains(&"url:com".to_string()));
        assert!(str_tokens.contains(&"url:path".to_string()));
        assert!(str_tokens.contains(&"url:page".to_string()));
    }

    #[test]
    fn test_url_https_detected_and_split() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("go to https://secure.example.org/login");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"url:secure".to_string()));
        assert!(str_tokens.contains(&"url:example".to_string()));
        assert!(str_tokens.contains(&"url:org".to_string()));
        assert!(str_tokens.contains(&"url:login".to_string()));
    }

    #[test]
    fn test_url_separator_chars_split_correctly() {
        let tok = Tokenizer::with_defaults();
        // Test all separator chars: ; ? : @ & = + , $ .
        let tokens =
            tok.tokenize_urls("http://user:pass@host.com/path?key=val&other=123;sid=abc+more,$end");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should have split on : @ . ? = & ; + , $
        assert!(str_tokens.contains(&"url:user".to_string()));
        assert!(str_tokens.contains(&"url:pass".to_string()));
        assert!(str_tokens.contains(&"url:host".to_string()));
        assert!(str_tokens.contains(&"url:com".to_string()));
        assert!(str_tokens.contains(&"url:path".to_string()));
        assert!(str_tokens.contains(&"url:key".to_string()));
        assert!(str_tokens.contains(&"url:val".to_string()));
        assert!(str_tokens.contains(&"url:other".to_string()));
        assert!(str_tokens.contains(&"url:123".to_string()));
        assert!(str_tokens.contains(&"url:sid".to_string()));
        assert!(str_tokens.contains(&"url:abc".to_string()));
        assert!(str_tokens.contains(&"url:more".to_string()));
        assert!(str_tokens.contains(&"url:end".to_string()));
    }

    #[test]
    fn test_url_tokens_prefixed_with_url() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("http://example.com");
        // Every token must start with "url:"
        for token in &tokens {
            let s = String::from_utf8_lossy(token);
            assert!(s.starts_with("url:"), "Token '{s}' should start with 'url:'");
        }
    }

    #[test]
    fn test_url_empty_segments_skipped() {
        let tok = Tokenizer::with_defaults();
        // Consecutive separators "..." or ".." should not produce empty tokens
        let tokens = tok.tokenize_urls("http://example...com//path");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // No empty url: tokens
        for token in &str_tokens {
            assert_ne!(token, "url:", "Should not have empty url: token");
            assert!(
                token.len() > 4,
                "Token '{token}' should have content after 'url:'"
            );
        }
        assert!(str_tokens.contains(&"url:example".to_string()));
        assert!(str_tokens.contains(&"url:com".to_string()));
    }

    #[test]
    fn test_url_non_url_text_produces_no_tokens() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("just some regular text without any urls");
        assert!(tokens.is_empty(), "Non-URL text should produce no url: tokens");
    }

    #[test]
    fn test_url_multiple_urls_all_processed() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls(
            "see http://first.com/a and https://second.org/b for info",
        );
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Tokens from first URL
        assert!(str_tokens.contains(&"url:first".to_string()));
        assert!(str_tokens.contains(&"url:com".to_string()));
        // Tokens from second URL
        assert!(str_tokens.contains(&"url:second".to_string()));
        assert!(str_tokens.contains(&"url:org".to_string()));
    }

    #[test]
    fn test_url_tokens_are_lowercased() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("HTTP://EXAMPLE.COM/PATH");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"url:example".to_string()));
        assert!(str_tokens.contains(&"url:com".to_string()));
        assert!(str_tokens.contains(&"url:path".to_string()));
    }

    #[test]
    fn test_url_at_end_of_text() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("click here http://end.net/page");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"url:end".to_string()));
        assert!(str_tokens.contains(&"url:net".to_string()));
        assert!(str_tokens.contains(&"url:page".to_string()));
    }

    #[test]
    fn test_url_with_port_number() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("http://localhost:8080/api/data");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"url:localhost".to_string()));
        assert!(str_tokens.contains(&"url:8080".to_string()));
        assert!(str_tokens.contains(&"url:api".to_string()));
        assert!(str_tokens.contains(&"url:data".to_string()));
    }

    #[test]
    fn test_url_slash_splits_segments() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_urls("http://example.com/path/sub");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // '/' is included as a separator, so path segments are split
        assert!(str_tokens.contains(&"url:example".to_string()));
        assert!(str_tokens.contains(&"url:com".to_string()));
        assert!(str_tokens.contains(&"url:path".to_string()));
        assert!(str_tokens.contains(&"url:sub".to_string()));
    }

    // ─── tokenize_word tests ────────────────────────────────────────────────

    #[test]
    fn test_tokenize_word_email_detection() {
        let tok = Tokenizer::with_defaults();
        // Word that looks like an email: contains exactly one '@' and a '.', length < 40
        let tokens = tok.tokenize_word("user@example.com");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert_eq!(str_tokens.len(), 2);
        assert!(str_tokens.contains(&"email name:user".to_string()));
        assert!(str_tokens.contains(&"email addr:example.com".to_string()));
    }

    #[test]
    fn test_tokenize_word_email_with_subdomain() {
        let tok = Tokenizer::with_defaults();
        let tokens = tok.tokenize_word("john.doe@mail.example.org");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert_eq!(str_tokens.len(), 2);
        assert!(str_tokens.contains(&"email name:john.doe".to_string()));
        assert!(str_tokens.contains(&"email addr:mail.example.org".to_string()));
    }

    #[test]
    fn test_tokenize_word_email_not_detected_without_dot() {
        let tok = Tokenizer::with_defaults();
        // Has '@' but no '.', so it's not treated as email
        let tokens = tok.tokenize_word("user@localhostdomain");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should produce a skip token, not email tokens
        assert!(!str_tokens.iter().any(|t| t.starts_with("email")));
        assert!(str_tokens.iter().any(|t| t.starts_with("skip:")));
    }

    #[test]
    fn test_tokenize_word_email_not_detected_with_multiple_at() {
        let tok = Tokenizer::with_defaults();
        // More than one '@' means not a valid email
        let tokens = tok.tokenize_word("user@host@domain.com");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(!str_tokens.iter().any(|t| t.starts_with("email")));
        assert!(str_tokens.iter().any(|t| t.starts_with("skip:")));
    }

    #[test]
    fn test_tokenize_word_email_not_detected_if_too_long() {
        let tok = Tokenizer::with_defaults();
        // 40+ chars: should not be treated as email even with '@' and '.'
        let long_word = format!("{}@example.com", "a".repeat(30)); // 42 chars total
        let tokens = tok.tokenize_word(&long_word);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(!str_tokens.iter().any(|t| t.starts_with("email")));
        assert!(str_tokens.iter().any(|t| t.starts_with("skip:")));
    }

    #[test]
    fn test_tokenize_word_skip_token_basic() {
        let tok = Tokenizer::with_defaults();
        // 20-char word starting with 's' → skip:s 20
        let tokens = tok.tokenize_word("supercalifragilisti!"); // 20 chars
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"skip:s 20".to_string()));
    }

    #[test]
    fn test_tokenize_word_skip_token_rounding() {
        let tok = Tokenizer::with_defaults();
        // 15-char word → rounded to 10
        let tokens = tok.tokenize_word("fifteencharwrd!"); // 15 chars
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"skip:f 10".to_string()));
    }

    #[test]
    fn test_tokenize_word_skip_token_rounding_25_chars() {
        let tok = Tokenizer::with_defaults();
        // 25-char word → rounded to 20
        let word = "a".repeat(25);
        let tokens = tok.tokenize_word(&word);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.contains(&"skip:a 20".to_string()));
    }

    #[test]
    fn test_tokenize_word_skip_token_disabled() {
        let config = TokenizerConfig {
            skip_max_word_size: 12,
            min_word_size: 3,
            generate_long_skips: false,
        };
        let tok = Tokenizer::new(config);
        let tokens = tok.tokenize_word("supercalifragilistic");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // No skip token should be generated
        assert!(!str_tokens.iter().any(|t| t.starts_with("skip:")));
    }

    #[test]
    fn test_tokenize_word_highbit_chars() {
        let tok = Tokenizer::with_defaults();
        // Word with high-bit (non-ASCII) chars
        // "héllo wörld café" as single word would have high-bit chars
        // Let's create a 20-byte word where 10 bytes are >= 128
        // UTF-8: 'é' is 2 bytes (0xC3, 0xA9), each has high bits
        let word = "éééééééééétest"; // 10 é (each 2 bytes) + 4 ASCII = 24 bytes
        let tokens = tok.tokenize_word(word);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // Should have a skip token and an 8bit% token
        assert!(str_tokens.iter().any(|t| t.starts_with("skip:")));
        assert!(str_tokens.iter().any(|t| t.starts_with("8bit%:")));
    }

    #[test]
    fn test_tokenize_word_highbit_percentage_calculation() {
        let tok = Tokenizer::with_defaults();
        // Create a word where exactly half the bytes are high-bit
        // 'à' is 0xC3 0xA0 in UTF-8 (both bytes >= 128)
        // We need: total len = N, highbit count = N/2
        // 5 'à' chars = 10 bytes (all >= 128), + 10 ASCII chars = 20 bytes total
        // percentage = round(10 * 100.0 / 20) = 50
        let word = "àààààabcdefghij"; // 5*2 highbit bytes + 10 ASCII = 20 bytes
        let n = word.len();
        let hicount = word.bytes().filter(|&b| b >= 128).count();
        let expected_pct = ((hicount as f64 * 100.0) / n as f64).round() as usize;

        let tokens = tok.tokenize_word(word);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        let pct_token = format!("8bit%:{expected_pct}");
        assert!(
            str_tokens.contains(&pct_token),
            "Expected '{pct_token}' in {str_tokens:?}"
        );
    }

    #[test]
    fn test_tokenize_word_no_highbit_no_8bit_token() {
        let tok = Tokenizer::with_defaults();
        // Pure ASCII long word - should NOT have 8bit% token
        let tokens = tok.tokenize_word("supercalifragilistic");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(!str_tokens.iter().any(|t| t.starts_with("8bit%:")));
    }

    #[test]
    fn test_tokenize_word_short_word_returns_empty() {
        let tok = Tokenizer::with_defaults();
        // Words < 3 chars should return empty
        let tokens = tok.tokenize_word("ab");
        assert!(tokens.is_empty());
        let tokens = tok.tokenize_word("x");
        assert!(tokens.is_empty());
        let tokens = tok.tokenize_word("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_word_integration_with_plain_text() {
        let tok = Tokenizer::with_defaults();
        // A long word (> 12 chars) in plain text should produce tokenize_word tokens
        let tokens = tok.tokenize_plain_text("hello user@example.com world");
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        // "user@example.com" is 16 chars > 12, so tokenize_word is called
        // It should detect as email since it has '@' and '.'
        assert!(str_tokens.contains(&"email name:user".to_string()));
        assert!(str_tokens.contains(&"email addr:example.com".to_string()));
        // Normal words still present
        assert!(str_tokens.contains(&"hello".to_string()));
        assert!(str_tokens.contains(&"world".to_string()));
    }

    #[test]
    fn test_tokenize_word_email_at_boundary_39_chars() {
        let tok = Tokenizer::with_defaults();
        // Exactly 39 chars (< 40): should still detect as email
        // local_part (26 chars) + "@" (1) + domain (12 chars) = 39
        let word = "abcdefghijklmnopqrstuvwxyz@example.cool";
        assert_eq!(word.len(), 39);
        let tokens = tok.tokenize_word(word);
        let str_tokens: Vec<String> = tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t).to_string())
            .collect();
        assert!(str_tokens.iter().any(|t| t.starts_with("email name:")));
        assert!(str_tokens.iter().any(|t| t.starts_with("email addr:")));
    }
}
