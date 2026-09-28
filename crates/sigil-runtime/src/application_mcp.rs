//! Host-local MCP declarations supplied for one application run, never persisted as user config.

use anyhow::{Result, ensure};
use sigil_kernel::{McpServerConfig, McpServerStartup, RootConfig, SecretString};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// One explicit transport declaration with process-local environment values. Debug remains
/// secret-safe; serialization is intentionally unavailable.
#[derive(Debug, Clone)]
pub struct ApplicationMcpServerDeclaration {
    pub server: McpServerConfig,
    pub environment: BTreeMap<String, SecretString>,
}

pub(crate) type ProcessEnvironments = Arc<BTreeMap<String, BTreeMap<String, SecretString>>>;

pub(crate) fn environments(
    declarations: &[ApplicationMcpServerDeclaration],
) -> Result<ProcessEnvironments> {
    let mut result = BTreeMap::new();
    for declaration in declarations {
        let Some((_, _, inherited)) = declaration.server.stdio() else {
            anyhow::bail!("application MCP process environments require a stdio declaration");
        };
        ensure!(
            inherited.is_empty(),
            "application MCP environment must be explicitly supplied, not inherited"
        );
        ensure!(
            declaration.server.startup == McpServerStartup::Lazy,
            "application MCP declarations must activate through the existing approval route"
        );
        ensure!(
            !declaration.server.name.trim().is_empty(),
            "application MCP server name is empty"
        );
        sigil_kernel::process_environment::resolve_explicit_extension_process_environment(
            &declaration.environment,
        )?;
        ensure!(
            result
                .insert(
                    declaration.server.name.clone(),
                    declaration.environment.clone()
                )
                .is_none(),
            "duplicate application MCP server name"
        );
    }
    Ok(Arc::new(result))
}

pub(crate) fn merge(
    root: &mut RootConfig,
    declarations: &[ApplicationMcpServerDeclaration],
) -> Result<()> {
    if declarations.is_empty() {
        return Ok(());
    }
    ensure!(
        root.composition
            .allows(sigil_kernel::OptionalCapability::Mcp),
        "MCP is disabled by the selected application composition"
    );
    let mut names = root
        .mcp_servers
        .iter()
        .map(|server| server.name.clone())
        .collect::<BTreeSet<_>>();
    for declaration in declarations {
        ensure!(
            names.insert(declaration.server.name.clone()),
            "application MCP server {} conflicts with an existing declaration",
            declaration.server.name
        );
    }
    root.mcp_servers.extend(
        declarations
            .iter()
            .map(|declaration| declaration.server.clone()),
    );
    Ok(())
}

#[cfg(test)]
#[path = "tests/application_mcp_tests.rs"]
mod tests;
