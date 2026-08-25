//! RFC-0071 section 9.4 / R71.4: runtime composition of authority-owned adapters (isolated).
//!
//! This module composes the ResourceAuthorityServiceFactoryV1 bundle into consumer-facing
//! semantic adapters: the exported resource-service view holds only pathless trait objects and
//! coordinates token/event/mutation receipts. Runtime never implements authority services or
//! holds a private token/primitive/connection lease. Its host-private execution coordinators do
//! own the concrete authority allocator that provisions and retires per-attempt temp generations;
//! that concrete type does not escape through a consumer-facing API. The production R71.6
//! composition also owns the plan-bound managed extension route; shadow composition keeps that
//! route absent and therefore remains fail-closed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use sigil_kernel::capability_issuer::{KernelCapabilityIssuerV1, VerifiedExecutionBundleViewV1};
use sigil_kernel::managed_execution::{
    BoundedProcessInputV1, CaptureModeV1, ExecutionCapturePolicy, ExecutionPurposeV1,
    ExecutionResourceLimits, ManagedExecutionPlanRequestV1, ManagedExecutionRequestV1,
    ManagedExecutionServiceV1, ManagedProcessHandleV1, ManagedProcessOutputChannelV1,
    ProcessCancelReasonV1,
};
use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_projection::ManagedProjectionServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::{
    CanonicalHash, IssuedExecutionAdmissionBundleV1, OpaqueAdmissionId, OpaquePermissionSubjectRef,
    PhysicalAttemptId, RequestedEnforcementV1, ResourceAccessV1, ResourceOwnerScopeV1,
    SandboxBackendClassV1,
};
use sigil_kernel::{
    EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionCaptureOutcome, ExecutionCleanupReceipt, ExecutionNetworkReceipt,
    ExecutionOutputReceipt, ExecutionOutputStream, ExecutionReceipt, ExecutionRequest,
    ExecutionResourceReceipt, ExecutionStreamCapture, ExecutionTerminationCause,
    ToolOutputStreamV1,
};
use sigil_resource_authority::arena::{ExecutionTempAuthorityV1, ExecutionTempGenerationV1};
use sigil_resource_authority::factory::ResourceAuthorityServiceBundleV1;
use sigil_sandbox::managed::ManagedExtensionLaunchEnforcementV1;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    /// Host-private borrowed native-save authority port. It is present only when the real
    /// registration capsule route is composed.
    pub borrowed_native_save:
        Option<Arc<dyn sigil_resource_authority::native_save::BorrowedNativeSaveServiceV1>>,
    /// Host-private borrowed configuration owner for the server's root config.
    pub borrowed_configuration:
        Option<Arc<dyn sigil_resource_authority::configuration::BorrowedConfigurationServiceV1>>,
    /// Nonshipping release-owner file/tree service. It is attached only by release qualification
    /// composition; normal shipping boot does not expose a release output writer.
    pub borrowed_release_output:
        Option<Arc<dyn sigil_resource_authority::release_output::BorrowedReleaseOutputServiceV1>>,
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
    execution_temp_authority: Arc<ExecutionTempAuthorityV1>,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
}

impl std::fmt::Debug for RuntimeManagedExtensionExecutionRouteV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeManagedExtensionExecutionRouteV1")
            .field("execution_temp_authority", &"[hidden]")
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
            execution_temp_authority: Arc::new(ExecutionTempAuthorityV1::new(execution_temp_root)),
            authority_generation: sigil_kernel::resource::AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([0x75; 32]),
            },
        }
    }

    #[must_use]
    pub fn with_authority_generation(
        mut self,
        authority_generation: sigil_kernel::resource::AuthorityGeneration,
    ) -> Self {
        self.authority_generation = authority_generation;
        self
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
        let prepared =
            self.prepare_persistent_with_options(server_name, plan, BTreeMap::new(), None, None)?;
        self.launch_prepared(prepared, None).await
    }

    fn prepare_persistent_with_options(
        &self,
        subject: &str,
        plan: sigil_tools_builtin::LongLivedStdioProcessPlan,
        additional_environment: BTreeMap<String, String>,
        max_runtime_ms: Option<u64>,
        max_output_bytes: Option<u64>,
    ) -> Result<
        PreparedManagedExtensionLaunchV1,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        let argv = std::iter::once(plan.program.as_os_str().to_os_string())
            .chain(plan.args.iter().cloned())
            .collect::<Vec<_>>();
        let mut environment = plan
            .environment
            .variables()
            .map(|(key, value)| (OsString::from(key), OsString::from(value.expose_secret())))
            .collect::<Vec<_>>();
        let mut environment_names = environment
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        for (name, value) in additional_environment {
            let name = OsString::from(name);
            if !environment_names.insert(name.clone()) {
                return Err(
                    sigil_kernel::managed_execution::ManagedExecutionErrorV1::AdmissionMismatch,
                );
            }
            environment.push((name, OsString::from(value)));
        }
        let structured_command_digest =
            extension_command_digest(&plan.program, &plan.args, &plan.cwd);
        let cwd_subject_ref = OpaquePermissionSubjectRef::new(format!(
            "extension-cwd:{}",
            extension_path_digest(&plan.cwd).to_hex()
        ));
        let max_output_bytes = max_output_bytes.unwrap_or(8 * 1024 * 1024);
        let capture = ExecutionCapturePolicy {
            stdout_capture: CaptureModeV1::BoundedRing {
                max_bytes: max_output_bytes,
            },
            stderr_capture: CaptureModeV1::BoundedRing {
                max_bytes: max_output_bytes,
            },
            pty: false,
        };
        let limits = ExecutionResourceLimits {
            max_output_bytes,
            max_runtime_ms: max_runtime_ms.unwrap_or(24 * 60 * 60 * 1000),
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
        let attempt_id =
            PhysicalAttemptId::new(format!("extension:{subject}:{}", uuid::Uuid::new_v4()));
        let request = ManagedExecutionRequestV1 {
            argv,
            cwd_subject_ref,
            structured_command_digest,
            admission_ref: OpaqueAdmissionId::new(attempt_id.as_str().to_owned()),
            execution_plan_draft_hash: draft.draft_hash,
            environment_profile: draft.environment_profile.clone(),
            capture,
            limits,
            pty_size: None,
            environment: environment.clone(),
        };
        let enforcement = extension_enforcement(&plan, draft.environment_profile.profile_hash);
        Ok(PreparedManagedExtensionLaunchV1 {
            plan,
            environment,
            draft,
            request,
            attempt_id,
            enforcement,
        })
    }

    async fn launch_prepared(
        &self,
        prepared: PreparedManagedExtensionLaunchV1,
        extension_admission: Option<
            &sigil_kernel::extension_admission::ExtensionProcessAdmissionV1,
        >,
    ) -> Result<
        Box<dyn sigil_kernel::managed_execution::ManagedProcessHandleV1>,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        let proof = if let Some(admission) = extension_admission {
            if admission.physical_attempt_id != prepared.attempt_id
                || admission.execution_plan_draft_hash != prepared.draft.draft_hash
                || admission.resource_plan_hash != prepared.draft.resource_plan_hash
            {
                return Err(
                    sigil_kernel::managed_execution::ManagedExecutionErrorV1::AdmissionMismatch,
                );
            }
            self.broker.seal_extension_execution_proof(admission)
        } else {
            self.broker.seal_execution_proof(
                sigil_kernel::capability_issuer::ProofKindV1::ExecutionExtension,
                "extension-process",
                prepared.attempt_id.as_str().as_bytes().to_vec(),
            )
        };
        let bundle = self.broker.issue_execution(proof).map_err(|_| {
            sigil_kernel::managed_execution::ManagedExecutionErrorV1::AdmissionMismatch
        })?;
        let execution_temp = self
            .execution_temp_authority
            .provision(prepared.attempt_id.as_str(), 1)
            .map_err(|_| {
                sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable
            })?;
        let launcher = Arc::new(
            sigil_sandbox::managed::CommandManagedExtensionLaunchServiceV1::new(
                prepared.plan.program,
                prepared.plan.args,
                prepared.plan.cwd,
                prepared.environment,
                prepared.enforcement,
            ),
        );
        let service = sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&self.planner),
            execution_temp.binding().root.clone(),
        )
        .with_extension_launcher(launcher);
        wrap_persistent_launch(
            service.start_persistent(bundle, prepared.request).await,
            execution_temp,
        )
    }
}

