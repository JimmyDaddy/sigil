use super::*;

const AGENT_INVOCATION_GRANT_TTL_MS: u64 = 30 * 60 * 1_000;

pub(super) fn tool_scope_summary(scope: &sigil_kernel::ToolRegistryScope) -> String {
    if scope.allow_all {
        return "all tools".to_owned();
    }
    let names = scope.names.iter().cloned().collect::<Vec<_>>().join(",");
    let prefixes = scope.prefixes.join(",");
    if names.is_empty() && prefixes.is_empty() {
        "no tools".to_owned()
    } else if prefixes.is_empty() {
        format!("names={names}")
    } else if names.is_empty() {
        format!("prefixes={prefixes}")
    } else {
        format!("names={names}; prefixes={prefixes}")
    }
}

pub(super) fn tool_contracts_are_safe_readonly_for_auto_spawn(
    contracts: &[sigil_kernel::ToolRuntimeContract],
) -> bool {
    !contracts.is_empty()
        && contracts.iter().all(|contract| {
            contract.spec.access == ToolAccess::Read
                && contract.spec.network_effect.is_none()
                && contract.mutation_tracking == sigil_kernel::ToolMutationTracking::None
                && matches!(
                    contract.spec.category,
                    ToolCategory::File | ToolCategory::Search | ToolCategory::Custom
                )
        })
}

pub(crate) fn tool_registry_is_safe_readonly_for_auto_spawn(registry: &ToolRegistry) -> bool {
    tool_contracts_are_safe_readonly_for_auto_spawn(&registry.contracts())
}

pub(super) fn admit_model_agent_spawn(
    mode: MultiAgentMode,
    authority: &DelegationAuthority,
    profile: &ResolvedAgentProfile,
    child_registry: &ToolRegistry,
) -> Result<()> {
    match authority {
        DelegationAuthority::SystemRecovery => bail!(
            "system recovery may reconcile prior work but cannot create forward-effect child authority"
        ),
        DelegationAuthority::UserExplicit
        | DelegationAuthority::AcceptedTaskPlan { .. }
        | DelegationAuthority::TaskOrchestrator { .. }
        | DelegationAuthority::ModelProactive => {}
    }
    match mode {
        MultiAgentMode::None => {
            bail!("model agent spawn is disabled by [task].multi_agent_mode=none")
        }
        MultiAgentMode::ExplicitRequestOnly => {
            if matches!(
                authority,
                DelegationAuthority::UserExplicit
                    | DelegationAuthority::AcceptedTaskPlan { .. }
                    | DelegationAuthority::TaskOrchestrator { .. }
            ) {
                return Ok(());
            }
            bail!(
                "model agent spawn requires explicit user or accepted task-plan authority under [task].multi_agent_mode=explicit_request_only"
            )
        }
        MultiAgentMode::Proactive => {}
    }

    if !matches!(authority, DelegationAuthority::ModelProactive) {
        return Ok(());
    }
    let is_builtin_explore = profile.id().as_str() == crate::EXPLORE_PROFILE_ID
        && profile.source == AgentProfileSource::System
        && profile.execution_role == AgentRole::SubagentRead;
    if !is_builtin_explore {
        bail!("proactive model delegation is limited to the trusted built-in explore profile")
    }
    if !tool_registry_is_safe_readonly_for_auto_spawn(child_registry) {
        bail!("proactive explore requires a resolved read-only, local, non-agent tool contract")
    }
    Ok(())
}

pub(crate) fn delegation_admission_entry(
    grant: &AgentInvocationGrant,
    thread_id: AgentThreadId,
    profile_id: AgentProfileId,
    invocation_mode: AgentInvocationMode,
    invocation_source: AgentInvocationSource,
    objective: &str,
) -> Result<sigil_kernel::AgentDelegationAdmissionEntry> {
    let binding = grant.binding();
    Ok(sigil_kernel::AgentDelegationAdmissionEntry {
        thread_id,
        profile_id,
        invocation_mode,
        invocation_source,
        authority: sigil_kernel::DelegationAuthorityRecord::from(&binding.authority),
        objective_hash: hash_text(&sigil_kernel::safe_persistence_text(objective)),
        tool_contract_fingerprint: binding.tool_contract_fingerprint.clone(),
        invocation_grant: Some(grant.durable_record()?),
        admitted_at_ms: None,
    })
}

