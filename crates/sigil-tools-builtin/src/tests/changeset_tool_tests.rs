use std::{fs, path::Path};

use anyhow::Result;
use serde_json::{Value, json};
use sigil_kernel::{
    ChangeSet, ChangeSetResult, DurableEventType, JsonlSessionStore, MutationBatchFinished,
    MutationBatchStatus, MutationEventRecorder, Tool, ToolContext, ToolErrorKind, ToolResult,
    ToolResultStatus,
};

use super::{
    ApplyChangeSetTool, ChangeSetArtifactStore, apply_changeset_plan, build_apply_changeset_plan,
    finish_apply_changeset_result,
};

fn open_recorder(state_root: &Path) -> Result<(JsonlSessionStore, MutationEventRecorder)> {
    let store = JsonlSessionStore::new(state_root.join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(
        store.clone(),
        state_root.join("mutation-artifacts"),
    );
    Ok((store, recorder))
}

fn changeset_tool(artifact_root: &Path) -> ApplyChangeSetTool {
    ApplyChangeSetTool {
        artifact_root: artifact_root.to_path_buf(),
        artifact_label_root: "changeset-artifacts".into(),
    }
}

fn two_file_change(id: &str) -> Value {
    json!({
        "id": id,
        "files": [
            { "path": "first.txt", "action": "create", "content": "first\n" },
            { "path": "second.txt", "action": "create", "content": "second\n" }
        ]
    })
}

fn assert_batch(
    store: &JsonlSessionStore,
    expected_status: MutationBatchStatus,
    expected_commits: usize,
) -> Result<()> {
    let events = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .map(|record| record.into_stored_event())
        .collect::<Vec<_>>();
    let batches = events
        .iter()
        .filter(|event| event.event_type == DurableEventType::MutationBatchFinished.as_str())
        .map(|event| serde_json::from_value::<MutationBatchFinished>(event.payload.clone()))
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].status, expected_status);
    assert_eq!(batches[0].committed_operations.len(), expected_commits);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == DurableEventType::MutationCommitted.as_str())
            .count(),
        expected_commits
    );
    Ok(())
}

fn assert_artifacts_unavailable(result: &ToolResult) {
    assert_eq!(
        result.metadata.details["artifacts"]["availability"],
        json!("unavailable")
    );
    assert_eq!(
        result.metadata.details["artifacts"]["reason"],
        json!("diff_artifact_persistence_failed")
    );
    assert!(
        result.metadata.details["artifacts"]
            .get("preview")
            .is_none()
    );
    assert!(
        result.metadata.details["artifacts"]
            .get("reverse")
            .is_none()
    );
}

#[tokio::test]
async fn apply_changeset_artifact_directory_failure_preserves_applied_and_recovery() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let artifact_root = state.path().join("diff-artifacts");
    fs::write(&artifact_root, "not a directory\n")?;
    let (store, recorder) = open_recorder(state.path())?;

    let result = changeset_tool(&artifact_root)
        .execute(
            ToolContext::new(workspace.path().to_path_buf(), 5).with_mutation_recorder(recorder),
            "apply-directory-failure".to_owned(),
            two_file_change("directory-failure"),
        )
        .await?;

    assert!(!result.is_error());
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("applied")
    );
    assert_eq!(result.metadata.changed_files, ["first.txt", "second.txt"]);
    assert_eq!(
        fs::read_to_string(workspace.path().join("first.txt"))?,
        "first\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("second.txt"))?,
        "second\n"
    );
    assert_artifacts_unavailable(&result);
    let model_result: Value = serde_json::from_str(&result.to_model_content())?;
    assert_eq!(model_result["status"], json!("ok"));
    assert!(result.content.contains("diff artifacts unavailable"));
    assert!(
        !result
            .to_model_content()
            .contains(&state.path().display().to_string())
    );
    assert_batch(&store, MutationBatchStatus::Applied, 2)?;

    let log_before = fs::read(store.path())?;
    fs::write(workspace.path().join("first.txt"), "subsequent user edit\n")?;
    drop(store);
    let (reopened_store, reopened_recorder) = open_recorder(state.path())?;
    assert!(
        reopened_recorder
            .reconcile_prepared_mutations(workspace.path())?
            .is_empty()
    );
    assert!(
        reopened_recorder
            .reconcile_prepared_mutations(workspace.path())?
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("first.txt"))?,
        "subsequent user edit\n"
    );
    assert_eq!(fs::read(reopened_store.path())?, log_before);
    assert_batch(&reopened_store, MutationBatchStatus::Applied, 2)?;
    Ok(())
}

#[tokio::test]
async fn apply_changeset_reverse_artifact_write_failure_preserves_applied() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let artifact_root = state.path().join("diff-artifacts");
    fs::create_dir_all(artifact_root.join("reverse-failure/reverse.diff"))?;
    let (store, recorder) = open_recorder(state.path())?;

    let result = changeset_tool(&artifact_root)
        .execute(
            ToolContext::new(workspace.path().to_path_buf(), 5).with_mutation_recorder(recorder),
            "apply-reverse-failure".to_owned(),
            two_file_change("reverse-failure"),
        )
        .await?;

    assert!(!result.is_error());
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("applied")
    );
    assert_artifacts_unavailable(&result);
    let preview = fs::read_to_string(artifact_root.join("reverse-failure/preview.diff"))?;
    assert!(preview.contains("+first"));
    assert!(preview.contains("+second"));
    assert!(artifact_root.join("reverse-failure/reverse.diff").is_dir());
    assert_batch(&store, MutationBatchStatus::Applied, 2)?;
    Ok(())
}

