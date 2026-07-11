use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::*;
use crate::provider::ToolCall;

struct DynamicNetworkTool;

#[async_trait]
impl Tool for DynamicNetworkTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "dynamic_network".to_owned(),
            description: "dynamic network effect".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Unknown),
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_network_effect(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<Option<NetworkEffect>> {
        match args.get("effect").and_then(Value::as_str) {
            Some("read") => Ok(Some(NetworkEffect::Read)),
            Some("mutate") => Ok(Some(NetworkEffect::Mutate)),
            Some("unknown") => Ok(Some(NetworkEffect::Unknown)),
            _ => Err(anyhow!("missing supported effect")),
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "dynamic_network",
            "ok",
            ToolResultMeta::default(),
        ))
    }
}

fn dynamic_call(effect: &str) -> ToolCall {
    ToolCall {
        id: format!("call-{effect}"),
        name: "dynamic_network".to_owned(),
        args_json: json!({"effect": effect}).to_string(),
    }
}

#[test]
fn registry_and_scoped_registry_forward_dynamic_network_effect() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(DynamicNetworkTool));
    let scoped = registry.scoped(ToolRegistryScope::from_names_and_prefixes(
        ["dynamic_network"],
        std::iter::empty::<&str>(),
    ));
    let ctx = ToolContext::new(".", 30);

    assert_eq!(
        registry.permission_network_effect(&ctx, &dynamic_call("read"))?,
        Some(NetworkEffect::Read)
    );
    assert_eq!(
        scoped.permission_network_effect(&ctx, &dynamic_call("mutate"))?,
        Some(NetworkEffect::Mutate)
    );
    assert!(
        scoped
            .permission_network_effect(&ctx, &dynamic_call("invalid"))
            .is_err()
    );
    Ok(())
}

#[test]
fn tool_context_network_authorization_defaults_and_fails_closed() {
    let default = ToolContext::new(".", 30);
    assert_eq!(default.network_policy(), crate::NetworkPolicy::Allow);
    assert!(!default.explicit_network_approval());

    let approved = default
        .clone()
        .with_network_authorization(crate::NetworkPolicy::Ask, true);
    assert_eq!(approved.network_policy(), crate::NetworkPolicy::Ask);
    assert!(approved.explicit_network_approval());

    let non_ask = default.with_network_authorization(crate::NetworkPolicy::Allow, true);
    assert_eq!(non_ask.network_policy(), crate::NetworkPolicy::Allow);
    assert!(!non_ask.explicit_network_approval());
}

fn tool_spec_json(access: &str, network_effect: Option<&str>) -> Value {
    let mut value = json!({
        "name": "legacy_network",
        "description": "legacy network tool",
        "input_schema": {"type": "object"},
        "category": "custom",
        "access": access,
        "preview": "none"
    });
    if let Some(network_effect) = network_effect {
        value["network_effect"] = Value::String(network_effect.to_owned());
    }
    value
}

#[test]
fn tool_access_is_strict_while_legacy_tool_spec_upcasts_contextually() -> Result<()> {
    assert!(serde_json::from_value::<ToolAccess>(json!("network")).is_err());

    let legacy: ToolSpec = serde_json::from_value(tool_spec_json("network", None))?;
    assert_eq!(legacy.access, ToolAccess::Read);
    assert_eq!(legacy.network_effect, Some(NetworkEffect::Unknown));
    let serialized = serde_json::to_value(&legacy)?;
    assert_eq!(serialized["access"], "read");
    assert_eq!(serialized["network_effect"], "unknown");
    assert_ne!(serialized["access"], "network");
    Ok(())
}

#[test]
fn tool_spec_v2_and_mixed_legacy_wire_are_conservative() -> Result<()> {
    let current: ToolSpec = serde_json::from_value(tool_spec_json("read", Some("read")))?;
    assert_eq!(current.access, ToolAccess::Read);
    assert_eq!(current.network_effect, Some(NetworkEffect::Read));

    let mixed: ToolSpec = serde_json::from_value(tool_spec_json("network", Some("read")))?;
    assert_eq!(mixed.access, ToolAccess::Read);
    assert_eq!(mixed.network_effect, Some(NetworkEffect::Unknown));
    assert_eq!(serde_json::to_value(mixed)?["access"], "read");
    Ok(())
}
