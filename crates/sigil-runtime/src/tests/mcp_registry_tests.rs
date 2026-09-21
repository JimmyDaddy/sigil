use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use sigil_kernel::{
    ControlEntry, JsonlSessionStore, PluginTrustDecision, PluginTrustEntry, RootConfig,
    SessionLogEntry,
};
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, fs::symlink};

use super::*;

fn core_config_with_unselected_payloads(workspace: &Path) -> Result<RootConfig> {
    let mut config = RootConfig::parse_persisted(
        r#"
config_version = 2
memory = "unselected invalid memory"
skills = "unselected invalid skills"
code_intelligence = "unselected invalid code intelligence"
web = "unselected invalid web"
mcp_servers = "unselected invalid MCP"
[agent]
connection = "fixture"
model = "fixture"
[composition]
profile = "core"
"#,
    )?;
    // Public builders must apply composition even when callers mutate a parsed RootConfig.
    config.memory.enabled = true;
    config.memory.writable = true;
    config.skills.enabled = true;
    config.code_intelligence.enabled = true;
    config.web.enabled = true;
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    Ok(config)
}

fn assert_core_tools_only(registry: &ToolRegistry) {
    let actual = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = [
        "read_file",
        "read_tool_artifact",
        "write_file",
        "edit_file",
        "delete_file",
        "ls",
        "glob",
        "grep",
        "vcs_inspect",
        "exec_command",
        "exec_read",
        "exec_wait",
        "exec_input",
        "exec_resize",
        "exec_cancel",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn core_public_builders_apply_selection_before_optional_configuration() -> Result<()> {
    let _environment = crate::test_env::lock();
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(
        workspace.join("README.md"),
        "must not retrieve this repository context",
    )?;
    let config = core_config_with_unselected_payloads(&workspace)?;
    let capabilities = provider_capabilities_for_name("deepseek").expect("capabilities");
    let surface = build_tool_surface_without_eager_mcp_with_workspace_trust(
        &config,
        &capabilities,
        workspace.clone(),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        WorkspaceTrust::Trusted,
    )?;
    assert_core_tools_only(&surface.registry);
    assert!(surface.terminal_control.is_some());
    assert!(!surface.context_resolver.has_shared_code_intelligence());
    assert!(config.memory.writable && config.skills.enabled && config.code_intelligence.enabled);
    let eager = build_tool_registry(&config, &capabilities, workspace.clone()).await?;
    assert_core_tools_only(&eager);
    assert!(!workspace.join("state").exists());
    assert!(!workspace.join("cache").exists());
    fs::remove_dir_all(&workspace)?;
    let context = surface.context_resolver.resolve("README.md").await?;
    assert!(context.items.is_empty());
    assert!(context.snippets.is_empty());
    assert!(!workspace.exists());
    Ok(())
}

#[tokio::test]
async fn core_explicit_mcp_activation_refresh_and_registration_are_unavailable() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let config = core_config_with_unselected_payloads(fixture.path())?;
    let capabilities = provider_capabilities_for_name("deepseek").expect("capabilities");
    let mut registry = ToolRegistry::new();
    let activation = activate_lazy_mcp_tools_detailed(
        &mut registry,
        &config,
        &capabilities,
        fixture.path().to_path_buf(),
        Some("server"),
    )
    .await
    .expect_err("unselected MCP must not report a successful no-op activation");
    assert!(activation.to_string().contains("MCP is unavailable"));
    let refresh = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &capabilities,
        fixture.path().to_path_buf(),
        "server",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
    .expect_err("unselected MCP must not report a successful no-op refresh");
    assert!(refresh.to_string().contains("MCP is unavailable"));
    let registration = match register_mcp_server_declarations(
        &mut registry,
        &config,
        &capabilities,
        fixture.path().to_path_buf(),
        &[],
        McpDeclarationRegistrationOptions::new(McpServerStartup::Eager),
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("unselected MCP registration must fail before inspecting declarations"),
    };
    assert!(registration.to_string().contains("MCP is unavailable"));
    assert!(registry.specs().is_empty());
    Ok(())
}

#[test]
fn selecting_optional_owner_reactivates_its_configuration_validation() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let config = core_config_with_unselected_payloads(fixture.path())?;
    let capabilities = provider_capabilities_for_name("deepseek").expect("capabilities");
    for capability in [
        sigil_kernel::OptionalCapability::Memory,
        sigil_kernel::OptionalCapability::Skills,
        sigil_kernel::OptionalCapability::CodeIntelligence,
        sigil_kernel::OptionalCapability::Web,
        sigil_kernel::OptionalCapability::Mcp,
    ] {
        let mut selected = config.clone();
        selected.composition.enhancements.insert(capability);
        let result = build_tool_surface_without_eager_mcp_with_workspace_trust(
            &selected,
            &capabilities,
            fixture.path().to_path_buf(),
            sigil_mcp::unsupported_mcp_elicitation_handler(),
            sigil_mcp::unsupported_mcp_runtime_event_handler(),
            WorkspaceTrust::Trusted,
        );
        assert!(
            result.is_err(),
            "selected {capability:?} must validate its deferred configuration"
        );
    }
    Ok(())
}