struct PreparedManagedExtensionLaunchV1 {
    plan: sigil_tools_builtin::LongLivedStdioProcessPlan,
    environment: Vec<(OsString, OsString)>,
    draft: sigil_kernel::managed_execution::ManagedExecutionPlanDraftV1,
    request: ManagedExecutionRequestV1,
    attempt_id: PhysicalAttemptId,
    enforcement: ManagedExtensionLaunchEnforcementV1,
}

/// Keeps one authority-owned ExecutionTemp generation alive for exactly the persistent process
/// lifetime and settles it before the terminal receipt becomes observable.
struct RuntimeManagedProcessWithExecutionTempV1 {
    inner: Box<dyn ManagedProcessHandleV1>,
    execution_temp: ExecutionTempGenerationV1,
}

fn wrap_persistent_launch(
    result: Result<
        Box<dyn ManagedProcessHandleV1>,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    >,
    execution_temp: ExecutionTempGenerationV1,
) -> Result<Box<dyn ManagedProcessHandleV1>, sigil_kernel::managed_execution::ManagedExecutionErrorV1>
{
    match result {
        Ok(inner) => Ok(Box::new(RuntimeManagedProcessWithExecutionTempV1 {
            inner,
            execution_temp,
        })),
        Err(error) => {
            let _ = execution_temp.finalize();
            Err(error)
        }
    }
}

#[async_trait::async_trait]
impl ManagedProcessHandleV1 for RuntimeManagedProcessWithExecutionTempV1 {
    fn process_ref(&self) -> sigil_kernel::resource::ReflectiveOpaqueProcessRef {
        self.inner.process_ref()
    }

    fn physical_attempt_id(&self) -> PhysicalAttemptId {
        self.inner.physical_attempt_id()
    }

    fn take_output_stream(
        &mut self,
    ) -> Result<
        Box<dyn sigil_kernel::managed_execution::ManagedProcessOutputStreamV1>,
        sigil_kernel::managed_execution::ManagedProcessControlErrorV1,
    > {
        self.inner.take_output_stream()
    }

    async fn write_stdin(
        &mut self,
        input: BoundedProcessInputV1,
    ) -> Result<
        sigil_kernel::managed_execution::ProcessControlReceiptV1,
        sigil_kernel::managed_execution::ManagedProcessControlErrorV1,
    > {
        self.inner.write_stdin(input).await
    }

    async fn resize_pty(
        &mut self,
        size: sigil_kernel::managed_execution::BoundedPtySizeV1,
    ) -> Result<
        sigil_kernel::managed_execution::ProcessControlReceiptV1,
        sigil_kernel::managed_execution::ManagedProcessControlErrorV1,
    > {
        self.inner.resize_pty(size).await
    }

    async fn close_stdin(
        &mut self,
    ) -> Result<
        sigil_kernel::managed_execution::ProcessControlReceiptV1,
        sigil_kernel::managed_execution::ManagedProcessControlErrorV1,
    > {
        self.inner.close_stdin().await
    }

    async fn cancel(
        &mut self,
        reason: ProcessCancelReasonV1,
    ) -> Result<
        sigil_kernel::managed_execution::ProcessControlReceiptV1,
        sigil_kernel::managed_execution::ManagedProcessControlErrorV1,
    > {
        self.inner.cancel(reason).await
    }

    async fn wait_and_finalize(
        self: Box<Self>,
    ) -> Result<
        sigil_kernel::managed_execution::ManagedExecutionReceiptV1,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        let Self {
            inner,
            execution_temp,
        } = *self;
        match inner.wait_and_finalize().await {
            Ok(mut receipt) => {
                receipt.resources.cleanup_status = finalize_execution_temp(execution_temp);
                Ok(receipt)
            }
            Err(error) => {
                let _ = execution_temp.finalize();
                Err(error)
            }
        }
    }
}

fn finalize_execution_temp(
    execution_temp: ExecutionTempGenerationV1,
) -> sigil_kernel::resource::ResourceCleanupStatusV1 {
    match execution_temp.finalize() {
        Ok(()) => sigil_kernel::resource::ResourceCleanupStatusV1::Released,
        Err(error) => sigil_kernel::resource::ResourceCleanupStatusV1::CleanupIncomplete {
            evidence_digest: crate::r71_shadow_planner::canonical_digest(
                error.to_string().as_bytes(),
            ),
        },
    }
}

fn execution_cleanup_receipt(
    status: &sigil_kernel::resource::ResourceCleanupStatusV1,
) -> ExecutionCleanupReceipt {
    match status {
        sigil_kernel::resource::ResourceCleanupStatusV1::Released => {
            ExecutionCleanupReceipt::completed("managed authority released ExecutionTemp")
        }
        sigil_kernel::resource::ResourceCleanupStatusV1::CleanupIncomplete { .. } => {
            ExecutionCleanupReceipt::failed("managed authority could not release ExecutionTemp")
        }
        other => {
            ExecutionCleanupReceipt::unknown(format!("managed authority cleanup status: {other:?}"))
        }
    }
}

/// Production plugin-hook route bound to Extension-purpose admission and one execution policy.
/// It cannot be substituted with the generic built-in one-shot command route.
#[derive(Clone)]
pub struct RuntimeManagedPluginHookExecutionRouteV1 {
    extension_execution: Arc<RuntimeManagedExtensionExecutionRouteV1>,
    execution_config: sigil_kernel::ExecutionConfig,
    backend: ExecutionBackendKind,
    backend_capabilities: ExecutionBackendCapabilities,
    sandbox_profile: sigil_kernel::ExecutionSandboxProfile,
    network: ExecutionNetworkReceipt,
    control_log: Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
}

impl std::fmt::Debug for RuntimeManagedPluginHookExecutionRouteV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeManagedPluginHookExecutionRouteV1")
            .field("backend", &self.backend)
            .field("sandbox_profile", &self.sandbox_profile)
            .finish_non_exhaustive()
    }
}

impl RuntimeManagedPluginHookExecutionRouteV1 {
    /// Freezes the configured execution facts used for both pre-admission checks and launch.
    pub fn new(
        extension_execution: Arc<RuntimeManagedExtensionExecutionRouteV1>,
        execution_config: sigil_kernel::ExecutionConfig,
        control_log: Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    ) -> Result<Self> {
        let backend = sigil_tools_builtin::build_execution_backend(&execution_config)?;
        Ok(Self {
            extension_execution,
            backend: backend.kind(),
            backend_capabilities: backend.capabilities(),
            sandbox_profile: execution_config.profile(),
            network: backend.planned_network_receipt(),
            execution_config,
            control_log,
        })
    }

    fn validate_environment(
        request: &crate::plugins::ManagedPluginHookExecutionRequestV1,
    ) -> Result<()> {
        let expected = BTreeSet::from([
            "SIGIL_PLUGIN_HOOK_ID",
            "SIGIL_PLUGIN_ID",
            "SIGIL_WORKSPACE_ROOT",
        ]);
        let actual = request
            .environment
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        anyhow::ensure!(
            actual == expected,
            "managed plugin hook environment contains undeclared host metadata"
        );
        anyhow::ensure!(
            request.environment.get("SIGIL_PLUGIN_ID") == Some(&request.plugin_id),
            "managed plugin hook identity drifted before execution"
        );
        anyhow::ensure!(
            request.environment.get("SIGIL_PLUGIN_HOOK_ID") == Some(&request.hook_id),
            "managed plugin hook id drifted before execution"
        );
        Ok(())
    }

