use std::collections::BTreeMap;

use anyhow::{Result, bail};
use sigil_kernel::{
    HostedCitationCandidate, HostedEvidence, HostedSourceCandidate, HostedToolKind,
    HostedToolRequest, SecretString,
};

use crate::models::{GeminiGroundingMetadata, GeminiGroundingSupport};

const MAX_GROUNDING_QUERIES: usize = 32;
const MAX_GROUNDING_CHUNKS: usize = 256;
const MAX_GROUNDING_SUPPORTS: usize = 512;
const MAX_QUERY_BYTES: usize = 8 * 1024;
const MAX_SOURCE_URL_BYTES: usize = 16 * 1024;
const MAX_SOURCE_TITLE_BYTES: usize = 8 * 1024;
const MAX_TRACKED_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct GeminiHostedInvocation {
    pub(crate) authorization_id: String,
    pub(crate) invocation_id: String,
}

pub(crate) fn hosted_invocation(
    requests: &[HostedToolRequest],
) -> Result<Option<GeminiHostedInvocation>> {
    if requests.is_empty() {
        return Ok(None);
    }
    if requests.len() != 1 {
        bail!("Gemini accepts exactly one hosted web-search request per provider request");
    }
    let request = &requests[0];
    request.validate()?;
    if request.kind != HostedToolKind::WebSearch {
        bail!("Gemini received an unsupported hosted tool kind");
    }
    if request.limits.max_uses.is_some()
        || !request.limits.allowed_domains.is_empty()
        || !request.limits.blocked_domains.is_empty()
    {
        bail!("Gemini google_search does not enforce requested hosted-tool limits");
    }
    Ok(Some(GeminiHostedInvocation {
        authorization_id: request.authorization_id.clone(),
        invocation_id: format!("gemini:{}", request.request_fingerprint),
    }))
}

/// Returns whether one exact Gemini model id is documented to support
/// Grounding with Google Search.
///
/// Unknown aliases and versioned variants fail closed. The optional `models/`
/// resource prefix is transport syntax and does not change model eligibility.
#[must_use]
pub(crate) fn gemini_hosted_web_search_supported(model_name: &str) -> bool {
    let model_name = exact_model_name(model_name);
    matches!(
        model_name,
        "gemini-3.5-flash"
            | "gemini-3.1-flash-image-preview"
            | "gemini-3.1-pro-preview"
            | "gemini-3-pro-image-preview"
            | "gemini-3-flash-preview"
            | "gemini-2.5-pro"
            | "gemini-2.5-flash"
            | "gemini-2.5-flash-lite"
            | "gemini-2.0-flash"
    )
}

/// Gemini 3 models document support for combining built-in tools with custom
/// function declarations. Earlier exact models remain fail-closed for this mix.
#[must_use]
pub(crate) fn gemini_hosted_custom_tools_supported(model_name: &str) -> bool {
    matches!(
        exact_model_name(model_name),
        "gemini-3.5-flash"
            | "gemini-3.1-flash-image-preview"
            | "gemini-3.1-pro-preview"
            | "gemini-3-pro-image-preview"
            | "gemini-3-flash-preview"
    )
}

fn exact_model_name(model_name: &str) -> &str {
    model_name
        .trim()
        .strip_prefix("models/")
        .unwrap_or(model_name.trim())
}

/// Provider-private accumulator for grounding evidence across streaming envelopes.
///
/// Gemini documents `groundingChunks` as incremental while support indices address
/// the accumulated chunk list. Text ranges are byte offsets within one response
/// part, so each streamed part is projected back into the concatenated provider
/// text emitted to kernel. Raw query/source values are immediately moved into
/// `SecretString` carriers with no serde contract and redacted `Debug`.
pub(crate) struct GeminiGroundingAccumulator {
    output_bytes: usize,
    parts: BTreeMap<(usize, usize), PartProjection>,
    source_ids: Vec<Option<String>>,
    observed_queries: Vec<SecretString>,
    support_count: usize,
}

