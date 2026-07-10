use sigil_kernel::{PluginManifestSnapshot, SkillDescriptor, SkillRunMode};
use sigil_runtime::ResolvedAgentProfile;

use super::{ConfigField, ConfigFieldMove, ConfigSection, ConfigState, McpServerDraft};

impl ConfigState {
    pub(super) fn section_collection_is_empty(&self, section: ConfigSection) -> bool {
        match section {
            ConfigSection::Mcp => self.draft.mcp_servers.is_empty(),
            ConfigSection::Agents => self.agent_profiles.is_empty(),
            ConfigSection::Skills => {
                skill_display_order_for_section(&self.skill_descriptors, ConfigSection::Skills)
                    .is_empty()
            }
            ConfigSection::Plugins => self.plugin_manifests.is_empty(),
            _ => false,
        }
    }

    pub(crate) fn sync_mcp_selection(&mut self) {
        if self.draft.mcp_servers.is_empty() {
            self.selected_mcp_server_index = 0;
            if self.selected_section == ConfigSection::Mcp {
                self.selected_field = None;
            }
            return;
        }
        self.selected_mcp_server_index = self
            .selected_mcp_server_index
            .min(self.draft.mcp_servers.len().saturating_sub(1));
    }

    pub(crate) fn set_agent_discovery(
        &mut self,
        profiles: Vec<ResolvedAgentProfile>,
        warnings: Vec<String>,
    ) {
        self.agent_profiles = profiles;
        self.agent_warnings = warnings;
        self.sync_agent_selection();
        if self.selected_section == ConfigSection::Agents {
            self.selected_field = self.first_field_for_section(ConfigSection::Agents);
        }
    }

    pub(crate) fn sync_agent_selection(&mut self) {
        if self.agent_profiles.is_empty() {
            self.selected_agent_index = 0;
            if self.selected_section == ConfigSection::Agents {
                self.selected_field = None;
            }
            return;
        }
        self.selected_agent_index = self
            .selected_agent_index
            .min(self.agent_profiles.len().saturating_sub(1));
    }

    pub(crate) fn set_skill_discovery(
        &mut self,
        descriptors: Vec<SkillDescriptor>,
        warnings: Vec<String>,
    ) {
        self.skill_descriptors = descriptors;
        self.skill_warnings = warnings;
        if let Some(first_index) =
            skill_display_order_for_section(&self.skill_descriptors, self.selected_section).first()
        {
            self.selected_skill_index = *first_index;
        } else if let Some(first_index) = skill_display_order(&self.skill_descriptors).first() {
            self.selected_skill_index = *first_index;
        }
        self.sync_skill_selection();
        if self.selected_section == ConfigSection::Skills {
            self.selected_field = self.first_field_for_section(self.selected_section);
        }
    }

    pub(crate) fn sync_skill_selection(&mut self) {
        if self.skill_descriptors.is_empty() {
            self.selected_skill_index = 0;
            if self.selected_section == ConfigSection::Skills {
                self.selected_field = None;
            }
            return;
        }
        let section_order =
            skill_display_order_for_section(&self.skill_descriptors, self.selected_section);
        if !section_order.is_empty() && !section_order.contains(&self.selected_skill_index) {
            self.selected_skill_index = section_order[0];
            return;
        }
        self.selected_skill_index = self
            .selected_skill_index
            .min(self.skill_descriptors.len().saturating_sub(1));
    }

    pub(crate) fn set_plugin_discovery(
        &mut self,
        manifests: Vec<PluginManifestSnapshot>,
        warnings: Vec<String>,
    ) {
        self.plugin_manifests = manifests;
        self.plugin_warnings = warnings;
        self.sync_plugin_selection();
        if self.selected_section == ConfigSection::Plugins {
            self.selected_field = self.first_field_for_section(ConfigSection::Plugins);
        }
    }

    pub(crate) fn sync_plugin_selection(&mut self) {
        if self.plugin_manifests.is_empty() {
            self.selected_plugin_index = 0;
            if self.selected_section == ConfigSection::Plugins {
                self.selected_field = None;
            }
            return;
        }
        self.selected_plugin_index = self
            .selected_plugin_index
            .min(self.plugin_manifests.len().saturating_sub(1));
    }

    pub(crate) fn selected_skill(&self) -> Option<&SkillDescriptor> {
        let skill = self.skill_descriptors.get(self.selected_skill_index)?;
        match self.selected_section {
            ConfigSection::Agents if !skill_is_agent(skill) => None,
            ConfigSection::Skills if skill_is_agent(skill) => None,
            _ => Some(skill),
        }
    }

    pub(crate) fn selected_agent(&self) -> Option<&ResolvedAgentProfile> {
        self.agent_profiles.get(self.selected_agent_index)
    }

    pub(crate) fn cycle_agent(&mut self, forward: bool) -> bool {
        if self.agent_profiles.is_empty() {
            return false;
        }
        let len = self.agent_profiles.len();
        if forward {
            self.selected_agent_index = (self.selected_agent_index + 1) % len;
        } else if self.selected_agent_index == 0 {
            self.selected_agent_index = len - 1;
        } else {
            self.selected_agent_index -= 1;
        }
        true
    }

