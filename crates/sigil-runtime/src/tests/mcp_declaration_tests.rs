use std::{fs, path::Path};

use sha2::{Digest, Sha256};
use sigil_kernel::{
    DurableEventType, ExtensionProcessLaunchPhase, ExtensionProcessLifecycleAudit,
    ExtensionProcessLifecycleStatus, JsonlSessionStore, McpServerConfig, McpServerStartup,
    MutationEventRecorder, PluginManifest, PluginManifestSnapshot, PluginTrustDecision,
    PluginTrustEntry, ProviderCapabilities, ReasoningStreamSupport, RootConfig, SessionLogEntry,
    ToolCall, ToolContext, ToolRegistry,
};
#[cfg(unix)]
use std::os::unix::fs::symlink;

use super::*;

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("fixture parent should create");
    }
    fs::write(path, content).expect("fixture file should write");
}

fn write_executable(path: &Path, content: &str) {
    write_file(path, content);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .expect("fixture metadata should read")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("fixture should become executable");
    }
}

fn fixture_executable_body() -> &'static str {
    if cfg!(windows) {
        "@exit /B 0\r\n"
    } else {
        "#!/bin/sh\nexit 0\n"
    }
}

fn relative_fixture_command() -> &'static str {
    if cfg!(windows) {
        "./bin/server.cmd"
    } else {
        "./bin/server"
    }
}

fn relative_fixture_path(root: &Path) -> std::path::PathBuf {
    root.join(if cfg!(windows) {
        "bin/server.cmd"
    } else {
        "bin/server"
    })
}

fn manifest_text(command: &str, args: &[&str], version: &str) -> String {
    let args = args
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"id = "fixture"
name = "Fixture"
version = "{version}"

[[mcp_servers]]
name = "server"
transport = "stdio"
command = {command:?}
args = [{args}]
startup = "lazy"
"#
    )
}

fn plugin_declaration_fixture(
    command: &str,
    args: &[&str],
) -> (
    tempfile::TempDir,
    ResolvedMcpServerDeclaration,
    PluginTrustEntry,
) {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let manifest_path = plugin_root.join("plugin.toml");
    let raw = manifest_text(command, args, "1.0.0");
    write_file(&manifest_path, &raw);
    let bytes = fs::read(&manifest_path).expect("manifest should read");
    let manifest_hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    let mut manifest = toml::from_str::<PluginManifest>(&raw).expect("manifest should parse");
    manifest.root = plugin_root.clone();
    manifest.validate().expect("manifest should validate");
    let trust_manifest_path = Path::new(".sigil/plugins/fixture/plugin.toml").to_path_buf();
    let snapshot = PluginManifestSnapshot {
        plugin_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        manifest_path: trust_manifest_path.clone(),
        manifest_hash: manifest_hash.clone(),
        capabilities: manifest.capabilities(),
        trust: PluginTrustDecision::NeedsReview,
    };
    let trust = PluginTrustEntry::for_snapshot(&snapshot, PluginTrustDecision::Trusted, 42)
        .expect("trust should build");
    let capability_digest = snapshot
        .capability_digest()
        .expect("capability digest should build");
    let attestation = PluginManifestAttestation::capture(
        "server",
        plugin_root.clone(),
        manifest_path,
        trust_manifest_path,
        manifest_hash.clone(),
        manifest.version.clone(),
        capability_digest.clone(),
        PluginTrustDecision::Trusted,
    )
    .expect("attestation should capture");
    let mut config = manifest.mcp_servers[0].clone();
    config.name = "fixture.server".to_owned();
    let declaration = ResolvedMcpServerDeclaration::plugin_manifest(
        "server".to_owned(),
        config,
        McpConfigOrigin::PluginManifest {
            plugin_id: "fixture".to_owned(),
            manifest_hash,
            manifest_version: manifest.version,
            capability_digest,
            trust: PluginTrustDecision::Trusted,
        },
        McpExecutionBase::PluginRoot(plugin_root),
        attestation,
    )
    .expect("declaration should build");
    (workspace, declaration, trust)
}

