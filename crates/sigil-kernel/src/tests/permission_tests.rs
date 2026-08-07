use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::Result;
use serde_json::json;

use crate::{
    ExecutionContainmentRequest, NetworkEffect, ToolAccess, ToolAnalysisReason,
    ToolAnalysisReasonCode, ToolAnalysisStatus, ToolApprovalSessionGrantUnavailableReasonCode,
    ToolCategory, ToolPermissionEffect, ToolPermissionPlanV2, ToolPermissionSummary,
    ToolPreviewCapability, ToolSemanticScope, ToolSpec, ToolSubject, ToolSubjectKind,
    ToolSubjectScope, infer_tool_operation,
};

use super::{
    ApprovalMode, CommandPermissionConfig, EffectivePermissionPolicyCap, ExternalDirectoryConfig,
    ExternalDirectoryRule, PathRiskOverlay, PathTrustZone, PermissionConfig,
    PermissionConfirmation, PermissionEvaluationContext, PermissionMode, PermissionPolicy,
    PermissionPolicyChain, PermissionRisk, PermissionRule, ToolOperation,
    classify_path_trust_analysis, classify_path_trust_analysis_with_context,
    classify_path_trust_zone, classify_path_trust_zone_with_context,
    tool_approval_session_grant_availability_for_plan, tool_approval_session_grant_available,
};

fn spec(access: ToolAccess) -> ToolSpec {
    ToolSpec {
        name: "tool".to_owned(),
        description: "tool".to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

fn network_spec(effect: NetworkEffect) -> ToolSpec {
    ToolSpec {
        network_effect: Some(effect),
        ..spec(ToolAccess::Read)
    }
}

#[test]
fn permission_policy_chain_combines_parent_role_and_profile_monotonically() -> Result<()> {
    let parent = PermissionConfig {
        tools: BTreeMap::from([
            ("parent_denied".to_owned(), ApprovalMode::Deny),
            ("parent_allowed".to_owned(), ApprovalMode::Allow),
        ]),
        ..PermissionConfig::default()
    };
    let role = PermissionConfig {
        tools: BTreeMap::from([
            ("parent_denied".to_owned(), ApprovalMode::Allow),
            ("parent_allowed".to_owned(), ApprovalMode::Ask),
        ]),
        ..parent.clone()
    };
    let profile = PermissionConfig {
        tools: BTreeMap::from([
            ("parent_denied".to_owned(), ApprovalMode::Allow),
            ("parent_allowed".to_owned(), ApprovalMode::Allow),
        ]),
        ..parent.clone()
    };
    let context = PermissionEvaluationContext {
        delegated_policy_constraints: vec![role, profile],
        ..PermissionEvaluationContext::default()
    };
    let chain = PermissionPolicyChain::new_with_context(&parent, &context);
    let tool_spec = spec(ToolAccess::Read);

    let denied = chain.decide_with_operation_network_effect_and_default(
        &tool_spec,
        "parent_denied",
        ToolAccess::Read,
        ToolOperation::Read,
        None,
        Vec::new(),
        None,
    )?;
    let narrowed_to_ask = chain.decide_with_operation_network_effect_and_default(
        &tool_spec,
        "parent_allowed",
        ToolAccess::Read,
        ToolOperation::Read,
        None,
        Vec::new(),
        None,
    )?;

    assert_eq!(denied.mode, ApprovalMode::Deny);
    assert_eq!(narrowed_to_ask.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn danger_full_access_suppresses_delegated_ask_but_preserves_delegated_deny() -> Result<()> {
    let primary = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        ..PermissionConfig::default()
    };
    let delegated = PermissionConfig {
        tools: BTreeMap::from([
            ("delegated_ask".to_owned(), ApprovalMode::Ask),
            ("delegated_deny".to_owned(), ApprovalMode::Deny),
        ]),
        ..PermissionConfig::default()
    };
    let context = PermissionEvaluationContext {
        delegated_policy_constraints: vec![delegated],
        ..PermissionEvaluationContext::default()
    };
    let chain = PermissionPolicyChain::new_with_context(&primary, &context);
    let tool_spec = spec(ToolAccess::Read);

    for (tool_name, expected) in [
        ("delegated_ask", ApprovalMode::Allow),
        ("delegated_deny", ApprovalMode::Deny),
    ] {
        let decision = chain.decide_with_operation_network_effect_and_default(
            &tool_spec,
            tool_name,
            ToolAccess::Read,
            ToolOperation::Read,
            None,
            Vec::new(),
            None,
        )?;
        assert_eq!(decision.mode, expected, "tool={tool_name}");
    }
    Ok(())
}

#[test]
fn danger_full_access_preserves_mandatory_durable_memory_approval() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        ..PermissionConfig::default()
    };
    let context = PermissionEvaluationContext::default();
    let policy = PermissionPolicyChain::new_with_context(&config, &context);
    let tool_spec = spec(ToolAccess::Write);

    for operation in [ToolOperation::RememberMemory, ToolOperation::ForgetMemory] {
        let decision = policy.decide_with_operation_network_effect_and_default(
            &tool_spec,
            "durable_memory_tool",
            ToolAccess::Write,
            operation,
            None,
            Vec::new(),
            Some(ApprovalMode::Ask),
        )?;
        assert_eq!(decision.mode, ApprovalMode::Ask, "operation={operation:?}");
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| reason.code == "durable_memory_approval_required")
        );
    }
    Ok(())
}

fn path_subject(path: &str) -> ToolSubject {
    ToolSubject::path(path.to_owned(), path.to_owned())
}

fn external_path_subject(path: PathBuf) -> ToolSubject {
    ToolSubject::path_with_scope(
        path.display().to_string(),
        path.display().to_string(),
        Some(path),
        ToolSubjectScope::External,
    )
}

fn synthetic_external_test_root() -> Result<PathBuf> {
    let current_dir = std::env::current_dir()?;
    let filesystem_root = current_dir
        .ancestors()
        .last()
        .ok_or_else(|| std::io::Error::other("current directory has no filesystem root"))?;
    Ok(std::fs::canonicalize(filesystem_root)?.join("sigil-test-external"))
}

fn external_canonical_path_subject(path: PathBuf) -> ToolSubject {
    ToolSubject::path_with_scope(
        path.display().to_string(),
        path.display().to_string(),
        Some(path),
        ToolSubjectScope::External,
    )
}

fn external_network_endpoint_subject(url: &str) -> ToolSubject {
    ToolSubject {
        kind: ToolSubjectKind::NetworkEndpoint,
        original: url.to_owned(),
        normalized: url.to_owned(),
        canonical_path: None,
        scope: ToolSubjectScope::External,
    }
}

fn command_subject(command: &str) -> ToolSubject {
    ToolSubject::command(command.to_owned(), command.to_owned())
}

