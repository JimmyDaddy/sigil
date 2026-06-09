use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiCommand {
    SubmitPrompt,
    CancelOrQuit,
    ToggleWriteMode,
    ToggleThinking,
    OpenKeyboardHelp,
    OpenConfig,
    CompactNow,
    FocusLatestToolCard,
    SelectNextToolCard,
    SelectPreviousToolCard,
    ToggleSelectedToolCard,
    ClearToolCardFocus,
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
        label: "Write mode",
        help: "Toggle runtime permission write mode.",
        surface: CommandSurface::Global,
    },
    UiCommandSpec {
        command: UiCommand::ToggleThinking,
        keys: &[KeyBinding { label: "Ctrl-T" }],
        slash: None,
        label: "Thinking view",
        help: "Expand or collapse thinking blocks.",
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
        command: UiCommand::FocusLatestToolCard,
        keys: &[KeyBinding { label: "Ctrl-G" }],
        slash: None,
        label: "Latest tool",
        help: "Focus the latest tool card.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SelectNextToolCard,
        keys: &[KeyBinding { label: "Alt-J" }],
        slash: None,
        label: "Next tool",
        help: "Focus the next tool card.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::SelectPreviousToolCard,
        keys: &[KeyBinding { label: "Alt-K" }],
        slash: None,
        label: "Previous tool",
        help: "Focus the previous tool card.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ToggleSelectedToolCard,
        keys: &[KeyBinding { label: "Ctrl-O" }],
        slash: None,
        label: "Toggle tool",
        help: "Open or close the focused tool card.",
        surface: CommandSurface::ToolCard,
    },
    UiCommandSpec {
        command: UiCommand::ClearToolCardFocus,
        keys: &[KeyBinding { label: "Esc" }],
        slash: None,
        label: "Clear tool focus",
        help: "Clear tool card focus when the composer is empty.",
        surface: CommandSurface::ToolCard,
    },
];

pub(crate) fn command_for_key_event(key: KeyEvent) -> Option<UiCommand> {
    match key.code {
        KeyCode::F(1) => Some(UiCommand::OpenKeyboardHelp),
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
        _ => None,
    }
}

pub(crate) fn global_control_hints(is_busy: bool) -> Vec<String> {
    let mut hints = vec![
        control_hint(UiCommand::OpenKeyboardHelp).expect("keyboard help metadata exists"),
        "/ or 、: command palette".to_owned(),
        control_hint(UiCommand::ToggleWriteMode).expect("write mode metadata exists"),
        format!("Ctrl-C: {}", if is_busy { "cancel" } else { "quit" }),
        control_hint(UiCommand::ToggleThinking).expect("thinking metadata exists"),
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn tool_card_commands() -> impl Iterator<Item = UiCommand> {
    COMMAND_SPECS
        .iter()
        .filter(|spec| spec.surface == CommandSurface::ToolCard)
        .map(|spec| spec.command)
}

pub(crate) fn keyboard_help_lines(include_tool_cards: bool) -> Vec<String> {
    let mut lines = vec!["Core shortcuts".to_owned()];
    lines.extend(
        COMMAND_SPECS
            .iter()
            .filter(|spec| spec.surface == CommandSurface::Global)
            .filter_map(command_spec_help_line),
    );
    lines.extend([
        "Shift-Enter: Insert a newline in the composer.".to_owned(),
        "Ctrl-U/D: Scroll transcript by page.".to_owned(),
        "Ctrl-P/N: Navigate prompt history.".to_owned(),
        String::new(),
    ]);
    if include_tool_cards {
        lines.push("Tool cards".to_owned());
        lines.extend(
            COMMAND_SPECS
                .iter()
                .filter(|spec| spec.surface == CommandSurface::ToolCard)
                .filter_map(command_spec_help_line),
        );
        lines.push(String::new());
    }
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

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
