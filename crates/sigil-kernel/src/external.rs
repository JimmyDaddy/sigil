use std::collections::BTreeSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ModelMessage, canonical_web_url_persistence_projection, safe_persistence_text};

const EXTERNAL_TITLE_MAX_BYTES: usize = 512;
const EXTERNAL_ORIGIN_MAX_BYTES: usize = 64;
const EXTERNAL_TIMESTAMP_MAX_BYTES: usize = 35;
const SHA256_HEX_BYTES: usize = 64;
const EXTERNAL_SESSION_SCOPE_ID_MAX_BYTES: usize = 256;
const EXTERNAL_MESSAGE_ID_MAX_BYTES: usize = 512;
const EXTERNAL_OBSERVED_URL_MAX_BYTES: usize = 8 * 1024;
const EXTERNAL_SAFE_DISPLAY_URL_MAX_BYTES: usize = 2 * 1024;
/// Maximum normalized sources admitted by one durable provenance sidecar.
pub const MAX_EXTERNAL_PROVENANCE_SOURCES: usize = 64;
/// Maximum claim-level citation spans admitted by one durable provenance sidecar.
pub const MAX_EXTERNAL_PROVENANCE_CITATIONS: usize = 256;

/// Trust marker applied to text and evidence that originated outside the local workspace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTrust {
    ExternalUntrusted,
}

/// Whether a live URL capability can survive a process restart.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolRestartPolicy {
    Replayable,
    InterruptOnRestart,
}

/// Fidelity of one normalized external source.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEvidenceLevel {
    SearchSnippet,
    ProviderGroundingSource,
    FetchedPage,
}

/// Freshness claim carried by a normalized source.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceFreshness {
    Fresh,
    Stale,
    #[default]
    Unknown,
}

/// Cache observation carried by a normalized source.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceCacheStatus {
    Hit,
    Miss,
    #[default]
    NotApplicable,
}

/// Secret-safe, provider-neutral evidence describing one external source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExternalSourceRecord {
    pub session_scope_id: String,
    pub source_id: String,
    pub evidence_level: ExternalEvidenceLevel,
    pub safe_display_url: String,
    pub safe_display_url_sha256: String,
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    pub retrieved_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(default)]
    pub freshness: SourceFreshness,
    #[serde(default)]
    pub cache_status: SourceCacheStatus,
    pub url_restart_policy: ToolRestartPolicy,
}

impl ExternalSourceRecord {
    /// Rewrites an optional provider/MCP identifier to a fresh session-local source identifier.
    #[allow(clippy::too_many_arguments)]
    pub fn from_remote_candidate(
        session_scope_id: impl Into<String>,
        remote_source_id: Option<&str>,
        evidence_level: ExternalEvidenceLevel,
        observed_url: impl Into<String>,
        origin: impl Into<String>,
        title: Option<String>,
        published_at: Option<String>,
        retrieved_at: impl Into<String>,
        content_sha256: Option<String>,
        rank: Option<usize>,
        freshness: SourceFreshness,
        cache_status: SourceCacheStatus,
        url_restart_policy: ToolRestartPolicy,
    ) -> Result<Self> {
        let session_scope_id = session_scope_id.into();
        if !valid_bounded_identity(&session_scope_id, EXTERNAL_SESSION_SCOPE_ID_MAX_BYTES) {
            bail!("external source session scope id is invalid or oversized");
        }
        let observed_url = observed_url.into();
        if observed_url.len() > EXTERNAL_OBSERVED_URL_MAX_BYTES {
            bail!("external source observed URL exceeds the transient input limit");
        }
        let url_projection = canonical_web_url_persistence_projection(&observed_url)?;
        if url_projection.restart_policy != url_restart_policy {
            bail!("external source URL restart policy contradicts the observed URL");
        }
        let safe_display_url = url_projection.safe_display_url;
        validate_safe_display_url(&safe_display_url, url_restart_policy)?;
        let origin = origin.into();
        validate_origin(&origin)?;
        validate_optional_timestamp(published_at.as_deref(), "published_at")?;
        let retrieved_at = retrieved_at.into();
        validate_timestamp(&retrieved_at, "retrieved_at")?;
        validate_optional_sha256(content_sha256.as_deref(), "content_sha256")?;
        let source_id = format!("src_{}", Uuid::new_v4().simple());
        let _ = remote_source_id;
        let title = title
            .map(|value| sanitize_external_text(&value, EXTERNAL_TITLE_MAX_BYTES))
            .filter(|value| !value.is_empty());
        Ok(Self {
            session_scope_id,
            source_id,
            evidence_level,
            safe_display_url_sha256: sha256_hex(safe_display_url.as_bytes()),
            safe_display_url,
            origin,
            title,
            published_at,
            retrieved_at,
            content_sha256,
            rank,
            freshness,
            cache_status,
            url_restart_policy,
        })
    }

