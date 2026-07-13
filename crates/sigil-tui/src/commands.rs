use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiCommand {
    SubmitPrompt,
    EnterPlanMode,
    SubmitTask,
    CancelOrQuit,
    ToggleWriteMode,
    ToggleThinking,
    ToggleInfoRailDetail,
    OpenKeyboardHelp,
    OpenConfig,
    OpenDoctor,
    StartNewSession,
    CompactNow,
    CycleAgentView,
    CycleAgentViewPrevious,
    CheckChangedFilesDiagnostics,
    FocusVerificationCard,
    FocusCheckpointReview,
    FocusLatestToolCard,
    SelectNextToolCard,
    SelectPreviousToolCard,
    ToggleSelectedToolCard,
    ClearToolCardFocus,
    CancelFocusedTerminalTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandSurface {
    Global,
    Slash,
    ToolCard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyBinding {
    pub(crate) label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiCommandSpec {
    pub(crate) command: UiCommand,
    pub(crate) keys: &'static [KeyBinding],
    pub(crate) slash: Option<&'static str>,
    pub(crate) label: &'static str,
    pub(crate) help: &'static str,
    pub(crate) surface: CommandSurface,
}

pub(crate) const COMMAND_SPECS: &[UiCommandSpec] = &[
    UiCommandSpec {
        command: UiCommand::OpenKeyboardHelp,
        keys: &[KeyBinding { label: "F1" }],
        slash: None,
        label: "Keyboard help",
        help: "Show available TUI shortcuts.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::SubmitPrompt,
        keys: &[KeyBinding { label: "Enter" }],
        slash: None,
        label: "Submit prompt",
        help: "Submit composer input or execute the selected slash command.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CancelOrQuit,
        keys: &[KeyBinding { label: "Ctrl-C" }],
        slash: Some("/quit"),
        label: "Cancel or quit",
        help: "Cancel an active run, or quit when idle.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleWriteMode,
        keys: &[KeyBinding { label: "Shift-Tab" }],
        slash: None,
        label: "Permission mode",
        help: "Cycle and persist the default permission mode.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleThinking,
        keys: &[KeyBinding { label: "Ctrl-T" }],
        slash: None,
        label: "Thinking view",
        help: "Expand or collapse thinking blocks when no activity is focused.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleInfoRailDetail,
        keys: &[KeyBinding { label: "F2" }],
        slash: None,
        label: "Info rail",
        help: "Toggle the right rail between compact and detailed information.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::FocusCheckpointReview,
        keys: &[KeyBinding { label: "Alt-R" }],
        slash: None,
        label: "Checkpoint review",
        help: "Focus the latest controlled checkpoint for restore preview or conversation fork.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::OpenConfig,
        keys: &[],
        slash: Some("/config"),
        label: "Config",
        help: "Open TUI config panel.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::CompactNow,
        keys: &[],
        slash: Some("/compact"),
        label: "Compact",
        help: "Request context compaction when idle.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::OpenDoctor,
        keys: &[],
        slash: Some("/doctor"),
        label: "Doctor",
        help: "Run local setup diagnostics.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::StartNewSession,
        keys: &[],
        slash: Some("/new"),
        label: "New session",
        help: "Start a fresh session.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::EnterPlanMode,
        keys: &[],
        slash: Some("/plan"),
        label: "Plan",
        help: "Run a read-only planning prompt; accept the plan to create and run a durable task.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::SubmitTask,
        keys: &[],
        slash: Some("/task"),
        label: "Task",
        help: "Start or continue a durable multi-step task.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::CycleAgentView,
        keys: &[KeyBinding { label: "Alt-A" }],
        slash: Some("/agent"),
        label: "Agent",
        help: "Switch the visible main chat between parent and child agents; /agent can rename child agents.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CycleAgentViewPrevious,
        keys: &[KeyBinding {
            label: "Shift-Alt-A",
        }],
        slash: None,
        label: "Previous agent",
        help: "Switch the visible main chat to the previous parent or child agent.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CheckChangedFilesDiagnostics,
        keys: &[KeyBinding { label: "Alt-D" }],
        slash: None,
        label: "Check changes",
        help: "Run code diagnostics for changed source files.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::FocusVerificationCard,
        keys: &[KeyBinding { label: "Alt-V" }],
        slash: None,
        label: "Verification",
        help: "Focus the current task verification card; Enter runs its action and I inspects evidence.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::FocusLatestToolCard,
        keys: &[KeyBinding { label: "Ctrl-G" }],
        slash: None,
        label: "Latest activity",
        help: "Focus the latest activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SelectNextToolCard,
        keys: &[KeyBinding { label: "Alt-J" }],
        slash: None,
        label: "Next activity",
        help: "Focus the next activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SelectPreviousToolCard,
        keys: &[KeyBinding { label: "Alt-K" }],
        slash: None,
        label: "Previous activity",
        help: "Focus the previous activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ToggleSelectedToolCard,
        keys: &[KeyBinding { label: "Ctrl-T" }],
        slash: None,
        label: "Toggle activity",
        help: "Expand or collapse the focused activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ClearToolCardFocus,
        keys: &[KeyBinding { label: "Esc" }],
        slash: None,
        label: "Clear activity focus",
        help: "Clear activity focus when the composer is empty.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::CancelFocusedTerminalTask,
        keys: &[KeyBinding { label: "Alt-X" }],
        slash: None,
        label: "Cancel terminal task",
        help: "Cancel the focused running terminal task after confirmation.",
        surface: CommandSurface::ToolCard,
    },
];

pub(crate) fn command_for_key_event(key: KeyEvent) -> Option<UiCommand> {
    match key.code {
        KeyCode::F(1) => Some(UiCommand::OpenKeyboardHelp),
        KeyCode::F(2) => Some(UiCommand::ToggleInfoRailDetail),
        KeyCode::Char('g') | KeyCode::Char('G') if key.modifiers == KeyModifiers::CONTROL => {
            Some(UiCommand::FocusLatestToolCard)
        }
        KeyCode::Char('o') | KeyCode::Char('O') if key.modifiers == KeyModifiers::CONTROL => {
            Some(UiCommand::ToggleSelectedToolCard)
        }
        KeyCode::Char('j') | KeyCode::Char('J') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::SelectNextToolCard)
        }
        KeyCode::Char('k') | KeyCode::Char('K') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::SelectPreviousToolCard)
        }
        KeyCode::Char('d') | KeyCode::Char('D') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::CheckChangedFilesDiagnostics)
        }
        KeyCode::Char('v') | KeyCode::Char('V') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::FocusVerificationCard)
        }
        KeyCode::Char('r') | KeyCode::Char('R') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::FocusCheckpointReview)
        }
        KeyCode::Char('i') | KeyCode::Char('I') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::ToggleInfoRailDetail)
        }
        KeyCode::Char('a') | KeyCode::Char('A') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::CycleAgentView)
        }
        KeyCode::Char('a') | KeyCode::Char('A')
            if key.modifiers == KeyModifiers::ALT | KeyModifiers::SHIFT =>
        {
            Some(UiCommand::CycleAgentViewPrevious)
        }
        KeyCode::Char('x') | KeyCode::Char('X') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::CancelFocusedTerminalTask)
        }
        _ => None,
    }
}