#[test]
fn permission_fine_grained_enums_serde_roundtrip() -> Result<()> {
    assert_eq!(
        serde_json::from_str::<ToolOperation>(r#""delete_file""#)?,
        ToolOperation::DeleteFile
    );
    assert_eq!(
        serde_json::from_str::<PathTrustZone>(r#""workspace_project_asset""#)?,
        PathTrustZone::WorkspaceProjectAsset
    );
    assert_eq!(
        serde_json::from_str::<PathRiskOverlay>(r#""sensitive_name""#)?,
        PathRiskOverlay::SensitiveName
    );
    assert_eq!(
        serde_json::from_str::<PermissionRisk>(r#""destructive""#)?,
        PermissionRisk::Destructive
    );
    assert_eq!(
        serde_json::to_string(&PermissionConfirmation::TypePath)?,
        r#"{"kind":"type_path"}"#
    );
    Ok(())
}

#[test]
fn permission_mode_defaults_to_manual_and_parses_all_user_modes() -> Result<()> {
    let default_config: PermissionConfig = toml::from_str("")?;
    assert_eq!(default_config.mode, PermissionMode::Manual);

    for (raw, expected) in [
        ("read-only", PermissionMode::ReadOnly),
        ("manual", PermissionMode::Manual),
        ("auto-edit", PermissionMode::AutoEdit),
        ("danger-full-access", PermissionMode::DangerFullAccess),
    ] {
        let config: PermissionConfig = toml::from_str(&format!(r#"mode = "{raw}""#))?;
        assert_eq!(config.mode, expected);
    }
    Ok(())
}

#[test]
fn command_permission_config_parses_grouped_patterns() -> Result<()> {
    let config: PermissionConfig = toml::from_str(
        r#"
[commands]
allow = ["git status*", "git diff*"]
ask = ["cargo test -p sigil-kernel*"]
deny = ["rm *"]
"#,
    )?;

    assert_eq!(config.commands.allow, ["git status*", "git diff*"]);
    assert_eq!(config.commands.ask, ["cargo test -p sigil-kernel*"]);
    assert_eq!(config.commands.deny, ["rm *"]);
    assert_eq!(config.commands.pattern_count(), 4);
    Ok(())
}

#[test]
fn command_permission_config_rejects_cross_group_duplicates() {
    let error = toml::from_str::<PermissionConfig>(
        r#"
[commands]
allow = ["git status*"]
ask = ["git status*"]
"#,
    )
    .expect_err("duplicate command pattern should fail config load");

    assert!(error.to_string().contains("appears in both allow and ask"));
}

#[test]
fn command_permission_config_rejects_empty_patterns() {
    let error = toml::from_str::<PermissionConfig>(
        r#"
[commands]
allow = ["   "]
"#,
    )
    .expect_err("empty command pattern should fail config load");

    assert!(error.to_string().contains("contains an empty pattern"));
}

#[test]
fn command_permission_allow_can_widen_manual_shell_default() -> Result<()> {
    let config = PermissionConfig {
        commands: CommandPermissionConfig {
            allow: vec!["git status*".to_owned()],
            ..CommandPermissionConfig::default()
        },
        ..PermissionConfig::default()
    };

    let decision = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteUnknownCommand,
        vec![
            command_subject("git status --short"),
            path_subject("src/main.rs"),
        ],
        None,
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn command_permission_ask_and_deny_take_precedence_over_allow() -> Result<()> {
    let config = PermissionConfig {
        commands: CommandPermissionConfig {
            allow: vec!["git *".to_owned(), "rm -i *".to_owned()],
            ask: vec!["git status*".to_owned()],
            deny: vec!["rm *".to_owned()],
        },
        ..PermissionConfig::default()
    };

    let ask = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteUnknownCommand,
        vec![command_subject("git status --short")],
        None,
    )?;
    let deny = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteUnknownCommand,
        vec![command_subject("rm -i target")],
        None,
    )?;

    assert_eq!(ask.mode, ApprovalMode::Ask);
    assert_eq!(deny.mode, ApprovalMode::Deny);
    assert_eq!(ask.command_permission_matches.len(), 2);
    assert_eq!(
        ask.command_permission_matches
            .iter()
            .map(|item| (
                item.group.as_str(),
                item.pattern.as_str(),
                item.command.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("ask", "git status*", "git status --short"),
            ("allow", "git *", "git status --short"),
        ]
    );
    assert_eq!(
        deny.command_permission_matches
            .iter()
            .map(|item| (
                item.group.as_str(),
                item.pattern.as_str(),
                item.command.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("deny", "rm *", "rm -i target"),
            ("allow", "rm -i *", "rm -i target"),
        ]
    );
    Ok(())
}

#[test]
fn command_permission_allow_does_not_override_read_only_or_external_caps() -> Result<()> {
    let read_only = PermissionConfig {
        mode: PermissionMode::ReadOnly,
        commands: CommandPermissionConfig {
            allow: vec!["git status*".to_owned()],
            ..CommandPermissionConfig::default()
        },
        ..PermissionConfig::default()
    };
    let read_only_decision = PermissionPolicy::new(&read_only).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteUnknownCommand,
        vec![command_subject("git status --short")],
        None,
    )?;
    assert_eq!(read_only_decision.mode, ApprovalMode::Deny);

    let external = PermissionConfig {
        commands: CommandPermissionConfig {
            allow: vec!["cat *".to_owned()],
            ..CommandPermissionConfig::default()
        },
        ..PermissionConfig::default()
    };
    let external_decision = PermissionPolicy::new(&external).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteUnknownCommand,
        vec![
            command_subject("cat /tmp/sigil-outside.txt"),
            external_path_subject(PathBuf::from("/tmp/sigil-outside.txt")),
        ],
        None,
    )?;
    assert_eq!(external_decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn manual_mode_allows_read_tools() -> Result<()> {
    let decision = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![path_subject("src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn read_only_mode_is_a_non_read_safety_cap() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::ReadOnly,
        tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn auto_edit_allows_file_edits_readonly_shell_and_network_policy_allow() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::AutoEdit,
        ..PermissionConfig::default()
    };
    let write = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("src/main.rs")],
    )?;
    let shell = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo check", "cargo check")],
    )?;
    let read_only_shell = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Read,
        ToolOperation::ExecuteReadOnlyCommand,
        vec![ToolSubject::command(
            "ls -la _site 2>/dev/null",
            "ls -la _site 2>/dev/null",
        )],
        None,
    )?;
    let network = PermissionPolicy::new(&config).decide(
        &network_spec(NetworkEffect::Unknown),
        "mcp__fake__echo",
        vec![ToolSubject::mcp_tool("mcp__fake__echo")],
    )?;

    assert_eq!(write.mode, ApprovalMode::Allow);
    assert_eq!(read_only_shell.mode, ApprovalMode::Allow);
    assert_eq!(shell.mode, ApprovalMode::Ask);
    assert_eq!(network.mode, ApprovalMode::Allow);
    assert_eq!(network.risk, PermissionRisk::High);
    Ok(())
}

#[test]
fn danger_full_access_cannot_bypass_protected_path_circuit_breaker() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject(".git/config")],
    )?;

    assert_eq!(decision.risk, PermissionRisk::Protected);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn critical_path_circuit_breaker_denies_root_home_and_system_mutations() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let policy = PermissionPolicy::new(&config);
    let root = std::fs::canonicalize(std::path::MAIN_SEPARATOR.to_string())?;
    let home = std::fs::canonicalize(super::home_dir()?)?.join("sigil-hard-safety-target");
    let system = super::platform_critical_system_roots()
        .iter()
        .next()
        .cloned()
        .expect("supported test platforms have critical system roots")
        .join("sigil-hard-safety-target");

    for (label, access, operation, subject) in [
        (
            "root",
            ToolAccess::Execute,
            ToolOperation::ExecuteDestructiveCommand,
            external_canonical_path_subject(root),
        ),
        (
            "home",
            ToolAccess::Write,
            ToolOperation::RecursiveDelete,
            external_canonical_path_subject(home),
        ),
        (
            "system",
            ToolAccess::Write,
            ToolOperation::EditFile,
            external_canonical_path_subject(system.clone()),
        ),
        (
            "system-unknown",
            ToolAccess::Execute,
            ToolOperation::ExecuteUnknownCommand,
            external_canonical_path_subject(system),
        ),
    ] {
        let decision = policy.decide_with_operation_and_default(
            &spec(access),
            "bash",
            access,
            operation,
            vec![subject],
            Some(ApprovalMode::Allow),
        )?;
        assert_eq!(decision.risk, PermissionRisk::Protected, "{label}");
        assert_eq!(decision.mode, ApprovalMode::Deny, "{label}");
        assert!(!decision.external_directory_required, "{label}");
        assert!(!tool_approval_session_grant_available(&decision), "{label}");
        assert!(decision.reasons.iter().any(|reason| {
            reason.source == super::PermissionDecisionSource::HardSafety
                && reason.code == "critical_path_circuit_breaker"
        }));
        let mut approval_override = decision;
        approval_override.request_external_directory_interactive_approval();
        assert_eq!(approval_override.mode, ApprovalMode::Deny, "{label}");
    }
    Ok(())
}

#[test]
fn critical_path_circuit_breaker_preserves_read_only_access_and_workspace_operations() -> Result<()>
{
    let home = std::fs::canonicalize(super::home_dir()?)?;
    let system = super::platform_critical_system_roots()
        .iter()
        .next()
        .cloned()
        .expect("supported test platforms have critical system roots");
    for path in [
        std::fs::canonicalize(std::path::MAIN_SEPARATOR.to_string())?,
        home.clone(),
        system,
    ] {
        let decision = super::PermissionDecision::new_with_operation(
            ApprovalMode::Allow,
            ToolOperation::Read,
            ToolAccess::Read,
            vec![external_canonical_path_subject(path)],
            false,
        );
        assert_eq!(decision.mode, ApprovalMode::Allow);
        assert_ne!(decision.risk, PermissionRisk::Protected);
    }

    let workspace_path = home.join("workspace/src/main.rs");
    let workspace_subject = ToolSubject::path_with_scope(
        "src/main.rs",
        "src/main.rs",
        Some(workspace_path),
        ToolSubjectScope::Workspace,
    );
    let edit = super::PermissionDecision::new_with_operation(
        ApprovalMode::Allow,
        ToolOperation::EditFile,
        ToolAccess::Write,
        vec![workspace_subject.clone()],
        false,
    );
    let delete = super::PermissionDecision::new_with_operation(
        ApprovalMode::Allow,
        ToolOperation::DeleteFile,
        ToolAccess::Write,
        vec![workspace_subject],
        false,
    );
    assert_eq!(edit.mode, ApprovalMode::Allow);
    assert_eq!(edit.risk, PermissionRisk::Medium);
    assert_eq!(delete.mode, ApprovalMode::Ask);
    assert_eq!(delete.risk, PermissionRisk::Destructive);

    let external_home_edit = super::PermissionDecision::new_with_operation(
        ApprovalMode::Allow,
        ToolOperation::EditFile,
        ToolAccess::Write,
        vec![external_canonical_path_subject(
            home.join("Documents/note.txt"),
        )],
        false,
    );
    assert_eq!(external_home_edit.mode, ApprovalMode::Allow);
    assert_eq!(external_home_edit.risk, PermissionRisk::Medium);
    Ok(())
}

#[cfg(unix)]
#[test]
fn critical_path_circuit_breaker_uses_symlink_resolved_canonical_subject() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let link = temp.path().join("home-link");
    symlink(super::home_dir()?, &link)?;
    let subject = ToolSubject::path_with_scope(
        link.display().to_string(),
        link.display().to_string(),
        Some(link.canonicalize()?),
        ToolSubjectScope::External,
    );
    let decision = super::PermissionDecision::new_with_policy_mode_operation_zones_and_overlays(
        PermissionMode::DangerFullAccess,
        ApprovalMode::Allow,
        ToolOperation::RecursiveDelete,
        ToolAccess::Write,
        vec![subject],
        vec![PathTrustZone::External],
        Vec::new(),
        false,
    );

    assert_eq!(decision.risk, PermissionRisk::Protected);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    assert!(!tool_approval_session_grant_available(&decision));
    Ok(())
}

