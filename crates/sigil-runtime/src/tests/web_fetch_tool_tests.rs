use async_trait::async_trait;
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    NetworkEffect, PreEgressDisclosure, RootConfig, ToolAccess, ToolRegistry,
};

use super::*;

struct AcceptingPresenter;

fn current_root() -> RootConfig {
    toml::from_str(
        r#"config_version = 2

[agent]
connection = "local"
model = "test"

[connections.local]
label = "Local"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }
"#,
    )
    .expect("current root config should parse")
}

#[async_trait]
impl EgressDisclosurePresenter for AcceptingPresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        disclosure.presentation_receipt("webfetch-public-tool-test")
    }
}

#[test]
fn public_webfetch_registration_tracks_web_enabled_and_exposes_capability_only_input() {
    let mut enabled = current_root();
    enabled.web.enabled = true;
    let mut registry = ToolRegistry::new();
    register_web_fetch_tool(&mut registry, &enabled, Arc::new(AcceptingPresenter));
    let spec = registry
        .spec_for("webfetch")
        .expect("enabled Web V1 must expose webfetch");
    assert_eq!(spec.access, ToolAccess::Read);
    assert_eq!(spec.network_effect, Some(NetworkEffect::Read));
    assert!(spec.description.contains("do not fan out"));
    assert!(spec.description.contains("explicitly asks"));
    assert_eq!(
        spec.input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required fields"),
        &[Value::String("source_id".to_owned())]
    );
    assert!(
        spec.input_schema.pointer("/properties/url").is_none(),
        "public webfetch must not accept a novel raw URL"
    );

    let mut disabled = enabled;
    disabled.web.enabled = false;
    let mut registry = ToolRegistry::new();
    register_web_fetch_tool(&mut registry, &disabled, Arc::new(AcceptingPresenter));
    assert!(registry.spec_for("webfetch").is_none());
}
