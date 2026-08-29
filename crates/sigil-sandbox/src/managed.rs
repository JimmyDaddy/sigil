//! RFC-0071 section 11 / R71.6: sandbox-owned managed execution service.
//!
//! The ONLY kernel-facing place that performs an OS spawn for the managed seam. It consumes the
//! kernel ManagedExecutionServiceV1 port: purpose closure against the issued admission bundle,
//! deterministic replan binding against the approved draft hash, planner-authoritative
//! enforcement (the consumer cannot self-declare confinement), truthful Local none enforcement
//! (never a fabricated subset), bounded output with exact-one-EOF, and kernel-shaped receipts.
//! A draft whose environment profile requires isolation is refused here because Local cannot
//! prove it; real backends (Seatbelt / bwrap / Docker cidfile / Windows helper-ACL) land in R71.8.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use sigil_kernel::managed_execution::{
    AccessWideningPolicyV1, BoundedOutputSummaryV1, BoundedProcessInputV1,
    BoundedProcessOutputFrameV1, BoundedPtySizeV1, ExecutionResourceReceiptV1,
    ManagedExecutionErrorV1, ManagedExecutionPlanDraftV1, ManagedExecutionPlanRequestV1,
    ManagedExecutionPlannerV1, ManagedExecutionReceiptV1, ManagedExecutionRequestV1,
    ManagedExecutionServiceV1, ManagedProcessControlErrorV1, ManagedProcessHandleV1,
    ManagedProcessOutputChannelV1, ManagedProcessOutputStreamV1, ProcessCancelReasonV1,
    ProcessControlActionV1, ProcessControlReceiptV1, ProcessExecutionReceiptV1,
    ProcessTerminationV1, ResourceEnforcementReceiptV1,
};
use sigil_kernel::resource::{
    CanonicalHash, EffectiveEnforcementV1, EnforcementCompletenessV1,
    EnforcementRequirementClassV1, EnvironmentProfileClassV1, IssuedExecutionAdmissionBundleV1,
    OpaquePermissionSubjectRef, OpaqueProcessRef, OpaqueResourceId, OpaqueSpawnIntentId,
    PhysicalAttemptId, ReflectiveOpaqueProcessRef, RequestedEnforcementV1, ResourceAccessV1,
    ResourceCleanupStatusV1, ResourceJournalScopeV1, ResourceKindV1, ResourceOwnerScopeV1,
    ResourceRefV1, SandboxBackendClassV1,
};

use crate::environment::{apply_reserved_environment, standard_reserved_environment};
use crate::launch_plan::SealedSandboxLaunchPlanV1;
use crate::receipt::verify_enforcement;

fn zero_hash() -> CanonicalHash {
    CanonicalHash::from_bytes([0u8; 32])
}