#[test]
fn destructive_allow_is_overlaid_to_ask() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("delete_file".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "delete_file",
        vec![path_subject("src/main.rs")],
    )?;

    assert_eq!(decision.operation, ToolOperation::DeleteFile);
    assert_eq!(decision.risk, PermissionRisk::Destructive);
    assert_eq!(decision.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn mutating_command_is_destructive_even_when_tool_allows() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteMutatingCommand,
        vec![ToolSubject::command("make install", "make install")],
        None,
    )?;

    assert_eq!(decision.operation, ToolOperation::ExecuteMutatingCommand);
    assert_eq!(decision.risk, PermissionRisk::Destructive);
    assert_eq!(decision.mode, ApprovalMode::Ask);
    assert!(decision.snapshot_required);
    Ok(())
}

#[test]
fn protected_paths_deny_even_when_tool_allows() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject(".git/config")],
    )?;

    assert_eq!(
        decision.subject_zones,
        vec![PathTrustZone::WorkspaceGitMetadata]
    );
    assert_eq!(decision.risk, PermissionRisk::Protected);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn project_asset_delete_requires_typed_path_confirmation() -> Result<()> {
    let decision = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Write),
        "delete_file",
        vec![path_subject(".sigil/agents/writer/agent.toml")],
    )?;

    assert_eq!(
        decision.subject_zones,
        vec![PathTrustZone::WorkspaceProjectAsset]
    );
    assert_eq!(decision.risk, PermissionRisk::Destructive);
    assert_eq!(
        decision.confirmation,
        Some(PermissionConfirmation::TypePath)
    );
    assert!(decision.snapshot_required);
    Ok(())
}

