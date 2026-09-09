use std::collections::BTreeSet;

use crate::APPLICATION_COMMANDS;

fn core_catalog_config(optional_payloads: &str) -> sigil_kernel::RootConfig {
    sigil_kernel::RootConfig::parse_persisted(&format!(
        r#"config_version = 2
{optional_payloads}
[agent]
connection = "fixture"
model = "fixture"
[composition]
profile = "core"
[connections.fixture]
label = "Local fixture"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = {{ source = "none" }}
"#
    ))
    .expect("catalog configuration")
}

#[test]
fn core_catalog_projects_selection_before_loading_optional_definitions() {
    let fixture = tempfile::tempdir().expect("fixture");
    let workspace = fixture.path().join("workspace-must-not-be-opened");
    let config = core_catalog_config("task = 'unselected task'\nskills = 'unselected skills'");
    let original = config.persisted_toml().expect("original config");
    assert!(config.task.enabled && config.skills.enabled);
    let catalog = crate::application_extension_catalog_view(&config, &workspace, &[])
        .expect("core catalog without optional definitions");
    assert!(catalog.skills.is_empty());
    assert!(catalog.agents.is_empty());
    assert!(!workspace.exists());
    for token in ["/agent", "/plan", "/intents", "/compact"] {
        let command = catalog
            .commands
            .iter()
            .find(|entry| entry.canonical == token)
            .expect("shared command");
        assert!(!command.available);
        assert!(command.unavailable_reason.is_some());
    }
    for token in ["/new", "/model", "/effort", "/resume", "/config"] {
        assert!(
            catalog
                .commands
                .iter()
                .any(|entry| entry.canonical == token && entry.available)
        );
    }
    assert_eq!(
        config.persisted_toml().expect("original preserved"),
        original
    );
}

#[test]
fn skills_and_agent_catalogs_activate_independently() {
    let _environment = crate::test_env::lock();
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_root = workspace.path().join(".sigil/skills/catalog-fixture");
    std::fs::create_dir_all(&skill_root).expect("skill directory");
    std::fs::write(
        skill_root.join("SKILL.md"),
        "---\nname: catalog-fixture\ndescription: Exact local fixture.\ntrust: trusted\n---\n\nFixture instructions.\n",
    )
    .expect("skill");
    let mut skills = core_catalog_config("task = 'task payload must remain deferred'");
    skills
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::Skills);
    skills.skills.user_skills = false;
    skills.skills.user_agents = false;
    skills.skills.compatibility_auto_discover = false;
    let skill_catalog = crate::application_extension_catalog_view(&skills, workspace.path(), &[])
        .expect("skills-only catalog");
    assert_eq!(skill_catalog.skills.len(), 1);
    assert_eq!(skill_catalog.skills[0].name, "catalog-fixture");
    assert!(skill_catalog.agents.is_empty());

    let mut agents = core_catalog_config("skills = 'skills payload must remain deferred'");
    agents
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::TaskOrchestration);
    agents.task.enabled = true;
    let agent_catalog = crate::application_extension_catalog_view(&agents, workspace.path(), &[])
        .expect("agents-only catalog");
    assert!(agent_catalog.skills.is_empty());
    assert!(
        agent_catalog
            .agents
            .iter()
            .any(|profile| profile.id == "build")
    );
    assert!(
        agent_catalog
            .commands
            .iter()
            .any(|entry| entry.canonical == "/agent" && entry.available)
    );
}

#[test]
fn selected_but_disabled_catalog_modules_remain_unavailable() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut config = core_catalog_config("");
    config.composition = sigil_kernel::RuntimeCompositionConfig::standard();
    config.task.enabled = false;
    config.skills.enabled = false;
    config.compaction.enabled = false;
    let catalog = crate::application_extension_catalog_view(&config, workspace.path(), &[])
        .expect("disabled catalog");
    assert!(catalog.skills.is_empty());
    assert!(catalog.agents.is_empty());
    assert!(catalog.commands.iter().all(|entry| {
        !matches!(
            entry.canonical.as_str(),
            "/agent" | "/plan" | "/intents" | "/compact"
        ) || !entry.available
    }));
}

#[test]
fn selecting_a_deferred_catalog_module_restores_configuration_validation() {
    let workspace = tempfile::tempdir().expect("workspace");
    for capability in [
        sigil_kernel::OptionalCapability::Skills,
        sigil_kernel::OptionalCapability::TaskOrchestration,
    ] {
        let mut config = core_catalog_config("task = 'invalid task'\nskills = 'invalid skills'");
        config.composition.enhancements.insert(capability);
        assert!(
            crate::application_extension_catalog_view(&config, workspace.path(), &[]).is_err(),
            "selected {capability:?} must validate its deferred payload"
        );
    }
}

#[test]
fn shared_command_tokens_are_unique_and_well_formed() {
    let mut tokens = BTreeSet::new();
    for command in APPLICATION_COMMANDS {
        assert!(command.canonical.starts_with('/'));
        assert!(tokens.insert(command.canonical));
        for alias in command.aliases {
            assert!(alias.starts_with('/'));
            assert!(tokens.insert(alias));
        }
    }
}

#[test]
fn agent_command_opens_the_shared_agent_workbench() {
    let command = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/agent")
        .expect("agent command");

    assert_eq!(
        command.client_action,
        Some(crate::ApplicationClientAction::OpenAgentWorkbench)
    );
}

#[test]
fn intent_stack_command_has_one_shared_graphical_route() {
    let command = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/intents")
        .expect("Intent Stack command");

    assert_eq!(
        command.client_action,
        Some(crate::ApplicationClientAction::OpenIntentStack)
    );
    assert!(!command.completes_with_space);
}

#[test]
fn desktop_equivalent_commands_have_explicit_client_routes() {
    let compact = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/compact")
        .expect("compact command");
    let config = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/config")
        .expect("config command");
    let plan = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/plan")
        .expect("plan command");
    let resume = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/resume")
        .expect("resume command");
    let doctor = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/doctor")
        .expect("doctor command");
    let feedback = APPLICATION_COMMANDS
        .iter()
        .find(|command| command.canonical == "/feedback")
        .expect("feedback command");

    assert_eq!(
        compact.client_action,
        Some(crate::ApplicationClientAction::PreviewCompaction)
    );
    assert_eq!(
        config.client_action,
        Some(crate::ApplicationClientAction::OpenSettings)
    );
    assert_eq!(
        plan.client_action,
        Some(crate::ApplicationClientAction::OpenAgentWorkbench)
    );
    assert_eq!(
        resume.client_action,
        Some(crate::ApplicationClientAction::OpenSessionPicker)
    );
    assert_eq!(
        doctor.client_action,
        Some(crate::ApplicationClientAction::OpenSupport)
    );
    assert_eq!(
        feedback.client_action,
        Some(crate::ApplicationClientAction::OpenSupport)
    );
}
