use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::*;

/// Current schema for a recoverable old tool-output shrink descriptor.
pub const TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION: u16 = 2;
const LEGACY_TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION: u16 = 1;
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

/// Truthful source reference included whenever a model-visible tool output is projection-shrunk.
///
/// It intentionally does not grant the model a retrieval capability. The TUI/audit surface may
/// inspect the raw append-only event, while a later retrieval-artifact implementation can add a
/// separate explicit capability rather than silently treating a local path as model-callable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ToolOutputProjectionSourceRef {
    DurableTranscriptEvent { event_id: crate::EventId },
}

/// Why a completed tool output is eligible for a next-epoch bounded preview.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputShrinkReasonV1 {
    LargeCompletedHistoricalResult,
}

/// Session-owned durable reference used to recover a projected tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ToolOutputArtifactRefV1 {
    DurableTranscriptEvent {
        event_id: crate::EventId,
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
    /// V2 metadata is deterministic and contains no raw output excerpt. Legacy V1 descriptors
    /// decode with these fields absent and are re-derived from their immutable source event.
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
        if !matches!(
            self.schema_version,
            LEGACY_TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION
                | TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION
        ) || self.source_event.stream_sequence == 0
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
            ToolOutputProjectionSourceRef::DurableTranscriptEvent { event_id }
                if event_id == &self.source_event.event_id => {}
            ToolOutputProjectionSourceRef::DurableTranscriptEvent { .. } => {
                bail!("tool-output projection source ref does not match its source event");
            }
        }
        if self.schema_version == TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION {
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
                ToolOutputArtifactRefV1::DurableTranscriptEvent {
                    event_id,
                    content_sha256: artifact_hash,
                } if event_id == &self.source_event.event_id && artifact_hash == content_sha256 => {
                }
                ToolOutputArtifactRefV1::DurableTranscriptEvent { .. } => {
                    bail!("tool-output projection artifact ref does not bind its source");
                }
            }
        } else if self.tool_name.is_some()
            || self.status.is_some()
            || self.content_sha256.is_some()
            || self.content_token_upper_bound.is_some()
            || self.artifact_ref.is_some()
            || self.reason.is_some()
        {
            bail!("legacy tool-output projection descriptor contains V2 metadata");
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
    /// The complete raw durable stream is revalidated through [`CompactionFoldPlan`]. Only a
    /// `ToolResult` event that is already in that plan's folded set can shrink. Tail entries,
    /// controls, tool-call assistant arguments, unfinished pairs and non-structured legacy-like
    /// tool content are never modified by this projection.
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
        let tool_names = records.iter().try_fold(
            BTreeMap::<String, String>::new(),
            |mut names, record| -> Result<_> {
                if let Some(SessionLogEntry::Assistant(message)) =
                    session_entry_from_stored_event(record.stored_event())?
                {
                    for call in message.tool_calls {
                        if let Some(previous) = names.insert(call.id.clone(), call.name.clone())
                            && previous != call.name
                        {
                            bail!("tool call id resolves to multiple tool names");
                        }
                    }
                }
                Ok(names)
            },
        )?;
        let mut outputs = Vec::new();
        for event_id in &plan.folded_event_ids {
            let event = by_id
                .get(event_id.as_str())
                .copied()
                .expect("validated fold plan references its current durable stream");
            let Some(SessionLogEntry::ToolResult(message)) =
                session_entry_from_stored_event(event)?
            else {
                continue;
            };
            let tool_call_id = message
                .tool_call_id
                .as_deref()
                .context("validated tool result has no call id")?;
            let tool_name = tool_names
                .get(tool_call_id)
                .with_context(|| format!("tool result {tool_call_id} has no durable tool name"))?;
            let Some(projected) =
                project_tool_result(event_ref(event), message, tool_name, policy)?
            else {
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

fn project_tool_result(
    source_event: CompactionEventRef,
    mut message: crate::ModelMessage,
    tool_name: &str,
    policy: &ToolOutputProjectionPolicy,
) -> Result<Option<ProjectedToolOutput>> {
    if !matches!(message.role, crate::MessageRole::Tool)
        || message
            .tool_call_id
            .as_deref()
            .is_none_or(|tool_call_id| tool_call_id.trim().is_empty())
        || !message.tool_calls.is_empty()
    {
        bail!("folded tool result has an unsafe model-message shape");
    }
    let Some(raw_message_content) = message.content.as_deref() else {
        return Ok(None);
    };
    let source_message_sha256 = format!("sha256:{:x}", Sha256::digest(raw_message_content));
    let mut envelope = match serde_json::from_str::<Value>(raw_message_content) {
        Ok(Value::Object(envelope)) => envelope,
        Ok(_) | Err(_) => return Ok(None),
    };
    let Some(content) = envelope.get("content").and_then(Value::as_str) else {
        return Ok(None);
    };
    if content.len() <= policy.max_projected_content_bytes {
        return Ok(None);
    }
    let tool_call_id = message
        .tool_call_id
        .clone()
        .expect("safe tool result was checked above");
    let status = normalized_tool_status(&envelope);
    let content_sha256 = format!("sha256:{:x}", Sha256::digest(content));
    let artifact_ref = ToolOutputArtifactRefV1::DurableTranscriptEvent {
        event_id: source_event.event_id.clone(),
        content_sha256: content_sha256.clone(),
    };
    let (head_bytes, tail_bytes, marker) = marker_budget(
        content,
        &source_event,
        tool_name,
        &tool_call_id,
        &status,
        &content_sha256,
        policy,
    )?;
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
        tool_call_id,
        source_message_sha256,
        original_content_bytes: content.len() as u64,
        retained_head_bytes,
        retained_tail_bytes,
        omitted_bytes,
        source_ref: ToolOutputProjectionSourceRef::DurableTranscriptEvent {
            event_id: source_event.event_id.clone(),
        },
        tool_name: Some(tool_name.to_owned()),
        status: Some(status.clone()),
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
    envelope.insert("content".to_owned(), Value::String(rendered));
    envelope.insert(
        "compaction_projection".to_owned(),
        projection_metadata_value(&shrink)?,
    );
    message.content = Some(
        serde_json::to_string(&Value::Object(envelope))
            .context("failed to serialize projected tool-result envelope")?,
    );
    let candidate = RecoverableToolOutputShrinkCandidateV1 {
        schema_version: RECOVERABLE_TOOL_OUTPUT_SHRINK_CANDIDATE_SCHEMA_VERSION,
        raw_durable_result: artifact_ref,
        epoch_visible_source_message_sha256: shrink.source_message_sha256.clone(),
        tool_name: tool_name.to_owned(),
        tool_call_id: shrink.tool_call_id.clone(),
        status,
        original_content_bytes: shrink.original_content_bytes,
        original_content_token_upper_bound: shrink
            .content_token_upper_bound
            .expect("V2 shrink includes its token upper bound"),
        head_excerpt,
        tail_excerpt,
        content_sha256,
        reason: ToolOutputShrinkReasonV1::LargeCompletedHistoricalResult,
        recovery_instruction:
            "Re-read the durable transcript event when omitted details are required.".to_owned(),
    };
    Ok(Some(ProjectedToolOutput {
        shrink,
        candidate,
        message,
    }))
}

fn marker_budget(
    content: &str,
    source_event: &CompactionEventRef,
    tool_name: &str,
    tool_call_id: &str,
    status: &str,
    content_sha256: &str,
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
            source_event,
            tool_name,
            tool_call_id,
            status,
            content_sha256,
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
    source_event: &CompactionEventRef,
    tool_name: &str,
    tool_call_id: &str,
    status: &str,
    content_sha256: &str,
    retained_head_bytes: u64,
    retained_tail_bytes: u64,
    omitted_bytes: u64,
) -> String {
    format!(
        "\n[sigil: next-epoch recoverable tool output; tool={tool_name}; call_id={tool_call_id}; status={status}; original_bytes={original_bytes}; retained_head_bytes={retained_head_bytes}; retained_tail_bytes={retained_tail_bytes}; omitted_bytes={omitted_bytes}; content_hash={content_sha256}; durable_transcript_event={}; re_read_when_needed=true; model_retrieval_available=false]\n",
        source_event.event_id,
    )
}

fn normalized_tool_status(envelope: &Map<String, Value>) -> String {
    let status = envelope
        .get("status")
        .and_then(|value| match value {
            Value::String(status) => Some(status.as_str()),
            Value::Bool(true) => Some("ok"),
            Value::Bool(false) => Some("failed"),
            _ => None,
        })
        .unwrap_or("unknown")
        .trim();
    let status = if status.is_empty() { "unknown" } else { status };
    status.chars().take(64).collect()
}

fn projection_metadata_value(shrink: &ToolOutputProjectionShrink) -> Result<Value> {
    let mut retrieval = Map::new();
    match &shrink.source_ref {
        ToolOutputProjectionSourceRef::DurableTranscriptEvent { event_id } => {
            retrieval.insert(
                "kind".to_owned(),
                Value::String("durable_transcript_event".to_owned()),
            );
            retrieval.insert("event_id".to_owned(), Value::String(event_id.clone()));
            retrieval.insert("model_retrieval_available".to_owned(), Value::Bool(false));
        }
    }
    serde_json::to_value(serde_json::json!({
        "schema_version": shrink.schema_version,
        "source_event": {
            "stream_sequence": shrink.source_event.stream_sequence,
            "event_id": shrink.source_event.event_id,
        },
        "tool_call_id": shrink.tool_call_id,
        "source_message_sha256": shrink.source_message_sha256,
        "original_content_bytes": shrink.original_content_bytes,
        "retained_head_bytes": shrink.retained_head_bytes,
        "retained_tail_bytes": shrink.retained_tail_bytes,
        "omitted_bytes": shrink.omitted_bytes,
        "source_ref": Value::Object(retrieval),
        "tool_name": shrink.tool_name,
        "status": shrink.status,
        "content_sha256": shrink.content_sha256,
        "content_token_upper_bound": shrink.content_token_upper_bound,
        "artifact_ref": shrink.artifact_ref,
        "reason": shrink.reason,
        "recovery_instruction": "Re-read the durable transcript event when omitted details are required.",
    }))
    .context("failed to serialize tool-output projection metadata")
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
