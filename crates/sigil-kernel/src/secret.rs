use std::{fmt, ops::Range, sync::OnceLock};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind, MatchKind};
use serde_json::Value;

use crate::process_environment::SecretString;

pub const REDACTED_SECRET: &str = "[redacted]";

const MAX_SECRET_CARRIERS: usize = 256;
const MAX_SECRET_CARRIER_BYTES: usize = 64 * 1024;

const SECRET_KEY_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "authorization",
    "password",
    "secret",
    "token",
];
const NORMALIZED_SECRET_KEY_MARKERS: &[&str] =
    &["apikey", "authorization", "password", "secret", "token"];

/// Redacts known secret values and common credential-shaped fields before
/// content is shown in UI, logs, tool metadata, or external egress.
pub struct SecretRedactor {
    secrets: Vec<SecretString>,
    saturated: bool,
    matcher: OnceLock<Option<AhoCorasick>>,
    replacement: OnceLock<&'static str>,
}

impl Default for SecretRedactor {
    fn default() -> Self {
        Self {
            secrets: Vec::new(),
            saturated: false,
            matcher: OnceLock::new(),
            replacement: OnceLock::new(),
        }
    }
}

impl Clone for SecretRedactor {
    fn clone(&self) -> Self {
        Self {
            secrets: self.secrets.clone(),
            saturated: self.saturated,
            matcher: OnceLock::new(),
            replacement: OnceLock::new(),
        }
    }
}

impl fmt::Debug for SecretRedactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretRedactor")
            .field("secret_count", &self.secrets.len())
            .field("saturated", &self.saturated)
            .finish()
    }
}

impl PartialEq for SecretRedactor {
    fn eq(&self, other: &Self) -> bool {
        self.secrets == other.secrets && self.saturated == other.saturated
    }
}

impl Eq for SecretRedactor {}

