//! Canonical, provider-neutral display projection for durable conversation history.

use std::{
    collections::{HashMap, VecDeque},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AssistantMessageKind, CheckpointRestoreConflictReason, CheckpointRestored, ControlEntry,
    ConversationRunLifecycleRecordV1, ConversationRunTerminalStatusV1, DurableEventType,
    JsonlSessionStore, MessageRole, SessionLogEntry, SessionStreamRecord, ToolApprovalAuditAction,
    ToolApprovalUserDecision, TypedDomainEvent, conversation_run_lifecycle_record_from_stream,
    safe_persistence_text,
};

/// Schema version for the canonical conversation display projection.
pub const CONVERSATION_DISPLAY_SCHEMA_VERSION: u16 = 1;
/// Default number of display items returned by one page.
pub const DEFAULT_CONVERSATION_DISPLAY_PAGE_SIZE: usize = 50;
/// Hard item limit for one display page.
pub const MAX_CONVERSATION_DISPLAY_PAGE_SIZE: usize = 100;
/// Hard safe-text limit for one projected item.
pub const MAX_CONVERSATION_DISPLAY_CONTENT_BYTES: usize = 64 * 1024;
/// Hard serialized-content budget for one projected page.
pub const MAX_CONVERSATION_DISPLAY_PAGE_BYTES: usize = 512 * 1024;
const MAX_CONVERSATION_DISPLAY_CURSOR_BYTES: usize = 4 * 1024;
const MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES: usize = 512;

/// Durable ordering key. The stream sequence is authoritative; `subindex` is deterministic
/// within one source event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplayOrderV1 {
    pub session_stream_sequence: u64,
    pub subindex: u32,
}

/// Provider-neutral visual category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayItemKindV1 {
    UserMessage,
    Reasoning,
    AssistantMessage,
    Tool,
    Approval,
    Checkpoint,
    Notice,
    Terminal,
}

/// Evidence source used to build a display item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplaySourceV1 {
    DurableTranscript,
    DurableRunEvent,
    LiveTransient,
}

/// Bounded status vocabulary shared by display item kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayStatusV1 {
    Recorded,
    Requested,
    WaitingForApproval,
    Approved,
    Denied,
    Completed,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayMessageRoleV1 {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayAssistantPhaseV1 {
    ToolPreamble,
    Progress,
    FinalAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayApprovalDecisionV1 {
    Approved,
    ApprovedForSession,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayCheckpointOutcomeV1 {
    Restored,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationDisplayCheckpointConflictReasonV1 {
    WorkspaceMismatch,
    CurrentHashMismatch,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

/// Stable semantic slot used to correlate one live protocol item with its durable successor.
///
/// The public identifier derived from this slot is a one-way digest; it never exposes the durable
/// session scope, run id, message id, or tool call id to a renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationLiveProvisionalSlotV1 {
    User,
    AssistantMessage { message_id: String },
    Tool { call_id: String },
    Approval { call_id: String },
    Terminal,
}

/// Typed, secret-safe content carried by one display item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum ConversationDisplayContentV1 {
    Message {
        role: ConversationDisplayMessageRoleV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assistant_phase: Option<ConversationDisplayAssistantPhaseV1>,
        image_attachment_count: usize,
        truncated: bool,
        original_content_bytes: usize,
    },
    Reasoning {
        text: String,
        truncated: bool,
        original_content_bytes: usize,
    },
    Tool {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        truncated: bool,
        original_content_bytes: usize,
    },
    Approval {
        call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<ConversationDisplayApprovalDecisionV1>,
    },
    Checkpoint {
        outcome: ConversationDisplayCheckpointOutcomeV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conflict_reason: Option<ConversationDisplayCheckpointConflictReasonV1>,
    },
    Notice {
        text: String,
        truncated: bool,
        original_content_bytes: usize,
    },
    Terminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        safe_summary: Option<String>,
        summary_truncated: bool,
    },
}

/// One canonical durable display item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplayItemV1 {
    pub schema_version: u16,
    pub display_id: String,
    pub display_order: ConversationDisplayOrderV1,
    pub source_event_id: String,
    pub kind: ConversationDisplayItemKindV1,
    pub source: ConversationDisplaySourceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_sequence: Option<u64>,
    pub status: ConversationDisplayStatusV1,
    pub content: ConversationDisplayContentV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciles: Option<Vec<String>>,
}

/// Latest proven terminal boundary at the page's fixed durable frontier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationTerminalFrontierV1 {
    pub run_id: String,
    pub session_stream_sequence: u64,
    pub status: ConversationDisplayStatusV1,
}

