use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Stable machine code returned when another interactive controller owns a session.
pub const SESSION_ATTACHMENT_BUSY_CODE: &str = "session_attachment_busy";

/// Builds an opaque recovery capability bound to one exact durable-session identity and the
/// attachment generation observed while acquisition was rejected.
#[must_use]
pub fn session_attachment_recovery_binding(
    session_identity: &str,
    observed_generation: &str,
) -> String {
    attachment_recovery_binding(session_identity.as_bytes(), observed_generation)
}

/// Builds an opaque recovery capability for surfaces that only know the exact session path at
/// attachment time. The path is normalized to an absolute identity and is never exposed.
#[must_use]
pub fn session_attachment_path_recovery_binding(
    session_path: &Path,
    observed_generation: &str,
) -> String {
    let identity = normalized_attachment_session_path(session_path);
    attachment_recovery_binding(identity.as_os_str().as_encoded_bytes(), observed_generation)
}

fn normalized_attachment_session_path(session_path: &Path) -> PathBuf {
    let absolute = if session_path.is_absolute() {
        session_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(session_path))
            .unwrap_or_else(|_| session_path.to_path_buf())
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return canonical;
    }
    let Some(parent) = absolute.parent() else {
        return absolute;
    };
    let Some(file_name) = absolute.file_name() else {
        return absolute;
    };
    fs::canonicalize(parent)
        .map(|canonical_parent| canonical_parent.join(file_name))
        .unwrap_or(absolute)
}

fn attachment_recovery_binding(session_identity: &[u8], observed_generation: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sigil-session-attachment-recovery-v1\n");
    digest.update(session_identity);
    digest.update(b"\n");
    digest.update(observed_generation.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

/// Cross-process, write-capable attachment to one durable session.
///
/// The operating-system lock is released on normal drop and process death. The sidecar contains
/// no owner metadata, endpoint, credential, or session transcript.
#[derive(Debug)]
pub struct InteractiveSessionAttachmentLease {
    session_path: PathBuf,
    lease_path: PathBuf,
    generation: String,
    route_authority: OnceLock<crate::provider_connections::SessionRouteMutationAuthority>,
    _lease: File,
}

impl InteractiveSessionAttachmentLease {
    /// Acquires the exact session attachment without waiting.
    pub fn acquire(
        session_path: impl AsRef<Path>,
    ) -> Result<Self, InteractiveSessionAttachmentError> {
        Self::acquire_with_expected_recovery(session_path, None)
    }

    /// Acquires an attachment only when the echoed recovery binding matches the exact generation
    /// last observed for this durable session identity.
    pub fn acquire_for_retry(
        session_path: impl AsRef<Path>,
        session_identity: &str,
        expected_recovery_binding: &str,
    ) -> Result<Self, InteractiveSessionAttachmentError> {
        Self::acquire_with_expected_recovery(
            session_path,
            Some(AttachmentRecoveryExpectation::SessionIdentity {
                session_identity,
                expected_recovery_binding,
            }),
        )
    }

    /// Acquires an attachment only when the echoed recovery binding matches the exact canonical
    /// session path and attachment generation observed by a path-oriented surface such as TUI.
    pub fn acquire_for_path_retry(
        session_path: impl AsRef<Path>,
        expected_recovery_binding: &str,
    ) -> Result<Self, InteractiveSessionAttachmentError> {
        Self::acquire_with_expected_recovery(
            session_path,
            Some(AttachmentRecoveryExpectation::SessionPath {
                expected_recovery_binding,
            }),
        )
    }

    fn acquire_with_expected_recovery(
        session_path: impl AsRef<Path>,
        expected_recovery: Option<AttachmentRecoveryExpectation<'_>>,
    ) -> Result<Self, InteractiveSessionAttachmentError> {
        let requested_path = session_path.as_ref();
        let absolute_path = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(InteractiveSessionAttachmentError::Unavailable)?
                .join(requested_path)
        };
        let requested_parent = absolute_path
            .parent()
            .ok_or(InteractiveSessionAttachmentError::InvalidPath)?;
        fs::create_dir_all(requested_parent)
            .map_err(InteractiveSessionAttachmentError::Unavailable)?;
        let session_path = normalized_attachment_session_path(&absolute_path);
        let file_name = session_path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or(InteractiveSessionAttachmentError::InvalidPath)?;
        let parent = session_path
            .parent()
            .ok_or(InteractiveSessionAttachmentError::InvalidPath)?;
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(InteractiveSessionAttachmentError::Unavailable)?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(InteractiveSessionAttachmentError::UnsafeLeasePath);
        }
        let lease_path = parent.join(format!("{file_name}.attachment-lock"));
        if let Ok(metadata) = fs::symlink_metadata(&lease_path)
            && (metadata.file_type().is_symlink() || !metadata.file_type().is_file())
        {
            return Err(InteractiveSessionAttachmentError::UnsafeLeasePath);
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut lease = options
            .open(&lease_path)
            .map_err(InteractiveSessionAttachmentError::Unavailable)?;
        let metadata = lease
            .metadata()
            .map_err(InteractiveSessionAttachmentError::Unavailable)?;
        if !metadata.is_file() {
            return Err(InteractiveSessionAttachmentError::UnsafeLeasePath);
        }
        #[cfg(unix)]
        {
            let mut permissions = metadata.permissions();
            if permissions.mode() & 0o777 != 0o600 {
                permissions.set_mode(0o600);
                lease
                    .set_permissions(permissions)
                    .map_err(InteractiveSessionAttachmentError::Unavailable)?;
            }
        }
        match lease.try_lock() {
            Ok(()) => {
                if let Some(expected) = expected_recovery {
                    let observed_generation = read_observed_generation(&mut lease);
                    let (current_binding, expected_binding) = match expected {
                        AttachmentRecoveryExpectation::SessionIdentity {
                            session_identity,
                            expected_recovery_binding,
                        } => (
                            session_attachment_recovery_binding(
                                session_identity,
                                &observed_generation,
                            ),
                            expected_recovery_binding,
                        ),
                        AttachmentRecoveryExpectation::SessionPath {
                            expected_recovery_binding,
                        } => (
                            attachment_recovery_binding(
                                session_path.as_os_str().as_encoded_bytes(),
                                &observed_generation,
                            ),
                            expected_recovery_binding,
                        ),
                    };
                    if current_binding != expected_binding {
                        let _ = lease.unlock();
                        return Err(InteractiveSessionAttachmentError::StaleRecoveryBinding {
                            recovery_binding: current_binding,
                        });
                    }
                }
                let generation = uuid::Uuid::new_v4().to_string();
                lease
                    .set_len(0)
                    .and_then(|()| lease.rewind())
                    .and_then(|()| lease.write_all(generation.as_bytes()))
                    .and_then(|()| lease.sync_data())
                    .map_err(InteractiveSessionAttachmentError::Unavailable)?;
                Ok(Self {
                    session_path,
                    lease_path,
                    generation,
                    route_authority: OnceLock::new(),
                    _lease: lease,
                })
            }
            Err(fs::TryLockError::WouldBlock) => {
                let observed_generation = read_observed_generation(&mut lease);
                Err(InteractiveSessionAttachmentError::Busy {
                    observed_generation,
                })
            }
            Err(fs::TryLockError::Error(error)) => {
                Err(InteractiveSessionAttachmentError::Unavailable(error))
            }
        }
    }

    #[must_use]
    pub fn session_path(&self) -> &Path {
        &self.session_path
    }

    #[must_use]
    pub fn lease_path(&self) -> &Path {
        &self.lease_path
    }

    /// Returns a process-local opaque generation for transition and quiescence binding.
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    /// Returns the one route-mutation authority shared by every execution and transition under
    /// this exact controller attachment.
    pub fn route_mutation_authority(
        &self,
        session_scope_id: &str,
    ) -> anyhow::Result<crate::provider_connections::SessionRouteMutationAuthority> {
        anyhow::ensure!(
            !session_scope_id.trim().is_empty(),
            "session route authority requires a durable scope"
        );
        let authority = self.route_authority.get_or_init(|| {
            crate::provider_connections::SessionRouteMutationAuthority::new(session_scope_id)
        });
        anyhow::ensure!(
            authority.session_scope_id() == session_scope_id,
            "session attachment route authority belongs to another durable scope"
        );
        Ok(authority.clone())
    }
}

enum AttachmentRecoveryExpectation<'a> {
    SessionIdentity {
        session_identity: &'a str,
        expected_recovery_binding: &'a str,
    },
    SessionPath {
        expected_recovery_binding: &'a str,
    },
}