fn plugin_declaration_with_expected_facts(
    raw: &str,
    expected_version: &str,
    expected_capability_digest: Option<&str>,
) -> (
    tempfile::TempDir,
    ResolvedMcpServerDeclaration,
    PluginTrustEntry,
) {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let manifest_path = plugin_root.join("plugin.toml");
    write_file(&manifest_path, raw);
    let bytes = fs::read(&manifest_path).expect("manifest should read");
    let manifest_hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    let mut manifest = toml::from_str::<PluginManifest>(raw).expect("manifest should parse");
    manifest.root = plugin_root.clone();
    manifest.validate().expect("manifest should validate");
    let trust_manifest_path = Path::new(".sigil/plugins/fixture/plugin.toml").to_path_buf();
    let observed_snapshot = PluginManifestSnapshot {
        plugin_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        manifest_path: trust_manifest_path.clone(),
        manifest_hash: manifest_hash.clone(),
        capabilities: manifest.capabilities(),
        trust: PluginTrustDecision::NeedsReview,
    };
    let trust =
        PluginTrustEntry::for_snapshot(&observed_snapshot, PluginTrustDecision::Trusted, 42)
            .expect("current trust should build");
    let observed_capability_digest = observed_snapshot
        .capability_digest()
        .expect("observed capability digest should build");
    let expected_capability_digest = expected_capability_digest
        .unwrap_or(&observed_capability_digest)
        .to_owned();
    let attestation = PluginManifestAttestation::capture(
        "server",
        plugin_root.clone(),
        manifest_path,
        trust_manifest_path,
        manifest_hash.clone(),
        expected_version.to_owned(),
        expected_capability_digest.clone(),
        PluginTrustDecision::Trusted,
    )
    .expect("attestation should capture");
    let mut config = manifest.mcp_servers[0].clone();
    config.name = "fixture.server".to_owned();
    let declaration = ResolvedMcpServerDeclaration::plugin_manifest(
        "server".to_owned(),
        config,
        McpConfigOrigin::PluginManifest {
            plugin_id: manifest.id,
            manifest_hash,
            manifest_version: expected_version.to_owned(),
            capability_digest: expected_capability_digest,
            trust: PluginTrustDecision::Trusted,
        },
        McpExecutionBase::PluginRoot(plugin_root),
        attestation,
    )
    .expect("declaration should build");
    (workspace, declaration, trust)
}

#[test]
fn mcp_declaration_promotes_legacy_root_without_losing_declared_name() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let declarations = resolve_user_root_mcp_declarations(
        &[mcp_server_config! {
            name: "root-server".to_owned(),
            command: "fixture-command".to_owned(),
            ..McpServerConfig::default()
        }],
        workspace.path(),
    )
    .expect("root declaration should resolve");
    let declaration = &declarations[0];

    assert_eq!(declaration.declared_name(), "root-server");
    assert_eq!(declaration.effective_name(), "root-server");
    assert!(matches!(declaration.origin(), McpConfigOrigin::UserRoot));
    assert!(matches!(
        declaration.execution_base(),
        McpExecutionBase::WorkspaceRoot(path) if path == &workspace.path().canonicalize().expect("workspace should canonicalize")
    ));
    assert!(declaration.plugin_attestation().is_none());
}

#[test]
fn mcp_declaration_rejects_root_reserved_namespace_and_duplicate_names() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let reserved = resolve_user_root_mcp_declarations(
        &[mcp_server_config! {
            name: "builtin:exa-anonymous".to_owned(),
            command: "fixture-command".to_owned(),
            ..McpServerConfig::default()
        }],
        workspace.path(),
    )
    .expect_err("root cannot claim builtin namespace");
    assert_eq!(reserved.code(), "reserved_mcp_namespace");

    let duplicate = resolve_user_root_mcp_declarations(
        &[
            mcp_server_config! {
                name: "same".to_owned(),
                command: "fixture-command".to_owned(),
                ..McpServerConfig::default()
            },
            mcp_server_config! {
                name: "same".to_owned(),
                command: "fixture-command".to_owned(),
                ..McpServerConfig::default()
            },
        ],
        workspace.path(),
    )
    .expect_err("duplicate names should fail before registry");
    assert_eq!(duplicate.code(), "duplicate_mcp_server_name");
}

#[test]
fn mcp_declaration_constructor_enforces_plugin_attestation_one_to_one() {
    let (_workspace, plugin, _trust) = plugin_declaration_fixture("fixture-command", &[]);
    let missing = ResolvedMcpServerDeclaration::new(
        plugin.declared_name.clone(),
        plugin.config.clone(),
        plugin.origin.clone(),
        plugin.execution_base.clone(),
        None,
    )
    .expect_err("plugin origin without attestation must fail");
    assert_eq!(missing.code(), "plugin_origin_attestation_mismatch");

    let workspace = tempfile::tempdir().expect("workspace should create");
    let user = ResolvedMcpServerDeclaration::new(
        "root".to_owned(),
        mcp_server_config! {
            name: "root".to_owned(),
            command: "fixture-command".to_owned(),
            ..McpServerConfig::default()
        },
        McpConfigOrigin::UserRoot,
        McpExecutionBase::WorkspaceRoot(
            workspace
                .path()
                .canonicalize()
                .expect("workspace should canonicalize"),
        ),
        plugin.plugin_attestation.clone(),
    )
    .expect_err("user origin with plugin attestation must fail");
    assert_eq!(user.code(), "plugin_origin_attestation_mismatch");
}

