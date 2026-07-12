use std::collections::BTreeSet;

use sigil_kernel::{
    ExternalEvidenceLevel, ExternalSourceRecord, SecretRedactor, SecretString, SourceCacheStatus,
    SourceFreshness, ToolRestartPolicy, canonical_web_url_persistence_projection,
    safe_persistence_text, sha256_hex, strip_terminal_control_sequences,
};
use url::Url;

use crate::web_search_connector::{
    SourceProjection, SourceProjectionUnavailableReason, WebSearchResponse,
    WebSearchSourceCapability,
};

pub(crate) const EXA_TEXT_V1_CODEC_ID: &str = "exa_text_v1";
const EXA_ORIGIN: &str = "exa_mcp";
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_RECORDS: usize = 64;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_TITLE_BYTES: usize = 512;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024;
const MAX_SAFE_MODEL_BYTES: usize = 256 * 1024;
const RECORD_SEPARATOR: &str = "\n\n---\n\n";

#[derive(Debug)]
struct ParsedRecord<'a> {
    title: &'a str,
    url: &'a str,
    published: &'a str,
    author: &'a str,
    body_label: &'static str,
    body: &'a str,
}

/// Decodes the release-pinned Exa plain-text envelope. Format drift degrades to safe text only;
/// it never creates source or claim-support records from guessed fields.
pub(crate) fn decode_exa_text_v1(
    raw: &str,
    session_scope_id: &str,
    retrieved_at: &str,
    redactor: &SecretRedactor,
) -> WebSearchResponse {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let recognizable = normalized.starts_with("Title: ") || normalized.contains(RECORD_SEPARATOR);
    let bounded = if normalized.len() <= MAX_INPUT_BYTES {
        normalized.as_str()
    } else {
        utf8_prefix(&normalized, MAX_INPUT_BYTES)
    };
    let safe_fallback = safe_model_text(bounded, redactor, MAX_SAFE_MODEL_BYTES);

    if normalized.len() > MAX_INPUT_BYTES {
        return unavailable(
            safe_fallback,
            SourceProjectionUnavailableReason::CodecFormatDrift,
        );
    }

    let mut sources = Vec::new();
    let mut source_capabilities = Vec::new();
    let mut safe_records = Vec::new();
    let mut seen_urls = BTreeSet::new();
    for (rank, candidate) in normalized
        .split(RECORD_SEPARATOR)
        .take(MAX_RECORDS + 1)
        .enumerate()
    {
        if rank == MAX_RECORDS || candidate.len() > MAX_RECORD_BYTES {
            continue;
        }
        let Some(record) = parse_record(candidate) else {
            continue;
        };
        let Some((canonical_url, restart_policy)) = strict_source_url(record.url) else {
            continue;
        };
        if !seen_urls.insert(canonical_url.clone()) {
            continue;
        }

        let title = safe_single_line(record.title, redactor, MAX_TITLE_BYTES);
        if title.is_empty() {
            continue;
        }
        let body = safe_model_text(record.body, redactor, MAX_BODY_BYTES);
        if body.is_empty() {
            continue;
        }
        let published_at = canonical_published_timestamp(record.published);
        let Ok(source) = ExternalSourceRecord::from_remote_candidate(
            session_scope_id,
            None,
            ExternalEvidenceLevel::SearchSnippet,
            canonical_url.clone(),
            EXA_ORIGIN,
            Some(title.clone()),
            published_at,
            retrieved_at,
            Some(sha256_hex(body.as_bytes())),
            Some(rank),
            SourceFreshness::Unknown,
            SourceCacheStatus::NotApplicable,
            restart_policy,
        ) else {
            continue;
        };
        let author = safe_single_line(record.author, redactor, MAX_TITLE_BYTES);
        safe_records.push(format!(
            "Title: {title}\nURL: {}\nPublished: {}\nAuthor: {author}\n{}:\n{body}",
            source.safe_display_url,
            safe_single_line(record.published, redactor, MAX_TITLE_BYTES),
            record.body_label,
        ));
        source_capabilities.push(WebSearchSourceCapability {
            source_id: source.source_id.clone(),
            raw_canonical_url: SecretString::new(canonical_url),
            safe_display_url: source.safe_display_url.clone(),
            restart_policy,
        });
        sources.push(source);
    }

    if sources.is_empty() {
        return unavailable(
            safe_fallback,
            if recognizable {
                SourceProjectionUnavailableReason::NoValidRecords
            } else {
                SourceProjectionUnavailableReason::CodecFormatDrift
            },
        );
    }

    WebSearchResponse {
        safe_model_content: safe_records.join(RECORD_SEPARATOR),
        source_projection: SourceProjection::Structured {
            codec_id: EXA_TEXT_V1_CODEC_ID.to_owned(),
            valid_records: sources.len(),
        },
        sources,
        source_capabilities,
    }
}

fn parse_record(value: &str) -> Option<ParsedRecord<'_>> {
    let (title, rest) = value.strip_prefix("Title: ")?.split_once("\nURL: ")?;
    let (url, rest) = rest.split_once("\nPublished: ")?;
    let (published, rest) = rest.split_once("\nAuthor: ")?;
    let (author, body_label, body) =
        if let Some((author, body)) = rest.split_once("\nHighlights:\n") {
            (author, "Highlights", body)
        } else {
            let (author, body) = rest.split_once("\nText: ")?;
            (author, "Text", body)
        };
    if title.is_empty()
        || title.len() > MAX_TITLE_BYTES
        || url.is_empty()
        || url.len() > MAX_URL_BYTES
        || published.len() > MAX_TITLE_BYTES
        || author.len() > MAX_TITLE_BYTES
        || body.is_empty()
        || body.len() > MAX_BODY_BYTES
        || [title, url, published, author]
            .iter()
            .any(|value| value.contains('\n'))
    {
        return None;
    }
    Some(ParsedRecord {
        title,
        url,
        published,
        author,
        body_label,
        body,
    })
}

fn strict_source_url(value: &str) -> Option<(String, ToolRestartPolicy)> {
    let parsed = Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let canonical = parsed.to_string();
    let projection = canonical_web_url_persistence_projection(&canonical).ok()?;
    Some((canonical, projection.restart_policy))
}

fn canonical_published_timestamp(value: &str) -> Option<String> {
    if value.len() < 20 || value.len() > 35 || value.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    Some(value.to_owned())
}

fn unavailable(
    safe_model_content: String,
    reason: SourceProjectionUnavailableReason,
) -> WebSearchResponse {
    WebSearchResponse {
        safe_model_content,
        sources: Vec::new(),
        source_capabilities: Vec::new(),
        source_projection: SourceProjection::Unavailable { reason },
    }
}

fn safe_model_text(value: &str, redactor: &SecretRedactor, max_bytes: usize) -> String {
    let stripped = strip_terminal_control_sequences(value);
    let redacted = redactor.redact_text(&stripped);
    let projected = safe_persistence_text(&redacted);
    utf8_prefix(&projected, max_bytes).trim().to_owned()
}

fn safe_single_line(value: &str, redactor: &SecretRedactor, max_bytes: usize) -> String {
    safe_model_text(value, redactor, max_bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
#[path = "tests/exa_text_v1_tests.rs"]
mod tests;
