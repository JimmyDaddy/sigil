//! Physical isolated-workspace materialization owned by the runtime.
//!
//! This module deliberately does not append session events or start child agents. Callers must
//! persist ownership before exposing a materialized workspace to a child and must record cleanup
//! outcomes through the durable write-isolation protocol.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use sigil_kernel::{
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, VerificationScope, WorkspaceSnapshotBuild,
    build_workspace_snapshot, stable_workspace_id,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

const ISOLATED_WORKTREE_ROOT: &str = "sigil-isolated-worktrees";
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_OUTPUT_LIMIT: usize = 64 * 1024;
const GIT_ERROR_OUTPUT_LIMIT: usize = 8 * 1024;
const MAX_ISOLATED_WORKSPACE_ID_BYTES: usize = 128;

/// Request for one detached Git worktree bound to an existing parent snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeMaterializationRequest {
    pub parent_workspace_root: PathBuf,
    pub isolated_workspace_id: String,
    pub base_snapshot_id: String,
}

/// Owned receipt for one materialized detached Git worktree.
///
/// The receipt is intentionally not `Clone`: cleanup consumes it so one runtime owner remains
/// responsible for the physical workspace.
#[derive(Debug)]
pub struct MaterializedGitWorktree {
    parent_workspace_root: PathBuf,
    workspace_root: PathBuf,
    isolation_root: PathBuf,
    isolated_workspace_id: String,
    base_snapshot_id: String,
    child_snapshot_id: String,
    base_commit: String,
}

impl MaterializedGitWorktree {
    #[must_use]
    pub fn parent_workspace_root(&self) -> &Path {
        &self.parent_workspace_root
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn isolated_workspace_id(&self) -> &str {
        &self.isolated_workspace_id
    }

    #[must_use]
    pub fn base_snapshot_id(&self) -> &str {
        &self.base_snapshot_id
    }

    #[must_use]
    pub fn child_snapshot_id(&self) -> &str {
        &self.child_snapshot_id
    }

    #[must_use]
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    /// Removes this exact worktree through Git and returns a bounded cleanup receipt.
    ///
    /// # Errors
    ///
    /// Returns an error if the receipt no longer resolves inside its frozen isolation root or Git
    /// cannot remove the worktree. The function never recursively deletes an arbitrary path.
    pub async fn cleanup(self) -> Result<GitWorktreeCleanupReceipt> {
        ensure_confined_destination(
            &self.isolation_root,
            &self.workspace_root,
            &self.isolated_workspace_id,
        )?;
        run_git(
            &self.parent_workspace_root,
            [
                OsString::from("worktree"),
                OsString::from("remove"),
                OsString::from("--force"),
                self.workspace_root.as_os_str().to_owned(),
            ],
        )
        .await
        .with_context(|| {
            format!(
                "failed to remove isolated Git worktree {}",
                self.isolated_workspace_id
            )
        })?;

        let isolation_root_removed = match tokio::fs::remove_dir(&self.isolation_root).await {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to remove empty isolated worktree root {}",
                        self.isolation_root.display()
                    )
                });
            }
        };
        Ok(GitWorktreeCleanupReceipt {
            isolated_workspace_id: self.isolated_workspace_id,
            workspace_root: self.workspace_root,
            isolation_root_removed,
        })
    }
}

/// Physical cleanup result for one materialized Git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeCleanupReceipt {
    pub isolated_workspace_id: String,
    pub workspace_root: PathBuf,
    pub isolation_root_removed: bool,
}

