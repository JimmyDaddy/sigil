use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ChangeSetFileAction, ChangeSetId, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    IntegrationBaseRepresentation, IntegrationContentClass, IsolatedWorkspaceBackend,
    IsolatedWorkspaceCleanupStatus, IsolatedWorkspaceCreated, IsolatedWorkspacePrepared,
    JsonlSessionStore, MutationEventRecorder, Session, VerificationScope, WriteIsolationMode,
    build_workspace_snapshot, stable_workspace_id,
};
use tempfile::TempDir;

use crate::isolated_workspace::{
    FrozenGitWorktreeBaseRestoreRequest, GitWorktreeBaseFreezeRequest,
    GitWorktreeMaterializationRequest, freeze_git_worktree_base, materialize_git_worktree,
    materialize_git_worktree_from_frozen_base, reconcile_isolated_workspace_cleanup,
    restore_frozen_git_worktree_base,
};

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
    let git_dir = dunce::simplified(&git_dir);
    assert_eq!(
        materialized
            .workspace_root()
            .parent()
            .and_then(Path::parent),
        Some(git_dir)
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
    assert_eq!(cleanup.status, IsolatedWorkspaceCleanupStatus::Removed);
    assert!(!cleanup.workspace_root.exists());
    let worktree_list = repository.git(&["worktree", "list", "--porcelain"])?;
    assert_eq!(
        worktree_list
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "{worktree_list}"
    );
    Ok(())
}