#[tokio::test]
async fn apply_changeset_artifact_constructor_failure_preserves_applied() -> Result<()> {
    let workspace_parent = tempfile::tempdir()?;
    let workspace = workspace_parent.path().join("workspace");
    fs::create_dir(&workspace)?;
    let state = tempfile::tempdir()?;
    let artifact_root = state.path().join("diff-artifacts");
    let (store, recorder) = open_recorder(state.path())?;
    let result = changeset_tool(&artifact_root)
        .execute(
            ToolContext::new(workspace.clone(), 5).with_mutation_recorder(recorder),
            "apply-constructor-failure".to_owned(),
            two_file_change("constructor-failure"),
        )
        .await?;
    assert!(!result.is_error());
    assert_eq!(
        result.metadata.details["artifacts"]["availability"],
        json!("available")
    );
    assert_batch(&store, MutationBatchStatus::Applied, 2)?;

    // Exercise the post-commit result boundary with real durable file outcomes and a real
    // constructor I/O failure. The full apply/write failure paths are exercised above.
    let change_set: ChangeSet =
        serde_json::from_value(result.metadata.details["change_set"].clone())?;
    let apply_result: ChangeSetResult =
        serde_json::from_value(result.metadata.details["apply_result"].clone())?;
    let moved_workspace = workspace_parent.path().join("moved-workspace");
    fs::rename(&workspace, &moved_workspace)?;
    let artifact_result = ChangeSetArtifactStore::new_with_artifact_root(
        &workspace,
        &artifact_root,
        "changeset-artifacts",
    )
    .and_then(|store| store.write_diff_artifacts(change_set.id.clone(), "+first\n", "-first\n"))
    .map(Some);
    assert!(artifact_result.is_err());
    let result = finish_apply_changeset_result(
        result.call_id,
        change_set,
        apply_result,
        artifact_result,
        result.metadata.changed_files,
        ToolErrorKind::Io,
    );

    assert!(!result.is_error());
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("applied")
    );
    assert_artifacts_unavailable(&result);
    assert_eq!(
        fs::read_to_string(moved_workspace.join("first.txt"))?,
        "first\n"
    );
    assert_eq!(
        fs::read_to_string(moved_workspace.join("second.txt"))?,
        "second\n"
    );
    assert_batch(&store, MutationBatchStatus::Applied, 2)?;
    Ok(())
}

#[tokio::test]
async fn apply_changeset_real_partial_failure_survives_artifact_failure() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let artifact_root = state.path().join("diff-artifacts");
    fs::write(&artifact_root, "not a directory\n")?;
    let (store, recorder) = open_recorder(state.path())?;
    let result = changeset_tool(&artifact_root)
        .execute(
            ToolContext::new(workspace.path().to_path_buf(), 5).with_mutation_recorder(recorder),
            "apply-partial-failure".to_owned(),
            json!({
                "id": "partial-failure",
                "files": [
                    { "path": "blocked", "action": "create", "content": "file\n" },
                    { "path": "blocked/child.txt", "action": "create", "content": "child\n" },
                    { "path": "after.txt", "action": "create", "content": "after\n" }
                ]
            }),
        )
        .await?;

    assert!(result.is_error());
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("partially_applied")
    );
    let file_results = &result.metadata.details["apply_result"]["file_results"];
    assert_eq!(file_results[0]["status"], json!("applied"));
    assert_eq!(file_results[1]["status"], json!("failed"));
    assert_eq!(file_results[2]["status"], json!("skipped"));
    assert_eq!(result.metadata.changed_files, ["blocked"]);
    assert_eq!(
        fs::read_to_string(workspace.path().join("blocked"))?,
        "file\n"
    );
    assert!(!workspace.path().join("blocked/child.txt").exists());
    assert!(!workspace.path().join("after.txt").exists());
    assert_artifacts_unavailable(&result);
    let ToolResultStatus::Error(error) = &result.status else {
        panic!("a real partial file failure must remain an error")
    };
    assert_eq!(error.details, result.metadata.details);
    assert!(!error.retryable);
    assert_batch(&store, MutationBatchStatus::PartiallyApplied, 1)?;
    Ok(())
}

#[test]
fn apply_changeset_artifact_failure_preserves_workspace_conflict() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let artifact_root = state.path().join("diff-artifacts");
    fs::write(&artifact_root, "not a directory\n")?;
    fs::write(workspace.path().join("conflict.txt"), "before\n")?;
    let (store, recorder) = open_recorder(state.path())?;
    let plan = build_apply_changeset_plan(
        workspace.path(),
        &json!({
            "id": "workspace-conflict",
            "files": [
                { "path": "first.txt", "action": "create", "content": "first\n" },
                { "path": "conflict.txt", "action": "update", "content": "planned\n" }
            ]
        }),
    )??;
    fs::write(workspace.path().join("conflict.txt"), "external change\n")?;

    let result = apply_changeset_plan(
        workspace.path(),
        &artifact_root,
        "changeset-artifacts".into(),
        "apply-workspace-conflict".to_owned(),
        Some(recorder),
        plan,
    )?;

    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("partially_applied")
    );
    assert_artifacts_unavailable(&result);
    let ToolResultStatus::Error(error) = &result.status else {
        panic!("workspace conflict must remain an error")
    };
    assert_eq!(error.kind, ToolErrorKind::WorkspaceConflict);
    assert_eq!(error.details, result.metadata.details);
    assert!(!error.retryable);
    assert_eq!(
        fs::read_to_string(workspace.path().join("first.txt"))?,
        "first\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("conflict.txt"))?,
        "external change\n"
    );
    assert_batch(&store, MutationBatchStatus::PartiallyApplied, 1)?;
    Ok(())
}