pub(super) fn resolved_tool_contract_fingerprint(registry: &ToolRegistry) -> Result<String> {
    registry.contract_fingerprint()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn mint_agent_invocation_grant(
    context: AgentDelegationRunContext,
    root_logical_run_id: &str,
    root_cancellation: &sigil_kernel::RunCancellationHandle,
    profile_id: AgentProfileId,
    role: AgentRole,
    isolation: TaskIsolationMode,
    child_registry: &ToolRegistry,
    options: &AgentRunOptions,
    now_ms: u64,
) -> Result<AgentInvocationGrant> {
    if root_cancellation.is_cancel_requested() {
        bail!("root run cancelled before child invocation grant minting");
    }
    let mut permission_upper_bound = options.permission_config.clone();
    if matches!(role, AgentRole::Planner | AgentRole::SubagentRead)
        || matches!(context.authority, DelegationAuthority::ModelProactive)
    {
        permission_upper_bound.mode = PermissionMode::ReadOnly;
        permission_upper_bound.external_directory.enabled = false;
        permission_upper_bound.external_directory.default_mode = ApprovalMode::Deny;
        permission_upper_bound.external_directory.rules.clear();
    }
    let network_upper_bound = if matches!(context.authority, DelegationAuthority::ModelProactive) {
        NetworkPolicy::Deny
    } else {
        options.permission_context.network_policy
    };
    AgentInvocationGrant::mint(
        AgentInvocationGrantBinding {
            source: context.source,
            authority: context.authority,
            root_logical_run_id: root_logical_run_id.to_owned(),
            profile_id,
            role,
            isolation,
            permission_upper_bound,
            network_upper_bound,
            tool_contract_fingerprint: resolved_tool_contract_fingerprint(child_registry)?,
            workspace_snapshot_id: sigil_kernel::agent_invocation_workspace_snapshot_id(
                &options.workspace_root,
            )?,
            root_cancellation_scope_id: root_cancellation.scope_id().to_owned(),
            expires_at_ms: now_ms.saturating_add(AGENT_INVOCATION_GRANT_TTL_MS),
        },
        now_ms,
    )
}

pub(crate) fn revalidate_agent_invocation_grant(
    grant: &AgentInvocationGrant,
    context: &AgentDelegationRunContext,
    root_logical_run_id: &str,
    root_cancellation: &sigil_kernel::RunCancellationHandle,
    profile_id: &AgentProfileId,
    role: AgentRole,
    isolation: TaskIsolationMode,
    child_registry: &ToolRegistry,
    workspace_root: &Path,
    now_ms: u64,
) -> Result<()> {
    if root_cancellation.is_cancel_requested() {
        bail!("root run cancelled before child provider admission");
    }
    grant.validate_invocation(
        &context.source,
        &context.authority,
        root_logical_run_id,
        profile_id,
        role,
        isolation,
        &resolved_tool_contract_fingerprint(child_registry)?,
        &sigil_kernel::agent_invocation_workspace_snapshot_id(workspace_root)?,
        root_cancellation.scope_id(),
        now_ms,
    )
}

pub(super) fn apply_child_permission_constraints(
    child: &mut AgentRunOptions,
    parent: &AgentRunOptions,
    role: AgentRole,
    profile: PermissionConfig,
    grant: &AgentInvocationGrant,
) {
    let mut role_policy = parent.permission_config.clone();
    if matches!(role, AgentRole::Planner | AgentRole::SubagentRead) {
        role_policy.mode = PermissionMode::ReadOnly;
    }
    child.permission_config = parent.permission_config.clone();
    child.permission_context = parent.permission_context.clone();
    child
        .permission_context
        .delegated_policy_constraints
        .extend([role_policy, profile, grant.permission_upper_bound().clone()]);
    child.permission_context.network_policy = strictest_network_policy(
        parent.permission_context.network_policy,
        grant.network_upper_bound(),
    );
}

/// Rebuilds the narrowest executable policy for a recovered child that was admitted only because
/// its frozen tool surface was local and read-only. Durable grant records are intentionally not
/// promoted back into runtime capability; recovery therefore denies network and agent delegation
/// and re-applies both the parent and current profile policies.
pub(super) fn apply_recovered_readonly_child_constraints(
    child: &mut AgentRunOptions,
    parent: &AgentRunOptions,
    profile: PermissionConfig,
) {
    let mut readonly = parent.permission_config.clone();
    readonly.mode = PermissionMode::ReadOnly;
    readonly.external_directory.enabled = false;
    readonly.external_directory.default_mode = ApprovalMode::Deny;
    readonly.external_directory.rules.clear();
    child.permission_config = parent.permission_config.clone();
    child.permission_context = parent.permission_context.clone();
    child
        .permission_context
        .delegated_policy_constraints
        .extend([readonly, profile]);
    child.permission_context.network_policy = NetworkPolicy::Deny;
}

fn strictest_network_policy(left: NetworkPolicy, right: NetworkPolicy) -> NetworkPolicy {
    match (left, right) {
        (NetworkPolicy::Deny, _) | (_, NetworkPolicy::Deny) => NetworkPolicy::Deny,
        (NetworkPolicy::Ask, _) | (_, NetworkPolicy::Ask) => NetworkPolicy::Ask,
        (NetworkPolicy::Allow, NetworkPolicy::Allow) => NetworkPolicy::Allow,
    }
}
