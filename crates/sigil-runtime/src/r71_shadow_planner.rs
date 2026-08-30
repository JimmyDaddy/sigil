//! RFC-0071 R71.1: side-effect-free shadow planner (isolated qualification only).
//!
//! The shadow planner implements the kernel ManagedExecutionPlannerV1 port with no filesystem
//! mutation, no session-log write and no V3 event. It is registered only in the isolated
//! qualification composition; production keeps the legacy planner until R71.6. Its drafts are
//! fed to deterministic hash fixtures so that a cache miss / restart can recompute a stable
//! digest instead of fabricating an approved requirement.

use std::collections::BTreeSet;

use sigil_kernel::managed_execution::{
    ExecutionPurposeV1, ManagedExecutionPlanDraftV1, ManagedExecutionPlanErrorV1,
    ManagedExecutionPlanRequestV1, ManagedExecutionPlannerV1,
};
use sigil_kernel::resource::{
    CanonicalHash, EnvironmentProfileClassV1, OpaqueExecutionPlanDraftId, OpaqueRequirementId,
    OpaqueWorkspaceId, ResourceAccessV1, ResourceBlockerScopeV1, ResourceCleanupPolicyV1,
    ResourceJournalScopeV1, ResourceKindV1, ResourceLeaseLifetimeV1, ResourceOwnerScopeV1,
    ResourcePurposeV1, ResourceQuotaClassV1, ResourceQuotaProfileV1, ResourceRequirementKeyV1,
    ResourceRequirementSetV1, ResourceRequirementV1, ResourceRetentionPolicyV1,
    ResourceVisibilityV1,
};

/// Draft of a shadow plan; the plan hash never depends on host path separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowPlannerConfigV1 {
    pub schema_version: u32,
    pub workspace_id: String,
    pub capture_product_defaults: bool,
    /// The current managed Local binder can only prove explicit unconfined execution. It must
    /// not advertise the isolated profile until a real platform binder is injected.
    pub local_execution_explicit_unconfined: bool,
}

impl Default for ShadowPlannerConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            workspace_id: "shadow-workspace".to_owned(),
            capture_product_defaults: true,
            local_execution_explicit_unconfined: true,
        }
    }
}

/// Canonical digest helper: stable field order, platform-independent encoding.
pub fn canonical_digest(payload: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn purpose_class(purpose: ExecutionPurposeV1) -> &'static str {
    match purpose {
        ExecutionPurposeV1::OneShot => "one-shot",
        ExecutionPurposeV1::Terminal => "terminal",
        ExecutionPurposeV1::ExtensionProcess => "extension",
        ExecutionPurposeV1::CodeIntelProcess => "code-intel",
    }
}

/// Side-effect-free shadow planner.
#[derive(Debug, Clone)]
pub struct ShadowPlannerV1 {
    config: ShadowPlannerConfigV1,
}

impl ShadowPlannerV1 {
    pub const fn new(config: ShadowPlannerConfigV1) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ShadowPlannerConfigV1 {
        &self.config
    }

    /// Resolves pathless resource requirements for one execution purpose. Pure computation:
    /// logical intent only; physical realization happens after allow.
    fn resource_requirements_for(&self, purpose: ExecutionPurposeV1) -> ResourceRequirementSetV1 {
        let kind = match purpose {
            ExecutionPurposeV1::OneShot | ExecutionPurposeV1::Terminal => {
                ResourceKindV1::ExecutionTemp
            }
            ExecutionPurposeV1::ExtensionProcess | ExecutionPurposeV1::CodeIntelProcess => {
                ResourceKindV1::RuntimeState
            }
        };
        let lifetime = match purpose {
            ExecutionPurposeV1::OneShot => ResourceLeaseLifetimeV1::ToolCall,
            ExecutionPurposeV1::Terminal => ResourceLeaseLifetimeV1::TerminalTask,
            ExecutionPurposeV1::ExtensionProcess | ExecutionPurposeV1::CodeIntelProcess => {
                ResourceLeaseLifetimeV1::ExtensionProcess
            }
        };
        let quota = ResourceQuotaProfileV1 {
            class: ResourceQuotaClassV1::AttemptEphemeral,
            max_bytes: 512 * 1024 * 1024,
            max_entries: 100_000,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: canonical_digest(b"shadow-quota-attempt-ephemeral-v1"),
        };
        let requirement = ResourceRequirementV1 {
            requirement_id: OpaqueRequirementId::new(format!("shadow-{}", purpose_class(purpose))),
            physical_owner_scope: ResourceOwnerScopeV1::Application,
            stable_key: ResourceRequirementKeyV1 {
                blocker_scope: ResourceBlockerScopeV1::Workspace(OpaqueWorkspaceId::new(
                    self.config.workspace_id.clone(),
                )),
                kind,
                purpose: ResourcePurposeV1::ExecutionPrerequisite,
                access: BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]),
                lease_lifetime: lifetime,
                quota_profile: quota.clone(),
                retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
                cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
                environment_class: if self.config.local_execution_explicit_unconfined {
                    EnvironmentProfileClassV1::ExplicitUnconfined
                } else {
                    EnvironmentProfileClassV1::FreshIsolatedHome
                },
                toolchain_class: None,
                subject_binding_hash: None,
                canonical_hash: canonical_digest(b"shadow-stable-key-1"),
            },
            kind,
            lease_lifetime: lifetime,
            access: BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]),
            purpose: ResourcePurposeV1::ExecutionPrerequisite,
            visibility: ResourceVisibilityV1::HostOnly,
            quota_profile: quota,
            retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
            cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
            implicit: true,
        };
        // Collection is bounded by the schema constant: build a Vec then convert via try_from.
        ResourceRequirementSetV1 {
            schema_version: 1,
            requirements: sigil_kernel::resource::BoundedVec::<_, 64>::try_from_vec(vec![
                requirement,
            ])
            .expect("shadow requirement fits bounded schema"),
            canonical_hash: canonical_digest(b"shadow-requirement-set-1"),
        }
    }
}

