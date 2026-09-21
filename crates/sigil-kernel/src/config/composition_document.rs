use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};

use super::{
    AgentConfig, AppearanceConfig, ExecutionConfig, ModelRequestConfig, MultiAgentMode,
    OptionalCapability, PermissionConfig, RootConfig, RuntimeCompositionConfig, SessionConfig,
    StorageConfig, TaskRoutingPolicy, TerminalConfig, VerificationConfig, WorkspaceConfig,
};

/// Unselected module documents have no runtime semantics until their owner is selected.
#[derive(Clone, Default)]
pub(super) struct DeferredModuleConfig {
    values: BTreeMap<OptionalCapability, toml::Value>,
    pub(super) runtime_effective: bool,
}

impl DeferredModuleConfig {
    fn read<T: Default + DeserializeOwned>(
        &mut self,
        capability: OptionalCapability,
        value: Option<toml::Value>,
        selected: bool,
    ) -> std::result::Result<T, toml::de::Error> {
        let Some(value) = value else {
            return Ok(T::default());
        };
        if selected {
            value.try_into()
        } else {
            self.values.insert(capability, value);
            Ok(T::default())
        }
    }

    fn write<T: Default + Serialize>(
        &self,
        capability: OptionalCapability,
        value: &T,
    ) -> std::result::Result<toml::Value, toml::ser::Error> {
        let encoded = toml::Value::try_from(value)?;
        if !self.runtime_effective
            && let Some(original) = self.values.get(&capability)
        {
            if encoded != toml::Value::try_from(T::default())? {
                return Err(serde::ser::Error::custom(format!(
                    "cannot publish edited deferred {capability:?} configuration; select and reload the module before editing it"
                )));
            }
            return Ok(original.clone());
        }
        Ok(encoded)
    }

    fn activate<T: DeserializeOwned>(
        &mut self,
        capability: OptionalCapability,
        selected: bool,
        target: &mut T,
    ) -> std::result::Result<(), toml::de::Error> {
        if selected && let Some(value) = self.values.remove(&capability) {
            *target = value.try_into()?;
        }
        Ok(())
    }
}

/// The root wire projects the fields needed by the current runtime before optional owners are
/// considered. Unknown root keys are ignored; known fields still undergo their normal type,
/// domain, permission, and authority validation.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RootConfigDocument {
    config_version: u32,
    #[serde(default)]
    composition: RuntimeCompositionConfig,
    #[serde(default)]
    workspace: WorkspaceConfig,
    #[serde(default)]
    storage: StorageConfig,
    #[serde(default)]
    session: SessionConfig,
    agent: AgentConfig,
    #[serde(default)]
    model_request: ModelRequestConfig,
    #[serde(default)]
    permission: PermissionConfig,
    #[serde(default)]
    memory: Option<toml::Value>,
    #[serde(default)]
    skills: Option<toml::Value>,
    #[serde(default)]
    compaction: Option<toml::Value>,
    #[serde(default)]
    code_intelligence: Option<toml::Value>,
    #[serde(default)]
    terminal: TerminalConfig,
    #[serde(default)]
    execution: ExecutionConfig,
    #[serde(default, skip_serializing_if = "VerificationConfig::is_empty")]
    verification: VerificationConfig,
    #[serde(default)]
    appearance: AppearanceConfig,
    #[serde(default)]
    task: Option<toml::Value>,
    #[serde(default)]
    web: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    connections: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    mcp_servers: Option<toml::Value>,
}

impl<'de> Deserialize<'de> for RootConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let document = RootConfigDocument::deserialize(deserializer)?;
        let mut deferred = DeferredModuleConfig::default();
        let selected = &document.composition;
        let mut config = Self {
            config_version: document.config_version,
            composition: selected.selection_only(),
            workspace: document.workspace,
            storage: document.storage,
            session: document.session,
            agent: document.agent,
            model_request: document.model_request,
            permission: document.permission,
            memory: deferred
                .read(
                    OptionalCapability::Memory,
                    document.memory,
                    selected.allows(OptionalCapability::Memory),
                )
                .map_err(serde::de::Error::custom)?,
            skills: deferred
                .read(
                    OptionalCapability::Skills,
                    document.skills,
                    selected.allows(OptionalCapability::Skills),
                )
                .map_err(serde::de::Error::custom)?,
            compaction: deferred
                .read(
                    OptionalCapability::Compaction,
                    document.compaction,
                    selected.allows(OptionalCapability::Compaction),
                )
                .map_err(serde::de::Error::custom)?,
            code_intelligence: deferred
                .read(
                    OptionalCapability::CodeIntelligence,
                    document.code_intelligence,
                    selected.allows(OptionalCapability::CodeIntelligence),
                )
                .map_err(serde::de::Error::custom)?,
            terminal: document.terminal,
            execution: document.execution,
            verification: document.verification,
            appearance: document.appearance,
            task: deferred
                .read(
                    OptionalCapability::TaskOrchestration,
                    document.task,
                    selected.allows(OptionalCapability::TaskOrchestration),
                )
                .map_err(serde::de::Error::custom)?,
            web: deferred
                .read(
                    OptionalCapability::Web,
                    document.web,
                    selected.allows(OptionalCapability::Web),
                )
                .map_err(serde::de::Error::custom)?,
            connections: document.connections,
            mcp_servers: deferred
                .read(
                    OptionalCapability::Mcp,
                    document.mcp_servers,
                    selected.allows(OptionalCapability::Mcp),
                )
                .map_err(serde::de::Error::custom)?,
        };
        config.composition.deferred = deferred;
        Ok(config)
    }
}

