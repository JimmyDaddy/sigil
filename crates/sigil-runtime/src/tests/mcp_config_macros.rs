macro_rules! set_mcp_server_config_field {
    ($config:ident, command, $value:expr) => {
        let sigil_kernel::McpServerTransportConfig::Stdio { command, .. } = &mut $config.transport
        else {
            panic!("test MCP config must use stdio transport");
        };
        *command = $value;
    };
    ($config:ident, args, $value:expr) => {
        let sigil_kernel::McpServerTransportConfig::Stdio { args, .. } = &mut $config.transport
        else {
            panic!("test MCP config must use stdio transport");
        };
        *args = $value;
    };
    ($config:ident, inherit_env, $value:expr) => {
        let sigil_kernel::McpServerTransportConfig::Stdio { inherit_env, .. } =
            &mut $config.transport
        else {
            panic!("test MCP config must use stdio transport");
        };
        *inherit_env = $value;
    };
    ($config:ident, $field:ident, $value:expr) => {
        $config.$field = $value;
    };
}

macro_rules! mcp_server_config {
    () => {
        sigil_kernel::McpServerConfig::default()
    };
    (.. $base:expr $(,)?) => {{
        let base: sigil_kernel::McpServerConfig = $base;
        base
    }};
    ($field:ident: $value:expr, $($rest:tt)*) => {{
        let mut config = mcp_server_config!($($rest)*);
        set_mcp_server_config_field!(config, $field, $value);
        config
    }};
    ($field:ident, $($rest:tt)*) => {{
        let mut config = mcp_server_config!($($rest)*);
        set_mcp_server_config_field!(config, $field, $field.clone());
        config
    }};
    ($field:ident: $value:expr $(,)?) => {{
        let mut config = sigil_kernel::McpServerConfig::default();
        set_mcp_server_config_field!(config, $field, $value);
        config
    }};
    ($field:ident $(,)?) => {{
        let mut config = sigil_kernel::McpServerConfig::default();
        set_mcp_server_config_field!(config, $field, $field.clone());
        config
    }};
}
