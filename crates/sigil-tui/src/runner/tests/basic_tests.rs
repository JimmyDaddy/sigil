use std::{fs, time::Duration};

use anyhow::Result;
use sigil_kernel::{
    Agent, AgentRole, ControlEntry, ConversationInputKind, ConversationInputQueueId,
    ConversationInputQueuedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, JsonlSessionStore, McpServerConfig, McpServerStartup,
    PlanApprovalPermission, PlanArtifactProjection, PlanDecision, PlanTaskStartMode, ProviderChunk,
    ReasoningEffort, RunEvent, SessionLogEntry, SkillDescriptor, SkillRunMode, SkillSource,
    SkillTrustState, TaskRunStatus, ToolCall, ToolErrorKind, ToolExecutionStatus, ToolRegistry,
    ToolResultStatus,
};
use tempfile::tempdir;

use super::{
    super::{
        McpActivationStatus, QueueMoveDirection, WorkerCommand, WorkerMessage, spawn_agent_worker,
        worker_loop::skill_child_agent_role,
    },
    common::{
        PlannedProvider, StreamPlan, WriteTool, spawn_test_worker, test_root_config,
        wait_for_session_entry,
    },
};

fn write_workspace_skill(workspace_root: &std::path::Path, id: &str, body: &str) -> Result<()> {
    let path = workspace_root
        .join(".sigil")
        .join("skills")
        .join(id)
        .join("SKILL.md");
    fs::create_dir_all(path.parent().expect("skill path should have parent"))?;
    fs::write(path, body)?;
    Ok(())
}

fn test_child_session_skill(agent: Option<&str>) -> SkillDescriptor {
    SkillDescriptor {
        id: "reviewer".to_owned(),
        name: "Reviewer".to_owned(),
        description: "Review current changes.".to_owned(),
        when_to_use: None,
        root: ".sigil/agents/reviewer.md".into(),
        entrypoint: ".sigil/agents/reviewer.md".into(),
        source: SkillSource::Workspace,
        sha256: "hash".to_owned(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::ChildSession,
        agent: agent.map(str::to_owned),
        argument_hint: None,
        allowed_tools: Default::default(),
        disallowed_tools: Default::default(),
        path_patterns: Vec::new(),
    }
}

fn structured_plan_text(summary: &str, title: &str, path: &str) -> String {
    format!(
        r#"Plan:

```sigil-plan-v1
{{
  "summary": "{summary}",
  "steps": [
    {{
      "id": "step-1",
      "title": "{title}",
      "target_paths": ["{path}"]
    }}
  ],
  "target_paths": ["{path}"],
  "suggested_checks": [
    {{
      "check_spec_id": "cargo-test",
      "command": "cargo",
      "args": ["test", "-p", "sigil-kernel", "plan"]
    }}
  ]
}}
```
"#
    )
}

fn write_fake_server_script(path: &std::path::Path) -> Result<()> {
    fs::write(
        path,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fake","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
"#,
    )?;
    Ok(())
}

#[test]
fn child_session_skill_agent_hint_defaults_to_read_role() {
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(None)),
        AgentRole::SubagentRead
    );
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(Some("reviewer"))),
        AgentRole::SubagentRead
    );
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(Some("subagent-write"))),
        AgentRole::SubagentWrite
    );
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(Some("writer"))),
        AgentRole::SubagentWrite
    );
}
#[test]
fn submit_prompt_emits_started_event_and_finished_messages() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta("hello from worker".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let started = worker.recv()?;
    assert!(matches!(
        started,
        WorkerMessage::RunStarted { ref prompt } if prompt == "hello"
    ));

    let text_event = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
        )
    })?;
    assert!(matches!(
        text_event,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
    ));

    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    assert!(matches!(
        finished,
        WorkerMessage::RunFinished { ref result, ref entries }
            if result.final_text == "hello from worker"
                && result.tool_calls == 0
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("hello")))
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn submit_plan_prompt_uses_readonly_registry_and_does_not_execute_write_tool() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-plan.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "call-write".to_owned(),
                delta: r#"{"path":"note.txt"}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
                args_json: r#"{"path":"note.txt"}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta(structured_plan_text(
                "plan after blocked write",
                "Inspect README.md after blocked write",
                "README.md",
            )),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta(structured_plan_text(
                "plan after blocked write",
                "Inspect README.md after blocked write",
                "README.md",
            )),
            ProviderChunk::Done,
        ]),
    ]);
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(WriteTool));
    let agent = Agent::new(provider, registry);
    let note_path = workspace_root.join("note.txt");
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "inspect first".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let started = worker.recv()?;
    assert!(matches!(
        started,
        WorkerMessage::PlanRunStarted { ref prompt } if prompt == "inspect first"
    ));

    let tool_result = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolResult(result)
                    if result.tool_name == "write_file"
                        && result.content.contains("not available in this role scope"))
        )
    })?;
    assert!(matches!(
        tool_result,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolResult(result)
                if result.tool_name == "write_file"
                    && result.is_error()
                    && result.content.contains("not available in this role scope"))
    ));

    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { result, entries } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    assert!(result.final_text.contains("plan after blocked write"));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.tool_name == "write_file"
                && execution.status == ToolExecutionStatus::Failed
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft))
            if draft.summary == "plan after blocked write"
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::User(message) if message.content.as_deref() == Some("inspect first")
    )));
    assert!(!note_path.exists());

    worker.shutdown()?;
    Ok(())
}