/// Opaque-cursor page over the canonical display projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationDisplayPageV1 {
    pub schema_version: u16,
    pub session_scope_id: String,
    pub through_session_stream_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_frontier: Option<ConversationTerminalFrontierV1>,
    pub total_items: u64,
    pub items: Vec<ConversationDisplayItemV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct ConversationDisplayCursorV1 {
    schema_version: u16,
    session_scope_sha256: String,
    through_session_stream_sequence: u64,
    frontier_binding_sha256: String,
    before_order: ConversationDisplayOrderV1,
}

#[derive(Debug, Clone)]
struct FixedFrontier {
    sequence: u64,
}

#[derive(Debug, Clone)]
struct ActiveRunProjection {
    run_id: String,
    final_message_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ToolProjection {
    name: String,
    requested_display_id: String,
}

/// Derives an opaque renderer identity for one live semantic slot.
///
/// # Errors
///
/// Returns an error when the durable scope, run id, or slot identity is empty or unbounded.
pub fn conversation_live_provisional_id(
    session_scope: &str,
    run_id: &str,
    slot: &ConversationLiveProvisionalSlotV1,
) -> Result<String> {
    validate_provisional_identity("session scope", session_scope)?;
    validate_provisional_identity("run id", run_id)?;
    let (slot_kind, slot_identity) = match slot {
        ConversationLiveProvisionalSlotV1::User => ("user", None),
        ConversationLiveProvisionalSlotV1::AssistantMessage { message_id } => {
            validate_provisional_identity("assistant message id", message_id)?;
            ("assistant_message", Some(message_id.as_str()))
        }
        ConversationLiveProvisionalSlotV1::Tool { call_id } => {
            validate_provisional_identity("tool call id", call_id)?;
            ("tool", Some(call_id.as_str()))
        }
        ConversationLiveProvisionalSlotV1::Approval { call_id } => {
            validate_provisional_identity("approval call id", call_id)?;
            ("approval", Some(call_id.as_str()))
        }
        ConversationLiveProvisionalSlotV1::Terminal => ("terminal", None),
    };
    let mut digest = Sha256::new();
    digest.update(b"sigil-conversation-live-v1\0");
    digest.update(session_scope.as_bytes());
    digest.update(b"\0");
    digest.update(run_id.as_bytes());
    digest.update(b"\0");
    digest.update(slot_kind.as_bytes());
    if let Some(identity) = slot_identity {
        digest.update(b"\0");
        digest.update(identity.as_bytes());
    }
    Ok(format!("live-v1:{:x}", digest.finalize()))
}

/// Reads a validated JSONL session and projects one canonical display page.
///
/// # Errors
///
/// Fails closed on malformed/tampered records, unknown recovery-critical events, scope or cursor
/// mismatch, invalid run lifecycle ordering, and invalid page bounds.
pub fn conversation_display_page(
    session_path: &Path,
    expected_scope: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<ConversationDisplayPageV1> {
    let records = JsonlSessionStore::read_event_records(session_path).with_context(|| {
        format!(
            "failed to read conversation session {}",
            session_path.display()
        )
    })?;
    conversation_display_page_from_records(&records, expected_scope, cursor, limit)
}

/// Projects one page from already-loaded durable stream records.
///
/// This entry point exists for adapters that already own a validated session snapshot and for
/// deterministic contract tests. It preserves the same fixed-frontier and fail-closed behavior as
/// [`conversation_display_page`].
///
/// # Errors
///
/// Returns an error for invalid bounds, scope/order/checksum violations, invalid cursor state, or
/// malformed critical durable records.
pub fn conversation_display_page_from_records(
    records: &[SessionStreamRecord],
    expected_scope: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<ConversationDisplayPageV1> {
    validate_page_request(expected_scope, limit)?;
    validate_stream(records, expected_scope)?;

    let decoded_cursor = cursor.map(decode_cursor).transpose()?;
    let frontier = fixed_frontier(records, expected_scope, decoded_cursor.as_ref())?;
    let before_order = decoded_cursor.as_ref().map(|cursor| cursor.before_order);
    let capacity = limit.saturating_add(1);
    let mut recent = VecDeque::with_capacity(capacity);
    let mut active_run: Option<ActiveRunProjection> = None;
    let mut tools = HashMap::<String, ToolProjection>::new();
    let mut approval_items = HashMap::<String, String>::new();
    let mut terminal_frontier = None;
    let mut total_items = 0_u64;
    let mut eligible_items = 0_u64;
    let mut cursor_boundary_found = decoded_cursor.is_none();

    for record in records
        .iter()
        .take_while(|record| record.stream_sequence() <= frontier.sequence)
    {
        let mut projected = project_record(
            record,
            expected_scope,
            &mut active_run,
            &mut tools,
            &mut approval_items,
            &mut terminal_frontier,
        )?;
        projected.sort_by_key(|item| item.display_order);
        for item in projected {
            if before_order == Some(item.display_order) {
                cursor_boundary_found = true;
            }
            total_items = total_items
                .checked_add(1)
                .ok_or_else(|| anyhow!("conversation display item count overflow"))?;
            if before_order.is_some_and(|before| item.display_order >= before) {
                continue;
            }
            eligible_items = eligible_items
                .checked_add(1)
                .ok_or_else(|| anyhow!("conversation display eligible count overflow"))?;
            recent.push_back(item);
            if recent.len() > capacity {
                recent.pop_front();
            }
        }
    }
    if !cursor_boundary_found {
        bail!("conversation display cursor boundary is not a projected item");
    }

    let mut selected_reversed = Vec::new();
    let mut selected_bytes = 0_usize;
    for item in recent.iter().rev() {
        if selected_reversed.len() == limit {
            break;
        }
        let item_bytes = serde_json::to_vec(item)
            .context("failed to measure canonical conversation display item")?
            .len();
        if !selected_reversed.is_empty()
            && selected_bytes.saturating_add(item_bytes) > MAX_CONVERSATION_DISPLAY_PAGE_BYTES
        {
            break;
        }
        selected_bytes = selected_bytes.saturating_add(item_bytes);
        selected_reversed.push(item.clone());
    }
    selected_reversed.reverse();
    let items = selected_reversed;
    let has_more = eligible_items > u64::try_from(items.len()).unwrap_or(u64::MAX);
    let next_cursor = if has_more {
        let oldest = items
            .first()
            .ok_or_else(|| anyhow!("bounded display page could not retain one item"))?;
        Some(encode_cursor(&ConversationDisplayCursorV1 {
            schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
            session_scope_sha256: scope_sha256(expected_scope),
            through_session_stream_sequence: frontier.sequence,
            frontier_binding_sha256: frontier_binding_sha256(
                expected_scope,
                records,
                frontier.sequence,
                oldest.display_order,
            ),
            before_order: oldest.display_order,
        })?)
    } else {
        None
    };

    Ok(ConversationDisplayPageV1 {
        schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
        session_scope_id: expected_scope.to_owned(),
        through_session_stream_sequence: frontier.sequence,
        terminal_frontier,
        total_items,
        items,
        next_cursor,
        has_more,
    })
}

fn validate_page_request(expected_scope: &str, limit: usize) -> Result<()> {
    if expected_scope.is_empty() || expected_scope.len() > MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES {
        bail!("conversation display requires a bounded non-empty session scope");
    }
    if limit == 0 || limit > MAX_CONVERSATION_DISPLAY_PAGE_SIZE {
        bail!(
            "conversation display page size must be between 1 and {MAX_CONVERSATION_DISPLAY_PAGE_SIZE}"
        );
    }
    Ok(())
}

fn validate_provisional_identity(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES {
        bail!("conversation live provisional {label} must be bounded and non-empty");
    }
    Ok(())
}

fn validate_stream(records: &[SessionStreamRecord], expected_scope: &str) -> Result<()> {
    let mut previous = 0_u64;
    for record in records {
        if record.session_id() != expected_scope {
            bail!("conversation display session scope mismatch");
        }
        let expected_sequence = previous
            .checked_add(1)
            .ok_or_else(|| anyhow!("conversation display stream sequence overflow"))?;
        if record.stream_sequence() != expected_sequence {
            bail!("conversation display stream order is invalid");
        }
        record
            .stored_event()
            .verify_record_checksum()
            .context("conversation display stream checksum verification failed")?;
        previous = record.stream_sequence();
    }
    Ok(())
}

fn fixed_frontier(
    records: &[SessionStreamRecord],
    expected_scope: &str,
    cursor: Option<&ConversationDisplayCursorV1>,
) -> Result<FixedFrontier> {
    let Some(cursor) = cursor else {
        return Ok(records.last().map_or_else(
            || FixedFrontier { sequence: 0 },
            |record| FixedFrontier {
                sequence: record.stream_sequence(),
            },
        ));
    };
    if cursor.schema_version != CONVERSATION_DISPLAY_SCHEMA_VERSION {
        bail!("unsupported conversation display cursor schema");
    }
    if cursor.session_scope_sha256 != scope_sha256(expected_scope) {
        bail!("conversation display cursor belongs to another session scope");
    }
    if cursor.before_order.session_stream_sequence == 0
        || cursor.before_order.session_stream_sequence > cursor.through_session_stream_sequence
    {
        bail!("conversation display cursor order is outside its fixed frontier");
    }
    let frontier = records
        .iter()
        .find(|record| record.stream_sequence() == cursor.through_session_stream_sequence)
        .ok_or_else(|| anyhow!("conversation display cursor frontier is unavailable"))?;
    let expected_binding = frontier_binding_sha256(
        expected_scope,
        records,
        frontier.stream_sequence(),
        cursor.before_order,
    );
    if expected_binding != cursor.frontier_binding_sha256 {
        bail!("conversation display cursor frontier no longer matches durable history");
    }
    Ok(FixedFrontier {
        sequence: cursor.through_session_stream_sequence,
    })
}

fn project_record(
    record: &SessionStreamRecord,
    expected_scope: &str,
    active_run: &mut Option<ActiveRunProjection>,
    tools: &mut HashMap<String, ToolProjection>,
    approval_items: &mut HashMap<String, String>,
    terminal_frontier: &mut Option<ConversationTerminalFrontierV1>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    if let Some(lifecycle) = conversation_run_lifecycle_record_from_stream(record)? {
        return project_lifecycle(
            record,
            expected_scope,
            lifecycle,
            active_run,
            tools,
            approval_items,
            terminal_frontier,
        );
    }

    if let Some(entry) = record.session_log_entry()? {
        return project_session_entry(
            record,
            expected_scope,
            entry,
            active_run,
            tools,
            approval_items,
        );
    }

    let Some(event_type) = record.stored_event().event_kind() else {
        // `session_log_entry` already performed the fail-closed critical decode.
        return Ok(Vec::new());
    };
    if event_type == DurableEventType::CheckpointRestored {
        let _: CheckpointRestored =
            serde_json::from_value(record.stored_event().payload.clone())
                .context("failed to decode checkpoint restored display source")?;
        return Ok(vec![new_item(
            expected_scope,
            record,
            0,
            ConversationDisplayItemKindV1::Checkpoint,
            ConversationDisplaySourceV1::DurableRunEvent,
            active_run_id(active_run),
            ConversationDisplayStatusV1::Completed,
            ConversationDisplayContentV1::Checkpoint {
                outcome: ConversationDisplayCheckpointOutcomeV1::Restored,
                checkpoint_id: None,
                conflict_reason: None,
            },
        )]);
    }
    if let Some(typed) = record.typed_domain_event_record()?
        && let TypedDomainEvent::CheckpointRestoreConflict(conflict) = typed.event
    {
        return Ok(vec![new_item(
            expected_scope,
            record,
            0,
            ConversationDisplayItemKindV1::Checkpoint,
            ConversationDisplaySourceV1::DurableRunEvent,
            active_run_id(active_run),
            ConversationDisplayStatusV1::Failed,
            ConversationDisplayContentV1::Checkpoint {
                outcome: ConversationDisplayCheckpointOutcomeV1::Conflict,
                checkpoint_id: Some(bound_identity(&conflict.checkpoint_id)),
                conflict_reason: Some(map_checkpoint_conflict_reason(conflict.reason)),
            },
        )]);
    }
    Ok(Vec::new())
}

fn project_lifecycle(
    record: &SessionStreamRecord,
    expected_scope: &str,
    lifecycle: ConversationRunLifecycleRecordV1,
    active_run: &mut Option<ActiveRunProjection>,
    tools: &mut HashMap<String, ToolProjection>,
    approval_items: &mut HashMap<String, String>,
    terminal_frontier: &mut Option<ConversationTerminalFrontierV1>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    match lifecycle {
        ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started) => {
            if active_run.is_some() {
                bail!("conversation display encountered overlapping durable runs");
            }
            tools.clear();
            approval_items.clear();
            *active_run = Some(ActiveRunProjection {
                run_id: started.run_id().to_owned(),
                final_message_id: None,
            });
            Ok(Vec::new())
        }
        ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized) => {
            let Some(active) = active_run.as_ref() else {
                bail!("conversation display terminal has no matching durable start");
            };
            if active.run_id != finalized.run_id() {
                bail!("conversation display terminal belongs to another active run");
            }
            match finalized.status() {
                ConversationRunTerminalStatusV1::Succeeded => {
                    let durable_final = active.final_message_id.as_deref().ok_or_else(|| {
                        anyhow!("succeeded conversation run has no durable final assistant")
                    })?;
                    if finalized.final_message_id() != Some(durable_final) {
                        bail!(
                            "succeeded conversation run terminal does not match its durable final assistant"
                        );
                    }
                }
                _ if finalized.final_message_id().is_some() => {
                    bail!("non-succeeded conversation run must not bind a final message id");
                }
                _ => {}
            }
            let status = map_terminal_status(finalized.status())?;
            let run_id = finalized.run_id().to_owned();
            *terminal_frontier = Some(ConversationTerminalFrontierV1 {
                run_id: run_id.clone(),
                session_stream_sequence: record.stream_sequence(),
                status,
            });
            *active_run = None;
            tools.clear();
            approval_items.clear();
            let mut item = new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Terminal,
                ConversationDisplaySourceV1::DurableRunEvent,
                Some(run_id.clone()),
                status,
                ConversationDisplayContentV1::Terminal {
                    final_message_id: finalized.final_message_id().map(ToOwned::to_owned),
                    safe_summary: finalized.safe_summary().map(ToOwned::to_owned),
                    summary_truncated: finalized.summary_truncated(),
                },
            );
            item.reconciles = Some(vec![conversation_live_provisional_id(
                expected_scope,
                &run_id,
                &ConversationLiveProvisionalSlotV1::Terminal,
            )?]);
            Ok(vec![item])
        }
    }
}