/// Materializes a detached Git worktree only when the parent is clean and still matches the
/// requested workspace snapshot.
///
/// The destination is derived from the canonical Git common directory plus a validated opaque
/// workspace id. It never accepts a caller-provided destination path.
///
/// # Errors
///
/// Returns an error before worktree creation when the parent is not a repository root, is dirty,
/// contains submodules, has drifted from `base_snapshot_id`, or the isolated id is unsafe. A
/// post-checkout snapshot mismatch triggers a best-effort Git-owned rollback and still fails
/// closed.
pub async fn materialize_git_worktree(
    request: GitWorktreeMaterializationRequest,
) -> Result<MaterializedGitWorktree> {
    validate_isolated_workspace_id(&request.isolated_workspace_id)?;
    if request.base_snapshot_id.trim().is_empty() {
        bail!("isolated Git worktree base snapshot id must not be empty");
    }
    let parent_workspace_root = canonical_directory(&request.parent_workspace_root)
        .await
        .context("failed to resolve parent workspace root for isolated Git worktree")?;
    validate_git_repository_root(&parent_workspace_root).await?;
    validate_clean_parent(&parent_workspace_root).await?;
    let parent_snapshot =
        validate_parent_snapshot(&parent_workspace_root, &request.base_snapshot_id).await?;

    let base_commit = git_text(
        &parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("HEAD^{commit}"),
        ],
    )
    .await
    .context("failed to resolve isolated Git worktree base commit")?;
    if !(40..=64).contains(&base_commit.len())
        || !base_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("Git returned an invalid base commit for isolated worktree");
    }

    let git_common_dir = resolve_git_common_dir(&parent_workspace_root).await?;
    let isolation_root = prepare_isolation_root(&git_common_dir).await?;
    let workspace_root = isolation_root.join(&request.isolated_workspace_id);
    ensure_confined_destination(
        &isolation_root,
        &workspace_root,
        &request.isolated_workspace_id,
    )?;
    if tokio::fs::symlink_metadata(&workspace_root).await.is_ok() {
        bail!(
            "isolated Git worktree destination already exists for {}",
            request.isolated_workspace_id
        );
    }

    let add_result = run_git(
        &parent_workspace_root,
        [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            workspace_root.as_os_str().to_owned(),
            OsString::from(&base_commit),
        ],
    )
    .await;
    if let Err(error) = add_result {
        let cleanup_error =
            cleanup_failed_materialization(&parent_workspace_root, &workspace_root).await;
        return Err(with_cleanup_context(error, cleanup_error));
    }

    let canonical_workspace_root = match canonical_directory(&workspace_root).await {
        Ok(path) => path,
        Err(error) => {
            let cleanup_error =
                cleanup_failed_materialization(&parent_workspace_root, &workspace_root).await;
            return Err(with_cleanup_context(error, cleanup_error));
        }
    };
    if let Err(error) = ensure_confined_destination(
        &isolation_root,
        &canonical_workspace_root,
        &request.isolated_workspace_id,
    ) {
        let cleanup_error =
            cleanup_failed_materialization(&parent_workspace_root, &canonical_workspace_root).await;
        return Err(with_cleanup_context(error, cleanup_error));
    }
    let child_snapshot_id =
        match validate_materialized_snapshot(&canonical_workspace_root, &parent_snapshot).await {
            Ok(snapshot_id) => snapshot_id,
            Err(error) => {
                let cleanup_error = cleanup_failed_materialization(
                    &parent_workspace_root,
                    &canonical_workspace_root,
                )
                .await;
                return Err(with_cleanup_context(error, cleanup_error));
            }
        };

    Ok(MaterializedGitWorktree {
        parent_workspace_root,
        workspace_root: canonical_workspace_root,
        isolation_root,
        isolated_workspace_id: request.isolated_workspace_id,
        base_snapshot_id: request.base_snapshot_id,
        child_snapshot_id,
        base_commit,
    })
}

async fn validate_git_repository_root(parent_workspace_root: &Path) -> Result<()> {
    let top_level = git_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--show-toplevel"),
        ],
    )
    .await
    .context("isolated worktree requires a non-bare Git working tree")?;
    let top_level = canonical_directory(Path::new(&top_level))
        .await
        .context("failed to canonicalize Git repository root")?;
    if top_level != parent_workspace_root {
        bail!(
            "isolated worktree requires workspace root {} to equal Git repository root {}",
            parent_workspace_root.display(),
            top_level.display()
        );
    }
    Ok(())
}

