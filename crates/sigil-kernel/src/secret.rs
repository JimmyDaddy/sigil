use serde_json::Value;

use crate::process_environment::SecretString;

pub const REDACTED_SECRET: &str = "[redacted]";

const SECRET_KEY_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "authorization",
    "password",
    "secret",
    "token",
];

/// Redacts known secret values and common credential-shaped fields before
/// content is shown in UI, logs, tool metadata, or external egress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretRedactor {
    secrets: Vec<SecretString>,
}

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
        if self
            .secrets
            .iter()
            .any(|value| value.expose_secret() == secret.expose_secret())
        {
            return;
        }
        self.secrets.push(secret);
        self.secrets
            .sort_by_key(|secret| std::cmp::Reverse(secret.expose_secret().len()));
    }

    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut redacted = redact_bearer_tokens(text);
        for secret in &self.secrets {
            redacted = redacted.replace(secret.expose_secret(), REDACTED_SECRET);
        }
        redact_secret_assignments(&redacted)
    }

    /// Redacts bytes captured from a body that ended at a hard byte boundary.
    ///
    /// The trailing prefix check runs before UTF-8 decoding, so a cap inside a multi-byte secret
    /// cannot turn the final character into a replacement marker and bypass known-value redaction.
    #[must_use]
    pub fn redact_truncated_bytes(&self, bytes: &[u8]) -> String {
        for secret in &self.secrets {
            let secret = secret.expose_secret().as_bytes();
            for prefix_len in (1..=secret.len()).rev() {
                if bytes.ends_with(&secret[..prefix_len]) {
                    let mut redacted = bytes[..bytes.len() - prefix_len].to_vec();
                    redacted.extend_from_slice(REDACTED_SECRET.as_bytes());
                    return self.redact_text(&String::from_utf8_lossy(&redacted));
                }
            }
        }
        self.redact_text(&String::from_utf8_lossy(bytes))
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
                            Value::String(REDACTED_SECRET.to_owned())
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
                    Value::String(REDACTED_SECRET.to_owned())
                } else {
                    Value::Bool(*value)
                }
            }
            Value::Number(value) => {
                if self.text_contains_secret(&value.to_string()) {
                    Value::String(REDACTED_SECRET.to_owned())
                } else {
                    Value::Number(value.clone())
                }
            }
            Value::Null => Value::Null,
        }
    }

    #[must_use]
    pub fn text_contains_secret(&self, text: &str) -> bool {
        self.secrets
            .iter()
            .any(|secret| text.contains(secret.expose_secret()))
            || self.redact_text(text) != text
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
}

fn secret_like_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    SECRET_KEY_MARKERS.iter().any(|marker| {
        let marker = marker.replace('_', "");
        normalized.contains(&marker)
    })
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

fn redact_bearer_tokens(text: &str) -> String {
    let mut output = text.to_owned();
    let mut search_start = 0usize;
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(relative) = lower[search_start..].find("bearer") else {
            break;
        };
        let bearer_start = search_start + relative;
        let mut cursor = bearer_start + "bearer".len();
        if !is_key_boundary_before(&lower, bearer_start) {
            search_start = cursor;
            continue;
        }
        while output
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            cursor += 1;
        }
        if cursor == bearer_start + "bearer".len() {
            search_start = cursor;
            continue;
        }
        let token_start = cursor;
        let token_end = scan_secret_token_end(&output, token_start);
        if token_start == token_end {
            search_start = token_end;
            continue;
        }
        output.replace_range(token_start..token_end, REDACTED_SECRET);
        search_start = token_start + REDACTED_SECRET.len();
    }
    output
}

fn redact_secret_assignments(text: &str) -> String {
    let mut output = text.to_owned();
    for marker in SECRET_KEY_MARKERS {
        let mut search_start = 0usize;
        loop {
            let lower = output.to_ascii_lowercase();
            let Some(relative) = lower[search_start..].find(marker) else {
                break;
            };
            let marker_start = search_start + relative;
            if !is_key_boundary_before(&lower, marker_start) {
                search_start = marker_start + marker.len();
                continue;
            }
            let mut cursor = marker_start + marker.len();
            while output
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
            if !matches!(output.as_bytes().get(cursor), Some(b'=') | Some(b':')) {
                search_start = cursor;
                continue;
            }
            cursor += 1;
            while output
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
            if cursor >= output.len() {
                break;
            }

            let bytes = output.as_bytes();
            if matches!(bytes.get(cursor), Some(b'"') | Some(b'\'')) {
                let quote = bytes[cursor];
                let value_start = cursor + 1;
                let value_end = bytes[value_start..]
                    .iter()
                    .position(|byte| *byte == quote)
                    .map_or(output.len(), |offset| value_start + offset);
                if value_start < value_end {
                    output.replace_range(value_start..value_end, REDACTED_SECRET);
                    search_start = value_start + REDACTED_SECRET.len();
                } else {
                    search_start = value_end;
                }
            } else {
                let value_start = cursor;
                let value_end = scan_secret_token_end(&output, value_start);
                if value_start < value_end {
                    output.replace_range(value_start..value_end, REDACTED_SECRET);
                    search_start = value_start + REDACTED_SECRET.len();
                } else {
                    search_start = value_end;
                }
            }
        }
    }
    output
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
