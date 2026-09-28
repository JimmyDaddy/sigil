use super::*;

fn client_server(name: &str) -> ApplicationMcpServerDeclaration {
    ApplicationMcpServerDeclaration {
        server: McpServerConfig {
            name: name.to_owned(),
            startup: McpServerStartup::Lazy,
            transport: sigil_kernel::McpServerTransportConfig::Stdio {
                command: "fixture-command-not-launched".to_owned(),
                args: Vec::new(),
                inherit_env: Vec::new(),
            },
            ..Default::default()
        },
        environment: BTreeMap::from([(
            "EXAMPLE_VALUE".to_owned(),
            SecretString::new("private-client-value"),
        )]),
    }
}

#[test]
fn application_mcp_overlay_is_not_persisted_and_conflicts_are_explicit() -> Result<()> {
    let mut config = crate::provider_connections::default_setup_root_config();
    config
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::Mcp);
    let original = toml::to_string(&config)?;
    let declaration = client_server("editor-server");
    let values = environments(std::slice::from_ref(&declaration))?;
    assert_eq!(
        values["editor-server"]["EXAMPLE_VALUE"].expose_secret(),
        "private-client-value"
    );
    merge(&mut config, std::slice::from_ref(&declaration))?;
    assert!(!toml::to_string(&config)?.contains("private-client-value"));
    assert!(!original.contains("editor-server"));
    assert!(merge(&mut config, std::slice::from_ref(&declaration)).is_err());
    assert!(environments(&[declaration.clone(), declaration]).is_err());
    Ok(())
}
