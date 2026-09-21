use std::{path::Path, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, stream};

use super::*;
use crate::{
    Agent, AgentRunDisposition, AgentRunInput, AgentRunOptions, AgentRunPurpose, CompactionConfig,
    CompletionRequest, DurableEventType, EventClass, InteractionMode, JsonlSessionStore,
    MemoryConfig, ModelMessage, NoopEventHandler, PermissionConfig, PermissionEvaluationContext,
    PlanReviewDraftContext, PlanReviewPurposeContext, PlanSourceRef, Provider,
    ProviderCapabilities, ProviderChunk, ReasoningStreamSupport, Session, SessionLogEntry,
    ToolRegistry,
};

struct DraftProvider;

#[async_trait]
impl Provider for DraftProvider {
    fn name(&self) -> &str {
        "plan-review-recovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Unsupported,
            supports_reasoning_effort: false,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: true,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: true,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call = ToolCall {
            id: "recovery-draft".to_owned(),
            name: crate::PLAN_REVIEW_RESULT_TOOL_NAME.to_owned(),
            args_json: json!({
                "schema_version": 1,
                "outcome": "draft",
                "content": "# Recovered plan\n\n1. Preserve the complete child result.",
            })
            .to_string(),
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call.id.clone(),
                name: call.name.clone(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call.id.clone(),
                delta: call.args_json.clone(),
            }),
            Ok(ProviderChunk::ToolCallComplete(call)),
            Ok(ProviderChunk::Done),
        ])))
    }
}

fn seed_parent(path: &Path) -> Result<(Session, PlanReviewAttemptEntry)> {
    let mut parent = Session::load_from_store("mock", "model", JsonlSessionStore::new(path)?)?;
    let source_turn =
        ConversationTurnRef::new(parent.session_scope_id(), "plan-source", "parent-run")?;
    let plan_review_id = plan_review_id_for_source(&source_turn);
    let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
    let attempt = PlanReviewAttemptEntry {
        plan_review_id: plan_review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id),
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn,
        explicit_objective: Some("Preserve completed Plan reviews on restart".to_owned()),
        route_decision_id: None,
        child_session_ref: plan_review_child_session_ref(&plan_review_id, &attempt_id),
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: Some("workspace-snapshot".to_owned()),
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 1,
    };
    parent.append_control(ControlEntry::PlanReviewAttempt(attempt.clone()))?;
    Ok((parent, attempt))
}

fn draft_source(attempt: &PlanReviewAttemptEntry) -> PlanSourceRef {
    PlanSourceRef {
        source_turn: Some(attempt.source_turn.clone()),
        plan_review_id: Some(attempt.plan_review_id.clone()),
        route_decision_id: attempt.route_decision_id.clone(),
        ..PlanSourceRef::default()
    }
}

async fn complete_child(path: &Path, attempt: &PlanReviewAttemptEntry) -> Result<Session> {
    let mut child = Session::load_from_store("mock", "model", JsonlSessionStore::new(path)?)?;
    let input = AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
        "Submit the complete researched Plan.",
    )])
    .with_logical_run_id("plan-review-recovery-finalizer")
    .with_run_purpose(AgentRunPurpose::PlanReview(PlanReviewPurposeContext {
        plan_review_id: attempt.plan_review_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        plan_id: attempt.plan_id.clone(),
        source_turn: attempt.source_turn.clone(),
        route_decision_id: attempt.route_decision_id.clone(),
    }))
    .with_plan_review_draft(PlanReviewDraftContext {
        plan_review_id: attempt.plan_review_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        plan_id: attempt.plan_id.clone(),
        source: draft_source(attempt),
        workspace_snapshot_id: attempt.workspace_snapshot_id.clone(),
    });
    let output = Agent::new(DraftProvider, ToolRegistry::new())
        .run_with_input(
            &mut child,
            input,
            AgentRunOptions {
                workspace_root: path.parent().context("child parent")?.to_path_buf(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: PermissionEvaluationContext::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut NoopEventHandler,
        )
        .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::PlanReviewDraftSubmitted(_)
    ));
    Ok(child)
}

fn latest_status(parent: &Session, attempt: &PlanReviewAttemptEntry) -> PlanReviewAttemptStatus {
    PlanReviewProjection::from_entries(parent.entries())
        .latest_attempt(&attempt.plan_review_id)
        .expect("parent attempt")
        .status
}

#[tokio::test]
async fn kernel_reopen_does_not_recover_managed_plan_without_authority() -> Result<()> {
    {
        let temp = tempfile::tempdir()?;
        let parent_path = temp.path().join("session.jsonl");
        let (parent, attempt) = seed_parent(&parent_path)?;
        let child_path = attempt.child_session_ref.resolve(temp.path());
        let child = complete_child(&child_path, &attempt).await?;
        assert!(
            child
                .plan_artifact_projection()
                .plans
                .contains_key(&attempt.plan_id)
        );
        let child_before = std::fs::read(&child_path)?;
        assert!(parent.plan_artifact_projection().plans.is_empty());
        drop(child);
        drop(parent);

        let recovered =
            Session::load_from_store("mock", "model", JsonlSessionStore::new(&parent_path)?)?;
        assert_eq!(
            latest_status(&recovered, &attempt),
            PlanReviewAttemptStatus::Interrupted
        );
        assert!(
            !recovered
                .plan_artifact_projection()
                .plans
                .contains_key(&attempt.plan_id),
            "kernel session reopen must not read a managed child by its persisted path"
        );
        let parent_after = std::fs::read(&parent_path)?;
        drop(recovered);
        let replayed =
            Session::load_from_store("mock", "model", JsonlSessionStore::new(&parent_path)?)?;
        assert_eq!(
            latest_status(&replayed, &attempt),
            PlanReviewAttemptStatus::Interrupted
        );
        assert_eq!(std::fs::read(&parent_path)?, parent_after);
        assert_eq!(std::fs::read(&child_path)?, child_before);
    }
    Ok(())
}