/// Content digest over observed bytes (sha256).
fn content_digest(bytes: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

/// Canonical env-map digest (stable key order, NUL-delimited pairs).
fn env_hash(env: &BTreeMap<String, String>) -> CanonicalHash {
    let mut acc = Vec::new();
    for (key, value) in env {
        acc.extend_from_slice(key.as_bytes());
        acc.push(0);
        acc.extend_from_slice(value.as_bytes());
        acc.push(0);
    }
    content_digest(&acc)
}

/// Sandbox-owned execution service over the kernel consumer port.
pub struct SandboxManagedExecutionServiceV1 {
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    execution_temp_root: PathBuf,
    process_inventory: Option<Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>>,
    one_shot_launcher: Option<Arc<dyn ManagedOneShotLaunchServiceV1>>,
    terminal_launcher: Option<Arc<dyn ManagedTerminalLaunchServiceV1>>,
    extension_launcher: Option<Arc<dyn ManagedExtensionLaunchServiceV1>>,
    code_intel_launcher: Option<Arc<dyn ManagedCodeIntelLaunchServiceV1>>,
}

/// Host-private one-shot launch seam. The service owns admission, planning, output bounds and
/// receipts; this injected launcher owns only the physical cwd/process material for one bound
/// command. Keeping it separate prevents a path-bearing command from entering the kernel request.
pub trait ManagedOneShotLaunchServiceV1: Send + Sync {
    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1>;
}

/// Host-private terminal launch seam. Persistent terminal cwd and argv remain bound to the
/// runtime-resolved plan; the sandbox service performs admission and process ownership around
/// this one physical launch.
pub trait ManagedTerminalLaunchServiceV1: Send + Sync {
    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1>;

    fn launch_pty(
        &self,
        _request: &ManagedExecutionRequestV1,
        _environment: &BTreeMap<String, String>,
        _size: BoundedPtySizeV1,
    ) -> Result<ManagedPtyLaunchV1, ManagedExecutionErrorV1> {
        Err(ManagedExecutionErrorV1::ProviderUnavailable)
    }
}

/// Sandbox-owned PTY launch material. Portable-PTY objects never cross the kernel/runtime
/// contract; they are consumed immediately by the managed process handle below.
pub struct ManagedPtyLaunchV1 {
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn std::io::Read + Send>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Real command-backed terminal launcher. It is plan-bound by construction and is not a generic
/// fallback: production composition must inject it explicitly for the Terminal purpose.
pub struct CommandManagedTerminalLaunchServiceV1 {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
    enforcement: ManagedExtensionLaunchEnforcementV1,
}

impl CommandManagedTerminalLaunchServiceV1 {
    #[must_use]
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        cwd: PathBuf,
        environment: Vec<(OsString, OsString)>,
        enforcement: ManagedExtensionLaunchEnforcementV1,
    ) -> Self {
        Self {
            program,
            args,
            cwd,
            environment,
            enforcement,
        }
    }
}

impl ManagedTerminalLaunchServiceV1 for CommandManagedTerminalLaunchServiceV1 {
    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1> {
        let launcher = CommandManagedExtensionLaunchServiceV1::new(
            self.program.clone(),
            self.args.clone(),
            self.cwd.clone(),
            self.environment.clone(),
            self.enforcement.clone(),
        );
        ManagedExtensionLaunchServiceV1::launch(&launcher, request, environment)
    }

    fn launch_pty(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
        size: BoundedPtySizeV1,
    ) -> Result<ManagedPtyLaunchV1, ManagedExecutionErrorV1> {
        let expected_argv = std::iter::once(self.program.as_os_str().to_os_string())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>();
        let expected_environment = self
            .environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        if request.argv != expected_argv || request.environment != expected_environment {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        if size.rows == 0 || size.cols == 0 {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
        let reader = pty
            .master
            .try_clone_reader()
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
        let mut command = CommandBuilder::new(&self.program);
        command.args(&self.args);
        command.cwd(&self.cwd);
        command.env_clear();
        for (key, value) in environment {
            command.env(key, value);
        }
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
        Ok(ManagedPtyLaunchV1 {
            master: pty.master,
            reader,
            writer,
            child,
        })
    }
}

/// Local host launcher used by the runtime managed one-shot route. It is valid only when the
/// planner has selected the explicit-unconfined Local profile; the sandbox still performs that
/// check before invoking this launcher.
pub struct CommandManagedOneShotLaunchServiceV1 {
    cwd: PathBuf,
    cwd_subject_ref: OpaquePermissionSubjectRef,
}

impl CommandManagedOneShotLaunchServiceV1 {
    #[must_use]
    pub fn new(cwd: PathBuf, cwd_subject_ref: OpaquePermissionSubjectRef) -> Self {
        Self {
            cwd,
            cwd_subject_ref,
        }
    }
}

impl ManagedOneShotLaunchServiceV1 for CommandManagedOneShotLaunchServiceV1 {
    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1> {
        if request.argv.is_empty() || request.cwd_subject_ref != self.cwd_subject_ref {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        let mut command = Command::new(&request.argv[0]);
        command
            .args(request.argv.iter().skip(1))
            .current_dir(&self.cwd)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
            .spawn()
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)
    }
}

/// Observed enforcement returned by the host-injected extension launcher. The sandbox owns the
/// final receipt; this value only describes the backend that the injected launcher is prepared to
/// invoke for this exact extension plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedExtensionLaunchEnforcementV1 {
    pub requested: RequestedEnforcementV1,
    pub backend: SandboxBackendClassV1,
    pub completeness: EnforcementCompletenessV1,
    pub effective_access: std::collections::BTreeSet<ResourceAccessV1>,
    pub effective_capability_set_hash: CanonicalHash,
    pub proof_set_hash: CanonicalHash,
}

/// Host-private extension launch seam. A launcher is bound to one resolved long-lived process
/// plan and is injected into the sandbox service for that launch only. It is deliberately not a
/// generic execution fallback: absence of this seam keeps Extension admission fail closed.
pub trait ManagedExtensionLaunchServiceV1: Send + Sync {
    fn enforcement(&self) -> ManagedExtensionLaunchEnforcementV1;

    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1>;
}

/// Host-private code-intelligence launch seam. It is separate from extension launch so an MCP
/// admission cannot be reused to start a language server.
pub trait ManagedCodeIntelLaunchServiceV1: Send + Sync {
    fn enforcement(&self) -> ManagedExtensionLaunchEnforcementV1;

    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1>;
}

/// Real command-backed extension launcher used by the runtime route. The command and cwd are
/// host-private launch material originating from a sealed `LongLivedStdioProcessPlan`; no caller
/// can replace them through the managed request after the plan has been bound.
pub struct CommandManagedExtensionLaunchServiceV1 {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
    enforcement: ManagedExtensionLaunchEnforcementV1,
}

impl CommandManagedExtensionLaunchServiceV1 {
    #[must_use]
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        cwd: PathBuf,
        environment: Vec<(OsString, OsString)>,
        enforcement: ManagedExtensionLaunchEnforcementV1,
    ) -> Self {
        Self {
            program,
            args,
            cwd,
            environment,
            enforcement,
        }
    }

    fn expected_argv(&self) -> Vec<OsString> {
        std::iter::once(self.program.as_os_str().to_os_string())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

impl ManagedExtensionLaunchServiceV1 for CommandManagedExtensionLaunchServiceV1 {
    fn enforcement(&self) -> ManagedExtensionLaunchEnforcementV1 {
        self.enforcement.clone()
    }

    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1> {
        if request.argv != self.expected_argv()
            || request.environment
                != self
                    .environment
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>()
        {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.enforcement.requested.require_process_tree_ownership || cfg!(unix) {
            sigil_process::configure_process_tree(&mut command);
        }
        command
            .spawn()
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)
    }
}

impl ManagedCodeIntelLaunchServiceV1 for CommandManagedExtensionLaunchServiceV1 {
    fn enforcement(&self) -> ManagedExtensionLaunchEnforcementV1 {
        self.enforcement.clone()
    }

    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1> {
        ManagedExtensionLaunchServiceV1::launch(self, request, environment)
    }
}

impl ManagedTerminalLaunchServiceV1 for CommandManagedExtensionLaunchServiceV1 {
    fn launch(
        &self,
        request: &ManagedExecutionRequestV1,
        environment: &BTreeMap<String, String>,
    ) -> Result<Child, ManagedExecutionErrorV1> {
        ManagedExtensionLaunchServiceV1::launch(self, request, environment)
    }
}

/// Purpose class derived from the issued bundle (never from the consumer request text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SigilPurpose {
    OneShot,
    Terminal,
    Extension,
    CodeIntel,
}

/// Fully bound local execution context; every check happens before any platform call.
struct PreparedLocalRunV1 {
    #[allow(dead_code)]
    purpose: SigilPurpose,
    attempt_id: PhysicalAttemptId,
    draft: ManagedExecutionPlanDraftV1,
    requested_enforcement: RequestedEnforcementV1,
    launch_plan: SealedSandboxLaunchPlanV1,
    env: BTreeMap<String, String>,
    environment_binding_hash: CanonicalHash,
    resource_receipt: ResourceEnforcementReceiptV1,
    effective_backend: SandboxBackendClassV1,
    enforcement_completeness: EnforcementCompletenessV1,
    effective_capability_set_hash: CanonicalHash,
    enforcement_proof_hash: CanonicalHash,
}

impl SandboxManagedExecutionServiceV1 {
    /// Creates the service. `execution_temp_root` is the authority-issued ExecutionTemp root
    /// (pathless binding: the service never creates it, only points reserved env vars at it).
    pub fn new(planner: Arc<dyn ManagedExecutionPlannerV1>, execution_temp_root: PathBuf) -> Self {
        Self {
            planner,
            execution_temp_root,
            process_inventory: None,
            one_shot_launcher: None,
            terminal_launcher: None,
            extension_launcher: None,
            code_intel_launcher: None,
        }
    }

    /// Attaches the authority-owned durable process inventory. Production launch routes must
    /// provide it; absence keeps physical spawn fail closed.
    #[must_use]
    pub fn with_process_inventory(
        mut self,
        inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>,
    ) -> Self {
        self.process_inventory = Some(inventory);
        self
    }

    #[must_use]
    pub fn with_one_shot_launcher(
        mut self,
        launcher: Arc<dyn ManagedOneShotLaunchServiceV1>,
    ) -> Self {
        self.one_shot_launcher = Some(launcher);
        self
    }

    #[must_use]
    pub fn with_terminal_launcher(
        mut self,
        launcher: Arc<dyn ManagedTerminalLaunchServiceV1>,
    ) -> Self {
        self.terminal_launcher = Some(launcher);
        self
    }

    #[must_use]
    pub fn with_extension_launcher(
        mut self,
        launcher: Arc<dyn ManagedExtensionLaunchServiceV1>,
    ) -> Self {
        self.extension_launcher = Some(launcher);
        self
    }

    #[must_use]
    pub fn with_code_intel_launcher(
        mut self,
        launcher: Arc<dyn ManagedCodeIntelLaunchServiceV1>,
    ) -> Self {
        self.code_intel_launcher = Some(launcher);
        self
    }

    fn prepare(
        &self,
        bundle: &IssuedExecutionAdmissionBundleV1,
        request: &ManagedExecutionRequestV1,
    ) -> Result<PreparedLocalRunV1, ManagedExecutionErrorV1> {
        let purpose = match bundle {
            IssuedExecutionAdmissionBundleV1::OneShot { .. } => SigilPurpose::OneShot,
            IssuedExecutionAdmissionBundleV1::Terminal { .. } => SigilPurpose::Terminal,
            IssuedExecutionAdmissionBundleV1::Extension { .. } => SigilPurpose::Extension,
            IssuedExecutionAdmissionBundleV1::CodeIntel { .. } => SigilPurpose::CodeIntel,
        };
        let execution_purpose = match purpose {
            SigilPurpose::OneShot => sigil_kernel::managed_execution::ExecutionPurposeV1::OneShot,
            SigilPurpose::Terminal => sigil_kernel::managed_execution::ExecutionPurposeV1::Terminal,
            SigilPurpose::Extension => {
                sigil_kernel::managed_execution::ExecutionPurposeV1::ExtensionProcess
            }
            SigilPurpose::CodeIntel => {
                sigil_kernel::managed_execution::ExecutionPurposeV1::CodeIntelProcess
            }
        };
        let plan_request = ManagedExecutionPlanRequestV1 {
            argv: request.argv.clone(),
            cwd_subject_ref: request.cwd_subject_ref.clone(),
            purpose: execution_purpose,
            structured_command_digest: request.structured_command_digest,
            owner_scope: ResourceOwnerScopeV1::Application,
            capture: request.capture.clone(),
            limits: request.limits.clone(),
            environment: request.environment.clone(),
        };
        let draft = self
            .planner
            .plan_execution(plan_request)
            .map_err(|_| ManagedExecutionErrorV1::ExecutionPlanDrift)?;
        // Closed-bound agreement first: over-bound argv/env never executes partially (the
        // bound is an admission precondition, not a plan-drift comparison).
        if request.environment.len()
            > sigil_kernel::managed_execution::MAX_MANAGED_EXECUTION_ENV_ENTRIES
            || request.argv.len()
                > sigil_kernel::managed_execution::MAX_MANAGED_EXECUTION_ARGV_ENTRIES
            || sigil_kernel::managed_execution::argv_encoded_bytes(&request.argv)
                > sigil_kernel::managed_execution::MAX_MANAGED_EXECUTION_ARGV_BYTES
        {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        if draft.draft_hash != request.execution_plan_draft_hash {
            return Err(ManagedExecutionErrorV1::ExecutionPlanDrift);
        }
        if draft.environment_digest
            != sigil_kernel::managed_execution::canonical_environment_digest(&request.environment)
        {
            // The sandbox never accepts an environment the planner did not seal.
            return Err(ManagedExecutionErrorV1::ExecutionPlanDrift);
        }
        // Planner-authoritative enforcement: the consumer never self-declares confinement.
        let (
            requested_enforcement,
            effective_backend,
            enforcement_completeness,
            effective_capability_set_hash,
            enforcement_proof_hash,
            effective_access,
        ) = match purpose {
            SigilPurpose::Extension => {
                let Some(launcher) = &self.extension_launcher else {
                    return Err(ManagedExecutionErrorV1::ProviderUnavailable);
                };
                let enforcement = launcher.enforcement();
                if enforcement.requested.profile_hash != draft.environment_profile.profile_hash {
                    return Err(ManagedExecutionErrorV1::ExecutionPlanDrift);
                }
                (
                    enforcement.requested,
                    enforcement.backend,
                    enforcement.completeness,
                    enforcement.effective_capability_set_hash,
                    enforcement.proof_set_hash,
                    enforcement.effective_access,
                )
            }
            SigilPurpose::CodeIntel => {
                let Some(launcher) = &self.code_intel_launcher else {
                    return Err(ManagedExecutionErrorV1::ProviderUnavailable);
                };
                let enforcement = launcher.enforcement();
                if enforcement.requested.profile_hash != draft.environment_profile.profile_hash {
                    return Err(ManagedExecutionErrorV1::ExecutionPlanDrift);
                }
                (
                    enforcement.requested,
                    enforcement.backend,
                    enforcement.completeness,
                    enforcement.effective_capability_set_hash,
                    enforcement.proof_set_hash,
                    enforcement.effective_access,
                )
            }
            _ => match draft.environment_profile.profile_class {
                EnvironmentProfileClassV1::ExplicitUnconfined => (
                    RequestedEnforcementV1 {
                        requirement: EnforcementRequirementClassV1::ExplicitUnconfined,
                        deny_ambient_system_temp_write: false,
                        deny_ambient_home_write: false,
                        deny_ungranted_workspace_write: false,
                        require_process_tree_ownership: false,
                        require_network_policy: false,
                        requested_capability_set_hash: zero_hash(),
                        profile_hash: draft.environment_profile.profile_hash,
                    },
                    SandboxBackendClassV1::LocalUnconfined,
                    EnforcementCompletenessV1::None,
                    zero_hash(),
                    zero_hash(),
                    std::collections::BTreeSet::new(),
                ),
                _ => return Err(ManagedExecutionErrorV1::ConfinementUnproven),
            },
        };
        if effective_backend == SandboxBackendClassV1::LocalUnconfined {
            crate::local::local_confinement_guard(
                crate::local::LocalRunPolicyV1::ExplicitUnconfined,
            )
            .map_err(|_| ManagedExecutionErrorV1::ConfinementUnproven)?;
        }

        let launch_plan = SealedSandboxLaunchPlanV1::build(
            request.admission_ref.as_str().to_owned(),
            requested_enforcement.clone(),
            draft.draft_hash,
            draft.environment_profile.profile_hash,
        );
        launch_plan
            .validate()
            .map_err(|_| ManagedExecutionErrorV1::ExecutionPlanDrift)?;

        let standard = standard_reserved_environment(&self.execution_temp_root);
        let mut candidate = BTreeMap::new();
        // Start with the planner-sealed request environment, then let the sandbox apply its
        // reserved bindings last. A caller may request TMPDIR/HOME, but it can never redirect
        // those names away from the authority-issued ExecutionTemp generation.
        for (key, value) in &request.environment {
            candidate.insert(
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            );
        }
        let (_override, env) = apply_reserved_environment(&mut candidate, &standard, None, false);
        let environment_binding_hash = env_hash(&env);

        let requirement = draft
            .resource_requirements
            .requirements
            .as_slice()
            .first()
            .cloned();
        let (kind, access) = match &requirement {
            Some(requirement) => (requirement.kind, requirement.access.clone()),
            None => (
                ResourceKindV1::ExecutionTemp,
                std::collections::BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]),
            ),
        };
        let resource_ref = ResourceRefV1 {
            resource_id: OpaqueResourceId::new(format!(
                "exec-attempt-{}",
                request.admission_ref.as_str()
            )),
            kind,
            owner_scope: ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
            generation: 1,
        };
        let requested_policy = match requested_enforcement.requirement {
            EnforcementRequirementClassV1::ExplicitUnconfined => {
                AccessWideningPolicyV1::ExplicitUnconfined
            }
            EnforcementRequirementClassV1::RequiredDeclaredSuperset { declaration_hash } => {
                AccessWideningPolicyV1::AllowDeclaredSuperset { declaration_hash }
            }
            EnforcementRequirementClassV1::RequiredExact
            | EnforcementRequirementClassV1::Preferred => AccessWideningPolicyV1::Exact,
        };
        let observed_access = if effective_backend == SandboxBackendClassV1::LocalUnconfined {
            std::collections::BTreeSet::new()
        } else if effective_access.is_empty() {
            access.clone()
        } else {
            effective_access.clone()
        };
        let resource_receipt = verify_enforcement(
            &resource_ref,
            &access,
            &requested_policy,
            &observed_access,
            effective_backend,
            enforcement_completeness,
        )
        .map_err(|_| ManagedExecutionErrorV1::ConfinementUnproven)?;

        Ok(PreparedLocalRunV1 {
            purpose,
            attempt_id: PhysicalAttemptId::new(request.admission_ref.as_str().to_owned()),
            draft,
            requested_enforcement,
            launch_plan,
            env,
            environment_binding_hash,
            resource_receipt,
            effective_backend,
            enforcement_completeness,
            effective_capability_set_hash,
            enforcement_proof_hash,
        })
    }

    fn service_resource_receipt(
        &self,
        prepared: &PreparedLocalRunV1,
    ) -> ExecutionResourceReceiptV1 {
        ExecutionResourceReceiptV1 {
            physical_attempt_id: prepared.attempt_id.clone(),
            manifest_hash: prepared.draft.draft_hash,
            sandbox_binding_hash: prepared.launch_plan.launch_plan_hash,
            requested_enforcement: prepared.requested_enforcement.clone(),
            effective_enforcement: EffectiveEnforcementV1 {
                backend: prepared.effective_backend,
                completeness: prepared.enforcement_completeness,
                effective_capability_set_hash: prepared.effective_capability_set_hash,
                access_widening_set_hash: zero_hash(),
                functional_probe_hash: prepared.launch_plan.launch_plan_hash,
                proof_set_hash: prepared.enforcement_proof_hash,
            },
            resources: vec![prepared.resource_receipt.clone()],
            enforcement_proof_set_hash: prepared.launch_plan.launch_plan_hash,
            environment_binding_hash: prepared.environment_binding_hash,
            cleanup_status: ResourceCleanupStatusV1::Released,
            effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
        }
    }
}

/// Kernel-shaped process receipt derived from observed facts.
fn process_receipt_from(
    prepared: &PreparedLocalRunV1,
    termination: ProcessTerminationV1,
    stdout_summary: BoundedOutputSummaryV1,
    stderr_summary: BoundedOutputSummaryV1,
) -> ProcessExecutionReceiptV1 {
    let combined: Vec<u8> = stdout_summary
        .content_digest
        .as_bytes()
        .iter()
        .chain(stderr_summary.content_digest.as_bytes())
        .copied()
        .collect();
    let frontier = content_digest(&combined);
    ProcessExecutionReceiptV1 {
        physical_attempt_id: prepared.attempt_id.clone(),
        spawn_intent_id: OpaqueSpawnIntentId::new(format!(
            "spawn-{}",
            prepared.attempt_id.as_str()
        )),
        process_ref: Some(ReflectiveOpaqueProcessRef::new(OpaqueProcessRef::new(
            format!("process-{}", prepared.attempt_id.as_str()),
        ))),
        process_frontier_hash: frontier,
        termination,
        stdout_summary,
        stderr_summary,
        effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
        receipt_hash: frontier,
    }
}

fn control_receipt(
    attempt_id: &PhysicalAttemptId,
    action: ProcessControlActionV1,
) -> ProcessControlReceiptV1 {
    ProcessControlReceiptV1 {
        process_ref: ReflectiveOpaqueProcessRef::new(OpaqueProcessRef::new(format!(
            "process-{}",
            attempt_id.as_str()
        ))),
        action,
        request_digest: zero_hash(),
        observed_process_frontier_hash: zero_hash(),
        effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
        receipt_hash: zero_hash(),
    }
}

/// Reads one pipe with a hard byte cap; the pipe is drained past the cap so the child never
/// blocks on a full pipe, while retained bytes never exceed the cap (truncation is observed).
struct BoundedReadOutcome {
    summary: BoundedOutputSummaryV1,
}

fn bounded_read(reader: &mut impl Read, cap_bytes: u64) -> std::io::Result<BoundedReadOutcome> {
    let mut observed: u64 = 0;
    let mut retained: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        observed += read as u64;
        let remaining = cap_bytes.saturating_sub(retained.len() as u64) as usize;
        if remaining > 0 {
            retained.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
    Ok(BoundedReadOutcome {
        summary: BoundedOutputSummaryV1 {
            observed_bytes: observed,
            retained_bytes: retained.len() as u64,
            retained_payload: retained.clone(),
            content_digest: content_digest(&retained),
            truncated: observed > cap_bytes,
            artifact_ref: None,
        },
    })
}

/// Classifies an exit status truthfully (code or signal).
fn classify_status(status: ExitStatus) -> ProcessTerminationV1 {
    if let Some(code) = status.code() {
        return ProcessTerminationV1::Exited { code };
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ProcessTerminationV1::Signaled {
                signal: signal as u32,
            };
        }
    }
    ProcessTerminationV1::Exited { code: -1 }
}

/// Polls exit within the runtime cap; on deadline it kills and reports TimedOut.
async fn poll_termination(child: &mut Child, max_runtime_ms: u64) -> ProcessTerminationV1 {
    if max_runtime_ms == 0 {
        return ProcessTerminationV1::NotSpawned;
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(max_runtime_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return classify_status(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProcessTerminationV1::TimedOut;
                }
                // Blocking poll: the managed seam is blocking-IO here; R71.8 backends
                // replace this with a backend-native wait.
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                return ProcessTerminationV1::OutcomeUncertain {
                    evidence_digest: zero_hash(),
                };
            }
        }
    }
}