async fn validate_clean_parent(parent_workspace_root: &Path) -> Result<()> {
    let status = git_bytes(
        parent_workspace_root,
        [
            OsString::from("status"),
            OsString::from("--porcelain=v1"),
            OsString::from("-z"),
            OsString::from("--untracked-files=all"),
        ],
    )
    .await
    .context("failed to inspect parent Git worktree status")?;
    if !status.is_empty() {
        bail!("isolated Git worktree requires a clean parent workspace");
    }
    let submodules = git_bytes(
        parent_workspace_root,
        [
            OsString::from("submodule"),
            OsString::from("status"),
            OsString::from("--recursive"),
        ],
    )
    .await
    .context("failed to inspect parent Git submodules")?;
    if !submodules.is_empty() {
        bail!("isolated Git worktree does not yet support repositories with submodules");
    }
    Ok(())
}

async fn validate_parent_snapshot(
    parent_workspace_root: &Path,
    expected: &str,
) -> Result<WorkspaceSnapshotBuild> {
    let observed = task_workspace_snapshot(parent_workspace_root.to_path_buf()).await?;
    if observed.workspace_snapshot_id.as_deref() != Some(expected) {
        bail!("parent workspace snapshot drifted before isolated worktree materialization");
    }
    Ok(observed)
}

async fn validate_materialized_snapshot(
    workspace_root: &Path,
    parent_snapshot: &WorkspaceSnapshotBuild,
) -> Result<String> {
    let observed = task_workspace_snapshot(workspace_root.to_path_buf()).await?;
    if observed.manifest.scope_hash != parent_snapshot.manifest.scope_hash
        || observed.manifest.entries != parent_snapshot.manifest.entries
    {
        bail!("materialized Git worktree does not match the requested parent snapshot");
    }
    observed
        .workspace_snapshot_id
        .ok_or_else(|| anyhow!("materialized Git worktree snapshot is incomplete"))
}

async fn task_workspace_snapshot(workspace_root: PathBuf) -> Result<WorkspaceSnapshotBuild> {
    tokio::task::spawn_blocking(move || {
        let workspace_id = stable_workspace_id(&workspace_root)?;
        let snapshot = build_workspace_snapshot(
            &workspace_root,
            workspace_id,
            &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
            0,
        )?;
        if snapshot.workspace_snapshot_id.is_none() {
            bail!("workspace snapshot is incomplete");
        }
        Ok(snapshot)
    })
    .await
    .context("isolated worktree snapshot task failed")?
}

async fn resolve_git_common_dir(parent_workspace_root: &Path) -> Result<PathBuf> {
    let common_dir = git_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--git-common-dir"),
        ],
    )
    .await?;
    let common_dir = PathBuf::from(common_dir);
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        parent_workspace_root.join(common_dir)
    };
    canonical_directory(&common_dir)
        .await
        .context("failed to canonicalize Git common directory")
}

async fn prepare_isolation_root(git_common_dir: &Path) -> Result<PathBuf> {
    let isolation_root = git_common_dir.join(ISOLATED_WORKTREE_ROOT);
    match tokio::fs::symlink_metadata(&isolation_root).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "isolated Git worktree root is not a regular directory: {}",
                    isolation_root.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(&isolation_root)
                .await
                .with_context(|| {
                    format!(
                        "failed to create isolated Git worktree root {}",
                        isolation_root.display()
                    )
                })?;
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect isolated Git worktree root {}",
                    isolation_root.display()
                )
            });
        }
    }
    let canonical = canonical_directory(&isolation_root).await?;
    if canonical.parent() != Some(git_common_dir) {
        bail!("isolated Git worktree root escaped the Git common directory");
    }
    Ok(canonical)
}