    async fn execute(
        &self,
        request: crate::plugins::ManagedPluginHookExecutionRequestV1,
    ) -> Result<ExecutionReceipt> {
        Self::validate_environment(&request)?;
        anyhow::ensure!(
            request.purpose == ExecutionPurposeV1::ExtensionProcess,
            "plugin hook purpose substitution was rejected before spawn"
        );
        let expected_grant_hash = crate::plugins::plugin_hook_config_grant_hash(
            &request.plugin_id,
            request.config_generation,
            &request.manifest_hash,
            &request.capability_digest,
        );
        anyhow::ensure!(
            request.config_generation > 0 && request.config_grant_hash == expected_grant_hash,
            "plugin hook durable config grant drifted before spawn"
        );
        let environment = sigil_kernel::resolve_extension_process_environment(&[])?;
        let plan = sigil_tools_builtin::long_lived_stdio_process_plan(
            &self.execution_config,
            &request.program,
            &request.args,
            &request.cwd,
            &environment,
        )?;
        let backend = plan.backend;
        let backend_capabilities = plan.backend_capabilities;
        let network = plan.network.clone();
        let output_limit_bytes = u64::try_from(request.output_limit_bytes)
            .unwrap_or(u64::MAX)
            .max(1);
        let identity = crate::r71_shadow_planner::canonical_digest(
            format!(
                "{}\0{}\0{}\0{}",
                request.plugin_id,
                request.hook_id,
                request.manifest_hash,
                request.capability_digest
            )
            .as_bytes(),
        );
        let prepared = self
            .extension_execution
            .prepare_persistent_with_options(
                &format!("plugin-hook:{}", identity.to_hex()),
                plan,
                request.environment.clone(),
                Some(request.timeout_ms.max(1)),
                Some(output_limit_bytes),
            )
            .map_err(|error| anyhow!("managed plugin hook planning failed: {error}"))?;
        let admission = materialize_plugin_extension_admission(
            &request,
            &prepared,
            self.extension_execution.authority_generation,
        )?;
        let control_lease = self
            .control_log
            .acquire_named(
                crate::managed_storage_writer::StorageWriterChannelV1::ApplicationControlLog,
                &identity.to_hex(),
            )
            .map_err(|error| anyhow!("plugin application control log unavailable: {error}"))?;
        append_plugin_control_record(
            &self.control_log,
            &control_lease,
            serde_json::json!({
                "schema_version": 1,
                "event": "extension_plan_decided",
                "plugin_id": request.plugin_id,
                "hook_id": request.hook_id,
                "extension_plan_hash": admission.extension_plan_hash,
                "decision_hash": admission.decision_hash,
                "config_grant_ref": request.config_grant_ref.as_str(),
                "config_grant_hash": request.config_grant_hash,
                "durable_scope": request.durable_scope,
            }),
        )?;
        append_plugin_control_record(
            &self.control_log,
            &control_lease,
            serde_json::json!({
                "schema_version": 1,
                "event": "extension_start_authorized",
                "admission_id": admission.admission_id.as_str(),
                "physical_attempt_id": admission.physical_attempt_id.as_str(),
                "admission_hash": admission.admission_hash,
                "execution_plan_draft_hash": admission.execution_plan_draft_hash,
                "resource_plan_hash": admission.resource_plan_hash,
            }),
        )?;
        let mut process = match self
            .extension_execution
            .launch_prepared(prepared, Some(&admission))
            .await
        {
            Ok(process) => process,
            Err(error) => {
                append_plugin_control_record(
                    &self.control_log,
                    &control_lease,
                    serde_json::json!({
                        "schema_version": 1,
                        "event": "extension_start_rejected",
                        "admission_hash": admission.admission_hash,
                        "reason": format!("{error}"),
                    }),
                )?;
                self.control_log.finalize(control_lease).map_err(|log_error| {
                    anyhow!("plugin control-log settlement failed after launch rejection: {log_error}")
                })?;
                return Err(anyhow!("managed plugin hook launch failed: {error}"));
            }
        };
        let mut output_stream = process
            .take_output_stream()
            .map_err(|error| anyhow!("managed plugin hook output unavailable: {error}"))?;
        let mut cancellation_observed = false;
        loop {
            let frame = if let Some(cancellation) = request
                .cancellation
                .as_ref()
                .filter(|_| !cancellation_observed)
            {
                tokio::select! {
                    frame = output_stream.next_frame() => frame,
                    () = cancellation.cancelled() => {
                        process
                            .cancel(ProcessCancelReasonV1::UserCancelled)
                            .await
                            .map_err(|error| anyhow!("managed plugin hook cancellation failed: {error}"))?;
                        cancellation_observed = true;
                        continue;
                    }
                }
            } else {
                output_stream.next_frame().await
            }
            .map_err(|error| anyhow!("managed plugin hook output failed: {error}"))?;
            if frame.is_none() {
                break;
            }
        }
        let managed_receipt = process
            .wait_and_finalize()
            .await
            .map_err(|error| anyhow!("managed plugin hook settlement failed: {error}"))?;
        append_plugin_control_record(
            &self.control_log,
            &control_lease,
            serde_json::json!({
                "schema_version": 1,
                "event": "extension_process_settled",
                "admission_hash": admission.admission_hash,
                "physical_attempt_id": managed_receipt.physical_attempt_id.as_str(),
                "termination": managed_receipt.process.termination,
                "cancel_requested": cancellation_observed,
            }),
        )?;
        self.control_log.finalize(control_lease).map_err(|error| {
            anyhow!("plugin application control log settlement failed: {error}")
        })?;
        Ok(persistent_execution_receipt(
            &managed_receipt.process,
            &managed_receipt.resources.cleanup_status,
            backend,
            backend_capabilities,
            network,
            output_limit_bytes,
        ))
    }
}

fn materialize_plugin_extension_admission(
    request: &crate::plugins::ManagedPluginHookExecutionRequestV1,
    prepared: &PreparedManagedExtensionLaunchV1,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
) -> Result<sigil_kernel::extension_admission::ExtensionProcessAdmissionV1> {
    use sigil_kernel::extension_admission::{
        ExtensionApprovalDecisionV1, ExtensionProcessAdmissionV1, ExtensionProcessDecisionV1,
        ExtensionProcessPlanV1, ExtensionRestartPolicyV1, authorize_extension,
    };
    use sigil_kernel::resource::{ExtensionKindV1, OpaqueDomainEventId, OpaqueExtensionId};

    let config_policy_digest = request.config_grant_hash;
    let permission_upper_bound_hash =
        crate::r71_shadow_planner::canonical_digest(request.capability_digest.as_bytes());
    let requested_enforcement_hash = canonical_debug_digest(&prepared.enforcement.requested);
    let extension_plan_hash = crate::r71_shadow_planner::canonical_digest(
        format!(
            "plugin-extension-plan-v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            request.plugin_id,
            request.hook_id,
            request.config_generation,
            prepared.draft.draft_hash.to_hex(),
            prepared.draft.resource_plan_hash.to_hex(),
            request.config_grant_hash.to_hex(),
            requested_enforcement_hash.to_hex(),
        )
        .as_bytes(),
    );
    let plan = ExtensionProcessPlanV1 {
        extension_kind: ExtensionKindV1::Plugin,
        extension_id: OpaqueExtensionId::new(format!(
            "plugin:{}:{}",
            request.plugin_id, request.hook_id
        )),
        config_generation: request.config_generation,
        attempt_journal_scope: prepared.draft.attempt_journal_scope.clone(),
        attempt_journal_scope_hash: prepared.draft.attempt_journal_scope_hash,
        executable_and_args_digest: prepared.draft.argv_digest,
        config_policy_digest,
        permission_upper_bound_hash,
        execution_plan_draft_hash: prepared.draft.draft_hash,
        resource_plan_hash: prepared.draft.resource_plan_hash,
        requirement_set_hash: prepared.draft.resource_requirements.canonical_hash,
        requested_enforcement_hash,
        resolver_proof_digest: prepared.draft.resolver_proof_digest,
        sandbox_preview_hash: prepared.draft.sandbox_preview_hash,
        capture_policy_hash: prepared.draft.capture_policy_hash,
        resource_limits_hash: prepared.draft.resource_limits_hash,
        restart_policy: ExtensionRestartPolicyV1::Never,
        extension_plan_hash,
    };
    let decision_hash = crate::r71_shadow_planner::canonical_digest(
        format!(
            "plugin-extension-decision-v1\0{}\0{}\0{}",
            extension_plan_hash.to_hex(),
            request.config_grant_ref.as_str(),
            request.config_grant_hash.to_hex(),
        )
        .as_bytes(),
    );
    let decision = ExtensionProcessDecisionV1 {
        decision_id: format!("plugin-decision:{}", prepared.attempt_id.as_str()),
        durable_scope: request.durable_scope.clone(),
        domain_event_id: OpaqueDomainEventId::new(format!(
            "plugin-plan:{}",
            prepared.attempt_id.as_str()
        )),
        extension_plan_hash,
        attempt_journal_scope_hash: prepared.draft.attempt_journal_scope_hash,
        policy_version: "plugin-config-grant-v1".to_owned(),
        authorization: ExtensionApprovalDecisionV1::AllowByDurableConfigGrant {
            grant_ref: request.config_grant_ref.clone(),
            grant_hash: request.config_grant_hash,
        },
        decision_hash,
    };
    authorize_extension(&decision, &plan)
        .map_err(|error| anyhow!("plugin extension authorization failed: {error}"))?;
    let durable_scope_hash = canonical_debug_digest(&request.durable_scope);
    let extension_start_event_digest = crate::r71_shadow_planner::canonical_digest(
        format!(
            "plugin-extension-start-v1\0{}\0{}\0{}",
            extension_plan_hash.to_hex(),
            decision_hash.to_hex(),
            prepared.attempt_id.as_str(),
        )
        .as_bytes(),
    );
    let admission_hash = crate::r71_shadow_planner::canonical_digest(
        format!(
            "plugin-extension-admission-v1\0{}\0{}\0{}\0{}",
            extension_plan_hash.to_hex(),
            decision_hash.to_hex(),
            durable_scope_hash.to_hex(),
            extension_start_event_digest.to_hex(),
        )
        .as_bytes(),
    );
    Ok(ExtensionProcessAdmissionV1 {
        admission_id: OpaqueAdmissionId::new(format!(
            "plugin-admission:{}",
            prepared.attempt_id.as_str()
        )),
        physical_attempt_id: prepared.attempt_id.clone(),
        extension_kind: plan.extension_kind,
        extension_id: plan.extension_id,
        config_generation: plan.config_generation,
        authority_generation,
        attempt_journal_scope: plan.attempt_journal_scope,
        attempt_journal_scope_hash: plan.attempt_journal_scope_hash,
        executable_and_args_digest: plan.executable_and_args_digest,
        config_policy_digest: plan.config_policy_digest,
        permission_upper_bound_hash: plan.permission_upper_bound_hash,
        execution_plan_draft_hash: plan.execution_plan_draft_hash,
        resource_plan_hash: plan.resource_plan_hash,
        extension_plan_hash: plan.extension_plan_hash,
        decision_hash,
        durable_scope_hash,
        extension_start_event_digest,
        resource_requirements: prepared.draft.resource_requirements.clone(),
        requirement_set_hash: plan.requirement_set_hash,
        requested_enforcement: prepared.enforcement.requested.clone(),
        requested_enforcement_hash: plan.requested_enforcement_hash,
        resolver_proof_digest: plan.resolver_proof_digest,
        sandbox_preview_hash: plan.sandbox_preview_hash,
        capture_policy_hash: plan.capture_policy_hash,
        resource_limits_hash: plan.resource_limits_hash,
        restart_policy: plan.restart_policy,
        admission_hash,
    })
}