struct UnexpectedCoreDisclosurePresenter;

#[async_trait::async_trait]
impl sigil_kernel::EgressDisclosurePresenter for UnexpectedCoreDisclosurePresenter {
    async fn present(
        &self,
        _disclosure: sigil_kernel::PreEgressDisclosure,
    ) -> std::result::Result<
        sigil_kernel::DisclosurePresentationReceipt,
        sigil_kernel::DisclosurePresentationError,
    > {
        panic!("core must not activate a web or MCP owner");
    }
}

#[test]
fn core_presenter_attachment_does_not_register_optional_tools() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let config = core_config_with_unselected_payloads(fixture.path())?;
    let capabilities = provider_capabilities_for_name("deepseek").expect("capabilities");
    let mut registry = ToolRegistry::new();
    attach_remote_mcp_activation_presenter(
        &mut registry,
        &config,
        &capabilities,
        fixture.path().to_path_buf(),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        Arc::new(UnexpectedCoreDisclosurePresenter),
    )?;
    assert!(registry.specs().is_empty());
    assert!(!fixture.path().join("state").exists());
    assert!(!fixture.path().join("cache").exists());
    Ok(())
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("fixture parent should create");
    }
    fs::write(path, content).expect("fixture should write");
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    write_file(path, content);
    let mut permissions = fs::metadata(path)
        .expect("fixture metadata should read")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fixture should become executable");
}

fn session_trust_source(workspace: &Path, trust: PluginTrustEntry) -> (JsonlSessionStore, PathBuf) {
    let path = workspace.join("state/session.jsonl");
    let store = JsonlSessionStore::new(&path).expect("session trust store should create");
    store
        .append(&SessionLogEntry::Control(
            ControlEntry::PluginTrustDecision(trust),
        ))
        .expect("current trust should append");
    (store, path)
}

#[cfg(unix)]
#[test]
fn declaration_launcher_rechecks_relative_symlink_before_spawn() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let manifest_path = plugin_root.join("plugin.toml");
    write_file(
        &manifest_path,
        r#"id = "fixture"
name = "Fixture"
version = "1.0.0"

[[mcp_servers]]
transport = "stdio"
name = "server"
command = "./bin/server"
startup = "eager"
"#,
    );
    let inside = plugin_root.join("inside/actual-server");
    write_executable(&inside, "#!/bin/sh\nexit 0\n");
    fs::create_dir_all(plugin_root.join("bin")).expect("bin should create");
    let command_link = plugin_root.join("bin/server");
    symlink(&inside, &command_link).expect("inside symlink should create");

    let pending =
        crate::discover_workspace_plugins(workspace.path(), &[]).expect("plugin should discover");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let trusted = crate::discover_workspace_plugins(workspace.path(), std::slice::from_ref(&trust))
        .expect("trusted plugin should discover");
    let declarations =
        crate::merge_mcp_server_declarations(&[], &trusted.registrations.mcp_servers)
            .expect("declaration should merge");
    let root_config: RootConfig = toml::from_str(
        r#"config_version = 2

[agent]
connection = "local"
model = "test"

[connections.local]
label = "Local"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }
"#,
    )
    .expect("root config should parse");
    let (_, trust_path) = session_trust_source(workspace.path(), trust);
    let launcher = declaration_mcp_process_launcher(
        &root_config,
        &declarations,
        Some(Arc::new(SessionMcpPluginTrustSource::new(trust_path))),
        None,
    )
    .expect("launcher should build");
    let request = launcher
        .resolve_launch_request(declarations[0].config(), None)
        .expect("initial declaration resolution should succeed");
    assert_eq!(
        request
            .declaration
            .as_ref()
            .expect("request should carry declaration metadata")
            .execution_base_kind,
        "plugin_root"
    );

    fs::remove_file(&command_link).expect("inside symlink should remove");
    let marker = workspace.path().join("spawned-marker");
    let outside = workspace.path().join("outside-server");
    write_executable(
        &outside,
        &format!("#!/bin/sh\ntouch {:?}\n", marker.to_string_lossy()),
    );
    symlink(&outside, &command_link).expect("escaping symlink should create");

    let error = match launcher.launch(request) {
        Ok(_) => panic!("fresh pre-spawn resolution must reject symlink drift"),
        Err(error) => error,
    };
    let typed = error
        .downcast_ref::<McpRegistrationError>()
        .expect("symlink drift should preserve typed declaration error");
    assert_eq!(typed.code(), "mcp_command_symlink_escape");
    assert!(
        !marker.exists(),
        "rejected declaration must remain zero-spawn"
    );
}

