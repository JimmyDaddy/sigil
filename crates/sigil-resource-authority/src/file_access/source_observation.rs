//! Side-effect-free host provenance observations. These facts never authorize file mutation.

use std::{
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use sigil_kernel::{
    managed_file_access::{ManagedFileAccessErrorV1, ManagedFileExecutionContextV1},
    resource::CanonicalHash,
};

use super::{AuthorityManagedFileAccessServiceV1, relative_io_error, traversal};

/// An entry observed through the host's workspace directory handle.
#[derive(Debug)]
pub enum HostSourceEntryObservation {
    Directory,
    /// A missing digest means the bounded read could not establish the file's source content.
    File {
        content_digest: Option<CanonicalHash>,
        observed_bytes: u64,
    },
}

/// Read-only host observation root; it cannot issue admission or perform writes.
#[derive(Debug)]
pub struct HostSourceObservationRoot {
    root: PathBuf,
    root_handle: Arc<File>,
    root_identity: CanonicalHash,
}

impl HostSourceObservationRoot {
    /// Opens the already selected host workspace without following its final component.
    ///
    /// # Errors
    /// Returns an error if the root cannot be safely opened or this platform is unsupported.
    pub fn open(root: &Path) -> Result<Self, ManagedFileAccessErrorV1> {
        #[cfg(any(unix, windows))]
        {
            let root_handle = AuthorityManagedFileAccessServiceV1::open_workspace_root(root)?;
            let root_identity = handle_identity(root, &root_handle)?;
            Ok(Self {
                root: root.to_path_buf(),
                root_handle,
                root_identity,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = root;
            Err(ManagedFileAccessErrorV1::OperationNotPermitted)
        }
    }

    /// Confirms that the selected path still identifies the same root directory.
    ///
    /// # Errors
    /// Returns an error if the root is missing, replaced, or resolves through an alias.
    pub fn validate_identity(&self) -> Result<(), ManagedFileAccessErrorV1> {
        let observed = crate::identity::canonical_identity(&self.root)
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        if observed.digest != self.root_identity || observed.is_symlink {
            return Err(ManagedFileAccessErrorV1::SubjectIdentityDrift);
        }
        Ok(())
    }

    fn open_relative(&self, relative: &Path) -> Result<Vec<File>, ManagedFileAccessErrorV1> {
        self.validate_identity()?;
        let mut handles = vec![self.root_handle.try_clone().map_err(relative_io_error)?];
        let mut path = self.root.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(ManagedFileAccessErrorV1::AliasCollision);
            };
            let name = name
                .to_str()
                .ok_or(ManagedFileAccessErrorV1::AliasCollision)?;
            path.push(name);
            let parent = handles
                .last()
                .ok_or(ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
            handles.push(traversal::open_child(parent, name, &path)?);
        }
        Ok(handles)
    }

    /// Lists bounded names, returning whether the directory was completely enumerated.
    /// Ancestor handles remain live throughout enumeration, including on Windows.
    ///
    /// # Errors
    /// Returns an error on identity drift, unsafe components, or failed directory enumeration.
    pub fn directory_names(
        &self,
        relative: &Path,
        limit: usize,
    ) -> Result<(Vec<String>, bool), ManagedFileAccessErrorV1> {
        let handles = self.open_relative(relative)?;
        let directory = handles
            .last()
            .ok_or(ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let limit = limit.min(super::MAX_DIRECTORY_SCAN_ENTRIES);
        let mut names = traversal::names(
            directory,
            &self.root.join(relative),
            limit.saturating_add(1),
            &ManagedFileExecutionContextV1::default(),
        )?;
        self.validate_identity()?;
        let complete = names.len() <= limit;
        names.truncate(limit);
        names.sort();
        Ok((names, complete))
    }

    /// Observes a source entry using the authority's existing non-following component walk.
    ///
    /// # Errors
    /// Returns an error on root drift, unsafe components, or unavailable metadata. A bounded
    /// content read that cannot establish a stable digest is represented explicitly as unknown.
    pub fn entry(
        &self,
        relative: &Path,
        max_bytes: u64,
    ) -> Result<HostSourceEntryObservation, ManagedFileAccessErrorV1> {
        use sha2::{Digest, Sha256};
        let handles = self.open_relative(relative)?;
        let file = handles
            .last()
            .ok_or(ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let metadata = file.metadata().map_err(relative_io_error)?;
        if metadata.is_dir() {
            return Ok(HostSourceEntryObservation::Directory);
        }
        if !metadata.is_file() {
            return Err(ManagedFileAccessErrorV1::AliasCollision);
        }
        let limit = max_bytes.min(super::MAX_READ_SCAN_BYTES);
        if metadata.len() > limit {
            return Ok(HostSourceEntryObservation::File {
                content_digest: None,
                observed_bytes: 0,
            });
        }
        let mut bytes = Vec::new();
        let complete = file
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.len() as u64 <= limit;
        self.validate_identity()?;
        let unchanged = file.metadata().is_ok_and(|after| {
            metadata.len() == after.len() && metadata.modified().ok() == after.modified().ok()
        });
        Ok(HostSourceEntryObservation::File {
            content_digest: (complete && unchanged)
                .then(|| CanonicalHash::from_bytes(Sha256::digest(&bytes).into())),
            observed_bytes: bytes.len() as u64,
        })
    }
}

fn handle_identity(path: &Path, file: &File) -> Result<CanonicalHash, ManagedFileAccessErrorV1> {
    #[cfg(windows)]
    let identity = crate::identity::canonical_identity_from_handle(path, file)
        .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
    #[cfg(not(windows))]
    let identity = crate::identity::canonical_identity_from_metadata(
        path,
        &file.metadata().map_err(relative_io_error)?,
    );
    Ok(identity.digest)
}

#[cfg(test)]
#[path = "../tests/source_observation_tests.rs"]
mod tests;
