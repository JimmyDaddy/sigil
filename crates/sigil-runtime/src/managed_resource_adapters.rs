//! RFC-0071 section 9.4 / R71.4: runtime composition of authority-owned adapters (isolated).
//!
//! This module composes the ResourceAuthorityServiceFactoryV1 bundle into consumer-facing
//! semantic adapters: runtime holds only pathless trait objects and coordinates token/event/
//! mutation receipts. It never implements authority services, never holds a private
//! token/primitive/connection lease, and never names authority concrete types. Production
//! cutover to these adapters is R71.6; here they are qualified in an isolated harness.

use std::sync::Arc;

use sigil_kernel::capability_issuer::{KernelCapabilityIssuerV1, VerifiedExecutionBundleViewV1};
use sigil_kernel::managed_execution::ManagedExecutionServiceV1;
use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_projection::ManagedProjectionServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::IssuedExecutionAdmissionBundleV1;
use sigil_resource_authority::factory::ResourceAuthorityServiceBundleV1;

/// Runtime composition snapshot: the only authority-derived surface runtime composes.
#[derive(Clone)]
pub struct RuntimeManagedResourceServicesV1 {
    pub execution: Arc<dyn ManagedExecutionServiceV1>,
    pub file_access: Arc<dyn ManagedFileAccessServiceV1>,
    pub storage: Arc<dyn ManagedStorageServiceV1>,
    pub projection: Arc<dyn ManagedProjectionServiceV1>,
    pub capability_issuer: Arc<dyn KernelCapabilityIssuerV1>,
    /// Actual seam kind behind `execution` (ShadowPlaceholder until the sandbox-backed
    /// managed execution protocol is composed; R71.6 cutover probe reads this truthfully).
    pub execution_seam: crate::r71_global_cutover::RuntimeExecutionSeamV1,
    /// Actual seam kind behind `file_access`.
    pub file_access_seam: crate::r71_global_cutover::RuntimeFileAccessSeamV1,
}

impl RuntimeManagedResourceServicesV1 {
    /// Composes the runtime view from the authority bundle. The bundle is the single source;
    /// the runtime adds only the issued execution / storage ports and the generic issuer.
    pub fn compose(
        bundle: ResourceAuthorityServiceBundleV1,
        capability_issuer: Arc<dyn KernelCapabilityIssuerV1>,
        projection: Arc<dyn ManagedProjectionServiceV1>,
    ) -> Self {
        Self {
            execution: Arc::new(RuntimeManagedExecutionAdapterV1 {
                _issuer: capability_issuer.clone(),
            }),
            file_access: bundle.file_access,
            storage: bundle.storage,
            projection,
            capability_issuer,
            execution_seam: crate::r71_global_cutover::RuntimeExecutionSeamV1::ShadowPlaceholder,
            file_access_seam: crate::r71_global_cutover::RuntimeFileAccessSeamV1::ShadowPlaceholder,
        }
    }

    /// Composes the R71.6 execution surface: the sandbox-owned managed execution service is
    /// the only execution port, and the seal registrar flags the seam as sandbox-backed so
    /// the cutover probe reflects reality.
    pub fn compose_sandbox_backed(
        bundle: ResourceAuthorityServiceBundleV1,
        capability_issuer: Arc<dyn KernelCapabilityIssuerV1>,
        projection: Arc<dyn ManagedProjectionServiceV1>,
        execution: Arc<dyn ManagedExecutionServiceV1>,
        file_access: Arc<dyn ManagedFileAccessServiceV1>,
        file_access_seam: crate::r71_global_cutover::RuntimeFileAccessSeamV1,
    ) -> Self {
        Self {
            execution,
            file_access,
            storage: bundle.storage,
            projection,
            capability_issuer,
            execution_seam: crate::r71_global_cutover::RuntimeExecutionSeamV1::SandboxBacked,
            file_access_seam,
        }
    }
}

/// Placeholder runtime execution adapter: owns the kernel consumer port and coordinates the
/// bundle consumption; actual authority/sandbox protocol in R71.5.
pub struct RuntimeManagedExecutionAdapterV1 {
    _issuer: Arc<dyn KernelCapabilityIssuerV1>,
}

#[async_trait::async_trait]
impl ManagedExecutionServiceV1 for RuntimeManagedExecutionAdapterV1 {
    async fn execute_once(
        &self,
        _bundle: IssuedExecutionAdmissionBundleV1,
        _request: sigil_kernel::managed_execution::ManagedExecutionRequestV1,
    ) -> Result<
        sigil_kernel::managed_execution::ManagedExecutionReceiptV1,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        Err(sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable)
    }

    async fn start_persistent(
        &self,
        _bundle: IssuedExecutionAdmissionBundleV1,
        _request: sigil_kernel::managed_execution::ManagedExecutionRequestV1,
    ) -> Result<
        Box<dyn sigil_kernel::managed_execution::ManagedProcessHandleV1>,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        Err(sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable)
    }
}

/// Runtime-only admission verifier: never constructs or re-signs a capability.
pub fn verify_bundle_view(
    issuer: &dyn KernelCapabilityIssuerV1,
    bundle: IssuedExecutionAdmissionBundleV1,
) -> Result<VerifiedExecutionBundleViewV1, sigil_kernel::process_observation::CapabilityVerifyErrorV1>
{
    issuer.verify_execution_bundle(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_kernel::resource::CanonicalHash;
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };

    #[test]
    fn r71_runtime_compose_holds_only_pathless_ports() {
        let storage = Arc::new(AuthorityManagedStorageServiceV1::new(
            AuthorityStorageGrantTableV1::new(),
            sigil_kernel::resource::AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([1u8; 32]),
            },
        ));
        let file_access = sigil_resource_authority::file_access_stub::stub_file_access_service();
        let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new(
            sigil_kernel::resource::AuthorityGeneration {
                epoch: 2,
                instance_hash: CanonicalHash::from_bytes([2u8; 32]),
            },
            storage,
            file_access,
        )
        .build_bundle();
        let composed = RuntimeManagedResourceServicesV1::compose(
            bundle,
            sigil_kernel::capability_issuer::mock_issuer(),
            Arc::new(StubProjectionServiceV1),
        );
        // The runtime view is fully trait-object-y: no concrete authority type escapes.
        let _ = composed.file_access;
        let _ = composed.storage;
        let _ = composed.projection;
    }

    /// Minimal projection stub for the isolated composition test.
    struct StubProjectionServiceV1;

    #[async_trait::async_trait]
    impl ManagedProjectionServiceV1 for StubProjectionServiceV1 {
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
}