impl Drop for InteractiveSessionAttachmentLease {
    fn drop(&mut self) {
        // Release explicitly so a same-process controller transition can reacquire immediately;
        // relying only on descriptor close is observably racy on some supported filesystems.
        let _ = self._lease.unlock();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InteractiveSessionAttachmentError {
    #[error("session_attachment_invalid_path")]
    InvalidPath,
    #[error("session_attachment_unsafe_lease_path")]
    UnsafeLeasePath,
    #[error("{SESSION_ATTACHMENT_BUSY_CODE}")]
    Busy { observed_generation: String },
    #[error("session_attachment_unavailable")]
    Unavailable(#[source] io::Error),
    #[error("session_attachment_recovery_stale")]
    StaleRecoveryBinding { recovery_binding: String },
}

impl InteractiveSessionAttachmentError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath => "session_attachment_invalid_path",
            Self::UnsafeLeasePath => "session_attachment_unsafe_lease_path",
            Self::Busy { .. } => SESSION_ATTACHMENT_BUSY_CODE,
            Self::Unavailable(_) => "session_attachment_unavailable",
            Self::StaleRecoveryBinding { .. } => "session_attachment_recovery_stale",
        }
    }
}

fn read_observed_generation(lease: &mut File) -> String {
    let mut generation = String::new();
    if lease.rewind().is_ok()
        && lease.take(64).read_to_string(&mut generation).is_ok()
        && uuid::Uuid::parse_str(generation.trim()).is_ok()
    {
        return generation.trim().to_owned();
    }
    "unavailable".to_owned()
}

#[cfg(test)]
#[path = "tests/interactive_session_attachment_tests.rs"]
mod tests;
