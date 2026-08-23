//! RFC-0071 section 18 R71.6: startup cutover manifest and mandatory adapter readiness.
//!
//! Application startup selects the legacy epoch or the new epoch exactly once. After the new
//! epoch is published, this binary only creates/reads current-schema sessions. There is no
//! per-consumer flag, no V2/V3 dual write, no legacy allocator fallback, and no active-process
//! provider switch. If any mandatory adapter/readiness probe fails the application fails
//! closed and does not start partially.

use serde::{Deserialize, Serialize};

use crate::external::sha256_hex;
use crate::resource::CanonicalHash;

pub const CUTOVER_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Closed startup epoch selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupEpochV1 {
    Legacy,
    NewCurrentSchema,
}

/// Closed mandatory adapter readiness channels (combined with section 9.5 rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MandatoryAdapterKindV1 {
    ExecutionOneShot,
    ExecutionTerminal,
    ExecutionExtension,
    FileAccessInProcess,
    StorageSessionLog,
    StorageSessionLifecycle,
    StorageInputHistory,
    StorageMemory,
    StorageSessionCatalog,
    StorageArtifact,
    StorageAdapterDurableState,
    ProjectionRebuildable,
    ProductStateUpdater,
    BorrowedNativeSave,
    BorrowedConfiguration,
    BorrowedReleaseOutput,
    RecoverySurface,
    BlockingGate,
}

/// One readiness probe result (adapter kind + pass/fail + evidence digest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterReadinessProbeV1 {
    pub adapter: MandatoryAdapterKindV1,
    pub passed: bool,
    pub evidence_digest: CanonicalHash,
}

/// Startup cutover manifest: exactly one epoch per application instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutoverManifestV1 {
    pub schema_version: u32,
    pub application_instance_id: String,
    pub selected_epoch: StartupEpochV1,
    pub application_generation: u64,
    pub authority_generation_digest: CanonicalHash,
    pub mandatory_readiness: Vec<AdapterReadinessProbeV1>,
    pub manifest_hash: CanonicalHash,
}

/// Closed cutover error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CutoverErrorV1 {
    #[error("cutover manifest references an unknown schema version")]
    UnknownSchemaVersion,
    #[error("cutover manifest was already published for this application instance")]
    AlreadyPublished,
    #[error("mandatory adapter readiness probe failed: {0:?}")]
    AdapterNotReady(MandatoryAdapterKindV1),
    #[error("new current-schema epoch selected but a readiness probe is missing")]
    MissingReadinessProbe,
    #[error("current-schema-only session cannot be opened by a legacy binary")]
    LegacyBinaryRejected,
    #[error("cutover manifest content hash does not match manifest_hash")]
    ManifestHashMismatch,
    #[error("old-schema session is explicitly unavailable in a current-schema binary")]
    LegacySessionUnavailable,
}

/// A session open attempt: session schema vs. binary epoch. After the new epoch is published
/// the current binary only creates/reads current-schema sessions; old-schema sessions are
/// explicitly unavailable (not opened with legacy interpretation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOpenAttemptV1 {
    pub session_epoch: StartupEpochV1,
    pub binary_epoch: StartupEpochV1,
}

/// Admits a session open attempt under the cutover schema boundary (shared by every surface).
pub fn admit_session_open(attempt: SessionOpenAttemptV1) -> Result<(), CutoverErrorV1> {
    match (attempt.session_epoch, attempt.binary_epoch) {
        (StartupEpochV1::Legacy, StartupEpochV1::NewCurrentSchema) => {
            Err(CutoverErrorV1::LegacySessionUnavailable)
        }
        (StartupEpochV1::NewCurrentSchema, StartupEpochV1::Legacy) => {
            Err(CutoverErrorV1::LegacyBinaryRejected)
        }
        (StartupEpochV1::Legacy, StartupEpochV1::Legacy)
        | (StartupEpochV1::NewCurrentSchema, StartupEpochV1::NewCurrentSchema) => Ok(()),
    }
}

/// Durable publication registry: an application instance publishes its epoch exactly once.
/// Re-reading the identical manifest is idempotent; a different manifest for the same instance
/// is rejected (fixed-forward).
#[derive(Debug, Default, Clone)]
pub struct CutoverManifestRegistryV1 {
    published: std::collections::BTreeMap<String, CanonicalHash>,
}

impl CutoverManifestRegistryV1 {
    pub const fn new() -> Self {
        Self {
            published: std::collections::BTreeMap::new(),
        }
    }

    /// Publishes (or idempotently re-reads) a cutover manifest.
    pub fn publish(&mut self, manifest: &CutoverManifestV1) -> Result<(), CutoverErrorV1> {
        validate_cutover_manifest(manifest)?;
        match self.published.get(&manifest.application_instance_id) {
            None => {
                self.published.insert(
                    manifest.application_instance_id.clone(),
                    manifest.manifest_hash,
                );
                Ok(())
            }
            Some(existing) if *existing == manifest.manifest_hash => Ok(()),
            Some(_) => Err(CutoverErrorV1::AlreadyPublished),
        }
    }

