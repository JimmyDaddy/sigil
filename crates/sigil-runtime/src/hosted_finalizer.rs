use std::collections::BTreeMap;

use async_trait::async_trait;
use sigil_kernel::{
    ExternalEvidenceLevel, ExternalSourceRecord, FinalizedHostedCitation, FinalizedHostedTurn,
    HostedEvidence, HostedEvidenceProcessor, HostedFinalizationContext, HostedToolTerminalStatus,
    HostedTurnBuffer, HostedTurnError, SourceCacheStatus, SourceFreshness,
    canonical_web_url_persistence_projection, safe_persistence_text,
};

/// Runtime-owned normalizer for provider-hosted evidence.
#[derive(Debug, Clone)]
pub struct HostedEvidenceFinalizer {
    retrieved_at: String,
}

impl HostedEvidenceFinalizer {
    #[must_use]
    pub fn new(retrieved_at: impl Into<String>) -> Self {
        Self {
            retrieved_at: retrieved_at.into(),
        }
    }
}

/// Maps a successful provider response to the unique durable hosted terminal status.
#[must_use]
pub fn hosted_terminal_status(finalized: &FinalizedHostedTurn) -> HostedToolTerminalStatus {
    if finalized.hosted_used {
        HostedToolTerminalStatus::Observed
    } else {
        HostedToolTerminalStatus::NotUsed
    }
}

#[async_trait]
impl HostedEvidenceProcessor for HostedEvidenceFinalizer {
    async fn finalize(
        &self,
        context: HostedFinalizationContext,
        buffer: &HostedTurnBuffer,
    ) -> Result<FinalizedHostedTurn, HostedTurnError> {
        let safe_text = safe_persistence_text(buffer.text());
        let safe_reasoning = safe_persistence_text(buffer.reasoning());
        let mut sources = Vec::new();
        let mut source_ids = BTreeMap::new();
        let mut citation_candidates = Vec::new();
        let mut query_observed = false;

        for evidence in buffer.evidence() {
            match evidence {
                HostedEvidence::Source(candidate) => {
                    if source_ids.contains_key(candidate.provider_source_id()) {
                        return Err(HostedTurnError::FinalizationFailed);
                    }
                    let projection = canonical_web_url_persistence_projection(candidate.raw_url())
                        .map_err(|_| HostedTurnError::FinalizationFailed)?;
                    let source = ExternalSourceRecord::from_remote_candidate(
                        context.session_scope_id.clone(),
                        Some(candidate.provider_source_id()),
                        ExternalEvidenceLevel::ProviderGroundingSource,
                        candidate.raw_url().to_owned(),
                        "provider_hosted",
                        candidate.raw_title().map(str::to_owned),
                        candidate.published_at().map(str::to_owned),
                        self.retrieved_at.clone(),
                        None,
                        candidate.rank(),
                        SourceFreshness::Unknown,
                        SourceCacheStatus::NotApplicable,
                        projection.restart_policy,
                    )
                    .map_err(|_| HostedTurnError::FinalizationFailed)?;
                    source_ids.insert(
                        candidate.provider_source_id().to_owned(),
                        source.source_id.clone(),
                    );
                    sources.push(source);
                }
                HostedEvidence::Citation(candidate) => citation_candidates.push(candidate),
                HostedEvidence::QueryObserved(_) => query_observed = true,
            }
        }

        let citations = citation_candidates
            .into_iter()
            .filter_map(|candidate| {
                let source_id = source_ids.get(candidate.provider_source_id())?.clone();
                let (start_byte, end_byte) = map_safe_text_offsets(
                    buffer.text(),
                    &safe_text,
                    candidate.start_byte(),
                    candidate.end_byte(),
                )?;
                Some(FinalizedHostedCitation {
                    source_id,
                    start_byte,
                    end_byte,
                })
            })
            .collect();

        Ok(FinalizedHostedTurn {
            assistant_text: safe_text,
            reasoning_trace: safe_reasoning,
            sources,
            citations,
            hosted_used: buffer.hosted_used(),
            query_observed,
        })
    }
}

fn map_safe_text_offsets(
    raw_text: &str,
    safe_text: &str,
    raw_start: usize,
    raw_end: usize,
) -> Option<(usize, usize)> {
    if raw_start >= raw_end
        || raw_end > raw_text.len()
        || !raw_text.is_char_boundary(raw_start)
        || !raw_text.is_char_boundary(raw_end)
    {
        return None;
    }
    let safe_prefix = safe_persistence_text(&raw_text[..raw_start]);
    let safe_through_span = safe_persistence_text(&raw_text[..raw_end]);
    if !safe_text.starts_with(&safe_prefix)
        || !safe_text.starts_with(&safe_through_span)
        || safe_prefix.len() >= safe_through_span.len()
    {
        return None;
    }
    let safe_span = safe_persistence_text(&raw_text[raw_start..raw_end]);
    let projected_span = safe_text.get(safe_prefix.len()..safe_through_span.len())?;
    (projected_span == safe_span).then_some((safe_prefix.len(), safe_through_span.len()))
}

#[cfg(test)]
#[path = "tests/hosted_finalizer_tests.rs"]
mod tests;