#[test]
fn mcp_declaration_builtin_origin_does_not_imply_none_execution_base() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let executable = workspace.path().join(if cfg!(windows) {
        "local-fixture.cmd"
    } else {
        "local-fixture"
    });
    write_executable(&executable, fixture_executable_body());
    let local = ResolvedMcpServerDeclaration::builtin_release_profile(
        mcp_server_config! {
            name: "builtin:local-fixture".to_owned(),
            command: executable.to_string_lossy().into_owned(),
            ..McpServerConfig::default()
        },
        "builtin:local-fixture",
        format!("sha256:{:064x}", 11),
        McpExecutionBase::WorkspaceRoot(workspace.path().to_path_buf()),
    )
    .expect("private profile may carry a local base");
    assert_eq!(
        local
            .resolve_stdio_launch(&[])
            .expect("local built-in should resolve")
            .cwd,
        workspace.path().canonicalize().expect("cwd should resolve")
    );

    let none = ResolvedMcpServerDeclaration::builtin_release_profile(
        mcp_server_config! {
            name: "builtin:remote-fixture".to_owned(),
            command: "definitely-missing-command".to_owned(),
            ..McpServerConfig::default()
        },
        "builtin:remote-fixture",
        format!("sha256:{:064x}", 11),
        McpExecutionBase::None,
    )
    .expect("private remote profile should construct");
    let error = none
        .resolve_stdio_launch(&[])
        .expect_err("None base must reject stdio before command lookup");
    assert_eq!(error.code(), "mcp_execution_base_unavailable");
}

#[test]
fn mcp_declaration_stable_pin_is_path_safe_while_exact_authorization_binds_base() {
    let first_root = tempfile::tempdir().expect("first root should create");
    let second_root = tempfile::tempdir().expect("second root should create");
    let executable_name = if cfg!(windows) {
        "server.cmd"
    } else {
        "server"
    };
    let first_executable = first_root.path().join(executable_name);
    let second_executable = second_root.path().join(executable_name);
    write_executable(&first_executable, fixture_executable_body());
    write_executable(&second_executable, fixture_executable_body());
    let release_digest = format!("sha256:{:064x}", 13);
    let make_declaration = |root: &Path, executable: &Path| {
        ResolvedMcpServerDeclaration::builtin_release_profile(
            mcp_server_config! {
                name: "builtin:path-safe-pin".to_owned(),
                command: executable.to_string_lossy().into_owned(),
                args: vec!["relative-argument.txt".to_owned()],
                ..McpServerConfig::default()
            },
            "builtin:path-safe-pin",
            release_digest.clone(),
            McpExecutionBase::WorkspaceRoot(root.to_path_buf()),
        )
        .expect("private declaration should build")
    };
    let first = make_declaration(first_root.path(), &first_executable);
    let second = make_declaration(second_root.path(), &second_executable);
    let first_launch = first
        .resolve_stdio_launch(&[])
        .expect("first executable should resolve");
    let second_launch = second
        .resolve_stdio_launch(&[])
        .expect("second executable should resolve");
    assert_eq!(
        first.safe_projection().declaration_fingerprint,
        second.safe_projection().declaration_fingerprint
    );
    let first_pin = sigil_mcp::mcp_resolved_launch_static_fingerprint_at(
        &first.safe_projection().declaration_fingerprint,
        &first_launch.executable,
    )
    .expect("first safe pin should build");
    let second_pin = sigil_mcp::mcp_resolved_launch_static_fingerprint_at(
        &second.safe_projection().declaration_fingerprint,
        &second_launch.executable,
    )
    .expect("second safe pin should build");
    assert_eq!(first_pin, second_pin, "plain stable pin must exclude paths");
    let first_metadata =
        first.launch_metadata(&first_launch, &first_pin, "hmac-sha256:test-environment");
    let second_metadata =
        second.launch_metadata(&second_launch, &second_pin, "hmac-sha256:test-environment");
    assert_ne!(
        first_metadata.authorization_fingerprint, second_metadata.authorization_fingerprint,
        "runtime-keyed authorization must still bind canonical base and executable identity"
    );
}

