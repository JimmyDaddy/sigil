//! RFC-0071 section 18 R71.6: production authority composition spine.
//!
//! The only place a boot surface turns verified bootstrap anchors + declared writer channels
//! into the composed runtime surface (services, storage writer adapter, authority-backed file
//! access). Declaring a writer channel registers exactly its grant: the cutover probe then
//! reflects what is composed and nothing more. Real authority services only - no stub in the
//! production path (the capability issuer, planner and projection facade are host-injected
//! because their production construction belongs to kernel/boot owners).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use sigil_kernel::capability_issuer::KernelCapabilityIssuerV1;
use sigil_kernel::managed_execution::ManagedExecutionPlannerV1;
use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_projection::ManagedProjectionServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash, ResourceJournalScopeV1};

use crate::managed_resource_adapters::RuntimeManagedResourceServicesV1;
use crate::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, StorageWriterChannelV1, grant_for_channel,
};

/// Composed runtime authority surface (everything a new-epoch boot needs once).
pub struct RuntimeAuthorityCompositionV1 {
    pub services: RuntimeManagedResourceServicesV1,
    pub storage_writer: ManagedStorageWriterAdapterV1,
    pub declared_channels: BTreeSet<StorageWriterChannelV1>,
}

/// Closed composition error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeAuthorityCompositionErrorV1 {
    #[error("bootstrap anchor validation failed: {0}")]
    AnchorInvalid(String),
    #[error("declared writer grant failed: {0}")]
    GrantDeclared(String),
}

/// Composes the R71.6 authority surface from verified anchors and declared channels.
///
/// `state_anchor` / `execution_temp_root` must already exist as owner-only dirs (the boot
/// owner resolves them through the authority bootstrap resolver; this function re-validates
/// via [validate anchors]). `planner` and `issuer` are host-injected (kernel/boot owned); the
/// projection facade is the runtime transitional edge.
#[allow(clippy::too_many_arguments)]
pub fn compose_runtime_authority(
    state_anchor: &Path,
    execution_temp_root: &Path,
    cutover_manifest_hash: CanonicalHash,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    projection: Arc<dyn ManagedProjectionServiceV1>,
    declared: &[StorageWriterChannelV1],
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    // The real kernel capability broker is the single issuer for this composition: execution
    // bundles and storage admission capabilities are broker-issued (one-shot proofs), never
    // fabricated by consumers.
    let broker =
        std::sync::Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new());
    let bootstrap = sigil_resource_authority::bootstrap::AuthorityBootstrapRoots {
        state_anchor: state_anchor.to_path_buf(),
        cache_anchor: state_anchor.join("cache"),
        execution_temp_anchor: execution_temp_root.to_path_buf(),
        state_identity: CanonicalHash::from_bytes([0x71; 32]),
        cache_identity: CanonicalHash::from_bytes([0x72; 32]),
        execution_temp_identity: CanonicalHash::from_bytes([0x73; 32]),
        manifest_hash: cutover_manifest_hash,
        journal_instance_hash: CanonicalHash::from_bytes([0x74; 32]),
    };
    bootstrap
        .validate_anchors()
        .map_err(|error| RuntimeAuthorityCompositionErrorV1::AnchorInvalid(error.to_string()))?;

    let authority = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x75; 32]),
    };
    let mut table = sigil_resource_authority::storage::AuthorityStorageGrantTableV1::new();
    for channel in declared {
        let grant = grant_for_channel(*channel, 0x76);
        table.register(grant).map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
        })?;
    }
    let storage: Arc<dyn ManagedStorageServiceV1> = Arc::new(
        sigil_resource_authority::storage::AuthorityManagedStorageServiceV1::new(table, authority),
    );
    let registry = Arc::new(std::sync::Mutex::new(
        sigil_resource_authority::borrowed::BorrowedSubjectRegistryV1::new(),
    ));
    let file_access: Arc<dyn ManagedFileAccessServiceV1> = Arc::new(
        sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1::new(registry),
    );
    let execution: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionServiceV1> = Arc::new(
        sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            planner,
            execution_temp_root.to_path_buf(),
        ),
    );
    let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new(
        authority,
        storage.clone() as Arc<dyn ManagedStorageServiceV1>,
        file_access.clone() as Arc<dyn ManagedFileAccessServiceV1>,
    )
    .build_bundle();
    let services = RuntimeManagedResourceServicesV1::compose_sandbox_backed(
        bundle,
        broker.clone() as Arc<dyn KernelCapabilityIssuerV1>,
        projection,
        execution,
        file_access,
        crate::r71_global_cutover::RuntimeFileAccessSeamV1::AuthorityBacked,
    );
    let storage_writer = ManagedStorageWriterAdapterV1::with_storage_issuer(
        storage,
        state_anchor.to_path_buf(),
        cutover_manifest_hash,
        broker,
    );
    Ok(RuntimeAuthorityCompositionV1 {
        services,
        storage_writer,
        declared_channels: declared.iter().copied().collect(),
    })
}

