use std::fs;

use anyhow::{Context, Result};
use serde_json::json;
use sigil_kernel::{
    JsonlSessionStore, ToolArtifactBindingV1, ToolArtifactSensitivity, ToolArtifactStore, ToolCall,
    ToolContext, ToolRegistry, ToolResultRecordedV3, ToolStorageCompletenessV1,
};

use crate::{BuiltinToolPaths, register_builtin_tools_with_paths};

fn command(command: &str) -> ToolCall {
    ToolCall {
        id: "execution-capture".to_owned(),
        name: "exec_command".to_owned(),
        args_json: json!({"command": command, "shell": "sh", "yield_time_ms": 10000}).to_string(),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exec_capture_storage_failure_preserves_success_without_republishing_preview() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&workspace)?;
    const OBSERVED: usize = 512 * 1024;
    fs::write(workspace.join("source-output.txt"), vec![b'x'; OBSERVED])?;
    let session_store = JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    let artifact_store = ToolArtifactStore::for_session_store(&session_store);
    let staging = artifact_store.staging_root();
    fs::create_dir_all(staging.parent().context("staging parent")?)?;
    // A concrete storage failure at the export boundary. Process capture logs use their own owner.
    fs::write(staging, "staging is blocked by a file")?;
    let context = ToolContext::new(&workspace, 5).with_tool_artifact_reader(
        artifact_store.clone(),
        sigil_kernel::session::ToolArtifactReadBudgetV1::default(),
        "capture-storage-failure",
    );
    let mut registry = ToolRegistry::new();
    register_builtin_tools_with_paths(
        &mut registry,
        BuiltinToolPaths::workspace_defaults(&workspace),
    );
    let result = registry
        .execute(context, command("cat source-output.txt"))
        .await?;
    assert!(!result.is_error(), "{result:?}");
    assert_eq!(result.metadata.exit_code, Some(0));
    assert_eq!(result.metadata.total_bytes, Some(OBSERVED as u64));
    assert!(result.content.len() < OBSERVED);
    assert_eq!(
        result.metadata.details["capture"]["code"],
        "capture_storage_failed"
    );
    assert_eq!(
        result.metadata.details["capture"]["command_completed"],
        true
    );
    let (recorded, display) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    let ToolArtifactBindingV1::Unavailable { unavailable } = &recorded.artifact else {
        anyhow::bail!(
            "failed export must never publish the smaller inline preview as full capture"
        );
    };
    assert_eq!(unavailable.observed_bytes, OBSERVED as u64);
    assert_eq!(
        recorded.capture_completeness.storage,
        ToolStorageCompletenessV1::Unavailable
    );
    assert_eq!(recorded.facts.status, "ok");
    assert!(recorded.initial_model_view.artifact_ref.is_none());
    assert_eq!(display.persisted_bytes, 0);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_full_capture_preserves_large_output_and_stream_provenance() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&workspace)?;
    let stdout = format!("{}END-OF-STDOUT\n", "line of command output\n".repeat(8192));
    fs::write(workspace.join("source-output.txt"), &stdout)?;
    let session_store = JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    let artifact_store = ToolArtifactStore::for_session_store(&session_store);
    let context = ToolContext::new(&workspace, 5).with_tool_artifact_reader(
        artifact_store.clone(),
        sigil_kernel::session::ToolArtifactReadBudgetV1::default(),
        "capture-full",
    );
    let mut registry = ToolRegistry::new();
    register_builtin_tools_with_paths(
        &mut registry,
        BuiltinToolPaths::workspace_defaults(&workspace),
    );
    let result = registry
        .execute(
            context,
            command("cat source-output.txt; printf 'STDERR-END\\n' >&2"),
        )
        .await?;
    assert!(!result.is_error(), "{result:?}");
    assert!(result.content.len() < stdout.len());
    assert_eq!(
        result.metadata.details["capture"]["scope"],
        "settled_execution"
    );
    let recorded = result
        .durable_v3_projection()
        .context("owned full capture projection")?;
    let display = recorded.display_view();
    assert_eq!(
        recorded.capture_completeness.storage,
        ToolStorageCompletenessV1::Complete
    );
    let descriptor = recorded.artifact.descriptor().context("full artifact")?;
    let bytes = artifact_store.read_all(descriptor)?;
    assert_eq!(bytes, format!("{stdout}STDERR-END\n").as_bytes());
    assert_eq!(descriptor.observed_bytes, bytes.len() as u64);
    assert_eq!(descriptor.persisted_bytes, bytes.len() as u64);
    assert!(display.has_more);
    let streams = result.metadata.details["capture"]["streams"]
        .as_array()
        .context("stream proofs")?;
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0]["bytes"], stdout.len());
    assert_eq!(streams[1]["bytes"], "STDERR-END\n".len());
    assert!(streams.iter().all(|stream| {
        stream["sha256"]
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    }));
    Ok(())
}