#[test]
fn mcp_declaration_resolves_bare_node_with_plugin_cwd_and_preserves_relative_arg() {
    let (workspace, declaration, trust) = plugin_declaration_fixture("node", &["server.js"]);
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    write_file(&plugin_root.join("server.js"), "process.exit(0);\n");
    let controlled_bin = workspace.path().join("controlled-bin");
    let fake_node = controlled_bin.join(if cfg!(windows) { "node.cmd" } else { "node" });
    write_executable(&fake_node, fixture_executable_body());
    let controlled_path =
        std::env::join_paths([&controlled_bin]).expect("controlled PATH should build");

    declaration
        .verify_activation(&[trust])
        .expect("plugin declaration should re-attest");
    let cwd = match declaration.execution_base() {
        McpExecutionBase::PluginRoot(path) => path.clone(),
        other => panic!("expected plugin root, got {other:?}"),
    };
    let executable = resolve_bare_stdio_executable(
        declaration.declared_name(),
        "node",
        &cwd,
        &controlled_path,
        cfg!(windows).then_some(".CMD"),
    )
    .expect("bare node should resolve on injected controlled PATH");

    assert_eq!(
        executable,
        fake_node
            .canonicalize()
            .expect("fake node should canonicalize")
    );
    assert_eq!(
        cwd,
        plugin_root
            .canonicalize()
            .expect("plugin root should resolve")
    );
    assert_eq!(
        declaration
            .config()
            .stdio()
            .expect("plugin declaration should remain stdio")
            .1,
        ["server.js"]
    );
}

#[test]
fn mcp_declaration_resolves_relative_and_absolute_commands_without_interpreting_args() {
    let (workspace, relative, trust) =
        plugin_declaration_fixture(relative_fixture_command(), &["relative-input.txt"]);
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let relative_executable = relative_fixture_path(&plugin_root);
    write_executable(&relative_executable, fixture_executable_body());
    let relative_launch = relative
        .resolve_stdio_launch(&[trust])
        .expect("relative command should resolve under plugin root");
    assert_eq!(
        relative_launch.executable,
        relative_executable
            .canonicalize()
            .expect("relative executable should canonicalize")
    );
    assert_eq!(
        relative
            .config()
            .stdio()
            .expect("relative declaration should remain stdio")
            .1,
        ["relative-input.txt"]
    );

    let absolute_executable = workspace.path().join(if cfg!(windows) {
        "absolute-server.cmd"
    } else {
        "absolute-server"
    });
    write_executable(&absolute_executable, fixture_executable_body());
    let absolute_command = absolute_executable.to_string_lossy().into_owned();
    let (_absolute_workspace, absolute, absolute_trust) =
        plugin_declaration_fixture(&absolute_command, &["still-relative.txt"]);
    let absolute_launch = absolute
        .resolve_stdio_launch(&[absolute_trust])
        .expect("absolute command should retain existing trust/pin path behavior");
    assert_eq!(
        absolute_launch.executable,
        absolute_executable
            .canonicalize()
            .expect("absolute executable should canonicalize")
    );
    assert_eq!(
        absolute
            .config()
            .stdio()
            .expect("absolute declaration should remain stdio")
            .1,
        ["still-relative.txt"]
    );
}

#[cfg(unix)]
#[test]
fn mcp_declaration_rejects_relative_command_symlink_escape() {
    let (workspace, declaration, trust) = plugin_declaration_fixture("./bin/server", &[]);
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let outside = workspace.path().join("outside-server");
    write_executable(&outside, "#!/bin/sh\nexit 0\n");
    fs::create_dir_all(plugin_root.join("bin")).expect("bin should create");
    symlink(&outside, plugin_root.join("bin/server")).expect("symlink should create");

    let error = declaration
        .resolve_stdio_launch(&[trust])
        .expect_err("relative symlink escape must fail");

    assert_eq!(error.code(), "mcp_command_symlink_escape");
}

#[cfg(unix)]
#[test]
fn mcp_declaration_rejects_execution_base_identity_replacement() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let execution_base = workspace.path().join("workspace-root");
    let moved_original = workspace.path().join("workspace-root-moved");
    let replacement = workspace.path().join("replacement-root");
    write_executable(
        &execution_base.join("bin/server"),
        fixture_executable_body(),
    );
    write_executable(&replacement.join("bin/server"), fixture_executable_body());
    let declaration = ResolvedMcpServerDeclaration::user_root(
        mcp_server_config! {
            name: "workspace-server".to_owned(),
            command: "./bin/server".to_owned(),
            ..McpServerConfig::default()
        },
        &execution_base,
    )
    .expect("user-root declaration should resolve its initial base");

    fs::rename(&execution_base, &moved_original).expect("original base should move");
    symlink(&replacement, &execution_base).expect("replacement symlink should create");

    let error = declaration
        .resolve_stdio_launch(&[])
        .expect_err("replaced execution base identity must fail before command lookup or spawn");
    assert_eq!(error.code(), "mcp_execution_base_unavailable");
    assert!(error.reason.contains("identity changed"));
}

