use async_trait::async_trait;
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    NetworkEffect, PreEgressDisclosure, RootConfig, SecretString, ToolAccess, ToolAnalysisStatus,
    ToolOperation, ToolPermissionEffect, ToolRegistry, ToolRestartPolicy, ToolSubjectScope,
    WebUrlProvenanceKind,
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

#[test]
fn webfetch_permission_plan_binds_exact_safe_endpoint_once() {
    let args = json!({
        "source_id": "src_exact",
        "format": "markdown",
        "max_content_bytes": 4096
    });
    let capability = sigil_kernel::ResolvedUserUrlCapability::new(
        "session-exact",
        "src_exact",
        SecretString::new("https://example.test/page?token=secret"),
        "https://example.test/page?[redacted]",
        ToolRestartPolicy::InterruptOnRestart,
        WebUrlProvenanceKind::UserMessage,
    );

    let first = webfetch_permission_plan(&args, &capability).expect("webfetch plan");
    let repeated = webfetch_permission_plan(&args, &capability).expect("repeated webfetch plan");

    assert_eq!(first, repeated);
    assert_eq!(first.operation, ToolOperation::NetworkRequest);
    assert_eq!(first.analysis, ToolAnalysisStatus::Complete);
    assert_eq!(
        first.effects,
        std::collections::BTreeSet::from([ToolPermissionEffect::NetworkRead])
    );
    assert_eq!(first.tool_default_mode, None);
    assert_eq!(first.subjects.len(), 1);
    assert_eq!(first.subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        first.subjects[0].normalized,
        "https://example.test/page?[redacted]"
    );
    assert!(!format!("{first:?}").contains("token=secret"));
}
