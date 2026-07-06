use super::ConfigSection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFooterAction {
    Save,
    SaveAndClose,
    CleanMutationArtifacts,
    ActivateMcp,
    TrustAgent,
    BlockAgent,
    #[cfg(test)]
    ToggleAgentEnabled,
    #[cfg(test)]
    ToggleAgentUser,
    #[cfg(test)]
    ToggleAgentModel,
    UseSkill,
    ApprovePlugin,
    DenyPlugin,
    Close,
}

impl ConfigFooterAction {
    const DEFAULT_ORDER: [Self; 3] = [Self::Save, Self::SaveAndClose, Self::Close];
    const STORAGE_ORDER: [Self; 3] = [
        Self::CleanMutationArtifacts,
        Self::SaveAndClose,
        Self::Close,
    ];
    const PERMISSIONS_ORDER: [Self; 3] = [Self::Save, Self::SaveAndClose, Self::Close];
    const MCP_ORDER: [Self; 3] = [Self::ActivateMcp, Self::SaveAndClose, Self::Close];
    const AGENTS_ORDER: [Self; 3] = [Self::TrustAgent, Self::BlockAgent, Self::SaveAndClose];
    const SKILLS_ORDER: [Self; 3] = [Self::UseSkill, Self::SaveAndClose, Self::Close];
    const PLUGINS_ORDER: [Self; 3] = [Self::ApprovePlugin, Self::DenyPlugin, Self::SaveAndClose];

    pub(crate) fn actions_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Mcp => &Self::MCP_ORDER,
            ConfigSection::Storage => &Self::STORAGE_ORDER,
            ConfigSection::Permissions => &Self::PERMISSIONS_ORDER,
            ConfigSection::Agents => &Self::AGENTS_ORDER,
            ConfigSection::Skills => &Self::SKILLS_ORDER,
            ConfigSection::Plugins => &Self::PLUGINS_ORDER,
            ConfigSection::Provider
            | ConfigSection::Memory
            | ConfigSection::Compaction
            | ConfigSection::CodeIntelligence
            | ConfigSection::Terminal
            | ConfigSection::Appearance => &Self::DEFAULT_ORDER,
        }
    }

    pub(crate) fn action_for_section_index(section: ConfigSection, index: usize) -> Option<Self> {
        Self::actions_for_section(section).get(index).copied()
    }

    pub(crate) fn button_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save+close",
            Self::CleanMutationArtifacts => "clean",
            Self::ActivateMcp => "activate",
            Self::TrustAgent => "trust",
            Self::BlockAgent => "disable",
            #[cfg(test)]
            Self::ToggleAgentEnabled => "enable",
            #[cfg(test)]
            Self::ToggleAgentUser => "user",
            #[cfg(test)]
            Self::ToggleAgentModel => "model",
            Self::UseSkill => "use",
            Self::ApprovePlugin => "approve",
            Self::DenyPlugin => "deny",
            Self::Close => "close",
        }
    }

    pub(crate) fn field_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save_and_close",
            Self::CleanMutationArtifacts => "clean_artifacts",
            Self::ActivateMcp => "activate_mcp",
            Self::TrustAgent => "trust_agent",
            Self::BlockAgent => "disable_agent",
            #[cfg(test)]
            Self::ToggleAgentEnabled => "toggle_agent_enabled",
            #[cfg(test)]
            Self::ToggleAgentUser => "toggle_agent_user",
            #[cfg(test)]
            Self::ToggleAgentModel => "toggle_agent_model",
            Self::UseSkill => "use_skill",
            Self::ApprovePlugin => "approve_plugin",
            Self::DenyPlugin => "deny_plugin",
            Self::Close => "close",
        }
    }

    pub(crate) fn next_for_section(self, section: ConfigSection) -> Self {
        let actions = Self::actions_for_section(section);
        let index = actions
            .iter()
            .position(|action| *action == self)
            .unwrap_or(0);
        actions[(index + 1) % actions.len()]
    }

    pub(crate) fn previous_for_section(self, section: ConfigSection) -> Self {
        let actions = Self::actions_for_section(section);
        let index = actions
            .iter()
            .position(|action| *action == self)
            .unwrap_or(0);
        if index == 0 {
            *actions.last().expect("footer actions are non-empty")
        } else {
            actions[index - 1]
        }
    }
}