impl Serialize for RootConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let deferred = &self.composition.deferred;
        let document = RootConfigDocument {
            config_version: self.config_version,
            composition: self.composition.selection_only(),
            workspace: self.workspace.clone(),
            storage: self.storage.clone(),
            session: self.session.clone(),
            agent: self.agent.clone(),
            model_request: self.model_request.clone(),
            permission: self.permission.clone(),
            memory: Some(
                deferred
                    .write(OptionalCapability::Memory, &self.memory)
                    .map_err(serde::ser::Error::custom)?,
            ),
            skills: Some(
                deferred
                    .write(OptionalCapability::Skills, &self.skills)
                    .map_err(serde::ser::Error::custom)?,
            ),
            compaction: Some(
                deferred
                    .write(OptionalCapability::Compaction, &self.compaction)
                    .map_err(serde::ser::Error::custom)?,
            ),
            code_intelligence: Some(
                deferred
                    .write(
                        OptionalCapability::CodeIntelligence,
                        &self.code_intelligence,
                    )
                    .map_err(serde::ser::Error::custom)?,
            ),
            terminal: self.terminal.clone(),
            execution: self.execution.clone(),
            verification: self.verification.clone(),
            appearance: self.appearance.clone(),
            task: Some(
                deferred
                    .write(OptionalCapability::TaskOrchestration, &self.task)
                    .map_err(serde::ser::Error::custom)?,
            ),
            web: Some(
                deferred
                    .write(OptionalCapability::Web, &self.web)
                    .map_err(serde::ser::Error::custom)?,
            ),
            connections: self.connections.clone(),
            mcp_servers: Some(
                deferred
                    .write(OptionalCapability::Mcp, &self.mcp_servers)
                    .map_err(serde::ser::Error::custom)?,
            ),
        };
        document.serialize(serializer)
    }
}

impl RootConfig {
    /// Lists the effective owner selection using the loaded module enable flags. Call this on
    /// the runtime view after changing a composition selection in memory.
    #[must_use]
    pub fn selected_capabilities(&self) -> std::collections::BTreeSet<OptionalCapability> {
        self.composition
            .selected_capabilities()
            .into_iter()
            .filter(|capability| match capability {
                OptionalCapability::TaskOrchestration => self.task.enabled,
                OptionalCapability::Memory => self.memory.enabled || self.memory.writable,
                OptionalCapability::Skills => self.skills.enabled,
                OptionalCapability::CodeIntelligence => self.code_intelligence.enabled,
                OptionalCapability::Web => self.web.enabled,
                OptionalCapability::Compaction => self.compaction.enabled,
                _ => true,
            })
            .collect()
    }

    /// Builds the runtime view without changing the configuration kept for durable publication.
    /// Unselected module fields are placeholders until this view explicitly disables their owners.
    ///
    /// # Errors
    ///
    /// Fails if a newly selected deferred module has invalid configuration. The returned view
    /// cannot be saved with `persisted_toml` or the config publication APIs.
    pub fn with_effective_composition(&self) -> Result<Self> {
        let mut effective = self.clone();
        let selected = effective.composition.selection_only();
        let deferred = &mut effective.composition.deferred;
        deferred.activate(
            OptionalCapability::Memory,
            selected.allows(OptionalCapability::Memory),
            &mut effective.memory,
        )?;
        deferred.activate(
            OptionalCapability::Skills,
            selected.allows(OptionalCapability::Skills),
            &mut effective.skills,
        )?;
        deferred.activate(
            OptionalCapability::Compaction,
            selected.allows(OptionalCapability::Compaction),
            &mut effective.compaction,
        )?;
        deferred.activate(
            OptionalCapability::CodeIntelligence,
            selected.allows(OptionalCapability::CodeIntelligence),
            &mut effective.code_intelligence,
        )?;
        deferred.activate(
            OptionalCapability::TaskOrchestration,
            selected.allows(OptionalCapability::TaskOrchestration),
            &mut effective.task,
        )?;
        deferred.activate(
            OptionalCapability::Web,
            selected.allows(OptionalCapability::Web),
            &mut effective.web,
        )?;
        deferred.activate(
            OptionalCapability::Mcp,
            selected.allows(OptionalCapability::Mcp),
            &mut effective.mcp_servers,
        )?;
        deferred.values.clear();
        deferred.runtime_effective = true;
        effective.validate_selected_module_config()?;
        if !selected.allows(OptionalCapability::TaskOrchestration) {
            effective.task.enabled = false;
            effective.task.routing_policy = TaskRoutingPolicy::Manual;
            effective.task.multi_agent_mode = MultiAgentMode::None;
        }
        if !selected.allows(OptionalCapability::Memory) {
            effective.memory.enabled = false;
            effective.memory.writable = false;
        }
        if !selected.allows(OptionalCapability::Skills) {
            effective.skills.enabled = false;
        }
        if !selected.allows(OptionalCapability::Compaction) {
            effective.compaction.enabled = false;
            effective.compaction.native_carrier_enabled = false;
        }
        if !selected.allows(OptionalCapability::CodeIntelligence) {
            effective.code_intelligence.enabled = false;
        }
        if !selected.allows(OptionalCapability::Web) {
            effective.web.enabled = false;
        }
        if !selected.allows(OptionalCapability::Mcp) {
            effective.mcp_servers.clear();
        }
        Ok(effective)
    }
}

#[cfg(test)]
#[path = "../tests/composition_config_tests.rs"]
mod tests;
