use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::json;

use super::{
    ExecutionContainmentRequest, ToolAnalysisStatus, ToolPermissionEffect, ToolPermissionPlanDraft,
    ToolPermissionPlanV2, ToolPermissionSummary, ToolSemanticScope,
};
use crate::{ToolAccess, ToolOperation, ToolSubject};

fn draft() -> ToolPermissionPlanDraft {
    ToolPermissionPlanDraft {
        access: ToolAccess::Execute,
        operation: ToolOperation::ExecuteWorkspaceCheckCommand,
        effects: BTreeSet::from([
            ToolPermissionEffect::ExecuteTrustedBinary,
            ToolPermissionEffect::ExecuteWorkspaceCode,
        ]),
        subjects: vec![ToolSubject::command("cargo check", "cargo check")],
        analysis: ToolAnalysisStatus::Complete,
        containment: ExecutionContainmentRequest::default(),
        semantic_scope: Some(ToolSemanticScope::new(
            "workspace_validation:cargo_check",
            1,
        )),
        tool_default_mode: None,
        analysis_bindings: Default::default(),
        safe_summary: ToolPermissionSummary {
            title: "Workspace validation".to_owned(),
            detail: "Runs one validation step".to_owned(),
            step_count: 1,
            workspace_code_steps: 1,
        },
        managed_file_access: None,
    }
}

fn mcp_spec(access: ToolAccess) -> crate::ToolSpec {
    crate::ToolSpec {
        name: "mcp__records__delete".to_owned(),
        description: "delete a remote record".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Mcp,
        access,
        network_effect: Some(crate::NetworkEffect::Mutate),
        preview: crate::ToolPreviewCapability::None,
    }
}

fn destructive_mcp_draft() -> ToolPermissionPlanDraft {
    ToolPermissionPlanDraft {
        access: ToolAccess::Write,
        operation: ToolOperation::NetworkRequest,
        effects: BTreeSet::from([
            ToolPermissionEffect::FileWrite,
            ToolPermissionEffect::FileDelete,
            ToolPermissionEffect::NetworkMutate,
            ToolPermissionEffect::RemoteMutation,
        ]),
        subjects: vec![
            ToolSubject::mcp_tool("mcp__records__delete"),
            ToolSubject::mcp_trust_class("records", "self_hosted"),
        ],
        analysis: ToolAnalysisStatus::Complete,
        containment: ExecutionContainmentRequest::default(),
        semantic_scope: Some(ToolSemanticScope::new("mcp_tool", 2)),
        tool_default_mode: Some(crate::ApprovalMode::Allow),
        analysis_bindings: BTreeMap::from([
            ("execution_backend".to_owned(), "mcp_stdio_v2".to_owned()),
            ("execution_profile".to_owned(), "stdio".to_owned()),
            (
                "environment_binding".to_owned(),
                "restricted:mcp-v2".to_owned(),
            ),
        ]),
        safe_summary: ToolPermissionSummary {
            title: "Delete remote record".to_owned(),
            detail: "MCP server declares a destructive mutation".to_owned(),
            step_count: 1,
            workspace_code_steps: 0,
        },
        managed_file_access: None,
    }
}

#[test]
fn plan_hash_is_stable_for_object_key_order_and_changes_with_arguments() -> Result<()> {
    let root = tempfile::tempdir()?;
    let first = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"cargo check","timeout":30}),
        root.path(),
        draft(),
    )?;
    let reordered = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"timeout":30,"command":"cargo check"}),
        root.path(),
        draft(),
    )?;
    let changed = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"cargo test","timeout":30}),
        root.path(),
        draft(),
    )?;

    assert_eq!(first.plan_hash, reordered.plan_hash);
    assert_ne!(first.plan_hash, changed.plan_hash);
    assert!(first.plan_hash.starts_with("sha256:"));
    Ok(())
}

#[test]
fn network_effect_uses_the_strictest_declared_network_fact() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut draft = draft();
    draft.effects.extend([
        ToolPermissionEffect::NetworkRead,
        ToolPermissionEffect::NetworkMutate,
    ]);
    let plan = ToolPermissionPlanV2::bind("tool", &json!({}), root.path(), draft)?;
    assert_eq!(plan.network_effect(), Some(crate::NetworkEffect::Mutate));
    Ok(())
}

