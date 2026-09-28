use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol::schema::v1 as acp;
use anyhow::{Result, bail, ensure};
use sigil_kernel::{McpServerConfig, McpServerStartup, McpServerTransportConfig, SecretString};

/// Client declarations are run-scoped input; secrets never enter the saved root configuration.
pub(super) fn declarations(
    servers: Vec<acp::McpServer>,
) -> Result<Vec<sigil_runtime::ApplicationMcpServerDeclaration>> {
    let mut names = BTreeSet::new();
    let mut declarations = Vec::with_capacity(servers.len());
    for server in servers {
        let server = match server {
            acp::McpServer::Stdio(server) => server,
            _ => bail!("ACP remote MCP transport is not advertised by this adapter"),
        };
        ensure!(
            names.insert(server.name.clone()),
            "duplicate ACP MCP server name: {}",
            server.name
        );
        ensure!(
            server.command.is_absolute(),
            "ACP stdio MCP executable must be absolute"
        );
        let command = server
            .command
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("ACP MCP executable is not UTF-8"))?
            .to_owned();
        let mut environment = BTreeMap::new();
        for variable in server.env {
            ensure!(
                !environment.contains_key(&variable.name),
                "duplicate ACP MCP environment name: {}",
                variable.name
            );
            ensure!(
                !variable.value.contains('\0'),
                "ACP MCP environment value contains a NUL byte"
            );
            environment.insert(variable.name, SecretString::new(variable.value));
        }
        sigil_kernel::normalize_environment_variable_names(
            &environment.keys().cloned().collect::<Vec<_>>(),
        )?;
        declarations.push(sigil_runtime::ApplicationMcpServerDeclaration {
            server: McpServerConfig {
                name: server.name,
                transport: McpServerTransportConfig::Stdio {
                    command,
                    args: server.args,
                    inherit_env: Vec::new(),
                },
                startup: McpServerStartup::Lazy,
                required: false,
                ..McpServerConfig::default()
            },
            environment,
        });
    }
    Ok(declarations)
}

#[cfg(test)]
#[path = "tests/mcp_tests.rs"]
mod tests;