#[test]
fn create_task_from_plan_command_appends_paused_task_handoff_entries() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-plan-task.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(structured_plan_text(
            "Update README",
            "Update README.md",
            "README.md",
        )),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan docs update".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();
    assert_eq!(draft.suggested_checks.len(), 1);

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    })?;
    let created = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))?;
    let WorkerMessage::TaskCreatedFromPlan {
        entry,
        start_mode,
        entries,
    } = created
    else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };

    assert_eq!(start_mode, PlanTaskStartMode::CreatePaused);
    assert_eq!(entry.plan_id, draft.plan_id);
    assert_eq!(entry.plan_hash, draft.plan_hash);
    assert_eq!(entry.task_plan_version, 0);
    assert!(entry.stale_reason.is_none());
    assert!(entry.step_mapping.is_empty());
    let created_task_id = entry.task_id.clone();
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
            if decision.decision == PlanDecision::Accepted
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.status == TaskRunStatus::Paused
                && run.objective.contains("Execute the following user-approved structured plan")
    )));
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_))))
    );
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(created))
            if created.task_id == created_task_id
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn reject_plan_command_appends_rejected_decision_and_clears_pending_projection() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-reject.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(structured_plan_text(
            "Update README",
            "Update README.md",
            "README.md",
        )),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan docs update".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();

    worker.send(WorkerCommand::RejectPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
    })?;
    let rejected =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRejected { .. }))?;
    let WorkerMessage::PlanRejected { entry, entries } = rejected else {
        unreachable!("recv_until only returns PlanRejected");
    };

    assert_eq!(entry.plan_id, draft.plan_id);
    assert_eq!(entry.plan_hash, draft.plan_hash);
    assert_eq!(entry.decision, PlanDecision::Rejected);
    assert_eq!(entry.reason.as_deref(), Some("discarded plan"));
    let projection = PlanArtifactProjection::from_entries(&entries);
    assert!(projection.latest_pending_plan().is_none());
    assert_eq!(projection.latest_decision(&entry.plan_id), Some(&entry));
    assert!(!projection.task_created_for_plan(&entry.plan_id));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
            if decision.decision == PlanDecision::Rejected
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn create_task_from_plan_run_now_starts_normal_task_planner_without_prebuilt_plan() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-task-run-now.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta(structured_plan_text(
                "Fix README typo",
                "Update the approved README typo",
                "README.md",
            )),
            ProviderChunk::Done,
        ]),
        StreamPlan::Pending,
    ]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan docs typo fix".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreateAndRun,
        permission_grant: None,
    })?;
    let created = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))?;
    let WorkerMessage::TaskCreatedFromPlan {
        entry,
        start_mode,
        entries,
    } = created
    else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };
    assert_eq!(start_mode, PlanTaskStartMode::CreateAndRun);
    assert_eq!(entry.task_plan_version, 0);
    assert!(entry.step_mapping.is_empty());
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_))))
    );

    let started =
        worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    assert!(matches!(
        started,
        WorkerMessage::TaskRunStarted { ref objective, .. }
            if objective.contains("Execute the following user-approved structured plan")
                && objective.contains("Update the approved README typo")
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn create_task_from_plan_records_stale_reason_after_workspace_change() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    fs::write(workspace_root.join("README.md"), "before\n")?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-task-stale.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(structured_plan_text(
            "Update README",
            "Update README.md",
            "README.md",
        )),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        agent,
        workspace_root.clone(),
    )?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan docs update".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();
    assert!(draft.workspace_snapshot_id.is_some());

    fs::write(workspace_root.join("README.md"), "after\n")?;
    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreatePaused,
        permission_grant: None,
    })?;
    let created = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))?;
    let WorkerMessage::TaskCreatedFromPlan { entry, entries, .. } = created else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };

    let stale_reason = entry
        .stale_reason
        .as_deref()
        .expect("changed workspace should mark plan stale");
    assert!(stale_reason.contains("workspace changed since plan"));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(created))
            if created.stale_reason.as_deref() == Some(stale_reason)
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn create_task_from_plan_with_scoped_edits_appends_task_bound_grant() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    fs::write(workspace_root.join("README.md"), "before\n")?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-task-grant.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(structured_plan_text(
            "Update README",
            "Update README.md",
            "README.md",
        )),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan docs update".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreatePaused,
        permission_grant: Some(PlanApprovalPermission::WorkspaceEdits),
    })?;
    let created = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))?;
    let WorkerMessage::TaskCreatedFromPlan { entry, entries, .. } = created else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };

    let grant = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanPermissionGranted(grant)) => Some(grant),
            _ => None,
        })
        .expect("scoped edits should append a permission grant");
    assert_eq!(grant.plan_id, draft.plan_id);
    assert_eq!(grant.plan_hash, draft.plan_hash);
    assert_eq!(grant.task_id, entry.task_id);
    assert_eq!(grant.permission, PlanApprovalPermission::WorkspaceEdits);
    assert_eq!(grant.scope.workspace_paths, vec!["README.md".to_owned()]);
    assert!(grant.workspace_snapshot_id.is_some());

    worker.shutdown()?;
    Ok(())
}

