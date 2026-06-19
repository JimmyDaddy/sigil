use std::{fs, path::Path};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentConfig, ApprovalMode, CodeIntelligenceConfig, CompactionConfig, McpServerStartup,
    MemoryConfig, PermissionConfig, PluginCapability, PluginSkillRef, PluginTrustDecision,
    PluginTrustEntry, ProviderCapabilities, ReasoningStreamSupport, RootConfig, SessionConfig,
    SkillConfig, SkillIndexSnapshot, SkillSource, TaskConfig, WorkspaceConfig,
};

use super::{
    PluginDiscoveryWarningKind, discover_workspace_plugins, merge_plugin_mcp_servers,
    merge_plugin_skill_descriptors,
};
use crate::build_tool_registry_without_eager_mcp;
use crate::skills::discover_plugin_skill_descriptors;

#[test]
fn missing_plugin_directory_returns_empty_report() {
    let workspace = tempfile::tempdir().expect("workspace should create");

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.is_empty());
}

#[test]
fn plugin_discovery_path_file_reports_invalid_path() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    fs::create_dir_all(workspace.path().join(".sigil")).expect("sigil dir should create");
    fs::write(workspace.path().join(".sigil/plugins"), "not a directory")
        .expect("plugins file should write");

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(
        report.warnings[0].kind,
        PluginDiscoveryWarningKind::InvalidPath
    );
    assert!(report.warnings[0].message.contains("not a directory"));
}

#[test]
fn untrusted_plugin_manifest_is_captured_without_runtime_registrations() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/review/SKILL.md",
        r#"---
id: review
description: Review repositories.
trust: trusted
---

# Review
"#,
    );
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"
description = "Reusable review pack."

[[skills]]
path = "skills/review/SKILL.md"

[[hooks]]
event = "pre_tool_use"
command = "scripts/check-tool-policy.sh"
approval = "ask"

[[mcp_servers]]
name = "repo-tools"
command = "node"
args = ["server.js"]
startup = "lazy"
required = false
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.warnings.is_empty());
    assert_eq!(report.manifests.len(), 1);
    let manifest = &report.manifests[0];
    assert_eq!(manifest.plugin_id, "repo-review");
    assert_eq!(manifest.name, "Repository Review");
    assert_eq!(manifest.trust, PluginTrustDecision::NeedsReview);
    assert!(manifest.manifest_hash.len() >= 32);
    assert_eq!(
        manifest.capabilities,
        vec![
            PluginCapability::Skill {
                path: "skills/review/SKILL.md".into()
            },
            PluginCapability::Hook {
                event: "pre_tool_use".to_owned(),
                command: "scripts/check-tool-policy.sh".to_owned(),
                args: Vec::new(),
                approval: ApprovalMode::Ask,
            },
            PluginCapability::McpServer {
                name: "repo-tools".to_owned(),
                command: "node".to_owned(),
                args: vec!["server.js".to_owned()],
                startup: McpServerStartup::Lazy,
                required: false,
            },
        ]
    );
    assert!(report.registrations.is_empty());
}

#[test]
fn matching_trust_entry_emits_source_attributed_registrations() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/review/SKILL.md",
        r#"---
id: review
description: Review repositories.
allowed-tools: [read_file, grep]
trust: trusted
---

# Review
"#,
    );
    let manifest_path = write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[skills]]
path = "skills/review/SKILL.md"

[[hooks]]
event = "pre_tool_use"
command = "scripts/check-tool-policy.sh"
approval = "ask"

[[mcp_servers]]
name = "repo-tools"
command = "node"
args = ["server.js"]
startup = "lazy"
required = false
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust = PluginTrustEntry {
        plugin_id: "repo-review".to_owned(),
        manifest_path: pending.manifests[0].manifest_path.clone(),
        manifest_hash: pending.manifests[0].manifest_hash.clone(),
        decision: PluginTrustDecision::Trusted,
        reviewed_at_ms: 42,
    };

    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");

    assert!(manifest_path.is_file());
    assert!(report.warnings.is_empty());
    assert_eq!(report.manifests[0].trust, PluginTrustDecision::Trusted);
    assert_eq!(report.registrations.skills.len(), 1);
    let skill = &report.registrations.skills[0];
    assert_eq!(skill.id, "repo-review/review");
    assert_eq!(
        skill.source,
        SkillSource::Plugin {
            plugin_id: "repo-review".to_owned()
        }
    );
    assert_eq!(
        skill.entrypoint,
        Path::new(".sigil/plugins/repo-review/skills/review/SKILL.md")
    );
    assert!(skill.allowed_tools.names.contains("read_file"));
    assert_eq!(report.registrations.hooks.len(), 1);
    assert_eq!(report.registrations.hooks[0].plugin_id, "repo-review");
    assert_eq!(
        report.registrations.hooks[0].hook.approval,
        ApprovalMode::Ask
    );
    assert_eq!(report.registrations.mcp_servers.len(), 1);
    let mcp = &report.registrations.mcp_servers[0];
    assert_eq!(mcp.plugin_id, "repo-review");
    assert_eq!(mcp.original_name, "repo-tools");
    assert_eq!(mcp.server.name, "repo-review.repo-tools");
    assert_eq!(mcp.server.startup, McpServerStartup::Lazy);
    assert_eq!(
        report.registrations.mcp_server_configs()[0].name,
        "repo-review.repo-tools"
    );

    let merged = merge_plugin_skill_descriptors(
        &SkillIndexSnapshot::new(Vec::new()).expect("empty index should build"),
        &report.registrations.skills,
    )
    .expect("plugin skills should merge");
    assert_eq!(merged.descriptors[0].id, "repo-review/review");
}