#[cfg(unix)]
#[test]
fn mcp_declaration_rejects_non_executable_command_before_spawn() {
    let (workspace, declaration, trust) = plugin_declaration_fixture("./bin/server", &[]);
    let command = workspace.path().join(".sigil/plugins/fixture/bin/server");
    write_file(&command, "not executable\n");

    let error = declaration
        .resolve_stdio_launch(&[trust])
        .expect_err("non-executable command should fail pre-spawn resolution");

    assert_eq!(error.code(), "mcp_command_resolution_failed");
    assert!(error.reason.contains("not executable"));
}

#[test]
fn mcp_declaration_rechecks_manifest_hash_and_current_trust_before_command_lookup() {
    let (workspace, declaration, trust) =
        plugin_declaration_fixture("definitely-missing-command", &[]);
    let manifest_path = workspace.path().join(".sigil/plugins/fixture/plugin.toml");
    fs::write(&manifest_path, manifest_text("/changed", &[], "1.0.0"))
        .expect("manifest should change");
    let error = declaration
        .resolve_stdio_launch(&[trust])
        .expect_err("manifest hash drift must stop before command lookup");
    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");

    let (_workspace, declaration, mut trust) =
        plugin_declaration_fixture("definitely-missing-command", &[]);
    trust.decision = PluginTrustDecision::Disabled;
    let error = declaration
        .resolve_stdio_launch(&[trust])
        .expect_err("trust drift must stop before command lookup");
    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");
}

#[test]
fn mcp_declaration_bounds_manifest_reread_before_hashing_or_parsing() {
    let (workspace, declaration, trust) =
        plugin_declaration_fixture("definitely-missing-command", &[]);
    let manifest_path = workspace.path().join(".sigil/plugins/fixture/plugin.toml");
    fs::write(&manifest_path, vec![b'x'; 1024 * 1024 + 1])
        .expect("oversized manifest should write");

    let error = declaration
        .verify_activation(&[trust])
        .expect_err("oversized current manifest must fail before unbounded processing");

    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");
    assert!(error.reason.contains("size limit"));
}

#[test]
fn mcp_declaration_rejects_non_regular_manifest_without_reading_it() {
    let (workspace, declaration, trust) =
        plugin_declaration_fixture("definitely-missing-command", &[]);
    let manifest_path = workspace.path().join(".sigil/plugins/fixture/plugin.toml");
    fs::remove_file(&manifest_path).expect("manifest file should remove");
    fs::create_dir(&manifest_path).expect("non-regular manifest fixture should create");

    let error = declaration
        .verify_activation(&[trust])
        .expect_err("non-regular manifest must fail before any stream read");

    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");
    assert!(error.reason.contains("cannot be read"));
}

#[test]
fn mcp_declaration_rechecks_manifest_version_and_capability_digest() {
    let observed_version = manifest_text("fixture-command", &[], "2.0.0");
    let (_workspace, declaration, trust) =
        plugin_declaration_with_expected_facts(&observed_version, "1.0.0", None);
    let error = declaration
        .verify_activation(&[trust])
        .expect_err("version drift should require review after hash/trust match");
    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");
    assert!(error.reason.contains("version"));

    let observed_capability = manifest_text("fixture-command", &["new-capability-arg"], "1.0.0");
    let stale_capability_digest = format!("sha256:{:064x}", 7);
    let (_workspace, declaration, trust) = plugin_declaration_with_expected_facts(
        &observed_capability,
        "1.0.0",
        Some(&stale_capability_digest),
    );
    let error = declaration
        .verify_activation(&[trust])
        .expect_err("capability drift should require review after hash/version/trust match");
    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");
    assert!(error.reason.contains("capabilities"));
}

#[test]
fn mcp_declaration_safe_projection_excludes_paths_commands_args_and_plain_digests() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let command_secret = "low-entropy-command-secret";
    let arg_secret = "low-entropy-arg-secret";
    let declaration = ResolvedMcpServerDeclaration::user_root(
        mcp_server_config! {
            name: "safe".to_owned(),
            command: command_secret.to_owned(),
            args: vec![arg_secret.to_owned()],
            startup: McpServerStartup::Lazy,
            ..McpServerConfig::default()
        },
        workspace.path(),
    )
    .expect("declaration should build");
    let projection_json =
        serde_json::to_string(&declaration.safe_projection()).expect("projection should serialize");
    let path = workspace
        .path()
        .canonicalize()
        .expect("workspace should canonicalize")
        .to_string_lossy()
        .into_owned();
    let path_digest = format!("{:x}", Sha256::digest(path.as_bytes()));
    let command_digest = format!("{:x}", Sha256::digest(command_secret.as_bytes()));
    let arg_digest = format!("{:x}", Sha256::digest(arg_secret.as_bytes()));

    for forbidden in [
        path.as_str(),
        command_secret,
        arg_secret,
        path_digest.as_str(),
        command_digest.as_str(),
        arg_digest.as_str(),
    ] {
        assert!(
            !projection_json.contains(forbidden),
            "safe projection leaked {forbidden:?}: {projection_json}"
        );
    }
    assert!(projection_json.contains("workspace_root"));
    assert!(projection_json.contains("user_root"));

    let debug = format!("{declaration:?}");
    for forbidden in [path.as_str(), command_secret, arg_secret] {
        assert!(
            !debug.contains(forbidden),
            "declaration Debug leaked {forbidden:?}: {debug}"
        );
    }
    assert!(debug.contains("safe_projection"));
}