fn terminate_reap_and_settle(
    child: &mut Child,
    process_owner: Option<&sigil_process::ProcessTreeOwnerGuard>,
    inventory: &dyn sigil_resource_authority::AuthorityProcessInventoryPortV1,
    claim: &mut Option<sigil_resource_authority::AuthorityProcessInventoryClaimV1>,
) -> Result<(), ManagedExecutionErrorV1> {
    if let Some(process_owner) = process_owner {
        let _ = process_owner.terminate();
    }
    let _ = child.kill();
    child
        .wait()
        .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
    inventory
        .settle_spawn(
            claim
                .take()
                .ok_or(ManagedExecutionErrorV1::OutcomeUncertain)?,
        )
        .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)
}

/// Per-channel capped retained buffer plus truncation observation.
#[derive(Default)]
struct CapState {
    retained: Vec<u8>,
    observed: u64,
    cap: u64,
}

impl CapState {
    fn new(cap: u64) -> Self {
        Self {
            retained: Vec::new(),
            observed: 0,
            cap,
        }
    }

    fn push(&mut self, payload: &[u8]) {
        self.observed += payload.len() as u64;
        let remaining = self.cap.saturating_sub(self.retained.len() as u64) as usize;
        if remaining > 0 {
            self.retained
                .extend_from_slice(&payload[..payload.len().min(remaining)]);
        }
    }

