//! Branch navigation and explicit conclusion import through existing session owners.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    AssistantMessageKind, BranchKnowledgeImportedV1, ConversationForkProjection,
    ConversationForked, DurableEventType, Session, SessionLogEntry, SessionRef,
    SessionStreamRecord, context_engine::DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES,
    safe_persistence_text, stable_event_hash,
};

use crate::{LocalSessionLifecycleService, application_recovery::read_bound_records};

/// One persisted parent/child relation. A missing parent remains visible without inventing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationBranchLinkView {
    pub session_ref: SessionRef,
    pub session_id: String,
    pub title: Option<String>,
    pub source_turn_index: usize,
    pub source_turn_digest: String,
}

/// Read-only lineage; unavailable catalog rows never block the known relations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationBranchLineageView {
    pub session_id: String,
    pub parent: Option<ApplicationBranchLinkView>,
    pub children: Vec<ApplicationBranchLinkView>,
    pub unavailable_count: usize,
}

/// A finalized model conclusion, bounded by the existing Context V2 snippet budget. Selection
/// binds both its full source text and the safe excerpt displayed to the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationBranchKnowledgePointView {
    pub source_turn_digest: String,
    pub source_message_id: String,
    pub source_text_sha256: String,
    pub summary_sha256: String,
    pub summary: String,
    pub truncated: bool,
}

/// Exact source binding with available conclusions. Querying never invokes a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationBranchKnowledgePreview {
    pub source_session_ref: SessionRef,
    pub source_session_id: String,
    pub points: Vec<ApplicationBranchKnowledgePointView>,
}

/// Explicit user selection. The host re-reads the source; callers cannot supply the summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationBranchKnowledgeImportRequest {
    pub source_session_ref: SessionRef,
    pub source_session_id: String,
    pub source_turn_digest: String,
    pub source_message_id: String,
    pub source_text_sha256: String,
    pub summary_sha256: String,
}

/// A real durable append or an exact already-imported result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationBranchKnowledgeImportReceipt {
    pub import_id: String,
    pub already_imported: bool,
}

/// Queries the recorded parent and immediate children using the current workspace catalog.
///
/// # Errors
/// Rejects a stale current identity or invalid current lineage. Other unavailable sessions are
/// counted explicitly and do not hide valid relations.
pub fn application_branch_lineage_view(
    service: &LocalSessionLifecycleService,
    current: &SessionRef,
    current_session_id: &str,
) -> Result<ApplicationBranchLineageView> {
    let binding = service.resolve_session_for_reopen(current, current_session_id)?;
    let records = read_bound_records(&binding.session_log_path, current_session_id)?;
    let parent = fork_source(&records)?.map(|fork| ApplicationBranchLinkView {
        session_ref: fork.parent_session_ref,
        session_id: fork.source_session_id,
        title: None,
        source_turn_index: fork.source_turn_index,
        source_turn_digest: fork.source_turn_digest,
    });
    let mut result = ApplicationBranchLineageView {
        session_id: current_session_id.to_owned(),
        parent,
        children: Vec::new(),
        unavailable_count: 0,
    };
    let catalog = service.catalog()?;
    result.unavailable_count = catalog.truncated_entry_count;
    let mut inspected_bytes = 0u64;
    for candidate in catalog.entries {
        inspected_bytes = inspected_bytes.saturating_add(candidate.bytes);
        if inspected_bytes
            > crate::session_lifecycle::DEFAULT_SESSION_CATALOG_MAX_TOTAL_VALIDATION_BYTES
        {
            result.unavailable_count += 1;
            continue;
        }
        if let Some(parent) = result.parent.as_mut()
            && candidate.session_id.as_deref() == Some(parent.session_id.as_str())
        {
            parent.title = candidate.title.clone();
        }
        if candidate.session_ref == *current {
            continue;
        }
        let Some(session_id) = candidate.session_id.as_deref() else {
            result.unavailable_count += 1;
            continue;
        };
        let source = read_bound_records(&candidate.path, session_id)
            .and_then(|records| fork_source(&records));
        match source {
            Ok(Some(fork)) if fork.source_session_id == current_session_id => {
                result.children.push(ApplicationBranchLinkView {
                    session_ref: candidate.session_ref,
                    session_id: session_id.to_owned(),
                    title: candidate.title,
                    source_turn_index: fork.source_turn_index,
                    source_turn_digest: fork.source_turn_digest,
                });
            }
            Ok(_) => {}
            Err(_) => result.unavailable_count += 1,
        }
    }
    Ok(result)
}

/// Projects completed conclusions from one exact session without creating an execution owner.
///
/// # Errors
/// Rejects unavailable or mismatched source identities and invalid durable streams.
pub fn application_branch_knowledge_preview(
    service: &LocalSessionLifecycleService,
    source: &SessionRef,
    source_session_id: &str,
) -> Result<ApplicationBranchKnowledgePreview> {
    let binding = service.resolve_session_for_reopen(source, source_session_id)?;
    let records = read_bound_records(&binding.session_log_path, source_session_id)?;
    Ok(ApplicationBranchKnowledgePreview {
        source_session_ref: source.clone(),
        source_session_id: source_session_id.to_owned(),
        points: conclusion_points(&records)?,
    })
}

