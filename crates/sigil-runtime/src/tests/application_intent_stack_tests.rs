use std::path::Path;

use anyhow::Result;
use sigil_kernel::{
    IntentDigest, IntentDropRequestV1, IntentOperationId, IntentStackVersion, PermissionMode,
    RootConfig, Session,
};

use crate::{
    ApplicationIntentConfirmationSource, ApplicationIntentStackCommandOutputV1,
    ApplicationIntentStackCommandV1, ApplicationIntentStackErrorClass,
    execute_application_intent_stack_command,
};

fn drop_request() -> Result<IntentDropRequestV1> {
    Ok(IntentDropRequestV1 {
        operation_id: IntentOperationId::new("operation-1")?,
        stack_version: IntentStackVersion::new(1)?,
        preview_digest: IntentDigest::new(format!("sha256:jcs-v1:{}", "a".repeat(64)))?,
    })
}

fn root_config(permission_mode: &str) -> Result<RootConfig> {
    RootConfig::parse_persisted(&format!(
        r#"
config_version = 2

[workspace]
root = "."

[agent]
connection = "test"
model = "test-model"

[permission]
mode = "{permission_mode}"

[connections.test]
label = "Test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = {{ source = "none" }}
"#
    ))
}

#[test]
fn canonical_command_inspect_returns_the_kernel_projection_unchanged() -> Result<()> {
    let session = Session::new("provider", "model");
    let expected = session.public_intent_stack_state_for_workspace(Path::new("."))?;
    let output = execute_application_intent_stack_command(
        &session,
        &root_config("manual")?,
        Path::new("."),
        &ApplicationIntentStackCommandV1::Inspect,
        ApplicationIntentConfirmationSource::Automation,
    )?;
    assert_eq!(
        output,
        ApplicationIntentStackCommandOutputV1::Projection { state: expected }
    );
    Ok(())
}

#[test]
fn canonical_execute_command_denies_read_only_before_creating_authority() -> Result<()> {
    let root_config = root_config("read-only")?;
    assert_eq!(root_config.permission.mode, PermissionMode::ReadOnly);
    let error = execute_application_intent_stack_command(
        &Session::new("provider", "model"),
        &root_config,
        Path::new("."),
        &ApplicationIntentStackCommandV1::ExecuteDrop {
            request: drop_request()?,
        },
        ApplicationIntentConfirmationSource::Http,
    )
    .expect_err("read-only execution must fail closed");
    assert_eq!(
        error.class(),
        ApplicationIntentStackErrorClass::PermissionRequired
    );
    Ok(())
}

#[test]
fn canonical_command_wire_contains_no_host_authority_or_file_payload() -> Result<()> {
    let value = serde_json::to_value(ApplicationIntentStackCommandV1::ExecuteDrop {
        request: drop_request()?,
    })?;
    assert_eq!(value["action"], "execute_drop");
    assert!(value["request"]["operation_id"].is_string());
    assert!(value["request"]["stack_version"].is_number());
    assert!(value["request"]["preview_digest"].is_string());
    let encoded = serde_json::to_string(&value)?;
    for forbidden in [
        "permission_policy",
        "approval_authority",
        "workspace_root",
        "file_effects",
        "patch",
        "content",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
    Ok(())
}
