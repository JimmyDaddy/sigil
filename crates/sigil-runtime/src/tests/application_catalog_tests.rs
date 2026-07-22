use std::collections::BTreeSet;

use crate::APPLICATION_COMMANDS;

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