    /// Validates that this record contains only a local source identity and consistent safe URL.
    pub fn validate(&self) -> Result<()> {
        if !valid_bounded_identity(&self.session_scope_id, EXTERNAL_SESSION_SCOPE_ID_MAX_BYTES) {
            bail!("external source session scope id is invalid or oversized");
        }
        if !is_session_local_source_id(&self.source_id) {
            bail!("external source id is not session-local");
        }
        validate_safe_display_url(&self.safe_display_url, self.url_restart_policy)?;
        if !is_sha256_hex(&self.safe_display_url_sha256)
            || self.safe_display_url_sha256 != sha256_hex(self.safe_display_url.as_bytes())
        {
            bail!("external source safe display URL digest does not match");
        }
        validate_origin(&self.origin)?;
        if let Some(title) = &self.title
            && (title.is_empty()
                || title.len() > EXTERNAL_TITLE_MAX_BYTES
                || sanitize_external_text(title, EXTERNAL_TITLE_MAX_BYTES) != *title)
        {
            bail!("external source title is not in canonical safe-text form");
        }
        validate_optional_timestamp(self.published_at.as_deref(), "published_at")?;
        validate_timestamp(&self.retrieved_at, "retrieved_at")?;
        validate_optional_sha256(self.content_sha256.as_deref(), "content_sha256")?;
        Ok(())
    }
}

/// Claim-level relation between final safe assistant text and one normalized source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CitationSupport {
    pub session_scope_id: String,
    pub message_id: String,
    pub source_id: String,
    pub output_text_sha256: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

impl CitationSupport {
    /// Creates support only for a real, non-empty UTF-8 byte range in final safe text.
    #[must_use]
    pub fn for_final_safe_text(
        session_scope_id: impl Into<String>,
        message_id: impl Into<String>,
        source_id: impl Into<String>,
        final_safe_text: &str,
        start_byte: usize,
        end_byte: usize,
    ) -> Option<Self> {
        let session_scope_id = session_scope_id.into();
        let message_id = message_id.into();
        let source_id = source_id.into();
        if start_byte >= end_byte
            || end_byte > final_safe_text.len()
            || !final_safe_text.is_char_boundary(start_byte)
            || !final_safe_text.is_char_boundary(end_byte)
        {
            return None;
        }
        if !valid_bounded_identity(&session_scope_id, EXTERNAL_SESSION_SCOPE_ID_MAX_BYTES)
            || !valid_bounded_identity(&message_id, EXTERNAL_MESSAGE_ID_MAX_BYTES)
            || !is_session_local_source_id(&source_id)
        {
            return None;
        }
        Some(Self {
            session_scope_id,
            message_id,
            source_id,
            output_text_sha256: sha256_hex(final_safe_text.as_bytes()),
            start_byte,
            end_byte,
        })
    }
}

/// Durable sidecar associated with one safe provider-visible message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExternalProvenanceEntry {
    pub session_scope_id: String,
    pub message_id: String,
    pub trust: ExternalTrust,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ExternalSourceRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationSupport>,
}

