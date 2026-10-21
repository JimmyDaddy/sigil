use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiCommand {
    SubmitPrompt,
    CopyTranscript,
    EnterPlanMode,
    SubmitTask,
    PauseActiveTask,
    CancelOrQuit,
    ToggleWriteMode,
    ToggleThinking,
    ToggleInfoRailVisibility,
    ToggleInfoRailDetail,
    OpenKeyboardHelp,
    OpenConfig,
    OpenDoctor,
    OpenFeedback,
    CheckForUpdate,
    StartNewSession,
    CompactContext,
    CycleAgentView,
    CycleAgentViewPrevious,
    CheckChangedFilesDiagnostics,
    FocusVerificationCard,
    OpenCheckpointRestore,
    OpenConversationFork,
    OpenChangeReview,
    OpenIntentStack,
    FocusLatestToolCard,
    SelectNextToolCard,
    SelectPreviousToolCard,
    ToggleSelectedToolCard,
    ReadNextToolArtifactPage,
    SearchToolArtifact,
    ClearToolCardFocus,
    CancelFocusedTerminalTask,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum KeyboardHelpPage {
    #[default]
    Common,
    All,
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
    pub(crate) help: &'static str,
    pub(crate) surface: CommandSurface,
}

pub(crate) const COMMAND_SPECS: &[UiCommandSpec] = &[
    UiCommandSpec {
        command: UiCommand::OpenKeyboardHelp,
        keys: &[KeyBinding { label: "F1" }],
        slash: None,
        help: "Show available TUI shortcuts.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::SubmitPrompt,
        keys: &[KeyBinding { label: "Enter" }],
        slash: None,
        help: "Submit composer input or execute the selected slash command.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CopyTranscript,
        keys: &[KeyBinding { label: "Ctrl-L" }],
        slash: None,
        help: "Copy selected transcript text, or the latest assistant response when nothing is selected.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CancelOrQuit,
        keys: &[KeyBinding { label: "Ctrl-C" }],
        slash: Some("/quit"),
        help: "Cancel an active run, or quit when idle.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleWriteMode,
        keys: &[KeyBinding { label: "Shift-Tab" }],
        slash: None,
        help: "Cycle and persist the default permission mode.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleThinking,
        keys: &[KeyBinding { label: "Ctrl-T" }],
        slash: None,
        help: "Expand or collapse the current task list, focused activity, or thinking details.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleInfoRailVisibility,
        keys: &[KeyBinding { label: "F2" }],
        slash: None,
        help: "Show or hide the right info rail for the current run.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleInfoRailDetail,
        keys: &[
            KeyBinding { label: "F4" },
            KeyBinding { label: "Alt-I" },
            KeyBinding { label: "Shift-F2" },
        ],
        slash: None,
        help: "Open or close scrollable workspace and session details.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::OpenChangeReview,
        keys: &[KeyBinding { label: "Alt-R" }],
        slash: None,
        help: "Select recorded diff lines and send a batch of review comments through the conversation.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::OpenConversationFork,
        keys: &[KeyBinding { label: "Alt-B" }],
        slash: None,
        help: "Choose a completed turn, open a branch, and edit a draft without sending it. Alt-B keeps word navigation while editing.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::OpenCheckpointRestore,
        keys: &[KeyBinding { label: "Ctrl-R" }],
        slash: None,
        help: "Open the latest controlled checkpoint in an exact restore preview dialog.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::OpenIntentStack,
        keys: &[KeyBinding { label: "Alt-S" }],
        slash: Some("/intents"),
        help: "Review durable intent ownership, verification, retention, conflicts, and exact Drop previews.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::OpenConfig,
        keys: &[KeyBinding { label: "F3" }],
        slash: Some("/config"),
        help: "Open TUI config panel.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::CompactContext,
        keys: &[],
        slash: Some("/compact"),
        help: "Generate, validate, and activate one recoverable semantic checkpoint.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::OpenDoctor,
        keys: &[],
        slash: Some("/doctor"),
        help: "Run local setup diagnostics.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::OpenFeedback,
        keys: &[],
        slash: Some("/feedback"),
        help: "Review and privately export a redacted support report.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::CheckForUpdate,
        keys: &[],
        slash: Some("/update"),
        help: "Check for a new Sigil release or install an admitted standalone update.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::StartNewSession,
        keys: &[],
        slash: Some("/new"),
        help: "Start a fresh session.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::EnterPlanMode,
        keys: &[],
        slash: Some("/plan"),
        help: "Run a read-only planning prompt; accept the plan to create and run a durable task.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::SubmitTask,
        keys: &[],
        slash: Some("/task"),
        help: "Start or continue a durable multi-step task.",
        surface: CommandSurface::Slash,
    },
    UiCommandSpec {
        command: UiCommand::PauseActiveTask,
        keys: &[KeyBinding { label: "F5" }, KeyBinding { label: "Alt-P" }],
        slash: None,
        help: "Stop the active task at a safe cancellation boundary and keep it resumable.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CycleAgentView,
        keys: &[KeyBinding { label: "Alt-A" }],
        slash: Some("/agent"),
        help: "Switch the visible main chat between parent and child agents; /agent can rename child agents.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CycleAgentViewPrevious,
        keys: &[KeyBinding {
            label: "Shift-Alt-A",
        }],
        slash: None,
        help: "Switch the visible main chat to the previous parent or child agent.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::CheckChangedFilesDiagnostics,
        keys: &[KeyBinding { label: "Alt-D" }],
        slash: None,
        help: "Run code diagnostics for changed source files.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::FocusVerificationCard,
        keys: &[KeyBinding { label: "Alt-V" }],
        slash: None,
        help: "Focus the current task verification card; Enter runs its action and I inspects evidence.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::FocusLatestToolCard,
        keys: &[KeyBinding { label: "Ctrl-G" }],
        slash: None,
        help: "Focus the latest activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SelectNextToolCard,
        keys: &[KeyBinding { label: "Alt-J" }],
        slash: None,
        help: "Focus the next activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SelectPreviousToolCard,
        keys: &[KeyBinding { label: "Alt-K" }],
        slash: None,
        help: "Focus the previous activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ToggleSelectedToolCard,
        keys: &[
            KeyBinding { label: "Ctrl-T" },
            KeyBinding { label: "Ctrl-O" },
        ],
        slash: None,
        help: "Expand or collapse the focused activity.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ReadNextToolArtifactPage,
        keys: &[KeyBinding { label: "Alt-N" }],
        slash: None,
        help: "Read the next bounded page from the focused tool output artifact.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SearchToolArtifact,
        keys: &[KeyBinding { label: "Alt-F" }],
        slash: None,
        help: "Search a bounded literal in the focused tool output artifact.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ClearToolCardFocus,
        keys: &[KeyBinding { label: "Esc" }],
        slash: None,
        help: "Clear activity focus when the composer is empty.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::CancelFocusedTerminalTask,
        keys: &[KeyBinding { label: "Alt-X" }],
        slash: None,
        help: "Cancel the focused running terminal task after confirmation.",
        surface: CommandSurface::ToolCard,
    },
];

pub(crate) fn command_for_key_event(key: KeyEvent) -> Option<UiCommand> {
    match key.code {
        KeyCode::F(1) => Some(UiCommand::OpenKeyboardHelp),
        KeyCode::F(2) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(UiCommand::ToggleInfoRailDetail)
        }
        KeyCode::F(2) => Some(UiCommand::ToggleInfoRailVisibility),
        KeyCode::F(3) => Some(UiCommand::OpenConfig),
        KeyCode::F(4) => Some(UiCommand::ToggleInfoRailDetail),
        KeyCode::F(5) => Some(UiCommand::PauseActiveTask),
        KeyCode::Char('l') | KeyCode::Char('L') if key.modifiers == KeyModifiers::CONTROL => {
            Some(UiCommand::CopyTranscript)
        }
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
        KeyCode::Char('n') | KeyCode::Char('N') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::ReadNextToolArtifactPage)
        }
        KeyCode::Char('f') | KeyCode::Char('F') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::SearchToolArtifact)
        }
        KeyCode::Char('d') | KeyCode::Char('D') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::CheckChangedFilesDiagnostics)
        }
        KeyCode::Char('v') | KeyCode::Char('V') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::FocusVerificationCard)
        }
        KeyCode::Char('p') | KeyCode::Char('P') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::PauseActiveTask)
        }
        KeyCode::Char('r') | KeyCode::Char('R') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::OpenChangeReview)
        }
        KeyCode::Char('r') | KeyCode::Char('R') if key.modifiers == KeyModifiers::CONTROL => {
            Some(UiCommand::OpenCheckpointRestore)
        }
        KeyCode::Char('b') | KeyCode::Char('B') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::OpenConversationFork)
        }
        KeyCode::Char('s') | KeyCode::Char('S') if key.modifiers == KeyModifiers::ALT => {
            Some(UiCommand::OpenIntentStack)
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

#[cfg(test)]
pub(crate) fn tool_card_commands() -> impl Iterator<Item = UiCommand> {
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.surface == CommandSurface::ToolCard)
        .map(|spec| spec.command)
}

