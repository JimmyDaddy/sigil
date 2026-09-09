//! Versioned bootstrap pointer validation. Historical bytes establish only the predecessor
//! identity for fixed-forward publication; current readiness is always recomputed by boot.

use sigil_kernel::cutover_manifest::{
    CUTOVER_MANIFEST_SCHEMA_VERSION, CutoverManifestV1, StartupEpochV1,
    ValidatedCutoverPredecessorV1, validate_bootstrap_cutover_predecessor_v1,
};
use sigil_resource_authority::bootstrap::{
    AuthorityBootstrapObjectClassV1, AuthorityBootstrapPublicationGuard, AuthorityBootstrapStoreV1,
    BootstrapErrorV1,
};

use super::{AuthorityConfigGenerationRecordV1, BootAuthorityErrorV1};

pub(super) enum ExistingBootPointerV1 {
    Schema1(ValidatedCutoverPredecessorV1),
    Current(CutoverManifestV1),
}

impl ExistingBootPointerV1 {
    pub fn application_generation(&self) -> u64 {
        match self {
            Self::Schema1(predecessor) => predecessor.application_generation(),
            Self::Current(manifest) => manifest.application_generation,
        }
    }
}

fn invalid_pointer(message: impl std::fmt::Display) -> BootAuthorityErrorV1 {
    BootAuthorityErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(format!(
        "cutover pointer validation failed: {message}"
    )))
}

/// Must run before inventory initialization or configuration-generation publication while
/// holding the same authority publication lock used for the eventual pointer replacement.
pub(super) fn load_validated_boot_pointer(
    bootstrap: &AuthorityBootstrapStoreV1,
    publication: &AuthorityBootstrapPublicationGuard,
    expected_instance: &str,
    configuration: Option<&AuthorityConfigGenerationRecordV1>,
) -> Result<Option<ExistingBootPointerV1>, BootAuthorityErrorV1> {
    let Some(bytes) = bootstrap
        .read_bytes(publication, AuthorityBootstrapObjectClassV1::CutoverPointer)
        .map_err(BootAuthorityErrorV1::Bootstrap)?
    else {
        return Ok(None);
    };
    #[derive(serde::Deserialize)]
    struct Header {
        schema_version: u32,
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(invalid_pointer)?;
    // Decode each schema directly from the original bytes. A serde_json::Value round trip
    // would erase duplicate JSON fields before the version-specific decoder could reject them.
    let pointer = match header.schema_version {
        1 => ExistingBootPointerV1::Schema1(
            validate_bootstrap_cutover_predecessor_v1(&bytes).map_err(invalid_pointer)?,
        ),
        CUTOVER_MANIFEST_SCHEMA_VERSION => ExistingBootPointerV1::Current(
            crate::r71_global_cutover::RuntimeGlobalCutoverV1::validate_manifest_bytes(&bytes)
                .map_err(invalid_pointer)?,
        ),
        _ => return Err(invalid_pointer("unknown manifest schema version")),
    };
    let (instance, authority_digest) = match &pointer {
        ExistingBootPointerV1::Schema1(predecessor) => (
            predecessor.application_instance_id(),
            predecessor.authority_generation_digest(),
        ),
        ExistingBootPointerV1::Current(manifest) => {
            if manifest.selected_epoch != StartupEpochV1::NewCurrentSchema {
                return Err(invalid_pointer(
                    "historical Legacy epoch cannot authorize startup",
                ));
            }
            (
                manifest.application_instance_id.as_str(),
                manifest.authority_generation_digest,
            )
        }
    };
    if instance != expected_instance {
        return Err(invalid_pointer(
            "application instance does not match this boot owner",
        ));
    }
    if authority_digest
        != bootstrap
            .authority_instance_hash(publication)
            .map_err(BootAuthorityErrorV1::Bootstrap)?
    {
        return Err(invalid_pointer(
            "authority generation does not match this bootstrap",
        ));
    }
    let configuration =
        configuration.ok_or_else(|| invalid_pointer("configuration generation is missing"))?;
    if pointer.application_generation() == 0
        || pointer.application_generation() > configuration.generation
    {
        return Err(invalid_pointer(
            "manifest generation is ahead of configuration metadata",
        ));
    }
    Ok(Some(pointer))
}