#[test]
fn destructive_mcp_effects_remain_ask_except_in_danger_full_access() -> Result<()> {
    let root = tempfile::tempdir()?;
    let plan = ToolPermissionPlanV2::bind(
        "mcp__records__delete",
        &json!({"record_id":"record-1"}),
        root.path(),
        destructive_mcp_draft(),
    )?;
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        network_policy: crate::NetworkPolicy::Allow,
        ..Default::default()
    };

    for mode in [
        crate::PermissionMode::Manual,
        crate::PermissionMode::AutoEdit,
        crate::PermissionMode::DangerFullAccess,
    ] {
        let config = crate::PermissionConfig {
            mode,
            ..Default::default()
        };
        let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
        let decision = policy.decide_plan(&mcp_spec(ToolAccess::Write), &plan)?;

        let expected_mode = if mode == crate::PermissionMode::DangerFullAccess {
            crate::ApprovalMode::Allow
        } else {
            crate::ApprovalMode::Ask
        };
        assert_eq!(decision.mode, expected_mode, "mode={mode:?}");
        assert_eq!(decision.risk, crate::PermissionRisk::Destructive);
        assert!(decision.snapshot_required);
        assert!(decision.reasons.iter().any(|reason| {
            reason.code == "permission_effect_floor"
                && reason.source == crate::PermissionDecisionSource::HardSafety
        }));
        assert!(!crate::tool_approval_session_grant_available_for_plan(
            &decision, &plan
        ));
    }
    Ok(())
}

#[test]
fn danger_full_access_cannot_use_weak_labels_to_bypass_effect_floor() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut weak = destructive_mcp_draft();
    weak.access = ToolAccess::Read;
    weak.operation = ToolOperation::Read;
    weak.tool_default_mode = Some(crate::ApprovalMode::Allow);
    let plan = ToolPermissionPlanV2::bind(
        "mcp__records__delete",
        &json!({"record_id":"record-1"}),
        root.path(),
        weak,
    )?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        network_policy: crate::NetworkPolicy::Allow,
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);

    let decision = policy.decide_plan(&mcp_spec(ToolAccess::Read), &plan)?;

    assert_eq!(decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(decision.risk, crate::PermissionRisk::Destructive);
    assert!(decision.snapshot_required);
    Ok(())
}

#[test]
fn credential_effect_is_protected_and_denied_even_with_allow_labels() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut weak = destructive_mcp_draft();
    weak.access = ToolAccess::Read;
    weak.operation = ToolOperation::Read;
    weak.effects = BTreeSet::from([ToolPermissionEffect::CredentialAccess]);
    let plan = ToolPermissionPlanV2::bind("mcp__records__delete", &json!({}), root.path(), weak)?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);

    let decision = policy.decide_plan(&mcp_spec(ToolAccess::Read), &plan)?;

    assert_eq!(decision.mode, crate::ApprovalMode::Deny);
    assert_eq!(decision.risk, crate::PermissionRisk::Protected);
    Ok(())
}

#[test]
fn danger_full_access_skips_the_ordinary_high_risk_approval_barrier() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut weak = destructive_mcp_draft();
    weak.access = ToolAccess::Read;
    weak.operation = ToolOperation::Read;
    weak.effects = BTreeSet::from([ToolPermissionEffect::ExecuteDynamicCode]);
    let plan = ToolPermissionPlanV2::bind("mcp__records__delete", &json!({}), root.path(), weak)?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);

    let decision = policy.decide_plan(&mcp_spec(ToolAccess::Read), &plan)?;

    assert_eq!(decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(decision.risk, crate::PermissionRisk::High);
    Ok(())
}

