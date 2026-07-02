use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::{
    ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec, ToolSubject, ToolSubjectScope,
};

use super::{
    ApprovalMode, EffectivePermissionPolicyCap, ExternalDirectoryConfig, ExternalDirectoryRule,
    PathTrustZone, PermissionAccessConfig, PermissionConfig, PermissionConfirmation,
    PermissionEvaluationContext, PermissionPolicy, PermissionPreset, PermissionRisk,
    PermissionRule, ToolOperation, classify_path_trust_zone, classify_path_trust_zone_with_context,
    tool_approval_session_grant_available,
};

fn spec(access: ToolAccess) -> ToolSpec {
    ToolSpec {
        name: "tool".to_owned(),
        description: "tool".to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        preview: ToolPreviewCapability::None,
    }
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
fn permission_preset_defaults_to_balanced_and_parses_read_only() -> Result<()> {
    let default_config: PermissionConfig = toml::from_str("")?;
    assert_eq!(default_config.preset, PermissionPreset::Balanced);

    let read_only: PermissionConfig = toml::from_str(r#"preset = "read_only""#)?;
    assert_eq!(read_only.preset, PermissionPreset::ReadOnly);
    Ok(())
}

#[test]
fn permission_access_overrides_default_mode_for_read_tools() -> Result<()> {
    let decision = PermissionPolicy::new(&PermissionConfig::default()).decide(
        &spec(ToolAccess::Read),
        "read_file",
        vec![path_subject("src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn read_only_preset_is_a_write_safety_cap() -> Result<()> {
    let config = PermissionConfig {
        preset: PermissionPreset::ReadOnly,
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
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
        effective_policy_cap: None,
    };
    let config = PermissionConfig {
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
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
        effective_policy_cap: Some(EffectivePermissionPolicyCap {
            policy_hash: "cap".to_owned(),
            mode: ApprovalMode::Deny,
        }),
    };
    let config = PermissionConfig {
        access: PermissionAccessConfig {
            read: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
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
        PathTrustZone::WorkspaceSource
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
        classify_path_trust_zone(&path_subject("docs/en/safety.md")),
        PathTrustZone::WorkspaceDocs
    );
    assert_eq!(
        classify_path_trust_zone(&ToolSubject::command("stdin", "terminal_input")),
        PathTrustZone::Unknown
    );
}

#[test]
fn terminal_input_cannot_be_auto_allowed_by_execute_allow() -> Result<()> {
    let config = PermissionConfig {
        access: PermissionAccessConfig {
            execute: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
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
fn permission_execute_and_network_inherit_default_ask() -> Result<()> {
    let config = PermissionConfig::default();
    let execute = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo test", "cargo test")],
    )?;
    let network = PermissionPolicy::new(&config).decide(
        &spec(ToolAccess::Network),
        "mcp__fake__echo",
        vec![ToolSubject::mcp_tool("mcp__fake__echo")],
    )?;

    assert_eq!(execute.mode, ApprovalMode::Ask);
    assert_eq!(network.mode, ApprovalMode::Ask);
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
fn permission_tool_default_mode_is_between_access_default_and_tool_rules() -> Result<()> {
    let subjects = vec![
        ToolSubject::mcp_tool("mcp__fake__echo"),
        ToolSubject::mcp_trust_class("fake", "third_party"),
    ];
    let access_deny = PermissionConfig {
        access: PermissionAccessConfig {
            network: Some(ApprovalMode::Deny),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
    let server_default = PermissionPolicy::new(&access_deny).decide_with_access_and_default(
        &spec(ToolAccess::Network),
        "mcp__fake__echo",
        ToolAccess::Network,
        subjects.clone(),
        Some(ApprovalMode::Allow),
    )?;

    assert_eq!(server_default.mode, ApprovalMode::Allow);

    let tool_override = PermissionConfig {
        access: PermissionAccessConfig {
            network: Some(ApprovalMode::Deny),
            ..PermissionAccessConfig::default()
        },
        tools: BTreeMap::from([("mcp__fake__echo".to_owned(), ApprovalMode::Ask)]),
        ..PermissionConfig::default()
    };
    let explicit_tool = PermissionPolicy::new(&tool_override).decide_with_access_and_default(
        &spec(ToolAccess::Network),
        "mcp__fake__echo",
        ToolAccess::Network,
        subjects.clone(),
        Some(ApprovalMode::Allow),
    )?;

    assert_eq!(explicit_tool.mode, ApprovalMode::Ask);

    let trust_rule = PermissionConfig {
        rules: vec![PermissionRule {
            tool_name: None,
            subject_glob: Some("mcp_trust_class:third_party".to_owned()),
            mode: ApprovalMode::Deny,
        }],
        ..PermissionConfig::default()
    };
    let explicit_rule = PermissionPolicy::new(&trust_rule).decide_with_access_and_default(
        &spec(ToolAccess::Network),
        "mcp__fake__echo",
        ToolAccess::Network,
        subjects,
        Some(ApprovalMode::Allow),
    )?;

    assert_eq!(explicit_rule.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn permission_tool_override_is_more_specific_than_access_default() -> Result<()> {
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
fn permission_rules_override_tool_and_access_defaults() -> Result<()> {
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
fn permission_deny_dominates_among_matching_rules() -> Result<()> {
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

    assert_eq!(allow.mode, ApprovalMode::Allow);
    assert_eq!(deny.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn permission_any_subject_ask_requires_approval() -> Result<()> {
    let config = PermissionConfig {
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
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
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("note.txt");
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
fn permission_external_directory_deny_rule_dominates_allow() -> Result<()> {
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
            ],
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
    assert!(any_compiled.matches_subject("read_file", None)?);

    let subject_rule = PermissionRule {
        tool_name: Some("read_file".to_owned()),
        subject_glob: Some("src/**".to_owned()),
        mode: ApprovalMode::Allow,
    };
    let subject_compiled = super::CompiledPermissionRule::new(&subject_rule);
    let error = subject_compiled
        .matches_subject("read_file", None)
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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME should be available in tests");
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
    assert!(pattern.starts_with(&home.display().to_string()));

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
