//! Frozen historical bootstrap evidence used only to advance to a freshly qualified current
//! decision. These decoders never supply a composition or a runnable historical decision.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{CutoverErrorV1, MandatoryAdapterKindV1, StartupEpochV1};
use crate::config::{OptionalCapability, RuntimeCompositionProfile};
use crate::resource::CanonicalHash;

/// Validation failure for historical bootstrap evidence, distinct from a current decision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CutoverPredecessorErrorV1 {
    #[error("historical cutover manifest shape or identity is invalid")]
    InvalidManifest,
    #[error(transparent)]
    Cutover(#[from] CutoverErrorV1),
}

/// Integrity-checked historical predecessor identity. It cannot authorize a runtime or session;
/// the boot owner must verify its own identity and publish a newer fully probed decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCutoverPredecessorV1 {
    application_instance_id: String,
    application_generation: u64,
    authority_generation_digest: CanonicalHash,
    manifest_hash: CanonicalHash,
}

impl ValidatedCutoverPredecessorV1 {
    #[must_use]
    pub fn application_instance_id(&self) -> &str {
        &self.application_instance_id
    }

    #[must_use]
    pub const fn application_generation(&self) -> u64 {
        self.application_generation
    }

    #[must_use]
    pub const fn authority_generation_digest(&self) -> CanonicalHash {
        self.authority_generation_digest
    }

    #[must_use]
    pub const fn manifest_hash(&self) -> CanonicalHash {
        self.manifest_hash
    }
}

// Freeze the original publisher's complete set. Current profile defaults and capability
// selection must never reinterpret a schema-1 manifest or its readiness evidence.
const SCHEMA_1_ADAPTERS: [MandatoryAdapterKindV1; 18] = [
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

#[derive(Serialize, Deserialize)]
struct Schema1ReadinessProbe {
    adapter: MandatoryAdapterKindV1,
    passed: bool,
    evidence_digest: CanonicalHash,
}

#[derive(Serialize, Deserialize)]
struct Schema1Manifest {
    schema_version: u32,
    application_instance_id: String,
    selected_epoch: StartupEpochV1,
    application_generation: u64,
    authority_generation_digest: CanonicalHash,
    mandatory_readiness: Vec<Schema1ReadinessProbe>,
    manifest_hash: CanonicalHash,
}

// This declared field order is the historical hash encoding. It is neither a JSON object
// sorted by key nor the current manifest encoding with a default composition inserted.
#[derive(Serialize)]
struct Schema1ManifestHashable<'a> {
    schema_version: u32,
    application_instance_id: &'a str,
    selected_epoch: StartupEpochV1,
    application_generation: u64,
    authority_generation_digest: CanonicalHash,
    mandatory_readiness: &'a [Schema1ReadinessProbe],
}

fn schema1_manifest_hash(manifest: &Schema1Manifest) -> CanonicalHash {
    let bytes = serde_json::to_vec(&Schema1ManifestHashable {
        schema_version: manifest.schema_version,
        application_instance_id: &manifest.application_instance_id,
        selected_epoch: manifest.selected_epoch,
        application_generation: manifest.application_generation,
        authority_generation_digest: manifest.authority_generation_digest,
        mandatory_readiness: &manifest.mandatory_readiness,
    })
    .expect("infallible: frozen manifest fields serialize");
    CanonicalHash::from_bytes(Sha256::digest(bytes).into())
}

/// Accepts only the complete schema-1 shape emitted by the historical current-epoch boot
/// publisher. Duplicate/failed readiness rows and a historical `Legacy` epoch are rejected;
/// the old permissive validator is not a compatibility path for arbitrary historical data.
pub fn validate_bootstrap_cutover_predecessor_v1(
    bytes: &[u8],
) -> Result<ValidatedCutoverPredecessorV1, CutoverPredecessorErrorV1> {
    #[derive(Deserialize)]
    struct Header {
        schema_version: u32,
    }
    let header: Header =
        serde_json::from_slice(bytes).map_err(|_| CutoverPredecessorErrorV1::InvalidManifest)?;
    if header.schema_version != 1 {
        return Err(CutoverErrorV1::UnknownSchemaVersion.into());
    }
    let manifest: Schema1Manifest =
        serde_json::from_slice(bytes).map_err(|_| CutoverPredecessorErrorV1::InvalidManifest)?;
    if manifest.application_instance_id.trim().is_empty()
        || manifest.application_generation == 0
        || manifest.selected_epoch != StartupEpochV1::NewCurrentSchema
    {
        return Err(CutoverPredecessorErrorV1::InvalidManifest);
    }
    if schema1_manifest_hash(&manifest) != manifest.manifest_hash {
        return Err(CutoverErrorV1::ManifestHashMismatch.into());
    }
    let mut seen = BTreeSet::new();
    for probe in &manifest.mandatory_readiness {
        if !SCHEMA_1_ADAPTERS.contains(&probe.adapter) {
            return Err(CutoverErrorV1::UnexpectedReadinessProbe(probe.adapter).into());
        }
        if !seen.insert(probe.adapter) {
            return Err(CutoverErrorV1::DuplicateReadinessProbe(probe.adapter).into());
        }
        if !probe.passed {
            return Err(CutoverErrorV1::AdapterNotReady(probe.adapter).into());
        }
    }
    if seen.len() != SCHEMA_1_ADAPTERS.len() {
        return Err(CutoverErrorV1::MissingReadinessProbe.into());
    }
    Ok(ValidatedCutoverPredecessorV1 {
        application_instance_id: manifest.application_instance_id,
        application_generation: manifest.application_generation,
        authority_generation_digest: manifest.authority_generation_digest,
        manifest_hash: manifest.manifest_hash,
    })
}