    /// Published manifest hash for an instance (None when not yet published).
    pub fn published_hash(&self, application_instance_id: &str) -> Option<&CanonicalHash> {
        self.published.get(application_instance_id)
    }
}

/// Hashable manifest content (everything except the content hash itself).
#[derive(Serialize)]
struct ManifestHashableV1<'a> {
    schema_version: u32,
    application_instance_id: &'a str,
    selected_epoch: StartupEpochV1,
    application_generation: u64,
    authority_generation_digest: CanonicalHash,
    mandatory_readiness: &'a [AdapterReadinessProbeV1],
}

/// Content-addressed manifest digest: stable JSON encoding of the hashable fields.
pub fn compute_manifest_hash(manifest: &CutoverManifestV1) -> CanonicalHash {
    let hashable = ManifestHashableV1 {
        schema_version: manifest.schema_version,
        application_instance_id: &manifest.application_instance_id,
        selected_epoch: manifest.selected_epoch,
        application_generation: manifest.application_generation,
        authority_generation_digest: manifest.authority_generation_digest,
        mandatory_readiness: &manifest.mandatory_readiness,
    };
    let encoded = serde_json::to_vec(&hashable).expect("infallible: manifest fields serialize");
    let hex = sha256_hex(&encoded);
    let mut bytes = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk).expect("sha256 hex"), 16)
            .expect("hex decode");
    }
    CanonicalHash::from_bytes(bytes)
}