#[tokio::test]
async fn plan_review_recovery_does_not_adopt_child_after_parent_cancelled() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("session.jsonl");
    let (mut parent, attempt) = seed_parent(&parent_path)?;
    let child_path = attempt.child_session_ref.resolve(temp.path());
    drop(complete_child(&child_path, &attempt).await?);
    let cancelled = PlanReviewAttemptEntry {
        status: PlanReviewAttemptStatus::Cancelled,
        terminal_reason: Some(PlanReviewTerminalReason::UserCancelled),
        recorded_at_ms: 2,
        ..attempt.clone()
    };
    parent.append_control(ControlEntry::PlanReviewAttempt(cancelled.clone()))?;
    assert!(
        recover_plan_review_draft_from_child_records(
            &cancelled,
            &JsonlSessionStore::read_event_records(&child_path)?,
        )?
        .is_none()
    );
    drop(parent);
    let recovered =
        Session::load_from_store("mock", "model", JsonlSessionStore::new(parent_path)?)?;
    assert_eq!(
        latest_status(&recovered, &attempt),
        PlanReviewAttemptStatus::Cancelled
    );
    assert!(recovered.plan_artifact_projection().plans.is_empty());
    Ok(())
}

#[tokio::test]
async fn plan_review_recovery_rejects_child_failed_or_cancelled_after_draft() -> Result<()> {
    for status in ["failed", "cancelled"] {
        let temp = tempfile::tempdir()?;
        let parent_path = temp.path().join("session.jsonl");
        let (parent, attempt) = seed_parent(&parent_path)?;
        let child_path = attempt.child_session_ref.resolve(temp.path());
        let mut child = complete_child(&child_path, &attempt).await?;
        child.append_durable_event(
            DurableEventType::RunFinalized,
            EventClass::Critical,
            json!({ "run_status": status, "terminal_reason": status }),
        )?;
        drop(child);
        assert!(
            recover_plan_review_draft_from_child_records(
                &attempt,
                &JsonlSessionStore::read_event_records(&child_path)?,
            )?
            .is_none()
        );
        drop(parent);
        let recovered =
            Session::load_from_store("mock", "model", JsonlSessionStore::new(parent_path)?)?;
        assert_eq!(
            latest_status(&recovered, &attempt),
            PlanReviewAttemptStatus::Interrupted
        );
        assert!(recovered.plan_artifact_projection().plans.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn plan_review_recovery_requires_full_typed_settlement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("session.jsonl");
    let (parent, attempt) = seed_parent(&parent_path)?;
    let child_path = attempt.child_session_ref.resolve(temp.path());
    let child = complete_child(&child_path, &attempt).await?;
    let draft = child.plan_artifact_projection().plans[&attempt.plan_id].clone();
    drop(child);
    let records = JsonlSessionStore::read_event_records(&child_path)?;
    let draft_index = records
        .iter()
        .position(|record| {
            matches!(
                record.session_log_entry(),
                Ok(Some(SessionLogEntry::Control(
                    ControlEntry::PlanDraftCreated(_)
                )))
            )
        })
        .context("durable draft")?;
    let finalized_index = records
        .iter()
        .position(|record| {
            DurableEventType::from_event_type(&record.stored_event().event_type)
                == Some(DurableEventType::RunFinalized)
        })
        .context("completed child terminal")?;
    for prefix_len in draft_index + 1..=finalized_index {
        assert!(
            recover_plan_review_draft_from_child_records(&attempt, &records[..prefix_len])?
                .is_none()
        );
    }
    let prefix = records[..finalized_index]
        .iter()
        .map(|record| record.stored_event().to_json_line())
        .collect::<Result<Vec<_>>>()?
        .concat();
    std::fs::write(&child_path, prefix)?;
    let _ = draft;
    drop(parent);
    let recovered =
        Session::load_from_store("mock", "model", JsonlSessionStore::new(parent_path)?)?;
    assert_eq!(
        latest_status(&recovered, &attempt),
        PlanReviewAttemptStatus::Interrupted
    );
    assert!(recovered.plan_artifact_projection().plans.is_empty());
    Ok(())
}

#[tokio::test]
async fn plan_review_recovery_rejects_mismatched_source_lineage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (parent, attempt) = seed_parent(&temp.path().join("session.jsonl"))?;
    let child_path = attempt.child_session_ref.resolve(temp.path());
    drop(complete_child(&child_path, &attempt).await?);
    let records = JsonlSessionStore::read_event_records(&child_path)?;
    let mut source_mismatch = attempt.clone();
    source_mismatch.source_turn =
        ConversationTurnRef::new(parent.session_scope_id(), "other-source", "parent-run")?;
    let mut route_mismatch = attempt.clone();
    route_mismatch.route_decision_id = Some(conversation_route_decision_id_for_source(
        &attempt.source_turn,
    ));
    let mut review_mismatch = attempt.clone();
    review_mismatch.plan_review_id = plan_review_id_for_source(&source_mismatch.source_turn);
    let mut workspace_mismatch = attempt;
    workspace_mismatch.workspace_snapshot_id = Some("other-workspace".to_owned());
    for mismatched in [
        source_mismatch,
        route_mismatch,
        review_mismatch,
        workspace_mismatch,
    ] {
        assert!(recover_plan_review_draft_from_child_records(&mismatched, &records).is_err());
    }
    Ok(())
}