fn canonical_debug_digest(value: &impl std::fmt::Debug) -> CanonicalHash {
    crate::r71_shadow_planner::canonical_digest(format!("{value:?}").as_bytes())
}

fn append_plugin_control_record(
    writer: &crate::managed_storage_writer::ManagedStorageWriterAdapterV1,
    lease: &crate::managed_storage_writer::ManagedStorageWriterLeaseV1,
    record: serde_json::Value,
) -> Result<()> {
    let bytes = serde_json::to_vec(&record)?;
    writer
        .write_record(lease, &bytes)
        .map_err(|error| anyhow!("plugin application control append failed: {error}"))
}

#[async_trait::async_trait]
impl crate::plugins::ManagedPluginHookExecutionPortV1 for RuntimeManagedPluginHookExecutionRouteV1 {
    fn kind(&self) -> ExecutionBackendKind {
        self.backend
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        self.backend_capabilities
    }

    fn sandbox_profile(&self) -> sigil_kernel::ExecutionSandboxProfile {
        self.sandbox_profile
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        self.network.clone()
    }

    async fn execute_plugin_hook(
        &self,
        request: crate::plugins::ManagedPluginHookExecutionRequestV1,
    ) -> Result<ExecutionReceipt> {
        self.execute(request).await
    }
}

/// Runtime adapter for non-interactive built-in commands. It owns the physical launch material
/// only after the tool has produced its ordinary permission-bound request; admission, planning,
/// broker issue, sandbox launch and receipt construction remain on the managed path.
#[derive(Clone)]
pub struct RuntimeManagedCommandExecutionRouteV1 {
    planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1>,
    broker: Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>,
    execution_temp_authority: Arc<ExecutionTempAuthorityV1>,
}

impl std::fmt::Debug for RuntimeManagedCommandExecutionRouteV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeManagedCommandExecutionRouteV1")
            .field("execution_temp_authority", &"[hidden]")
            .finish_non_exhaustive()
    }
}

impl RuntimeManagedCommandExecutionRouteV1 {
    #[must_use]
    pub fn new(
        planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1>,
        broker: Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>,
        execution_temp_root: PathBuf,
    ) -> Self {
        Self {
            planner,
            broker,
            execution_temp_authority: Arc::new(ExecutionTempAuthorityV1::new(execution_temp_root)),
        }
    }

    fn command_digest(request: &ExecutionRequest) -> CanonicalHash {
        let mut bytes = b"managed-one-shot-command-v1\0".to_vec();
        bytes.extend_from_slice(request.program.as_bytes());
        bytes.push(0);
        for arg in &request.args {
            bytes.extend_from_slice(arg.as_bytes());
            bytes.push(0);
        }
        bytes.extend_from_slice(request.cwd.to_string_lossy().as_bytes());
        crate::r71_shadow_planner::canonical_digest(&bytes)
    }

    fn cwd_subject(request: &ExecutionRequest) -> OpaquePermissionSubjectRef {
        OpaquePermissionSubjectRef::new(format!(
            "managed-one-shot-cwd:{}",
            crate::r71_shadow_planner::canonical_digest(request.cwd.to_string_lossy().as_bytes())
                .to_hex()
        ))
    }

    fn limits(request: &ExecutionRequest) -> ExecutionResourceLimits {
        ExecutionResourceLimits {
            max_output_bytes: 8 * 1024 * 1024,
            max_runtime_ms: request.timeout_millis().unwrap_or(24 * 60 * 60 * 1000),
            max_children: request.process_count_limit.unwrap_or(64),
            max_fds: 1024,
            pty_required: false,
        }
    }

    fn capture(limits: &ExecutionResourceLimits) -> ExecutionCapturePolicy {
        ExecutionCapturePolicy {
            stdout_capture: CaptureModeV1::BoundedRing {
                max_bytes: limits.max_output_bytes,
            },
            stderr_capture: CaptureModeV1::BoundedRing {
                max_bytes: limits.max_output_bytes,
            },
            pty: false,
        }
    }

