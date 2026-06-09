use anyhow::Result;
use serde_json::json;

use crate::ToolSpec;

use super::{ApprovalMode, PermissionConfig, PermissionPolicy, PermissionRule};

fn write_spec() -> ToolSpec {
    ToolSpec {
        name: "write_file".to_owned(),
        description: "write".to_owned(),
        input_schema: json!({"type":"object"}),
        read_only: false,
    }
}

#[test]
fn permission_rules_override_default_write_mode() -> Result<()> {
    let config = PermissionConfig {
        write_mode: ApprovalMode::Ask,
        rules: vec![PermissionRule {
            tool_name: "write_file".to_owned(),
            subject_glob: None,
            mode: ApprovalMode::Deny,
        }],
    };
    let decision = PermissionPolicy::new(&config).decide(
        &write_spec(),
        "write_file",
        Some("src/main.rs".to_owned()),
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn permission_rules_match_subject_glob() -> Result<()> {
    let config = PermissionConfig {
        write_mode: ApprovalMode::Ask,
        rules: vec![
            PermissionRule {
                tool_name: "write_file".to_owned(),
                subject_glob: Some("src/**".to_owned()),
                mode: ApprovalMode::Allow,
            },
            PermissionRule {
                tool_name: "write_file".to_owned(),
                subject_glob: Some("src/**/*.md".to_owned()),
                mode: ApprovalMode::Deny,
            },
        ],
    };
    let allow = PermissionPolicy::new(&config).decide(
        &write_spec(),
        "write_file",
        Some("src/main.rs".to_owned()),
    )?;
    let deny = PermissionPolicy::new(&config).decide(
        &write_spec(),
        "write_file",
        Some("src/docs/guide.md".to_owned()),
    )?;

    assert_eq!(allow.mode, ApprovalMode::Allow);
    assert_eq!(deny.mode, ApprovalMode::Deny);
    Ok(())
}