#[test]
fn permission_context_classifies_runtime_user_and_project_asset_roots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let state_root = temp.path().join("state");
    let cache_root = temp.path().join("cache");
    std::fs::create_dir_all(workspace.join(".sigil/skills"))?;
    std::fs::create_dir_all(state_root.join("workspaces/ws/artifacts"))?;
    std::fs::create_dir_all(cache_root.join("workspaces/ws"))?;
    let context = PermissionEvaluationContext {
        workspace_root: workspace.clone(),
        project_asset_roots: vec![workspace.join(".sigil")],
        runtime_state_roots: vec![state_root.join("workspaces/ws")],
        user_state_roots: vec![state_root.clone()],
        user_cache_roots: vec![cache_root.clone()],
        delegated_policy_constraints: Vec::new(),
        effective_policy_cap: None,
        network_policy: super::NetworkPolicy::Allow,
    };
    let config = PermissionConfig {
        mode: PermissionMode::AutoEdit,
        ..PermissionConfig::default()
    };
    let policy = PermissionPolicy::new_with_context(&config, &context);

    let project_asset = policy.decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path_with_scope(
            ".sigil/skills/review/SKILL.md",
            ".sigil/skills/review/SKILL.md",
            Some(workspace.join(".sigil/skills/review/SKILL.md")),
            ToolSubjectScope::Workspace,
        )],
    )?;
    assert_eq!(
        project_asset.subject_zones,
        vec![PathTrustZone::WorkspaceProjectAsset]
    );

    let runtime_state = policy.decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path_with_scope(
            "state/artifacts/changesets/change-1",
            "state/artifacts/changesets/change-1",
            Some(state_root.join("workspaces/ws/artifacts/changesets/change-1")),
            ToolSubjectScope::Workspace,
        )],
    )?;
    assert_eq!(
        runtime_state.subject_zones,
        vec![PathTrustZone::WorkspaceRuntimeState]
    );
    assert_eq!(runtime_state.mode, ApprovalMode::Deny);

    let user_cache = policy.decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path_with_scope(
            "cache/tmp/output.txt",
            "cache/tmp/output.txt",
            Some(cache_root.join("workspaces/ws/tmp/output.txt")),
            ToolSubjectScope::Workspace,
        )],
    )?;
    assert_eq!(user_cache.subject_zones, vec![PathTrustZone::UserCache]);
    assert_eq!(user_cache.mode, ApprovalMode::Deny);

    let workspace_doc_path = workspace.join("docs/credentials.md");
    let workspace_doc_analysis = classify_path_trust_analysis_with_context(
        &ToolSubject::path_with_scope(
            workspace_doc_path.display().to_string(),
            workspace_doc_path.display().to_string(),
            Some(workspace_doc_path),
            ToolSubjectScope::Workspace,
        ),
        &context,
    );
    assert_eq!(workspace_doc_analysis.zone, PathTrustZone::WorkspaceDocs);
    assert_eq!(
        workspace_doc_analysis.overlays,
        vec![PathRiskOverlay::SensitiveName]
    );
    Ok(())
}

#[test]
fn permission_context_handles_caps_relative_roots_and_fallbacks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".sigil/plugins"))?;
    let context = PermissionEvaluationContext {
        workspace_root: workspace.clone(),
        project_asset_roots: vec![PathBuf::from(".sigil")],
        runtime_state_roots: Vec::new(),
        user_state_roots: Vec::new(),
        user_cache_roots: Vec::new(),
        delegated_policy_constraints: Vec::new(),
        effective_policy_cap: Some(EffectivePermissionPolicyCap {
            policy_hash: "cap".to_owned(),
            mode: ApprovalMode::Deny,
        }),
        network_policy: super::NetworkPolicy::Allow,
    };
    let config = PermissionConfig::default();
    let policy = PermissionPolicy::new_with_context(&config, &context);

    let capped = policy.decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![ToolSubject::path_with_scope(
            ".sigil/plugins/example/plugin.toml",
            ".sigil/plugins/example/plugin.toml",
            None,
            ToolSubjectScope::Workspace,
        )],
    )?;
    assert_eq!(capped.mode, ApprovalMode::Deny);
    assert_eq!(
        capped.subject_zones,
        vec![PathTrustZone::WorkspaceProjectAsset]
    );

    let empty_subject = ToolSubject::path_with_scope(
        String::new(),
        String::new(),
        None,
        ToolSubjectScope::Workspace,
    );
    assert_eq!(
        classify_path_trust_zone_with_context(&empty_subject, &context),
        PathTrustZone::Unknown
    );

    let outside_workspace = ToolSubject::path_with_scope(
        "outside".to_owned(),
        "outside".to_owned(),
        Some(temp.path().join("outside.txt")),
        ToolSubjectScope::Workspace,
    );
    assert_eq!(
        classify_path_trust_zone_with_context(&outside_workspace, &context),
        PathTrustZone::External
    );
    Ok(())
}

#[test]
fn built_in_path_trust_zone_classifier_covers_sensitive_and_doc_paths() {
    assert_eq!(
        classify_path_trust_zone(&path_subject(".sigil/sessions/session.jsonl")),
        PathTrustZone::WorkspaceRuntimeState
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("sigil.toml")),
        PathTrustZone::WorkspaceConfigSecret
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject(".env.local")),
        PathTrustZone::WorkspaceConfigSecret
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("credentials.json")),
        PathTrustZone::WorkspaceConfigSecret
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("secrets/api.toml")),
        PathTrustZone::WorkspaceConfigSecret
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("docs/en/safety.md")),
        PathTrustZone::WorkspaceDocs
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("docs/credentials.md")),
        PathTrustZone::WorkspaceDocs
    );
    let doc_secret = classify_path_trust_analysis(&path_subject("docs/credentials.md"));
    assert_eq!(doc_secret.zone, PathTrustZone::WorkspaceDocs);
    assert_eq!(doc_secret.overlays, vec![PathRiskOverlay::SensitiveName]);
    assert_eq!(
        classify_path_trust_zone(&path_subject("dev/docs/secret-handling.md")),
        PathTrustZone::WorkspaceDocs
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("src/credentials_provider.rs")),
        PathTrustZone::WorkspaceSource
    );
    assert_eq!(
        classify_path_trust_zone(&path_subject("src/secretary.rs")),
        PathTrustZone::WorkspaceSource
    );
    assert_eq!(
        classify_path_trust_zone(&ToolSubject::command("stdin", "terminal_input")),
        PathTrustZone::Unknown
    );
}

#[test]
fn terminal_input_cannot_be_auto_allowed_by_execute_allow() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("terminal_input".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Execute),
        "terminal_input",
        vec![ToolSubject::command("stdin:12 bytes", "terminal_input")],
    )?;

    assert_eq!(decision.operation, ToolOperation::SendTerminalInput);
    assert_eq!(decision.risk, PermissionRisk::High);
    assert_eq!(decision.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn terminal_management_respects_explicit_allow_without_input_overlay() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([
            ("terminal_resize".to_owned(), ApprovalMode::Allow),
            ("terminal_cancel".to_owned(), ApprovalMode::Allow),
        ]),
        ..PermissionConfig::default()
    };
    let policy = PermissionPolicy::new(&config);

    for (tool_name, operation) in [
        ("terminal_resize", ToolOperation::ResizeTerminalTask),
        ("terminal_cancel", ToolOperation::CancelTerminalTask),
    ] {
        let decision = policy.decide(
            &spec(ToolAccess::Execute),
            tool_name,
            vec![ToolSubject::command("terminal_task:task-1", tool_name)],
        )?;

        assert_eq!(decision.operation, operation);
        assert_eq!(decision.risk, PermissionRisk::Medium);
        assert_eq!(decision.mode, ApprovalMode::Allow);
    }
    Ok(())
}

