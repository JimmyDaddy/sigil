use std::{path::Path, pin::Pin, process::Command, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    Agent, CompletionRequest, ControlEntry, IntegrationPromotionStatus, IntentApplicationState,
    IntentDigest, IntentDropRequestV1, IntentOperationId, IntentOperationResolution,
    IntentStackVersion, JsonlSessionStore, MessageRole, PermissionMode, PlanApprovalPermission,
    PlanArtifactProjection, PlanTaskStartMode, Provider, ProviderCapabilities, ProviderChunk,
    PublicIntentStackStateV1, ReasoningEffort, ReasoningStreamSupport, RunEvent, SessionLogEntry,
    TaskRunStatus, ToolCall, ToolRegistry, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    stable_workspace_id,
};
use sigil_runtime::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder;
use tempfile::tempdir;

use super::{
    super::{WorkerApprovalCommand, WorkerCommand, WorkerCommandEnvelope, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, spawn_test_worker,
        spawn_test_worker_with_role_provider_builder, test_root_config,
    },
};

fn drop_request() -> IntentDropRequestV1 {
    IntentDropRequestV1 {
        operation_id: IntentOperationId::new("operation_drop_leaf").expect("operation id"),
        stack_version: IntentStackVersion::new(1).expect("stack version"),
        preview_digest: IntentDigest::new(format!("sha256:jcs-v1:{}", "a".repeat(64)))
            .expect("preview digest"),
    }
}

#[test]
fn intent_stack_history_and_permission_boundaries_survive_worker_restart() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-intent-stack.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.permission.mode = PermissionMode::ReadOnly;

    let worker = spawn_test_worker(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root.clone(),
    )?;
    worker.send(WorkerCommand::LoadIntentStack { request_id: 1 })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::IntentStackLoaded { request_id: 1, .. }
        ))?,
        WorkerMessage::IntentStackLoaded {
            request_id: 1,
            stack_state: PublicIntentStackStateV1::NotCreated { .. },
        }
    ));
    worker.shutdown()?;

    let worker = spawn_test_worker(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
    )?;
    worker.send(WorkerCommand::LoadIntentStack { request_id: 2 })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::IntentStackLoaded { request_id: 2, .. }
        ))?,
        WorkerMessage::IntentStackLoaded {
            request_id: 2,
            stack_state: PublicIntentStackStateV1::NotCreated { .. },
        }
    ));

    worker.send(WorkerCommand::ExecuteIntentDrop {
        request_id: 3,
        request: drop_request(),
    })?;
    assert!(matches!(
        worker.recv_until(|message| matches!(
            message,
            WorkerMessage::IntentStackOperationFailed { request_id: 3, .. }
        ))?,
        WorkerMessage::IntentStackOperationFailed {
            request_id: 3,
            error,
        } if error.starts_with("Intent Stack permission is required:")
            && error.contains("read-only permission mode denies Intent drop")
    ));
    worker.shutdown()
}

struct IntentDogfoodRoleProviderBuilder;

#[async_trait]
impl TaskRoleProviderBuilder for IntentDogfoodRoleProviderBuilder {
    async fn build(
        &self,
        _root_config: &sigil_kernel::RootConfig,
        _role: sigil_kernel::AgentRole,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(IntentDogfoodRoleProvider))
    }
}

struct IntentDogfoodRoleProvider;

#[async_trait]
impl Provider for IntentDogfoodRoleProvider {
    fn name(&self) -> &str {
        "intent-dogfood"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
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
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let latest_user_prompt = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref())
            .unwrap_or_default();
        if latest_user_prompt.contains("Produce the single user-visible final answer") {
            return Ok(chunks(vec![
                ProviderChunk::TextDelta("Intent Stack dogfood task complete.".to_owned()),
                ProviderChunk::Done,
            ]));
        }

        let mapping = [
            (
                "implement-retry",
                "retry.txt",
                "retry policy: bounded exponential backoff\n",
            ),
            (
                "add-telemetry",
                "telemetry.txt",
                "telemetry: retry attempts and terminal outcome\n",
            ),
            (
                "document-operations",
                "operations.md",
                "# Operations\n\nRetry alerts and rollback guidance.\n",
            ),
        ];
        let Some((step_id, path, content)) = mapping
            .into_iter()
            .find(|(step_id, _, _)| latest_user_prompt.contains(&format!("\nStep: {step_id}\n")))
        else {
            return Err(anyhow!(
                "unexpected Intent dogfood role prompt: {latest_user_prompt}"
            ));
        };
        let tool_used = request
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool);
        if tool_used {
            return Ok(chunks(vec![
                ProviderChunk::TextDelta(format!("{step_id} completed")),
                ProviderChunk::Done,
            ]));
        }

        let call_id = format!("write-{step_id}");
        let args_json = serde_json::to_string(&serde_json::json!({
            "path": path,
            "content": content,
        }))?;
        Ok(chunks(vec![
            ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: "write_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: args_json.clone(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: "write_file".to_owned(),
                args_json,
            }),
            ProviderChunk::Done,
        ]))
    }
}