#[test]
fn inline_skill_invocation_applies_skill_tool_scope() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    write_workspace_skill(
        &workspace_root,
        "readonly",
        r#"---
name: readonly
description: Read only skill.
trust: trusted
user-invocable: true
run-as: inline
disallowed-tools: [write_file]
---

# Readonly
"#,
    )?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-inline-skill.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "call-write".to_owned(),
                delta: r#"{"path":"note.txt"}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
                args_json: r#"{"path":"note.txt"}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("done".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(WriteTool));
    let agent = Agent::new(provider, registry);
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::InvokeInlineSkill {
        skill_id: "readonly".to_owned(),
        arguments: "target".to_owned(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    let _ =
        worker.recv_until(|message| matches!(message, WorkerMessage::SkillRunStarted { .. }))?;
    let tool_error = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(
                    event.as_ref(),
                    RunEvent::ToolResult(result)
                        if matches!(
                            &result.status,
                            ToolResultStatus::Error(error)
                                if error.kind == ToolErrorKind::Internal
                                    && error.message.contains("not available in this role scope")
                        )
                )
        )
    })?;
    assert!(matches!(tool_error, WorkerMessage::Event(_)));
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;

    worker.shutdown()?;
    Ok(())
}

#[test]
fn worker_revalidates_skill_run_mode_after_loading() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    write_workspace_skill(
        &workspace_root,
        "child-only",
        r#"---
name: child-only
description: Child only skill.
trust: trusted
user-invocable: true
run-as: child-session
---

# Child Only
"#,
    )?;
    let session_log_path = temp.path().join(".sigil/sessions/session-skill-mode.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::InvokeInlineSkill {
        skill_id: "child-only".to_owned(),
        arguments: String::new(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    match failure {
        WorkerMessage::RunFailed(error) => {
            assert!(
                error.contains(
                    "agent child-only is configured for child_session mode, not inline skill mode"
                ),
                "{error}"
            );
        }
        other => panic!("expected run failure, got {other:?}"),
    }

    worker.shutdown()?;
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_provider_configuration_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;

    let message = message_rx.recv_timeout(Duration::from_secs(3))?;

    assert!(matches!(
        message,
        WorkerMessage::RunFailed(ref error)
            if error.contains("missing [providers.deepseek] in sigil.toml")
    ));
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_eager_mcp_failure_without_stopping_worker() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.providers.insert(
        "deepseek".to_owned(),
        serde_json::json!({
            "api_key": "test-key"
        }),
    );
    root_config.mcp_servers.push(McpServerConfig {
        name: "required-eager".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Eager,
        ..McpServerConfig::default()
    });

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;

    let failure = loop {
        let message = message_rx.recv_timeout(Duration::from_secs(3))?;
        if matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Failed { .. },
                ..
            }
        ) {
            break message;
        }
    };

    assert!(matches!(
        failure,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Failed { ref error },
        } if server_name == "required-eager"
            && error.contains("failed to spawn MCP server required-eager")
    ));

    command_tx.send(WorkerCommand::CancelRun)?;
    let still_running = loop {
        let message = message_rx.recv_timeout(Duration::from_secs(3))?;
        if matches!(message, WorkerMessage::RunFailed(_)) {
            break message;
        }
    };
    assert!(matches!(
        still_running,
        WorkerMessage::RunFailed(ref error) if error == "no active run to cancel"
    ));
    let _ = command_tx.send(WorkerCommand::Shutdown);
    Ok(())
}

