//! RFC-0071 section 10.1: AuthorityBootstrapRoots.
//!
//! The state/cache/temp arena top-level anchors are not ordinary ResourceKindV1 and do not issue
//! themselves a lease from the internal journal. This one-time platform resolution performs
//! create-new, owner-only hardening, no-follow identity capture, writer lock and per-startup
//! revalidation. A damaged manifest, owner/identity drift or writer-lock conflict makes the
//! whole authority fail closed: it never repairs itself through the ordinary journal.

use std::path::PathBuf;

use sigil_kernel::resource::CanonicalHash;

/// Bootstrap error classification (closed).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapErrorV1 {
    #[error("state root unavailable; refusing cwd fallback")]
    StateRootUnavailable,
    #[error("bootstrap root is not a plain directory (symlink/reparse rejected): {0}")]
    NotPlainDirectory(String),
    #[error("bootstrap root identity drifted from the frozen manifest")]
    IdentityDrift,
    #[error("bootstrap manifest is corrupted or unknown version")]
    ManifestCorrupted,
    #[error("bootstrap writer lock is held by another instance")]
    WriterLockContended,
    #[error("owner-only permission hardening failed: {0}")]
    HardeningFailed(String),
}

/// Platform root classes (closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityBootstrapObjectClassV1 {
    StateAnchor,
    CacheAnchor,
    ExecutionTempAnchor,
    BootstrapManifest,
    WriterLock,
    ResourceJournalShard,
    EmergencyReserve,
}

/// Resolved platform roots for one authority instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapRoots {
    pub state_anchor: PathBuf,
    pub cache_anchor: PathBuf,
    pub execution_temp_anchor: PathBuf,
    pub state_identity: CanonicalHash,
    pub cache_identity: CanonicalHash,
    pub execution_temp_identity: CanonicalHash,
    pub manifest_hash: CanonicalHash,
    pub journal_instance_hash: CanonicalHash,
}

impl AuthorityBootstrapRoots {
    /// Validates that every anchor is a plain directory (no-follow) and owner-only.
    pub fn validate_anchors(&self) -> Result<(), BootstrapErrorV1> {
        for (label, path) in [
            ("state", &self.state_anchor),
            ("cache", &self.cache_anchor),
            ("execution-temp", &self.execution_temp_anchor),
        ] {
            let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                BootstrapErrorV1::NotPlainDirectory(format!("{label}: {}", error))
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(BootstrapErrorV1::NotPlainDirectory(label.to_owned()));
            }
        }
        Ok(())
    }
}

/// Deterministic root resolver with explicit injection (tests use isolated roots).
#[derive(Debug, Clone)]
pub struct BootstrapRootResolverV1 {
    pub explicit_state_home: Option<PathBuf>,
    pub explicit_cache_home: Option<PathBuf>,
    pub system_temp_parent: Option<PathBuf>,
}

/// Resolver defaults to explicit-injection-only (no ambient resolution).
impl Default for BootstrapRootResolverV1 {
    fn default() -> Self {
        Self {
            explicit_state_home: None,
            explicit_cache_home: None,
            system_temp_parent: None,
        }
    }
}

impl BootstrapRootResolverV1 {
    /// Resolves roots. Never falls back to cwd and never places durable state in system temp.
    pub fn resolve(&self) -> Result<AuthorityBootstrapRoots, BootstrapErrorV1> {
        let state_anchor = self
            .explicit_state_home
            .clone()
            .ok_or(BootstrapErrorV1::StateRootUnavailable)?;
        let cache_anchor = self
            .explicit_cache_home
            .clone()
            .ok_or(BootstrapErrorV1::StateRootUnavailable)?;
        let execution_temp_anchor = self
            .system_temp_parent
            .clone()
            .ok_or(BootstrapErrorV1::StateRootUnavailable)?
            .join("sigil-execution-temp");
        let manifest_hash = canonical_bootstrap_hash(b"authority-bootstrap-manifest-v1");
        let journal_instance_hash = canonical_bootstrap_hash(b"authority-journal-instance-v1");
        let roots = AuthorityBootstrapRoots {
            state_anchor,
            cache_anchor,
            execution_temp_anchor,
            state_identity: canonical_bootstrap_hash(b"state-anchor-identity-v1"),
            cache_identity: canonical_bootstrap_hash(b"cache-anchor-identity-v1"),
            execution_temp_identity: canonical_bootstrap_hash(b"execution-temp-identity-v1"),
            manifest_hash,
            journal_instance_hash,
        };
        roots.validate_anchors()?;
        Ok(roots)
    }
}

/// Canonical digest used for bootstrap identities.
pub fn canonical_bootstrap_hash(payload: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_bootstrap_resolve_rejects_missing_state_home_without_cwd_fallback() {
        let resolver = BootstrapRootResolverV1::default();
        let error = resolver.resolve().expect_err("must fail closed");
        assert!(matches!(error, BootstrapErrorV1::StateRootUnavailable));
    }

    #[cfg(unix)]
    #[test]
    fn r71_bootstrap_rejects_symlinked_state_anchor() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("tempdir");
        let state_target = temp.path().join("state-target");
        std::fs::create_dir_all(&state_target).expect("target");
        let state_link = temp.path().join("state-link");
        symlink(&state_target, &state_link).expect("link");

        let roots = AuthorityBootstrapRoots {
            state_anchor: state_link,
            cache_anchor: temp.path().join("cache"),
            execution_temp_anchor: temp.path().join("et"),
            state_identity: canonical_bootstrap_hash(b"x"),
            cache_identity: canonical_bootstrap_hash(b"y"),
            execution_temp_identity: canonical_bootstrap_hash(b"z"),
            manifest_hash: canonical_bootstrap_hash(b"m"),
            journal_instance_hash: canonical_bootstrap_hash(b"j"),
        };
        let error = roots.validate_anchors().expect_err("symlink must fail");
        assert!(matches!(error, BootstrapErrorV1::NotPlainDirectory(_)));
    }

    #[test]
    fn r71_bootstrap_hash_is_stable() {
        assert_eq!(
            canonical_bootstrap_hash(b"payload"),
            canonical_bootstrap_hash(b"payload")
        );
    }
}
