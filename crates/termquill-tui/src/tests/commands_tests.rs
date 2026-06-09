use super::*;

#[test]
fn maps_tool_card_key_events_to_commands() {
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        Some(UiCommand::FocusLatestToolCard)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::ALT)),
        Some(UiCommand::SelectNextToolCard)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT)),
        Some(UiCommand::SelectPreviousToolCard)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        Some(UiCommand::ToggleSelectedToolCard)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
        Some(UiCommand::OpenKeyboardHelp)
    );
    assert!(tool_card_commands().any(|command| command == UiCommand::ClearToolCardFocus));
}

#[test]
fn command_metadata_generates_help_and_control_hints() {
    let global = global_control_hints(false);
    assert!(global.iter().any(|hint| hint == "F1: keyboard help"));
    assert!(global.iter().any(|hint| hint == "Ctrl-C: quit"));

    let help = keyboard_help_lines(true);
    assert!(help.iter().any(|line| line.contains("F1:")));
    assert!(help.iter().any(|line| line.contains("Ctrl-G:")));

    let slash = metadata_slash_help_lines();
    assert!(slash.iter().any(|line| line.starts_with("/config:")));
    assert!(metadata_slash_commands().any(|command| command == "/compact"));
}
