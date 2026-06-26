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
        command_for_key_event(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::ALT)),
        Some(UiCommand::CheckChangedFilesDiagnostics)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::ALT)),
        Some(UiCommand::CycleAgentView)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(
            KeyCode::Char('A'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        )),
        Some(UiCommand::CycleAgentViewPrevious)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        None
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
    assert!(global.iter().any(|hint| hint == "Alt-A: agent"));
    assert!(
        global
            .iter()
            .any(|hint| hint == "Shift-Alt-A: previous agent")
    );
    assert!(global.iter().any(|hint| hint == "Alt-D: check changes"));
    assert!(
        global_control_hints(true)
            .iter()
            .any(|hint| hint == "Esc: interrupt")
    );
    let activity_controls = tool_card_control_hints().collect::<Vec<_>>();
    assert!(
        activity_controls
            .iter()
            .any(|hint| hint == "Ctrl-T: toggle activity")
    );
    assert!(
        activity_controls
            .iter()
            .any(|hint| hint == "Alt-J: next activity")
    );

    let help = keyboard_help_lines(true);
    assert!(help.iter().any(|line| line.contains("F1:")));
    assert!(help.iter().any(|line| {
        line == "Shift-Enter, Alt-Enter, or Ctrl-J: Insert a newline in the composer."
    }));
    assert!(help.iter().any(|line| {
        line == "Paste: Insert pasted text without submitting; large pastes are folded in the composer display."
    }));
    assert!(help.iter().any(|line| {
        line == "@agent or trusted /agent-name: Invoke an enabled trusted agent profile with a prompt."
    }));
    assert!(
        help.iter().any(|line| {
            line == "Ctrl-A/E: Move to the start/end of the current composer line."
        })
    );
    assert!(
        help.iter().any(|line| {
            line == "Alt-B/F or Ctrl-Left/Right: Move the composer cursor by word."
        })
    );
    assert!(
        help.iter().any(|line| {
            line == "Ctrl-K/Y: Kill to composer line end and yank the killed text."
        })
    );
    assert!(
        help.iter()
            .any(|line| line == "Ctrl-Z: Restore the last draft cleared with Esc.")
    );
    assert!(
        help.iter()
            .any(|line| line == "Up/Down or Ctrl-P/N: Navigate prompt history.")
    );
    assert!(
        help.iter()
            .any(|line| line == "PageUp/PageDown or Ctrl-U/D: Scroll transcript by page.")
    );
    assert!(
        help.iter()
            .any(|line| line == "Alt-D: Run code diagnostics for changed source files.")
    );
    assert!(help.iter().any(|line| {
        line == "Alt-A: Switch the visible main chat between parent and child agents; /agent can rename child agents."
    }));
    assert!(help.iter().any(|line| {
        line == "Shift-Alt-A: Switch the visible main chat to the previous parent or child agent."
    }));
    assert!(help.iter().any(|line| line == "Activities"));
    assert!(help.iter().any(|line| line.contains("Ctrl-G:")));
    assert!(
        help.iter()
            .any(|line| { line == "Ctrl-T: Expand or collapse the focused activity." })
    );

    let slash = metadata_slash_help_lines();
    assert!(slash.iter().any(|line| line.starts_with("/config:")));
    assert!(slash.iter().any(|line| line.starts_with("/new:")));
    assert!(
        slash
            .iter()
            .any(|line| line.starts_with("/trust-workspace:"))
    );
    assert!(slash.iter().any(|line| line.starts_with("/plan:")));
    assert!(slash.iter().any(|line| line.starts_with("/task:")));
    assert!(metadata_slash_commands().any(|command| command == "/compact"));
    assert!(metadata_slash_commands().any(|command| command == "/agent"));
    assert!(metadata_slash_commands().any(|command| command == "/doctor"));
    assert!(metadata_slash_commands().any(|command| command == "/new"));
    assert!(metadata_slash_commands().any(|command| command == "/trust-workspace"));
    assert!(metadata_slash_commands().any(|command| command == "/plan"));
    assert!(metadata_slash_commands().any(|command| command == "/task"));
}
