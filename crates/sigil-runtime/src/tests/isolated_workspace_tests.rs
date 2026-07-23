use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use sigil_kernel::{
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, VerificationScope, build_workspace_snapshot,
    stable_workspace_id,
};
use tempfile::TempDir;

use crate::isolated_workspace::{GitWorktreeMaterializationRequest, materialize_git_worktree};

#[tokio::test]
async fn git_worktree_materialization_is_snapshot_bound_confined_and_consumably_cleaned()
-> Result<()> {
    let repository = TestRepository::new()?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;

    let materialized = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "task-1-step-write-a".to_owned(),
        base_snapshot_id: base_snapshot_id.clone(),
    })
    .await?;

    let git_dir = fs::canonicalize(repository.root().join(".git"))?;
    assert_eq!(
        materialized
            .workspace_root()
            .parent()
            .and_then(Path::parent),
        Some(git_dir.as_path())
    );
    assert_eq!(
        materialized
            .workspace_root()
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str()),
        Some("sigil-isolated-worktrees")
    );
    assert_eq!(materialized.base_snapshot_id(), base_snapshot_id);
    assert_ne!(materialized.child_snapshot_id(), base_snapshot_id);
    assert_eq!(materialized.base_commit(), repository.head()?.as_str());
    assert_eq!(
        fs::read_to_string(materialized.workspace_root().join("base.txt"))?,
        "base\n"
    );

    fs::write(
        materialized.workspace_root().join("base.txt"),
        "isolated edit\n",
    )?;
    assert_eq!(
        fs::read_to_string(repository.root().join("base.txt"))?,
        "base\n"
    );
    let workspace_root = materialized.workspace_root().to_path_buf();
    let cleanup = materialized.cleanup().await?;
    assert_eq!(cleanup.isolated_workspace_id, "task-1-step-write-a");
    assert_eq!(cleanup.workspace_root, workspace_root);
    assert!(cleanup.isolation_root_removed);
    assert!(!cleanup.workspace_root.exists());
    assert!(
        repository
            .git(&["worktree", "list", "--porcelain"])?
            .contains(repository.root_text())
    );
    Ok(())
}

#[tokio::test]
async fn git_worktree_materialization_rejects_dirty_parent_without_creating_owned_root()
-> Result<()> {
    let repository = TestRepository::new()?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;
    fs::write(repository.root().join("base.txt"), "dirty\n")?;

    let error = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "task-1-step-write-a".to_owned(),
        base_snapshot_id,
    })
    .await
    .expect_err("dirty parent must fail before worktree creation");

    assert!(
        format!("{error:#}").contains("requires a clean parent workspace"),
        "{error:#}"
    );
    assert!(
        !repository
            .root()
            .join(".git/sigil-isolated-worktrees")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn git_worktree_materialization_rejects_unsafe_id_and_snapshot_drift() -> Result<()> {
    let repository = TestRepository::new()?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;

    let unsafe_error = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "../escape".to_owned(),
        base_snapshot_id: base_snapshot_id.clone(),
    })
    .await
    .expect_err("unsafe id must fail");
    assert!(
        format!("{unsafe_error:#}").contains("unsafe path characters"),
        "{unsafe_error:#}"
    );

    let drift_error = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "task-1-step-write-a".to_owned(),
        base_snapshot_id: "sha256:jcs-v1:not-the-parent".to_owned(),
    })
    .await
    .expect_err("snapshot drift must fail");
    assert!(
        format!("{drift_error:#}").contains("snapshot drifted"),
        "{drift_error:#}"
    );
    assert!(
        !repository
            .root()
            .join(".git/sigil-isolated-worktrees")
            .exists()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn git_worktree_materialization_rejects_symlinked_owned_root() -> Result<()> {
    use std::os::unix::fs::symlink;

    let repository = TestRepository::new()?;
    let external = tempfile::tempdir()?;
    symlink(
        external.path(),
        repository.root().join(".git/sigil-isolated-worktrees"),
    )?;

    let error = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "task-1-step-write-a".to_owned(),
        base_snapshot_id: task_snapshot_id(repository.root())?,
    })
    .await
    .expect_err("symlinked isolation root must fail");

    assert!(
        format!("{error:#}").contains("not a regular directory"),
        "{error:#}"
    );
    assert_eq!(fs::read_dir(external.path())?.count(), 0);
    Ok(())
}

struct TestRepository {
    _temp: TempDir,
    root: PathBuf,
}

impl TestRepository {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("repo");
        fs::create_dir(&root)?;
        run_git(&root, &["init", "--quiet"])?;
        run_git(&root, &["config", "user.name", "Sigil Tests"])?;
        run_git(
            &root,
            &["config", "user.email", "sigil-tests@example.invalid"],
        )?;
        fs::write(root.join("base.txt"), "base\n")?;
        run_git(&root, &["add", "base.txt"])?;
        run_git(&root, &["commit", "--quiet", "-m", "base"])?;
        Ok(Self { _temp: temp, root })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn root_text(&self) -> &str {
        self.root
            .to_str()
            .expect("temporary repository path should be UTF-8")
    }

    fn head(&self) -> Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        run_git(&self.root, args)
    }
}

fn task_snapshot_id(workspace_root: &Path) -> Result<String> {
    let workspace_id = stable_workspace_id(workspace_root)?;
    build_workspace_snapshot(
        workspace_root,
        workspace_id,
        &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        0,
    )?
    .workspace_snapshot_id
    .ok_or_else(|| anyhow::anyhow!("test workspace snapshot should be complete"))
}

fn run_git(workspace_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