    fn summary(&self) -> BoundedOutputSummaryV1 {
        BoundedOutputSummaryV1 {
            observed_bytes: self.observed,
            retained_bytes: self.retained.len() as u64,
            retained_payload: self.retained.clone(),
            content_digest: content_digest(&self.retained),
            truncated: self.observed > self.cap,
            artifact_ref: None,
        }
    }
}

/// Drains one pipe into bounded frames; the EOF frame carries end_of_stream exactly once.
/// A chunk is emitted only when it fits fully under the cap; beyond the cap the pipe is still
/// drained (child never blocks), the shared state observes truncation, and the EOF frame
/// reports it. The shared cap state is the single source for finalize summaries.
fn spawn_drain(
    mut pipe: impl Read + Send + 'static,
    channel: ManagedProcessOutputChannelV1,
    cap: u64,
    frame_tx: tokio::sync::mpsc::Sender<BoundedProcessOutputFrameV1>,
    state: Arc<Mutex<CapState>>,
) {
    std::thread::spawn(move || {
        let mut sequence: u64 = 0;
        let mut chunk = [0u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let mut guard = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let previous = guard.retained.len();
                    guard.push(&chunk[..read]);
                    let fit_fully = guard.retained.len() == previous + read;
                    drop(guard);
                    if fit_fully && read > 0 {
                        let payload = chunk[..read].to_vec();
                        let _ = frame_tx.blocking_send(BoundedProcessOutputFrameV1 {
                            channel,
                            sequence,
                            payload,
                            end_of_stream: false,
                            truncated: false,
                        });
                        sequence += 1;
                    }
                }
            }
        }
        let truncated = state
            .lock()
            .map(|guard| guard.observed > cap)
            .unwrap_or(false);
        let _ = frame_tx.blocking_send(BoundedProcessOutputFrameV1 {
            channel,
            sequence,
            payload: Vec::new(),
            end_of_stream: true,
            truncated,
        });
    });
}

