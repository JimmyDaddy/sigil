use std::collections::BTreeMap;

use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

/// Builds the provider-neutral base used while a product surface is collecting first-run setup.
///
/// The value is intentionally not runnable until a provider connection and compound default model
/// are materialized. It is safe to use for setup-only server state and as the base for an atomic
/// first publish.
#[must_use]
pub fn default_setup_root_config() -> RootConfig {
    RootConfig {
        config_version: None,
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig::default(),
        agent: AgentConfig {
            provider: String::new(),
            connection: None,
            model: String::new(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        connections: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}
