use anyhow::Result;
use serde_json::{Value, json};

use super::*;

fn legacy_approval(network_effect: Option<&str>) -> Value {
    let mut value = json!({
        "action": "requested",
        "call_id": "legacy-call",
        "tool_name": "legacy-network-tool",
        "access": "network",
        "subjects": [],
        "policy_decision": "ask",
        "external_directory_required": false,
        "user_decision": null,
        "reason": null,
        "preview_hash": null
    });
    if let Some(network_effect) = network_effect {
        value["network_effect"] = Value::String(network_effect.to_owned());
    }
    value
}

#[test]
fn legacy_tool_approval_upcasts_network_axis_and_serializes_v2_only() -> Result<()> {
    let entry: ToolApprovalEntry = serde_json::from_value(legacy_approval(None))?;
    assert_eq!(entry.access, ToolAccess::Read);
    assert_eq!(entry.network_effect, Some(NetworkEffect::Unknown));
    assert_eq!(entry.local_policy_decision, ApprovalMode::Allow);
    assert_eq!(entry.network_policy_decision, ApprovalMode::Ask);
    assert_eq!(entry.source_policy_decision, ApprovalMode::Allow);

    let serialized = serde_json::to_value(&entry)?;
    assert_eq!(serialized["access"], "read");
    assert_eq!(serialized["network_effect"], "unknown");
    assert_ne!(serialized["access"], "network");
    Ok(())
}

#[test]
fn mixed_legacy_tool_approval_cannot_claim_narrower_network_effect() -> Result<()> {
    let entry: ToolApprovalEntry = serde_json::from_value(legacy_approval(Some("read")))?;
    assert_eq!(entry.access, ToolAccess::Read);
    assert_eq!(entry.network_effect, Some(NetworkEffect::Unknown));
    Ok(())
}

#[test]
fn current_tool_approval_round_trips_all_permission_facets() -> Result<()> {
    let value = json!({
        "action": "policy_evaluated",
        "call_id": "current-call",
        "tool_name": "current-network-tool",
        "access": "read",
        "network_effect": "read",
        "local_policy_decision": "allow",
        "network_policy_decision": "deny",
        "source_policy_decision": "ask",
        "operation": "network_request",
        "risk": "high",
        "subjects": [],
        "policy_decision": "deny",
        "external_directory_required": false,
        "user_decision": null,
        "reason": null,
        "preview_hash": null
    });
    let entry: ToolApprovalEntry = serde_json::from_value(value)?;
    assert_eq!(entry.network_effect, Some(NetworkEffect::Read));
    assert_eq!(entry.local_policy_decision, ApprovalMode::Allow);
    assert_eq!(entry.network_policy_decision, ApprovalMode::Deny);
    assert_eq!(entry.source_policy_decision, ApprovalMode::Ask);
    assert_eq!(serde_json::to_value(entry)?["access"], "read");
    Ok(())
}

fn legacy_grant(network_effect: Option<&str>) -> Value {
    let mut value = json!({
        "call_id": "legacy-grant",
        "tool_name": "legacy-network-tool",
        "access": "network",
        "operation": "network_request",
        "risk": "high",
        "subjects": [],
        "expires": "session",
        "granted_at_ms": 42
    });
    if let Some(network_effect) = network_effect {
        value["network_effect"] = Value::String(network_effect.to_owned());
    }
    value
}

#[test]
fn legacy_session_grant_upcasts_unknown_and_never_reserializes_network_access() -> Result<()> {
    let grant: ToolApprovalSessionGrantEntry = serde_json::from_value(legacy_grant(None))?;
    assert_eq!(grant.access, ToolAccess::Read);
    assert_eq!(grant.network_effect, Some(NetworkEffect::Unknown));
    assert_eq!(grant.facets, [crate::ToolApprovalSessionGrantFacet::Local]);
    assert_eq!(
        grant.scope,
        crate::ToolApprovalSessionGrantScope::ExactSubjects
    );
    let serialized = serde_json::to_value(grant)?;
    assert_eq!(serialized["access"], "read");
    assert_eq!(serialized["network_effect"], "unknown");
    assert_ne!(serialized["access"], "network");

    let mixed: ToolApprovalSessionGrantEntry = serde_json::from_value(legacy_grant(Some("read")))?;
    assert_eq!(mixed.network_effect, Some(NetworkEffect::Unknown));
    Ok(())
}

#[test]
fn legacy_session_log_entry_recovers_through_contextual_control_upcaster() -> Result<()> {
    let entry: SessionLogEntry = serde_json::from_value(json!({
        "control": {
            "tool_approval": legacy_approval(None)
        }
    }))?;
    let SessionLogEntry::Control(ControlEntry::ToolApproval(approval)) = entry else {
        panic!("legacy control entry should recover as tool approval");
    };
    assert_eq!(approval.access, ToolAccess::Read);
    assert_eq!(approval.network_effect, Some(NetworkEffect::Unknown));
    Ok(())
}