pub(crate) fn common_keyboard_help_lines() -> Vec<String> {
    vec![
        "Input".to_owned(),
        concise_help_line(
            UiCommand::SubmitPrompt,
            "Send; while busy, queue a follow-up.",
        ),
        "Ctrl-J: New line.".to_owned(),
        format!(
            "{} · / or 、 commands",
            concise_help_line(UiCommand::OpenConfig, "Settings; keep the draft.")
        ),
        "Up/Down: Prompt history.".to_owned(),
        String::new(),
        "Read".to_owned(),
        concise_help_line(UiCommand::CopyTranscript, "Copy selection or latest reply."),
        "PageUp/PageDown: Scroll conversation.".to_owned(),
        concise_help_line(
            UiCommand::ToggleThinking,
            "Show or hide activity and thinking details.",
        ),
        concise_help_line(
            UiCommand::ToggleInfoRailDetail,
            "Workspace and session details.",
        ),
        String::new(),
        "Control".to_owned(),
        concise_help_line(UiCommand::ToggleWriteMode, "Change permission mode."),
        concise_help_line(
            UiCommand::PauseActiveTask,
            "Pause the running task; /task continue resumes.",
        ),
        concise_help_line(
            UiCommand::CancelOrQuit,
            "Cancel work / quit when idle; a selection is copied first.",
        ),
    ]
}

