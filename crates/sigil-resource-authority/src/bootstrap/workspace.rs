//! Workspace realm selection preserves the original configuration realm's physical identity.

use serde::{Deserialize, Serialize};

use super::*;

/// Frozen canonical workspace identity used only by host bootstrap composition.
///
/// Paths are hashed inside Resource Authority; neither a caller-selected namespace nor a
/// replacement directory at the same path can substitute for an existing workspace binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapWorkspaceV1 {
    schema_version: u32,
    path_hash: CanonicalHash,
    identity: CanonicalHash,
    canonical_root: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct WorkspaceBindingRecordV1 {
    schema_version: u32,
    path_hash: CanonicalHash,
    identity: CanonicalHash,
    canonical_root: PathBuf,
    legacy_shared_history: bool,
}

impl AuthorityBootstrapWorkspaceV1 {
    /// Returns the host-private canonical root captured with this binding.
    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    /// Returns the observed directory identity frozen into this workspace binding.
    #[must_use]
    pub const fn identity(&self) -> CanonicalHash {
        self.identity
    }

    /// Observes a workspace before selecting its durable controller realm.
    ///
    /// # Errors
    /// Returns an error when the workspace cannot be canonicalized or its identity observed.
    pub fn observe(workspace: &Path) -> Result<Self, BootstrapErrorV1> {
        let canonical = fs::canonicalize(workspace)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        let observed = crate::identity::canonical_identity(&canonical)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        if !observed.is_directory {
            return Err(BootstrapErrorV1::NotPlainDirectory(
                "workspace is not a directory".to_owned(),
            ));
        }
        Ok(Self {
            schema_version: 1,
            path_hash: canonical_bootstrap_hash(
                format!("authority-workspace-path-v1\0{}", canonical.display()).as_bytes(),
            ),
            identity: observed.digest,
            canonical_root: canonical,
        })
    }

    fn validate(&self) -> Result<(), BootstrapErrorV1> {
        if self.schema_version != 1
            || !self.canonical_root.is_absolute()
            || self.path_hash
                != canonical_bootstrap_hash(
                    format!(
                        "authority-workspace-path-v1\0{}",
                        self.canonical_root.display()
                    )
                    .as_bytes(),
                )
        {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "unknown authority workspace binding schema".to_owned(),
            ));
        }
        Ok(())
    }
}

impl AuthorityBootstrapStoreV1 {
    /// Resolves a diagnostic pointer path without creating an authority realm.
    ///
    /// # Errors
    /// Rejects an unavailable config identity or unsafe host configuration namespace.
    pub fn cutover_manifest_path_for_config(
        config_path: &Path,
    ) -> Result<PathBuf, BootstrapErrorV1> {
        let canonical = fs::canonicalize(config_path)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        Ok(Self::root_for_canonical_config_path(&canonical)?.join(CUTOVER_POINTER_FILE))
    }

    /// Selects one workspace's controller realm while retaining the original config realm.
    ///
    /// An unbound original realm is returned unchanged. Its runtime must complete the existing
    /// authenticated boot before recording the shared-history access anchor with
    /// [`Self::bind_workspace`]. This does not assert historical workspace ownership. Once bound,
    /// every other workspace gets a distinct root under the same configuration namespace.
    ///
    /// # Errors
    /// Rejects unsafe bootstrap files, malformed bindings and workspace identity drift.
    pub fn for_canonical_config_workspace_with_publication(
        config_path: &Path,
        workspace: &AuthorityBootstrapWorkspaceV1,
    ) -> Result<(Self, AuthorityBootstrapPublicationGuard), BootstrapErrorV1> {
        workspace.validate()?;
        let (original, publication) =
            Self::for_canonical_config_path_with_publication(config_path)?;
        let Some(binding) = original.workspace_binding(&publication)? else {
            return Ok((original, publication));
        };
        if binding.path_hash == workspace.path_hash {
            if binding != *workspace {
                return Err(BootstrapErrorV1::IdentityDrift);
            }
            return Ok((original, publication));
        }
        let namespace = original.namespace.clone();
        let root = namespace
            .join("workspace-realms-v1")
            .join(workspace.path_hash.to_hex());
        drop(publication);
        let (selected, publication) =
            Self::open_namespace_with_publication(namespace, root, original.authority_epoch)?;
        if let Some(binding) = selected.workspace_binding(&publication)?
            && binding != *workspace
        {
            return Err(BootstrapErrorV1::IdentityDrift);
        }
        Ok((selected, publication))
    }