pub(crate) fn global_control_hints(is_busy: bool) -> Vec<String> {
    let mut hints = vec![
        control_hint(UiCommand::OpenKeyboardHelp).expect("keyboard help metadata exists"),
        "/ or 、: command palette".to_owned(),
        control_hint(UiCommand::ToggleWriteMode).expect("write mode metadata exists"),
        control_hint(UiCommand::ToggleInfoRailDetail).expect("info rail metadata exists"),
        if is_busy {
            "Esc: interrupt".to_owned()
        } else {
            "Ctrl-C: quit".to_owned()
        },
        control_hint(UiCommand::ToggleThinking).expect("thinking metadata exists"),
        control_hint(UiCommand::CycleAgentView).expect("agent metadata exists"),
        control_hint(UiCommand::CycleAgentViewPrevious).expect("previous agent metadata exists"),
        control_hint(UiCommand::CheckChangedFilesDiagnostics)
            .expect("check changes metadata exists"),
        control_hint(UiCommand::FocusVerificationCard).expect("verification metadata exists"),
        control_hint(UiCommand::FocusCheckpointReview).expect("checkpoint metadata exists"),
    ];
    hints.retain(|hint| !hint.is_empty());
    hints
}

pub(crate) fn tool_card_control_hints() -> impl Iterator<Item = String> {
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.surface == CommandSurface::ToolCard)
        .filter_map(command_spec_control_hint)
}

#[cfg(test)]
pub(crate) fn tool_card_commands() -> impl Iterator<Item = UiCommand> {
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.surface == CommandSurface::ToolCard)
        .map(|spec| spec.command)
}

