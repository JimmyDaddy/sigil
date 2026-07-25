pub(crate) use sigil_runtime::{
    APPLICATION_COMMANDS as SLASH_COMMANDS, ApplicationCommandSpec as SlashCommandSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSlashCommand {
    pub(crate) canonical: String,
    pub(crate) arg: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SlashSelectorEntry {
    pub(crate) fill: String,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) resolved: ResolvedSlashCommand,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SlashArgumentOption {
    pub(crate) label: &'static str,
    pub(crate) value: &'static str,
    pub(crate) description: &'static str,
    pub(crate) keywords: &'static [&'static str],
}

pub(crate) const EFFORT_SELECTOR_OPTIONS: &[SlashArgumentOption] = &[
    SlashArgumentOption {
        label: "low",
        value: "low",
        description: "lighter reasoning",
        keywords: &["low"],
    },
    SlashArgumentOption {
        label: "medium",
        value: "medium",
        description: "default reasoning",
        keywords: &["medium", "med"],
    },
    SlashArgumentOption {
        label: "high",
        value: "high",
        description: "deeper reasoning",
        keywords: &["high"],
    },
    SlashArgumentOption {
        label: "max",
        value: "max",
        description: "strongest reasoning",
        keywords: &["max"],
    },
];