fn chunks(values: Vec<ProviderChunk>) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    Box::pin(stream::iter(values.into_iter().map(Ok::<_, anyhow::Error>)))
}

#[test]
fn accepted_plan_intents_run_in_parallel_promote_reload_and_drop_through_worker_loop() -> Result<()>
{
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("retry.txt"), "retry policy: none\n")?;
    std::fs::write(workspace_root.join("telemetry.txt"), "telemetry: none\n")?;
    std::fs::write(
        workspace_root.join("operations.md"),
        "# Operations\n\nNone.\n",
    )?;
    init_git_repo(&workspace_root)?;
    run_git(&workspace_root, &["add", "."])?;
    run_git(&workspace_root, &["commit", "-qm", "initial"])?;
    let workspace_root = std::fs::canonicalize(workspace_root)?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-intent-stack-dogfood.jsonl");
    JsonlSessionStore::new(&session_log_path)?.append(&SessionLogEntry::Control(
        ControlEntry::WorkspaceTrustDecision(WorkspaceTrustDecisionEntry {
            workspace_id: stable_workspace_id(&workspace_root)?,
            workspace_trust_snapshot_id: "workspace-trust:intent-stack-dogfood".to_owned(),
            trust: WorkspaceTrust::Trusted,
            decided_by_event_id: None,
            reason: Some("Intent Stack worker-loop dogfood trust".to_owned()),
        }),
    ))?;
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.task.max_parallel_changeset_steps = 3;
    root_config.task.max_subagents = 8;

    let plan_text = r#"Plan:

