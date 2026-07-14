use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::*;

/// Current schema for an in-memory, projection-only old tool-output shrink descriptor.
pub const TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION: u16 = 1;
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
            ToolOutputProjectionSourceRef::DurableTranscriptEvent { event_id }
                if event_id == &self.source_event.event_id => {}
            ToolOutputProjectionSourceRef::DurableTranscriptEvent { .. } => {
                bail!("tool-output projection source ref does not match its source event");
            }
        }
        Ok(())
    }
}

/// One model-visible replacement derived from, but never written over, a raw tool result.
#[derive(Debug, Clone)]
pub struct ProjectedToolOutput {
    pub shrink: ToolOutputProjectionShrink,
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
            let Some(projected) = project_tool_result(event_ref(event), message, policy)? else {
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
    let (head_bytes, tail_bytes, marker) = marker_budget(content, &source_event, policy)?;
    let head_end = previous_char_boundary(content, head_bytes);
    let tail_start =
        next_char_boundary(content, content.len().saturating_sub(tail_bytes)).max(head_end);
    let retained_head_bytes = head_end as u64;
    let retained_tail_bytes = content.len().saturating_sub(tail_start) as u64;
    let omitted_bytes = tail_start.saturating_sub(head_end) as u64;
    if omitted_bytes == 0 {
        return Ok(None);
    }

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
    };
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
    Ok(Some(ProjectedToolOutput { shrink, message }))
}

fn marker_budget(
    content: &str,
    source_event: &CompactionEventRef,
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
    retained_head_bytes: u64,
    retained_tail_bytes: u64,
    omitted_bytes: u64,
) -> String {
    format!(
        "\n[sigil: old tool output projection; original_bytes={original_bytes}; retained_head_bytes={retained_head_bytes}; retained_tail_bytes={retained_tail_bytes}; omitted_bytes={omitted_bytes}; durable_transcript_event={}; model_retrieval_available=false]\n",
        source_event.event_id
    )
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