#[test]
fn changed_manifest_hash_invalidates_trust_and_suppresses_registrations() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/review/SKILL.md",
        "# Review",
    );
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[skills]]
path = "skills/review/SKILL.md"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let stale_trust = PluginTrustEntry {
        plugin_id: "repo-review".to_owned(),
        manifest_path: pending.manifests[0].manifest_path.clone(),
        manifest_hash: pending.manifests[0].manifest_hash.clone(),
        decision: PluginTrustDecision::Trusted,
        reviewed_at_ms: 42,
    };
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.2.0"

[[skills]]
path = "skills/review/SKILL.md"
"#,
    );

    let changed = discover_workspace_plugins(workspace.path(), &[stale_trust])
        .expect("changed manifest discovery should succeed");

    assert_eq!(changed.manifests[0].version, "0.2.0");
    assert_eq!(changed.manifests[0].trust, PluginTrustDecision::NeedsReview);
    assert!(changed.registrations.is_empty());
}

#[test]
fn invalid_plugin_paths_are_rejected_without_registering_plugin() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    fs::create_dir_all(workspace.path().join(".sigil/plugins")).expect("plugins dir should create");
    fs::write(workspace.path().join(".sigil/plugins/not-a-dir"), "")
        .expect("non directory entry should write");
    fs::write(workspace.path().join(".sigil/plugins-file"), "").expect("extra file should write");
    fs::create_dir_all(workspace.path().join(".sigil/plugins/missing"))
        .expect("missing manifest plugin should create");
    write_plugin_manifest(
        workspace.path(),
        "escape",
        r#"id = "escape"
name = "Escape"
version = "0.1.0"

[[skills]]
path = "../outside/SKILL.md"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidPath
            && warning.message.contains("missing plugin.toml")
    }));
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidManifest
            && warning.message.contains("cannot escape plugin root")
    }));
}

#[cfg(unix)]
#[test]
fn plugin_root_or_manifest_symlink_escape_is_rejected() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let outside = tempfile::tempdir().expect("outside should create");
    write_file(
        &outside.path().join("escaped-root/plugin.toml"),
        r#"id = "escaped-root"
name = "Escaped Root"
version = "0.1.0"
"#,
    );
    let plugins_dir = workspace.path().join(".sigil/plugins");
    fs::create_dir_all(&plugins_dir).expect("plugins dir should create");
    symlink(
        outside.path().join("escaped-root"),
        plugins_dir.join("escaped-root"),
    )
    .expect("plugin root symlink should create");

    fs::create_dir_all(plugins_dir.join("manifest-link"))
        .expect("manifest link plugin dir should create");
    write_file(
        &outside.path().join("plugin.toml"),
        r#"id = "manifest-link"
name = "Manifest Link"
version = "0.1.0"
"#,
    );
    symlink(
        outside.path().join("plugin.toml"),
        plugins_dir.join("manifest-link/plugin.toml"),
    )
    .expect("manifest symlink should create");

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| {
                warning.kind == PluginDiscoveryWarningKind::InvalidPath
                    && warning.message.contains("escapes")
            })
            .count(),
        2
    );
}

#[test]
fn invalid_manifest_encoding_and_missing_skill_are_reported() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let utf8_path = workspace
        .path()
        .join(".sigil/plugins/invalid-utf8/plugin.toml");
    fs::create_dir_all(utf8_path.parent().expect("utf8 path should have parent"))
        .expect("invalid utf8 parent should create");
    fs::write(&utf8_path, [0xff, 0xfe, 0xfd]).expect("invalid utf8 manifest should write");
    write_plugin_manifest(
        workspace.path(),
        "missing-skill",
        r#"id = "missing-skill"
name = "Missing Skill"
version = "0.1.0"

[[skills]]
path = "skills/missing/SKILL.md"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::ReadFailed
            && warning.message.contains("not utf-8")
    }));
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidPath
            && warning
                .message
                .contains("failed to resolve plugin missing-skill skill")
    }));
}