```sigil-plan-v2
{
  "summary": "Implement retry behavior, retry telemetry, and operator guidance",
  "intents": [
    {
      "intent_alias": "retry",
      "title": "Retry behavior",
      "statement": "Use bounded exponential backoff for failed operations.",
      "acceptance_criteria": [{
        "criterion_alias": "retry-content",
        "statement": "The retry policy is recorded in retry.txt.",
        "required": true
      }],
      "depends_on_aliases": []
    },
    {
      "intent_alias": "telemetry",
      "title": "Retry telemetry",
      "statement": "Expose retry attempts and terminal outcomes.",
      "acceptance_criteria": [{
        "criterion_alias": "telemetry-content",
        "statement": "Retry telemetry is recorded in telemetry.txt.",
        "required": true
      }],
      "depends_on_aliases": ["retry"]
    },
    {
      "intent_alias": "operations",
      "title": "Operator guidance",
      "statement": "Document retry alerts and rollback guidance.",
      "acceptance_criteria": [{
        "criterion_alias": "operations-content",
        "statement": "Operator guidance is recorded in operations.md.",
        "required": true
      }],
      "depends_on_aliases": ["retry"]
    }
  ],
  "steps": [
    {
      "step_id": "implement-retry",
      "title": "Implement retry policy",
      "role": "subagent_write",
      "depends_on": [],
      "intent_aliases": ["retry"],
      "mode": "write",
      "isolation": "worktree",
      "target_paths": ["retry.txt"]
    },
    {
      "step_id": "add-telemetry",
      "title": "Add retry telemetry",
      "role": "subagent_write",
      "depends_on": [],
      "intent_aliases": ["telemetry"],
      "mode": "write",
      "isolation": "worktree",
      "target_paths": ["telemetry.txt"]
    },
    {
      "step_id": "document-operations",
      "title": "Document operator guidance",
      "role": "subagent_write",
      "depends_on": [],
      "intent_aliases": ["operations"],
      "mode": "write",
      "isolation": "worktree",
      "target_paths": ["operations.md"]
    }
  ],
  "target_paths": ["retry.txt", "telemetry.txt", "operations.md"]
}
```
"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(plan_text.to_owned()),
        ProviderChunk::Done,
    ])]);
    let mut tools = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut tools);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config.clone(),
        session_log_path.clone(),
        Agent::new(provider, tools),
        workspace_root.clone(),
        Arc::new(IntentDogfoodRoleProviderBuilder),
    )?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "Implement retry, telemetry, and operations guidance as separate outcomes."
            .to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::PlanRunStarted { .. })
    })?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let draft = PlanArtifactProjection::from_entries(&entries)
        .latest_pending_plan()
        .context("plan run should append an intent-enabled draft")?
        .clone();
    assert_eq!(
        draft
            .intent_proposal
            .as_ref()
            .context("plan should retain the model's semantic intent proposal")?
            .intents
            .len(),
        3
    );

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreateAndRun,
        permission_grant: Some(PlanApprovalPermission::WorkspaceEdits),
    })?;
    let created = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::TaskCreatedFromPlan { .. })
    })?;
    let WorkerMessage::TaskCreatedFromPlan {
        entry: created_task,
        entries,
        ..
    } = created
    else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };
    let accepted_plan = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.task_id == created_task.task_id =>
            {
                Some(plan)
            }
            _ => None,
        })
        .context("accepted task plan should be durable")?;
    assert_eq!(accepted_plan.steps.len(), 3);
    assert!(
        accepted_plan
            .steps
            .iter()
            .all(|step| step.intent_refs.len() == 1)
    );

    let _ = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::TaskRunStarted { .. })
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut unexpected_approvals = Vec::new();
    let finished = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for Intent dogfood task"));
        }
        let message = worker.recv_with_timeout(remaining)?;
        match &message {
            WorkerMessage::Event(event) => {
                if let RunEvent::ToolApprovalRequested {
                    approval_identity,
                    call,
                    subjects,
                    operation,
                    ..
                } = event.as_ref()
                {
                    unexpected_approvals.push(format!("{}:{operation:?}:{subjects:?}", call.id));
                    worker.send(WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
                        format!("intent-dogfood-{}", approval_identity.approval_request_id),
                        "sigil-tui-test",
                        session_log_path.display().to_string(),
                        WorkerApprovalCommand::Decision {
                            call_id: call.id.clone(),
                            approval_request_id: approval_identity.approval_request_id.clone(),
                            approved: true,
                        },
                    )))?;
                }
            }
            WorkerMessage::TaskRunFinished { .. } | WorkerMessage::RunFailed(_) => break message,
            _ => {}
        }
    };
    if let WorkerMessage::RunFailed(error) = finished {
        return Err(anyhow!("Intent dogfood task failed: {error}"));
    }
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("terminal task message checked above");
    };
    if status != TaskRunStatus::Paused {
        return Err(anyhow!(
            "Intent dogfood task ended as {status:?}; durable entries: {entries:#?}"
        ));
    }
    if !unexpected_approvals.is_empty() {
        return Err(anyhow!(
            "accepted plan workspace grant did not cover child writes: {unexpected_approvals:#?}"
        ));
    }
    let review = sigil_kernel::task_integration_review_product(&entries).ok_or_else(|| {
        let relevant = entries
            .iter()
            .filter_map(|entry| {
                let debug = format!("{entry:?}");
                (debug.contains("Integration") || debug.contains("TaskRun")).then_some(debug)
            })
            .collect::<Vec<_>>();
        anyhow!(
            "parallel changesets should produce an exact integration review; relevant entries: {relevant:#?}"
        )
    })?;
    assert!(
        review.preview.intent_binding.is_some(),
        "promotion preview must bind the accepted task intents"
    );

    worker.send(WorkerCommand::AcceptTaskIntegration {
        request: review.request.clone(),
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut integration_notices = Vec::new();
    let accepted = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for Intent dogfood promotion"));
        }
        let message = worker.recv_with_timeout(remaining)?;
        match message {
            WorkerMessage::Notice(notice) => integration_notices.push(notice),
            WorkerMessage::Event(event) => {
                if let RunEvent::Notice(notice) = event.as_ref() {
                    integration_notices.push(notice.clone());
                }
            }
            WorkerMessage::TaskIntegrationAccepted { .. }
            | WorkerMessage::TaskIntegrationAcceptanceFailed { .. } => break message,
            _ => {}
        }
    };
    let WorkerMessage::TaskIntegrationAccepted {
        promotion_status,
        entries,
        ..
    } = accepted
    else {
        let WorkerMessage::TaskIntegrationAcceptanceFailed { error, .. } = accepted else {
            unreachable!("terminal integration message checked above");
        };
        return Err(anyhow!("Intent dogfood promotion failed: {error}"));
    };
    assert_eq!(promotion_status, IntegrationPromotionStatus::Promoted);
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("retry.txt"))?,
        "retry policy: bounded exponential backoff\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("telemetry.txt"))?,
        "telemetry: retry attempts and terminal outcome\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("operations.md"))?,
        "# Operations\n\nRetry alerts and rollback guidance.\n"
    );
    assert!(
        !entries.is_empty(),
        "promotion should return the reloaded durable session"
    );
    let applied_changesets = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ChangeSetApplied(result)) => {
                Some((result.id.as_str(), result.status))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        applied_changesets.len(),
        3,
        "promotion should record one applied result per Intent-bound ChangeSet: {applied_changesets:?}"
    );
    worker.shutdown()?;

    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root.clone(),
    )?;
    worker.send(WorkerCommand::LoadIntentStack { request_id: 10 })?;
    let loaded = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(
            message,
            WorkerMessage::IntentStackLoaded { request_id: 10, .. }
        )
    })?;
    let WorkerMessage::IntentStackLoaded {
        stack_state: PublicIntentStackStateV1::Available { stack, .. },
        ..
    } = loaded
    else {
        return Err(anyhow!("restarted worker should reload the Intent Stack"));
    };
    assert_eq!(stack.intents.len(), 3);
    assert!(
        stack
            .intents
            .iter()
            .all(|intent| intent.application_state == IntentApplicationState::Applied),
        "promoted Intent layers were not all applied: {:?}; notices: {integration_notices:?}",
        stack
            .intents
            .iter()
            .map(|intent| (
                intent.title.as_str(),
                intent.application_state,
                intent.exclusive_artifact_count,
                intent.shared_artifact_count,
                intent.drifted_artifact_count,
            ))
            .collect::<Vec<_>>()
    );
    let telemetry = stack
        .intents
        .iter()
        .find(|intent| intent.title == "Retry telemetry")
        .context("telemetry intent should be present")?;
    let telemetry_ref = telemetry.intent_ref.clone();
    let retry_ref = stack
        .intents
        .iter()
        .find(|intent| intent.title == "Retry behavior")
        .context("retry intent should be present")?
        .intent_ref
        .clone();

    worker.send(WorkerCommand::PreviewIntentDrop {
        request_id: 11,
        intent_ref: telemetry_ref.clone(),
    })?;
    let previewed = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(
            message,
            WorkerMessage::IntentDropPreviewed { request_id: 11, .. }
        )
    })?;
    let WorkerMessage::IntentDropPreviewed { preview, .. } = previewed else {
        unreachable!("recv_until only returns IntentDropPreviewed");
    };
    assert!(preview.target_is_leaf);
    assert_eq!(preview.target_intents, vec![telemetry_ref]);
    worker.send(WorkerCommand::ExecuteIntentDrop {
        request_id: 12,
        request: IntentDropRequestV1 {
            operation_id: preview.operation_id.clone(),
            stack_version: preview.stack_version,
            preview_digest: preview.preview_digest.clone(),
        },
    })?;
    let dropped = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(
            message,
            WorkerMessage::IntentDropCompleted { request_id: 12, .. }
                | WorkerMessage::IntentStackOperationFailed { request_id: 12, .. }
        )
    })?;
    let WorkerMessage::IntentDropCompleted {
        execution,
        stack_state: PublicIntentStackStateV1::Available { stack, .. },
        ..
    } = dropped
    else {
        let WorkerMessage::IntentStackOperationFailed { error, .. } = dropped else {
            unreachable!("terminal Intent drop message checked above");
        };
        return Err(anyhow!("Intent dogfood drop failed: {error}"));
    };
    assert_eq!(execution.resolution, IntentOperationResolution::Committed);
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("telemetry.txt"))?,
        "telemetry: none\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("retry.txt"))?,
        "retry policy: bounded exponential backoff\n"
    );
    assert!(stack.intents.iter().any(|intent| {
        intent.intent_ref == retry_ref
            && intent.application_state == IntentApplicationState::Applied
    }));
    assert!(stack.intents.iter().any(|intent| {
        intent.intent_ref == execution.preview.target_intents[0]
            && intent.application_state == IntentApplicationState::Dropped
    }));
    worker.shutdown()
}

fn init_git_repo(workspace_root: &Path) -> Result<()> {
    run_git(workspace_root, &["init", "-q"])?;
    run_git(
        workspace_root,
        &["config", "user.email", "sigil-tests@example.invalid"],
    )?;
    run_git(workspace_root, &["config", "user.name", "Sigil Tests"])
}

fn run_git(workspace_root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