#[tokio::test]
async fn git_worktree_extracts_bounded_review_artifact_without_mutating_parent() -> Result<()> {
    let repository = TestRepository::new()?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;
    let materialized = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "task-1-step-artifact".to_owned(),
        base_snapshot_id,
    })
    .await?;
    fs::write(
        materialized.workspace_root().join("base.txt"),
        "isolated edit\n",
    )?;
    fs::write(
        materialized.workspace_root().join("created.txt"),
        "created\n",
    )?;

    let proposal = materialized
        .extract_changeset(
            ChangeSetId::new("changeset-worktree-test")?,
            "Worktree edit",
            "Review isolated files",
        )
        .await?
        .expect("worker edits should produce a proposal");

    assert_eq!(proposal.source_isolation, WriteIsolationMode::Worktree);
    assert!(proposal.child_snapshot_id.is_some());
    assert!(matches!(
        &proposal.integration_facts.base_representation,
        IntegrationBaseRepresentation::CleanCommit { .. }
    ));
    assert!(
        proposal.integration_facts.gaps.is_empty(),
        "{:?}",
        proposal.integration_facts.gaps
    );
    assert!(
        proposal
            .integration_facts
            .paths
            .iter()
            .all(|fact| fact.content_class == IntegrationContentClass::Text)
    );
    assert!(proposal.change_set.files.iter().all(|file| {
        file.before_hash
            .as_deref()
            .is_none_or(|digest| digest.starts_with("sha256:"))
            && file
                .after_hash
                .as_deref()
                .is_none_or(|digest| digest.starts_with("sha256:"))
    }));
    assert_eq!(proposal.change_set.files.len(), 2);
    assert!(
        proposal
            .change_set
            .files
            .iter()
            .any(|file| { file.path == "base.txt" && file.action == ChangeSetFileAction::Update })
    );
    assert!(
        proposal.change_set.files.iter().any(|file| {
            file.path == "created.txt" && file.action == ChangeSetFileAction::Create
        })
    );
    assert!(proposal.artifact.content.contains("isolated edit"));
    assert!(proposal.artifact.content.contains("created.txt"));
    assert_eq!(
        fs::read_to_string(repository.root().join("base.txt"))?,
        "base\n"
    );
    assert!(!repository.root().join("created.txt").exists());
    materialized.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn frozen_dirty_overlay_is_shared_by_value_and_becomes_the_delta_baseline() -> Result<()> {
    let repository = TestRepository::new()?;
    fs::write(repository.root().join("base.txt"), "user edit\n")?;
    fs::write(repository.root().join("notes.txt"), "user notes\n")?;
    fs::write(
        repository.root().join(".git/info/exclude"),
        "ignored-output.txt\n",
    )?;
    fs::write(
        repository.root().join("ignored-output.txt"),
        "must not leak\n",
    )?;
    fs::create_dir_all(repository.root().join(".sigil/cache"))?;
    fs::write(
        repository.root().join(".sigil/cache/runtime-state.txt"),
        "must not leak\n",
    )?;
    fs::create_dir(repository.root().join("target"))?;
    fs::write(
        repository.root().join("target/build-output.txt"),
        "must not leak\n",
    )?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;
    let frozen = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id: base_snapshot_id.clone(),
        operation_id: "overlay-shared-baseline".to_owned(),
        artifact_recorder: repository.mutation_recorder()?,
    })
    .await?;

    assert_eq!(frozen.base_snapshot_id(), base_snapshot_id);
    assert_eq!(frozen.overlay_entry_count(), 2);
    let first = materialize_git_worktree_from_frozen_base(&frozen, "dirty-child-first").await?;
    let second = materialize_git_worktree_from_frozen_base(&frozen, "dirty-child-second").await?;
    assert_ne!(first.workspace_root(), second.workspace_root());
    for child in [&first, &second] {
        assert_eq!(
            fs::read(child.workspace_root().join("base.txt"))?,
            b"user edit\n"
        );
        assert_eq!(
            fs::read(child.workspace_root().join("notes.txt"))?,
            b"user notes\n"
        );
        assert!(!child.workspace_root().join("ignored-output.txt").exists());
        assert!(
            !child
                .workspace_root()
                .join(".sigil/cache/runtime-state.txt")
                .exists()
        );
        assert!(
            !child
                .workspace_root()
                .join("target/build-output.txt")
                .exists()
        );
        assert_eq!(child.overlay_digest(), Some(frozen.overlay_digest()));
        assert_eq!(
            child.overlay_artifact_ref(),
            Some(frozen.overlay_artifact_ref())
        );
    }
    assert!(
        first
            .extract_changeset(
                ChangeSetId::new("changeset-inherited-noop")?,
                "No worker edits",
                "Inherited dirty bytes are not a proposal",
            )
            .await?
            .is_none()
    );

    fs::write(first.workspace_root().join("base.txt"), "agent edit\n")?;
    let proposal = first
        .extract_changeset(
            ChangeSetId::new("changeset-overlay-delta")?,
            "Worker edit",
            "Only the worker delta is reviewable",
        )
        .await?
        .expect("worker edit should produce a proposal");
    let base_file = proposal
        .change_set
        .files
        .iter()
        .find(|file| file.path == "base.txt")
        .expect("base file proposal");
    assert!(matches!(
        &proposal.integration_facts.base_representation,
        IntegrationBaseRepresentation::SnapshotWorkspace {
            overlay_digest,
            ..
        } if overlay_digest == frozen.overlay_digest()
    ));
    assert_eq!(
        base_file.before_hash.as_deref(),
        Some(format!("sha256:{}", hex_sha256(b"user edit\n")).as_str())
    );
    assert!(proposal.artifact.content.contains("-user edit"));
    assert!(!proposal.artifact.content.contains("-base"));
    assert_eq!(
        fs::read(repository.root().join("base.txt"))?,
        b"user edit\n"
    );
    first.cleanup().await?;
    second.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn frozen_dirty_overlay_restores_from_exact_durable_artifact_bindings() -> Result<()> {
    let repository = TestRepository::new()?;
    fs::write(repository.root().join("base.txt"), "durable user edit\n")?;
    fs::write(repository.root().join("notes.txt"), "durable notes\n")?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;
    let recorder = repository.mutation_recorder()?;
    let frozen = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id: base_snapshot_id.clone(),
        operation_id: "overlay-durable-restore".to_owned(),
        artifact_recorder: recorder.clone(),
    })
    .await?;
    let request = FrozenGitWorktreeBaseRestoreRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id,
        base_commit: frozen.base_commit().to_owned(),
        overlay_digest: frozen.overlay_digest().to_owned(),
        overlay_artifact_ref: frozen.overlay_artifact_ref().clone(),
        overlay_content_artifact_refs: frozen.overlay_content_artifact_refs(),
        overlay_entry_count: frozen.overlay_entry_count(),
        artifact_recorder: recorder,
    };
    let restored = restore_frozen_git_worktree_base(request.clone()).await?;
    let materialized =
        materialize_git_worktree_from_frozen_base(&restored, "dirty-restored-child").await?;
    assert_eq!(
        fs::read_to_string(materialized.workspace_root().join("base.txt"))?,
        "durable user edit\n"
    );
    assert_eq!(
        fs::read_to_string(materialized.workspace_root().join("notes.txt"))?,
        "durable notes\n"
    );
    materialized.cleanup().await?;

    let mut substituted = request;
    substituted.overlay_content_artifact_refs.clear();
    let error = restore_frozen_git_worktree_base(substituted)
        .await
        .expect_err("substituted durable artifact inventory must fail closed");
    assert!(format!("{error:#}").contains("content artifact set mismatch"));
    Ok(())
}