impl SecretRedactor {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_values<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut redactor = Self::default();
        for value in values {
            redactor.add_secret(value);
        }
        redactor
    }

    pub fn add_secret(&mut self, secret: impl AsRef<str>) {
        let trimmed = secret.as_ref().trim();
        if trimmed.chars().count() < 4 {
            return;
        }
        self.insert_secret(SecretString::new(trimmed));
    }

    /// Adds a resolved secret carrier without materializing it as a debug-visible `String`.
    ///
    /// Unlike [`Self::add_secret`], this accepts short non-empty values because an explicitly
    /// granted process credential must never be emitted merely to avoid broad redaction.
    pub fn add_secret_carrier(&mut self, secret: SecretString) {
        if secret.expose_secret().is_empty() {
            return;
        }
        self.insert_secret(secret);
    }

    fn insert_secret(&mut self, secret: SecretString) {
        if self.saturated {
            return;
        }
        if self
            .secrets
            .iter()
            .any(|value| value.expose_secret() == secret.expose_secret())
        {
            return;
        }
        let retained_bytes = self.secrets.iter().fold(0usize, |total, value| {
            total.saturating_add(value.expose_secret().len())
        });
        if self.secrets.len() >= MAX_SECRET_CARRIERS
            || retained_bytes.saturating_add(secret.expose_secret().len())
                > MAX_SECRET_CARRIER_BYTES
        {
            // Never silently stop redacting when configured credentials exceed the bounded
            // multi-pattern matcher budget. The conservative mode hides every non-empty value.
            self.secrets.clear();
            self.saturated = true;
            self.matcher.take();
            self.replacement.take();
            return;
        }
        self.secrets.push(secret);
        self.secrets
            .sort_by_key(|secret| std::cmp::Reverse(secret.expose_secret().len()));
        self.matcher.take();
        self.replacement.take();
    }

    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        if self.saturated {
            return String::new();
        }

        let replacement = self.safe_replacement();
        if replacement.is_empty() && !self.secrets.is_empty() {
            return String::new();
        }
        let mut redacted = vec![0u8; text.len()];
        mark_structural_secrets(text, &mut redacted);
        if !self.secrets.is_empty() {
            let Some(matcher) = self.known_secret_matcher() else {
                return String::new();
            };
            for matched in matcher.find_iter(text) {
                mark_redacted_range(&mut redacted, matched.range());
            }
        }
        let output = render_redacted_mask(text, &redacted, replacement);
        if self
            .known_secret_matcher()
            .is_some_and(|matcher| matcher.is_match(&output))
        {
            replacement.to_owned()
        } else {
            output
        }
    }

    /// Redacts bytes captured from a body that ended at a hard byte boundary.
    ///
    /// The trailing prefix check runs before UTF-8 decoding, so a cap inside a multi-byte secret
    /// cannot turn the final character into a replacement marker and bypass known-value redaction.
    #[must_use]
    pub fn redact_truncated_bytes(&self, bytes: &[u8]) -> String {
        if self.saturated && !bytes.is_empty() {
            return String::new();
        }
        if let Some(prefix_len) = self.longest_trailing_secret_prefix(bytes) {
            let mut redacted = bytes[..bytes.len() - prefix_len].to_vec();
            redacted.extend_from_slice(REDACTED_SECRET.as_bytes());
            return self.redact_text(&String::from_utf8_lossy(&redacted));
        }
        self.redact_text(&String::from_utf8_lossy(bytes))
    }

    /// Redacts independently retained head/tail bytes whose omitted middle may split a secret.
    ///
    /// The head boundary removes the longest known secret prefix at the end of `head`; the tail
    /// boundary removes the longest known secret suffix at the start of `tail`. Full secrets and
    /// credential-shaped text inside either segment are then handled by normal text redaction.
    #[must_use]
    pub fn redact_truncated_head_tail_bytes(&self, head: &[u8], tail: &[u8]) -> (String, String) {
        if self.saturated {
            return (String::new(), String::new());
        }
        let head = if let Some(prefix_len) = self.longest_trailing_secret_prefix(head) {
            let mut redacted = head[..head.len() - prefix_len].to_vec();
            redacted.extend_from_slice(REDACTED_SECRET.as_bytes());
            redacted
        } else {
            head.to_vec()
        };
        let tail = if let Some(suffix_len) = self.longest_leading_secret_suffix(tail) {
            let mut redacted = REDACTED_SECRET.as_bytes().to_vec();
            redacted.extend_from_slice(&tail[suffix_len..]);
            redacted
        } else {
            tail.to_vec()
        };
        (
            self.redact_text(&String::from_utf8_lossy(&head)),
            self.redact_text(&String::from_utf8_lossy(&tail)),
        )
    }

    fn longest_trailing_secret_prefix(&self, bytes: &[u8]) -> Option<usize> {
        self.secrets
            .iter()
            .map(|secret| {
                let pattern = secret.expose_secret().as_bytes();
                let candidate = &bytes[bytes.len().saturating_sub(pattern.len())..];
                longest_pattern_prefix_at_sequence_end(pattern, candidate.iter().copied())
            })
            .filter(|matched| *matched > 0)
            .max()
    }

    fn longest_leading_secret_suffix(&self, bytes: &[u8]) -> Option<usize> {
        self.secrets
            .iter()
            .map(|secret| {
                let pattern = secret.expose_secret().as_bytes();
                let candidate = &bytes[..bytes.len().min(pattern.len())];
                longest_pattern_prefix_at_sequence_end(
                    pattern.iter().rev().copied().collect::<Vec<_>>().as_slice(),
                    candidate.iter().rev().copied(),
                )
            })
            .filter(|matched| *matched > 0)
            .max()
    }

    #[must_use]
    pub fn redact_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.redact_text(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.redact_value(item)).collect())
            }
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, nested)| {
                        let value = if secret_like_key(key) && value_has_non_empty_data(nested) {
                            Value::String(self.safe_replacement().to_owned())
                        } else {
                            self.redact_value(nested)
                        };
                        (self.redact_text(key), value)
                    })
                    .collect(),
            ),
            Value::Bool(value) => {
                let rendered = value.to_string();
                if self.text_contains_secret(&rendered) {
                    Value::String(self.safe_replacement().to_owned())
                } else {
                    Value::Bool(*value)
                }
            }
            Value::Number(value) => {
                if self.text_contains_secret(&value.to_string()) {
                    Value::String(self.safe_replacement().to_owned())
                } else {
                    Value::Number(value.clone())
                }
            }
            Value::Null => Value::Null,
        }
    }

    #[must_use]
    pub fn text_contains_secret(&self, text: &str) -> bool {
        if self.saturated {
            return !text.is_empty();
        }
        self.redact_text(text) != text
    }

    #[must_use]
    pub fn value_contains_secret(&self, value: &Value) -> bool {
        match value {
            Value::String(text) => self.text_contains_secret(text),
            Value::Array(items) => items.iter().any(|item| self.value_contains_secret(item)),
            Value::Object(object) => object.iter().any(|(key, nested)| {
                self.text_contains_secret(key)
                    || (secret_like_key(key) && value_has_non_empty_data(nested))
                    || self.value_contains_secret(nested)
            }),
            Value::Bool(value) => self.text_contains_secret(&value.to_string()),
            Value::Number(value) => self.text_contains_secret(&value.to_string()),
            Value::Null => false,
        }
    }

    fn known_secret_matcher(&self) -> Option<&AhoCorasick> {
        self.matcher
            .get_or_init(|| {
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostLongest)
                    .kind(Some(AhoCorasickKind::NoncontiguousNFA))
                    .build(self.secrets.iter().map(SecretString::expose_secret))
                    .ok()
            })
            .as_ref()
    }

    fn safe_replacement(&self) -> &'static str {
        if self.saturated {
            return "";
        }
        self.replacement.get_or_init(|| {
            [REDACTED_SECRET, "<hidden>", "***"]
                .into_iter()
                .find(|candidate| {
                    self.secrets
                        .iter()
                        .all(|secret| !candidate.contains(secret.expose_secret()))
                })
                .unwrap_or("")
        })
    }
}

