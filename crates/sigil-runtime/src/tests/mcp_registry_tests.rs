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
        r#"[agent]
provider = "deepseek"
model = "test"
"#,
    )
    .expect("root config should parse");
    let (_, trust_path) = session_trust_source(workspace.path(), trust);
    let launcher = declaration_mcp_process_launcher(
        &root_config,
        &declarations,
        Some(Arc::new(SessionMcpPluginTrustSource::new(trust_path))),
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
        r#"[agent]
provider = "deepseek"
model = "test"
"#,
    )
    .expect("root config should parse");
    let (store, trust_path) = session_trust_source(workspace.path(), trusted_entry);
    let launcher = declaration_mcp_process_launcher(
        &root_config,
        &declarations,
        Some(Arc::new(SessionMcpPluginTrustSource::new(trust_path))),
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
        r#"[agent]
provider = "deepseek"
model = "test"
"#,
    )
    .expect("root config should parse");
    let launcher =
        declaration_mcp_process_launcher(&root_config, std::slice::from_ref(&declaration), None)
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
