use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::json;

use super::*;
use crate::{ToolCategory, ToolPreviewCapability};

fn network_spec(effect: NetworkEffect) -> ToolSpec {
    ToolSpec {
        name: "network_tool".to_owned(),
        description: "network tool".to_owned(),
        input_schema: json!({"type": "object"}),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: Some(effect),
        preview: ToolPreviewCapability::None,
    }
}

fn network_decision(
    permission_mode: PermissionMode,
    network_policy: NetworkPolicy,
    effect: NetworkEffect,
    source_default: Option<ApprovalMode>,
) -> Result<PermissionDecision> {
    let config = PermissionConfig {
        mode: permission_mode,
        ..PermissionConfig::default()
    };
    let context = PermissionEvaluationContext {
        network_policy,
        ..PermissionEvaluationContext::default()
    };
    PermissionPolicy::new_with_context(&config, &context)
        .decide_with_operation_network_effect_and_default(
            &network_spec(effect),
            "network_tool",
            ToolAccess::Read,
            ToolOperation::NetworkRequest,
            Some(effect),
            vec![ToolSubject::mcp_tool("network_tool")],
            source_default,
        )
}

#[test]
fn read_only_network_read_follows_independent_policy_matrix() -> Result<()> {
    for (policy, expected) in [
        (NetworkPolicy::Allow, ApprovalMode::Allow),
        (NetworkPolicy::Ask, ApprovalMode::Ask),
        (NetworkPolicy::Deny, ApprovalMode::Deny),
    ] {
        let decision =
            network_decision(PermissionMode::ReadOnly, policy, NetworkEffect::Read, None)?;
        assert_eq!(decision.access, ToolAccess::Read);
        assert_eq!(decision.network_effect, Some(NetworkEffect::Read));
        assert_eq!(decision.local_policy_decision, ApprovalMode::Allow);
        assert_eq!(decision.network_policy_decision, expected);
        assert_eq!(decision.source_policy_decision, ApprovalMode::Allow);
        assert_eq!(decision.mode, expected);
        assert_eq!(decision.risk, PermissionRisk::High);
    }
    Ok(())
}

#[test]
fn read_only_denies_mutating_or_unknown_network_effects() -> Result<()> {
    for effect in [NetworkEffect::Mutate, NetworkEffect::Unknown] {
        for policy in [
            NetworkPolicy::Allow,
            NetworkPolicy::Ask,
            NetworkPolicy::Deny,
        ] {
            let decision = network_decision(PermissionMode::ReadOnly, policy, effect, None)?;
            assert_eq!(decision.network_policy_decision, ApprovalMode::Deny);
            assert_eq!(decision.mode, ApprovalMode::Deny);
            assert_eq!(decision.risk, PermissionRisk::High);
        }
    }
    Ok(())
}

#[test]
fn non_read_only_modes_meet_network_ask_or_deny_including_danger() -> Result<()> {
    for permission_mode in [
        PermissionMode::Manual,
        PermissionMode::AutoEdit,
        PermissionMode::DangerFullAccess,
    ] {
        for (network_policy, expected) in [
            (NetworkPolicy::Allow, ApprovalMode::Allow),
            (NetworkPolicy::Ask, ApprovalMode::Ask),
            (NetworkPolicy::Deny, ApprovalMode::Deny),
        ] {
            let decision = network_decision(
                permission_mode,
                network_policy,
                NetworkEffect::Unknown,
                None,
            )?;
            assert_eq!(decision.network_policy_decision, expected);
            assert_eq!(decision.mode, expected);
        }
    }
    Ok(())
}

#[test]
fn danger_does_not_override_parent_cap_or_source_deny() -> Result<()> {
    let config = PermissionConfig {
        mode: PermissionMode::DangerFullAccess,
        ..PermissionConfig::default()
    };
    let context = PermissionEvaluationContext {
        effective_policy_cap: Some(EffectivePermissionPolicyCap {
            policy_hash: "parent-deny".to_owned(),
            mode: ApprovalMode::Deny,
        }),
        network_policy: NetworkPolicy::Allow,
        ..PermissionEvaluationContext::default()
    };
    let parent_denied = PermissionPolicy::new_with_context(&config, &context).decide(
        &network_spec(NetworkEffect::Read),
        "network_tool",
        Vec::new(),
    )?;
    assert_eq!(parent_denied.local_policy_decision, ApprovalMode::Deny);
    assert_eq!(parent_denied.mode, ApprovalMode::Deny);

    let source_denied = network_decision(
        PermissionMode::DangerFullAccess,
        NetworkPolicy::Allow,
        NetworkEffect::Read,
        Some(ApprovalMode::Deny),
    )?;
    assert_eq!(source_denied.source_policy_decision, ApprovalMode::Deny);
    assert_eq!(source_denied.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn explicit_tool_policy_can_override_delegated_source_default() -> Result<()> {
    let config = PermissionConfig {
        tools: BTreeMap::from([("network_tool".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };
    let decision = PermissionPolicy::new(&config).decide_with_access_and_default(
        &network_spec(NetworkEffect::Unknown),
        "network_tool",
        ToolAccess::Read,
        vec![ToolSubject::mcp_tool("network_tool")],
        Some(ApprovalMode::Ask),
    )?;

    assert_eq!(decision.local_policy_decision, ApprovalMode::Allow);
    assert_eq!(decision.source_policy_decision, ApprovalMode::Allow);
    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn session_grants_cover_local_ask_and_read_only_network_ask_facets() -> Result<()> {
    let subjects = vec![ToolSubject::path("src/lib.rs", "src/lib.rs")];
    assert!(tool_approval_session_grant_available_for_facets(
        ToolAccess::Write,
        None,
        ToolOperation::EditFile,
        PermissionRisk::Medium,
        &subjects,
        &[PathTrustZone::WorkspaceSource],
        None,
        false,
        ApprovalMode::Ask,
        ApprovalMode::Allow,
        ApprovalMode::Allow,
    ));
    assert!(!tool_approval_session_grant_available_for_facets(
        ToolAccess::Write,
        Some(NetworkEffect::Read),
        ToolOperation::EditFile,
        PermissionRisk::Medium,
        &subjects,
        &[PathTrustZone::WorkspaceSource],
        None,
        false,
        ApprovalMode::Ask,
        ApprovalMode::Allow,
        ApprovalMode::Allow,
    ));
    assert!(tool_approval_session_grant_available_for_facets(
        ToolAccess::Read,
        Some(NetworkEffect::Read),
        ToolOperation::NetworkRequest,
        PermissionRisk::High,
        &[ToolSubject::mcp_tool("builtin:websearch")],
        &[PathTrustZone::Unknown],
        None,
        false,
        ApprovalMode::Allow,
        ApprovalMode::Ask,
        ApprovalMode::Allow,
    ));
    for (effect, source) in [
        (NetworkEffect::Mutate, ApprovalMode::Allow),
        (NetworkEffect::Unknown, ApprovalMode::Allow),
        (NetworkEffect::Read, ApprovalMode::Ask),
        (NetworkEffect::Read, ApprovalMode::Deny),
    ] {
        assert!(!tool_approval_session_grant_available_for_facets(
            ToolAccess::Read,
            Some(effect),
            ToolOperation::NetworkRequest,
            PermissionRisk::High,
            &[ToolSubject::mcp_tool("network_tool")],
            &[PathTrustZone::Unknown],
            None,
            false,
            ApprovalMode::Allow,
            ApprovalMode::Ask,
            source,
        ));
    }
    Ok(())
}