#[test]
fn trusted_plugin_registration_errors_leave_manifest_unregistered() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/first/SKILL.md",
        r#"---
id: review
---

# Review
"#,
    );
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/second/SKILL.md",
        r#"---
id: review
---

# Review
"#,
    );
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[skills]]
path = "skills/first/SKILL.md"

[[skills]]
path = "skills/second/SKILL.md"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust = PluginTrustEntry {
        plugin_id: "repo-review".to_owned(),
        manifest_path: pending.manifests[0].manifest_path.clone(),
        manifest_hash: pending.manifests[0].manifest_hash.clone(),
        decision: PluginTrustDecision::Trusted,
        reviewed_at_ms: 42,
    };

    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidManifest
            && warning.message.contains("duplicate skill")
    }));
}

#[test]
fn malformed_manifest_and_id_mismatch_are_reported_as_invalid_manifest() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "broken",
        r#"id = "broken"
name = "Broken"
version = "0.1.0"
[[skills]]
path =
"#,
    );
    write_plugin_manifest(
        workspace.path(),
        "actual-id",
        r#"id = "other-id"
name = "Mismatch"
version = "0.1.0"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert_eq!(report.warnings.len(), 2);
    assert!(report.warnings.iter().all(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidManifest
            && (warning.message.contains("invalid plugin manifest")
                || warning.message.contains("does not match directory"))
    }));
}

#[cfg(unix)]
#[test]
fn symlinked_skill_entrypoint_escape_is_rejected_as_invalid_path() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let outside = tempfile::tempdir().expect("outside should create");
    fs::create_dir_all(outside.path().join("skill")).expect("outside skill dir should create");
    fs::write(outside.path().join("skill/SKILL.md"), "# Outside")
        .expect("outside skill should write");
    let link_parent = workspace.path().join(".sigil/plugins/repo-review/skills");
    fs::create_dir_all(&link_parent).expect("link parent should create");
    symlink(outside.path().join("skill"), link_parent.join("review"))
        .expect("symlink should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[skills]]
path = "skills/review/SKILL.md"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidPath
            && warning.message.contains("escapes plugin root")
    }));
}

#[test]
fn plugin_mcp_servers_remain_lifecycle_inputs_until_existing_registry_activation() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[mcp_servers]]
name = "repo-tools"
command = "/definitely/missing/plugin-mcp-server"
startup = "lazy"
required = false
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust = PluginTrustEntry {
        plugin_id: "repo-review".to_owned(),
        manifest_path: pending.manifests[0].manifest_path.clone(),
        manifest_hash: pending.manifests[0].manifest_hash.clone(),
        decision: PluginTrustDecision::Trusted,
        reviewed_at_ms: 42,
    };
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");

    let merged_mcp = merge_plugin_mcp_servers(&[], &report.registrations.mcp_servers);
    assert_eq!(merged_mcp[0].name, "repo-review.repo-tools");
    let conflicting_base = vec![sigil_kernel::McpServerConfig {
        name: "repo-review.repo-tools".to_owned(),
        command: "existing".to_owned(),
        ..sigil_kernel::McpServerConfig::default()
    }];
    let identity = "repo-review\0repo-tools";
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let deeply_conflicting_base = vec![
        conflicting_base[0].clone(),
        sigil_kernel::McpServerConfig {
            name: format!("repo-review.repo-tools.{}", &hash[..8]),
            command: "existing-hash".to_owned(),
            ..sigil_kernel::McpServerConfig::default()
        },
    ];
    let conflict_merged =
        merge_plugin_mcp_servers(&conflicting_base, &report.registrations.mcp_servers);
    assert_eq!(conflict_merged[0].name, "repo-review.repo-tools");
    assert!(
        conflict_merged[1]
            .name
            .starts_with("repo-review.repo-tools.")
    );
    assert_ne!(conflict_merged[0].name, conflict_merged[1].name);
    let deep_conflict_merged =
        merge_plugin_mcp_servers(&deeply_conflicting_base, &report.registrations.mcp_servers);
    assert!(deep_conflict_merged[2].name.ends_with(".1"));
    let mut config = root_config();
    config.mcp_servers = merged_mcp;
    let registry = build_tool_registry_without_eager_mcp(
        &config,
        &provider_capabilities(),
        workspace.path().to_path_buf(),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    );

    assert!(registry.spec_for("mcp_activate_server").is_some());
    assert!(
        registry
            .spec_for("mcp__repo_review_repo_tools__echo")
            .is_none()
    );
}

#[test]
fn plugin_skill_merge_rejects_duplicate_ids() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/review/SKILL.md",
        r#"---
id: review
---

