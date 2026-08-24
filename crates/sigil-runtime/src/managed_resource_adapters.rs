//! RFC-0071 section 9.4 / R71.4: runtime composition of authority-owned adapters (isolated).
//!
//! This module composes the ResourceAuthorityServiceFactoryV1 bundle into consumer-facing
//! semantic adapters: runtime holds only pathless trait objects and coordinates token/event/
//! mutation receipts. It never implements authority services, never holds a private
//! token/primitive/connection lease, and never names authority concrete types. The production
//! R71.6 composition also owns the plan-bound managed extension route; shadow composition keeps
//! that route absent and therefore remains fail-closed.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sigil_kernel::capability_issuer::{KernelCapabilityIssuerV1, VerifiedExecutionBundleViewV1};
use sigil_kernel::managed_execution::{
    CaptureModeV1, ExecutionCapturePolicy, ExecutionPurposeV1, ExecutionResourceLimits,
    ManagedExecutionPlanRequestV1, ManagedExecutionRequestV1, ManagedExecutionServiceV1,
};
use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_projection::ManagedProjectionServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::{
    CanonicalHash, IssuedExecutionAdmissionBundleV1, OpaqueAdmissionId, OpaquePermissionSubjectRef,
    PhysicalAttemptId, RequestedEnforcementV1, ResourceAccessV1, ResourceOwnerScopeV1,
    SandboxBackendClassV1,
};
use sigil_resource_authority::factory::ResourceAuthorityServiceBundleV1;
use sigil_sandbox::managed::ManagedExtensionLaunchEnforcementV1;

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
    /// Actual extension (MCP / plugin) launch seam. Shadow compositions remain legacy; the
    /// authority composition marks this managed only when the real route is injected.
    pub extension_execution_seam: crate::r71_global_cutover::RuntimeExecutionExtensionSeamV1,
    /// Actual seam kind behind `file_access`.
    pub file_access_seam: crate::r71_global_cutover::RuntimeFileAccessSeamV1,
    /// True when the composed projection port is the production records-backed rebuildable
    /// projection service (R71.6 probe reads this truthfully).
    pub projection_backed: bool,
    /// Real runtime route for MCP/plugin extension plans. It remains absent on isolated shadow
    /// compositions, so the extension readiness probe cannot be toggled independently of the
    /// launch path.
    pub extension_execution: Option<Arc<RuntimeManagedExtensionExecutionRouteV1>>,
    /// Product-plane updater owner. It is deliberately independent from agent storage grants;
    /// the product-state probe passes only when this real owner is attached.
    pub product_updater: Option<Arc<sigil_updater::ProductUpdaterState>>,
    /// Whether the desktop product-state updater is attached to its real owner route.
    pub product_state_updater_seam: crate::r71_global_cutover::RuntimeProductStateSeamV1,
    /// Whether native support-save is attached to its host-private registration route.
    pub borrowed_native_save_seam: crate::r71_global_cutover::RuntimeProductStateSeamV1,
    /// Whether configuration mutation is attached to its host-private registration route.
    pub borrowed_configuration_seam: crate::r71_global_cutover::RuntimeProductStateSeamV1,
    /// Whether release output is attached to its host-private registration route.
    pub borrowed_release_output_seam: crate::r71_global_cutover::RuntimeProductStateSeamV1,
}

/// Runtime-owned coordinator for one managed extension launch. It binds a resolved stdio plan to
/// the same planner and kernel broker used by the composed application, then injects a
/// plan-bound, real sandbox command launcher for the Extension purpose.
#[derive(Clone)]
pub struct RuntimeManagedExtensionExecutionRouteV1 {
    planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1>,
    broker: Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>,
    execution_temp_root: PathBuf,
}

impl std::fmt::Debug for RuntimeManagedExtensionExecutionRouteV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeManagedExtensionExecutionRouteV1")
            .field("execution_temp_root", &"[hidden]")
            .finish_non_exhaustive()
    }
}

impl RuntimeManagedExtensionExecutionRouteV1 {
    pub fn new(
        planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1>,
        broker: Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>,
        execution_temp_root: PathBuf,
    ) -> Self {
        Self {
            planner,
            broker,
            execution_temp_root,
        }
    }