#[async_trait]
impl ManagedExecutionServiceV1 for SandboxManagedExecutionServiceV1 {
    async fn execute_once(
        &self,
        bundle: IssuedExecutionAdmissionBundleV1,
        request: ManagedExecutionRequestV1,
    ) -> Result<ManagedExecutionReceiptV1, ManagedExecutionErrorV1> {
        if !matches!(bundle, IssuedExecutionAdmissionBundleV1::OneShot { .. }) {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        if request.limits.pty_required || request.capture.pty {
            return Err(ManagedExecutionErrorV1::ProviderUnavailable);
        }
        if request.limits.max_runtime_ms == 0 {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        let prepared = self.prepare(&bundle, &request)?;
        let Some(launcher) = &self.one_shot_launcher else {
            return Err(ManagedExecutionErrorV1::ProviderUnavailable);
        };
        let inventory = self
            .process_inventory
            .as_ref()
            .ok_or(ManagedExecutionErrorV1::ProviderUnavailable)?;
        let claim = inventory
            .prepare_spawn(prepared.attempt_id.as_str())
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
        let mut child = match launcher.launch(&request, &prepared.env) {
            Ok(child) => child,
            Err(error) => {
                inventory
                    .settle_spawn(claim)
                    .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
                return Err(error);
            }
        };
        if inventory.attach_spawn(&claim, child.id()).is_err() {
            let _ = child.kill();
            if child.wait().is_ok() {
                let _ = inventory.settle_spawn(claim);
            }
            return Err(ManagedExecutionErrorV1::ProviderUnavailable);
        }
        let process_owner = if prepared
            .requested_enforcement
            .require_process_tree_ownership
            || cfg!(unix)
        {
            match sigil_process::ProcessTreeOwnerGuard::assign(Some(child.id())) {
                Ok(owner) => Some(owner),
                Err(_error) => {
                    let _ = child.kill();
                    if child.wait().is_ok() {
                        let _ = inventory.settle_spawn(claim);
                    }
                    return Err(ManagedExecutionErrorV1::ConfinementUnproven);
                }
            }
        } else {
            None
        };
        let mut claim = Some(claim);
        let cap = request.limits.max_output_bytes;
        let stdout_pipe = match child.stdout.take() {
            Some(pipe) => pipe,
            None => {
                terminate_reap_and_settle(
                    &mut child,
                    process_owner.as_ref(),
                    inventory.as_ref(),
                    &mut claim,
                )?;
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            }
        };
        let stderr_pipe = match child.stderr.take() {
            Some(pipe) => pipe,
            None => {
                terminate_reap_and_settle(
                    &mut child,
                    process_owner.as_ref(),
                    inventory.as_ref(),
                    &mut claim,
                )?;
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            }
        };
        let mut stdout_pipe = stdout_pipe;
        let mut stderr_pipe = stderr_pipe;
        let stdout_outcome = match bounded_read(&mut stdout_pipe, cap) {
            Ok(outcome) => outcome,
            Err(_) => {
                terminate_reap_and_settle(
                    &mut child,
                    process_owner.as_ref(),
                    inventory.as_ref(),
                    &mut claim,
                )?;
                return Err(ManagedExecutionErrorV1::OutcomeUncertain);
            }
        };
        let stderr_outcome = match bounded_read(&mut stderr_pipe, cap) {
            Ok(outcome) => outcome,
            Err(_) => {
                terminate_reap_and_settle(
                    &mut child,
                    process_owner.as_ref(),
                    inventory.as_ref(),
                    &mut claim,
                )?;
                return Err(ManagedExecutionErrorV1::OutcomeUncertain);
            }
        };
        let termination = poll_termination(&mut child, request.limits.max_runtime_ms).await;
        if matches!(termination, ProcessTerminationV1::OutcomeUncertain { .. }) {
            if let Some(process_owner) = process_owner.as_ref() {
                let _ = process_owner.terminate();
            }
            let _ = child.kill();
            if child.wait().is_ok() {
                inventory
                    .settle_spawn(
                        claim
                            .take()
                            .ok_or(ManagedExecutionErrorV1::OutcomeUncertain)?,
                    )
                    .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
            } else {
                return Err(ManagedExecutionErrorV1::OutcomeUncertain);
            }
        } else {
            inventory
                .settle_spawn(
                    claim
                        .take()
                        .ok_or(ManagedExecutionErrorV1::OutcomeUncertain)?,
                )
                .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
        }
        let process_receipt = process_receipt_from(
            &prepared,
            termination,
            stdout_outcome.summary,
            stderr_outcome.summary,
        );
        let resource_receipt = self.service_resource_receipt(&prepared);
        Ok(ManagedExecutionReceiptV1 {
            physical_attempt_id: prepared.attempt_id,
            process: process_receipt,
            resources: resource_receipt,
            check: None,
        })
    }

    async fn start_persistent(
        &self,
        bundle: IssuedExecutionAdmissionBundleV1,
        request: ManagedExecutionRequestV1,
    ) -> Result<Box<dyn ManagedProcessHandleV1>, ManagedExecutionErrorV1> {
        let is_extension = matches!(bundle, IssuedExecutionAdmissionBundleV1::Extension { .. });
        let is_code_intel = matches!(bundle, IssuedExecutionAdmissionBundleV1::CodeIntel { .. });
        if !matches!(bundle, IssuedExecutionAdmissionBundleV1::Terminal { .. })
            && !is_extension
            && !is_code_intel
        {
            return Err(ManagedExecutionErrorV1::AdmissionMismatch);
        }
        let prepared = self.prepare(&bundle, &request)?;
        let inventory = self
            .process_inventory
            .as_ref()
            .ok_or(ManagedExecutionErrorV1::ProviderUnavailable)?;
        if request.limits.pty_required || request.capture.pty {
            if is_extension || is_code_intel {
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            }
            let Some(size) = request.pty_size else {
                return Err(ManagedExecutionErrorV1::AdmissionMismatch);
            };
            let Some(launcher) = &self.terminal_launcher else {
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            };
            let claim = inventory
                .prepare_spawn(prepared.attempt_id.as_str())
                .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
            let launch = match launcher.launch_pty(&request, &prepared.env, size) {
                Ok(launch) => launch,
                Err(error) => {
                    inventory
                        .settle_spawn(claim)
                        .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
                    return Err(error);
                }
            };
            return self.start_persistent_pty(
                prepared,
                launch,
                request.limits.max_output_bytes,
                Arc::clone(inventory),
                claim,
            );
        }
        let claim = inventory
            .prepare_spawn(prepared.attempt_id.as_str())
            .map_err(|_| ManagedExecutionErrorV1::ProviderUnavailable)?;
        let launched = if is_extension {
            let Some(launcher) = &self.extension_launcher else {
                let _ = inventory.settle_spawn(claim);
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            };
            launcher.launch(&request, &prepared.env)
        } else if is_code_intel {
            let Some(launcher) = &self.code_intel_launcher else {
                let _ = inventory.settle_spawn(claim);
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            };
            launcher.launch(&request, &prepared.env)
        } else if let Some(launcher) = &self.terminal_launcher {
            launcher.launch(&request, &prepared.env)
        } else {
            #[cfg(not(test))]
            {
                let _ = inventory.settle_spawn(claim);
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            }
            #[cfg(test)]
            {
                let mut command = Command::new(&request.argv[0]);
                command
                    .args(request.argv.iter().skip(1))
                    .env_clear()
                    .envs(prepared.env.iter())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                command
                    .spawn()
                    .map_err(|_error| ManagedExecutionErrorV1::ProviderUnavailable)
            }
        };
        let mut child = match launched {
            Ok(child) => child,
            Err(error) => {
                inventory
                    .settle_spawn(claim)
                    .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
                return Err(error);
            }
        };
        if inventory.attach_spawn(&claim, child.id()).is_err() {
            let _ = child.kill();
            if child.wait().is_ok() {
                let _ = inventory.settle_spawn(claim);
            }
            return Err(ManagedExecutionErrorV1::ProviderUnavailable);
        }
        let process_owner = if prepared
            .requested_enforcement
            .require_process_tree_ownership
            || cfg!(unix)
        {
            match sigil_process::ProcessTreeOwnerGuard::assign(Some(child.id())) {
                Ok(owner) => Some(owner),
                Err(_error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = inventory.settle_spawn(claim);
                    return Err(ManagedExecutionErrorV1::ConfinementUnproven);
                }
            }
        } else {
            None
        };
        let cap = request.limits.max_output_bytes;
        let stdout_pipe = match child.stdout.take() {
            Some(pipe) => pipe,
            None => {
                if let Some(process_owner) = process_owner.as_ref() {
                    let _ = process_owner.terminate();
                }
                let _ = child.kill();
                if child.wait().is_ok() {
                    let _ = inventory.settle_spawn(claim);
                }
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            }
        };
        let stderr_pipe = match child.stderr.take() {
            Some(pipe) => pipe,
            None => {
                if let Some(process_owner) = process_owner.as_ref() {
                    let _ = process_owner.terminate();
                }
                let _ = child.kill();
                if child.wait().is_ok() {
                    let _ = inventory.settle_spawn(claim);
                }
                return Err(ManagedExecutionErrorV1::ProviderUnavailable);
            }
        };
        let stdin_pipe = child.stdin.take();
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<BoundedProcessOutputFrameV1>(128);
        let handle_stdout_cap = Arc::new(Mutex::new(CapState::new(cap)));
        let handle_stderr_cap = Arc::new(Mutex::new(CapState::new(cap)));
        spawn_drain(
            stdout_pipe,
            ManagedProcessOutputChannelV1::Stdout,
            cap,
            frame_tx.clone(),
            Arc::clone(&handle_stdout_cap),
        );
        spawn_drain(
            stderr_pipe,
            ManagedProcessOutputChannelV1::Stderr,
            cap,
            frame_tx,
            Arc::clone(&handle_stderr_cap),
        );

        let handle = LocalPersistentProcessHandleV1 {
            child: Arc::new(Mutex::new(child)),
            process_owner,
            stdin: Arc::new(Mutex::new(stdin_pipe)),
            stdin_open: Arc::new(AtomicBool::new(true)),
            frame_rx: Some(frame_rx),
            stdout_cap: handle_stdout_cap,
            stderr_cap: handle_stderr_cap,
            attempt_id: prepared.attempt_id.clone(),
            prepared,
            finalizing: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            process_inventory: Arc::clone(inventory),
            process_claim: Some(claim),
        };
        Ok(Box::new(handle))
    }
}

impl SandboxManagedExecutionServiceV1 {
    fn start_persistent_pty(
        &self,
        prepared: PreparedLocalRunV1,
        launch: ManagedPtyLaunchV1,
        cap: u64,
        process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>,
        process_claim: sigil_resource_authority::AuthorityProcessInventoryClaimV1,
    ) -> Result<Box<dyn ManagedProcessHandleV1>, ManagedExecutionErrorV1> {
        let process_id = launch.child.process_id();
        let Some(process_id) = process_id else {
            let mut child = launch.child;
            let _ = child.kill();
            if child.wait().is_ok() {
                let _ = process_inventory.settle_spawn(process_claim);
            }
            return Err(ManagedExecutionErrorV1::ProviderUnavailable);
        };
        if process_inventory
            .attach_spawn(&process_claim, process_id)
            .is_err()
        {
            let mut child = launch.child;
            let _ = child.kill();
            if child.wait().is_ok() {
                let _ = process_inventory.settle_spawn(process_claim);
            }
            return Err(ManagedExecutionErrorV1::ProviderUnavailable);
        }
        let process_owner = if prepared
            .requested_enforcement
            .require_process_tree_ownership
            || cfg!(unix)
        {
            match sigil_process::ProcessTreeOwnerGuard::assign(Some(process_id)) {
                Ok(owner) => Some(owner),
                Err(_error) => {
                    let mut child = launch.child;
                    let _ = child.kill();
                    if child.wait().is_ok() {
                        let _ = process_inventory.settle_spawn(process_claim);
                    }
                    return Err(ManagedExecutionErrorV1::ConfinementUnproven);
                }
            }
        } else {
            None
        };
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<BoundedProcessOutputFrameV1>(128);
        let stdout_cap = Arc::new(Mutex::new(CapState::new(cap)));
        let stderr_cap = Arc::new(Mutex::new(CapState::new(cap)));
        spawn_drain(
            launch.reader,
            ManagedProcessOutputChannelV1::Pty,
            cap,
            frame_tx,
            Arc::clone(&stdout_cap),
        );
        let handle = LocalPersistentPtyProcessHandleV1 {
            child: Arc::new(Mutex::new(launch.child)),
            process_owner,
            master: Arc::new(Mutex::new(Some(launch.master))),
            stdin: Arc::new(Mutex::new(Some(launch.writer))),
            stdin_open: Arc::new(AtomicBool::new(true)),
            frame_rx: Some(frame_rx),
            stdout_cap,
            stderr_cap,
            attempt_id: prepared.attempt_id.clone(),
            prepared,
            cancelled: Arc::new(AtomicBool::new(false)),
            process_inventory,
            process_claim: Some(process_claim),
        };
        Ok(Box::new(handle))
    }
}

/// Local persistent process handle (non-clone, non-serialize).
struct LocalPersistentProcessHandleV1 {
    child: Arc<Mutex<Child>>,
    process_owner: Option<sigil_process::ProcessTreeOwnerGuard>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    stdin_open: Arc<AtomicBool>,
    frame_rx: Option<tokio::sync::mpsc::Receiver<BoundedProcessOutputFrameV1>>,
    stdout_cap: Arc<Mutex<CapState>>,
    stderr_cap: Arc<Mutex<CapState>>,
    attempt_id: PhysicalAttemptId,
    prepared: PreparedLocalRunV1,
    finalizing: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>,
    process_claim: Option<sigil_resource_authority::AuthorityProcessInventoryClaimV1>,
}

struct LocalPersistentOutputStreamV1 {
    rx: tokio::sync::mpsc::Receiver<BoundedProcessOutputFrameV1>,
}

#[async_trait]
impl ManagedProcessOutputStreamV1 for LocalPersistentOutputStreamV1 {
    async fn next_frame(
        &mut self,
    ) -> Result<Option<BoundedProcessOutputFrameV1>, ManagedProcessControlErrorV1> {
        Ok(self.rx.recv().await)
    }
}

#[async_trait]
impl ManagedProcessHandleV1 for LocalPersistentProcessHandleV1 {
    fn process_ref(&self) -> ReflectiveOpaqueProcessRef {
        ReflectiveOpaqueProcessRef::new(OpaqueProcessRef::new(format!(
            "process-{}",
            self.attempt_id.as_str()
        )))
    }

    fn physical_attempt_id(&self) -> PhysicalAttemptId {
        self.attempt_id.clone()
    }

    fn take_output_stream(
        &mut self,
    ) -> Result<Box<dyn ManagedProcessOutputStreamV1>, ManagedProcessControlErrorV1> {
        let Some(rx) = self.frame_rx.take() else {
            return Err(ManagedProcessControlErrorV1::StreamAlreadyTaken);
        };
        Ok(Box::new(LocalPersistentOutputStreamV1 { rx }))
    }

    async fn write_stdin(
        &mut self,
        input: BoundedProcessInputV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        if !self.stdin_open.load(Ordering::SeqCst) {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "write_stdin",
            });
        }
        let mut guard =
            self.stdin
                .lock()
                .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                    action: "write_stdin",
                })?;
        match guard.as_mut() {
            Some(stdin) => {
                use std::io::Write;
                stdin.write_all(&input.payload).map_err(|_| {
                    ManagedProcessControlErrorV1::InvalidState {
                        action: "write_stdin",
                    }
                })?;
            }
            None => {
                return Err(ManagedProcessControlErrorV1::InvalidState {
                    action: "write_stdin",
                });
            }
        }
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::WriteStdin,
        ))
    }

    async fn resize_pty(
        &mut self,
        _size: BoundedPtySizeV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        Err(ManagedProcessControlErrorV1::InvalidState {
            action: "resize_pty",
        })
    }

    async fn close_stdin(
        &mut self,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        if !self.stdin_open.swap(false, Ordering::SeqCst) {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "close_stdin",
            });
        }
        let mut guard =
            self.stdin
                .lock()
                .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                    action: "close_stdin",
                })?;
        *guard = None;
        // The kernel action set labels the stdin boundary mutation as WriteStdin; the typed
        // effect (EOF) is real, the label is the closed-set approximation.
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::WriteStdin,
        ))
    }

    async fn cancel(
        &mut self,
        _reason: ProcessCancelReasonV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(process_owner) = self.process_owner.as_ref() {
            let _ = process_owner.terminate();
        }
        let mut guard = self
            .child
            .lock()
            .map_err(|_| ManagedProcessControlErrorV1::InvalidState { action: "cancel" })?;
        let _ = guard.kill();
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::Cancel,
        ))
    }

    async fn wait_and_finalize(
        mut self: Box<Self>,
    ) -> Result<ManagedExecutionReceiptV1, ManagedExecutionErrorV1> {
        self.finalizing.store(true, Ordering::SeqCst);
        let (termination, reaped) = {
            let mut guard = self
                .child
                .lock()
                .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
            match guard.wait() {
                Ok(status) if self.cancelled.load(Ordering::SeqCst) => {
                    let _ = status;
                    (ProcessTerminationV1::Cancelled, true)
                }
                Ok(status) => (classify_status(status), true),
                Err(_) => (
                    ProcessTerminationV1::OutcomeUncertain {
                        evidence_digest: zero_hash(),
                    },
                    false,
                ),
            }
        };
        if reaped {
            let claim = self
                .process_claim
                .take()
                .ok_or(ManagedExecutionErrorV1::OutcomeUncertain)?;
            self.process_inventory
                .settle_spawn(claim)
                .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
        }
        // Drain whatever the pipes still hold so EOF markers land (bounded by pipe content).
        if let Some(mut rx) = self.frame_rx.take() {
            while let Some(frame) = rx.recv().await {
                let _ = frame;
            }
        }
        let stdout_summary = self
            .stdout_cap
            .lock()
            .map(|cap| cap.summary())
            .unwrap_or_else(|_| zero_summary());
        let stderr_summary = self
            .stderr_cap
            .lock()
            .map(|cap| cap.summary())
            .unwrap_or_else(|_| zero_summary());
        let process_receipt =
            process_receipt_from(&self.prepared, termination, stdout_summary, stderr_summary);
        let resource_receipt = self.handle_resource_receipt();
        Ok(ManagedExecutionReceiptV1 {
            physical_attempt_id: self.attempt_id.clone(),
            process: process_receipt,
            resources: resource_receipt,
            check: None,
        })
    }
}

