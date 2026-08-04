use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;

/// Current schema for a recoverable old tool-output shrink descriptor.
pub const TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION: u16 = 2;
/// Current schema for an in-memory recoverable shrink candidate.
pub const RECOVERABLE_TOOL_OUTPUT_SHRINK_CANDIDATE_SCHEMA_VERSION: u16 = 1;
/// A bounded plan avoids materializing an unbounded number of old outputs in one projection.
pub const MAX_TOOL_OUTPUT_PROJECTION_SHRINKS: usize = 128;

/// Bounded head/tail policy for an already-completed historical tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputProjectionPolicy {
    /// Maximum bytes for the rendered tool-result `content` field, including the marker.
    pub max_projected_content_bytes: usize,
    /// Preferred byte cap for the retained UTF-8 head.
    pub retained_head_bytes: usize,
    /// Preferred byte cap for the retained UTF-8 tail.
    pub retained_tail_bytes: usize,
}

impl Default for ToolOutputProjectionPolicy {
    fn default() -> Self {
        Self {
            max_projected_content_bytes: 8 * 1024,
            retained_head_bytes: 4 * 1024,
            retained_tail_bytes: 4 * 1024,
        }
    }
}

impl ToolOutputProjectionPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_projected_content_bytes < 256 {
            bail!("tool-output projection limit must leave room for its structured marker");
        }
        if self.retained_head_bytes == 0 || self.retained_tail_bytes == 0 {
            bail!("tool-output projection head and tail limits must be non-zero");
        }
        Ok(())
    }
}

/// Source event plus the real opaque artifact capability for a projected V2 tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ToolOutputProjectionSourceRef {
    PublishedArtifact {
        source_event_id: crate::EventId,
        artifact_ref: ToolArtifactRefV1,
    },
}

/// Why a completed tool output is eligible for a next-epoch bounded preview.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputShrinkReasonV1 {
    LargeCompletedHistoricalResult,
}

/// Session-owned durable reference used to retrieve a projected V2 tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ToolOutputArtifactRefV1 {
    PublishedArtifact {
        artifact_ref: ToolArtifactRefV1,
        content_sha256: String,
    },
}

/// Metadata proving how one completed historical tool result was projection-shrunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputProjectionShrink {
    pub schema_version: u16,
    pub source_event: CompactionEventRef,
    pub tool_call_id: String,
    /// SHA-256 of the complete raw persisted tool message content before projection.
    pub source_message_sha256: String,
    /// Original byte length of the result's model-visible `content` field.
    pub original_content_bytes: u64,
    pub retained_head_bytes: u64,
    pub retained_tail_bytes: u64,
    pub omitted_bytes: u64,
    pub source_ref: ToolOutputProjectionSourceRef,
    /// V2 metadata is deterministic and contains no raw output excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_token_upper_bound: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ToolOutputArtifactRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ToolOutputShrinkReasonV1>,
}