    /// Reads the immutable workspace binding through the bootstrap's no-follow boundary.
    ///
    /// # Errors
    /// Rejects malformed, unsafe or unknown-schema bindings.
    pub fn workspace_binding(
        &self,
        publication: &AuthorityBootstrapPublicationGuard,
    ) -> Result<Option<AuthorityBootstrapWorkspaceV1>, BootstrapErrorV1> {
        let binding = self
            .read_json::<WorkspaceBindingRecordV1>(
                publication,
                AuthorityBootstrapObjectClassV1::WorkspaceBinding,
            )?
            .map(|record| AuthorityBootstrapWorkspaceV1 {
                schema_version: record.schema_version,
                path_hash: record.path_hash,
                identity: record.identity,
                canonical_root: record.canonical_root,
            });
        if let Some(binding) = &binding {
            binding.validate()?;
        }
        Ok(binding)
    }

    /// Returns the shared-history access workspace after a validated legacy boot.
    /// This is display/navigation evidence only and grants no access to the original realm.
    ///
    /// # Errors
    /// Rejects unsafe or malformed original binding metadata.
    pub fn legacy_shared_history_workspace(
        &self,
        publication: &AuthorityBootstrapPublicationGuard,
    ) -> Result<Option<AuthorityBootstrapWorkspaceV1>, BootstrapErrorV1> {
        self.validate_guard(publication)?;
        if self.root == self.namespace {
            return Ok(None);
        }
        // All workspace realms hold this same namespace publication lock. The original
        // write-once binding can therefore be read without acquiring a competing root lock.
        let original = Self {
            namespace: self.namespace.clone(),
            root: self.namespace.clone(),
            fresh: false,
            authority_epoch: self.authority_epoch,
        };
        let path = original.path(AuthorityBootstrapObjectClassV1::WorkspaceBinding);
        let Some(file) = open_private_read_file(&path)? else {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "original authority workspace binding is missing".to_owned(),
            ));
        };
        let mut bytes = Vec::new();
        file.take(MAX_BOOTSTRAP_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
        if bytes.len() as u64 > MAX_BOOTSTRAP_METADATA_BYTES {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "original authority workspace binding exceeds the metadata budget".to_owned(),
            ));
        }
        let record: WorkspaceBindingRecordV1 = serde_json::from_slice(&bytes)
            .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
        let workspace = AuthorityBootstrapWorkspaceV1 {
            schema_version: record.schema_version,
            path_hash: record.path_hash,
            identity: record.identity,
            canonical_root: record.canonical_root,
        };
        workspace.validate()?;
        Ok(record.legacy_shared_history.then_some(workspace))
    }

    /// Publishes the first binding after the runtime has validated the complete boot.
    /// Existing bytes and authority identities are never replaced by a different workspace.
    ///
    /// # Errors
    /// Rejects a conflicting binding or any bootstrap publication failure.
    pub fn bind_workspace(
        &self,
        publication: &AuthorityBootstrapPublicationGuard,
        workspace: &AuthorityBootstrapWorkspaceV1,
    ) -> Result<(), BootstrapErrorV1> {
        workspace.validate()?;
        if let Some(binding) = self.workspace_binding(publication)? {
            return if binding == *workspace {
                Ok(())
            } else {
                Err(BootstrapErrorV1::IdentityDrift)
            };
        }
        let bytes = serde_json::to_vec(&WorkspaceBindingRecordV1 {
            schema_version: workspace.schema_version,
            path_hash: workspace.path_hash,
            identity: workspace.identity,
            canonical_root: workspace.canonical_root.clone(),
            legacy_shared_history: self.root == self.namespace && !self.fresh,
        })
        .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
        self.publish_bytes(
            publication,
            AuthorityBootstrapObjectClassV1::WorkspaceBinding,
            &bytes,
        )
    }
}

#[cfg(test)]
#[path = "../tests/bootstrap_workspace_tests.rs"]
mod tests;