#[test]
fn agent_lifecycle_is_distinct_from_operating_system_process_control() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
    let spec = crate::ToolSpec {
        name: "list_agents".to_owned(),
        description: "list host-owned agent threads".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Agent,
        access: ToolAccess::Read,
        network_effect: None,
        preview: crate::ToolPreviewCapability::None,
    };
    let mut lifecycle = draft();
    lifecycle.access = ToolAccess::Read;
    lifecycle.operation = ToolOperation::Read;
    lifecycle.effects = BTreeSet::from([ToolPermissionEffect::AgentLifecycle]);
    lifecycle.subjects = vec![ToolSubject::agent("explore")];
    lifecycle.tool_default_mode = Some(crate::ApprovalMode::Allow);

    let lifecycle_plan =
        ToolPermissionPlanV2::bind("list_agents", &json!({}), root.path(), lifecycle.clone())?;
    let lifecycle_decision = policy.decide_plan(&spec, &lifecycle_plan)?;
    assert_eq!(lifecycle_decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(lifecycle_decision.risk, crate::PermissionRisk::Low);

    lifecycle.effects = BTreeSet::from([ToolPermissionEffect::ProcessControl]);
    let process_plan =
        ToolPermissionPlanV2::bind("list_agents", &json!({}), root.path(), lifecycle)?;
    let process_decision = policy.decide_plan(&spec, &process_plan)?;
    assert_eq!(process_decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(process_decision.risk, crate::PermissionRisk::High);
    Ok(())
}

#[test]
fn owned_terminal_cancel_and_resize_do_not_inherit_arbitrary_process_control_approval() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::Manual,
        tools: BTreeMap::from([
            ("terminal_cancel".to_owned(), crate::ApprovalMode::Allow),
            ("terminal_resize".to_owned(), crate::ApprovalMode::Allow),
        ]),
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);

    for (tool_name, operation) in [
        ("terminal_cancel", ToolOperation::CancelTerminalTask),
        ("terminal_resize", ToolOperation::ResizeTerminalTask),
    ] {
        let spec = crate::ToolSpec {
            name: tool_name.to_owned(),
            description: "control one owned terminal task".to_owned(),
            input_schema: json!({"type":"object"}),
            category: crate::ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: crate::ToolPreviewCapability::None,
        };
        let mut plan = draft();
        plan.access = ToolAccess::Execute;
        plan.operation = operation;
        plan.effects = BTreeSet::from([ToolPermissionEffect::ProcessControl]);
        plan.subjects = vec![ToolSubject::command(
            "terminal_task:task-owned",
            "terminal_task:task-owned",
        )];
        plan.tool_default_mode = None;
        let plan = ToolPermissionPlanV2::bind(
            tool_name,
            &json!({"task_id":"task-owned"}),
            root.path(),
            plan,
        )?;

        let decision = policy.decide_plan(&spec, &plan)?;
        assert_eq!(decision.mode, crate::ApprovalMode::Allow);
        assert_eq!(decision.risk, crate::PermissionRisk::Medium);
    }
    Ok(())
}

#[test]
fn trusted_binary_read_exception_requires_a_structured_shell_operation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
    let spec = crate::ToolSpec {
        name: "bash".to_owned(),
        description: "finite shell execution".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Shell,
        access: ToolAccess::Execute,
        network_effect: None,
        preview: crate::ToolPreviewCapability::Optional,
    };
    let mut candidate = draft();
    candidate.access = ToolAccess::Read;
    candidate.operation = ToolOperation::Read;
    candidate.effects = BTreeSet::from([ToolPermissionEffect::ExecuteTrustedBinary]);
    candidate.subjects = vec![ToolSubject::command("git status", "git status")];
    candidate.tool_default_mode = Some(crate::ApprovalMode::Allow);

    let weak = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"git status"}),
        root.path(),
        candidate.clone(),
    )?;
    let weak_decision = policy.decide_plan(&spec, &weak)?;
    assert_eq!(weak_decision.mode, crate::ApprovalMode::Allow);

    candidate.operation = ToolOperation::ExecuteReadOnlyCommand;
    let analyzed = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"git status"}),
        root.path(),
        candidate,
    )?;
    let analyzed_decision = policy.decide_plan(&spec, &analyzed)?;
    assert_eq!(analyzed_decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(analyzed_decision.risk, crate::PermissionRisk::Low);
    Ok(())
}

#[test]
fn incomplete_shell_analysis_cannot_be_widened_by_raw_allow_pattern() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut draft = draft();
    draft.access = ToolAccess::Execute;
    draft.operation = ToolOperation::ExecuteUnknownCommand;
    draft.effects = BTreeSet::from([ToolPermissionEffect::Unknown]);
    draft.analysis = super::ToolAnalysisStatus::Conservative {
        reasons: vec![super::ToolAnalysisReason::new(
            super::ToolAnalysisReasonCode::UnknownProgram,
            Some("unknown command".to_owned()),
        )],
    };
    draft.semantic_scope = None;
    let plan = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"unknown-command"}),
        root.path(),
        draft,
    )?;
    let config = crate::PermissionConfig {
        commands: crate::CommandPermissionConfig {
            allow: vec!["*".to_owned()],
            ..Default::default()
        },
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
    let spec = crate::ToolSpec {
        name: "bash".to_owned(),
        description: "shell".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Shell,
        access: ToolAccess::Execute,
        network_effect: None,
        preview: crate::ToolPreviewCapability::Optional,
    };

    let decision = policy.decide_plan(&spec, &plan)?;
    assert_eq!(decision.mode, crate::ApprovalMode::Ask);
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason.code == "deterministic_analysis_incomplete")
    );
    Ok(())
}