#[tokio::test]
async fn frozen_clean_base_restores_empty_overlay_and_remains_a_clean_commit() -> Result<()> {
    let repository = TestRepository::new()?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;
    let recorder = repository.mutation_recorder()?;
    let frozen = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id: base_snapshot_id.clone(),
        operation_id: "clean-overlay-durable-restore".to_owned(),
        artifact_recorder: recorder.clone(),
    })
    .await?;
    assert_eq!(frozen.overlay_entry_count(), 0);

    let restored = restore_frozen_git_worktree_base(FrozenGitWorktreeBaseRestoreRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id,
        base_commit: frozen.base_commit().to_owned(),
        overlay_digest: frozen.overlay_digest().to_owned(),
        overlay_artifact_ref: frozen.overlay_artifact_ref().clone(),
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
        artifact_recorder: recorder,
    })
    .await?;
    let materialized =
        materialize_git_worktree_from_frozen_base(&restored, "clean-restored-child").await?;
    assert_eq!(
        materialized.overlay_digest(),
        Some(frozen.overlay_digest()),
        "the durable empty overlay binding remains available for recovery"
    );
    fs::write(
        materialized.workspace_root().join("created.txt"),
        "created\n",
    )?;
    let proposal = materialized
        .extract_changeset(
            ChangeSetId::new("changeset-clean-restored")?,
            "Clean base edit",
            "Classify an empty overlay as a clean commit",
        )
        .await?
        .expect("worker edit should produce a proposal");
    assert!(matches!(
        proposal.integration_facts.base_representation,
        IntegrationBaseRepresentation::CleanCommit { .. }
    ));
    materialized.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn frozen_overlay_rejects_secret_paths_before_owned_workspace_creation() -> Result<()> {
    let repository = TestRepository::new()?;
    fs::write(
        repository.root().join(".env.local"),
        "TOKEN=not-for-child\n",
    )?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;

    let error = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id,
        operation_id: "overlay-secret-rejection".to_owned(),
        artifact_recorder: repository.mutation_recorder()?,
    })
    .await
    .expect_err("secret-like paths must fail closed");

    assert!(
        format!("{error:#}").contains("secret-like path"),
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
async fn frozen_overlay_rejects_secret_content_at_otherwise_safe_paths() -> Result<()> {
    let repository = TestRepository::new()?;
    fs::write(
        repository.root().join("local-config.txt"),
        "OPENAI_API_KEY=sk-live-value-that-must-not-leak\n",
    )?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;

    let error = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id,
        operation_id: "overlay-secret-content-rejection".to_owned(),
        artifact_recorder: repository.mutation_recorder()?,
    })
    .await
    .expect_err("secret-like content must fail closed");

    assert!(
        format!("{error:#}").contains("secret-like content"),
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

#[cfg(unix)]
#[tokio::test]
async fn frozen_overlay_rejects_unsupported_symlink_entries() -> Result<()> {
    use std::os::unix::fs::symlink;

    let repository = TestRepository::new()?;
    symlink("base.txt", repository.root().join("linked-base.txt"))?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;

    let error = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id,
        operation_id: "overlay-symlink-rejection".to_owned(),
        artifact_recorder: repository.mutation_recorder()?,
    })
    .await
    .expect_err("symlink overlays must fail closed");

    assert!(
        format!("{error:#}").contains("must be a regular file"),
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
async fn frozen_overlay_rejects_parent_drift_before_materialization() -> Result<()> {
    let repository = TestRepository::new()?;
    let frozen = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        base_snapshot_id: task_snapshot_id(repository.root())?,
        operation_id: "overlay-parent-drift".to_owned(),
        artifact_recorder: repository.mutation_recorder()?,
    })
    .await?;
    fs::write(repository.root().join("base.txt"), "drifted\n")?;

    let error = materialize_git_worktree_from_frozen_base(&frozen, "drifted-child-materialization")
        .await
        .expect_err("parent drift must reject the frozen baseline");

    assert!(
        format!("{error:#}").contains("snapshot drifted"),
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
async fn startup_reconciliation_removes_durable_created_worktree_once() -> Result<()> {
    let repository = TestRepository::new()?;
    let base_snapshot_id = task_snapshot_id(repository.root())?;
    let materialized = materialize_git_worktree(GitWorktreeMaterializationRequest {
        parent_workspace_root: repository.root().to_path_buf(),
        isolated_workspace_id: "task-1-step-restart".to_owned(),
        base_snapshot_id: base_snapshot_id.clone(),
    })
    .await?;
    let workspace_root = materialized.workspace_root().to_path_buf();
    std::mem::forget(materialized);
    let parent_workspace_id = stable_workspace_id(repository.root())?;
    let prepared = IsolatedWorkspacePrepared {
        isolated_workspace_id: "task-1-step-restart".to_owned(),
        parent_workspace_id: parent_workspace_id.clone(),
        owner_agent_id: "task:task-1:v1:write".to_owned(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id: base_snapshot_id.clone(),
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: None,
        overlay_digest: None,
        overlay_artifact_ref: None,
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
    };
    let created = IsolatedWorkspaceCreated {
        isolated_workspace_id: prepared.isolated_workspace_id.clone(),
        parent_workspace_id,
        owner_agent_id: prepared.owner_agent_id.clone(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id,
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: None,
        overlay_digest: None,
        overlay_artifact_ref: None,
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
        materialized_snapshot_id: None,
    };
    let session_path = repository
        .root()
        .parent()
        .expect("test repository should have a temporary parent")
        .join("parent.jsonl");
    let store = JsonlSessionStore::new(session_path.clone())?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    session.append_control(sigil_kernel::ControlEntry::IsolatedWorkspacePrepared(
        prepared,
    ))?;
    session.append_control(sigil_kernel::ControlEntry::IsolatedWorkspaceCreated(
        created,
    ))?;
    drop(session);
    let store = JsonlSessionStore::new(session_path)?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

    let first = reconcile_isolated_workspace_cleanup(&mut session, repository.root()).await?;
    assert_eq!(first.inspected, 1);
    assert_eq!(first.removed, 1);
    assert!(!workspace_root.exists());
    assert!(
        session
            .write_isolation_projection()
            .isolated_workspace_cleanup_inventory()
            .is_empty()
    );

    let second = reconcile_isolated_workspace_cleanup(&mut session, repository.root()).await?;
    assert_eq!(second.inspected, 0);
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
        run_git(&root, &["config", "core.autocrlf", "false"])?;
        fs::write(root.join("base.txt"), "base\n")?;
        run_git(&root, &["add", "base.txt"])?;
        run_git(&root, &["commit", "--quiet", "-m", "base"])?;
        Ok(Self { _temp: temp, root })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn head(&self) -> Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    fn mutation_recorder(&self) -> Result<MutationEventRecorder> {
        let state_root = self
            .root
            .parent()
            .expect("test repository should have a temporary parent");
        Ok(MutationEventRecorder::with_artifact_root(
            JsonlSessionStore::new(state_root.join("mutation-session.jsonl"))?,
            state_root.join("mutation-artifacts"),
        ))
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        run_git(&self.root, args)
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
