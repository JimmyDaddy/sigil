use crate::{
    MultiAgentMode, OptionalCapability, RootConfig, RuntimeCompositionConfig,
    RuntimeCompositionProfile, TaskRoutingPolicy,
};

const CORE_CONFIG: &str = r#"
config_version = 2
[agent]
connection = "fixture"
model = "fixture-model"
[composition]
profile = "core"
"#;

fn config_with_module(raw: &str) -> String {
    format!("{CORE_CONFIG}\n{raw}")
}

#[test]
fn composition_selection_is_closed_and_core_enhancements_are_explicit() {
    let core = RuntimeCompositionConfig::core();
    assert!(core.selected_capabilities().is_empty());
    let standard = RuntimeCompositionConfig::standard();
    assert_eq!(standard.profile, RuntimeCompositionProfile::Standard);
    assert_eq!(
        standard.selected_capabilities().len(),
        OptionalCapability::ALL.len()
    );
    let selected = RuntimeCompositionConfig::new(
        RuntimeCompositionProfile::Core,
        [OptionalCapability::Skills, OptionalCapability::Mcp],
    );
    assert!(selected.allows(OptionalCapability::Skills));
    assert!(selected.allows(OptionalCapability::Mcp));
    assert!(!selected.allows(OptionalCapability::TaskOrchestration));
    assert!(
        toml::from_str::<RuntimeCompositionConfig>(
            "profile = 'core'\nenhancements = ['unknown_module']"
        )
        .is_err()
    );
}

#[test]
fn core_effective_view_disables_optional_owners_without_mutating_persisted_config() {
    let config = RootConfig::parse_persisted(CORE_CONFIG).expect("core config");
    let original = config.persisted_toml().expect("original config rendering");
    let effective = config
        .with_effective_composition()
        .expect("effective config");
    assert!(!effective.task.enabled);
    assert_eq!(effective.task.routing_policy, TaskRoutingPolicy::Manual);
    assert_eq!(effective.task.multi_agent_mode, MultiAgentMode::None);
    assert!(!effective.memory.enabled);
    assert!(!effective.memory.writable);
    assert!(!effective.skills.enabled);
    assert!(!effective.compaction.enabled);
    assert!(!effective.compaction.native_carrier_enabled);
    assert!(!effective.code_intelligence.enabled);
    assert!(!effective.web.enabled);
    assert!(effective.mcp_servers.is_empty());
    assert_eq!(
        config.persisted_toml().expect("original still valid"),
        original
    );
    assert!(effective.persisted_toml().is_err());
}

#[test]
fn invalid_unselected_module_payloads_roundtrip_and_fail_only_when_selected() {
    let cases = [
        (
            OptionalCapability::TaskOrchestration,
            "task",
            "[task]\nenabled = 'invalid'",
        ),
        (
            OptionalCapability::Memory,
            "memory",
            "[memory]\nwritable = ['invalid']",
        ),
        (
            OptionalCapability::Skills,
            "skills",
            "[skills]\nunknown = 4",
        ),
        (
            OptionalCapability::Compaction,
            "compaction",
            "[compaction]\nstrategy = 'invalid'",
        ),
        (
            OptionalCapability::CodeIntelligence,
            "code_intelligence",
            "[code_intelligence]\nserver_startup = 'invalid'",
        ),
        (
            OptionalCapability::Web,
            "web",
            "[web]\nsearch_route = 'invalid'",
        ),
        (
            OptionalCapability::Mcp,
            "mcp_servers",
            "[[mcp_servers]]\nname = 'broken'\ntransport = 42",
        ),
    ];
    for (capability, key, module) in cases {
        let raw = config_with_module(module);
        let mut config = RootConfig::parse_persisted(&raw).expect("unselected payload is deferred");
        config
            .with_effective_composition()
            .expect("unselected module cannot block core");
        config.agent.max_turns = Some(3);
        let rendered = config
            .persisted_toml()
            .expect("unrelated edit preserves deferred module");
        let original: toml::Value = toml::from_str(&raw).expect("source TOML");
        let saved: toml::Value = toml::from_str(&rendered).expect("saved TOML");
        assert_eq!(original.get(key), saved.get(key), "{capability:?}");
        let mut selected = config.clone();
        selected.composition.enhancements.insert(capability);
        assert!(
            selected.with_effective_composition().is_err(),
            "{capability:?}"
        );
        let selected_raw = raw.replace("profile = \"core\"", "profile = \"standard\"");
        assert!(
            RootConfig::parse_persisted(&selected_raw).is_err(),
            "{capability:?}"
        );
    }
}