    pub(crate) fn move_agent(&mut self, forward: bool) -> ConfigFieldMove {
        if self.agent_profiles.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        if forward {
            if self.selected_agent_index + 1 >= self.agent_profiles.len() {
                return ConfigFieldMove::Boundary;
            }
            self.selected_agent_index += 1;
        } else {
            if self.selected_agent_index == 0 {
                return ConfigFieldMove::Boundary;
            }
            self.selected_agent_index -= 1;
        }
        self.selected_field = Some(ConfigField::SkillId);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    pub(crate) fn cycle_skill(&mut self, forward: bool) -> bool {
        let order = skill_display_order_for_section(&self.skill_descriptors, self.selected_section);
        if order.is_empty() {
            return false;
        }
        let current_position = order
            .iter()
            .position(|index| *index == self.selected_skill_index)
            .unwrap_or(0);
        let next_position = if forward {
            (current_position + 1) % order.len()
        } else if current_position == 0 {
            order.len() - 1
        } else {
            current_position - 1
        };
        self.selected_skill_index = order[next_position];
        true
    }

    pub(crate) fn move_skill(&mut self, forward: bool) -> ConfigFieldMove {
        let order = skill_display_order_for_section(&self.skill_descriptors, self.selected_section);
        if order.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        let current_position = order
            .iter()
            .position(|index| *index == self.selected_skill_index)
            .unwrap_or(0);
        if forward {
            if current_position + 1 >= order.len() {
                return ConfigFieldMove::Boundary;
            }
            self.selected_skill_index = order[current_position + 1];
        } else {
            if current_position == 0 {
                return ConfigFieldMove::Boundary;
            }
            self.selected_skill_index = order[current_position - 1];
        }
        self.selected_field = Some(ConfigField::SkillId);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    pub(crate) fn selected_plugin(&self) -> Option<&PluginManifestSnapshot> {
        self.plugin_manifests.get(self.selected_plugin_index)
    }

    pub(crate) fn selected_plugin_mut(&mut self) -> Option<&mut PluginManifestSnapshot> {
        self.plugin_manifests.get_mut(self.selected_plugin_index)
    }

    pub(crate) fn cycle_plugin(&mut self, forward: bool) -> bool {
        if self.plugin_manifests.is_empty() {
            return false;
        }
        let len = self.plugin_manifests.len();
        if forward {
            self.selected_plugin_index = (self.selected_plugin_index + 1) % len;
        } else if self.selected_plugin_index == 0 {
            self.selected_plugin_index = len - 1;
        } else {
            self.selected_plugin_index -= 1;
        }
        true
    }

    pub(crate) fn move_plugin(&mut self, forward: bool) -> ConfigFieldMove {
        if self.plugin_manifests.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        if forward {
            if self.selected_plugin_index + 1 >= self.plugin_manifests.len() {
                return ConfigFieldMove::Boundary;
            }
            self.selected_plugin_index += 1;
        } else {
            if self.selected_plugin_index == 0 {
                return ConfigFieldMove::Boundary;
            }
            self.selected_plugin_index -= 1;
        }
        self.selected_field = Some(ConfigField::PluginId);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    pub(crate) fn selected_mcp_server(&self) -> Option<&McpServerDraft> {
        self.draft.mcp_servers.get(self.selected_mcp_server_index)
    }

    pub(crate) fn selected_mcp_server_mut(&mut self) -> Option<&mut McpServerDraft> {
        self.draft
            .mcp_servers
            .get_mut(self.selected_mcp_server_index)
    }

    #[allow(dead_code)]
    pub(crate) fn add_mcp_server(&mut self) {
        let next_index = self.draft.mcp_servers.len() + 1;
        self.draft
            .mcp_servers
            .push(McpServerDraft::new_default(format!("server-{next_index}")));
        self.selected_mcp_server_index = self.draft.mcp_servers.len() - 1;
        if self.selected_section == ConfigSection::Mcp {
            self.footer_selected = false;
            self.selected_field = None;
        }
        self.dirty = true;
    }

    #[allow(dead_code)]
    pub(crate) fn remove_selected_mcp_server(&mut self) -> bool {
        if self.draft.mcp_servers.is_empty() {
            return false;
        }
        self.draft
            .mcp_servers
            .remove(self.selected_mcp_server_index);
        self.sync_mcp_selection();
        if self.selected_section == ConfigSection::Mcp && self.draft.mcp_servers.is_empty() {
            self.selected_field = None;
        }
        self.dirty = true;
        true
    }

    pub(crate) fn cycle_mcp_server(&mut self, forward: bool) -> bool {
        if self.draft.mcp_servers.is_empty() {
            return false;
        }
        let len = self.draft.mcp_servers.len();
        if forward {
            self.selected_mcp_server_index = (self.selected_mcp_server_index + 1) % len;
        } else if self.selected_mcp_server_index == 0 {
            self.selected_mcp_server_index = len - 1;
        } else {
            self.selected_mcp_server_index -= 1;
        }
        self.selected_field = Some(ConfigField::McpName);
        self.footer_selected = false;
        true
    }
}

fn skill_display_order(descriptors: &[SkillDescriptor]) -> Vec<usize> {
    let mut agents = Vec::new();
    let mut skills = Vec::new();
    for (index, descriptor) in descriptors.iter().enumerate() {
        if skill_is_agent(descriptor) {
            agents.push(index);
        } else {
            skills.push(index);
        }
    }
    agents.extend(skills);
    agents
}

fn skill_display_order_for_section(
    descriptors: &[SkillDescriptor],
    section: ConfigSection,
) -> Vec<usize> {
    match section {
        ConfigSection::Agents => descriptors
            .iter()
            .enumerate()
            .filter_map(|(index, descriptor)| skill_is_agent(descriptor).then_some(index))
            .collect(),
        ConfigSection::Skills => descriptors
            .iter()
            .enumerate()
            .filter_map(|(index, descriptor)| (!skill_is_agent(descriptor)).then_some(index))
            .collect(),
        _ => skill_display_order(descriptors),
    }
}

fn skill_is_agent(descriptor: &SkillDescriptor) -> bool {
    matches!(descriptor.run_as, SkillRunMode::ChildSession)
}