    fn environment(request: &ExecutionRequest) -> Vec<(OsString, OsString)> {
        let mut environment = std::collections::BTreeMap::<OsString, OsString>::new();
        if request.environment_policy == sigil_kernel::ProcessEnvironmentPolicy::InheritParent {
            // InheritParent is still an explicit managed environment agreement. Preserve the
            // closed command-toolchain baseline, not every ambient secret/debug variable.
            for name in [
                "PATH",
                "CARGO_HOME",
                "RUSTUP_HOME",
                "LANG",
                "LC_ALL",
                "LC_CTYPE",
            ] {
                if let Some(value) = std::env::var_os(name) {
                    environment.insert(OsString::from(name), value);
                }
            }
            // HOME itself is reserved for the fresh ExecutionTemp profile. Toolchain stores are
            // borrowed, read-only inputs, so materialize their conventional locations as
            // explicit bindings when the parent did not already name them.
            if let Some(parent_home) = std::env::var_os("HOME") {
                for (name, relative) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
                    environment.entry(OsString::from(name)).or_insert_with(|| {
                        PathBuf::from(&parent_home).join(relative).into_os_string()
                    });
                }
            }
        }
        environment.extend(
            request
                .env
                .iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        );
        environment.into_iter().collect()
    }

    async fn start_managed_terminal(
        &self,
        request: sigil_tools_builtin::ManagedTerminalStartRequestV1,
    ) -> Result<
        Box<dyn sigil_kernel::managed_execution::ManagedProcessHandleV1>,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        let argv = std::iter::once(OsString::from(&request.program))
            .chain(request.args.iter().map(OsString::from))
            .collect::<Vec<_>>();
        let environment = request
            .environment
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect::<Vec<_>>();
        let cwd_subject_ref = OpaquePermissionSubjectRef::new(format!(
            "managed-terminal-cwd:{}",
            crate::r71_shadow_planner::canonical_digest(request.cwd.to_string_lossy().as_bytes())
                .to_hex()
        ));
        let pty_requested = request.pty_size.is_some();
        let capture = ExecutionCapturePolicy {
            stdout_capture: CaptureModeV1::ArtifactStreaming,
            stderr_capture: CaptureModeV1::ArtifactStreaming,
            pty: pty_requested,
        };
        let limits = ExecutionResourceLimits {
            max_output_bytes: 64 * 1024 * 1024,
            max_runtime_ms: 24 * 60 * 60 * 1000,
            max_children: 64,
            max_fds: 1024,
            pty_required: pty_requested,
        };
        let structured_command_digest = terminal_command_digest(&request);
        let plan_request = ManagedExecutionPlanRequestV1 {
            argv: argv.clone(),
            cwd_subject_ref: cwd_subject_ref.clone(),
            purpose: ExecutionPurposeV1::Terminal,
            structured_command_digest,
            owner_scope: ResourceOwnerScopeV1::Application,
            capture: capture.clone(),
            limits: limits.clone(),
            environment: environment.clone(),
        };
        let draft = self.planner.plan_execution(plan_request).map_err(|_| {
            sigil_kernel::managed_execution::ManagedExecutionErrorV1::ExecutionPlanDrift
        })?;
        let attempt_id =
            PhysicalAttemptId::new(format!("managed-terminal:{}", uuid::Uuid::new_v4()));
        let managed_request = ManagedExecutionRequestV1 {
            argv: argv.clone(),
            cwd_subject_ref: cwd_subject_ref.clone(),
            structured_command_digest,
            admission_ref: OpaqueAdmissionId::new(attempt_id.as_str().to_owned()),
            execution_plan_draft_hash: draft.draft_hash,
            environment_profile: draft.environment_profile.clone(),
            capture,
            limits,
            pty_size: request.pty_size,
            environment: environment.clone(),
        };
        let proof = self.broker.seal_execution_proof(
            sigil_kernel::capability_issuer::ProofKindV1::ExecutionTerminal,
            "builtin-terminal",
            attempt_id.as_str().as_bytes().to_vec(),
        );
        let bundle = self.broker.issue_execution(proof).map_err(|_| {
            sigil_kernel::managed_execution::ManagedExecutionErrorV1::AdmissionMismatch
        })?;
        let execution_temp = self
            .execution_temp_authority
            .provision(attempt_id.as_str(), 1)
            .map_err(|_| {
                sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable
            })?;
        let terminal_enforcement = terminal_enforcement(draft.environment_profile.profile_hash);
        let launcher = Arc::new(
            sigil_sandbox::managed::CommandManagedTerminalLaunchServiceV1::new(
                PathBuf::from(&request.program),
                request.args.iter().map(OsString::from).collect(),
                request.cwd,
                environment,
                terminal_enforcement,
            ),
        );
        let service = sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&self.planner),
            execution_temp.binding().root.clone(),
        )
        .with_terminal_launcher(launcher);
        wrap_persistent_launch(
            service.start_persistent(bundle, managed_request).await,
            execution_temp,
        )
    }

    async fn start_managed_code_intel(
        &self,
        request: sigil_code_intel::LanguageServerLaunchRequestV1,
    ) -> Result<sigil_code_intel::LanguageServerProcessIoV1> {
        let argv = std::iter::once(OsString::from(&request.program))
            .chain(request.args.iter().map(OsString::from))
            .collect::<Vec<_>>();
        let environment = request
            .environment
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect::<Vec<_>>();
        let cwd_subject_ref = OpaquePermissionSubjectRef::new(format!(
            "managed-code-intel-cwd:{}",
            crate::r71_shadow_planner::canonical_digest(request.cwd.to_string_lossy().as_bytes())
                .to_hex()
        ));
        let capture = ExecutionCapturePolicy {
            stdout_capture: CaptureModeV1::ArtifactStreaming,
            stderr_capture: CaptureModeV1::BoundedRing {
                max_bytes: 2 * 1024 * 1024,
            },
            pty: false,
        };
        let limits = ExecutionResourceLimits {
            max_output_bytes: 8 * 1024 * 1024,
            max_runtime_ms: 24 * 60 * 60 * 1000,
            max_children: 32,
            max_fds: 512,
            pty_required: false,
        };
        let structured_command_digest = code_intel_command_digest(&request);
        let plan_request = ManagedExecutionPlanRequestV1 {
            argv: argv.clone(),
            cwd_subject_ref: cwd_subject_ref.clone(),
            purpose: ExecutionPurposeV1::CodeIntelProcess,
            structured_command_digest,
            owner_scope: ResourceOwnerScopeV1::Application,
            capture: capture.clone(),
            limits: limits.clone(),
            environment: environment.clone(),
        };
        let draft = self
            .planner
            .plan_execution(plan_request)
            .map_err(|error| anyhow!("managed code-intel planning failed: {error}"))?;
        let attempt_id = PhysicalAttemptId::new(format!(
            "code-intel:{}:{}",
            request.server_name,
            uuid::Uuid::new_v4()
        ));
        let managed_request = ManagedExecutionRequestV1 {
            argv,
            cwd_subject_ref,
            structured_command_digest,
            admission_ref: OpaqueAdmissionId::new(attempt_id.as_str().to_owned()),
            execution_plan_draft_hash: draft.draft_hash,
            environment_profile: draft.environment_profile.clone(),
            capture,
            limits,
            pty_size: None,
            environment: environment.clone(),
        };
        let proof = self.broker.seal_execution_proof(
            sigil_kernel::capability_issuer::ProofKindV1::ExecutionCodeIntel,
            "code-intel",
            attempt_id.as_str().as_bytes().to_vec(),
        );
        let bundle = self
            .broker
            .issue_execution(proof)
            .map_err(|error| anyhow!("managed code-intel admission failed: {error}"))?;
        let execution_temp = self
            .execution_temp_authority
            .provision(attempt_id.as_str(), 1)
            .map_err(|error| anyhow!("managed code-intel temp provisioning failed: {error}"))?;
        let enforcement = terminal_enforcement(draft.environment_profile.profile_hash);
        let launcher = Arc::new(
            sigil_sandbox::managed::CommandManagedExtensionLaunchServiceV1::new(
                request.program,
                request.args.iter().map(OsString::from).collect(),
                request.cwd,
                environment,
                enforcement,
            ),
        );
        let service = sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&self.planner),
            execution_temp.binding().root.clone(),
        )
        .with_code_intel_launcher(launcher);
        let mut process = wrap_persistent_launch(
            service.start_persistent(bundle, managed_request).await,
            execution_temp,
        )
        .map_err(|error| anyhow!("managed code-intel launch failed: {error}"))?;
        let mut output = process
            .take_output_stream()
            .map_err(|error| anyhow!("managed code-intel output unavailable: {error}"))?;
        let (client_side, bridge_side) = tokio::io::duplex(128 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_side);
        let (mut bridge_reader, mut bridge_writer) = tokio::io::split(bridge_side);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            let mut stdin_closed = false;
            let mut input = vec![0_u8; 64 * 1024];
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        let _ = process.cancel(ProcessCancelReasonV1::ParentShutdown).await;
                        let _ = process.wait_and_finalize().await;
                        break;
                    }
                    frame = output.next_frame() => {
                        match frame {
                            Ok(Some(frame)) => {
                                if matches!(frame.channel, ManagedProcessOutputChannelV1::Stdout | ManagedProcessOutputChannelV1::Pty)
                                    && !frame.payload.is_empty()
                                    && bridge_writer.write_all(&frame.payload).await.is_err()
                                {
                                    let _ = process.cancel(ProcessCancelReasonV1::ParentShutdown).await;
                                    let _ = process.wait_and_finalize().await;
                                    break;
                                }
                            }
                            Ok(None) | Err(_) => {
                                let _ = process.wait_and_finalize().await;
                                break;
                            }
                        }
                    }
                    read = bridge_reader.read(&mut input), if !stdin_closed => {
                        match read {
                            Ok(0) => {
                                stdin_closed = true;
                                let _ = process.close_stdin().await;
                            }
                            Ok(count) => {
                                if process.write_stdin(BoundedProcessInputV1 {
                                    payload: input[..count].to_vec(),
                                }).await.is_err() {
                                    let _ = process.cancel(ProcessCancelReasonV1::ParentShutdown).await;
                                    let _ = process.wait_and_finalize().await;
                                    break;
                                }
                            }
                            Err(_) => {
                                let _ = process.cancel(ProcessCancelReasonV1::ParentShutdown).await;
                                let _ = process.wait_and_finalize().await;
                                break;
                            }
                        }
                    }
                }
            }
        });
        let shutdown = move || {
            let _ = shutdown_tx.send(());
        };
        Ok(sigil_code_intel::LanguageServerProcessIoV1::new(
            client_reader,
            client_writer,
            shutdown,
        ))
    }

    async fn execute_managed(
        &self,
        mut request: ExecutionRequest,
        _cancellation: Option<sigil_kernel::RunCancellationHandle>,
    ) -> Result<ExecutionReceipt> {
        let cwd_subject_ref = Self::cwd_subject(&request);
        let argv = std::iter::once(OsString::from(&request.program))
            .chain(request.args.iter().map(OsString::from))
            .collect::<Vec<_>>();
        let limits = Self::limits(&request);
        let capture = Self::capture(&limits);
        let environment = Self::environment(&request);
        let plan_request = ManagedExecutionPlanRequestV1 {
            argv: argv.clone(),
            cwd_subject_ref: cwd_subject_ref.clone(),
            purpose: ExecutionPurposeV1::OneShot,
            structured_command_digest: Self::command_digest(&request),
            owner_scope: ResourceOwnerScopeV1::Application,
            capture: capture.clone(),
            limits: limits.clone(),
            environment: environment.clone(),
        };
        let draft = self
            .planner
            .plan_execution(plan_request)
            .map_err(|error| anyhow!("managed one-shot planning failed: {error}"))?;
        let attempt_id =
            PhysicalAttemptId::new(format!("managed-one-shot:{}", uuid::Uuid::new_v4()));
        let structured_command_digest = Self::command_digest(&request);
        let managed_request = ManagedExecutionRequestV1 {
            argv,
            cwd_subject_ref: cwd_subject_ref.clone(),
            structured_command_digest,
            admission_ref: OpaqueAdmissionId::new(attempt_id.as_str().to_owned()),
            execution_plan_draft_hash: draft.draft_hash,
            environment_profile: draft.environment_profile.clone(),
            capture,
            limits: limits.clone(),
            pty_size: None,
            environment,
        };
        let proof = self.broker.seal_execution_proof(
            sigil_kernel::capability_issuer::ProofKindV1::ExecutionOneShot,
            "builtin-command",
            attempt_id.as_str().as_bytes().to_vec(),
        );
        let bundle = self
            .broker
            .issue_execution(proof)
            .map_err(|error| anyhow!("managed one-shot admission failed: {error}"))?;
        let execution_temp = self
            .execution_temp_authority
            .provision(attempt_id.as_str(), 1)
            .map_err(|error| anyhow!("managed one-shot temp provisioning failed: {error}"))?;
        let launcher = Arc::new(
            sigil_sandbox::managed::CommandManagedOneShotLaunchServiceV1::new(
                request.cwd.clone(),
                cwd_subject_ref,
            ),
        );
        let service = sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&self.planner),
            execution_temp.binding().root.clone(),
        )
        .with_one_shot_launcher(launcher);
        let managed_result = service.execute_once(bundle, managed_request).await;
        let mut managed_receipt = match managed_result {
            Ok(receipt) => receipt,
            Err(error) => {
                let _ = execution_temp.finalize();
                return Err(anyhow!("managed one-shot execution failed: {error}"));
            }
        };
        managed_receipt.resources.cleanup_status = finalize_execution_temp(execution_temp);
        let process = &managed_receipt.process;
        let output = ExecutionOutputReceipt {
            schema_version: EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
            stdout: stream_capture(&process.stdout_summary, limits.max_output_bytes),
            stderr: stream_capture(&process.stderr_summary, limits.max_output_bytes),
            combined_total_bytes: process
                .stdout_summary
                .observed_bytes
                .saturating_add(process.stderr_summary.observed_bytes),
            combined_hard_limit_bytes: limits.max_output_bytes.saturating_mul(2),
            termination: map_termination(&process.termination),
        };
        let exit_code = match process.termination {
            sigil_kernel::managed_execution::ProcessTerminationV1::Exited { code } => Some(code),
            _ => None,
        };
        let capture_outcome = request.capture.take().map(|capture| {
            let mut sink = capture.sink;
            let stdout_result = sink.write_stream(
                ToolOutputStreamV1::Stdout,
                &process.stdout_summary.retained_payload,
            );
            let stderr_result = sink.write_stream(
                ToolOutputStreamV1::Stderr,
                &process.stderr_summary.retained_payload,
            );
            if stdout_result.is_err() || stderr_result.is_err() {
                sink.mark_process_write_failed();
            }
            ExecutionCaptureOutcome {
                sink,
                source: source_completeness(&process.termination),
                observed_bytes: output.combined_total_bytes,
                reader_failed: false,
            }
        });
        Ok(ExecutionReceipt {
            backend: ExecutionBackendKind::Local,
            capabilities: ExecutionBackendCapabilities::default(),
            network: ExecutionNetworkReceipt::unknown(
                "managed Local execution is explicitly unconfined and has no network enforcement",
            ),
            resources: ExecutionResourceReceipt {
                cleanup: execution_cleanup_receipt(&managed_receipt.resources.cleanup_status),
                ..ExecutionResourceReceipt::default()
            },
            environment_policy: request.environment_policy,
            exit_code,
            stdout: process.stdout_summary.retained_payload.clone(),
            stderr: process.stderr_summary.retained_payload.clone(),
            timed_out: matches!(
                process.termination,
                sigil_kernel::managed_execution::ProcessTerminationV1::TimedOut
            ),
            output,
            capture: capture_outcome,
        })
    }
}