    /// Starts one resolved long-lived extension process through the Extension broker bundle and
    /// sandbox service. The path-bearing plan never crosses the kernel port; it is consumed by the
    /// sandbox-private command launcher only.
    pub async fn start_persistent(
        &self,
        server_name: &str,
        plan: sigil_tools_builtin::LongLivedStdioProcessPlan,
    ) -> Result<
        Box<dyn sigil_kernel::managed_execution::ManagedProcessHandleV1>,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        let argv = std::iter::once(plan.program.as_os_str().to_os_string())
            .chain(plan.args.iter().cloned())
            .collect::<Vec<_>>();
        let environment = plan
            .environment
            .variables()
            .map(|(key, value)| (OsString::from(key), OsString::from(value.expose_secret())))
            .collect::<Vec<_>>();
        let structured_command_digest =
            extension_command_digest(&plan.program, &plan.args, &plan.cwd);
        let cwd_subject_ref = OpaquePermissionSubjectRef::new(format!(
            "extension-cwd:{}",
            extension_path_digest(&plan.cwd).to_hex()
        ));
        let capture = ExecutionCapturePolicy {
            stdout_capture: CaptureModeV1::BoundedRing {
                max_bytes: 8 * 1024 * 1024,
            },
            stderr_capture: CaptureModeV1::BoundedRing {
                max_bytes: 8 * 1024 * 1024,
            },
            pty: false,
        };
        let limits = ExecutionResourceLimits {
            max_output_bytes: 8 * 1024 * 1024,
            max_runtime_ms: 24 * 60 * 60 * 1000,
            max_children: 64,
            max_fds: 1024,
            pty_required: false,
        };
        let plan_request = ManagedExecutionPlanRequestV1 {
            argv: argv.clone(),
            cwd_subject_ref: cwd_subject_ref.clone(),
            purpose: ExecutionPurposeV1::ExtensionProcess,
            structured_command_digest,
            owner_scope: ResourceOwnerScopeV1::Application,
            capture: capture.clone(),
            limits: limits.clone(),
            environment: environment.clone(),
        };
        let draft = self.planner.plan_execution(plan_request).map_err(|_| {
            sigil_kernel::managed_execution::ManagedExecutionErrorV1::ExecutionPlanDrift
        })?;
        let attempt_id = PhysicalAttemptId::new(format!(
            "mcp-extension:{server_name}:{}",
            uuid::Uuid::new_v4()
        ));
        let request = ManagedExecutionRequestV1 {
            argv,
            cwd_subject_ref,
            structured_command_digest,
            admission_ref: OpaqueAdmissionId::new(attempt_id.as_str().to_owned()),
            execution_plan_draft_hash: draft.draft_hash,
            environment_profile: draft.environment_profile.clone(),
            capture,
            limits,
            environment: environment.clone(),
        };
        let proof = self.broker.seal_execution_proof(
            sigil_kernel::capability_issuer::ProofKindV1::ExecutionExtension,
            "extension-process",
            attempt_id.as_str().as_bytes().to_vec(),
        );
        let bundle = self.broker.issue_execution(proof).map_err(|_| {
            sigil_kernel::managed_execution::ManagedExecutionErrorV1::AdmissionMismatch
        })?;
        let enforcement = extension_enforcement(&plan, draft.environment_profile.profile_hash);
        let launcher = Arc::new(
            sigil_sandbox::managed::CommandManagedExtensionLaunchServiceV1::new(
                plan.program,
                plan.args,
                plan.cwd,
                environment,
                enforcement,
            ),
        );
        let service = sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&self.planner),
            self.execution_temp_root.clone(),
        )
        .with_extension_launcher(launcher);
        service.start_persistent(bundle, request).await
    }
}

fn extension_path_digest(path: &Path) -> CanonicalHash {
    crate::r71_shadow_planner::canonical_digest(path.to_string_lossy().as_bytes())
}

fn extension_command_digest(program: &Path, args: &[OsString], cwd: &Path) -> CanonicalHash {
    let mut bytes = b"managed-extension-command-v1\0".to_vec();
    bytes.extend_from_slice(program.to_string_lossy().as_bytes());
    bytes.push(0);
    for arg in args {
        bytes.extend_from_slice(arg.as_os_str().as_encoded_bytes());
        bytes.push(0);
    }
    bytes.extend_from_slice(cwd.to_string_lossy().as_bytes());
    crate::r71_shadow_planner::canonical_digest(&bytes)
}

