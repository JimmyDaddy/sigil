use std::{
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;

use super::*;
use crate::{
    CompactionConfig, CompletionRequest, MemoryConfig, MessageRole, NetworkEffect, NetworkPolicy,
    PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalScope, PlanApprovedEntry, Provider,
    ProviderCapabilities, ProviderChunk, ReasoningStreamSupport, Tool, ToolAccess, ToolApproval,
    ToolApprovalSessionGrantEntry, ToolApprovalSessionGrantExpiry, ToolCategory, ToolContext,
    ToolOperation, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, ToolSubjectAudit, ToolSubjectKind, ToolSubjectScope,
};

fn spec(
    name: &str,
    category: ToolCategory,
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    preview: ToolPreviewCapability,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: name.to_owned(),
        input_schema: json!({"type": "object"}),
        category,
        access,
        network_effect,
        preview,
    }
}

fn policy_decision(
    permission_mode: crate::PermissionMode,
    network_policy: NetworkPolicy,
    spec: &ToolSpec,
    operation: ToolOperation,
    subjects: Vec<ToolSubject>,
) -> Result<crate::PermissionDecision> {
    let config = PermissionConfig {
        mode: permission_mode,
        ..PermissionConfig::default()
    };
    let context = PermissionEvaluationContext {
        network_policy,
        ..PermissionEvaluationContext::default()
    };
    PermissionPolicy::new_with_context(&config, &context)
        .decide_with_operation_network_effect_and_default(
            spec,
            &spec.name,
            spec.access,
            operation,
            spec.network_effect,
            subjects,
            None,
        )
}

fn approved_plan() -> PlanApprovedEntry {
    PlanApprovedEntry {
        plan_version: 1,
        plan_hash: "sha256:approved-plan".to_owned(),
        approved_at_ms: 42,
        permission: PlanApprovalPermission::WorkspaceEdits,
        scope: PlanApprovalScope {
            summary: "approved source edit".to_owned(),
            workspace_paths: vec!["src/lib.rs".to_owned()],
        },
        expires: PlanApprovalExpiry::Session,
        clear_planning_context: true,
    }
}

#[test]
fn plan_approval_does_not_override_network_ask_or_deny() -> Result<()> {
    let mut session = Session::new("test", "test");
    session.append_control(ControlEntry::PlanApproved(approved_plan()))?;
    let tool_spec = spec(
        "write_file",
        ToolCategory::File,
        ToolAccess::Write,
        Some(NetworkEffect::Read),
        ToolPreviewCapability::Required,
    );
    for network_policy in [NetworkPolicy::Ask, NetworkPolicy::Deny] {
        let decision = policy_decision(
            crate::PermissionMode::Manual,
            network_policy,
            &tool_spec,
            ToolOperation::EditFile,
            vec![ToolSubject::path("src/lib.rs", "src/lib.rs")],
        )?;
        let expected = decision.mode;
        let decision = plan_approval_decision_override(&session, &tool_spec, decision);
        assert_eq!(decision.mode, expected);
        assert_eq!(
            decision.network_policy_decision,
            match network_policy {
                NetworkPolicy::Allow => ApprovalMode::Allow,
                NetworkPolicy::Ask => ApprovalMode::Ask,
                NetworkPolicy::Deny => ApprovalMode::Deny,
            }
        );
    }
    Ok(())
}

fn command_subject_audit(command: &str) -> ToolSubjectAudit {
    ToolSubjectAudit {
        kind: ToolSubjectKind::Command,
        original: command.to_owned(),
        normalized: command.to_owned(),
        canonical_path: None,
        scope: ToolSubjectScope::Unknown,
    }
}