#[async_trait::async_trait]
impl sigil_tools_builtin::ManagedCommandExecutionPortV1 for RuntimeManagedCommandExecutionRouteV1 {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        ExecutionNetworkReceipt::unknown(
            "managed Local execution is explicitly unconfined and has no network enforcement",
        )
    }

    async fn execute_with_cancellation(
        &self,
        request: ExecutionRequest,
        cancellation: Option<sigil_kernel::RunCancellationHandle>,
    ) -> Result<ExecutionReceipt> {
        self.execute_managed(request, cancellation).await
    }
}

#[async_trait::async_trait]
impl sigil_kernel::verification::VerificationExecutionPortV1
    for RuntimeManagedCommandExecutionRouteV1
{
    async fn execute_check(&self, request: ExecutionRequest) -> anyhow::Result<ExecutionReceipt> {
        self.execute_managed(request, None).await
    }
}

#[async_trait::async_trait]
impl sigil_tools_builtin::ManagedTerminalExecutionPortV1 for RuntimeManagedCommandExecutionRouteV1 {
    async fn start_persistent(
        &self,
        request: sigil_tools_builtin::ManagedTerminalStartRequestV1,
    ) -> Result<
        Box<dyn sigil_kernel::managed_execution::ManagedProcessHandleV1>,
        sigil_kernel::managed_execution::ManagedExecutionErrorV1,
    > {
        self.start_managed_terminal(request).await
    }
}

#[async_trait::async_trait]
impl sigil_code_intel::LanguageServerLaunchPortV1 for RuntimeManagedCommandExecutionRouteV1 {
    async fn launch(
        &self,
        request: sigil_code_intel::LanguageServerLaunchRequestV1,
    ) -> Result<sigil_code_intel::LanguageServerProcessIoV1> {
        self.start_managed_code_intel(request).await
    }
}

fn stream_capture(
    summary: &sigil_kernel::managed_execution::BoundedOutputSummaryV1,
    limit_bytes: u64,
) -> ExecutionStreamCapture {
    ExecutionStreamCapture {
        total_bytes: summary.observed_bytes,
        returned_bytes: summary.retained_bytes,
        omitted_bytes: summary
            .observed_bytes
            .saturating_sub(summary.retained_bytes),
        retained_head_bytes: summary.retained_bytes,
        retained_tail_bytes: 0,
        retained_limit_bytes: limit_bytes,
        hard_limit_bytes: limit_bytes,
        total_lines: summary
            .retained_payload
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u64,
        truncated: summary.truncated,
    }
}