#[test]
fn destructive_shell_operation_cannot_be_auto_allowed() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteDestructiveCommand,
        vec![path_subject("src/main.rs")],
        None,
    )?;

    assert_eq!(decision.risk, PermissionRisk::Destructive);
    assert_eq!(decision.mode, ApprovalMode::Ask);
    assert!(decision.snapshot_required);
    Ok(())
}

#[test]
fn destructive_shell_operation_on_sigil_root_is_protected() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide_with_operation_and_default(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Execute,
        ToolOperation::ExecuteDestructiveCommand,
        vec![path_subject(".sigil")],
        None,
    )?;

    assert_eq!(
        decision.subject_zones,
        vec![PathTrustZone::WorkspaceRuntimeState]
    );
    assert_eq!(decision.risk, PermissionRisk::Protected);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn permission_execute_defaults_to_ask_while_network_uses_independent_default_allow() -> Result<()> {
    let config = PermissionConfig::default();
    let execute = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo test", "cargo test")],
    )?;
    let network = PermissionPolicy::new(&config).decide(
        &network_spec(NetworkEffect::Unknown),
        "mcp__fake__echo",
        vec![ToolSubject::mcp_tool("mcp__fake__echo")],
    )?;

    assert_eq!(execute.mode, ApprovalMode::Ask);
    assert_eq!(network.mode, ApprovalMode::Allow);
    assert_eq!(network.network_policy_decision, ApprovalMode::Allow);
    assert_eq!(network.risk, PermissionRisk::High);
    Ok(())
}

#[test]
fn permission_dynamic_access_can_downgrade_execute_to_read() -> Result<()> {
    let config = PermissionConfig::default();
    let decision = PermissionPolicy::new(&config).decide_with_access(
        &spec(ToolAccess::Execute),
        "bash",
        ToolAccess::Read,
        vec![ToolSubject::command("pwd", "pwd")],
    )?;

    assert_eq!(decision.access, ToolAccess::Read);
    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_tool_default_mode_is_delegated_source_not_local_baseline() -> Result<()> {
    let subjects = vec![
        ToolSubject::mcp_tool("mcp__fake__echo"),
        ToolSubject::mcp_trust_class("fake", "third_party"),
    ];
    let config = PermissionConfig::default();
    let server_default = PermissionPolicy::new(&config).decide_with_access_and_default(
        &network_spec(NetworkEffect::Unknown),
        "mcp__fake__echo",
        ToolAccess::Read,
        subjects.clone(),
        Some(ApprovalMode::Allow),
    )?;

    assert_eq!(server_default.mode, ApprovalMode::Allow);
    assert_eq!(server_default.local_policy_decision, ApprovalMode::Allow);
    assert_eq!(server_default.source_policy_decision, ApprovalMode::Allow);

    let source_ask = PermissionPolicy::new(&config).decide_with_access_and_default(
        &network_spec(NetworkEffect::Unknown),
        "mcp__fake__echo",
        ToolAccess::Read,
        subjects.clone(),
        Some(ApprovalMode::Ask),
    )?;
    assert_eq!(source_ask.local_policy_decision, ApprovalMode::Allow);
    assert_eq!(source_ask.source_policy_decision, ApprovalMode::Ask);
    assert_eq!(source_ask.mode, ApprovalMode::Ask);

    let local_write_still_asks = PermissionPolicy::new(&config).decide_with_access_and_default(
        &spec(ToolAccess::Write),
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("src/lib.rs", "src/lib.rs")],
        Some(ApprovalMode::Allow),
    )?;
    assert_eq!(
        local_write_still_asks.local_policy_decision,
        ApprovalMode::Ask
    );
    assert_eq!(
        local_write_still_asks.source_policy_decision,
        ApprovalMode::Allow
    );
    assert_eq!(local_write_still_asks.mode, ApprovalMode::Ask);

    let tool_override = PermissionConfig {
        tools: BTreeMap::from([("mcp__fake__echo".to_owned(), ApprovalMode::Ask)]),
        ..PermissionConfig::default()
    };
    let explicit_tool = PermissionPolicy::new(&tool_override).decide_with_access_and_default(
        &network_spec(NetworkEffect::Unknown),
        "mcp__fake__echo",
        ToolAccess::Read,
        subjects.clone(),
        Some(ApprovalMode::Allow),
    )?;

    assert_eq!(explicit_tool.mode, ApprovalMode::Ask);
    assert_eq!(explicit_tool.local_policy_decision, ApprovalMode::Ask);
    assert_eq!(explicit_tool.source_policy_decision, ApprovalMode::Allow);

    let trust_rule = PermissionConfig {
        rules: vec![PermissionRule {
            tool_name: None,
            subject_glob: Some("mcp_trust_class:third_party".to_owned()),
            mode: ApprovalMode::Deny,
        }],
        ..PermissionConfig::default()
    };
    let explicit_rule = PermissionPolicy::new(&trust_rule).decide_with_access_and_default(
        &network_spec(NetworkEffect::Unknown),
        "mcp__fake__echo",
        ToolAccess::Read,
        subjects,
        Some(ApprovalMode::Allow),
    )?;

    assert_eq!(explicit_rule.mode, ApprovalMode::Deny);
    assert_eq!(explicit_rule.local_policy_decision, ApprovalMode::Deny);
    assert_eq!(explicit_rule.source_policy_decision, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_tool_override_is_more_specific_than_mode_baseline() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("read_file".to_owned(), ApprovalMode::Ask)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![path_subject("src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn permission_rules_override_tool_and_mode_defaults() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("read_file".to_owned(), ApprovalMode::Deny)]),
        rules: vec![PermissionRule {
            tool_name: Some("read_file".to_owned()),
            subject_glob: Some("src/**".to_owned()),
            mode: ApprovalMode::Allow,
        }],
        ..PermissionConfig::default()
    };
    let allow = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![path_subject("src/main.rs")],
    )?;
    let deny = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![path_subject("README.md")],
    )?;

    assert_eq!(allow.mode, ApprovalMode::Allow);
    assert_eq!(deny.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn permission_last_matching_rule_wins_for_one_subject() -> Result<()> {
    let config = PermissionConfig {
        rules: vec![
            PermissionRule {
                tool_name: Some("write_file".to_owned()),
                subject_glob: Some("src/**".to_owned()),
                mode: ApprovalMode::Allow,
            },
            PermissionRule {
                tool_name: Some("write_file".to_owned()),
                subject_glob: Some("src/**/*.md".to_owned()),
                mode: ApprovalMode::Deny,
            },
            PermissionRule {
                tool_name: Some("write_file".to_owned()),
                subject_glob: Some("src/docs/public/**".to_owned()),
                mode: ApprovalMode::Allow,
            },
        ],
        ..PermissionConfig::default()
    };
    let allow = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("src/main.rs")],
    )?;
    let deny = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("src/docs/guide.md")],
    )?;
    let public = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("src/docs/public/guide.md")],
    )?;

    assert_eq!(allow.mode, ApprovalMode::Allow);
    assert_eq!(deny.mode, ApprovalMode::Deny);
    assert_eq!(public.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_tool_name_glob_rules_match_tools() -> Result<()> {
    let config = PermissionConfig {
        rules: vec![
            PermissionRule {
                tool_name: Some("mcp__*".to_owned()),
                subject_glob: None,
                mode: ApprovalMode::Deny,
            },
            PermissionRule {
                tool_name: Some("mcp__docs__search".to_owned()),
                subject_glob: None,
                mode: ApprovalMode::Allow,
            },
        ],
        ..PermissionConfig::default()
    };
    let deny = PermissionPolicy::new(&config).decide(
        &network_spec(NetworkEffect::Unknown),
        "mcp__files__read",
        vec![],
    )?;
    let allow = PermissionPolicy::new(&config).decide(
        &network_spec(NetworkEffect::Unknown),
        "mcp__docs__search",
        vec![],
    )?;

    assert_eq!(deny.mode, ApprovalMode::Deny);
    assert_eq!(allow.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_any_subject_ask_requires_approval() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::AutoEdit,
        rules: vec![PermissionRule {
            tool_name: Some("write_file".to_owned()),
            subject_glob: Some("sensitive/**".to_owned()),
            mode: ApprovalMode::Ask,
        }],
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![
            path_subject("src/main.rs"),
            path_subject("sensitive/config.toml"),
        ],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Ask);
    assert_eq!(decision.subjects.len(), 2);
    Ok(())
}

#[test]
fn permission_invalid_subject_glob_returns_error() {
    let config = PermissionConfig {
        rules: vec![PermissionRule {
            tool_name: Some("write_file".to_owned()),
            subject_glob: Some("src/**[".to_owned()),
            mode: ApprovalMode::Allow,
        }],
        ..PermissionConfig::default()
    };
    let error = PermissionPolicy::new(&config)
        .decide(
            &spec(ToolAccess::Write),
            "write_file",
            vec![path_subject("src/main.rs")],
        )
        .expect_err("invalid glob should be surfaced");

    assert!(error.to_string().contains("invalid permission glob"));
}

#[test]
fn permission_external_directory_disabled_denies_external_subjects() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
    let decision = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    assert!(decision.external_directory_required);
    Ok(())
}