impl ExternalProvenanceEntry {
    /// Validates source identity and citation binding against the final persisted message.
    pub fn validate_against_message(&self, message: &ModelMessage) -> Result<()> {
        if !valid_bounded_identity(&self.session_scope_id, EXTERNAL_SESSION_SCOPE_ID_MAX_BYTES)
            || !valid_bounded_identity(&self.message_id, EXTERNAL_MESSAGE_ID_MAX_BYTES)
        {
            bail!("external provenance session or message identity is invalid or oversized");
        }
        if self.message_id != message.id {
            bail!("external provenance message id does not match persisted message");
        }
        let final_text = message.content.as_deref().unwrap_or_default();
        let final_digest = sha256_hex(final_text.as_bytes());
        if self.sources.len() > MAX_EXTERNAL_PROVENANCE_SOURCES {
            bail!("external provenance exceeds the durable source limit");
        }
        if self.citations.len() > MAX_EXTERNAL_PROVENANCE_CITATIONS {
            bail!("external provenance exceeds the durable citation limit");
        }
        let mut source_ids = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if source.session_scope_id != self.session_scope_id {
                bail!("external source belongs to a different session scope");
            }
            if !source_ids.insert(source.source_id.as_str()) {
                bail!("external provenance contains duplicate source ids");
            }
        }
        for citation in &self.citations {
            if citation.session_scope_id != self.session_scope_id
                || citation.message_id != self.message_id
            {
                bail!("citation belongs to a different session or assistant message");
            }
            if !is_session_local_source_id(&citation.source_id)
                || !is_sha256_hex(&citation.output_text_sha256)
            {
                bail!("citation contains an invalid source id or output digest");
            }
            if !source_ids.contains(citation.source_id.as_str()) {
                bail!("citation references an unknown external source id");
            }
            if citation.output_text_sha256 != final_digest
                || citation.start_byte >= citation.end_byte
                || citation.end_byte > final_text.len()
                || !final_text.is_char_boundary(citation.start_byte)
                || !final_text.is_char_boundary(citation.end_byte)
            {
                bail!("citation is not bound to a valid final safe text span");
            }
        }
        Ok(())
    }
}

#[must_use]
pub fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn is_session_local_source_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("src_") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sanitize_external_text(value: &str, max_bytes: usize) -> String {
    let terminal_safe = strip_terminal_control_sequences(value);
    let projected = safe_persistence_text(&terminal_safe);
    let mut safe = String::with_capacity(projected.len().min(max_bytes));
    let mut pending_space = false;
    for character in projected.chars() {
        if is_unsafe_external_control(character) {
            pending_space = !safe.is_empty();
            continue;
        }
        if character.is_whitespace() {
            pending_space = !safe.is_empty();
            continue;
        }
        if pending_space && safe.len() < max_bytes {
            safe.push(' ');
        }
        pending_space = false;
        if safe.len().saturating_add(character.len_utf8()) > max_bytes {
            break;
        }
        safe.push(character);
    }
    safe
}