fn project_session_entry(
    record: &SessionStreamRecord,
    expected_scope: &str,
    entry: SessionLogEntry,
    active_run: &mut Option<ActiveRunProjection>,
    tools: &mut HashMap<String, ToolProjection>,
    approval_items: &mut HashMap<String, String>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    match entry {
        SessionLogEntry::User(message) => {
            if message.role != MessageRole::User {
                bail!("conversation display user entry has a non-user role");
            }
            let content = project_optional_text(message.content.as_deref());
            if content.is_none() && message.image_attachments.is_empty() {
                return Ok(Vec::new());
            }
            let run_id = active_run_id(active_run);
            let mut item = new_message_item(
                expected_scope,
                record,
                0,
                run_id.clone(),
                ConversationDisplayMessageRoleV1::User,
                content,
                None,
                message.image_attachments.len(),
            );
            if let Some(run_id) = run_id {
                item.reconciles = Some(vec![conversation_live_provisional_id(
                    expected_scope,
                    &run_id,
                    &ConversationLiveProvisionalSlotV1::User,
                )?]);
            }
            Ok(vec![item])
        }
        SessionLogEntry::Assistant(message) => {
            if message.role != MessageRole::Assistant {
                bail!("conversation display assistant entry has a non-assistant role");
            }
            if message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
                && let Some(active) = active_run.as_mut()
                && active
                    .final_message_id
                    .replace(message.id.clone())
                    .is_some()
            {
                bail!("conversation run contains more than one durable final assistant");
            }
            let run_id = active_run_id(active_run);
            let assistant_provisional = run_id
                .as_deref()
                .map(|run_id| {
                    conversation_live_provisional_id(
                        expected_scope,
                        run_id,
                        &ConversationLiveProvisionalSlotV1::AssistantMessage {
                            message_id: message.id.clone(),
                        },
                    )
                })
                .transpose()?;
            let mut items = Vec::new();
            let mut subindex = 0_u32;
            if let Some(content) = project_optional_text(message.content.as_deref()) {
                if message.assistant_kind == Some(AssistantMessageKind::ReasoningTrace) {
                    let mut item = new_item(
                        expected_scope,
                        record,
                        subindex,
                        ConversationDisplayItemKindV1::Reasoning,
                        ConversationDisplaySourceV1::DurableTranscript,
                        run_id.clone(),
                        ConversationDisplayStatusV1::Recorded,
                        ConversationDisplayContentV1::Reasoning {
                            text: content.text,
                            truncated: content.truncated,
                            original_content_bytes: content.original_bytes,
                        },
                    );
                    item.reconciles = assistant_provisional.clone().map(|id| vec![id]);
                    items.push(item);
                } else {
                    let mut item = new_message_item(
                        expected_scope,
                        record,
                        subindex,
                        run_id.clone(),
                        ConversationDisplayMessageRoleV1::Assistant,
                        Some(content),
                        map_assistant_phase(message.assistant_kind),
                        message.image_attachments.len(),
                    );
                    item.reconciles = assistant_provisional.clone().map(|id| vec![id]);
                    items.push(item);
                }
                subindex = subindex
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("conversation display subindex overflow"))?;
            }
            for call in message.tool_calls {
                let tool_name_key = call.id.clone();
                let call_id = bound_identity(&call.id);
                let tool_name = bound_identity(&call.name);
                let mut item = new_item(
                    expected_scope,
                    record,
                    subindex,
                    ConversationDisplayItemKindV1::Tool,
                    ConversationDisplaySourceV1::DurableTranscript,
                    run_id.clone(),
                    ConversationDisplayStatusV1::Requested,
                    ConversationDisplayContentV1::Tool {
                        call_id: Some(call_id),
                        tool_name: Some(tool_name.clone()),
                        output: None,
                        truncated: false,
                        original_content_bytes: 0,
                    },
                );
                if let Some(run_id) = run_id.as_deref() {
                    item.reconciles = Some(vec![conversation_live_provisional_id(
                        expected_scope,
                        run_id,
                        &ConversationLiveProvisionalSlotV1::Tool {
                            call_id: call.id.clone(),
                        },
                    )?]);
                }
                tools.insert(
                    tool_name_key,
                    ToolProjection {
                        name: tool_name,
                        requested_display_id: item.display_id.clone(),
                    },
                );
                items.push(item);
                subindex = subindex
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("conversation display subindex overflow"))?;
            }
            Ok(items)
        }
        SessionLogEntry::ToolResult(message) => {
            if message.role != MessageRole::Tool {
                bail!("conversation display tool entry has a non-tool role");
            }
            let tool = message
                .tool_call_id
                .as_ref()
                .and_then(|call_id| tools.get(call_id))
                .cloned();
            let call_id = message.tool_call_id.as_deref().map(bound_identity);
            let output = project_optional_text(message.content.as_deref());
            let run_id = active_run_id(active_run);
            let mut item = new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Tool,
                ConversationDisplaySourceV1::DurableTranscript,
                run_id.clone(),
                ConversationDisplayStatusV1::Completed,
                ConversationDisplayContentV1::Tool {
                    call_id,
                    tool_name: tool.as_ref().map(|tool| tool.name.clone()),
                    output: output.as_ref().map(|output| output.text.clone()),
                    truncated: output.as_ref().is_some_and(|output| output.truncated),
                    original_content_bytes: output.map_or(0, |output| output.original_bytes),
                },
            );
            let mut reconciles = tool
                .as_ref()
                .map(|tool| vec![tool.requested_display_id.clone()])
                .unwrap_or_default();
            if let (Some(run_id), Some(call_id)) =
                (run_id.as_deref(), message.tool_call_id.as_ref())
            {
                reconciles.push(conversation_live_provisional_id(
                    expected_scope,
                    run_id,
                    &ConversationLiveProvisionalSlotV1::Tool {
                        call_id: call_id.clone(),
                    },
                )?);
            }
            if !reconciles.is_empty() {
                item.reconciles = Some(reconciles);
            }
            Ok(vec![item])
        }
        SessionLogEntry::Control(control) => {
            project_control(record, expected_scope, control, active_run, approval_items)
        }
    }
}