fn validate_isolated_workspace_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_ISOLATED_WORKSPACE_ID_BYTES {
        bail!(
            "isolated workspace id must contain between 1 and {} bytes",
            MAX_ISOLATED_WORKSPACE_ID_BYTES
        );
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("isolated workspace id contains unsafe path characters");
    }
    Ok(())
}

fn ensure_confined_destination(
    isolation_root: &Path,
    destination: &Path,
    isolated_workspace_id: &str,
) -> Result<()> {
    if destination.parent() != Some(isolation_root)
        || destination.file_name().and_then(|name| name.to_str()) != Some(isolated_workspace_id)
    {
        bail!("isolated Git worktree destination escaped its owned root");
    }
    Ok(())
}

async fn cleanup_failed_materialization(
    parent_workspace_root: &Path,
    workspace_root: &Path,
) -> Result<()> {
    run_git(
        parent_workspace_root,
        [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            workspace_root.as_os_str().to_owned(),
        ],
    )
    .await
    .map(|_| ())
}

fn with_cleanup_context(error: anyhow::Error, cleanup_error: Result<()>) -> anyhow::Error {
    match cleanup_error {
        Ok(()) => error,
        Err(cleanup_error) => error.context(format!(
            "isolated Git worktree rollback was incomplete: {cleanup_error:#}"
        )),
    }
}

async fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .with_context(|| format!("failed to inspect directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("path is not a regular directory: {}", path.display());
    }
    tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("failed to canonicalize directory {}", path.display()))
}

async fn git_text(current_dir: &Path, args: impl IntoIterator<Item = OsString>) -> Result<String> {
    let output = git_bytes(current_dir, args).await?;
    let text = String::from_utf8(output).context("Git output path was not valid UTF-8")?;
    let text = text.trim();
    if text.is_empty() {
        bail!("Git command returned an empty result");
    }
    Ok(text.to_owned())
}

async fn git_bytes(
    current_dir: &Path,
    args: impl IntoIterator<Item = OsString>,
) -> Result<Vec<u8>> {
    run_git(current_dir, args).await
}

async fn run_git(current_dir: &Path, args: impl IntoIterator<Item = OsString>) -> Result<Vec<u8>> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(current_dir)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start Git command {}", display_git_args(&args)))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Git command stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Git command stderr pipe is unavailable"))?;
    let output = tokio::time::timeout(GIT_COMMAND_TIMEOUT, async move {
        let (stdout, stderr, status) = tokio::try_join!(
            read_bounded_output(stdout, GIT_OUTPUT_LIMIT),
            read_bounded_output(stderr, GIT_ERROR_OUTPUT_LIMIT),
            child.wait()
        )?;
        Ok::<_, std::io::Error>(BoundedGitOutput {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .map_err(|_| {
        anyhow!(
            "Git command timed out after {} seconds",
            GIT_COMMAND_TIMEOUT.as_secs()
        )
    })?
    .with_context(|| format!("failed to collect Git command {}", display_git_args(&args)))?;
    if output.stdout.truncated {
        bail!(
            "Git command {} exceeded the {} byte stdout limit",
            display_git_args(&args),
            GIT_OUTPUT_LIMIT
        );
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        let suffix = if output.stderr.truncated {
            " [truncated]"
        } else {
            ""
        };
        bail!(
            "Git command {} failed with status {}: {}{}",
            display_git_args(&args),
            output.status,
            stderr.trim(),
            suffix
        );
    }
    Ok(output.stdout.bytes)
}

fn display_git_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

struct BoundedGitOutput {
    status: ExitStatus,
    stdout: BoundedBytes,
    stderr: BoundedBytes,
}

struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded_output(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<BoundedBytes> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let keep = remaining.min(read);
        bytes.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
    }
    Ok(BoundedBytes { bytes, truncated })
}
