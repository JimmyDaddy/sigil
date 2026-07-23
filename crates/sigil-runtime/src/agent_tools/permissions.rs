use super::*;

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
    contracts: &[(ToolSpec, sigil_kernel::ToolMutationTracking)],
) -> bool {
    !contracts.is_empty()
        && contracts.iter().all(|(spec, mutation_tracking)| {
            spec.access == ToolAccess::Read
                && spec.network_effect.is_none()
                && *mutation_tracking == sigil_kernel::ToolMutationTracking::None
                && matches!(
                    spec.category,
                    ToolCategory::File | ToolCategory::Search | ToolCategory::Custom
                )
        })
}

pub(super) fn tool_registry_is_safe_readonly_for_auto_spawn(registry: &ToolRegistry) -> bool {
    tool_contracts_are_safe_readonly_for_auto_spawn(&registry.contracts())
}

pub(super) fn admit_model_agent_spawn(
    mode: MultiAgentMode,
    authority: &DelegationAuthority,
    profile: &ResolvedAgentProfile,
    child_registry: &ToolRegistry,
) -> Result<()> {
    match authority {
        DelegationAuthority::AcceptedTaskPlan { .. } => bail!(
            "accepted task-plan delegation requires an O2 scoped grant bound to a durable plan step"
        ),
        DelegationAuthority::SystemRecovery => bail!(
            "system recovery delegation requires a scoped recovery grant and is not available yet"
        ),
        DelegationAuthority::UserExplicit | DelegationAuthority::ModelProactive => {}
    }
    match mode {
        MultiAgentMode::None => {
            bail!("model agent spawn is disabled by [task].multi_agent_mode=none")
        }
        MultiAgentMode::ExplicitRequestOnly => {
            if matches!(authority, DelegationAuthority::UserExplicit) {
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

pub(super) fn delegation_admission_entry(
    authority: DelegationAuthority,
    thread_id: AgentThreadId,
    profile_id: AgentProfileId,
    invocation_mode: AgentInvocationMode,
    invocation_source: AgentInvocationSource,
    objective: &str,
    child_registry: &ToolRegistry,
) -> Result<sigil_kernel::AgentDelegationAdmissionEntry> {
    let durable_authority = sigil_kernel::DelegationAuthorityRecord::from(&authority);
    let contracts = child_registry
        .contracts()
        .into_iter()
        .map(|(spec, tracking)| {
            json!({
                "spec": spec,
                "mutation_tracking": match tracking {
                    sigil_kernel::ToolMutationTracking::None => "none",
                    sigil_kernel::ToolMutationTracking::Controlled => "controlled",
                    sigil_kernel::ToolMutationTracking::Unknown => "unknown",
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(sigil_kernel::AgentDelegationAdmissionEntry {
        thread_id,
        profile_id,
        invocation_mode,
        invocation_source,
        authority: durable_authority,
        objective_hash: hash_text(&sigil_kernel::safe_persistence_text(objective)),
        tool_contract_fingerprint: hash_text(&serde_json::to_string(&contracts)?),
        admitted_at_ms: None,
    })
}

pub(super) fn apply_child_permission_constraints(
    child: &mut AgentRunOptions,
    parent: &AgentRunOptions,
    role: AgentRole,
    profile: PermissionConfig,
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
        .extend([role_policy, profile]);
}