impl GeminiGroundingAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            output_bytes: 0,
            parts: BTreeMap::new(),
            source_ids: Vec::new(),
            observed_queries: Vec::new(),
            support_count: 0,
        }
    }

    pub(crate) fn record_text(
        &mut self,
        candidate_index: usize,
        part_index: usize,
        text: &str,
    ) -> Result<()> {
        let next_output_bytes = self.output_bytes.saturating_add(text.len());
        if next_output_bytes > MAX_TRACKED_OUTPUT_BYTES {
            bail!("Gemini hosted response text exceeded its transient byte limit");
        }
        let part = self.parts.entry((candidate_index, part_index)).or_default();
        let local_start = part.text.len();
        part.text.push_str(text);
        part.ranges.push(PartRangeProjection {
            local_start,
            local_end: part.text.len(),
            output_start: self.output_bytes,
            output_end: next_output_bytes,
        });
        self.output_bytes = next_output_bytes;
        Ok(())
    }

    pub(crate) fn map_metadata(
        &mut self,
        candidate_index: usize,
        metadata: GeminiGroundingMetadata,
    ) -> Result<Vec<HostedEvidence>> {
        if self
            .observed_queries
            .len()
            .saturating_add(metadata.web_search_queries.len())
            > MAX_GROUNDING_QUERIES
        {
            bail!("Gemini grounding query count exceeded its transient limit");
        }
        if self
            .source_ids
            .len()
            .saturating_add(metadata.grounding_chunks.len())
            > MAX_GROUNDING_CHUNKS
        {
            bail!("Gemini grounding source count exceeded its transient limit");
        }
        if self
            .support_count
            .saturating_add(metadata.grounding_supports.len())
            > MAX_GROUNDING_SUPPORTS
        {
            bail!("Gemini grounding support count exceeded its transient limit");
        }

        let mut evidence = Vec::new();
        for query in metadata.web_search_queries {
            if query.expose_secret().len() > MAX_QUERY_BYTES {
                bail!("Gemini grounding query exceeded its transient byte limit");
            }
            if !self
                .observed_queries
                .iter()
                .any(|seen| seen.expose_secret() == query.expose_secret())
            {
                self.observed_queries.push(query.clone());
                evidence.push(HostedEvidence::QueryObserved(query));
            }
        }

        for chunk in metadata.grounding_chunks {
            let source_index = self.source_ids.len();
            let source_id = chunk.web.and_then(|web| {
                let uri = web.uri?.expose_secret().trim().to_owned();
                if uri.is_empty() || uri.len() > MAX_SOURCE_URL_BYTES {
                    return None;
                }
                let title = web.title.and_then(|title| {
                    (title.expose_secret().len() <= MAX_SOURCE_TITLE_BYTES)
                        .then(|| title.expose_secret().to_owned())
                });
                let source_id = format!("gemini-grounding-chunk-{source_index}");
                evidence.push(HostedEvidence::Source(
                    HostedSourceCandidate::new(source_id.clone(), uri, title)
                        .with_rank(source_index),
                ));
                Some(source_id)
            });
            self.source_ids.push(source_id);
        }

        self.support_count = self
            .support_count
            .saturating_add(metadata.grounding_supports.len());
        for support in metadata.grounding_supports {
            evidence.extend(self.map_support(candidate_index, support));
        }
        Ok(evidence)
    }

    fn map_support(
        &self,
        candidate_index: usize,
        support: GeminiGroundingSupport,
    ) -> Vec<HostedEvidence> {
        let Some(segment) = support.segment else {
            return Vec::new();
        };
        let Some(start) = segment.start_index else {
            return Vec::new();
        };
        let Some(end) = segment.end_index else {
            return Vec::new();
        };
        let Some(part) = self.parts.get(&(candidate_index, segment.part_index)) else {
            return Vec::new();
        };
        let Some((output_start, output_end)) = part.map_range(start, end) else {
            return Vec::new();
        };
        if segment
            .text
            .as_ref()
            .is_some_and(|expected| part.text.get(start..end) != Some(expected.expose_secret()))
        {
            return Vec::new();
        }

        support
            .grounding_chunk_indices
            .into_iter()
            .filter_map(|index| self.source_ids.get(index)?.as_ref())
            .map(|source_id| {
                HostedEvidence::Citation(HostedCitationCandidate::new(
                    source_id.clone(),
                    output_start,
                    output_end,
                ))
            })
            .collect()
    }
}

#[derive(Default)]
struct PartProjection {
    text: String,
    ranges: Vec<PartRangeProjection>,
}

struct PartRangeProjection {
    local_start: usize,
    local_end: usize,
    output_start: usize,
    output_end: usize,
}

impl PartProjection {
    fn map_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start >= end
            || end > self.text.len()
            || !self.text.is_char_boundary(start)
            || !self.text.is_char_boundary(end)
        {
            return None;
        }
        let first_index = self
            .ranges
            .iter()
            .position(|range| start >= range.local_start && start < range.local_end)?;
        let last_index = self
            .ranges
            .iter()
            .position(|range| end > range.local_start && end <= range.local_end)?;
        let relevant = self.ranges.get(first_index..=last_index)?;
        if relevant.windows(2).any(|pair| {
            pair[0].local_end != pair[1].local_start || pair[0].output_end != pair[1].output_start
        }) {
            return None;
        }
        let first = relevant.first()?;
        let last = relevant.last()?;
        Some((
            first.output_start.saturating_add(start - first.local_start),
            last.output_start.saturating_add(end - last.local_start),
        ))
    }
}

#[cfg(test)]
#[path = "tests/hosted_search_tests.rs"]
mod tests;