#[test]
fn activate_lazy_mcp_reports_notice_when_no_lazy_servers_match() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("missing".to_owned()),
    })?;
    let activating = worker.recv()?;
    assert!(matches!(
        activating,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Activating,
        } if server_name == "missing"
    ));
    let status = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Deferred,
                ..
            }
        )
    })?;
    assert!(matches!(
        status,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Deferred,
        } if server_name == "missing"
    ));
    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;

    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text) if text == "no lazy MCP tools activated for missing"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn refresh_mcp_server_reports_deferred_for_unknown_server() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::RefreshMcpServer {
        server_name: "missing".to_owned(),
    })?;
    let refreshing = worker.recv()?;
    assert!(matches!(
        refreshing,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Refreshing,
        } if server_name == "missing"
    ));
    let status = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Deferred,
                ..
            }
        )
    })?;
    assert!(matches!(
        status,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Deferred,
        } if server_name == "missing"
    ));
    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;

    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text) if text == "MCP refresh skipped for unknown server missing"
    ));

    Ok(())
}

#[test]
fn activate_lazy_mcp_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    worker.send(WorkerCommand::ActivateLazyMcp { server_name: None })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error == "cannot activate MCP while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn activate_lazy_mcp_reports_failed_status_for_required_server_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.mcp_servers.push(McpServerConfig {
        name: "required-lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("required-lazy".to_owned()),
    })?;
    let status = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Failed { .. },
                ..
            }
        )
    })?;

    assert!(matches!(
        status,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Failed { ref error },
        } if server_name == "required-lazy" && error.contains("failed to spawn MCP server required-lazy")
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn activate_lazy_mcp_reports_ready_status_when_server_registers_tools() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let script_path = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(&script_path)?;

    let session_log_path = temp.path().join(".sigil/sessions/session-ready-lazy.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.mcp_servers.push(McpServerConfig {
        name: "ready-lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script_path.to_string_lossy().to_string()],
        startup: McpServerStartup::Lazy,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });

    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("ready-lazy".to_owned()),
    })?;
    let activating = worker.recv()?;
    assert!(matches!(
        activating,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Activating,
        } if server_name == "ready-lazy"
    ));

    let ready = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                server_name: Some(server_name),
                status: McpActivationStatus::Ready {
                    added_tools: 1,
                    process_coverage: Some(process_coverage),
                },
            } if server_name == "ready-lazy"
                && process_coverage == "local stdio outside local sandbox"
        )
    })?;
    assert!(matches!(
        ready,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Ready {
                added_tools: 1,
                process_coverage: Some(ref process_coverage),
            },
        } if server_name == "ready-lazy"
            && process_coverage == "local stdio outside local sandbox"
    ));

    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;
    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text)
            if text == "activated 1 lazy MCP tools for ready-lazy"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    worker.send(WorkerCommand::CheckChangedFilesDiagnostics)?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error == "cannot check changes while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_provider_build_failure() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-failure.jsonl");
    let root_config = test_root_config(&workspace_root, "missing-provider", "planned-model");

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let failure = message_rx.recv_timeout(std::time::Duration::from_secs(3))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error.contains("unsupported provider missing-provider")
    ));
    Ok(())
}