fn zero_summary() -> BoundedOutputSummaryV1 {
    BoundedOutputSummaryV1 {
        observed_bytes: 0,
        retained_bytes: 0,
        retained_payload: Vec::new(),
        content_digest: zero_hash(),
        truncated: false,
        artifact_ref: None,
    }
}

impl LocalPersistentProcessHandleV1 {
    fn handle_resource_receipt(&self) -> ExecutionResourceReceiptV1 {
        resource_receipt_from_prepared(&self.prepared)
    }
}

fn resource_receipt_from_prepared(prepared: &PreparedLocalRunV1) -> ExecutionResourceReceiptV1 {
    ExecutionResourceReceiptV1 {
        physical_attempt_id: prepared.attempt_id.clone(),
        manifest_hash: prepared.draft.draft_hash,
        sandbox_binding_hash: prepared.launch_plan.launch_plan_hash,
        requested_enforcement: prepared.requested_enforcement.clone(),
        effective_enforcement: EffectiveEnforcementV1 {
            backend: prepared.effective_backend,
            completeness: prepared.enforcement_completeness,
            effective_capability_set_hash: prepared.effective_capability_set_hash,
            access_widening_set_hash: zero_hash(),
            functional_probe_hash: prepared.launch_plan.launch_plan_hash,
            proof_set_hash: prepared.enforcement_proof_hash,
        },
        resources: vec![prepared.resource_receipt.clone()],
        enforcement_proof_set_hash: prepared.launch_plan.launch_plan_hash,
        environment_binding_hash: prepared.environment_binding_hash,
        cleanup_status: ResourceCleanupStatusV1::Released,
        effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
    }
}

