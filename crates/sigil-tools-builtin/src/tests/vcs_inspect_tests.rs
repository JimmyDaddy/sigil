use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sigil_kernel::{
    Tool, ToolAccess, ToolCapability, ToolConcurrencyClass, ToolContext, ToolErrorKind,
    ToolMutationTracking, ToolOperation, ToolPermissionEffect, ToolRegistry, ToolResultStatus,
    ToolSubjectScope,
};

use super::{VcsInspectTool, VcsInspectionOperation, inspection_payload};
use crate::register_builtin_tools;

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("failed to run git test fixture command")?;
    if !output.status.success() {
        bail!(
            "git fixture command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("failed to run git test fixture query")?;
    if !output.status.success() {
        bail!(
            "git fixture query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn committed_repository() -> Result<tempfile::TempDir> {
    let temp = tempfile::tempdir()?;
    git(temp.path(), &["init", "-q"])?;
    git(
        temp.path(),
        &["config", "user.email", "sigil@example.invalid"],
    )?;
    git(temp.path(), &["config", "user.name", "Sigil Tests"])?;
    fs::write(temp.path().join("tracked.txt"), "one\ntwo\n")?;
    git(temp.path(), &["add", "tracked.txt"])?;
    git(temp.path(), &["commit", "-q", "-m", "initial"])?;
    Ok(temp)
}

async fn execute(root: &Path, operation: &str, limit: usize) -> Result<sigil_kernel::ToolResult> {
    VcsInspectTool
        .execute(
            ToolContext::new(root, 5),
            format!("call-{operation}"),
            json!({ "operation": operation, "limit": limit }),
        )
        .await
}

fn content(result: &sigil_kernel::ToolResult) -> Result<Value> {
    Ok(serde_json::from_str(&result.content)?)
}

#[test]
fn vcs_inspect_contract_is_fixed_read_only_and_registered() -> Result<()> {
    let tool = VcsInspectTool;
    let spec = tool.spec();
    assert_eq!(spec.name, "vcs_inspect");
    assert_eq!(spec.access, ToolAccess::Read);
    assert_eq!(spec.input_schema["additionalProperties"], false);
    assert!(spec.input_schema["properties"].get("command").is_none());
    assert!(spec.input_schema["properties"].get("path").is_none());
    assert_eq!(tool.mutation_tracking(), ToolMutationTracking::None);
    assert_eq!(
        tool.concurrency_class(),
        ToolConcurrencyClass::ParallelReadOnly
    );
    assert_eq!(
        tool.capabilities(),
        [ToolCapability::WorkspaceRead, ToolCapability::VcsRead]
            .into_iter()
            .collect()
    );

    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path(), 5);
    let plan = tool.permission_plan(&ctx, &json!({ "operation": "status" }))?;
    assert_eq!(plan.access, ToolAccess::Read);
    assert_eq!(plan.operation, ToolOperation::Search);
    assert_eq!(
        plan.effects,
        [ToolPermissionEffect::FileRead].into_iter().collect()
    );
    assert!(plan.analysis.is_complete());
    assert_eq!(plan.subjects.len(), 1);
    assert_eq!(plan.subjects[0].scope, ToolSubjectScope::Workspace);

    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    let contract = registry
        .contracts()
        .into_iter()
        .find(|contract| contract.spec.name == "vcs_inspect")
        .context("vcs_inspect should be registered")?;
    assert_eq!(contract.spec.access, ToolAccess::Read);
    assert_eq!(contract.mutation_tracking, ToolMutationTracking::None);
    assert_eq!(
        contract.concurrency_class,
        ToolConcurrencyClass::ParallelReadOnly
    );
    assert_eq!(
        contract.capabilities,
        [ToolCapability::WorkspaceRead, ToolCapability::VcsRead]
            .into_iter()
            .collect()
    );
    Ok(())
}

#[tokio::test]
async fn vcs_inspect_reports_status_names_and_stats_as_bounded_json() -> Result<()> {
    let repo = committed_repository()?;
    fs::write(repo.path().join("tracked.txt"), "one\nchanged\nthree\n")?;
    fs::write(repo.path().join("untracked.txt"), "new\n")?;
    let index_before_reads = fs::read(repo.path().join(".git/index"))?;

    let status = execute(repo.path(), "status", 10).await?;
    assert!(
        matches!(status.status, ToolResultStatus::Ok),
        "vcs status failed: {status:?}"
    );
    let status_content = content(&status)?;
    assert_eq!(status_content["operation"], "status");
    let entries = status_content["entries"]
        .as_array()
        .context("status entries should be an array")?;
    assert!(
        entries
            .iter()
            .any(|entry| entry["status"] == " M" && entry["path"] == "tracked.txt")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["status"] == "??" && entry["path"] == "untracked.txt")
    );

    let names = execute(repo.path(), "diff_names", 10).await?;
    let names_content = content(&names)?;
    assert_eq!(names_content["entries"][0]["path"], "tracked.txt");

    let stat = execute(repo.path(), "diff_stat", 10).await?;
    let stat_content = content(&stat)?;
    assert_eq!(stat_content["entries"][0]["path"], "tracked.txt");
    assert_eq!(stat_content["entries"][0]["added"], 2);
    assert_eq!(stat_content["entries"][0]["deleted"], 1);
    assert_eq!(
        fs::read(repo.path().join(".git/index"))?,
        index_before_reads,
        "read-only inspections must not refresh or rewrite the Git index"
    );

    git(repo.path(), &["add", "tracked.txt"])?;
    let index_before_staged = fs::read(repo.path().join(".git/index"))?;
    let staged = execute(repo.path(), "staged_stat", 10).await?;
    let staged_content = content(&staged)?;
    assert_eq!(staged_content["entries"][0]["path"], "tracked.txt");
    assert_eq!(staged.metadata.details["operation"], "staged_stat");
    assert_eq!(
        fs::read(repo.path().join(".git/index"))?,
        index_before_staged,
        "staged inspection must not rewrite the Git index"
    );
    Ok(())
}

#[tokio::test]
async fn vcs_inspect_reports_unmerged_paths_without_arbitrary_git_arguments() -> Result<()> {
    let repo = committed_repository()?;
    let default_branch = git_stdout(repo.path(), &["branch", "--show-current"])?;
    git(repo.path(), &["checkout", "-q", "-b", "side"])?;
    fs::write(repo.path().join("tracked.txt"), "side\n")?;
    git(repo.path(), &["add", "tracked.txt"])?;
    git(repo.path(), &["commit", "-q", "-m", "side"])?;
    git(repo.path(), &["checkout", "-q", &default_branch])?;
    fs::write(repo.path().join("tracked.txt"), "main\n")?;
    git(repo.path(), &["add", "tracked.txt"])?;
    git(repo.path(), &["commit", "-q", "-m", "main"])?;
    let merge = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["merge", "--no-edit", "side"])
        .output()?;
    assert!(!merge.status.success(), "fixture merge should conflict");

    let result = execute(repo.path(), "unmerged", 10).await?;
    assert!(
        matches!(result.status, ToolResultStatus::Ok),
        "vcs unmerged failed: {result:?}"
    );
    let value = content(&result)?;
    assert_eq!(value["entries"][0]["path"], "tracked.txt");
    Ok(())
}

#[tokio::test]
async fn vcs_inspect_rejects_git_metadata_outside_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let external = temp.path().join("external-git");
    fs::create_dir(&workspace)?;
    fs::create_dir(&external)?;
    fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", external.display()),
    )?;

    let result = execute(&workspace, "status", 10).await?;
    let ToolResultStatus::Error(error) = result.status else {
        bail!("external Git metadata should be rejected");
    };
    assert_eq!(error.kind, ToolErrorKind::PathOutsideWorkspace);
    assert!(!error.message.contains(&temp.path().display().to_string()));
    Ok(())
}

#[test]
fn vcs_inspect_payload_limit_is_deterministic() -> Result<()> {
    let payload = inspection_payload(
        VcsInspectionOperation::DiffNames,
        "a.rs\nb.rs\nc.rs\n",
        2,
        false,
    )?;
    assert_eq!(payload.returned_entries, 2);
    assert_eq!(payload.total_entries, Some(3));
    assert!(payload.truncated);
    assert_eq!(payload.value["entries"].as_array().map(Vec::len), Some(2));
    Ok(())
}
