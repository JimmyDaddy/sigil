use std::{path::Path, pin::Pin, process::Command, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    Agent, CompletionRequest, ControlEntry, IntentDigest, IntentDropRequestV1, IntentOperationId,
    IntentStackVersion, JsonlSessionStore, MessageRole, PermissionMode, PlanApprovalPermission,
    PlanArtifactProjection, PlanTaskStartMode, Provider, ProviderCapabilities, ProviderChunk,
    PublicIntentStackStateV1, ReasoningEffort, ReasoningStreamSupport, SessionLogEntry,
    TaskRunStatus, ToolCall, ToolRegistry, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    stable_workspace_id,
};
use sigil_runtime::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder;
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, routed_unauthenticated_test_root_config, spawn_test_worker,
        spawn_test_worker_with_role_provider_builder, submit_plan_draft_chunks, test_root_config,
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
        if latest_user_prompt
            .contains("Execute the following complete, user-approved Task objective now")
        {
            return Ok(chunks(vec![
                ProviderChunk::TextDelta(
                    "Direct executor received the complete approved Plan.".to_owned(),
                ),
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
fn approved_plan_intent_proposals_do_not_become_execution_authority() -> Result<()> {
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
    let mut root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    root_config.task.max_parallel_changeset_steps = 3;
    root_config.task.max_subagents = 8;

    let plan_args = r#"{
  "schema_version": 2,
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
  "target_paths": ["retry.txt", "telemetry.txt", "operations.md"],
  "suggested_checks": ["cargo test -p sigil-tui intent_stack"]
}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(submit_plan_draft_chunks(
        "intent-stack-plan-draft",
        plan_args,
    ))]);
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
    // Model-authored intent/DAG metadata remains Plan content, not Task execution authority.
    let task_projection = sigil_kernel::TaskStateProjection::from_entries(&entries);
    let task = task_projection
        .tasks
        .get(&created_task.task_id)
        .context("direct Task execution authority should be durable")?;
    assert!(task.plans.is_empty());
    assert!(task.latest_plan_version.is_none());
    assert!(task.direct_execution_admission.is_some());

    let _ = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::TaskRunStarted { .. })
    })?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(
            message,
            WorkerMessage::TaskRunFinished { .. } | WorkerMessage::RunFailed(_)
        )
    })?;
    if let WorkerMessage::RunFailed(error) = finished {
        return Err(anyhow!("direct Plan task failed: {error}"));
    }
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("terminal task message checked above");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskMaterializationPreparedV1(_))
    )));
    assert!(matches!(
        sigil_kernel::Session::load_from_store(
            "planned",
            "planned-model",
            JsonlSessionStore::new(&session_log_path)?,
        )?
        .public_intent_stack_state_for_workspace(&workspace_root)?,
        PublicIntentStackStateV1::NotCreated { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("retry.txt"))?,
        "retry policy: none\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("telemetry.txt"))?,
        "telemetry: none\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("operations.md"))?,
        "# Operations\n\nNone.\n"
    );
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