#[test]
fn danger_full_access_keeps_incomplete_local_shell_non_interactive_and_suppresses_explicit_ask()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let mut draft = draft();
    draft.access = ToolAccess::Execute;
    draft.operation = ToolOperation::ExecuteUnknownCommand;
    draft.effects = BTreeSet::from([ToolPermissionEffect::ExecuteDynamicCode]);
    draft.analysis = super::ToolAnalysisStatus::Conservative {
        reasons: vec![super::ToolAnalysisReason::new(
            super::ToolAnalysisReasonCode::DynamicCommand,
            Some("command substitution".to_owned()),
        )],
    };
    draft.semantic_scope = None;
    let plan = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"echo $(git status --short)"}),
        root.path(),
        draft,
    )?;
    let spec = crate::ToolSpec {
        name: "bash".to_owned(),
        description: "shell".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Shell,
        access: ToolAccess::Execute,
        network_effect: None,
        preview: crate::ToolPreviewCapability::Optional,
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };

    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let decision = crate::PermissionPolicyChain::new_with_context(&config, &context)
        .decide_plan(&spec, &plan)?;
    assert_eq!(decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(decision.risk, crate::PermissionRisk::High);
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| { reason.code == "danger_full_access_incomplete_analysis_allowed" })
    );

    let explicit_ask = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        commands: crate::CommandPermissionConfig {
            ask: vec!["*".to_owned()],
            ..Default::default()
        },
        ..Default::default()
    };
    let decision = crate::PermissionPolicyChain::new_with_context(&explicit_ask, &context)
        .decide_plan(&spec, &plan)?;
    assert_eq!(decision.mode, crate::ApprovalMode::Allow);
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason.code == "danger_full_access_ask_suppressed")
    );
    Ok(())
}

#[test]
fn danger_full_access_suppresses_network_ask_for_incomplete_non_shell_analysis() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut draft = draft();
    draft.access = ToolAccess::Read;
    draft.operation = ToolOperation::NetworkRequest;
    draft.effects = BTreeSet::from([ToolPermissionEffect::NetworkUnknown]);
    draft.analysis = ToolAnalysisStatus::Unsupported {
        reason: super::ToolAnalysisReason::new(
            super::ToolAnalysisReasonCode::UnsupportedSyntax,
            Some("remote tool effect is not analyzable".to_owned()),
        ),
    };
    draft.semantic_scope = None;
    let plan = ToolPermissionPlanV2::bind("remote_tool", &json!({}), root.path(), draft)?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::DangerFullAccess,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        network_policy: crate::NetworkPolicy::Ask,
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
    let spec = crate::ToolSpec {
        name: "remote_tool".to_owned(),
        description: "remote tool".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Mcp,
        access: ToolAccess::Read,
        network_effect: Some(crate::NetworkEffect::Unknown),
        preview: crate::ToolPreviewCapability::None,
    };

    let decision = policy.decide_plan(&spec, &plan)?;
    assert_eq!(decision.mode, crate::ApprovalMode::Allow);
    assert_eq!(decision.network_policy_decision, crate::ApprovalMode::Allow);
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| { reason.code == "danger_full_access_incomplete_analysis_allowed" })
    );
    Ok(())
}

#[test]
fn unproven_macos_binding_remains_ask_but_can_offer_exact_session_authority() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut draft = draft();
    draft.analysis_bindings = BTreeMap::from([
        ("containment_proven".to_owned(), "false".to_owned()),
        (
            "execution_backend".to_owned(),
            "macos_seatbelt:network_unsupported".to_owned(),
        ),
        (
            "execution_profile".to_owned(),
            "workspace_validation".to_owned(),
        ),
        (
            "environment_binding".to_owned(),
            "restricted:shell-v1".to_owned(),
        ),
    ]);
    let plan = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"cargo check"}),
        root.path(),
        draft,
    )?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::AutoEdit,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
    let spec = crate::ToolSpec {
        name: "bash".to_owned(),
        description: "shell".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Shell,
        access: ToolAccess::Execute,
        network_effect: None,
        preview: crate::ToolPreviewCapability::Optional,
    };

    let decision = policy.decide_plan(&spec, &plan)?;
    assert_eq!(decision.mode, crate::ApprovalMode::Ask);
    assert!(plan.session_grant_containment_binding().is_some());
    assert!(crate::tool_approval_session_grant_available_for_plan(
        &decision, &plan
    ));
    Ok(())
}