impl ToolOutputProjectionShrink {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION
            || self.source_event.stream_sequence == 0
            || self.source_event.event_id.trim().is_empty()
            || self.tool_call_id.trim().is_empty()
            || !is_sha256(&self.source_message_sha256)
            || self.original_content_bytes == 0
            || self.retained_head_bytes == 0
            || self.retained_tail_bytes == 0
            || self.omitted_bytes == 0
        {
            bail!("tool-output projection shrink metadata is invalid");
        }
        let total = self
            .retained_head_bytes
            .checked_add(self.retained_tail_bytes)
            .and_then(|value| value.checked_add(self.omitted_bytes))
            .ok_or_else(|| anyhow::anyhow!("tool-output projection byte metadata overflowed"))?;
        if total != self.original_content_bytes {
            bail!("tool-output projection byte metadata does not cover original content");
        }
        match &self.source_ref {
            ToolOutputProjectionSourceRef::PublishedArtifact {
                source_event_id,
                artifact_ref,
            } if source_event_id == &self.source_event.event_id => {
                artifact_ref.validate()?;
            }
            ToolOutputProjectionSourceRef::PublishedArtifact { .. } => {
                bail!("tool-output projection source ref does not match its source event");
            }
        }
        let tool_name = self
            .tool_name
            .as_deref()
            .context("tool-output projection tool name is missing")?;
        let status = self
            .status
            .as_deref()
            .context("tool-output projection status is missing")?;
        let content_sha256 = self
            .content_sha256
            .as_deref()
            .context("tool-output projection content hash is missing")?;
        let content_token_upper_bound = self
            .content_token_upper_bound
            .context("tool-output projection token upper bound is missing")?;
        let artifact_ref = self
            .artifact_ref
            .as_ref()
            .context("tool-output projection artifact ref is missing")?;
        if tool_name.trim().is_empty()
            || tool_name.len() > 256
            || status.trim().is_empty()
            || status.len() > 64
            || !is_sha256(content_sha256)
            || content_token_upper_bound == 0
            || self.reason.is_none()
        {
            bail!("recoverable tool-output projection metadata is invalid");
        }
        match artifact_ref {
            ToolOutputArtifactRefV1::PublishedArtifact {
                artifact_ref: candidate_ref,
                content_sha256: artifact_hash,
            } if artifact_hash == content_sha256 => {
                candidate_ref.validate()?;
                let ToolOutputProjectionSourceRef::PublishedArtifact {
                    artifact_ref: source_ref,
                    ..
                } = &self.source_ref;
                if candidate_ref != source_ref {
                    bail!("tool-output projection artifact refs do not match");
                }
            }
            ToolOutputArtifactRefV1::PublishedArtifact { .. } => {
                bail!("tool-output projection artifact ref does not bind its source");
            }
        }
        Ok(())
    }
}

/// Explicit raw/visible/candidate lifecycle for one tool output during V3 prepare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverableToolOutputShrinkCandidateV1 {
    pub schema_version: u16,
    pub raw_durable_result: ToolOutputArtifactRefV1,
    /// Hash of the immutable bytes still visible in the current epoch.
    pub epoch_visible_source_message_sha256: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub status: String,
    pub original_content_bytes: u64,
    pub original_content_token_upper_bound: u64,
    pub head_excerpt: String,
    pub tail_excerpt: String,
    pub content_sha256: String,
    pub reason: ToolOutputShrinkReasonV1,
    pub recovery_instruction: String,
}

/// One model-visible replacement derived from, but never written over, a raw tool result.
#[derive(Debug, Clone)]
pub struct ProjectedToolOutput {
    pub shrink: ToolOutputProjectionShrink,
    pub candidate: RecoverableToolOutputShrinkCandidateV1,
    pub message: crate::ModelMessage,
}

/// Read-only projection of eligible old tool outputs from one exact safe-fold plan.
#[derive(Debug, Clone, Default)]
pub struct ToolOutputProjection {
    pub outputs: Vec<ProjectedToolOutput>,
}

impl ToolOutputProjection {
    /// Builds bounded model-visible replacements for old, completed tool results in `plan`.
    ///
    /// The complete durable stream is revalidated through [`CompactionFoldPlan`]. Only an
    /// artifact-backed `ToolResultV2` event already in the folded set can shrink. Invalid inline
    /// records fail session loading before this projection and are never treated as recoverable.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan is stale, the stream is malformed, an eligible event no
    /// longer resolves to a tool result, or the bounded descriptor cannot be rendered truthfully.
    pub fn from_fold_plan(
        records: &[SessionStreamRecord],
        plan: &CompactionFoldPlan,
        policy: &ToolOutputProjectionPolicy,
    ) -> Result<Self> {
        policy.validate()?;
        plan.validate_against(records)?;

        let by_id = records
            .iter()
            .map(|record| {
                let event = record.stored_event();
                (event.event_id.as_str(), event)
            })
            .collect::<BTreeMap<_, _>>();
        let mut outputs = Vec::new();
        for event_id in &plan.folded_event_ids {
            let event = by_id
                .get(event_id.as_str())
                .copied()
                .expect("validated fold plan references its current durable stream");
            let Some(SessionLogEntry::ToolResultV3(result)) =
                session_entry_from_stored_event(event)?
            else {
                continue;
            };
            let Some(projected) = project_tool_result_v2(event_ref(event), &result, policy)? else {
                continue;
            };
            if outputs.len() == MAX_TOOL_OUTPUT_PROJECTION_SHRINKS {
                bail!("too many old tool outputs for one bounded projection");
            }
            outputs.push(projected);
        }
        Ok(Self { outputs })
    }
}