pub(crate) fn keyboard_help_lines(include_tool_cards: bool) -> Vec<String> {
    let mut lines = vec!["Session".to_owned()];
    lines.extend(command_help_lines([
        UiCommand::OpenKeyboardHelp,
        UiCommand::SubmitPrompt,
        UiCommand::CancelOrQuit,
        UiCommand::StartNewSession,
        UiCommand::CompactNow,
    ]));
    lines.extend([
        "Ctrl-J: Insert a newline in the composer.".to_owned(),
        "Shift-Enter or Alt-Enter: Insert a newline when terminal keyboard enhancement is active and reports modifiers.".to_owned(),
        "Paste: Insert pasted text without submitting; large pastes are folded in the composer display.".to_owned(),
        "Enter while busy: Add a visible follow-up for the next safe turn.".to_owned(),
        String::new(),
        "Review".to_owned(),
    ]);
    lines.extend(command_help_lines([
        UiCommand::CheckChangedFilesDiagnostics,
        UiCommand::FocusVerificationCard,
        UiCommand::FocusCheckpointReview,
    ]));
    lines.push(
        "Checkpoint focus: Enter previews/confirms controlled file restore; F forks conversation; I inspects evidence. Shell and remote effects are never undone."
            .to_owned(),
    );
    if include_tool_cards {
        lines.extend(command_help_lines([
            UiCommand::FocusLatestToolCard,
            UiCommand::SelectNextToolCard,
            UiCommand::SelectPreviousToolCard,
            UiCommand::ToggleSelectedToolCard,
            UiCommand::ClearToolCardFocus,
            UiCommand::CancelFocusedTerminalTask,
        ]));
    }
    lines.extend([
        String::new(),
        "Approval".to_owned(),
        "Y/N: Allow or deny the pending tool call.".to_owned(),
        "V: Inspect the pending diff or preview when available.".to_owned(),
        "Esc: Dismiss plan approval or return to the active run when allowed.".to_owned(),
        String::new(),
        "Agents".to_owned(),
    ]);
    lines.extend(command_help_lines([
        UiCommand::CycleAgentView,
        UiCommand::CycleAgentViewPrevious,
    ]));
    lines.extend([
        "@agent or trusted /agent-name: Invoke an enabled trusted agent profile with a prompt."
            .to_owned(),
        "Ctrl-B while waiting for a foreground agent: Move that agent to the background."
            .to_owned(),
        "/agent message <agent|current> <prompt>: Send a targeted child-agent mailbox message."
            .to_owned(),
        String::new(),
        "Config".to_owned(),
    ]);
    lines.extend(command_help_lines([
        UiCommand::ToggleWriteMode,
        UiCommand::ToggleInfoRailDetail,
        UiCommand::OpenConfig,
        UiCommand::OpenDoctor,
    ]));
    lines.extend([
        String::new(),
        "Navigation".to_owned(),
        "Ctrl-A/E: Move to the start/end of the current composer line.".to_owned(),
        "Ctrl-B/F or Left/Right: Move the composer cursor by character.".to_owned(),
        "Alt-B/F or Ctrl-Left/Right: Move the composer cursor by word.".to_owned(),
        "Backspace/Delete, Ctrl-H, Ctrl-W, Ctrl/Alt-Backspace, or Ctrl/Alt-Delete: Delete composer text.".to_owned(),
        "Ctrl-K/Y: Kill to composer line end and yank the killed text.".to_owned(),
        "Ctrl-Z: Restore the last draft cleared with Esc.".to_owned(),
        "Up/Down or Ctrl-P/N: Navigate prompt history.".to_owned(),
        "PageUp/PageDown or Ctrl-U/D: Scroll transcript by page.".to_owned(),
    ]);
    lines.push(String::new());
    lines
}

pub(crate) fn metadata_slash_help_lines() -> Vec<String> {
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.surface == CommandSurface::Slash)
        .filter_map(command_spec_help_line)
        .collect()
}

pub(crate) fn metadata_slash_commands() -> impl Iterator<Item = &'static str> {
    COMMAND_SPECS.iter().filter_map(|spec| spec.slash)
}

fn control_hint(command: UiCommand) -> Option<String> {
    COMMAND_SPECS
        .iter()
        .find(|spec| spec.command == command)
        .and_then(command_spec_control_hint)
}

fn command_spec_control_hint(spec: &UiCommandSpec) -> Option<String> {
    let key = spec.keys.first()?.label;
    Some(format!("{key}: {}", spec.label.to_ascii_lowercase()))
}

fn command_spec_help_line(spec: &UiCommandSpec) -> Option<String> {
    let trigger = spec.keys.first().map(|key| key.label).or(spec.slash)?;
    Some(format!("{trigger}: {}", spec.help))
}

fn command_help_lines(commands: impl IntoIterator<Item = UiCommand>) -> Vec<String> {
    commands
        .into_iter()
        .filter_map(|command| {
            COMMAND_SPECS
                .iter()
                .find(|spec| spec.command == command)
                .and_then(command_spec_help_line)
        })
        .collect()
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/commands_tests.rs"]
mod tests;
