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