struct LocalPersistentPtyProcessHandleV1 {
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    process_owner: Option<sigil_process::ProcessTreeOwnerGuard>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    stdin: Arc<Mutex<Option<Box<dyn std::io::Write + Send>>>>,
    stdin_open: Arc<AtomicBool>,
    frame_rx: Option<tokio::sync::mpsc::Receiver<BoundedProcessOutputFrameV1>>,
    stdout_cap: Arc<Mutex<CapState>>,
    stderr_cap: Arc<Mutex<CapState>>,
    attempt_id: PhysicalAttemptId,
    prepared: PreparedLocalRunV1,
    cancelled: Arc<AtomicBool>,
    process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>,
    process_claim: Option<sigil_resource_authority::AuthorityProcessInventoryClaimV1>,
}

#[async_trait]
impl ManagedProcessHandleV1 for LocalPersistentPtyProcessHandleV1 {
    fn process_ref(&self) -> ReflectiveOpaqueProcessRef {
        ReflectiveOpaqueProcessRef::new(OpaqueProcessRef::new(format!(
            "process-{}",
            self.attempt_id.as_str()
        )))
    }

    fn physical_attempt_id(&self) -> PhysicalAttemptId {
        self.attempt_id.clone()
    }

    fn take_output_stream(
        &mut self,
    ) -> Result<Box<dyn ManagedProcessOutputStreamV1>, ManagedProcessControlErrorV1> {
        let Some(rx) = self.frame_rx.take() else {
            return Err(ManagedProcessControlErrorV1::StreamAlreadyTaken);
        };
        Ok(Box::new(LocalPersistentOutputStreamV1 { rx }))
    }

