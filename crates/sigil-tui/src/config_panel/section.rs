#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSection {
    Provider,
    Storage,
    Permissions,
    Web,
    Memory,
    Compaction,
    CodeIntelligence,
    Terminal,
    Appearance,
    Agents,
    Skills,
    Plugins,
    Mcp,
}

impl ConfigSection {
    pub(crate) const DEFAULT_FLOW: [Self; 7] = [
        Self::Provider,
        Self::Permissions,
        Self::Web,
        Self::Memory,
        Self::Compaction,
        Self::Mcp,
        Self::Appearance,
    ];

    pub(crate) const FLOW: [Self; 13] = [
        Self::Provider,
        Self::Storage,
        Self::Permissions,
        Self::Web,
        Self::Memory,
        Self::Compaction,
        Self::CodeIntelligence,
        Self::Terminal,
        Self::Appearance,
        Self::Agents,
        Self::Skills,
        Self::Plugins,
        Self::Mcp,
    ];

    pub(crate) fn visible_flow(show_advanced: bool) -> &'static [Self] {
        if show_advanced {
            &Self::FLOW
        } else {
            &Self::DEFAULT_FLOW
        }
    }

    pub(crate) fn is_default_surface(self) -> bool {
        Self::DEFAULT_FLOW.contains(&self)
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Storage => "Storage",
            Self::Permissions => "Permissions",
            Self::Web => "Web",
            Self::Memory => "Memory",
            Self::Compaction => "Compaction",
            Self::CodeIntelligence => "Code Intel",
            Self::Terminal => "Terminal",
            Self::Appearance => "Appearance",
            Self::Agents => "Agents",
            Self::Skills => "Skills",
            Self::Plugins => "Plugins",
            Self::Mcp => "MCP",
        }
    }

    pub(crate) fn nav_label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Storage => "storage",
            Self::Permissions => "permissions",
            Self::Web => "web",
            Self::Memory => "memory",
            Self::Compaction => "compaction",
            Self::CodeIntelligence => "code intel",
            Self::Terminal => "terminal",
            Self::Appearance => "appearance",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Plugins => "plugins",
            Self::Mcp => "mcp",
        }
    }

    pub(crate) fn step_token(self) -> &'static str {
        match self {
            Self::CodeIntelligence => "code-intel",
            _ => self.nav_label(),
        }
    }

    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Provider => "provider settings",
            Self::Storage => "local state paths",
            Self::Permissions => "safety settings",
            Self::Web => "network data tools",
            Self::Memory => "memory status",
            Self::Compaction => "context and thresholds",
            Self::CodeIntelligence => "LSP readiness",
            Self::Terminal => "terminal integration",
            Self::Appearance => "TUI theme",
            Self::Agents => "agent profiles",
            Self::Skills => "reusable skills",
            Self::Plugins => "plugin trust review",
            Self::Mcp => "MCP servers",
        }
    }

    #[cfg(test)]
    pub(crate) fn next_flow(self) -> Self {
        let index = Self::FLOW
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        Self::FLOW[(index + 1) % Self::FLOW.len()]
    }

    #[cfg(test)]
    pub(crate) fn previous_flow(self) -> Self {
        let index = Self::FLOW
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        if index == 0 {
            *Self::FLOW
                .last()
                .expect("config flow sections are non-empty")
        } else {
            Self::FLOW[index - 1]
        }
    }

    pub(crate) fn flow_index(self) -> Option<usize> {
        Self::FLOW.iter().position(|section| *section == self)
    }

    pub(crate) fn from_flow_index(index: usize) -> Option<Self> {
        Self::FLOW.get(index).copied()
    }
}