fn longest_pattern_prefix_at_sequence_end(
    pattern: &[u8],
    sequence: impl IntoIterator<Item = u8>,
) -> usize {
    if pattern.is_empty() {
        return 0;
    }
    let mut prefix = vec![0usize; pattern.len()];
    for index in 1..pattern.len() {
        let mut matched = prefix[index - 1];
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }

    let mut matched = 0usize;
    let mut sequence = sequence.into_iter().peekable();
    while let Some(byte) = sequence.next() {
        while matched > 0 && byte != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if byte == pattern[matched] {
            matched += 1;
        }
        if matched == pattern.len() && sequence.peek().is_some() {
            matched = prefix[matched - 1];
        }
    }
    matched
}

fn mark_structural_secrets(text: &str, redacted: &mut [u8]) {
    let lower = text.to_ascii_lowercase();
    mark_bearer_secrets(text, &lower, redacted);
    mark_assignment_secrets(text, &lower, redacted);
}

fn mark_bearer_secrets(text: &str, lower: &str, redacted: &mut [u8]) {
    let mut search_start = 0usize;
    while let Some(relative) = lower[search_start..].find("bearer") {
        let bearer_start = search_start + relative;
        let after_marker = bearer_start + "bearer".len();
        search_start = after_marker;
        if !is_key_boundary_before(lower, bearer_start) {
            continue;
        }
        let mut cursor = after_marker;
        while text
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if cursor == after_marker {
            continue;
        }
        let token_end = scan_secret_token_end(text, cursor);
        if cursor < token_end {
            mark_redacted_range(redacted, cursor..token_end);
            search_start = token_end;
        }
    }
}

