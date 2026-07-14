use anyhow::Result;
use serde_json::json;

use super::*;

fn current_approval() -> serde_json::Value {
    json!({
        "action": "requested",
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
        "snapshot_required": false
    })
}

#[test]
fn approval_payload_requires_current_facets_and_rejects_removed_network_access() -> Result<()> {
    let entry: ToolApprovalEntry = serde_json::from_value(current_approval())?;
    assert_eq!(entry.access, ToolAccess::Read);
    assert_eq!(entry.network_effect, Some(NetworkEffect::Read));
    assert_eq!(entry.local_policy_decision, ApprovalMode::Allow);
    assert_eq!(entry.network_policy_decision, ApprovalMode::Deny);
    assert_eq!(entry.source_policy_decision, ApprovalMode::Ask);

    let mut removed_access = current_approval();
    removed_access["access"] = json!("network");
    assert!(serde_json::from_value::<ToolApprovalEntry>(removed_access).is_err());

    let mut incomplete = current_approval();
    incomplete
        .as_object_mut()
        .expect("approval fixture is an object")
        .remove("source_policy_decision");
    assert!(serde_json::from_value::<ToolApprovalEntry>(incomplete).is_err());
    Ok(())
}

#[test]
fn session_grant_requires_current_access_and_scope_fields() -> Result<()> {
    let current = json!({
        "call_id": "grant-call",
        "tool_name": "current-network-tool",
        "access": "read",
        "network_effect": "read",
        "operation": "network_request",
        "risk": "high",
        "subjects": [],
        "facets": ["local"],
        "scope": "exact_subjects",
        "expires": "session",
        "granted_at_ms": 42
    });
    let grant: ToolApprovalSessionGrantEntry = serde_json::from_value(current.clone())?;
    assert_eq!(grant.access, ToolAccess::Read);
    assert_eq!(grant.network_effect, Some(NetworkEffect::Read));

    let mut removed_access = current.clone();
    removed_access["access"] = json!("network");
    assert!(serde_json::from_value::<ToolApprovalSessionGrantEntry>(removed_access).is_err());

    let mut incomplete = current;
    incomplete
        .as_object_mut()
        .expect("grant fixture is an object")
        .remove("scope");
    assert!(serde_json::from_value::<ToolApprovalSessionGrantEntry>(incomplete).is_err());
    Ok(())
}

#[test]
fn control_entry_rejects_removed_approval_payload_shape() {
    let mut removed_access = current_approval();
    removed_access["access"] = json!("network");
    assert!(
        serde_json::from_value::<SessionLogEntry>(json!({
            "control": {"tool_approval": removed_access}
        }))
        .is_err()
    );
}