# Review
"#,
    );
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[skills]]
path = "skills/review/SKILL.md"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust = PluginTrustEntry {
        plugin_id: "repo-review".to_owned(),
        manifest_path: pending.manifests[0].manifest_path.clone(),
        manifest_hash: pending.manifests[0].manifest_hash.clone(),
        decision: PluginTrustDecision::Trusted,
        reviewed_at_ms: 42,
    };
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let existing = SkillIndexSnapshot::new(report.registrations.skills.clone())
        .expect("existing index should build");

    let error = merge_plugin_skill_descriptors(&existing, &report.registrations.skills)
        .expect_err("duplicate plugin skill should fail");

    assert!(error.to_string().contains("conflicts with existing skill"));
}

#[cfg(unix)]
#[test]
fn direct_plugin_skill_descriptor_helper_rejects_filesystem_edges() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let outside = tempfile::tempdir().expect("outside should create");
    let plugin_root = workspace.path().join(".sigil/plugins/repo-review");
    write_file(
        &plugin_root.join("skills/review/SKILL.md"),
        r#"---
id: review
---

# Review
"#,
    );
    let outside_plugin_root = outside.path().join("repo-review");
    fs::create_dir_all(&outside_plugin_root).expect("outside plugin root should create");
    let root_error = discover_plugin_skill_descriptors(
        workspace.path(),
        "repo-review",
        &outside_plugin_root,
        &[],
    )
    .expect_err("outside plugin root should fail");
    assert!(root_error.to_string().contains("root escapes workspace"));

    let missing_error = discover_plugin_skill_descriptors(
        workspace.path(),
        "repo-review",
        &plugin_root,
        &[PluginSkillRef {
            path: "skills/missing/SKILL.md".into(),
        }],
    )
    .expect_err("missing plugin skill should fail");
    assert!(
        missing_error
            .to_string()
            .contains("failed to resolve plugin repo-review skill")
    );

    fs::create_dir_all(outside.path().join("skill")).expect("outside skill dir should create");
    fs::write(outside.path().join("skill/SKILL.md"), "# Outside")
        .expect("outside skill should write");
    symlink(
        outside.path().join("skill"),
        plugin_root.join("skills/escape"),
    )
    .expect("skill symlink should create");
    let escape_error = discover_plugin_skill_descriptors(
        workspace.path(),
        "repo-review",
        &plugin_root,
        &[PluginSkillRef {
            path: "skills/escape/SKILL.md".into(),
        }],
    )
    .expect_err("escaped plugin skill should fail");
    assert!(escape_error.to_string().contains("escapes plugin root"));
}

#[test]
fn direct_plugin_skill_descriptor_helper_rejects_id_edges() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/repo-review");
    write_file(&plugin_root.join("bad name/SKILL.md"), "# Bad Directory");
    write_file(&plugin_root.join("skills/bad!.md"), "# Bad File");
    write_file(
        &plugin_root.join("skills/one/SKILL.md"),
        r#"---
id: same
---

# One
"#,
    );
    write_file(
        &plugin_root.join("skills/two/SKILL.md"),
        r#"---
id: same
---

# Two
"#,
    );

    let bad_dir = discover_plugin_skill_descriptors(
        workspace.path(),
        "repo-review",
        &plugin_root,
        &[PluginSkillRef {
            path: "bad name/SKILL.md".into(),
        }],
    )
    .expect_err("invalid directory fallback should fail");
    assert!(
        bad_dir
            .to_string()
            .contains("invalid plugin skill directory")
    );

    let bad_file = discover_plugin_skill_descriptors(
        workspace.path(),
        "repo-review",
        &plugin_root,
        &[PluginSkillRef {
            path: "skills/bad!.md".into(),
        }],
    )
    .expect_err("invalid file fallback should fail");
    assert!(bad_file.to_string().contains("invalid plugin skill file"));

    let duplicate = discover_plugin_skill_descriptors(
        workspace.path(),
        "repo-review",
        &plugin_root,
        &[
            PluginSkillRef {
                path: "skills/one/SKILL.md".into(),
            },
            PluginSkillRef {
                path: "skills/two/SKILL.md".into(),
            },
        ],
    )
    .expect_err("duplicate fallback ids should fail");
    assert!(duplicate.to_string().contains("duplicate skill"));
}

fn write_plugin_manifest(workspace: &Path, plugin_id: &str, manifest: &str) -> std::path::PathBuf {
    let path = workspace
        .join(".sigil/plugins")
        .join(plugin_id)
        .join("plugin.toml");
    write_file(&path, manifest);
    path
}

fn write_plugin_skill(workspace: &Path, plugin_id: &str, relative_path: &str, body: &str) {
    write_file(
        &workspace
            .join(".sigil/plugins")
            .join(plugin_id)
            .join(relative_path),
        body,
    );
}

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("path should have parent"))
        .expect("parent should create");
    fs::write(path, content).expect("file should write");
}

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig::default(),
        skills: SkillConfig::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        task: TaskConfig::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
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
    }
}
