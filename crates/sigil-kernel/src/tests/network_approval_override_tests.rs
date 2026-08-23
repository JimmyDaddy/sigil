use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;

use super::*;
use crate::{
    CompactionConfig, CompletionRequest, MemoryConfig, MessageRole, NetworkEffect, NetworkPolicy,
    PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalScope, PlanId,
    PlanPermissionGrantedEntry, Provider, ProviderCapabilities, ProviderChunk,
    ReasoningStreamSupport, TaskId, Tool, ToolAccess, ToolApproval, ToolCategory, ToolContext,
    ToolOperation, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, ToolSubjectScope,
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

fn plan_permission_grant() -> PlanPermissionGrantedEntry {
    PlanPermissionGrantedEntry {
        plan_id: PlanId::new("network-test-plan").expect("plan id"),
        plan_hash: "sha256:approved-plan".to_owned(),
        task_id: TaskId::new("network-test-task").expect("task id"),
        workspace_snapshot_id: None,
        permission: PlanApprovalPermission::WorkspaceEdits,
        scope: PlanApprovalScope {
            summary: "approved source edit".to_owned(),
            workspace_paths: vec!["src/lib.rs".to_owned()],
        },
        expires: PlanApprovalExpiry::Session,
        granted_at_ms: 42,
    }
}

#[test]
fn plan_approval_does_not_override_network_ask_or_deny() -> Result<()> {
    let mut session = Session::new("test", "test");
    session.append_control(ControlEntry::PlanPermissionGranted(plan_permission_grant()))?;
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
        permission_mode_override: None,
        tool_authority: None,
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
    assert_eq!(network_ask.mode, ApprovalMode::Allow);
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

struct NetworkSessionGrantProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for NetworkSessionGrantProvider {
    fn name(&self) -> &str {
        "network-session-grant-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        NetworkApprovalProvider { args_json: "{}" }.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(call_index, 0 | 2) {
            let call_id = format!("network-session-call-{}", (call_index / 2) + 1);
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(crate::ToolCall {
                    id: call_id,
                    name: "network_session_probe".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct NetworkSessionGrantProbe {
    observed: Arc<Mutex<Vec<(NetworkPolicy, bool)>>>,
}

#[async_trait]
impl Tool for NetworkSessionGrantProbe {
    fn spec(&self) -> ToolSpec {
        spec(
            "network_session_probe",
            ToolCategory::Custom,
            ToolAccess::Read,
            Some(NetworkEffect::Read),
            ToolPreviewCapability::None,
        )
    }

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        Ok(crate::ToolPermissionPlanDraft {
            access: ToolAccess::Read,
            operation: ToolOperation::NetworkRequest,
            effects: BTreeSet::from([crate::ToolPermissionEffect::NetworkRead]),
            subjects: vec![ToolSubject::mcp_tool("network-session-probe")],
            analysis: crate::ToolAnalysisStatus::Complete,
            containment: Default::default(),
            semantic_scope: Some(crate::ToolSemanticScope::new(
                "network_read:session_probe",
                1,
            )),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([
                ("execution_backend".to_owned(), "test-network-v1".to_owned()),
                ("execution_profile".to_owned(), "network-read".to_owned()),
                (
                    "environment_binding".to_owned(),
                    "test-restricted-v1".to_owned(),
                ),
            ]),
            safe_summary: crate::ToolPermissionSummary {
                title: "Read test network source".to_owned(),
                detail: "Exercise reusable network approval authority".to_owned(),
                step_count: 1,
                workspace_code_steps: 0,
            },
        })
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        let authorization = (ctx.network_policy(), ctx.explicit_network_approval());
        if authorization == (NetworkPolicy::Ask, false) {
            return Ok(ToolResult::error(
                call_id,
                "network_session_probe",
                crate::ToolErrorKind::PermissionDenied,
                "network session probe requires current network authorization",
            ));
        }
        self.observed
            .lock()
            .map_err(|_| anyhow!("network session observation lock poisoned"))?
            .push(authorization);
        Ok(ToolResult::ok(
            call_id,
            "network_session_probe",
            "ok",
            ToolResultMeta::default(),
        ))
    }
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let network_effect = match args.get("effect").and_then(serde_json::Value::as_str) {
            None | Some("read") => Some(NetworkEffect::Read),
            Some("mutate") => Some(NetworkEffect::Mutate),
            Some("unknown") => Some(NetworkEffect::Unknown),
            Some(effect) => return Err(anyhow!("unsupported network effect {effect}")),
        };
        crate::declared_tool_permission_plan(
            &self.spec(),
            args,
            crate::DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Read,
                network_effect,
                subjects: Vec::new(),
                tool_default_mode: None,
            },
        )
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

struct ExplicitNetworkApproveForSessionHandler {
    approvals: Arc<AtomicUsize>,
}

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

impl crate::ApprovalHandler for ExplicitNetworkApproveForSessionHandler {
    fn approve_tool_call(
        &mut self,
        _call: &crate::ToolCall,
        _spec: &ToolSpec,
    ) -> Result<ToolApproval> {
        self.approvals.fetch_add(1, Ordering::SeqCst);
        Ok(ToolApproval::ApproveForSession)
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
    options.permission_config.mode = crate::PermissionMode::Manual;
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
async fn danger_full_access_authorizes_network_ask_without_approval() -> Result<()> {
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
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::SessionLogEntry::Control(crate::ControlEntry::ToolApproval(_))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_reuses_exact_network_session_grant_without_second_prompt() -> Result<()> {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(NetworkSessionGrantProbe {
        observed: Arc::clone(&observed),
    }));
    let agent = Agent::new(
        NetworkSessionGrantProvider {
            calls: Arc::clone(&provider_calls),
        },
        tools,
    );
    let mut session = Session::new("network-session-grant-test", "test-model");
    let mut event_handler = crate::NoopEventHandler;
    let mut approval_handler = ExplicitNetworkApproveForSessionHandler {
        approvals: Arc::clone(&approvals),
    };
    let run_options = || {
        let mut options = interactive_options(NetworkPolicy::Ask);
        options.permission_config.mode = crate::PermissionMode::Manual;
        options
    };

    let first = agent
        .run_with_approval(
            &mut session,
            "read the network source once",
            run_options(),
            &mut event_handler,
            &mut approval_handler,
        )
        .await?;
    let second = agent
        .run_with_approval(
            &mut session,
            "read the same network source again",
            run_options(),
            &mut event_handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(first.final_text, "done");
    assert_eq!(second.final_text, "done");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 4);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(
        *observed
            .lock()
            .map_err(|_| anyhow!("network session assertion lock poisoned"))?,
        vec![(NetworkPolicy::Ask, true), (NetworkPolicy::Ask, true)]
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::SessionLogEntry::Control(crate::ControlEntry::ToolApprovalSessionGrant(grant))
                if grant.source_call_id == "network-session-call-1"
                    && grant.facets
                        == vec![crate::ToolApprovalSessionGrantFacet::Network]
                    && grant.scope == crate::ToolApprovalSessionGrantScope::ExactSubjects
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::SessionLogEntry::Control(
                crate::ControlEntry::ToolPermissionDecisionV2(decision)
            ) if decision.call_id == "network-session-call-2"
                && decision.policy_decision == ApprovalMode::Allow
                && decision.allow_source
                    == Some(crate::ToolApprovalAllowSource::SessionGrant)
        )
    }));
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
        options.permission_config.mode = crate::PermissionMode::Manual;

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
                        && approval.user_decision.is_none()
                        && approval.terminal_status
                            == Some(crate::ToolApprovalTerminalStatusV2::Stale)
                        && approval.reason.as_deref().is_some_and(|reason| {
                            reason.contains("altered the permission plan")
                        })
            )
        }));
    }
    Ok(())
}
