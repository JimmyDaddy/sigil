use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, ensure};
use sigil_kernel::{ChangeSet, ChangeSetFileAction, write_isolation::MergeReviewSourceBinding};
use sigil_resource_authority::file_access::{
    HostSourceEntryObservation, HostSourceObservationRoot,
};

const MAX_SOURCE_ENTRIES: usize = 100_000;
const MAX_SOURCE_FILE_BYTES: u64 = sigil_kernel::verification::MAX_WORKSPACE_SNAPSHOT_FILE_BYTES;
const MAX_SOURCE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Process-local source facts captured before a changeset-only child starts. Unknown content
/// remains unknown; neither another file's failure nor a later workspace observation rewrites it.
#[derive(Debug, Clone)]
pub(super) struct ChangesetSourceObservation {
    root: PathBuf,
    source_root: Arc<HostSourceObservationRoot>,
    files: BTreeMap<PathBuf, Option<String>>,
    complete_directories: BTreeSet<PathBuf>,
}

impl ChangesetSourceObservation {
    pub(super) async fn capture(root: &Path) -> Result<Self> {
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || Self::capture_blocking(&root))
            .await
            .context("changeset source observation task failed")?
    }

    fn capture_blocking(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root).context("changeset source workspace is unavailable")?;
        let source_root = Arc::new(HostSourceObservationRoot::open(&root)?);
        let mut excludes = ignore::gitignore::GitignoreBuilder::new(&root);
        for pattern in sigil_kernel::VerificationScope::all_tracked(
            sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        )
        .exclude
        {
            // Reuse the snapshot's rooted exclusions; excluded subtrees remain unknown.
            excludes.add_line(None, &format!("/{pattern}"))?;
            if let Some(directory) = pattern.strip_suffix("/**") {
                excludes.add_line(None, &format!("/{directory}/"))?;
            }
        }
        let excludes = excludes.build()?;
        let mut observation = Self {
            root: root.clone(),
            source_root: source_root.clone(),
            files: BTreeMap::new(),
            complete_directories: BTreeSet::new(),
        };
        let mut pending = vec![PathBuf::new()];
        let mut remaining_entries = MAX_SOURCE_ENTRIES;
        let mut remaining_bytes = MAX_SOURCE_TOTAL_BYTES;
        while let Some(relative_dir) = pending.pop() {
            let Ok((names, complete)) =
                source_root.directory_names(&relative_dir, remaining_entries)
            else {
                continue;
            };
            remaining_entries = remaining_entries.saturating_sub(names.len());
            for name in names {
                let relative = relative_dir.join(name);
                observation.files.insert(relative.clone(), None);
                if excludes
                    .matched_path_or_any_parents(&relative, true)
                    .is_ignore()
                {
                    continue;
                }
                match source_root.entry(&relative, MAX_SOURCE_FILE_BYTES.min(remaining_bytes)) {
                    Ok(HostSourceEntryObservation::Directory) => {
                        pending.push(relative);
                    }
                    Ok(HostSourceEntryObservation::File {
                        content_digest,
                        observed_bytes,
                    }) => {
                        remaining_bytes = remaining_bytes.saturating_sub(observed_bytes);
                        if let Some(digest) = content_digest {
                            observation
                                .files
                                .insert(relative, Some(format!("sha256:{}", digest.to_hex())));
                        }
                    }
                    Err(_) => {}
                }
            }
            if complete {
                observation.complete_directories.insert(relative_dir);
            }
            if remaining_entries == 0 {
                break;
            }
        }
        source_root.validate_identity()?;
        Ok(observation)
    }

    fn source_hash(&self, path: &Path) -> Result<Option<String>> {
        if let Some(hash) = self.files.get(path) {
            return hash.clone().map(Some).ok_or_else(|| {
                anyhow!(
                    "changeset target source was not observed: {}",
                    path.display()
                )
            });
        }
        let mut ancestor = path.to_path_buf();
        while let Some(parent) = ancestor.parent() {
            if self.complete_directories.contains(parent) && !self.files.contains_key(&ancestor) {
                return Ok(None);
            }
            if self.files.contains_key(&ancestor) {
                break;
            }
            ancestor = parent.to_path_buf();
        }
        Err(anyhow!(
            "changeset target source was not observed: {}",
            path.display()
        ))
    }

    pub(super) fn bind(
        &self,
        root: &Path,
        change_set: &mut ChangeSet,
        artifact: &str,
    ) -> Result<MergeReviewSourceBinding> {
        self.source_root.validate_identity()?;
        ensure!(
            fs::canonicalize(root)? == self.root,
            "changeset source workspace identity changed"
        );
        let mut targets = BTreeMap::new();
        for file in &mut change_set.files {
            let path = Path::new(&file.path);
            ensure!(
                !path.as_os_str().is_empty()
                    && path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "changeset target path is not workspace-relative"
            );
            let hash = self.source_hash(path)?;
            ensure!(
                match file.action {
                    ChangeSetFileAction::Create => hash.is_none(),
                    ChangeSetFileAction::Update | ChangeSetFileAction::Delete => hash.is_some(),
                    ChangeSetFileAction::Rename => false,
                },
                "changeset action does not match its host-observed source: {}",
                file.path
            );
            file.before_hash = hash.clone();
            ensure!(
                targets.insert(path.to_path_buf(), hash).is_none(),
                "changeset repeats a target path"
            );
        }
        MergeReviewSourceBinding::new(root, targets, artifact)
    }
}

#[cfg(test)]
#[path = "../tests/changeset_source_observation_tests.rs"]
mod tests;