fn persistent_execution_receipt(
    process: &sigil_kernel::managed_execution::ProcessExecutionReceiptV1,
    cleanup_status: &sigil_kernel::resource::ResourceCleanupStatusV1,
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: ExecutionNetworkReceipt,
    output_limit_bytes: u64,
) -> ExecutionReceipt {
    let output = ExecutionOutputReceipt {
        schema_version: EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
        stdout: stream_capture(&process.stdout_summary, output_limit_bytes),
        stderr: stream_capture(&process.stderr_summary, output_limit_bytes),
        combined_total_bytes: process
            .stdout_summary
            .observed_bytes
            .saturating_add(process.stderr_summary.observed_bytes),
        combined_hard_limit_bytes: output_limit_bytes.saturating_mul(2),
        termination: map_termination(&process.termination),
    };
    let exit_code = match process.termination {
        sigil_kernel::managed_execution::ProcessTerminationV1::Exited { code } => Some(code),
        _ => None,
    };
    ExecutionReceipt {
        backend,
        capabilities,
        network,
        resources: ExecutionResourceReceipt {
            cleanup: execution_cleanup_receipt(cleanup_status),
            ..ExecutionResourceReceipt::default()
        },
        environment_policy: sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension,
        exit_code,
        stdout: process.stdout_summary.retained_payload.clone(),
        stderr: process.stderr_summary.retained_payload.clone(),
        timed_out: matches!(
            process.termination,
            sigil_kernel::managed_execution::ProcessTerminationV1::TimedOut
        ),
        output,
        capture: None,
    }
}

fn map_termination(
    termination: &sigil_kernel::managed_execution::ProcessTerminationV1,
) -> ExecutionTerminationCause {
    match termination {
        sigil_kernel::managed_execution::ProcessTerminationV1::Exited { .. } => {
            ExecutionTerminationCause::Exited
        }
        sigil_kernel::managed_execution::ProcessTerminationV1::TimedOut => {
            ExecutionTerminationCause::TimedOut
        }
        sigil_kernel::managed_execution::ProcessTerminationV1::Cancelled => {
            ExecutionTerminationCause::Cancelled
        }
        sigil_kernel::managed_execution::ProcessTerminationV1::NotSpawned => {
            ExecutionTerminationCause::ReaderFailed {
                stream: ExecutionOutputStream::Combined,
                reason: "managed process was not spawned".to_owned(),
            }
        }
        sigil_kernel::managed_execution::ProcessTerminationV1::Signaled { .. }
        | sigil_kernel::managed_execution::ProcessTerminationV1::OutcomeUncertain { .. } => {
            ExecutionTerminationCause::ReaderFailed {
                stream: ExecutionOutputStream::Combined,
                reason: "managed process termination did not provide an exit code".to_owned(),
            }
        }
    }
}

fn source_completeness(
    termination: &sigil_kernel::managed_execution::ProcessTerminationV1,
) -> sigil_kernel::ToolSourceCompletenessV1 {
    match termination {
        sigil_kernel::managed_execution::ProcessTerminationV1::Exited { .. } => {
            sigil_kernel::ToolSourceCompletenessV1::Complete
        }
        sigil_kernel::managed_execution::ProcessTerminationV1::TimedOut => {
            sigil_kernel::ToolSourceCompletenessV1::Interrupted
        }
        sigil_kernel::managed_execution::ProcessTerminationV1::Cancelled => {
            sigil_kernel::ToolSourceCompletenessV1::Interrupted
        }
        _ => sigil_kernel::ToolSourceCompletenessV1::ReaderFailed,
    }
}

fn extension_path_digest(path: &Path) -> CanonicalHash {
    crate::r71_shadow_planner::canonical_digest(path.to_string_lossy().as_bytes())
}