/// Imports exact selected knowledge through the caller's current destination writer. The
/// application command owner must retain ownership of the selected destination session.
/// No model, tool, approval, lease or verification state is executed or copied.
///
/// # Errors
/// Rejects stale source selection, foreign destination identity, invalid summaries and failed durable append.
pub fn import_application_branch_knowledge(
    service: &LocalSessionLifecycleService,
    target: &mut Session,
    request: &ApplicationBranchKnowledgeImportRequest,
) -> Result<ApplicationBranchKnowledgeImportReceipt> {
    if let Some(existing) = target.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Control(sigil_kernel::ControlEntry::BranchKnowledgeImportedV1(entry))
            if entry.source_session_id == request.source_session_id
                && entry.source_turn_digest == request.source_turn_digest
                && entry.source_message_id == request.source_message_id
                && entry.source_text_sha256 == request.source_text_sha256
                && entry.summary_sha256 == request.summary_sha256 =>
        {
            Some(entry.clone())
        }
        _ => None,
    }) {
        existing.validate()?;
        ensure!(
            existing.target_session_id == target.session_scope_id(),
            "branch knowledge destination scope changed"
        );
        let import_id = existing.import_id.clone();
        target
            .append_branch_knowledge(existing)
            .map_err(crate::application_operation_owner::ApplicationPublicationError)?;
        return Ok(ApplicationBranchKnowledgeImportReceipt {
            import_id,
            already_imported: true,
        });
    }
    let preview = application_branch_knowledge_preview(
        service,
        &request.source_session_ref,
        &request.source_session_id,
    )?;
    let point = preview
        .points
        .into_iter()
        .find(|point| {
            point.source_turn_digest == request.source_turn_digest
                && point.source_message_id == request.source_message_id
                && point.source_text_sha256 == request.source_text_sha256
                && point.summary_sha256 == request.summary_sha256
        })
        .context("branch conclusion changed; refresh the source selection")?;
    let mut imported = BranchKnowledgeImportedV1 {
        schema_version: 1,
        import_id: String::new(),
        target_session_id: target.session_scope_id().to_owned(),
        source_session_id: request.source_session_id.clone(),
        source_turn_digest: point.source_turn_digest,
        source_message_id: point.source_message_id,
        source_text_sha256: point.source_text_sha256,
        summary_sha256: point.summary_sha256,
        summary: point.summary,
        truncated: point.truncated,
    };
    imported.import_id = imported.expected_import_id()?;
    let import_id = imported.import_id.clone();
    let appended = target
        .append_branch_knowledge(imported)
        .map_err(crate::application_operation_owner::ApplicationPublicationError)?;
    Ok(ApplicationBranchKnowledgeImportReceipt {
        import_id,
        already_imported: !appended,
    })
}

fn fork_source(records: &[SessionStreamRecord]) -> Result<Option<ConversationForked>> {
    let mut source = None;
    for record in records {
        if record.stored_event().event_kind() == Some(DurableEventType::ConversationForked) {
            let fork: ConversationForked =
                serde_json::from_value(record.stored_event().payload.clone())?;
            ensure!(
                fork.destination_session_id == record.session_id() && source.is_none(),
                "invalid conversation branch lineage"
            );
            source = Some(fork);
        }
    }
    Ok(source)
}

fn conclusion_points(
    records: &[SessionStreamRecord],
) -> Result<Vec<ApplicationBranchKnowledgePointView>> {
    let projection = ConversationForkProjection::from_records(records)?;
    let mut points = Vec::new();
    let mut records = records.iter().peekable();
    for point in projection.points {
        while records
            .peek()
            .is_some_and(|record| record.stream_sequence() <= point.source_boundary_stream_sequence)
        {
            records.next();
        }
        let mut candidates = Vec::new();
        while records
            .peek()
            .is_some_and(|record| record.stream_sequence() < point.source_finalized_stream_sequence)
        {
            let Some(record) = records.next() else {
                break;
            };
            if let Some(SessionLogEntry::Assistant(message)) = record.session_log_entry()?
                && message.tool_calls.is_empty()
                && message
                    .assistant_kind
                    .is_none_or(|kind| kind == AssistantMessageKind::FinalAnswer)
            {
                candidates.push(message);
            }
        }
        let final_id = records
            .next()
            .and_then(|record| record.stored_event().payload.get("final_message_id"))
            .and_then(serde_json::Value::as_str);
        let Some(final_id) = final_id else {
            continue;
        };
        for message in candidates
            .into_iter()
            .filter(|message| message.id == final_id)
        {
            let Some(text) = message.content.filter(|text| !text.trim().is_empty()) else {
                continue;
            };
            let source_text_sha256 = stable_event_hash(text.as_bytes());
            let mut summary = safe_persistence_text(&text);
            let truncated = summary.len() > DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES;
            if truncated {
                let mut end = DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES;
                while !summary.is_char_boundary(end) {
                    end -= 1;
                }
                summary.truncate(end);
            }
            points.push(ApplicationBranchKnowledgePointView {
                source_turn_digest: point.source_turn_digest.clone(),
                source_message_id: message.id,
                source_text_sha256,
                summary_sha256: stable_event_hash(summary.as_bytes()),
                summary,
                truncated,
            });
        }
    }
    Ok(points)
}

#[cfg(test)]
#[path = "tests/application_branch_knowledge_tests.rs"]
mod tests;