fn concise_help_line(command: UiCommand, summary: &str) -> String {
    let spec = COMMAND_SPECS
        .iter()
        .find(|spec| spec.command == command)
        .expect("common help has command metadata");
    let trigger = spec
        .keys
        .first()
        .map(|key| key.label)
        .or(spec.slash)
        .expect("common help has a trigger");
    format!("{trigger}: {summary}")
}

pub(crate) fn keyboard_help_lines(include_tool_cards: bool) -> Vec<String> {
    let mut lines = vec!["Session".to_owned()];
    lines.extend(command_help_lines([
        UiCommand::OpenKeyboardHelp,
        UiCommand::SubmitPrompt,
        UiCommand::PauseActiveTask,
    ]));
    lines.push(
        "Drag: Auto-copy. Ctrl-C: Retry/cancel/quit. F5: Pause task. Ctrl-L: Latest reply."
            .to_owned(),
    );
    lines.extend(command_help_lines([
        UiCommand::StartNewSession,
        UiCommand::CompactContext,
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
        UiCommand::OpenCheckpointRestore,
        UiCommand::OpenIntentStack,
    ]));
    lines.push(
        "Review dialogs require an exact preview before file changes; shell and remote effects are never undone."
            .to_owned(),
    );
    if include_tool_cards {
        lines.extend(command_help_lines([
            UiCommand::FocusLatestToolCard,
            UiCommand::SelectNextToolCard,
            UiCommand::SelectPreviousToolCard,
            UiCommand::ToggleSelectedToolCard,
            UiCommand::ReadNextToolArtifactPage,
            UiCommand::SearchToolArtifact,
            UiCommand::ClearToolCardFocus,
            UiCommand::CancelFocusedTerminalTask,
        ]));
    }
    lines.extend([
        String::new(),
        "Approval".to_owned(),
        "Y/N: Allow or deny the pending tool call.".to_owned(),
        "Tab/Enter: Cycle and select Allow session or Allow family when offered.".to_owned(),
        "F: Allow this command family and persist a matching permission.commands rule.".to_owned(),
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
    lines.extend(command_help_lines([UiCommand::ToggleWriteMode]));
    lines.extend(command_help_lines([
        UiCommand::ToggleInfoRailVisibility,
        UiCommand::ToggleInfoRailDetail,
    ]));
    lines.extend(command_help_lines([
        UiCommand::OpenConfig,
        UiCommand::OpenDoctor,
    ]));
    lines.extend([
        String::new(),
        "Navigation".to_owned(),
        "Ctrl-A/E: Move to the start/end of the current composer line.".to_owned(),
        "Ctrl-B/F or Left/Right: Move the composer cursor by character.".to_owned(),
        "Alt-B/F or Ctrl-Left/Right: Move the composer cursor by word.".to_owned(),
        "Shift-Left/Right/Up/Down or Shift-Home/End: Select composer text; typing or pasting replaces it.".to_owned(),
        "Backspace/Delete, Ctrl-H, Ctrl-W, Ctrl/Alt-Backspace, or Ctrl/Alt-Delete: Delete composer text.".to_owned(),
        "Ctrl-K/Y: Kill to composer line end and yank the killed text.".to_owned(),
        "Ctrl-Z: Restore the last draft cleared with Esc.".to_owned(),
        "Up/Down or Ctrl-P/N: Navigate prompt history.".to_owned(),
        "PageUp/PageDown or Ctrl-U/D: Scroll transcript by page.".to_owned(),
    ]);
    lines.push(String::new());
    lines
}

pub(crate) fn metadata_slash_help_lines(task_unavailable_hint: Option<&str>) -> Vec<String> {
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.surface == CommandSurface::Slash)
        .filter_map(|spec| {
            spec.slash.map(|slash| {
                let help = if spec.command == UiCommand::SubmitTask {
                    task_unavailable_hint.unwrap_or(spec.help)
                } else {
                    spec.help
                };
                format!("{slash}: {help}")
            })
        })
        .collect()
}

pub(crate) fn metadata_slash_commands() -> impl Iterator<Item = &'static str> {
    COMMAND_SPECS.iter().filter_map(|spec| spec.slash)
}

fn command_spec_help_line(spec: &UiCommandSpec) -> Option<String> {
    let trigger = if spec.keys.is_empty() {
        spec.slash?.to_owned()
    } else {
        spec.keys
            .iter()
            .map(|key| key.label)
            .collect::<Vec<_>>()
            .join(" / ")
    };
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