fn project_control(
    record: &SessionStreamRecord,
    expected_scope: &str,
    control: ControlEntry,
    active_run: &Option<ActiveRunProjection>,
    approval_items: &mut HashMap<String, String>,
) -> Result<Vec<ConversationDisplayItemV1>> {
    match control {
        ControlEntry::Note { kind, data } if kind == "reasoning_trace" => {
            let Some(text) = data.get("text").and_then(serde_json::Value::as_str) else {
                bail!("reasoning trace note is missing text");
            };
            let text = project_text(text);
            Ok(vec![new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Reasoning,
                ConversationDisplaySourceV1::DurableTranscript,
                active_run_id(active_run),
                ConversationDisplayStatusV1::Recorded,
                ConversationDisplayContentV1::Reasoning {
                    text: text.text,
                    truncated: text.truncated,
                    original_content_bytes: text.original_bytes,
                },
            )])
        }
        ControlEntry::ToolApproval(approval)
            if approval.action != ToolApprovalAuditAction::PolicyEvaluated =>
        {
            let raw_call_id = approval.call_id.clone();
            let (status, decision) = match approval.action {
                ToolApprovalAuditAction::Requested => {
                    (ConversationDisplayStatusV1::WaitingForApproval, None)
                }
                ToolApprovalAuditAction::Resolved => {
                    let decision = approval.user_decision.map(map_approval_decision);
                    let status = match decision {
                        Some(ConversationDisplayApprovalDecisionV1::Denied) => {
                            ConversationDisplayStatusV1::Denied
                        }
                        Some(_) => ConversationDisplayStatusV1::Approved,
                        None => ConversationDisplayStatusV1::Completed,
                    };
                    (status, decision)
                }
                ToolApprovalAuditAction::PreviewFailed => {
                    (ConversationDisplayStatusV1::Failed, None)
                }
                ToolApprovalAuditAction::PolicyEvaluated => unreachable!(),
            };
            let run_id = active_run_id(active_run);
            let mut item = new_item(
                expected_scope,
                record,
                0,
                ConversationDisplayItemKindV1::Approval,
                ConversationDisplaySourceV1::DurableRunEvent,
                run_id.clone(),
                status,
                ConversationDisplayContentV1::Approval {
                    call_id: bound_identity(&approval.call_id),
                    tool_name: bound_identity(&approval.tool_name),
                    decision,
                },
            );
            let mut reconciles = Vec::new();
            if approval.action != ToolApprovalAuditAction::Requested
                && let Some(requested) = approval_items.get(&raw_call_id)
            {
                reconciles.push(requested.clone());
            }
            if let Some(run_id) = run_id.as_deref() {
                reconciles.push(conversation_live_provisional_id(
                    expected_scope,
                    run_id,
                    &ConversationLiveProvisionalSlotV1::Approval {
                        call_id: raw_call_id.clone(),
                    },
                )?);
            }
            if !reconciles.is_empty() {
                item.reconciles = Some(reconciles);
            }
            if approval.action == ToolApprovalAuditAction::Requested {
                approval_items.insert(raw_call_id, item.display_id.clone());
            }
            Ok(vec![item])
        }
        _ => Ok(Vec::new()),
    }
}

