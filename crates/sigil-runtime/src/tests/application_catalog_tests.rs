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
