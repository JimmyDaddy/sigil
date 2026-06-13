#[derive(Debug, Clone, Copy)]
pub(crate) struct SlashCommandSpec {
    pub(crate) canonical: &'static str,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) description: &'static str,
    pub(crate) completes_with_space: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSlashCommand {
    pub(crate) canonical: &'static str,
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

pub(crate) const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        canonical: "/compact",
        aliases: &[],
        description: "compact context",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/config",
        aliases: &[],
        description: "edit config",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/doctor",
        aliases: &[],
        description: "run local diagnostics",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/effort",
        aliases: &["/e"],
        description: "set effort <low|medium|high|max>",
        completes_with_space: true,
    },
    SlashCommandSpec {
        canonical: "/model",
        aliases: &["/m"],
        description: "switch model <flash|pro|id>",
        completes_with_space: true,
    },
    SlashCommandSpec {
        canonical: "/quit",
        aliases: &["/q", "/exit"],
        description: "quit TUI",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/resume",
        aliases: &[],
        description: "choose a saved session",
        completes_with_space: true,
    },
];

pub(crate) const KNOWN_MODEL_IDS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];

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
        description: "balanced default",
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

pub(crate) const MODEL_SELECTOR_OPTIONS: &[SlashArgumentOption] = &[
    SlashArgumentOption {
        label: "flash",
        value: "deepseek-v4-flash",
        description: "fast default model",
        keywords: &["flash", "v4-flash", "deepseek-v4-flash"],
    },
    SlashArgumentOption {
        label: "pro",
        value: "deepseek-v4-pro",
        description: "stronger reasoning model",
        keywords: &["pro", "v4-pro", "deepseek-v4-pro"],
    },
];
