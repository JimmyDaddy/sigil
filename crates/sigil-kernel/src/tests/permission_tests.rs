use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use serde_json::json;

use crate::{
    ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec, ToolSubject, ToolSubjectScope,
};

use super::{
    ApprovalMode, ExternalDirectoryConfig, ExternalDirectoryRule, PermissionAccessConfig,
    PermissionConfig, PermissionPolicy, PermissionRule,
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