#[cfg(unix)]
#[test]
fn declaration_launcher_reloads_disabled_trust_immediately_before_spawn() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let marker = workspace.path().join("spawned-marker");
    let server = plugin_root.join("bin/server");
    write_executable(
        &server,
        &format!("#!/bin/sh\ntouch {:?}\n", marker.to_string_lossy()),
    );
    write_file(
        &plugin_root.join("plugin.toml"),
        r#"id = "fixture"
name = "Fixture"
version = "1.0.0"

[[mcp_servers]]
transport = "stdio"
name = "server"
command = "./bin/server"
startup = "eager"
"#,
    );

    let pending =
        crate::discover_workspace_plugins(workspace.path(), &[]).expect("plugin should discover");
    let trusted_entry =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trusted entry should build");
    let disabled_entry =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Disabled, 43)
            .expect("disabled entry should build");
    let trusted =
        crate::discover_workspace_plugins(workspace.path(), std::slice::from_ref(&trusted_entry))
            .expect("trusted plugin should discover");
    let declarations =
        crate::merge_mcp_server_declarations(&[], &trusted.registrations.mcp_servers)
            .expect("declaration should merge");
    let root_config: RootConfig = toml::from_str(
        r#"config_version = 2

[agent]
connection = "local"
model = "test"

[connections.local]
label = "Local"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }
"#,
    )
    .expect("root config should parse");
    let (store, trust_path) = session_trust_source(workspace.path(), trusted_entry);
    let launcher = declaration_mcp_process_launcher(
        &root_config,
        &declarations,
        Some(Arc::new(SessionMcpPluginTrustSource::new(trust_path))),
        None,
    )
    .expect("launcher should build");
    let request = launcher
        .resolve_launch_request(declarations[0].config(), None)
        .expect("trusted declaration should resolve");

    store
        .append(&SessionLogEntry::Control(
            ControlEntry::PluginTrustDecision(disabled_entry),
        ))
        .expect("disabled trust should append before spawn");

    let error = match launcher.launch(request) {
        Ok(_) => panic!("disabled current trust must reject spawn"),
        Err(error) => error,
    };
    let typed = error
        .downcast_ref::<McpRegistrationError>()
        .expect("trust downgrade should preserve typed declaration error");
    assert_eq!(typed.code(), "plugin_mcp_attestation_review_required");
    assert!(
        !marker.exists(),
        "current trust downgrade must remain zero-spawn"
    );
}

#[cfg(unix)]
#[test]
fn declaration_launcher_rejects_replaced_workspace_execution_base_identity() {
    let fixture = tempfile::tempdir().expect("fixture root should create");
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace should create");
    let declaration = ResolvedMcpServerDeclaration::user_root(
        mcp_server_config! {
            name: "root-server".to_owned(),
            command: "./bin/server".to_owned(),
            ..sigil_kernel::McpServerConfig::default()
        },
        &workspace,
    )
    .expect("root declaration should capture the canonical workspace base");
    let root_config: RootConfig = toml::from_str(
        r#"config_version = 2

[agent]
connection = "local"
model = "test"

[connections.local]
label = "Local"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }
"#,
    )
    .expect("root config should parse");
    let launcher = declaration_mcp_process_launcher(
        &root_config,
        std::slice::from_ref(&declaration),
        None,
        None,
    )
    .expect("root launcher should build");

    fs::rename(&workspace, fixture.path().join("workspace-original"))
        .expect("captured workspace should move");
    let outside = fixture.path().join("outside");
    let marker = fixture.path().join("spawned-marker");
    write_executable(
        &outside.join("bin/server"),
        &format!("#!/bin/sh\ntouch {:?}\n", marker.to_string_lossy()),
    );
    symlink(&outside, &workspace).expect("replacement workspace symlink should create");

    let error = launcher
        .resolve_launch_request(declaration.config(), None)
        .expect_err("fresh canonical base must equal the captured base identity");
    let typed = error
        .downcast_ref::<McpRegistrationError>()
        .expect("execution-base drift should stay typed");
    assert_eq!(typed.code(), "mcp_execution_base_unavailable");
    assert!(typed.safe_projection.is_some());
    assert!(
        !marker.exists(),
        "execution-base drift must remain zero-spawn"
    );
}
