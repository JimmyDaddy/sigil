use std::{fs, path::Path};

use anyhow::Result;
use sigil_kernel::{McpServerStartup, McpTrustClass, RootConfig};
use tempfile::tempdir;

use super::{McpCommand, McpStartupArg, execute_mcp_command};

#[test]
fn stdio_add_list_and_remove_round_trip_without_echoing_command_details() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_config(&config_path)?;

    let added = execute_mcp_command(
        &config_path,
        McpCommand::Add {
            name: "filesystem".to_owned(),
            url: None,
            bearer_token_env_var: None,
            inherit_env: vec!["MCP_TOKEN".to_owned(), "MCP_TOKEN".to_owned()],
            required: false,
            startup: McpStartupArg::Lazy,
            command: vec![
                "/private/bin/mcp-secret".to_owned(),
                "--token=must-not-echo".to_owned(),
            ],
        },
    )?;

    assert!(added.contains("filesystem"));
    assert!(!added.contains("/private/bin"));
    assert!(!added.contains("must-not-echo"));
    let config = RootConfig::load(&config_path)?;
    assert_eq!(config.mcp_servers.len(), 1);
    let server = &config.mcp_servers[0];
    assert_eq!(server.name, "filesystem");
    assert_eq!(server.startup, McpServerStartup::Lazy);
    assert!(!server.required);
    assert_eq!(server.trust.trust_class, McpTrustClass::SelfHosted);
    assert_eq!(
        server.stdio(),
        Some((
            "/private/bin/mcp-secret",
            ["--token=must-not-echo".to_owned()].as_slice(),
            ["MCP_TOKEN".to_owned()].as_slice(),
        ))
    );

    let listed = execute_mcp_command(&config_path, McpCommand::List { json: false })?;
    assert!(listed.contains("\"filesystem\""));
    assert!(listed.contains("stdio"));
    assert!(!listed.contains("/private/bin"));
    assert!(!listed.contains("must-not-echo"));

    let removed = execute_mcp_command(
        &config_path,
        McpCommand::Remove {
            name: "filesystem".to_owned(),
        },
    )?;
    assert!(removed.contains("filesystem"));
    assert!(RootConfig::load(&config_path)?.mcp_servers.is_empty());
    Ok(())
}

#[test]
fn remote_add_uses_safe_trust_defaults_and_json_list_projection() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_config(&config_path)?;

    execute_mcp_command(
        &config_path,
        McpCommand::Add {
            name: "search".to_owned(),
            url: Some("https://mcp.example.com/mcp".to_owned()),
            bearer_token_env_var: Some("SEARCH_BEARER_TOKEN".to_owned()),
            inherit_env: Vec::new(),
            required: true,
            startup: McpStartupArg::Eager,
            command: Vec::new(),
        },
    )?;

    let config = RootConfig::load(&config_path)?;
    let server = &config.mcp_servers[0];
    assert_eq!(server.trust.trust_class, McpTrustClass::ThirdParty);
    assert!(!server.trust.allow_secrets);
    assert_eq!(
        server
            .streamable_http()
            .and_then(|remote| remote.bearer_token_env_var.as_deref()),
        Some("SEARCH_BEARER_TOKEN")
    );
    let listed = execute_mcp_command(&config_path, McpCommand::List { json: true })?;
    let listed: serde_json::Value = serde_json::from_str(&listed)?;
    assert_eq!(listed[0]["name"], "search");
    assert_eq!(listed[0]["transport"], "streamable_http");
    assert_eq!(listed[0]["approval_default"], "ask");
    assert!(!listed.to_string().contains("SEARCH_BEARER_TOKEN"));
    Ok(())
}

#[test]
fn mcp_config_updates_fail_closed_for_missing_duplicate_and_ambiguous_inputs() -> Result<()> {
    let temp = tempdir()?;
    let missing = temp.path().join("missing.toml");
    assert!(
        execute_mcp_command(&missing, McpCommand::List { json: false })
            .expect_err("missing config must fail")
            .to_string()
            .contains("failed to load")
    );
    assert!(!missing.exists());

    let config_path = temp.path().join("sigil.toml");
    write_config(&config_path)?;
    let add = || McpCommand::Add {
        name: "filesystem".to_owned(),
        url: None,
        bearer_token_env_var: None,
        inherit_env: Vec::new(),
        required: false,
        startup: McpStartupArg::Eager,
        command: vec!["node".to_owned(), "server.js".to_owned()],
    };
    execute_mcp_command(&config_path, add())?;
    let unchanged = fs::read(&config_path)?;
    assert!(execute_mcp_command(&config_path, add()).is_err());
    assert_eq!(fs::read(&config_path)?, unchanged);
    assert!(
        execute_mcp_command(
            &config_path,
            McpCommand::Add {
                name: "ambiguous".to_owned(),
                url: Some("https://mcp.example.com".to_owned()),
                bearer_token_env_var: None,
                inherit_env: Vec::new(),
                required: false,
                startup: McpStartupArg::Eager,
                command: vec!["node".to_owned()],
            },
        )
        .is_err()
    );
    assert_eq!(fs::read(&config_path)?, unchanged);
    Ok(())
}

fn write_config(path: &Path) -> Result<()> {
    fs::write(
        path,
        r#"[agent]
provider = "deepseek"
model = "deepseek-chat"
"#,
    )?;
    Ok(())
}
