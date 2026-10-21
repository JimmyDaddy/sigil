use super::*;

#[test]
fn task_availability_help_override_preserves_the_command_directory() {
    let available = metadata_slash_help_lines(None);
    let unavailable = metadata_slash_help_lines(Some("Task unavailable · see sigil.toml"));
    assert_eq!(available.len(), unavailable.len());
    assert!(unavailable.contains(&"/task: Task unavailable · see sigil.toml".to_owned()));
    for line in available.iter().filter(|line| !line.starts_with("/task:")) {
        assert!(unavailable.contains(line), "unchanged command: {line}");
    }
    assert!(available.contains(&"/task: Start or continue a durable multi-step task.".to_owned()));
}

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
        command_for_key_event(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::ALT)),
        Some(UiCommand::ReadNextToolArtifactPage)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)),
        Some(UiCommand::SearchToolArtifact)
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
        command_for_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL,)),
        Some(UiCommand::OpenCheckpointRestore)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT)),
        Some(UiCommand::OpenChangeReview)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::ALT)),
        Some(UiCommand::ToggleInfoRailDetail)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
        Some(UiCommand::ToggleInfoRailVisibility)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)),
        Some(UiCommand::ToggleInfoRailDetail)
    );
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::SHIFT)),
        Some(UiCommand::ToggleInfoRailDetail)
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
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL,)),
        Some(UiCommand::CopyTranscript)
    );
    for key in [
        KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
    ] {
        assert_eq!(command_for_key_event(key), Some(UiCommand::PauseActiveTask));
    }
    assert_eq!(
        command_for_key_event(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)),
        Some(UiCommand::OpenConfig)
    );
    assert!(tool_card_commands().any(|command| command == UiCommand::ClearToolCardFocus));
}

#[test]
fn command_metadata_generates_help_and_control_hints() {
    let common = common_keyboard_help_lines();
    for prefix in ["F3:", "F4:", "F5:", "Shift-Tab:", "Ctrl-C:"] {
        assert!(
            common.iter().any(|line| line.starts_with(prefix)),
            "missing common control: {prefix}"
        );
    }
    let help = keyboard_help_lines(true);
    for section in [
        "Session",
        "Review",
        "Approval",
        "Agents",
        "Config",
        "Navigation",
    ] {
        assert!(help.iter().any(|line| line == section));
    }
    assert!(help.iter().any(|line| line.contains("F1:")));
    assert!(help.iter().any(|line| line.starts_with("/new:")));
    assert!(help.iter().any(|line| line.starts_with("/compact:")));
    assert!(
        help.iter()
            .any(|line| line == "Ctrl-J: Insert a newline in the composer.")
    );
    assert!(help.iter().any(|line| {
        line == "Shift-Enter or Alt-Enter: Insert a newline when terminal keyboard enhancement is active and reports modifiers."
    }));
    assert!(help.iter().any(|line| {
        line == "Paste: Insert pasted text without submitting; large pastes are folded in the composer display."
    }));
    assert!(
        help.iter()
            .any(|line| line == "Enter while busy: Add a visible follow-up for the next safe turn.")
    );
    assert!(help.iter().any(|line| {
        line == "@agent or trusted /agent-name: Invoke an enabled trusted agent profile with a prompt."
    }));
    assert!(
        help.iter()
            .any(|line| line == "Y/N: Allow or deny the pending tool call.")
    );
    assert!(
        help.iter()
            .any(|line| line == "V: Inspect the pending diff or preview when available.")
    );
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
    assert!(help.iter().any(|line| {
        line == "Drag: Auto-copy. Ctrl-C: Retry/cancel/quit. F5: Pause task. Ctrl-L: Latest reply."
    }));
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
    assert!(
        help.iter()
            .any(|line| line == "F2: Show or hide the right info rail for the current run.")
    );
    assert!(help.iter().any(|line| {
        line == "F4 / Alt-I / Shift-F2: Open or close scrollable workspace and session details."
    }));
    assert!(help.iter().any(|line| {
        line == "Alt-A: Switch the visible main chat between parent and child agents; /agent can rename child agents."
    }));
    assert!(help.iter().any(|line| {
        line == "Shift-Alt-A: Switch the visible main chat to the previous parent or child agent."
    }));
    assert!(help.iter().any(|line| line.contains("Ctrl-G:")));
    assert!(
        help.iter()
            .any(|line| line == "Ctrl-T / Ctrl-O: Expand or collapse the focused activity.")
    );
    assert!(help.iter().any(|line| {
        line == "Alt-N: Read the next bounded page from the focused tool output artifact."
    }));
    assert!(help.iter().any(|line| {
        line == "Alt-F: Search a bounded literal in the focused tool output artifact."
    }));

    let slash = metadata_slash_help_lines(None);
    assert!(slash.iter().any(|line| line.starts_with("/config:")));
    assert!(help.iter().any(|line| line == "F3: Open TUI config panel."));
    assert!(
        common_keyboard_help_lines()
            .iter()
            .any(|line| line.starts_with("F3: Settings; keep the draft."))
    );
    assert!(slash.iter().any(|line| line.starts_with("/new:")));
    assert!(slash.iter().any(|line| line.starts_with("/plan:")));
    assert!(slash.iter().any(|line| line.starts_with("/task:")));
    assert!(metadata_slash_commands().any(|command| command == "/compact"));
    assert!(metadata_slash_commands().any(|command| command == "/agent"));
    assert!(metadata_slash_commands().any(|command| command == "/doctor"));
    assert!(metadata_slash_commands().any(|command| command == "/feedback"));
    assert!(metadata_slash_commands().any(|command| command == "/new"));
    assert!(metadata_slash_commands().any(|command| command == "/plan"));
    assert!(metadata_slash_commands().any(|command| command == "/task"));
}