// The old schema-2 publisher serialized both composition fields and a sorted, unique
// enhancement set. Do not apply current defaults or normalize malformed historical bytes.
#[derive(Serialize, Deserialize)]
struct OptionalTerminalCompositionV2 {
    profile: RuntimeCompositionProfile,
    enhancements: Vec<OptionalCapability>,
}

const OPTIONAL_TERMINAL_CAPABILITIES_V2: [OptionalCapability; 12] = [
    OptionalCapability::TaskOrchestration,
    OptionalCapability::Memory,
    OptionalCapability::Skills,
    OptionalCapability::CodeIntelligence,
    OptionalCapability::RepositoryContext,
    OptionalCapability::Web,
    OptionalCapability::Mcp,
    OptionalCapability::Terminal,
    OptionalCapability::ChangeSets,
    OptionalCapability::SessionTitles,
    OptionalCapability::Compaction,
    OptionalCapability::Updater,
];

#[derive(Serialize, Deserialize)]
struct OptionalTerminalManifestV2 {
    schema_version: u32,
    application_instance_id: String,
    selected_epoch: StartupEpochV1,
    application_generation: u64,
    authority_generation_digest: CanonicalHash,
    composition: OptionalTerminalCompositionV2,
    mandatory_readiness: Vec<Schema1ReadinessProbe>,
    manifest_hash: CanonicalHash,
}

#[derive(Serialize)]
struct OptionalTerminalManifestHashableV2<'a> {
    schema_version: u32,
    application_instance_id: &'a str,
    selected_epoch: StartupEpochV1,
    application_generation: u64,
    authority_generation_digest: CanonicalHash,
    composition: &'a OptionalTerminalCompositionV2,
    mandatory_readiness: &'a [Schema1ReadinessProbe],
}

fn optional_terminal_manifest_hash(manifest: &OptionalTerminalManifestV2) -> CanonicalHash {
    let bytes = serde_json::to_vec(&OptionalTerminalManifestHashableV2 {
        schema_version: manifest.schema_version,
        application_instance_id: &manifest.application_instance_id,
        selected_epoch: manifest.selected_epoch,
        application_generation: manifest.application_generation,
        authority_generation_digest: manifest.authority_generation_digest,
        composition: &manifest.composition,
        mandatory_readiness: &manifest.mandatory_readiness,
    })
    .expect("infallible: frozen manifest fields serialize");
    CanonicalHash::from_bytes(Sha256::digest(bytes).into())
}

/// Validates only the obsolete schema-2 Core selection in which terminal execution was
/// optional and unselected. Its complete passing historical closure is predecessor evidence,
/// never current readiness; boot must freshly probe the now-mandatory execution adapter.
pub fn validate_optional_terminal_cutover_predecessor_v2(
    bytes: &[u8],
) -> Result<ValidatedCutoverPredecessorV1, CutoverPredecessorErrorV1> {
    let manifest: OptionalTerminalManifestV2 =
        serde_json::from_slice(bytes).map_err(|_| CutoverPredecessorErrorV1::InvalidManifest)?;
    if manifest.schema_version != 2 {
        return Err(CutoverErrorV1::UnknownSchemaVersion.into());
    }
    let selected = &manifest.composition.enhancements;
    if manifest.application_instance_id.trim().is_empty()
        || manifest.application_generation == 0
        || manifest.selected_epoch != StartupEpochV1::NewCurrentSchema
        || manifest.composition.profile != RuntimeCompositionProfile::Core
        || selected.contains(&OptionalCapability::Terminal)
        || selected
            .iter()
            .any(|capability| !OPTIONAL_TERMINAL_CAPABILITIES_V2.contains(capability))
        || !selected.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err(CutoverPredecessorErrorV1::InvalidManifest);
    }
    if optional_terminal_manifest_hash(&manifest) != manifest.manifest_hash {
        return Err(CutoverErrorV1::ManifestHashMismatch.into());
    }
    // Freeze the old publisher's closure independently of current required_adapter_kinds_v1.
    let required: BTreeSet<_> = SCHEMA_1_ADAPTERS
        .into_iter()
        .filter(|adapter| match adapter {
            MandatoryAdapterKindV1::ExecutionTerminal => false,
            MandatoryAdapterKindV1::ExecutionExtension => {
                selected.contains(&OptionalCapability::Mcp)
                    || selected.contains(&OptionalCapability::Skills)
            }
            MandatoryAdapterKindV1::StorageMemory => selected.contains(&OptionalCapability::Memory),
            MandatoryAdapterKindV1::ProductStateUpdater
            | MandatoryAdapterKindV1::BorrowedReleaseOutput => {
                selected.contains(&OptionalCapability::Updater)
            }
            _ => true,
        })
        .collect();
    let mut seen = BTreeSet::new();
    for probe in &manifest.mandatory_readiness {
        if !required.contains(&probe.adapter) {
            return Err(CutoverErrorV1::UnexpectedReadinessProbe(probe.adapter).into());
        }
        if !seen.insert(probe.adapter) {
            return Err(CutoverErrorV1::DuplicateReadinessProbe(probe.adapter).into());
        }
        if !probe.passed {
            return Err(CutoverErrorV1::AdapterNotReady(probe.adapter).into());
        }
    }
    if seen != required {
        return Err(CutoverErrorV1::MissingReadinessProbe.into());
    }
    Ok(ValidatedCutoverPredecessorV1 {
        application_instance_id: manifest.application_instance_id,
        application_generation: manifest.application_generation,
        authority_generation_digest: manifest.authority_generation_digest,
        manifest_hash: manifest.manifest_hash,
    })
}

#[cfg(test)]
#[path = "../tests/cutover_predecessor_tests.rs"]
mod tests;