fn project_tool_result_v2(
    source_event: CompactionEventRef,
    result: &ToolResultRecordedV3,
    policy: &ToolOutputProjectionPolicy,
) -> Result<Option<ProjectedToolOutput>> {
    result.validate()?;
    let Some(descriptor) = result.artifact.descriptor() else {
        return Ok(None);
    };
    let raw_message_content = result.model_content()?;
    let source_message_sha256 = format!(
        "sha256:{:x}",
        Sha256::digest(raw_message_content.as_bytes())
    );
    let content = result.initial_model_view.preview.as_str();
    if content.len() <= policy.max_projected_content_bytes {
        return Ok(None);
    }
    let content_sha256 = descriptor.content_sha256.clone();
    let artifact_ref = ToolOutputArtifactRefV1::PublishedArtifact {
        artifact_ref: descriptor.artifact_ref.clone(),
        content_sha256: content_sha256.clone(),
    };
    let (head_bytes, tail_bytes, marker) =
        marker_budget(content, &descriptor.artifact_ref, policy)?;
    let head_end = previous_char_boundary(content, head_bytes);
    let tail_start =
        next_char_boundary(content, content.len().saturating_sub(tail_bytes)).max(head_end);
    let retained_head_bytes = head_end as u64;
    let retained_tail_bytes = content.len().saturating_sub(tail_start) as u64;
    let omitted_bytes = tail_start.saturating_sub(head_end) as u64;
    if omitted_bytes == 0 {
        return Ok(None);
    }
    let head_excerpt = content[..head_end].to_owned();
    let tail_excerpt = content[tail_start..].to_owned();

    let shrink = ToolOutputProjectionShrink {
        schema_version: TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION,
        source_event: source_event.clone(),
        tool_call_id: result.call_id.clone(),
        source_message_sha256,
        original_content_bytes: content.len() as u64,
        retained_head_bytes,
        retained_tail_bytes,
        omitted_bytes,
        source_ref: ToolOutputProjectionSourceRef::PublishedArtifact {
            source_event_id: source_event.event_id.clone(),
            artifact_ref: descriptor.artifact_ref.clone(),
        },
        tool_name: Some(result.tool_name.clone()),
        status: Some(result.facts.status.clone()),
        content_sha256: Some(content_sha256.clone()),
        content_token_upper_bound: Some(content.len() as u64),
        artifact_ref: Some(artifact_ref.clone()),
        reason: Some(ToolOutputShrinkReasonV1::LargeCompletedHistoricalResult),
    };
    shrink.validate_shape()?;
    let mut rendered = String::with_capacity(head_end + marker.len() + content.len() - tail_start);
    rendered.push_str(&content[..head_end]);
    rendered.push_str(&marker);
    rendered.push_str(&content[tail_start..]);
    debug_assert!(rendered.len() <= policy.max_projected_content_bytes);
    let mut projected_view = result.initial_model_view.clone();
    projected_view.preview = rendered;
    projected_view.preview_kind = ToolPreviewKind::HeadTail;
    projected_view.token_upper_bound = projected_view.preview.len().div_ceil(4) as u64;
    projected_view.retrieval_hint =
        Some("use read_tool_artifact with the opaque artifact_ref".to_owned());
    let message = result.model_message_with_view(&projected_view)?;
    let candidate = RecoverableToolOutputShrinkCandidateV1 {
        schema_version: RECOVERABLE_TOOL_OUTPUT_SHRINK_CANDIDATE_SCHEMA_VERSION,
        raw_durable_result: artifact_ref,
        epoch_visible_source_message_sha256: shrink.source_message_sha256.clone(),
        tool_name: result.tool_name.clone(),
        tool_call_id: shrink.tool_call_id.clone(),
        status: result.facts.status.clone(),
        original_content_bytes: shrink.original_content_bytes,
        original_content_token_upper_bound: shrink
            .content_token_upper_bound
            .expect("V2 shrink includes its token upper bound"),
        head_excerpt,
        tail_excerpt,
        content_sha256,
        reason: ToolOutputShrinkReasonV1::LargeCompletedHistoricalResult,
        recovery_instruction:
            "Use read_tool_artifact with the opaque artifact ref when omitted details are required."
                .to_owned(),
    };
    Ok(Some(ProjectedToolOutput {
        shrink,
        candidate,
        message,
    }))
}