#[test]
fn danger_full_access_does_not_turn_disabled_external_directory_deny_into_ask() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
    let config = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        ..PermissionConfig::default()
    };
    let mut decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    assert!(decision.external_directory_required);
    decision.request_external_directory_interactive_approval();
    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn external_network_endpoint_does_not_enter_the_external_directory_gate() -> Result<()> {
    let subject = external_network_endpoint_subject("https://example.com/docs");
    for mode in [PermissionMode::Manual, PermissionMode::AutoEdit] {
        let config = PermissionConfig {
            mode,
            ..PermissionConfig::default()
        };
        let decision = PermissionPolicy::new(&config)
            .decide_with_operation_network_effect_and_default(
                &network_spec(NetworkEffect::Read),
                "webfetch",
                ToolAccess::Read,
                ToolOperation::NetworkRequest,
                Some(NetworkEffect::Read),
                vec![subject.clone()],
                None,
            )?;

        assert_eq!(decision.local_policy_decision, ApprovalMode::Allow);
        assert_eq!(decision.network_policy_decision, ApprovalMode::Allow);
        assert_eq!(decision.mode, ApprovalMode::Allow);
        assert!(!decision.external_directory_required);
        assert_eq!(decision.subject_zones, [PathTrustZone::Unknown]);
    }
    Ok(())
}

#[test]
fn permission_external_directory_enabled_defaults_to_ask() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            ..ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Ask);
    assert!(!decision.external_directory_required);
    Ok(())
}

#[test]
fn session_grant_availability_requires_stable_low_or_medium_risk_scope() -> Result<()> {
    let external_path = synthetic_external_test_root()?.join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            ..ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    };
    let external_read = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path.clone())],
    )?;
    assert!(tool_approval_session_grant_available(&external_read));

    let external_write = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![external_path_subject(external_path)],
    )?;
    assert_eq!(
        external_write.confirmation,
        Some(PermissionConfirmation::TypePath)
    );
    assert!(!tool_approval_session_grant_available(&external_write));

    let destructive = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Write),
        "delete_file",
        vec![path_subject("src/main.rs")],
    )?;
    assert!(!tool_approval_session_grant_available(&destructive));
    Ok(())
}

#[test]
fn session_grant_availability_reports_one_typed_reason_or_none() {
    let decision = super::PermissionDecision::new(
        ApprovalMode::Ask,
        "read_file",
        ToolAccess::Read,
        vec![path_subject("src/main.rs")],
        false,
    );
    let mut plan = ToolPermissionPlanV2 {
        schema_version: crate::TOOL_PERMISSION_PLAN_SCHEMA_VERSION,
        tool_name: "read_file".to_owned(),
        access: ToolAccess::Read,
        operation: ToolOperation::Read,
        effects: BTreeSet::from([ToolPermissionEffect::FileRead]),
        subjects: decision.subjects.clone(),
        analysis: ToolAnalysisStatus::Complete,
        containment: ExecutionContainmentRequest::default(),
        semantic_scope: Some(ToolSemanticScope::new("file_read", 1)),
        tool_default_mode: None,
        analysis_bindings: BTreeMap::from([
            ("execution_backend".to_owned(), "local".to_owned()),
            ("execution_profile".to_owned(), "workspace".to_owned()),
            ("environment_binding".to_owned(), "default".to_owned()),
        ]),
        plan_hash: "sha256:test".to_owned(),
        safe_summary: ToolPermissionSummary::default(),
    };
    let available = tool_approval_session_grant_availability_for_plan(&decision, &plan);
    assert!(available.is_available());
    assert_eq!(available.unavailable_reason(), None);

    plan.analysis = ToolAnalysisStatus::Conservative {
        reasons: vec![ToolAnalysisReason::new(
            ToolAnalysisReasonCode::UnprovenContainment,
            Option::<String>::None,
        )],
    };
    let unavailable = tool_approval_session_grant_availability_for_plan(&decision, &plan);
    assert!(!unavailable.is_available());
    assert_eq!(
        unavailable.unavailable_reason().map(|reason| reason.code),
        Some(ToolApprovalSessionGrantUnavailableReasonCode::AnalysisIncomplete)
    );

    plan.analysis = ToolAnalysisStatus::Complete;
    plan.semantic_scope = None;
    assert_eq!(
        tool_approval_session_grant_availability_for_plan(&decision, &plan)
            .unavailable_reason()
            .map(|reason| reason.code),
        Some(ToolApprovalSessionGrantUnavailableReasonCode::SemanticScopeUnavailable)
    );

    plan.semantic_scope = Some(ToolSemanticScope::new("file_read", 1));
    plan.effects.insert(ToolPermissionEffect::CredentialAccess);
    assert_eq!(
        tool_approval_session_grant_availability_for_plan(&decision, &plan)
            .unavailable_reason()
            .map(|reason| reason.code),
        Some(ToolApprovalSessionGrantUnavailableReasonCode::NonGrantableEffect)
    );

    plan.effects.remove(&ToolPermissionEffect::CredentialAccess);
    plan.analysis_bindings.clear();
    assert_eq!(
        tool_approval_session_grant_availability_for_plan(&decision, &plan)
            .unavailable_reason()
            .map(|reason| reason.code),
        Some(ToolApprovalSessionGrantUnavailableReasonCode::ContainmentBindingUnavailable)
    );

    plan.analysis_bindings = BTreeMap::from([
        ("execution_backend".to_owned(), "local".to_owned()),
        ("execution_profile".to_owned(), "workspace".to_owned()),
        ("environment_binding".to_owned(), "default".to_owned()),
    ]);
    let mut confirmation_decision = decision;
    confirmation_decision.confirmation = Some(PermissionConfirmation::Standard);
    assert_eq!(
        tool_approval_session_grant_availability_for_plan(&confirmation_decision, &plan)
            .unavailable_reason()
            .map(|reason| reason.code),
        Some(ToolApprovalSessionGrantUnavailableReasonCode::ConfirmationRequired)
    );
}