#[test]
fn invalid_task_model_routes_cannot_block_core_but_selected_routes_are_validated() {
    let raw = config_with_module("[task.planner]\nmodel = 'orphaned-model'");
    let mut config = RootConfig::parse_persisted(&raw).expect("unselected role config");
    config
        .with_effective_composition()
        .expect("core does not validate task roles");
    config
        .composition
        .enhancements
        .insert(OptionalCapability::TaskOrchestration);
    assert!(config.with_effective_composition().is_err());
}

#[test]
fn selecting_a_deferred_module_restores_its_exact_configuration() {
    let raw = config_with_module(
        "[skills]\nenabled = true\nuser_skills = false\ncompatibility_sources = ['custom']",
    );
    let mut config = RootConfig::parse_persisted(&raw).expect("deferred skill config");
    config
        .composition
        .enhancements
        .insert(OptionalCapability::Skills);
    let effective = config
        .with_effective_composition()
        .expect("activate skills");
    assert!(effective.skills.enabled);
    assert!(!effective.skills.user_skills);
    assert_eq!(effective.skills.compatibility_sources, ["custom"]);
    assert!(!effective.memory.enabled);
}

#[test]
fn composition_does_not_enable_a_module_disabled_by_its_own_configuration() {
    let raw = config_with_module("[skills]\nenabled = false");
    for profile in [
        RuntimeCompositionProfile::Core,
        RuntimeCompositionProfile::Standard,
    ] {
        let mut config = RootConfig::parse_persisted(&raw).expect("deferred skill config");
        config.composition.profile = profile;
        config
            .composition
            .enhancements
            .insert(OptionalCapability::Skills);
        let effective = config
            .with_effective_composition()
            .expect("selected disabled module");
        assert!(!effective.skills.enabled);
        assert!(
            !effective
                .selected_capabilities()
                .contains(&OptionalCapability::Skills)
        );
    }
}

#[test]
fn core_keeps_syntax_root_schema_and_permission_validation_strict() {
    for raw in [
        format!("{CORE_CONFIG}\n[task"),
        CORE_CONFIG.replace("config_version = 2", "config_version = 2\nunknown = true"),
        config_with_module("[permission]\nmode = 'invalid-mode'"),
        config_with_module("[composition.unknown]\nenabled = true"),
    ] {
        assert!(RootConfig::parse_persisted(&raw).is_err(), "{raw}");
    }
}

#[test]
fn selection_contract_never_serializes_or_debugs_deferred_secrets() {
    let config = RootConfig::parse_persisted(&config_with_module(
        "[[mcp_servers]]\nname = 'disabled'\nsecret_payload = 'fixture-secret'",
    ))
    .expect("unselected arbitrary MCP configuration");
    let selected = config.composition.selection_only();
    assert_eq!(selected, config.composition);
    assert!(!format!("{config:?}").contains("fixture-secret"));
    assert!(
        !toml::to_string(&config.composition)
            .expect("selection serialization")
            .contains("fixture-secret")
    );
    assert!(
        !serde_json::to_string(&selected)
            .expect("selection JSON")
            .contains("fixture-secret")
    );
    assert!(
        config
            .persisted_toml()
            .expect("root preserves config")
            .contains("fixture-secret")
    );
}

#[test]
fn editing_unparsed_module_placeholders_fails_instead_of_losing_original_values() {
    let mut config = RootConfig::parse_persisted(&config_with_module(
        "[skills]\nunknown = 'original-setting'",
    ))
    .expect("deferred skill configuration");
    config.skills.compatibility_sources = vec!["changed".to_owned()];
    assert!(config.persisted_toml().is_err());
}

#[test]
fn root_json_roundtrip_preserves_unselected_documents() {
    let config = RootConfig::parse_persisted(&config_with_module(
        "[[mcp_servers]]\nname = 'disabled'\nunknown = 42",
    ))
    .expect("deferred MCP config");
    let json = serde_json::to_string(&config).expect("root JSON");
    let restored: RootConfig = serde_json::from_str(&json).expect("restore root JSON");
    assert_eq!(
        config.persisted_toml().expect("original"),
        restored.persisted_toml().expect("restored")
    );
}
