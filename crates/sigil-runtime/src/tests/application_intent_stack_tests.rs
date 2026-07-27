use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::symlink;

use anyhow::Result;
use sigil_kernel::{
    ConversationRunStartedEntryV1, IntentDigest, IntentDropRequestV1, IntentId, IntentOperationId,
    IntentStackVersion, IntentVersionRef, JsonlSessionStore, PermissionMode, RootConfig, Session,
};

use super::ensure_durable_intent_stack_session_idle;
use crate::{
    ApplicationIntentConfirmationSource, ApplicationIntentStackCommandOutputV1,
    ApplicationIntentStackCommandV1, ApplicationIntentStackErrorClass,
    execute_application_intent_stack_command, execute_durable_application_intent_stack_command,
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

#[test]
fn durable_mutation_gate_rejects_an_active_run_and_reopens_after_terminal_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let session = Session::new("provider", "model").with_store(store);
    let recorder = session.conversation_run_lifecycle_recorder()?;

    ensure_durable_intent_stack_session_idle(&session_path)?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-active", 1)?)?;
    assert!(
        ensure_durable_intent_stack_session_idle(&session_path)
            .expect_err("active run must exclude an out-of-process mutation")
            .to_string()
            .contains("foreground run is active")
    );
    recorder.reconcile_unfinished(2)?;
    ensure_durable_intent_stack_session_idle(&session_path)?;
    Ok(())
}

#[test]
fn durable_inspect_remains_available_to_recovery_ui_while_mutation_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let session = Session::new("provider", "model").with_store(store);
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new("run-active", 1)?)?;
    let missing_config = temp.path().join("missing.toml");

    let inspect_error = execute_durable_application_intent_stack_command(
        &missing_config,
        temp.path(),
        &session_path,
        session.session_scope_id(),
        &ApplicationIntentStackCommandV1::Inspect,
        ApplicationIntentConfirmationSource::Automation,
    )
    .expect_err("missing config should be reached after inspect bypasses the mutation gate");
    assert_eq!(
        inspect_error.class(),
        ApplicationIntentStackErrorClass::Unavailable
    );

    let mutation_error = execute_durable_application_intent_stack_command(
        &missing_config,
        temp.path(),
        &session_path,
        session.session_scope_id(),
        &ApplicationIntentStackCommandV1::PreviewDrop {
            intent_ref: IntentVersionRef::new(IntentId::new("intent-active")?, 1)?,
        },
        ApplicationIntentConfirmationSource::Automation,
    )
    .expect_err("active run must fail before loading adapter configuration");
    assert_eq!(
        mutation_error.class(),
        ApplicationIntentStackErrorClass::Conflict
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn durable_command_rejects_a_symlink_before_lifecycle_or_session_loading() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let target = temp.path().join("target.jsonl");
    let _store = JsonlSessionStore::new(&target)?;
    let link = temp.path().join("session.jsonl");
    symlink(&target, &link)?;

    let error = execute_durable_application_intent_stack_command(
        &temp.path().join("missing.toml"),
        temp.path(),
        &link,
        "session-scope",
        &ApplicationIntentStackCommandV1::Inspect,
        ApplicationIntentConfirmationSource::Automation,
    )
    .expect_err("symlink session sources must fail closed");
    assert_eq!(error.class(), ApplicationIntentStackErrorClass::Unavailable);
    Ok(())
}