fn mark_assignment_secrets(text: &str, lower: &str, redacted: &mut [u8]) {
    for marker in SECRET_KEY_MARKERS {
        let mut search_start = 0usize;
        while let Some(relative) = lower[search_start..].find(marker) {
            let marker_start = search_start + relative;
            let after_marker = marker_start + marker.len();
            search_start = after_marker;
            if !is_key_boundary_before(lower, marker_start) {
                continue;
            }
            let mut cursor = after_marker;
            while text
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
            if !matches!(text.as_bytes().get(cursor), Some(b'=') | Some(b':')) {
                continue;
            }
            cursor += 1;
            while text
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
            let Some(first) = text.as_bytes().get(cursor).copied() else {
                continue;
            };
            let value_start;
            let value_end;
            if matches!(first, b'"' | b'\'') {
                value_start = cursor + 1;
                value_end = scan_quoted_secret_end(text, value_start, first);
            } else {
                value_start = cursor;
                value_end = scan_secret_token_end(text, value_start);
            }
            if value_start < value_end {
                mark_redacted_range(redacted, value_start..value_end);
                search_start = value_end;
            }
        }
    }
}

fn mark_redacted_range(redacted: &mut [u8], range: Range<usize>) {
    redacted[range].fill(1);
}

fn scan_quoted_secret_end(text: &str, start: usize, quote: u8) -> usize {
    let mut preceding_backslashes = 0usize;
    for (offset, byte) in text.as_bytes()[start..].iter().copied().enumerate() {
        if byte == quote && preceding_backslashes.is_multiple_of(2) {
            return start + offset;
        }
        if byte == b'\\' {
            preceding_backslashes = preceding_backslashes.saturating_add(1);
        } else {
            preceding_backslashes = 0;
        }
    }
    text.len()
}

fn render_redacted_mask(text: &str, redacted: &[u8], replacement: &str) -> String {
    let mut removed_bytes = 0usize;
    let mut redacted_segments = 0usize;
    let mut inside_redaction = false;
    for marker in redacted {
        if *marker == 0 {
            inside_redaction = false;
        } else {
            removed_bytes = removed_bytes.saturating_add(1);
            if !inside_redaction {
                redacted_segments = redacted_segments.saturating_add(1);
                inside_redaction = true;
            }
        }
    }
    if removed_bytes == 0 {
        return text.to_owned();
    }
    let retained_bytes = text
        .len()
        .saturating_sub(removed_bytes)
        .saturating_add(redacted_segments.saturating_mul(replacement.len()));
    let output_limit = text.len().saturating_add(replacement.len());
    if retained_bytes > output_limit {
        return replacement.to_owned();
    }

    let mut output = String::with_capacity(retained_bytes);
    let mut cursor = 0usize;
    while cursor < redacted.len() {
        let Some(relative_start) = redacted[cursor..].iter().position(|marker| *marker != 0) else {
            break;
        };
        let start = cursor + relative_start;
        let end = redacted[start..]
            .iter()
            .position(|marker| *marker == 0)
            .map_or(redacted.len(), |relative_end| start + relative_end);
        debug_assert!(text.is_char_boundary(start));
        debug_assert!(text.is_char_boundary(end));
        output.push_str(&text[cursor..start]);
        output.push_str(replacement);
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

fn secret_like_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    NORMALIZED_SECRET_KEY_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn value_has_non_empty_data(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items.iter().any(value_has_non_empty_data),
        Value::Object(object) => !object.is_empty(),
    }
}

#[cfg(test)]
fn redact_bearer_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut redacted = vec![0u8; text.len()];
    mark_bearer_secrets(text, &lower, &mut redacted);
    render_redacted_mask(text, &redacted, REDACTED_SECRET)
}

#[cfg(test)]
fn redact_secret_assignments(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut redacted = vec![0u8; text.len()];
    mark_assignment_secrets(text, &lower, &mut redacted);
    render_redacted_mask(text, &redacted, REDACTED_SECRET)
}

fn is_key_boundary_before(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let byte = text.as_bytes()[index - 1];
    !byte.is_ascii_alphanumeric() && byte != b'_' && byte != b'-'
}

fn scan_secret_token_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while let Some(byte) = bytes.get(cursor) {
        if byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b'}' | b']' | b'"' | b'\'') {
            break;
        }
        cursor += 1;
    }
    cursor
}

#[cfg(test)]
#[path = "tests/secret_tests.rs"]
mod tests;