#[test]
fn mcp_declaration_safe_projection_sanitizes_untrusted_root_and_builtin_labels() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let malicious_root_name = "/Users/example\napi_key=sk-secret-value";
    let root = ResolvedMcpServerDeclaration::user_root(
        mcp_server_config! {
            name: malicious_root_name.to_owned(),
            command: "fixture-command".to_owned(),
            ..McpServerConfig::default()
        },
        workspace.path(),
    )
    .expect("runtime projection must safely carry an untrusted root label");
    let profile_secret = "C:\\private\\authorization-token";
    let release_secret = "secret-release-token";
    let builtin = ResolvedMcpServerDeclaration::builtin_release_profile(
        mcp_server_config! {
            name: "builtin:safe-fixture".to_owned(),
            command: "fixture-command".to_owned(),
            ..McpServerConfig::default()
        },
        profile_secret,
        release_secret,
        McpExecutionBase::None,
    )
    .expect("private profile should construct before safe projection");

    let root_json = serde_json::to_string(&root.safe_projection())
        .expect("root projection should serialize safely");
    let builtin_json = serde_json::to_string(&builtin.safe_projection())
        .expect("builtin projection should serialize safely");
    for forbidden in [
        malicious_root_name,
        "/Users/example",
        "sk-secret-value",
        profile_secret,
        "authorization-token",
        release_secret,
    ] {
        assert!(!root_json.contains(forbidden));
        assert!(!builtin_json.contains(forbidden));
    }
    assert_eq!(root.safe_projection().declared_name, "[redacted]");
    assert_eq!(
        builtin.safe_projection().origin_id.as_deref(),
        Some("[redacted]")
    );
    assert!(builtin.safe_projection().manifest_hash.is_none());
    assert_eq!(
        builtin.safe_projection().release_digest.as_deref(),
        Some("[redacted]")
    );
    let origin_debug = format!("{:?}", builtin.origin());
    assert!(!origin_debug.contains(profile_secret));
    assert!(!origin_debug.contains(release_secret));

    let unsafe_error = ResolvedMcpServerDeclaration::user_root(
        mcp_server_config! {
            name: "builtin:/private\nauthorization=debug-secret".to_owned(),
            command: "fixture-command".to_owned(),
            ..McpServerConfig::default()
        },
        workspace.path(),
    )
    .expect_err("reserved namespace should produce a typed error");
    let error_debug = format!("{unsafe_error:?}");
    let error_display = unsafe_error.to_string();
    for rendered in [error_debug, error_display] {
        assert!(!rendered.contains("/private"));
        assert!(!rendered.contains("debug-secret"));
        assert!(rendered.contains("[redacted]"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_declaration_registry_preserves_plugin_binding_in_lifecycle_audit() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    let server_path = plugin_root.join("bin/server");
    write_executable(
        &server_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fixture","version":"1.0.0"},"capabilities":{"resources":{},"prompts":{}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}}'
      ;;
  esac
done
"#,
    );
    let manifest_path = plugin_root.join("plugin.toml");
    write_file(
        &manifest_path,
        &manifest_text("./bin/server", &[], "1.0.0")
            .replace("startup = \"lazy\"", "startup = \"eager\""),
    );
    let pending =
        crate::discover_workspace_plugins(workspace.path(), &[]).expect("plugin should discover");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let trusted = crate::discover_workspace_plugins(workspace.path(), std::slice::from_ref(&trust))
        .expect("trusted plugin should discover");
    let declarations =
        crate::merge_mcp_server_declarations(&[], &trusted.registrations.mcp_servers)
            .expect("plugin declaration should merge");
    let expected_projection = declarations[0].safe_projection();
    let root_config: RootConfig = toml::from_str(
        r#"[agent]
provider = "deepseek"
model = "test"
"#,
    )
    .expect("root config should parse");
    let capabilities = ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        reasoning_stream: ReasoningStreamSupport::Unsupported,
        supports_reasoning_effort: false,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: false,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    };
    let mut registry = ToolRegistry::new();
    let trust_log_path = workspace.path().join("state/session.jsonl");
    sigil_kernel::JsonlSessionStore::new(&trust_log_path)
        .expect("trust session should create")
        .append(&sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::PluginTrustDecision(trust),
        ))
        .expect("trusted decision should append");
    let report = crate::register_mcp_server_declarations(
        &mut registry,
        &root_config,
        &capabilities,
        workspace.path().to_path_buf(),
        &declarations,
        crate::McpDeclarationRegistrationOptions::new(McpServerStartup::Eager)
            .with_plugin_trust_session_log(trust_log_path)
            .with_strict_registration(),
    )
    .await
    .expect("declaration registry should start plugin MCP");

    assert!(registry.spec_for("mcp__fixture_server__echo").is_some());
    let receipt = &report.process_launch_receipts[0];
    assert_eq!(
        receipt.classification,
        McpProcessClass::LocalStdioPluginDeclared
    );
    let declaration = receipt
        .declaration
        .as_ref()
        .expect("lifecycle receipt should retain safe declaration metadata");
    assert_eq!(declaration.declared_name, "server");
    assert_eq!(declaration.effective_name, "fixture.server");
    assert_eq!(declaration.origin_kind, "plugin_manifest");
    assert_eq!(declaration.origin_id, expected_projection.origin_id);
    assert_eq!(declaration.execution_base_kind, "plugin_root");
    assert_eq!(declaration.manifest_hash, expected_projection.manifest_hash);
    assert_eq!(
        declaration.manifest_version,
        expected_projection.manifest_version
    );
    assert_eq!(
        declaration.capability_digest,
        expected_projection.capability_digest
    );
    assert!(declaration.release_digest.is_none());
    assert_eq!(declaration.trust.as_deref(), Some("trusted"));
    assert!(
        declaration
            .authorization_fingerprint
            .starts_with("hmac-sha256:")
    );
    let audit = receipt.audit_metadata();
    assert_eq!(audit["mcp_declared_name"], "server");
    assert_eq!(audit["mcp_effective_name"], "fixture.server");
    assert_eq!(audit["mcp_config_origin"], "plugin_manifest");
    assert_eq!(audit["mcp_config_origin_id"], "fixture");
    assert_eq!(audit["mcp_execution_base_kind"], "plugin_root");
    assert_eq!(
        audit["mcp_manifest_hash"],
        expected_projection
            .manifest_hash
            .as_deref()
            .expect("plugin hash should project")
    );
    assert_eq!(audit["mcp_manifest_version"], "1.0.0");
    assert_eq!(
        audit["mcp_capability_digest"],
        expected_projection
            .capability_digest
            .as_deref()
            .expect("capability digest should project")
    );
    assert_eq!(audit["mcp_plugin_trust"], "trusted");
    let context = ToolContext::new(workspace.path().to_path_buf(), 5);
    for (tool_name, args_json) in [
        ("mcp__fixture_server__echo", "{}"),
        ("mcp__fixture_server__resources_list", "{}"),
        ("mcp__fixture_server__prompts_list", "{}"),
    ] {
        let call = ToolCall {
            id: format!("subject-{tool_name}"),
            name: tool_name.to_owned(),
            args_json: args_json.to_owned(),
        };
        let subjects = registry
            .permission_subjects(&context, &call)
            .expect("every MCP surface should expose permission subjects");
        assert!(
            subjects.iter().any(|subject| {
                subject
                    .original
                    .contains(&declaration.authorization_fingerprint)
            }),
            "{tool_name} must bind permission identity to the exact declaration process"
        );
        let egress = registry
            .egress_audit(&context, &call)
            .expect("egress audit should evaluate")
            .expect("plugin MCP surface should emit egress metadata");
        assert_eq!(
            egress.payload["server_identity"]["declaration"]["origin_kind"],
            "plugin_manifest"
        );
        assert_eq!(
            egress.payload["server_identity"]["declaration"]["execution_base_kind"],
            "plugin_root"
        );
        assert_eq!(
            egress.payload["server_identity"]["declaration"]["origin_id"],
            "fixture"
        );
        assert_eq!(
            egress.payload["server_identity"]["declaration"]["manifest_version"],
            "1.0.0"
        );
        assert_eq!(
            egress.payload["server_identity"]["declaration"]["trust"],
            "trusted"
        );
        assert_eq!(
            egress.payload["server_identity"]["declaration"]["authorization_fingerprint"],
            declaration.authorization_fingerprint
        );
    }
    let serialized = serde_json::to_string(receipt).expect("receipt should serialize");
    assert!(!serialized.contains(&plugin_root.to_string_lossy().into_owned()));

    let registered = report
        .lifecycle_owners
        .iter()
        .flat_map(|owner| registry.drain_by_lifecycle_owner(owner))
        .collect::<Vec<_>>();
    crate::mcp_registry::shutdown_registered_tools(&registered)
        .await
        .expect("fixture server should stop");
}