#[test]
fn workspace_config_secret_write_is_protected_and_not_session_grantable() -> Result<()> {
    let decision = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("sigil.toml")],
    )?;

    assert_eq!(decision.risk, PermissionRisk::Protected);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    assert!(
        decision
            .subject_zones
            .contains(&PathTrustZone::WorkspaceConfigSecret)
    );
    assert!(!tool_approval_session_grant_available(&decision));
    Ok(())
}

#[test]
fn sensitive_named_doc_write_is_protected_by_overlay() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Write),
        "write_file",
        vec![path_subject("docs/credentials.md")],
    )?;

    assert_eq!(decision.subject_zones, vec![PathTrustZone::WorkspaceDocs]);
    assert_eq!(
        decision.subject_risk_overlays,
        vec![PathRiskOverlay::SensitiveName]
    );
    assert_eq!(decision.risk, PermissionRisk::Protected);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    assert!(!tool_approval_session_grant_available(&decision));
    Ok(())
}

#[test]
fn session_grant_availability_rejects_unknown_high_risk_commands() -> Result<()> {
    let exact_command = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo check 2>&1", "cargo check 2>&1")],
    )?;
    assert_eq!(
        exact_command.operation,
        ToolOperation::ExecuteUnknownCommand
    );
    assert_eq!(exact_command.risk, PermissionRisk::High);
    assert!(!tool_approval_session_grant_available(&exact_command));

    let truncated_command = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "cargo check --workspace --all-targets --features really-long-feature-name",
            "cargo check --workspace --all-targets...",
        )],
    )?;
    assert!(!tool_approval_session_grant_available(&truncated_command));

    let external_path = tempfile::tempdir()?
        .path()
        .canonicalize()?
        .join("input.txt");
    let external_command = PermissionPolicy::new(&PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            ..ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    })
    .decide(
        &spec(ToolAccess::Execute),
        "bash",
        vec![
            ToolSubject::command(
                "python script.py /tmp/input.txt",
                "python script.py /tmp/input.txt",
            ),
            external_path_subject(external_path),
        ],
    )?;
    assert!(!tool_approval_session_grant_available(&external_command));

    Ok(())
}

#[test]
fn permission_external_directory_rules_can_allow_matching_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_root = temp.path().canonicalize()?;
    std::fs::create_dir_all(external_root.join("allowed"))?;
    let external_path = external_root.join("allowed").join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Ask,
            rules: vec![ExternalDirectoryRule {
                path_glob: format!("{}/allowed/**", external_root.display()),
                mode: ApprovalMode::Allow,
            }],
        },
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_external_directory_uses_default_when_rules_do_not_match() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_root = temp.path().canonicalize()?;
    std::fs::create_dir_all(external_root.join("allowed"))?;
    std::fs::create_dir_all(external_root.join("other"))?;
    let external_path = external_root.join("other").join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Deny,
            rules: vec![ExternalDirectoryRule {
                path_glob: format!("{}/allowed/**", external_root.display()),
                mode: ApprovalMode::Allow,
            }],
        },
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn permission_external_directory_rules_are_compiled_once_per_policy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_root = temp.path().canonicalize()?;
    let allowed_dir = external_root.join("allowed");
    std::fs::create_dir_all(&allowed_dir)?;
    let external_path = allowed_dir.join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Ask,
            rules: vec![ExternalDirectoryRule {
                path_glob: format!("{}/allowed/**", external_root.display()),
                mode: ApprovalMode::Allow,
            }],
        },
        ..PermissionConfig::default()
    };
    let policy = PermissionPolicy::new(&config);

    std::fs::remove_dir_all(&allowed_dir)?;
    let decision = policy.decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_external_directory_last_matching_rule_wins() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_root = temp.path().canonicalize()?;
    std::fs::create_dir_all(external_root.join("allowed"))?;
    let external_path = external_root.join("allowed").join("secret.txt");
    std::fs::write(&external_path, "secret")?;
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Ask,
            rules: vec![
                ExternalDirectoryRule {
                    path_glob: format!("{}/allowed/**", external_root.display()),
                    mode: ApprovalMode::Allow,
                },
                ExternalDirectoryRule {
                    path_glob: format!("{}/allowed/secret.txt", external_root.display()),
                    mode: ApprovalMode::Deny,
                },
                ExternalDirectoryRule {
                    path_glob: format!("{}/allowed/secret.txt", external_root.display()),
                    mode: ApprovalMode::Allow,
                },
            ],
        },
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![external_path_subject(external_path)],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn permission_external_directory_rule_rejects_parent_components() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            rules: vec![ExternalDirectoryRule {
                path_glob: format!("{}/../**", temp.path().display()),
                mode: ApprovalMode::Allow,
            }],
            ..ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    };
    let error = PermissionPolicy::new(&config)
        .decide(
            &spec(ToolAccess::Read),
            "read_file",
            vec![external_path_subject(external_path)],
        )
        .expect_err("parent components should be rejected");

    assert!(error.to_string().contains("must not contain .."));
    Ok(())
}

#[test]
fn permission_rule_without_subject_glob_applies_when_call_has_no_subjects() -> Result<()> {
    let config = PermissionConfig {
        rules: vec![PermissionRule {
            tool_name: Some("bash".to_owned()),
            subject_glob: None,
            mode: ApprovalMode::Deny,
        }],
        ..PermissionConfig::default()
    };

    let decision =
        PermissionPolicy::new(&config).decide(&spec(ToolAccess::Execute), "bash", vec![])?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    assert!(decision.subjects.is_empty());
    Ok(())
}

#[test]
fn permission_external_directory_rule_requires_absolute_or_home_anchored_glob() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
    let config = PermissionConfig {
        external_directory: ExternalDirectoryConfig {
            enabled: true,
            rules: vec![ExternalDirectoryRule {
                path_glob: "relative/path/**".to_owned(),
                mode: ApprovalMode::Allow,
            }],
            ..ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    };

    let error = PermissionPolicy::new(&config)
        .decide(
            &spec(ToolAccess::Read),
            "read_file",
            vec![external_path_subject(external_path)],
        )
        .expect_err("relative external glob should fail");

    assert!(error.to_string().contains("must be absolute"));
    Ok(())
}