#[test]
fn session_grant_requires_exact_network_effect_match() -> Result<()> {
    let command = "cargo check";
    let tool_spec = spec(
        "bash",
        ToolCategory::Shell,
        ToolAccess::Execute,
        Some(NetworkEffect::Unknown),
        ToolPreviewCapability::None,
    );
    let decision = policy_decision(
        crate::PermissionMode::Manual,
        NetworkPolicy::Allow,
        &tool_spec,
        ToolOperation::ExecuteUnknownCommand,
        vec![ToolSubject::command(command, command)],
    )?;
    assert_eq!(decision.mode, ApprovalMode::Ask);
    assert!(crate::tool_approval_session_grant_available(&decision));

    let mut session = Session::new("test", "test");
    session.append_control(ControlEntry::ToolApprovalSessionGrant(
        ToolApprovalSessionGrantEntry {
            call_id: "older-call".to_owned(),
            tool_name: "bash".to_owned(),
            access: ToolAccess::Execute,
            network_effect: Some(NetworkEffect::Read),
            operation: ToolOperation::ExecuteUnknownCommand,
            risk: crate::PermissionRisk::High,
            subjects: vec![command_subject_audit(command)],
            subject_zones: vec![crate::PathTrustZone::Unknown],
            expires: ToolApprovalSessionGrantExpiry::Session,
            granted_at_ms: 42,
        },
    ))?;

    let (decision, grant) = tool_session_grant_decision_override(&session, "bash", decision);
    assert_eq!(decision.mode, ApprovalMode::Ask);
    assert!(grant.is_none());
    Ok(())
}

fn interactive_options(network_policy: NetworkPolicy) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: PathBuf::from("."),
        max_turns: None,
        tool_timeout_secs: 30,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: PermissionEvaluationContext {
            network_policy,
            ..PermissionEvaluationContext::default()
        },
        memory_config: MemoryConfig::default(),
        compaction_config: CompactionConfig::default(),
    }
}