#[must_use]
pub fn strip_terminal_control_sequences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.next() {
                Some(']') => {
                    while let Some(osc) = characters.next() {
                        if osc == '\u{7}' {
                            break;
                        }
                        if osc == '\u{1b}' && characters.next_if_eq(&'\\').is_some() {
                            break;
                        }
                    }
                }
                Some('[') => {
                    for csi in characters.by_ref() {
                        if ('@'..='~').contains(&csi) {
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }
        if character == '\u{009b}' {
            for csi in characters.by_ref() {
                if ('@'..='~').contains(&csi) {
                    break;
                }
            }
            continue;
        }
        output.push(character);
    }
    output
}

#[must_use]
pub fn is_unsafe_external_control(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn validate_safe_display_url(value: &str, restart_policy: ToolRestartPolicy) -> Result<()> {
    if value.is_empty()
        || value.len() > EXTERNAL_SAFE_DISPLAY_URL_MAX_BYTES
        || value.chars().any(is_unsafe_external_control)
    {
        bail!("external source safe display URL contains unsafe text");
    }
    let projection = canonical_web_url_persistence_projection(value)?;
    let parsed = url::Url::parse(value)?;
    if parsed.as_str() != value {
        bail!("external source safe display URL must use canonical URL serialization");
    }
    if parsed.fragment().is_some() {
        bail!("external source safe display URL must not contain a fragment");
    }
    let redacted_query = parsed.query() == Some("[redacted]");
    if parsed.query().is_some() && !redacted_query {
        bail!("external source safe display URL contains raw query material");
    }
    let redacted_path = parsed.path() == "/[redacted]";
    if parsed.path().contains("[redacted]") && !redacted_path {
        bail!("external source safe display URL has an invalid redacted path");
    }
    if redacted_query || redacted_path {
        if restart_policy != ToolRestartPolicy::InterruptOnRestart {
            bail!("redacted external source URL must interrupt on restart");
        }
    } else if projection.safe_display_url != value || projection.restart_policy != restart_policy {
        bail!("external source safe display URL is not canonical or safely projected");
    }
    Ok(())
}

fn valid_bounded_identity(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(is_unsafe_external_control)
}

fn validate_origin(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > EXTERNAL_ORIGIN_MAX_BYTES
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
    {
        bail!("external source origin must match [a-z0-9][a-z0-9_.-]{{0,63}}");
    }
    Ok(())
}

fn validate_optional_timestamp(value: Option<&str>, field: &str) -> Result<()> {
    if let Some(value) = value {
        validate_timestamp(value, field)?;
    }
    Ok(())
}

fn validate_timestamp(value: &str, field: &str) -> Result<()> {
    if !is_rfc3339_timestamp(value) {
        bail!("external source {field} must be a canonical RFC 3339 timestamp");
    }
    Ok(())
}

fn is_rfc3339_timestamp(value: &str) -> bool {
    if value.len() < 20
        || value.len() > EXTERNAL_TIMESTAMP_MAX_BYTES
        || !value.is_ascii()
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || value.as_bytes().get(10) != Some(&b'T')
        || value.as_bytes().get(13) != Some(&b':')
        || value.as_bytes().get(16) != Some(&b':')
    {
        return false;
    }
    let Some(year) = decimal(&value[0..4]) else {
        return false;
    };
    let (Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        decimal(&value[5..7]),
        decimal(&value[8..10]),
        decimal(&value[11..13]),
        decimal(&value[14..16]),
        decimal(&value[17..19]),
    ) else {
        return false;
    };
    if year == 0
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return false;
    }
    let suffix = &value[19..];
    let timezone = if suffix == "Z" {
        return true;
    } else if let Some(without_fraction) = suffix.strip_prefix('.') {
        let digit_count = without_fraction
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digit_count == 0 || digit_count > 9 {
            return false;
        }
        &without_fraction[digit_count..]
    } else {
        suffix
    };
    if timezone == "Z" {
        return true;
    }
    if timezone.len() != 6
        || !matches!(timezone.as_bytes()[0], b'+' | b'-')
        || timezone.as_bytes()[3] != b':'
    {
        return false;
    }
    matches!(
        (decimal(&timezone[1..3]), decimal(&timezone[4..6])),
        (Some(offset_hour), Some(offset_minute)) if offset_hour <= 23 && offset_minute <= 59
    )
}

fn decimal(value: &str) -> Option<u32> {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| value.parse().ok())
        .flatten()
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn validate_optional_sha256(value: Option<&str>, field: &str) -> Result<()> {
    if value.is_some_and(|value| !is_sha256_hex(value)) {
        bail!("external source {field} must be 64 lowercase hexadecimal bytes");
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == SHA256_HEX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
#[path = "tests/external_provenance_tests.rs"]
mod tests;