    async fn write_stdin(
        &mut self,
        input: BoundedProcessInputV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        if !self.stdin_open.load(Ordering::SeqCst) {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "write_stdin",
            });
        }
        let mut guard =
            self.stdin
                .lock()
                .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                    action: "write_stdin",
                })?;
        let Some(writer) = guard.as_mut() else {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "write_stdin",
            });
        };
        use std::io::Write;
        writer
            .write_all(&input.payload)
            .and_then(|_| writer.flush())
            .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                action: "write_stdin",
            })?;
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::WriteStdin,
        ))
    }

    async fn resize_pty(
        &mut self,
        size: BoundedPtySizeV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        if size.rows == 0 || size.cols == 0 {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "resize_pty",
            });
        }
        let guard = self
            .master
            .lock()
            .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                action: "resize_pty",
            })?;
        let Some(master) = guard.as_ref() else {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "resize_pty",
            });
        };
        master
            .resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                action: "resize_pty",
            })?;
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::ResizePty,
        ))
    }

    async fn close_stdin(
        &mut self,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        if !self.stdin_open.swap(false, Ordering::SeqCst) {
            return Err(ManagedProcessControlErrorV1::InvalidState {
                action: "close_stdin",
            });
        }
        let mut guard =
            self.stdin
                .lock()
                .map_err(|_| ManagedProcessControlErrorV1::InvalidState {
                    action: "close_stdin",
                })?;
        guard.take();
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::WriteStdin,
        ))
    }

    async fn cancel(
        &mut self,
        _reason: ProcessCancelReasonV1,
    ) -> Result<ProcessControlReceiptV1, ManagedProcessControlErrorV1> {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(process_owner) = self.process_owner.as_ref() {
            process_owner
                .terminate()
                .map_err(|_| ManagedProcessControlErrorV1::InvalidState { action: "cancel" })?;
        }
        let mut child = self
            .child
            .lock()
            .map_err(|_| ManagedProcessControlErrorV1::InvalidState { action: "cancel" })?;
        let _ = child.kill();
        Ok(control_receipt(
            &self.attempt_id,
            ProcessControlActionV1::Cancel,
        ))
    }

    async fn wait_and_finalize(
        mut self: Box<Self>,
    ) -> Result<ManagedExecutionReceiptV1, ManagedExecutionErrorV1> {
        let (termination, reaped) = {
            let mut child = self
                .child
                .lock()
                .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
            match child.wait() {
                Ok(status) if self.cancelled.load(Ordering::SeqCst) => {
                    let _ = status;
                    (ProcessTerminationV1::Cancelled, true)
                }
                Ok(status) => (classify_pty_status(status), true),
                Err(_) => (
                    ProcessTerminationV1::OutcomeUncertain {
                        evidence_digest: zero_hash(),
                    },
                    false,
                ),
            }
        };
        if reaped {
            let claim = self
                .process_claim
                .take()
                .ok_or(ManagedExecutionErrorV1::OutcomeUncertain)?;
            self.process_inventory
                .settle_spawn(claim)
                .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
        }
        // Closing the master releases the PTY reader after the child has been reaped.
        self.master
            .lock()
            .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?
            .take();
        if let Some(mut rx) = self.frame_rx.take() {
            while let Some(frame) = rx.recv().await {
                let _ = frame;
            }
        }
        let stdout_summary = self
            .stdout_cap
            .lock()
            .map(|cap| cap.summary())
            .unwrap_or_else(|_| zero_summary());
        let stderr_summary = self
            .stderr_cap
            .lock()
            .map(|cap| cap.summary())
            .unwrap_or_else(|_| zero_summary());
        Ok(ManagedExecutionReceiptV1 {
            physical_attempt_id: self.attempt_id.clone(),
            process: process_receipt_from(
                &self.prepared,
                termination,
                stdout_summary,
                stderr_summary,
            ),
            resources: resource_receipt_from_prepared(&self.prepared),
            check: None,
        })
    }
}

fn classify_pty_status(status: portable_pty::ExitStatus) -> ProcessTerminationV1 {
    if let Some(signal) = status.signal() {
        return ProcessTerminationV1::Signaled {
            signal: pty_signal_number(signal),
        };
    }
    ProcessTerminationV1::Exited {
        code: i32::try_from(status.exit_code()).unwrap_or(-1),
    }
}

fn pty_signal_number(signal: &str) -> u32 {
    match signal.to_ascii_uppercase().as_str() {
        "SIGKILL" | "KILLED" => 9,
        "SIGTERM" | "TERMINATED" => 15,
        "SIGINT" | "INTERRUPTED" => 2,
        "SIGHUP" | "HANGUP" => 1,
        _ => 0,
    }
}

#[cfg(test)]
#[path = "tests/managed_tests.rs"]
mod tests;