#[test]
fn external_directory_interactive_override_preserves_network_deny() -> Result<()> {
    let tool_spec = spec(
        "read_file",
        ToolCategory::File,
        ToolAccess::Read,
        Some(NetworkEffect::Read),
        ToolPreviewCapability::None,
    );
    let external = ToolSubject::path_with_scope(
        "/tmp/input.txt",
        "/tmp/input.txt",
        Some(PathBuf::from("/tmp/input.txt")),
        ToolSubjectScope::External,
    );
    let decision = policy_decision(
        crate::PermissionMode::Manual,
        NetworkPolicy::Deny,
        &tool_spec,
        ToolOperation::Read,
        vec![external],
    )?;
    assert_eq!(decision.mode, ApprovalMode::Deny);
    let decision = interactive_external_directory_approval_override(
        &interactive_options(NetworkPolicy::Deny),
        decision,
    );
    assert_eq!(decision.local_policy_decision, ApprovalMode::Ask);
    assert_eq!(decision.network_policy_decision, ApprovalMode::Deny);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn prepared_policy_fingerprint_binds_network_facets() -> Result<()> {
    let tool_spec = spec(
        "bash",
        ToolCategory::Shell,
        ToolAccess::Execute,
        Some(NetworkEffect::Read),
        ToolPreviewCapability::None,
    );
    let subjects = vec![ToolSubject::command("cargo check", "cargo check")];
    let local_ask = policy_decision(
        crate::PermissionMode::Manual,
        NetworkPolicy::Allow,
        &tool_spec,
        ToolOperation::ExecuteUnknownCommand,
        subjects.clone(),
    )?;
    let network_ask = policy_decision(
        crate::PermissionMode::DangerFullAccess,
        NetworkPolicy::Ask,
        &tool_spec,
        ToolOperation::ExecuteUnknownCommand,
        subjects,
    )?;
    assert_eq!(local_ask.mode, ApprovalMode::Ask);
    assert_eq!(network_ask.mode, ApprovalMode::Ask);
    assert_eq!(local_ask.risk, network_ask.risk);
    assert_ne!(
        preparation_policy_fingerprint(&local_ask)?,
        preparation_policy_fingerprint(&network_ask)?
    );
    Ok(())
}

struct NetworkApprovalProvider {
    args_json: &'static str,
}

#[async_trait]
impl Provider for NetworkApprovalProvider {
    fn name(&self) -> &str {
        "network-approval-test"
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
        let tool_used = request
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool);
        let chunks = if tool_used {
            vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ]
        } else {
            vec![
                Ok(ProviderChunk::ToolCallComplete(crate::ToolCall {
                    id: "network-call".to_owned(),
                    name: "network_probe".to_owned(),
                    args_json: self.args_json.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

struct NetworkContextProbe {
    observed: Arc<Mutex<Option<(NetworkPolicy, bool)>>>,
}

#[async_trait]
impl Tool for NetworkContextProbe {
    fn spec(&self) -> ToolSpec {
        spec(
            "network_probe",
            ToolCategory::Custom,
            ToolAccess::Read,
            Some(NetworkEffect::Read),
            ToolPreviewCapability::None,
        )
    }

    fn permission_network_effect(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<Option<NetworkEffect>> {
        match args.get("effect").and_then(serde_json::Value::as_str) {
            None | Some("read") => Ok(Some(NetworkEffect::Read)),
            Some("mutate") => Ok(Some(NetworkEffect::Mutate)),
            Some("unknown") => Ok(Some(NetworkEffect::Unknown)),
            Some(effect) => Err(anyhow!("unsupported network effect {effect}")),
        }
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        *self
            .observed
            .lock()
            .map_err(|_| anyhow!("network context observation lock poisoned"))? =
            Some((ctx.network_policy(), ctx.explicit_network_approval()));
        Ok(ToolResult::ok(
            call_id,
            "network_probe",
            "ok",
            ToolResultMeta::default(),
        ))
    }
}

struct ExplicitNetworkApproveHandler;

impl crate::ApprovalHandler for ExplicitNetworkApproveHandler {
    fn approve_tool_call(
        &mut self,
        _call: &crate::ToolCall,
        _spec: &ToolSpec,
    ) -> Result<ToolApproval> {
        Ok(ToolApproval::Approve)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

struct ExplicitNetworkApproveWithArgsHandler {
    args_json: String,
}

impl crate::ApprovalHandler for ExplicitNetworkApproveWithArgsHandler {
    fn approve_tool_call(
        &mut self,
        _call: &crate::ToolCall,
        _spec: &ToolSpec,
    ) -> Result<ToolApproval> {
        Ok(ToolApproval::ApproveWithArgs {
            args_json: self.args_json.clone(),
        })
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

struct NonExplicitNetworkApprovalHandler {
    approval: ToolApproval,
}

impl crate::ApprovalHandler for NonExplicitNetworkApprovalHandler {
    fn approve_tool_call(
        &mut self,
        _call: &crate::ToolCall,
        _spec: &ToolSpec,
    ) -> Result<ToolApproval> {
        Ok(self.approval.clone())
    }
}

async fn run_network_probe_with_handler<A>(mut approval_handler: A) -> Result<(Session, bool)>
where
    A: crate::ApprovalHandler + Send,
{
    let observed = Arc::new(Mutex::new(None));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(NetworkContextProbe {
        observed: observed.clone(),
    }));
    let agent = Agent::new(NetworkApprovalProvider { args_json: "{}" }, tools);
    let mut session = Session::new("network-non-explicit-test", "test-model");
    let mut event_handler = crate::NoopEventHandler;
    let mut options = interactive_options(NetworkPolicy::Ask);
    options.permission_config.mode = crate::PermissionMode::DangerFullAccess;
    agent
        .run_with_approval(
            &mut session,
            "use the network probe",
            options,
            &mut event_handler,
            &mut approval_handler,
        )
        .await?;
    let executed = observed
        .lock()
        .map_err(|_| anyhow!("network non-explicit assertion lock poisoned"))?
        .is_some();
    Ok((session, executed))
}

fn session_has_non_explicit_network_denial(session: &Session) -> bool {
    session.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::SessionLogEntry::Control(crate::ControlEntry::ToolApproval(approval))
                if approval.action == crate::ToolApprovalAuditAction::Resolved
                    && approval.user_decision.is_none()
                    && approval.reason.as_deref()
                        == Some("network approval requires an explicit user action")
        )
    })
}

#[tokio::test]
async fn agent_marks_network_ask_context_only_for_explicit_user_approval() -> Result<()> {
    assert!(!crate::AutoApproveHandler.approval_is_explicit_user_action());

    let observed = Arc::new(Mutex::new(None));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(NetworkContextProbe {
        observed: observed.clone(),
    }));
    let agent = Agent::new(NetworkApprovalProvider { args_json: "{}" }, tools);
    let mut session = Session::new("network-approval-test", "test-model");
    let mut event_handler = crate::NoopEventHandler;
    let mut approval_handler = ExplicitNetworkApproveHandler;
    let mut options = interactive_options(NetworkPolicy::Ask);
    options.permission_config.mode = crate::PermissionMode::DangerFullAccess;

    agent
        .run_with_approval(
            &mut session,
            "use the network probe",
            options,
            &mut event_handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(
        *observed
            .lock()
            .map_err(|_| anyhow!("network context assertion lock poisoned"))?,
        Some((NetworkPolicy::Ask, true))
    );
    Ok(())
}

#[tokio::test]
async fn network_ask_rejects_auto_and_all_non_explicit_approving_variants() -> Result<()> {
    let (auto_session, auto_executed) =
        run_network_probe_with_handler(crate::AutoApproveHandler).await?;
    assert!(!auto_executed);
    assert!(session_has_non_explicit_network_denial(&auto_session));

    for approval in [
        ToolApproval::Approve,
        ToolApproval::ApproveForSession,
        ToolApproval::ApproveWithArgs {
            args_json: "{}".to_owned(),
        },
    ] {
        let (session, executed) =
            run_network_probe_with_handler(NonExplicitNetworkApprovalHandler { approval }).await?;
        assert!(!executed);
        assert!(session_has_non_explicit_network_denial(&session));
    }
    Ok(())
}

#[tokio::test]
async fn approval_time_args_cannot_change_read_network_effect_to_mutate_or_unknown() -> Result<()> {
    for effect in ["mutate", "unknown"] {
        let observed = Arc::new(Mutex::new(None));
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(NetworkContextProbe {
            observed: observed.clone(),
        }));
        let agent = Agent::new(
            NetworkApprovalProvider {
                args_json: r#"{"effect":"read"}"#,
            },
            tools,
        );
        let mut session = Session::new("network-args-override-test", "test-model");
        let mut event_handler = crate::NoopEventHandler;
        let mut approval_handler = ExplicitNetworkApproveWithArgsHandler {
            args_json: json!({"effect": effect}).to_string(),
        };
        let mut options = interactive_options(NetworkPolicy::Ask);
        options.permission_config.mode = crate::PermissionMode::DangerFullAccess;

        agent
            .run_with_approval(
                &mut session,
                "use the network probe",
                options,
                &mut event_handler,
                &mut approval_handler,
            )
            .await?;

        assert_eq!(
            *observed
                .lock()
                .map_err(|_| anyhow!("network override assertion lock poisoned"))?,
            None,
            "{effect} override must not execute"
        );
        assert!(session.entries().iter().any(|entry| {
            matches!(
                entry,
                crate::SessionLogEntry::Control(crate::ControlEntry::ToolApproval(approval))
                    if approval.action == crate::ToolApprovalAuditAction::Resolved
                        && approval.user_decision
                            == Some(crate::ToolApprovalUserDecision::Denied)
                        && approval.reason.as_deref().is_some_and(|reason| {
                            reason.contains("altered the permission scope")
                        })
            )
        }));
    }
    Ok(())
}