fn active_run_id(active_run: &Option<ActiveRunProjection>) -> Option<String> {
    active_run.as_ref().map(|active| active.run_id.clone())
}

#[derive(Debug)]
struct ProjectedText {
    text: String,
    truncated: bool,
    original_bytes: usize,
}

fn project_optional_text(value: Option<&str>) -> Option<ProjectedText> {
    value.filter(|value| !value.is_empty()).map(project_text)
}

fn project_text(value: &str) -> ProjectedText {
    let original_bytes = value.len();
    let safe = safe_persistence_text(value);
    let (text, truncated) = truncate_utf8(&safe, MAX_CONVERSATION_DISPLAY_CONTENT_BYTES);
    ProjectedText {
        text,
        truncated,
        original_bytes,
    }
}

fn new_message_item(
    expected_scope: &str,
    record: &SessionStreamRecord,
    subindex: u32,
    run_id: Option<String>,
    role: ConversationDisplayMessageRoleV1,
    text: Option<ProjectedText>,
    assistant_phase: Option<ConversationDisplayAssistantPhaseV1>,
    image_attachment_count: usize,
) -> ConversationDisplayItemV1 {
    let kind = match role {
        ConversationDisplayMessageRoleV1::User => ConversationDisplayItemKindV1::UserMessage,
        ConversationDisplayMessageRoleV1::Assistant => {
            ConversationDisplayItemKindV1::AssistantMessage
        }
    };
    new_item(
        expected_scope,
        record,
        subindex,
        kind,
        ConversationDisplaySourceV1::DurableTranscript,
        run_id,
        ConversationDisplayStatusV1::Recorded,
        ConversationDisplayContentV1::Message {
            role,
            text: text.as_ref().map(|text| text.text.clone()),
            assistant_phase,
            image_attachment_count,
            truncated: text.as_ref().is_some_and(|text| text.truncated),
            original_content_bytes: text.map_or(0, |text| text.original_bytes),
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn new_item(
    expected_scope: &str,
    record: &SessionStreamRecord,
    subindex: u32,
    kind: ConversationDisplayItemKindV1,
    source: ConversationDisplaySourceV1,
    run_id: Option<String>,
    status: ConversationDisplayStatusV1,
    content: ConversationDisplayContentV1,
) -> ConversationDisplayItemV1 {
    ConversationDisplayItemV1 {
        schema_version: CONVERSATION_DISPLAY_SCHEMA_VERSION,
        display_id: stable_display_id(expected_scope, record.event_id(), subindex),
        display_order: ConversationDisplayOrderV1 {
            session_stream_sequence: record.stream_sequence(),
            subindex,
        },
        source_event_id: record.event_id().to_owned(),
        kind,
        source,
        run_id,
        run_sequence: None,
        status,
        content,
        reconciles: None,
    }
}

fn stable_display_id(scope: &str, source_event_id: &str, subindex: u32) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sigil-conversation-display-v1\0");
    digest.update(u64::try_from(scope.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(scope.as_bytes());
    digest.update(
        u64::try_from(source_event_id.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(source_event_id.as_bytes());
    digest.update(subindex.to_be_bytes());
    format!("display-sha256:{:x}", digest.finalize())
}

fn scope_sha256(scope: &str) -> String {
    format!("{:x}", Sha256::digest(scope.as_bytes()))
}

fn frontier_binding_sha256(
    scope: &str,
    records: &[SessionStreamRecord],
    sequence: u64,
    before_order: ConversationDisplayOrderV1,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sigil-conversation-display-frontier-v1\0");
    digest.update(u64::try_from(scope.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(scope.as_bytes());
    digest.update(sequence.to_be_bytes());
    digest.update(before_order.session_stream_sequence.to_be_bytes());
    digest.update(before_order.subindex.to_be_bytes());
    for record in records
        .iter()
        .take_while(|record| record.stream_sequence() <= sequence)
    {
        digest.update(record.stream_sequence().to_be_bytes());
        digest.update(
            u64::try_from(record.event_id().len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(record.event_id().as_bytes());
        digest.update(
            u64::try_from(record.record_checksum().len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(record.record_checksum().as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn encode_cursor(cursor: &ConversationDisplayCursorV1) -> Result<String> {
    let encoded = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(cursor).context("failed to encode conversation display cursor")?,
    );
    if encoded.len() > MAX_CONVERSATION_DISPLAY_CURSOR_BYTES {
        bail!("conversation display cursor exceeds bounded size");
    }
    Ok(encoded)
}

fn decode_cursor(encoded: &str) -> Result<ConversationDisplayCursorV1> {
    if encoded.is_empty() || encoded.len() > MAX_CONVERSATION_DISPLAY_CURSOR_BYTES {
        bail!("conversation display cursor has invalid size");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("conversation display cursor is not valid base64url")?;
    serde_json::from_slice(&bytes).context("conversation display cursor payload is invalid")
}

fn bound_identity(value: &str) -> String {
    let safe = safe_persistence_text(value);
    truncate_utf8(&safe, MAX_CONVERSATION_DISPLAY_IDENTITY_BYTES).0
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn map_assistant_phase(
    kind: Option<AssistantMessageKind>,
) -> Option<ConversationDisplayAssistantPhaseV1> {
    match kind {
        Some(AssistantMessageKind::ToolPreamble) => {
            Some(ConversationDisplayAssistantPhaseV1::ToolPreamble)
        }
        Some(AssistantMessageKind::Progress) => Some(ConversationDisplayAssistantPhaseV1::Progress),
        Some(AssistantMessageKind::FinalAnswer) => {
            Some(ConversationDisplayAssistantPhaseV1::FinalAnswer)
        }
        Some(AssistantMessageKind::ReasoningTrace) | None => None,
    }
}

fn map_terminal_status(
    status: ConversationRunTerminalStatusV1,
) -> Result<ConversationDisplayStatusV1> {
    Ok(match status {
        ConversationRunTerminalStatusV1::Succeeded => ConversationDisplayStatusV1::Succeeded,
        ConversationRunTerminalStatusV1::Failed => ConversationDisplayStatusV1::Failed,
        ConversationRunTerminalStatusV1::Cancelled => ConversationDisplayStatusV1::Cancelled,
        ConversationRunTerminalStatusV1::Interrupted => ConversationDisplayStatusV1::Interrupted,
        ConversationRunTerminalStatusV1::Blocked => ConversationDisplayStatusV1::Blocked,
        _ => bail!("unsupported conversation run terminal status"),
    })
}

fn map_approval_decision(
    decision: ToolApprovalUserDecision,
) -> ConversationDisplayApprovalDecisionV1 {
    match decision {
        ToolApprovalUserDecision::Approved => ConversationDisplayApprovalDecisionV1::Approved,
        ToolApprovalUserDecision::ApprovedForSession => {
            ConversationDisplayApprovalDecisionV1::ApprovedForSession
        }
        ToolApprovalUserDecision::Denied => ConversationDisplayApprovalDecisionV1::Denied,
    }
}

fn map_checkpoint_conflict_reason(
    reason: CheckpointRestoreConflictReason,
) -> ConversationDisplayCheckpointConflictReasonV1 {
    match reason {
        CheckpointRestoreConflictReason::WorkspaceMismatch => {
            ConversationDisplayCheckpointConflictReasonV1::WorkspaceMismatch
        }
        CheckpointRestoreConflictReason::CurrentHashMismatch => {
            ConversationDisplayCheckpointConflictReasonV1::CurrentHashMismatch
        }
        CheckpointRestoreConflictReason::ArtifactUnavailable => {
            ConversationDisplayCheckpointConflictReasonV1::ArtifactUnavailable
        }
        CheckpointRestoreConflictReason::SensitiveSnapshot => {
            ConversationDisplayCheckpointConflictReasonV1::SensitiveSnapshot
        }
        CheckpointRestoreConflictReason::UnsupportedSnapshot => {
            ConversationDisplayCheckpointConflictReasonV1::UnsupportedSnapshot
        }
        CheckpointRestoreConflictReason::InvalidBinding => {
            ConversationDisplayCheckpointConflictReasonV1::InvalidBinding
        }
    }
}