#[test]
fn submit_prompt_queues_second_run_while_agent_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-busy.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "first".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "second".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let update = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { .. })
    })?;

    assert!(matches!(
        update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. }
            if items.len() == 1
                && items[0].queued.prompt == "second"
                && items[0].queued.kind == ConversationInputKind::Chat
                && items[0].status == ConversationInputStatus::Queued
    ));
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued))
            if queued.prompt == "second"
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn submit_plan_prompt_queues_while_agent_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-plan-busy.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "first".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan second".to_owned(),
        reasoning_effort: ReasoningEffort::High,
    })?;
    let update = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { .. })
    })?;

    assert!(matches!(
        update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. }
            if items.len() == 1
                && items[0].queued.prompt == "plan second"
                && items[0].queued.kind == ConversationInputKind::PlanPrompt
                && items[0].status == ConversationInputStatus::Queued
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn queue_conversation_input_persists_while_agent_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-queue.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "first".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::QueueConversationInput {
        prompt: "queued while running".to_owned(),
        kind: ConversationInputKind::Chat,
        target: ConversationInputTarget::MainThread,
        reasoning_effort: ReasoningEffort::High,
    })?;
    wait_for_session_entry(&session_log_path, |entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued))
                if queued.prompt == "queued while running"
        )
    })?;
    let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued))
            if queued.prompt == "queued while running"
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn queue_control_commands_persist_and_update_projection() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-queue-controls.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SetConversationQueuePaused { paused: true })?;
    let _ = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { paused: true, .. }
        )
    })?;

    worker.send(WorkerCommand::QueueConversationInput {
        prompt: "first queued".to_owned(),
        kind: ConversationInputKind::Chat,
        target: ConversationInputTarget::MainThread,
        reasoning_effort: ReasoningEffort::High,
    })?;
    let _ = worker.recv_until(|message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { items, .. } if items.len() == 1)
    })?;
    worker.send(WorkerCommand::QueueConversationInput {
        prompt: "second queued".to_owned(),
        kind: ConversationInputKind::Chat,
        target: ConversationInputTarget::MainThread,
        reasoning_effort: ReasoningEffort::High,
    })?;
    let _ = worker.recv_until(|message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { items, .. } if items.len() == 2)
    })?;

    worker.send(WorkerCommand::SetConversationQueuePaused { paused: true })?;
    let paused_update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { paused: true, .. }
        )
    })?;
    assert!(matches!(
        paused_update,
        WorkerMessage::ConversationQueueUpdated { paused: true, .. }
    ));

    let queue_2 = sigil_kernel::ConversationInputQueueId::new("queue_2")?;
    worker.send(WorkerCommand::EditQueuedConversationInput {
        queue_id: queue_2.clone(),
        prompt: "second edited".to_owned(),
        reasoning_effort: ReasoningEffort::Low,
    })?;
    let edit_update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { items, .. }
                if items.iter().any(|item| item.queued.prompt == "second edited")
        )
    })?;
    assert!(matches!(
        edit_update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. }
            if items[1].queued.queue_id.as_str() == "queue_2"
                && items[1].queued.prompt == "second edited"
    ));

    worker.send(WorkerCommand::MoveQueuedConversationInput {
        queue_id: queue_2.clone(),
        direction: QueueMoveDirection::Up,
    })?;
    let move_update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { items, .. }
                if items.first().is_some_and(|item| item.queued.queue_id.as_str() == "queue_2")
        )
    })?;
    assert!(matches!(
        move_update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. }
            if items[0].queued.prompt == "second edited"
    ));

    let queue_1 = sigil_kernel::ConversationInputQueueId::new("queue_1")?;
    worker.send(WorkerCommand::PromoteQueuedConversationInput {
        queue_id: queue_1.clone(),
    })?;
    let promote_update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { items, paused: false, .. }
                if items.first().is_some_and(|item| item.queued.queue_id.as_str() == "queue_1")
        )
    })?;
    assert!(matches!(
        promote_update,
        WorkerMessage::ConversationQueueUpdated { ref items, paused: false, .. }
            if items[0].queued.prompt == "first queued"
    ));

    worker.send(WorkerCommand::CancelQueuedConversationInput { queue_id: queue_2 })?;
    let cancel_update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { items, .. }
                if items.len() == 1
                    && items[0].queued.queue_id.as_str() == "queue_1"
        )
    })?;
    assert!(matches!(
        cancel_update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. }
            if items.len() == 1 && items[0].queued.prompt == "first queued"
    ));

    let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputQueueControl(control))
            if control.action == sigil_kernel::ConversationInputQueueControlAction::Pause
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputEdited(edited))
            if edited.queue_id.as_str() == "queue_2" && edited.prompt == "second edited"
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputReordered(reordered))
            if reordered.queue_id.as_str() == "queue_1" && reordered.after_queue_id.is_none()
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.queue_id.as_str() == "queue_2"
                && status.status == sigil_kernel::ConversationInputStatus::Cancelled
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn failed_queued_run_pauses_queue() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-queue-failure.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Fail("queued failure")]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::QueueConversationInput {
        prompt: "queued will fail".to_owned(),
        kind: ConversationInputKind::Chat,
        target: ConversationInputTarget::MainThread,
        reasoning_effort: ReasoningEffort::High,
    })?;
    let _ = worker.recv_until(|message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { items, .. } if items.len() == 1)
    })?;
    let _ = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueDispatchStarted { prompt, .. }
                if prompt == "queued will fail"
        )
    })?;

    let paused_update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { entries, paused: true, .. }
                if entries.iter().any(|entry| matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                        if status.status == ConversationInputStatus::Rejected
                ))
        )
    })?;
    assert!(matches!(
        paused_update,
        WorkerMessage::ConversationQueueUpdated { ref entries, paused: true, .. }
            if entries.iter().any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id.as_str() == "queue_1"
                        && status.status == ConversationInputStatus::Rejected
            ))
    ));
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert!(matches!(failure, WorkerMessage::RunFailed(ref error) if error == "queued failure"));

    let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputQueueControl(control))
            if control.action == sigil_kernel::ConversationInputQueueControlAction::Pause
                && control.reason.as_deref() == Some("queued run failed")
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn send_queued_input_now_interrupts_active_run_and_dispatches_selected_item() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-queue-send-now.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![
        StreamPlan::Pending,
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("urgent done".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "current run".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::QueueConversationInput {
        prompt: "urgent queued prompt".to_owned(),
        kind: ConversationInputKind::Chat,
        target: ConversationInputTarget::MainThread,
        reasoning_effort: ReasoningEffort::High,
    })?;
    let _ = worker.recv_until(|message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { items, .. } if items.len() == 1)
    })?;

    worker.send(WorkerCommand::SendQueuedConversationInputNow {
        queue_id: ConversationInputQueueId::new("queue_1")?,
    })?;

    let first = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(
            message,
            WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. }
        ) || matches!(
            message,
            WorkerMessage::ConversationQueueDispatchStarted { prompt, .. }
                if prompt == "urgent queued prompt"
        )
    })?;
    let saw_cancel_first = matches!(
        first,
        WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. }
    );
    let second = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        if saw_cancel_first {
            matches!(
                message,
                WorkerMessage::ConversationQueueDispatchStarted { prompt, .. }
                    if prompt == "urgent queued prompt"
            )
        } else {
            matches!(
                message,
                WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. }
            )
        }
    })?;
    assert!(
        (saw_cancel_first
            && matches!(
                second,
                WorkerMessage::ConversationQueueDispatchStarted { .. }
            ))
            || (!saw_cancel_first
                && matches!(
                    second,
                    WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. }
                ))
    );

    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.queue_id.as_str() == "queue_1"
                && status.status == ConversationInputStatus::Dispatching
    )));

    let _ = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::RunFinished { .. })
    })?;

    worker.shutdown()?;
    Ok(())
}

#[test]
fn restored_dispatching_queue_item_is_marked_stale() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-queue-restore.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let store = sigil_kernel::JsonlSessionStore::new(&session_log_path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "planned".to_owned(),
        model_name: "planned-model".to_owned(),
    }))?;
    let queue_id = ConversationInputQueueId::new("queue_1")?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: "hash".to_owned(),
            prompt: "stale after restore".to_owned(),
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(1),
        }),
    ))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id: queue_id.clone(),
            status: ConversationInputStatus::Dispatching,
            reason: Some("dispatching".to_owned()),
            updated_at_ms: Some(2),
        }),
    ))?;

    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    let update = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ConversationQueueUpdated { items, .. } if items.is_empty()
        )
    })?;
    assert!(matches!(
        update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. } if items.is_empty()
    ));

    let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.queue_id == queue_id
                && status.status == ConversationInputStatus::Stale
                && status
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("session restore"))
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn submit_prompt_surfaces_provider_startup_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-error.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Fail("provider startup failed")]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error.contains("provider startup failed")
    ));

    worker.shutdown()?;
    Ok(())
}