impl ManagedExecutionPlannerV1 for ShadowPlannerV1 {
    fn plan_execution(
        &self,
        request: ManagedExecutionPlanRequestV1,
    ) -> Result<ManagedExecutionPlanDraftV1, ManagedExecutionPlanErrorV1> {
        let argv_digest = canonical_digest(
            request
                .argv
                .iter()
                .flat_map(|arg| arg.as_os_str().as_encoded_bytes().to_vec())
                .collect::<Vec<_>>()
                .as_slice(),
        );
        let resource_requirements = self.resource_requirements_for(request.purpose);
        let attempt_scope = ResourceJournalScopeV1::Workspace(OpaqueWorkspaceId::new(
            self.config.workspace_id.clone(),
        ));
        let capture_policy_hash = canonical_digest(
            format!(
                "capture:{:?}:{:?}:{}",
                request.capture, request.limits, self.config.capture_product_defaults
            )
            .as_bytes(),
        );
        let environment_digest =
            sigil_kernel::managed_execution::canonical_environment_digest(&request.environment);
        let draft_id = OpaqueExecutionPlanDraftId::new(format!(
            "shadow-draft-{}-{}",
            request
                .argv
                .first()
                .and_then(|arg| arg.to_str())
                .unwrap_or("cmd"),
            self.config.schema_version
        ));
        let environment_profile_class = match request.purpose {
            ExecutionPurposeV1::ExtensionProcess | ExecutionPurposeV1::CodeIntelProcess => {
                EnvironmentProfileClassV1::ExtensionProcess
            }
            _ if self.config.local_execution_explicit_unconfined => {
                EnvironmentProfileClassV1::ExplicitUnconfined
            }
            _ => EnvironmentProfileClassV1::FreshIsolatedHome,
        };
        let draft = ManagedExecutionPlanDraftV1 {
            draft_id,
            argv_digest,
            structured_command_digest: request.structured_command_digest,
            cwd_subject_binding_hash: canonical_digest(request.cwd_subject_ref.as_str().as_bytes()),
            attempt_journal_scope: attempt_scope,
            attempt_journal_scope_hash: canonical_digest(b"shadow-attempt-scope-hash-1"),
            resource_plan_hash: canonical_digest(b"shadow-resource-plan-hash-1"),
            resource_requirements,
            environment_profile: sigil_kernel::managed_execution::EnvironmentProfileRefV1 {
                profile_class: environment_profile_class,
                profile_hash: canonical_digest(b"shadow-env-profile-1"),
            },
            toolchain_plan_hash: CanonicalHash::from_bytes([0u8; 32]),
            resolver_proof_digest: canonical_digest(b"shadow-resolver-proof-1"),
            sandbox_preview_hash: canonical_digest(b"shadow-sandbox-preview-1"),
            sandbox_binder_registration_hash: CanonicalHash::from_bytes([0u8; 32]),
            sandbox_provider_generation: 0,
            capture_policy_hash,
            resource_limits_hash: canonical_digest(b"shadow-resource-limits-1"),
            environment_digest,
            draft_hash: canonical_digest(
                &[
                    argv_digest.as_bytes().as_slice(),
                    environment_digest.as_bytes().as_slice(),
                ]
                .concat(),
            ),
        };
        Ok(draft)
    }
}

#[cfg(test)]
#[path = "tests/r71_shadow_planner_tests.rs"]
mod tests;