/// Convenience: authoritative resource journal scope for the composition (application-level).
pub fn composition_journal_scope() -> ResourceJournalScopeV1 {
    ResourceJournalScopeV1::Application
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r71_global_cutover::{
        RuntimeExecutionSeamV1, RuntimeFileAccessSeamV1, probe_mandatory_adapters,
    };
    use crate::resource_recovery_surface::RuntimeResourceRecoveryFacadeV1;
    use sigil_kernel::cutover_manifest::MandatoryAdapterKindV1;

    struct CompositionStubProjectionServiceV1;

    #[async_trait::async_trait]
    impl ManagedProjectionServiceV1 for CompositionStubProjectionServiceV1 {
        async fn open_rebuildable_projection(
            &self,
            _handle: &sigil_kernel::managed_storage::ManagedStorageNamespaceHandleV1,
            _request: sigil_kernel::managed_projection::OpenProjectionConnectionRequestV1,
        ) -> Result<
            Box<dyn sigil_kernel::managed_projection::ManagedProjectionConnectionV1>,
            sigil_kernel::managed_projection::ProjectionErrorV1,
        > {
            Err(sigil_kernel::managed_projection::ProjectionErrorV1::ConnectionClosed)
        }
    }

    #[test]
    fn r71_composition_declared_channel_writes_and_probes_exactly() {
        use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        let exec = dir.path().join("exec");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::create_dir_all(state.join("cache")).expect("cache dir");
        std::fs::create_dir_all(&exec).expect("exec dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("mode");
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let projection: Arc<dyn sigil_kernel::managed_projection::ManagedProjectionServiceV1> =
            Arc::new(CompositionStubProjectionServiceV1);
        let composition = crate::r71_authority_composition::compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x55; 32]),
            planner,
            projection,
            &[Ch::SessionLog],
        )
        .expect("compose");
        let lease = composition
            .storage_writer
            .acquire(Ch::SessionLog)
            .expect("acquire");
        composition
            .storage_writer
            .write_record(&lease, b"seq=1")
            .expect("write");
        composition
            .storage_writer
            .finalize(lease)
            .expect("finalize");
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
        let probes = probe_mandatory_adapters(
            &composition.services,
            &recovery,
            CanonicalHash::from_bytes([0x56; 32]),
            1,
        );
        let session_log = probes
            .iter()
            .find(|p| p.adapter == MandatoryAdapterKindV1::StorageSessionLog)
            .expect("session log probe");
        assert!(session_log.passed);
        let input_history = probes
            .iter()
            .find(|p| p.adapter == MandatoryAdapterKindV1::StorageInputHistory)
            .expect("input history probe");
        assert!(!input_history.passed);
        assert!(matches!(
            composition.services.execution_seam,
            RuntimeExecutionSeamV1::SandboxBacked
        ));
        assert!(matches!(
            composition.services.file_access_seam,
            RuntimeFileAccessSeamV1::AuthorityBacked
        ));
    }
}