/// Validates the manifest shape, content hash and readiness closure for a new-epoch cutover.
pub fn validate_cutover_manifest(manifest: &CutoverManifestV1) -> Result<(), CutoverErrorV1> {
    if manifest.schema_version != CUTOVER_MANIFEST_SCHEMA_VERSION {
        return Err(CutoverErrorV1::UnknownSchemaVersion);
    }
    if compute_manifest_hash(manifest) != manifest.manifest_hash {
        return Err(CutoverErrorV1::ManifestHashMismatch);
    }
    if manifest.selected_epoch == StartupEpochV1::NewCurrentSchema {
        // Every mandatory adapter must be present and passing; missing = fail closed.
        let required: &[MandatoryAdapterKindV1] = &[
            MandatoryAdapterKindV1::ExecutionOneShot,
            MandatoryAdapterKindV1::ExecutionTerminal,
            MandatoryAdapterKindV1::ExecutionExtension,
            MandatoryAdapterKindV1::FileAccessInProcess,
            MandatoryAdapterKindV1::StorageSessionLog,
            MandatoryAdapterKindV1::StorageSessionLifecycle,
            MandatoryAdapterKindV1::StorageInputHistory,
            MandatoryAdapterKindV1::StorageMemory,
            MandatoryAdapterKindV1::StorageSessionCatalog,
            MandatoryAdapterKindV1::StorageArtifact,
            MandatoryAdapterKindV1::StorageAdapterDurableState,
            MandatoryAdapterKindV1::ProjectionRebuildable,
            MandatoryAdapterKindV1::ProductStateUpdater,
            MandatoryAdapterKindV1::BorrowedNativeSave,
            MandatoryAdapterKindV1::BorrowedConfiguration,
            MandatoryAdapterKindV1::BorrowedReleaseOutput,
            MandatoryAdapterKindV1::RecoverySurface,
            MandatoryAdapterKindV1::BlockingGate,
        ];
        for adapter in required {
            let probe = manifest
                .mandatory_readiness
                .iter()
                .find(|probe| &probe.adapter == adapter)
                .ok_or(CutoverErrorV1::MissingReadinessProbe)?;
            if !probe.passed {
                return Err(CutoverErrorV1::AdapterNotReady(*adapter));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(adapter: MandatoryAdapterKindV1, passed: bool) -> AdapterReadinessProbeV1 {
        AdapterReadinessProbeV1 {
            adapter,
            passed,
            evidence_digest: CanonicalHash::from_bytes([0x11; 32]),
        }
    }

    fn ready_manifest() -> CutoverManifestV1 {
        let mut manifest = CutoverManifestV1 {
            schema_version: CUTOVER_MANIFEST_SCHEMA_VERSION,
            application_instance_id: "inst-1".into(),
            selected_epoch: StartupEpochV1::NewCurrentSchema,
            application_generation: 1,
            authority_generation_digest: CanonicalHash::from_bytes([0x22; 32]),
            mandatory_readiness: Vec::new(),
            manifest_hash: CanonicalHash::from_bytes([0u8; 32]),
        };
        manifest.mandatory_readiness = vec![
            MandatoryAdapterKindV1::ExecutionOneShot,
            MandatoryAdapterKindV1::ExecutionTerminal,
            MandatoryAdapterKindV1::ExecutionExtension,
            MandatoryAdapterKindV1::FileAccessInProcess,
            MandatoryAdapterKindV1::StorageSessionLog,
            MandatoryAdapterKindV1::StorageSessionLifecycle,
            MandatoryAdapterKindV1::StorageInputHistory,
            MandatoryAdapterKindV1::StorageMemory,
            MandatoryAdapterKindV1::StorageSessionCatalog,
            MandatoryAdapterKindV1::StorageArtifact,
            MandatoryAdapterKindV1::StorageAdapterDurableState,
            MandatoryAdapterKindV1::ProjectionRebuildable,
            MandatoryAdapterKindV1::ProductStateUpdater,
            MandatoryAdapterKindV1::BorrowedNativeSave,
            MandatoryAdapterKindV1::BorrowedConfiguration,
            MandatoryAdapterKindV1::BorrowedReleaseOutput,
            MandatoryAdapterKindV1::RecoverySurface,
            MandatoryAdapterKindV1::BlockingGate,
        ]
        .into_iter()
        .map(|adapter| probe(adapter, true))
        .collect();
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        manifest
    }

    #[test]
    fn r71_cutover_new_epoch_all_adapters_ready_passes() {
        let manifest = ready_manifest();
        validate_cutover_manifest(&manifest).expect("valid new-epoch manifest");
    }

    #[test]
    fn r71_cutover_missing_adapter_fails_closed() {
        let mut manifest = ready_manifest();
        manifest
            .mandatory_readiness
            .retain(|p| p.adapter != MandatoryAdapterKindV1::StorageArtifact);
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        let error = validate_cutover_manifest(&manifest).expect_err("missing probe");
        assert!(matches!(error, CutoverErrorV1::MissingReadinessProbe));
    }

    #[test]
    fn r71_cutover_failed_adapter_fails_closed() {
        let mut manifest = ready_manifest();
        for probe in manifest.mandatory_readiness.iter_mut() {
            if probe.adapter == MandatoryAdapterKindV1::BlockingGate {
                probe.passed = false;
            }
        }
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        let error = validate_cutover_manifest(&manifest).expect_err("failed probe");
        assert!(matches!(
            error,
            CutoverErrorV1::AdapterNotReady(MandatoryAdapterKindV1::BlockingGate)
        ));
    }

    #[test]
    fn r71_cutover_unknown_schema_version_fails_closed() {
        let mut manifest = ready_manifest();
        manifest.schema_version = 7;
        let error = validate_cutover_manifest(&manifest).expect_err("unknown version");
        assert!(matches!(error, CutoverErrorV1::UnknownSchemaVersion));
    }

    #[test]
    fn r71_cutover_legacy_epoch_does_not_require_probes() {
        let mut manifest = ready_manifest();
        manifest.selected_epoch = StartupEpochV1::Legacy;
        manifest.mandatory_readiness.clear();
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        validate_cutover_manifest(&manifest).expect("legacy needs no probes");
    }

    #[test]
    fn r71_cutover_manifest_round_trips_json_losslessly() {
        let manifest = ready_manifest();
        let encoded = serde_json::to_string(&manifest).expect("encode");
        let decoded: CutoverManifestV1 = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn r71_current_schema_only_new_binary_rejects_legacy_session() {
        let error = admit_session_open(SessionOpenAttemptV1 {
            session_epoch: StartupEpochV1::Legacy,
            binary_epoch: StartupEpochV1::NewCurrentSchema,
        })
        .expect_err("old session unavailable");
        assert!(matches!(error, CutoverErrorV1::LegacySessionUnavailable));
    }

    #[test]
    fn r71_current_schema_only_legacy_binary_rejects_new_session() {
        let error = admit_session_open(SessionOpenAttemptV1 {
            session_epoch: StartupEpochV1::NewCurrentSchema,
            binary_epoch: StartupEpochV1::Legacy,
        })
        .expect_err("new session unreadable");
        assert!(matches!(error, CutoverErrorV1::LegacyBinaryRejected));
    }

    #[test]
    fn r71_current_schema_only_matching_epoch_open_passes() {
        admit_session_open(SessionOpenAttemptV1 {
            session_epoch: StartupEpochV1::Legacy,
            binary_epoch: StartupEpochV1::Legacy,
        })
        .expect("legacy on legacy");
        admit_session_open(SessionOpenAttemptV1 {
            session_epoch: StartupEpochV1::NewCurrentSchema,
            binary_epoch: StartupEpochV1::NewCurrentSchema,
        })
        .expect("new on new");
    }

    #[test]
    fn r71_current_schema_only_republish_identical_manifest_idempotent() {
        let manifest = ready_manifest();
        let mut registry = CutoverManifestRegistryV1::new();
        registry.publish(&manifest).expect("first publish");
        registry.publish(&manifest).expect("idempotent re-read");
    }

    #[test]
    fn r71_current_schema_only_different_manifest_republish_rejected() {
        let mut manifest = ready_manifest();
        let mut registry = CutoverManifestRegistryV1::new();
        registry.publish(&manifest).expect("publish");
        manifest.application_generation += 1;
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        let error = registry.publish(&manifest).expect_err("fixed forward");
        assert!(matches!(error, CutoverErrorV1::AlreadyPublished));
    }
}