fn marker_budget(
    content: &str,
    artifact_ref: &ToolArtifactRefV1,
    policy: &ToolOutputProjectionPolicy,
) -> Result<(usize, usize, String)> {
    let mut available = policy.max_projected_content_bytes;
    // The marker includes the eventual byte counts. Its fixed wording means this settles in at
    // most a couple of rounds, but keep the bounded loop explicit instead of trusting a first
    // estimate whose digit width might be smaller than the final value.
    for _ in 0..4 {
        let head_limit = policy.retained_head_bytes.min(available / 2);
        let tail_limit = policy
            .retained_tail_bytes
            .min(available.saturating_sub(head_limit));
        if head_limit == 0 || tail_limit == 0 {
            bail!("tool-output projection limit leaves no truthful head/tail content");
        }
        let head_end = previous_char_boundary(content, head_limit);
        let tail_start =
            next_char_boundary(content, content.len().saturating_sub(tail_limit)).max(head_end);
        let tail_bytes = content.len().saturating_sub(tail_start);
        let marker = tool_output_marker(
            content.len() as u64,
            artifact_ref,
            head_end as u64,
            tail_bytes as u64,
            tail_start.saturating_sub(head_end) as u64,
        );
        let next_available = policy
            .max_projected_content_bytes
            .checked_sub(marker.len())
            .ok_or_else(|| {
                anyhow::anyhow!("tool-output projection marker exceeds its configured limit")
            })?;
        if next_available == available {
            return Ok((head_end, tail_bytes, marker));
        }
        available = next_available;
    }
    bail!("tool-output projection marker budget did not stabilize")
}

fn tool_output_marker(
    original_bytes: u64,
    artifact_ref: &ToolArtifactRefV1,
    retained_head_bytes: u64,
    retained_tail_bytes: u64,
    omitted_bytes: u64,
) -> String {
    format!(
        "\n[sigil: next-epoch artifact-backed tool output; original_bytes={original_bytes}; retained_head_bytes={retained_head_bytes}; retained_tail_bytes={retained_tail_bytes}; omitted_bytes={omitted_bytes}; artifact_ref={}; use_read_tool_artifact=true; model_retrieval_available=true]\n",
        artifact_ref.artifact_id,
    )
}

fn event_ref(event: &crate::StoredEvent) -> CompactionEventRef {
    CompactionEventRef {
        stream_sequence: event.stream_sequence,
        event_id: event.event_id.clone(),
    }
}

fn previous_char_boundary(value: &str, max_index: usize) -> usize {
    let mut index = max_index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(value: &str, min_index: usize) -> usize {
    let mut index = min_index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "tests/tool_output_projection_tests.rs"]
mod tests;