#[test]
fn auto_edit_allows_workspace_code_only_when_containment_is_proven() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut draft = draft();
    draft.containment.network = crate::NetworkContainment::Deny;
    draft.analysis_bindings = BTreeMap::from([
        ("containment_proven".to_owned(), "true".to_owned()),
        (
            "execution_backend".to_owned(),
            "linux_namespace:v2".to_owned(),
        ),
        (
            "execution_profile".to_owned(),
            "workspace_validation".to_owned(),
        ),
        (
            "environment_binding".to_owned(),
            "restricted:shell-v1".to_owned(),
        ),
    ]);
    let plan = ToolPermissionPlanV2::bind(
        "bash",
        &json!({"command":"cargo check"}),
        root.path(),
        draft,
    )?;
    let config = crate::PermissionConfig {
        mode: crate::PermissionMode::AutoEdit,
        ..Default::default()
    };
    let context = crate::PermissionEvaluationContext {
        workspace_root: root.path().to_path_buf(),
        ..Default::default()
    };
    let policy = crate::PermissionPolicyChain::new_with_context(&config, &context);
    let spec = crate::ToolSpec {
        name: "bash".to_owned(),
        description: "shell".to_owned(),
        input_schema: json!({"type":"object"}),
        category: crate::ToolCategory::Shell,
        access: ToolAccess::Execute,
        network_effect: None,
        preview: crate::ToolPreviewCapability::Optional,
    };

    let decision = policy.decide_plan(&spec, &plan)?;
    assert_eq!(decision.mode, crate::ApprovalMode::Allow);
    assert!(decision.reasons.iter().any(|reason| {
        reason.code == "contained_workspace_validation_default"
            && reason.source == crate::PermissionDecisionSource::SandboxSubstitution
    }));
    Ok(())
}

#[test]
fn permission_plan_rejects_unbounded_subject_count_and_bytes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut too_many = draft();
    too_many.subjects = (0..=crate::MAX_TOOL_PERMISSION_SUBJECTS)
        .map(|index| ToolSubject::command(format!("command-{index}"), format!("command-{index}")))
        .collect();
    let error = ToolPermissionPlanV2::bind("bash", &json!({}), root.path(), too_many)
        .expect_err("subject count must be bounded");
    assert!(error.to_string().contains("subjects exceed maximum count"));

    let mut too_large = draft();
    let command = "x".repeat(crate::MAX_TOOL_PERMISSION_SUBJECT_BYTES + 1);
    too_large.subjects = vec![ToolSubject::command(command.clone(), command)];
    let error = ToolPermissionPlanV2::bind("bash", &json!({}), root.path(), too_large)
        .expect_err("subject bytes must be bounded");
    assert!(error.to_string().contains("subject exceeds maximum"));
    Ok(())
}

#[test]
fn durable_permission_plan_contains_only_bounded_safe_projections() -> Result<()> {
    let root = tempfile::tempdir()?;
    let secret = "sigil-secret-value-123";
    let absolute = root.path().join("outside-sensitive-name.txt");
    let raw_command = format!("curl 'https://example.test/run?token={secret}'");
    let mut draft = draft();
    draft.subjects = vec![
        ToolSubject::command(&raw_command, &raw_command),
        ToolSubject::path_with_scope(
            absolute.display().to_string(),
            absolute.display().to_string(),
            Some(absolute.clone()),
            crate::ToolSubjectScope::External,
        ),
    ];
    draft.analysis_bindings.insert(
        "shell_program_sha256".to_owned(),
        crate::stable_event_hash(raw_command.as_bytes()),
    );
    draft.safe_summary = ToolPermissionSummary {
        title: "Run remote check".to_owned(),
        detail: format!("Use https://example.test/run?token={secret}"),
        step_count: 1,
        workspace_code_steps: 0,
    };

    let plan =
        ToolPermissionPlanV2::bind("bash", &json!({"command": raw_command}), root.path(), draft)?;
    let entry = crate::ToolPermissionPlannedV2Entry::from_plan("call-private", &plan)?;
    let json = serde_json::to_string(&entry)?;

    assert!(json.len() <= crate::MAX_DURABLE_TOOL_CONTROL_BYTES);
    assert!(!json.contains(secret));
    assert!(!json.contains("curl"));
    assert!(!json.contains(&absolute.display().to_string()));
    assert!(!json.contains(root.path().to_string_lossy().as_ref()));
    assert!(entry.subjects.iter().all(|subject| {
        subject
            .identity_sha256
            .strip_prefix("sha256:")
            .is_some_and(|hash| hash.len() == 64)
            && subject.relative_label.is_none()
    }));
    assert_eq!(entry.analysis_binding_names, vec!["shell_program_sha256"]);
    Ok(())
}