fn extension_enforcement(
    plan: &sigil_tools_builtin::LongLivedStdioProcessPlan,
    profile_hash: CanonicalHash,
) -> sigil_sandbox::managed::ManagedExtensionLaunchEnforcementV1 {
    let capability_hash = crate::r71_shadow_planner::canonical_digest(
        format!("{:?}", plan.backend_capabilities).as_bytes(),
    );
    let (backend, completeness, requirement, flags) = if plan.sandboxed {
        let backend = match plan.backend {
            sigil_kernel::ExecutionBackendKind::MacosSeatbelt => {
                SandboxBackendClassV1::MacOsSeatbelt
            }
            sigil_kernel::ExecutionBackendKind::LinuxBubblewrap => {
                SandboxBackendClassV1::LinuxBubblewrap
            }
            sigil_kernel::ExecutionBackendKind::Docker => SandboxBackendClassV1::Docker,
            sigil_kernel::ExecutionBackendKind::Local => SandboxBackendClassV1::LocalUnconfined,
        };
        (
            backend,
            sigil_kernel::resource::EnforcementCompletenessV1::Exact,
            sigil_kernel::resource::EnforcementRequirementClassV1::RequiredExact,
            true,
        )
    } else {
        (
            SandboxBackendClassV1::LocalUnconfined,
            sigil_kernel::resource::EnforcementCompletenessV1::None,
            sigil_kernel::resource::EnforcementRequirementClassV1::ExplicitUnconfined,
            false,
        )
    };
    ManagedExtensionLaunchEnforcementV1 {
        requested: RequestedEnforcementV1 {
            requirement,
            deny_ambient_system_temp_write: flags,
            deny_ambient_home_write: flags,
            deny_ungranted_workspace_write: flags,
            require_process_tree_ownership: flags,
            require_network_policy: flags,
            requested_capability_set_hash: if flags {
                capability_hash
            } else {
                CanonicalHash::from_bytes([0u8; 32])
            },
            profile_hash,
        },
        backend,
        completeness,
        effective_access: BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]),
        effective_capability_set_hash: capability_hash,
        proof_set_hash: crate::r71_shadow_planner::canonical_digest(
            b"managed-extension-launcher-proof-v1",
        ),
    }
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
            extension_execution_seam:
                crate::r71_global_cutover::RuntimeExecutionExtensionSeamV1::LegacyLauncher,
            file_access_seam: crate::r71_global_cutover::RuntimeFileAccessSeamV1::ShadowPlaceholder,
            projection_backed: false,
            extension_execution: None,
            product_updater: None,
            product_state_updater_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_native_save_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_configuration_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_release_output_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
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
            // Extension processes keep the legacy launcher until the managed route is
            // composed for them (the cutover probe stays honestly red).
            extension_execution_seam:
                crate::r71_global_cutover::RuntimeExecutionExtensionSeamV1::LegacyLauncher,
            file_access_seam,
            projection_backed: true,
            extension_execution: None,
            product_updater: None,
            product_state_updater_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_native_save_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_configuration_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_release_output_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compose_sandbox_backed_with_extension_execution(
        bundle: ResourceAuthorityServiceBundleV1,
        capability_issuer: Arc<dyn KernelCapabilityIssuerV1>,
        projection: Arc<dyn ManagedProjectionServiceV1>,
        execution: Arc<dyn ManagedExecutionServiceV1>,
        file_access: Arc<dyn ManagedFileAccessServiceV1>,
        file_access_seam: crate::r71_global_cutover::RuntimeFileAccessSeamV1,
        extension_execution: Arc<RuntimeManagedExtensionExecutionRouteV1>,
    ) -> Self {
        let mut composed = Self::compose_sandbox_backed(
            bundle,
            capability_issuer,
            projection,
            execution,
            file_access,
            file_access_seam,
        );
        composed.extension_execution_seam =
            crate::r71_global_cutover::RuntimeExecutionExtensionSeamV1::ManagedExecutionBacked;
        composed.extension_execution = Some(extension_execution);
        composed
    }

    /// Attaches the transport-neutral product updater owner to the composed surface.
    ///
    /// This is a product-plane owner attachment, not an agent Resource Authority grant.
    #[must_use]
    pub fn with_product_updater(
        self,
        product_updater: Arc<sigil_updater::ProductUpdaterState>,
    ) -> Self {
        self.with_optional_product_updater(Some(product_updater))
    }

    /// Keeps isolated/shadow compositions truthful when no product-plane owner is available.
    #[must_use]
    pub fn with_optional_product_updater(
        mut self,
        product_updater: Option<Arc<sigil_updater::ProductUpdaterState>>,
    ) -> Self {
        self.product_state_updater_seam = if product_updater.is_some() {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::ProductOwnerAtomicBacked
        } else {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter
        };
        self.product_updater = product_updater;
        self
    }
}

/// Shadow runtime execution adapter: owns the kernel consumer port and coordinates the
/// bundle consumption; production composition injects the sandbox-backed implementation.
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