fn terminal_command_digest(
    request: &sigil_tools_builtin::ManagedTerminalStartRequestV1,
) -> CanonicalHash {
    let mut bytes = b"managed-terminal-command-v1\0".to_vec();
    bytes.extend_from_slice(request.program.as_bytes());
    bytes.push(0);
    for arg in &request.args {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    bytes.extend_from_slice(request.cwd.to_string_lossy().as_bytes());
    for (key, value) in &request.environment {
        bytes.push(0);
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.as_bytes());
    }
    crate::r71_shadow_planner::canonical_digest(&bytes)
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

fn code_intel_command_digest(
    request: &sigil_code_intel::LanguageServerLaunchRequestV1,
) -> CanonicalHash {
    let mut bytes = b"managed-code-intel-command-v1\0".to_vec();
    bytes.extend_from_slice(request.server_name.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(request.program.to_string_lossy().as_bytes());
    bytes.push(0);
    for arg in &request.args {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    bytes.extend_from_slice(request.cwd.to_string_lossy().as_bytes());
    for (key, value) in &request.environment {
        bytes.push(0);
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.as_bytes());
    }
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

fn terminal_enforcement(
    profile_hash: CanonicalHash,
) -> sigil_sandbox::managed::ManagedExtensionLaunchEnforcementV1 {
    sigil_sandbox::managed::ManagedExtensionLaunchEnforcementV1 {
        requested: RequestedEnforcementV1 {
            requirement: sigil_kernel::resource::EnforcementRequirementClassV1::ExplicitUnconfined,
            deny_ambient_system_temp_write: false,
            deny_ambient_home_write: false,
            deny_ungranted_workspace_write: false,
            require_process_tree_ownership: false,
            require_network_policy: false,
            requested_capability_set_hash: CanonicalHash::from_bytes([0u8; 32]),
            profile_hash,
        },
        backend: SandboxBackendClassV1::LocalUnconfined,
        completeness: sigil_kernel::resource::EnforcementCompletenessV1::None,
        effective_access: BTreeSet::new(),
        effective_capability_set_hash: CanonicalHash::from_bytes([0u8; 32]),
        proof_set_hash: crate::r71_shadow_planner::canonical_digest(
            b"managed-terminal-launcher-proof-v1",
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
            borrowed_native_save: bundle.borrowed_native_save.clone(),
            borrowed_configuration: None,
            borrowed_release_output: None,
            product_state_updater_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_native_save_seam: if bundle.borrowed_native_save.is_some() {
                crate::r71_global_cutover::RuntimeProductStateSeamV1::AuthorityRegistrationBacked
            } else {
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter
            },
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
            borrowed_native_save: bundle.borrowed_native_save.clone(),
            borrowed_configuration: None,
            borrowed_release_output: None,
            product_state_updater_seam:
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter,
            borrowed_native_save_seam: if bundle.borrowed_native_save.is_some() {
                crate::r71_global_cutover::RuntimeProductStateSeamV1::AuthorityRegistrationBacked
            } else {
                crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter
            },
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

    /// Attaches the host-private borrowed native-save registration route.
    #[must_use]
    pub fn with_optional_borrowed_native_save(
        mut self,
        borrowed_native_save: Option<
            Arc<dyn sigil_resource_authority::native_save::BorrowedNativeSaveServiceV1>,
        >,
    ) -> Self {
        self.borrowed_native_save_seam = if borrowed_native_save.is_some() {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::AuthorityRegistrationBacked
        } else {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter
        };
        self.borrowed_native_save = borrowed_native_save;
        self
    }

    /// Attaches the host-private borrowed configuration registration route.
    #[must_use]
    pub fn with_optional_borrowed_configuration(
        mut self,
        borrowed_configuration: Option<
            Arc<dyn sigil_resource_authority::configuration::BorrowedConfigurationServiceV1>,
        >,
    ) -> Self {
        self.borrowed_configuration_seam = if borrowed_configuration.is_some() {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::AuthorityRegistrationBacked
        } else {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter
        };
        self.borrowed_configuration = borrowed_configuration;
        self
    }

    /// Attaches the real nonshipping release-owner file/tree service.
    #[must_use]
    pub fn with_optional_borrowed_release_output(
        mut self,
        borrowed_release_output: Option<
            Arc<dyn sigil_resource_authority::release_output::BorrowedReleaseOutputServiceV1>,
        >,
    ) -> Self {
        self.borrowed_release_output_seam = if borrowed_release_output.is_some() {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::AuthorityRegistrationBacked
        } else {
            crate::r71_global_cutover::RuntimeProductStateSeamV1::LegacyDirectWriter
        };
        self.borrowed_release_output = borrowed_release_output;
        self
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

    #[tokio::test]
    async fn r71_managed_command_route_seals_and_executes_one_shot() {
        use sigil_tools_builtin::ManagedCommandExecutionPortV1;

        let root = tempfile::tempdir().expect("workspace");
        let execution_temp = tempfile::tempdir().expect("execution temp");
        let route = RuntimeManagedCommandExecutionRouteV1::new(
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp.path().to_path_buf(),
        );
        let mut environment = std::collections::BTreeMap::new();
        environment.insert("TMPDIR".to_owned(), "/ambient-override".to_owned());
        environment.insert("HOME".to_owned(), "/ambient-home".to_owned());
        let receipt = route
            .execute_with_cancellation(
                sigil_kernel::ExecutionRequest {
                    program: "/bin/sh".to_owned(),
                    args: vec![
                        "-c".to_owned(),
                        concat!(
                            "test -d \"$TMPDIR\" && test -d \"$HOME\" && ",
                            "test -d \"$XDG_STATE_HOME\" && test -d \"$XDG_CACHE_HOME\" && ",
                            "test -d \"$SIGIL_STATE_HOME\" && test -d \"$SIGIL_CACHE_HOME\" && ",
                            "test \"$TMPDIR\" != /ambient-override && ",
                            "test \"$HOME\" != /ambient-home && ",
                            "touch \"$TMPDIR/child-created\" && printf managed-route"
                        )
                        .to_owned(),
                    ],
                    cwd: root.path().to_path_buf(),
                    env: environment,
                    environment_policy: sigil_kernel::ProcessEnvironmentPolicy::default(),
                    timeout_ms: Some(30_000),
                    timeout_secs: 30,
                    cpu_time_ms: None,
                    memory_limit_bytes: None,
                    process_count_limit: None,
                    capture: None,
                },
                None,
            )
            .await
            .expect("managed command route");
        assert_eq!(receipt.stdout, b"managed-route");
        assert_eq!(receipt.exit_code, Some(0));
        assert_eq!(receipt.backend, sigil_kernel::ExecutionBackendKind::Local);
        assert_eq!(
            receipt.resources.cleanup.status,
            sigil_kernel::ExecutionCleanupStatus::Completed
        );
        assert_eq!(
            std::fs::read_dir(execution_temp.path())
                .expect("execution temp anchor")
                .count(),
            0,
            "the exact per-attempt generation must be released"
        );
    }

    #[tokio::test]
    async fn r71_managed_code_intel_route_bridges_stdout_and_finalizes_process() {
        use sigil_code_intel::LanguageServerLaunchPortV1;
        use tokio::io::AsyncReadExt;

        let root = tempfile::tempdir().expect("workspace");
        let execution_temp = tempfile::tempdir().expect("execution temp");
        let route = RuntimeManagedCommandExecutionRouteV1::new(
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp.path().to_path_buf(),
        );
        let io = route
            .launch(sigil_code_intel::LanguageServerLaunchRequestV1 {
                server_name: "fake-lsp".to_owned(),
                program: PathBuf::from("/bin/sh"),
                args: vec!["-c".to_owned(), "printf managed-code-intel".to_owned()],
                cwd: root.path().to_path_buf(),
                environment: Vec::new(),
            })
            .await
            .expect("managed code-intel route");
        let (mut reader, _writer, _shutdown) = io.into_parts();
        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .await
            .expect("managed code-intel output");
        assert_eq!(output, b"managed-code-intel");
        assert_eq!(
            std::fs::read_dir(execution_temp.path())
                .expect("execution temp anchor")
                .count(),
            0,
            "code-intel settlement must release the exact ExecutionTemp generation"
        );
    }

    #[tokio::test]
    async fn r71_managed_terminal_route_seals_and_owns_persistent_process() {
        use sigil_tools_builtin::{ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1};

        let root = tempfile::tempdir().expect("workspace");
        let execution_temp = tempfile::tempdir().expect("execution temp");
        let route = RuntimeManagedCommandExecutionRouteV1::new(
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp.path().to_path_buf(),
        );
        let mut handle = route
            .start_persistent(ManagedTerminalStartRequestV1 {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "printf terminal-route".to_owned()],
                cwd: root.path().to_path_buf(),
                environment: std::collections::BTreeMap::new(),
                pty_size: None,
            })
            .await
            .expect("managed terminal route");
        let mut stream = handle.take_output_stream().expect("managed stream");
        let mut output = Vec::new();
        while let Some(frame) = stream.next_frame().await.expect("output frame") {
            if !frame.end_of_stream {
                output.extend(frame.payload);
            }
        }
        let receipt = handle.wait_and_finalize().await.expect("terminal receipt");
        assert_eq!(output, b"terminal-route");
        assert!(matches!(
            receipt.process.termination,
            sigil_kernel::managed_execution::ProcessTerminationV1::Exited { code: 0 }
        ));
        assert_eq!(
            receipt.resources.cleanup_status,
            sigil_kernel::resource::ResourceCleanupStatusV1::Released
        );
        assert_eq!(
            std::fs::read_dir(execution_temp.path())
                .expect("execution temp anchor")
                .count(),
            0,
            "persistent settlement must release the exact ExecutionTemp generation"
        );
    }

    #[tokio::test]
    async fn r71_managed_terminal_route_supports_pty_control_and_receipt() {
        use sigil_tools_builtin::{ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1};

        let root = tempfile::tempdir().expect("workspace");
        let execution_temp = tempfile::tempdir().expect("execution temp");
        let route = RuntimeManagedCommandExecutionRouteV1::new(
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp.path().to_path_buf(),
        );
        let mut handle = route
            .start_persistent(ManagedTerminalStartRequestV1 {
                program: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "read line; printf '%s\\n' \"$line\"".to_owned(),
                ],
                cwd: root.path().to_path_buf(),
                environment: std::collections::BTreeMap::new(),
                pty_size: Some(sigil_kernel::managed_execution::BoundedPtySizeV1 {
                    rows: 24,
                    cols: 80,
                }),
            })
            .await
            .expect("managed pty terminal route");
        handle
            .resize_pty(sigil_kernel::managed_execution::BoundedPtySizeV1 {
                rows: 32,
                cols: 100,
            })
            .await
            .expect("resize");
        let mut stream = handle.take_output_stream().expect("managed stream");
        handle
            .write_stdin(sigil_kernel::managed_execution::BoundedProcessInputV1 {
                payload: b"runtime-pty\n".to_vec(),
            })
            .await
            .expect("write");
        handle.close_stdin().await.expect("close");
        let mut output = Vec::new();
        while let Some(frame) = stream.next_frame().await.expect("output frame") {
            if !frame.end_of_stream {
                output.extend(frame.payload);
            }
        }
        let receipt = handle.wait_and_finalize().await.expect("terminal receipt");
        assert!(
            output
                .windows(b"runtime-pty".len())
                .any(|window| window == b"runtime-pty")
        );
        assert!(matches!(
            receipt.process.termination,
            sigil_kernel::managed_execution::ProcessTerminationV1::Exited { code: 0 }
        ));
        assert_eq!(
            receipt.resources.cleanup_status,
            sigil_kernel::resource::ResourceCleanupStatusV1::Released
        );
    }

    #[tokio::test]
    async fn r71_managed_terminal_manager_cancel_waits_for_persistent_receipt() -> anyhow::Result<()>
    {
        let root = tempfile::tempdir().expect("workspace");
        let artifact_root = tempfile::tempdir().expect("artifacts");
        let execution_temp = tempfile::tempdir().expect("execution temp");
        let route = Arc::new(RuntimeManagedCommandExecutionRouteV1::new(
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp.path().to_path_buf(),
        ));
        let manager = sigil_tools_builtin::TerminalProcessManager::new_with_artifact_root(
            root.path(),
            artifact_root.path(),
            "state/artifacts/tasks",
        )?
        .with_managed_execution(route);
        let entry = manager
            .start_pty(
                sigil_tools_builtin::TerminalStartRequest {
                    command: "sleep 30".to_owned(),
                    shell: Some("/bin/sh".to_owned()),
                    ..Default::default()
                },
                Some(sigil_tools_builtin::TerminalPtySize { rows: 24, cols: 80 }),
            )
            .await?;
        assert_eq!(
            entry.handle.execution_backend,
            Some(sigil_kernel::terminal_task::TerminalExecutionBackendKind::SandboxedPty)
        );
        manager
            .resize(
                &entry.handle.task_id,
                sigil_tools_builtin::TerminalPtySize {
                    rows: 32,
                    cols: 100,
                },
            )
            .await?;
        let input = manager
            .input(&entry.handle.task_id, "managed-input\n")
            .await?;
        assert_eq!(input.input_bytes, "managed-input\n".len() as u64);
        let cancelled = manager.cancel(&entry.handle.task_id).await?;
        assert!(matches!(
            cancelled.status,
            sigil_kernel::TerminalTaskStatus::Cancelled
        ));
        Ok(())
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