#[tokio::test]
async fn mcp_declaration_pre_spawn_rejection_keeps_safe_lifecycle_identity() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/fixture");
    write_file(
        &plugin_root.join("plugin.toml"),
        &manifest_text("definitely-missing-command", &[], "1.0.0")
            .replace("startup = \"lazy\"", "startup = \"eager\""),
    );
    let pending =
        crate::discover_workspace_plugins(workspace.path(), &[]).expect("plugin should discover");
    let trusted_entry =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trusted decision should build");
    let disabled_entry =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Disabled, 43)
            .expect("disabled decision should build");
    let trusted =
        crate::discover_workspace_plugins(workspace.path(), std::slice::from_ref(&trusted_entry))
            .expect("trusted plugin should discover");
    let declarations =
        crate::merge_mcp_server_declarations(&[], &trusted.registrations.mcp_servers)
            .expect("plugin declaration should merge");
    let expected_projection = declarations[0].safe_projection();
    let state = tempfile::tempdir().expect("state directory should create");
    let session_log_path = state.path().join("session.jsonl");
    let session_store = JsonlSessionStore::new(&session_log_path)
        .expect("session trust and lifecycle store should create");
    for trust in [trusted_entry, disabled_entry] {
        session_store
            .append(&SessionLogEntry::Control(
                sigil_kernel::ControlEntry::PluginTrustDecision(trust),
            ))
            .expect("trust decision should append");
    }
    let root_config: RootConfig = toml::from_str(
        r#"[agent]
provider = "deepseek"
model = "test"
"#,
    )
    .expect("root config should parse");
    let capabilities = ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        reasoning_stream: ReasoningStreamSupport::Unsupported,
        supports_reasoning_effort: false,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: false,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    };
    let mut registry = ToolRegistry::new();
    let error = crate::register_mcp_server_declarations(
        &mut registry,
        &root_config,
        &capabilities,
        workspace.path().to_path_buf(),
        &declarations,
        crate::McpDeclarationRegistrationOptions::new(McpServerStartup::Eager)
            .with_plugin_trust_session_log(session_log_path)
            .with_mutation_recorder(MutationEventRecorder::new(session_store.clone()))
            .with_strict_registration(),
    )
    .await
    .expect_err("current disabled trust must reject before command lookup or spawn");
    let typed = error
        .downcast_ref::<McpRegistrationError>()
        .expect("pre-spawn failure should retain typed declaration error");
    let safe_projection = typed
        .safe_projection
        .as_ref()
        .expect("typed rejection should carry only the safe declaration projection");
    assert_eq!(safe_projection.declared_name, "server");
    assert_eq!(safe_projection.effective_name, "fixture.server");

    let lifecycle = JsonlSessionStore::read_event_records(session_store.path())
        .expect("lifecycle events should read")
        .into_iter()
        .find_map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ExtensionProcessLifecycleRecorded.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .expect("zero-spawn rejection should still record lifecycle identity");
    let audit: ExtensionProcessLifecycleAudit =
        serde_json::from_value(lifecycle.payload).expect("lifecycle payload should decode");
    assert_eq!(audit.phase, ExtensionProcessLaunchPhase::PreSpawn);
    assert_eq!(audit.status, ExtensionProcessLifecycleStatus::StartupFailed);
    assert_eq!(audit.safe_metadata["mcp_declared_name"], "server");
    assert_eq!(audit.safe_metadata["mcp_effective_name"], "fixture.server");
    assert_eq!(audit.safe_metadata["mcp_config_origin"], "plugin_manifest");
    assert_eq!(audit.safe_metadata["mcp_config_origin_id"], "fixture");
    assert_eq!(
        audit.safe_metadata["mcp_execution_base_kind"],
        "plugin_root"
    );
    assert_eq!(
        audit.safe_metadata["mcp_manifest_hash"],
        expected_projection
            .manifest_hash
            .as_deref()
            .expect("plugin hash should project")
    );
    assert_eq!(audit.safe_metadata["mcp_manifest_version"], "1.0.0");
    assert_eq!(
        audit.safe_metadata["mcp_capability_digest"],
        expected_projection
            .capability_digest
            .as_deref()
            .expect("capability digest should project")
    );
    assert_eq!(audit.safe_metadata["mcp_plugin_trust"], "trusted");
    assert!(audit.safe_metadata["mcp_declaration_projection_fingerprint"].starts_with("sha256:"));
    let serialized = serde_json::to_string(&audit).expect("audit should serialize");
    assert!(!serialized.contains(&plugin_root.to_string_lossy().into_owned()));
    assert!(!serialized.contains("definitely-missing-command"));
}