#[test]
fn permission_helper_matchers_cover_any_missing_subject_and_invalid_external_rules() -> Result<()> {
    let any_rule = PermissionRule {
        tool_name: None,
        subject_glob: None,
        mode: ApprovalMode::Allow,
    };
    let any_compiled = super::CompiledPermissionRule::new(&any_rule);
    assert!(any_compiled.matches("read_file", None)?);

    let subject_rule = PermissionRule {
        tool_name: Some("read_file".to_owned()),
        subject_glob: Some("src/**".to_owned()),
        mode: ApprovalMode::Allow,
    };
    let subject_compiled = super::CompiledPermissionRule::new(&subject_rule);
    let error = subject_compiled
        .matches("read_file", None)
        .expect_err("subject-specific rules should require a subject");
    assert!(error.to_string().contains("requires a subject"));

    let invalid_external_rule = ExternalDirectoryRule {
        path_glob: "relative/**".to_owned(),
        mode: ApprovalMode::Allow,
    };
    let invalid_compiled = super::CompiledExternalDirectoryRule::new(&invalid_external_rule);
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
    let invalid_error = invalid_compiled
        .matches_subject(&external_path_subject(external_path))
        .expect_err("relative external rules should stay invalid");
    assert!(invalid_error.to_string().contains("must be absolute"));

    assert!(!invalid_compiled.matches_subject(&path_subject("src/main.rs"))?);
    assert!(super::CompiledMatcher::Any.is_match("/tmp")?);
    Ok(())
}

#[test]
fn permission_external_path_helpers_expand_home_and_validate_patterns() -> Result<()> {
    let home = super::home_dir()?;
    assert_eq!(
        super::expand_external_rule_path("~")?,
        home.display().to_string()
    );
    assert_eq!(
        super::expand_external_rule_path("~/sigil")?,
        home.join("sigil").display().to_string()
    );
    assert_eq!(
        super::expand_external_rule_path("$HOME/sigil")?,
        home.join("sigil").display().to_string()
    );
    assert_eq!(
        super::expand_external_rule_path("$HOME")?,
        home.display().to_string()
    );
    assert_eq!(super::home_dir()?, home);

    let unsupported = super::expand_external_rule_path("$TMP/sigil")
        .expect_err("only HOME expansion should be accepted");
    assert!(unsupported.to_string().contains("only supports $HOME"));

    let absolute_rule = format!("{}/**/*.txt", home.display());
    let pattern = super::canonical_external_rule_pattern(&absolute_rule)?;
    assert!(pattern.starts_with(&std::fs::canonicalize(&home)?.display().to_string()));

    let relative = super::canonical_external_rule_pattern("notes/**")
        .expect_err("relative patterns should be rejected");
    assert!(relative.to_string().contains("must be absolute"));

    let missing_prefix = super::canonical_external_rule_pattern(&format!(
        "{}/sigil-missing-{}/*.txt",
        std::env::temp_dir().display(),
        uuid::Uuid::new_v4()
    ))
    .expect_err("missing literal prefixes should be rejected");
    assert!(missing_prefix.to_string().contains("literal prefix"));
    Ok(())
}
#[test]
fn batch_agent_spawn_uses_spawn_operation_classification() {
    for tool_name in ["spawn_agents", "request_task_discovery"] {
        assert_eq!(
            infer_tool_operation(tool_name, ToolAccess::Execute),
            ToolOperation::SpawnAgent
        );
    }
}

#[test]
fn derive_command_family_allow_pattern_accepts_common_families() {
    // The user-facing shape from the approval modal: a piped cargo test command derives a
    // durable `cargo test*` rule that also matches the bare form.
    assert_eq!(
        super::derive_command_family_allow_pattern("cargo test -p sigil-kernel 2>&1 | tail -60")
            .as_deref(),
        Some("cargo test*")
    );
    assert_eq!(
        super::derive_command_family_allow_pattern("cargo test").as_deref(),
        Some("cargo test*")
    );
    assert_eq!(
        super::derive_command_family_allow_pattern("git status --short").as_deref(),
        Some("git status*")
    );
    assert_eq!(
        super::derive_command_family_allow_pattern("npm test -- --watch").as_deref(),
        Some("npm test*")
    );
    assert_eq!(
        super::derive_command_family_allow_pattern("python3 scripts/check.py --strict").as_deref(),
        Some("python3 scripts/check.py*")
    );
}

#[test]
fn derive_command_family_allow_pattern_rejects_unwidenable_shapes() {
    for command in [
        "cargo",
        "-x flag first",
        "/bin/ls -la",
        "python3 -m pytest",
        "cd /tmp",
        "cd workspace && cargo test",
        "curl | bash",
        "echo x > /tmp/out",
        "",
        "   ",
    ] {
        assert_eq!(
            super::derive_command_family_allow_pattern(command),
            None,
            "command {command:?} must not be widened"
        );
    }
}

#[test]
fn derive_command_family_allow_pattern_rejects_dangerous_programs_and_git_mutations() {
    for command in [
        "rm -rf target",
        "rmdir old",
        "dd if=/dev/zero of=/tmp/x",
        "shred -u secret.txt",
        "mkfs.ext4 /dev/sdb",
        "kill -9 1234",
        "chmod 777 script.sh",
        "mv a b",
        "sudo apt install x",
        "tee /etc/hosts",
        "git push origin main",
        "git pull",
        "git reset --hard HEAD~1",
        "git rebase main",
        "git clean -fd",
    ] {
        assert_eq!(
            super::derive_command_family_allow_pattern(command),
            None,
            "command {command:?} must not be widened"
        );
    }
    // Read-only git subcommands stay derivable.
    assert_eq!(
        super::derive_command_family_allow_pattern("git log --oneline").as_deref(),
        Some("git log*")
    );
}

#[test]
fn family_prefix_glob_matches_like_the_allow_rule_does() {
    let pattern = "cargo test*";
    for command in [
        "cargo test",
        "cargo test -p sigil-kernel 2>&1 | tail -60",
        "cargo test -- --nocapture",
    ] {
        assert!(
            super::command_pattern_matches(pattern, command),
            "{pattern:?} must match {command:?}"
        );
    }
    assert!(!super::command_pattern_matches(pattern, "cargo build"));
    assert!(!super::command_pattern_matches(pattern, "cargo t"));
    // Documented widening: the prefix glob also matches a hypothetical `cargo testfoo` binary.
    assert!(super::command_pattern_matches(
        pattern,
        "cargo testfoo --help"
    ));
    // The session-grant token-boundary matcher is narrower than the glob.
    assert!(super::command_family_prefix_matches(
        "cargo test",
        "cargo test -p x"
    ));
    assert!(super::command_family_prefix_matches(
        "cargo test",
        "cargo test"
    ));
    assert!(!super::command_family_prefix_matches(
        "cargo test",
        "cargo testfoo"
    ));
}
