//! RFC-0071 section 18 R71.6: startup cutover manifest and mandatory adapter readiness.
//!
//! Application startup selects the current epoch exactly once. `Legacy` remains a wire-level
//! historical value so old persisted records can be decoded and reported, but it is not a
//! runnable boot mode. If any mandatory adapter/readiness probe fails the application fails
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
    #[error("current-schema authority composition is unavailable")]
    AuthorityUnavailable,
}

/// Schema version for the renderer-neutral cutover status shared by all product surfaces.
pub const CUTOVER_SURFACE_SCHEMA_VERSION: u16 = 1;

/// Epoch displayed by a surface. `Legacy` is retained only for historical DTO compatibility;
/// a legacy manifest is projected as unavailable with an explicit unsupported-data blocker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutoverSurfaceEpochV1 {
    Legacy,
    NewCurrentSchema,
    Unavailable,
}

impl CutoverSurfaceEpochV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::NewCurrentSchema => "new_current_schema",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Authority/readiness state displayed by a surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutoverAuthorityStateV1 {
    Legacy,
    Ready,
    Blocked,
    Unavailable,
}

impl CutoverAuthorityStateV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Stable blocker reason; no host path, error text, or runtime-private handle crosses the
/// product boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutoverBlockerCodeV1 {
    ManifestCorrupt,
    MissingReadinessProbe,
    AdapterNotReady,
    UnsupportedLegacyData,
}

/// One bounded current-schema blocker projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CutoverBlockerV1 {
    pub code: CutoverBlockerCodeV1,
    pub adapter: Option<MandatoryAdapterKindV1>,
}

/// Shared epoch/authority/blocker DTO. CLI, TUI, HTTP and Desktop must project this value
/// without recomputing a second readiness state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CutoverSurfaceStatusV1 {
    pub schema_version: u16,
    pub epoch: CutoverSurfaceEpochV1,
    pub authority: CutoverAuthorityStateV1,
    pub blockers: Vec<CutoverBlockerV1>,
}

impl Default for CutoverSurfaceStatusV1 {
    fn default() -> Self {
        Self::unavailable()
    }
}

impl CutoverSurfaceStatusV1 {
    #[must_use]
    pub fn from_manifest(manifest: &CutoverManifestV1) -> Self {
        let epoch = match manifest.selected_epoch {
            StartupEpochV1::Legacy => CutoverSurfaceEpochV1::Legacy,
            StartupEpochV1::NewCurrentSchema => CutoverSurfaceEpochV1::NewCurrentSchema,
        };
        let blockers = if manifest.selected_epoch == StartupEpochV1::Legacy {
            vec![CutoverBlockerV1 {
                code: CutoverBlockerCodeV1::UnsupportedLegacyData,
                adapter: None,
            }]
        } else if manifest.selected_epoch == StartupEpochV1::NewCurrentSchema {
            mandatory_adapter_kinds_v1()
                .iter()
                .filter_map(|adapter| {
                    let probe = manifest
                        .mandatory_readiness
                        .iter()
                        .find(|probe| probe.adapter == *adapter);
                    match probe {
                        Some(probe) if probe.passed => None,
                        Some(_) => Some(CutoverBlockerV1 {
                            code: CutoverBlockerCodeV1::AdapterNotReady,
                            adapter: Some(*adapter),
                        }),
                        None => Some(CutoverBlockerV1 {
                            code: CutoverBlockerCodeV1::MissingReadinessProbe,
                            adapter: Some(*adapter),
                        }),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let authority = match epoch {
            CutoverSurfaceEpochV1::Legacy => CutoverAuthorityStateV1::Unavailable,
            CutoverSurfaceEpochV1::NewCurrentSchema if blockers.is_empty() => {
                CutoverAuthorityStateV1::Ready
            }
            CutoverSurfaceEpochV1::NewCurrentSchema => CutoverAuthorityStateV1::Blocked,
            CutoverSurfaceEpochV1::Unavailable => CutoverAuthorityStateV1::Unavailable,
        };
        Self {
            schema_version: CUTOVER_SURFACE_SCHEMA_VERSION,
            epoch,
            authority,
            blockers,
        }
    }

    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            schema_version: CUTOVER_SURFACE_SCHEMA_VERSION,
            epoch: CutoverSurfaceEpochV1::Unavailable,
            authority: CutoverAuthorityStateV1::Unavailable,
            blockers: vec![CutoverBlockerV1 {
                code: CutoverBlockerCodeV1::ManifestCorrupt,
                adapter: None,
            }],
        }
    }

    #[must_use]
    pub fn unsupported_legacy_data() -> Self {
        Self {
            schema_version: CUTOVER_SURFACE_SCHEMA_VERSION,
            epoch: CutoverSurfaceEpochV1::Legacy,
            authority: CutoverAuthorityStateV1::Unavailable,
            blockers: vec![CutoverBlockerV1 {
                code: CutoverBlockerCodeV1::UnsupportedLegacyData,
                adapter: None,
            }],
        }
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.authority == CutoverAuthorityStateV1::Ready && self.blockers.is_empty()
    }
}

/// Closed mandatory adapter set used both by manifest validation and by the shared surface
/// projection. Keeping one source prevents a surface from silently omitting a blocker.
pub const fn mandatory_adapter_kinds_v1() -> &'static [MandatoryAdapterKindV1] {
    &[
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
        (StartupEpochV1::Legacy, StartupEpochV1::Legacy) => {
            Err(CutoverErrorV1::LegacySessionUnavailable)
        }
        (StartupEpochV1::NewCurrentSchema, StartupEpochV1::NewCurrentSchema) => Ok(()),
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
        for adapter in mandatory_adapter_kinds_v1() {
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
#[path = "tests/cutover_manifest_tests.rs"]
mod tests;
