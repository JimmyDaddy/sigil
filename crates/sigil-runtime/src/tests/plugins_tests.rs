use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use anyhow::Result;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionCoverageLabel, ExecutionFuture, ExecutionNetworkReceipt, ExecutionReceipt,
    ExecutionRequest, ExecutionSandboxProfile, ExtensionProcessNetworkAdmission, JsonlSessionStore,
    MAX_PLUGIN_HOOK_ARTIFACT_REFS, McpServerStartup, MutationEventRecorder, NetworkEffect,
    NetworkPolicy, PluginCapability, PluginHookExecutionStatus, PluginHookKind,
    PluginHookOutputArtifactRef, PluginSkillRef, PluginTrustDecision, PluginTrustEntry,
    RedactionState, SecretRedactor, SkillIndexSnapshot, SkillSource, ToolAccess, ToolCategory,
    ToolEffect, WorkspaceMutationDetected,
};

use super::{
    MAX_PLUGIN_MANIFEST_BYTES, PluginDiscoveryWarningKind, PluginHookExecutionRequest,
    PluginHookExecutionRunner, discover_workspace_plugins, merge_mcp_server_declarations,
    merge_plugin_skill_descriptors,
};
use crate::skills::discover_plugin_skill_descriptors;
use crate::{McpConfigOrigin, McpExecutionBase, resolve_user_root_mcp_declarations};

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
fn plugin_discovery_uses_fixed_sigil_plugins_dir() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let plugin_root = workspace.path().join(".sigil/plugins/repo-review");
    write_file(
        &plugin_root.join("plugin.toml"),
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"
description = "Reusable review pack."

[[skills]]
path = "skills/review/SKILL.md"
"#,
    );
    write_file(
        &plugin_root.join("skills/review/SKILL.md"),
        r#"---
id: review
description: Review repositories.
trust: trusted
---

# Review
"#,
    );

    let pending =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");
    assert_eq!(pending.manifests.len(), 1);
    assert_eq!(pending.manifests[0].plugin_id, "repo-review");
    assert!(
        pending.manifests[0]
            .manifest_path
            .starts_with(".sigil/plugins")
    );
}

#[test]
fn plugin_discovery_accepts_manifest_at_limit_and_rejects_limit_plus_one() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let manifest_path = workspace.path().join(".sigil/plugins/bounded/plugin.toml");
    let base = r#"id = "bounded"
name = "Bounded"
version = "1.0.0"
"#;
    assert!(base.len() + 2 < MAX_PLUGIN_MANIFEST_BYTES);
    let mut at_limit = base.to_owned();
    at_limit.push_str("\n#");
    at_limit.push_str(&"x".repeat(MAX_PLUGIN_MANIFEST_BYTES - at_limit.len()));
    assert_eq!(at_limit.len(), MAX_PLUGIN_MANIFEST_BYTES);
    write_file(&manifest_path, &at_limit);

    let accepted = discover_workspace_plugins(workspace.path(), &[])
        .expect("manifest at the hard limit should discover");
    assert_eq!(accepted.manifests.len(), 1);
    assert!(accepted.warnings.is_empty());

    at_limit.push('x');
    write_file(&manifest_path, &at_limit);
    let rejected = discover_workspace_plugins(workspace.path(), &[])
        .expect("oversized manifest should become a bounded warning");
    assert!(rejected.manifests.is_empty());
    assert!(rejected.registrations.is_empty());
    assert_eq!(rejected.warnings.len(), 1);
    assert_eq!(
        rejected.warnings[0].kind,
        PluginDiscoveryWarningKind::ReadFailed
    );
    assert!(rejected.warnings[0].message.contains("1 MiB"));
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
    write_plugin_agent(
        workspace.path(),
        "repo-review",
        "agents/reviewer/agent.toml",
        r#"description = "Review agent."
instructions = "Review repository changes."
trust = "trusted"
"#,
    );
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
    let manifest_path = write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"
description = "Reusable review pack."

[[agents]]
path = "agents/reviewer/agent.toml"

[[skills]]
path = "skills/review/SKILL.md"

[[hooks]]
event = "pre_tool_use"
command = "scripts/check-tool-policy.sh"
approval = "ask"

[[mcp_servers]]
transport = "stdio"
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
    assert_eq!(
        manifest.manifest_hash,
        expected_manifest_digest(&manifest_path)
    );
    assert_eq!(
        manifest.capabilities,
        vec![
            PluginCapability::Agent {
                path: "agents/reviewer/agent.toml".into()
            },
            PluginCapability::Skill {
                path: "skills/review/SKILL.md".into()
            },
            PluginCapability::Hook {
                id: "pre_tool_use".to_owned(),
                event: "pre_tool_use".to_owned(),
                hook_kind: PluginHookKind::Event,
                command: "scripts/check-tool-policy.sh".to_owned(),
                args: Vec::new(),
                declared_effect: ToolEffect::Unknown,
                timeout_ms: 30_000,
                input_schema_digest: None,
                output_schema_digest: None,
                approval: ApprovalMode::Ask,
                egress_logging: true,
                allow_secrets: false,
            },
            PluginCapability::McpServer {
                name: "repo-tools".to_owned(),
                command: "node".to_owned(),
                args: vec!["server.js".to_owned()],
                startup: McpServerStartup::Lazy,
                required: false,
                approval: ApprovalMode::Ask,
                egress_logging: true,
                allow_secrets: false,
            },
        ]
    );
    assert!(report.registrations.is_empty());
}

#[test]
fn untrusted_plugin_does_not_load_skill_content_before_review() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/review/SKILL.md",
        r#"---
id review
---

# Invalid frontmatter that must not be parsed before trust
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

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.warnings.is_empty());
    assert_eq!(report.manifests.len(), 1);
    assert_eq!(report.manifests[0].trust, PluginTrustDecision::NeedsReview);
    assert!(report.registrations.is_empty());
}

#[test]
fn plugin_discovery_projects_tool_egress_secret_policy() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "context-pack"
event = "pre_tool_use"
kind = "context"
command = "scripts/check-tool-policy.sh"
args = ["--strict"]
declared_effect = "workspace_write"
timeout_ms = 45000
input_schema_digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
output_schema_digest = "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
approval = "deny"
egress_logging = false
allow_secrets = true

[[mcp_servers]]
transport = "stdio"
name = "repo-tools"
command = "node"
args = ["server.js"]
startup = "lazy"
required = false

[mcp_servers.trust]
approval_default = "allow"
egress_logging = false
allow_secrets = true
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");
    let manifest = &report.manifests[0];
    let hook = manifest
        .capabilities
        .iter()
        .find(|capability| matches!(capability, PluginCapability::Hook { .. }))
        .expect("hook capability should project");
    let mcp = manifest
        .capabilities
        .iter()
        .find(|capability| matches!(capability, PluginCapability::McpServer { .. }))
        .expect("mcp capability should project");

    let hook_policy = hook.policy_summary();
    assert_eq!(hook_policy.tool_category, Some(ToolCategory::Custom));
    assert_eq!(hook_policy.tool_access, Some(ToolAccess::Execute));
    assert_eq!(hook_policy.approval_default, Some(ApprovalMode::Deny));
    assert!(hook_policy.execution_backend_required);
    assert!(!hook_policy.egress_logging);
    assert!(hook_policy.allow_secrets);
    assert_eq!(hook_policy.mutation_effect, ToolEffect::WorkspaceWrite);
    assert!(matches!(
        hook,
        PluginCapability::Hook {
            id,
            hook_kind,
            args,
            timeout_ms,
            input_schema_digest,
            output_schema_digest,
            ..
        } if id == "context-pack"
            && *hook_kind == PluginHookKind::Context
            && args == &vec!["--strict".to_owned()]
            && *timeout_ms == 45_000
            && input_schema_digest.as_deref()
                == Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            && output_schema_digest.as_deref()
                == Some("sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
    ));

    let mcp_policy = mcp.policy_summary();
    assert_eq!(mcp_policy.tool_category, Some(ToolCategory::Mcp));
    assert_eq!(mcp_policy.tool_access, Some(ToolAccess::Execute));
    assert_eq!(mcp_policy.network_effect, Some(NetworkEffect::Unknown));
    assert_eq!(mcp_policy.approval_default, Some(ApprovalMode::Allow));
    assert!(mcp_policy.execution_backend_required);
    assert!(!mcp_policy.egress_logging);
    assert!(mcp_policy.allow_secrets);
    assert_eq!(mcp_policy.mutation_effect, ToolEffect::Unknown);
}

#[test]
fn disabled_plugin_manifest_does_not_register_capabilities() {
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

[[hooks]]
event = "pre_tool_use"
command = "scripts/check-tool-policy.sh"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Disabled, 42)
            .expect("trust should build");

    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("disabled plugin discovery should succeed");

    assert!(report.warnings.is_empty());
    assert_eq!(report.manifests[0].trust, PluginTrustDecision::Disabled);
    assert!(report.registrations.is_empty());
}

#[test]
fn plugin_manifest_digest_is_sha256_of_static_manifest_content() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let manifest_path = write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[skills]]
path = "skills/review/SKILL.md"
"#,
    );
    write_plugin_skill(
        workspace.path(),
        "repo-review",
        "skills/review/SKILL.md",
        "# Review",
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.warnings.is_empty());
    assert_eq!(
        report.manifests[0].manifest_hash,
        expected_manifest_digest(&manifest_path)
    );
}

#[test]
fn invalid_plugin_version_is_rejected_before_registration() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "bad version"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidManifest
            && warning
                .message
                .contains("version cannot contain whitespace")
    }));
}

#[test]
fn plugin_mcp_environment_grant_is_typed_pre_discovery_rejection() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[mcp_servers]]
transport = "stdio"
name = "credentialed"
command = "node"
inherit_env = ["PLUGIN_TOKEN"]
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("discovery should not abort");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.kind == PluginDiscoveryWarningKind::McpEnvironmentGrantNotSupported)
        .expect("typed environment grant diagnostic should exist");
    assert_eq!(
        warning.kind.code(),
        "plugin_mcp_environment_grant_not_supported"
    );
    assert_eq!(warning.entry_index, Some(0));
    assert_eq!(warning.server_name.as_deref(), Some("credentialed"));
    assert_eq!(warning.field.as_deref(), Some("inherit_env"));
    assert!(warning.remediation.is_some());
    assert!(!warning.trust_action_allowed);
}

#[test]
fn matching_trust_entry_emits_source_attributed_registrations() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_agent(
        workspace.path(),
        "repo-review",
        "agents/reviewer/agent.toml",
        r#"description = "Review agent."
instructions = "Review repository changes."
trust = "trusted"
"#,
    );
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

[[agents]]
path = "agents/reviewer/agent.toml"

[[skills]]
path = "skills/review/SKILL.md"

[[hooks]]
event = "pre_tool_use"
command = "scripts/check-tool-policy.sh"
approval = "ask"

[[mcp_servers]]
transport = "stdio"
name = "repo-tools"
command = "node"
args = ["server.js"]
startup = "lazy"
required = false
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");

    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");

    assert!(manifest_path.is_file());
    assert!(report.warnings.is_empty());
    assert_eq!(report.manifests[0].trust, PluginTrustDecision::Trusted);
    assert_eq!(report.registrations.agents.len(), 1);
    let agent = &report.registrations.agents[0];
    assert_eq!(agent.plugin_id, "repo-review");
    assert_eq!(agent.agent.path, Path::new("agents/reviewer/agent.toml"));
    assert_eq!(
        agent.plugin_root,
        workspace.path().join(".sigil/plugins/repo-review")
    );
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
    let hook = &report.registrations.hooks[0];
    assert_eq!(hook.plugin_id, "repo-review");
    assert_eq!(
        hook.plugin_root,
        workspace.path().join(".sigil/plugins/repo-review")
    );
    assert_eq!(
        hook.manifest_path,
        Path::new(".sigil/plugins/repo-review/plugin.toml")
    );
    assert_eq!(hook.manifest_hash, expected_manifest_digest(&manifest_path));
    assert_eq!(hook.manifest_version, "0.1.0");
    assert_eq!(
        hook.capability_digest,
        report.manifests[0]
            .capability_digest()
            .expect("capability digest should compute")
    );
    assert_eq!(hook.trust, PluginTrustDecision::Trusted);
    assert_eq!(hook.hook.approval, ApprovalMode::Ask);
    assert_eq!(report.registrations.mcp_servers.len(), 1);
    let mcp = &report.registrations.mcp_servers[0];
    assert_eq!(mcp.plugin_id, "repo-review");
    assert_eq!(mcp.original_name, "repo-tools");
    assert_eq!(mcp.server.name, "repo-tools");
    assert_eq!(mcp.server.startup, McpServerStartup::Lazy);
    assert_eq!(
        mcp.plugin_root,
        workspace
            .path()
            .join(".sigil/plugins/repo-review")
            .canonicalize()
            .expect("plugin root should canonicalize")
    );
    assert_eq!(
        mcp.manifest_path,
        manifest_path
            .canonicalize()
            .expect("manifest should canonicalize")
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

[[hooks]]
event = "context"
kind = "context"
command = "scripts/context.sh"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let stale_trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.2.0"

[[skills]]
path = "skills/review/SKILL.md"

[[hooks]]
event = "context"
kind = "context"
command = "scripts/context.sh"
"#,
    );

    let changed = discover_workspace_plugins(workspace.path(), &[stale_trust])
        .expect("changed manifest discovery should succeed");

    assert_eq!(changed.manifests[0].version, "0.2.0");
    assert_eq!(changed.manifests[0].trust, PluginTrustDecision::NeedsReview);
    assert!(changed.registrations.is_empty());
}

#[tokio::test]
async fn trusted_plugin_hook_runner_uses_execution_backend_and_emits_evidence() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let manifest_path = write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "context-pack"
event = "context"
kind = "context"
command = "hook-runner"
args = ["--json"]
declared_effect = "read_only"
timeout_ms = 45000
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let registration = report.registrations.hooks[0].clone();
    let backend = RecordingExecutionBackend::default();
    let requests = backend.requests.clone();
    let runner = PluginHookExecutionRunner::new(Arc::new(backend));

    let outcome = runner
        .execute(PluginHookExecutionRequest::new(
            registration,
            workspace.path().to_path_buf(),
        ))
        .await
        .expect("hook execution should succeed");

    let requests = requests.lock().expect("requests should lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].program, "hook-runner");
    assert_eq!(requests[0].args, vec!["--json"]);
    assert_eq!(
        requests[0].cwd,
        workspace
            .path()
            .join(".sigil/plugins/repo-review")
            .canonicalize()
            .expect("plugin root should canonicalize")
    );
    assert_eq!(
        requests[0].env.get("SIGIL_WORKSPACE_ROOT"),
        Some(&workspace.path().to_string_lossy().into_owned())
    );
    assert_eq!(
        requests[0].env.get("SIGIL_PLUGIN_HOOK_ID"),
        Some(&"context-pack".to_owned())
    );
    assert_eq!(requests[0].timeout_ms, Some(45_000));
    assert_eq!(requests[0].timeout_secs, 0);
    assert_eq!(
        requests[0].environment_policy,
        sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension
    );
    assert!(!requests[0].env.contains_key("HOME"));
    drop(requests);

    assert_eq!(outcome.started.plugin_id, "repo-review");
    assert_eq!(outcome.started.hook_id, "context-pack");
    assert_eq!(outcome.started.command, vec!["hook-runner", "--json"]);
    assert_eq!(outcome.started.backend, ExecutionBackendKind::Local);
    assert_eq!(
        outcome.started.execution_coverage,
        ExecutionCoverageLabel::LocalBackendEnforced
    );
    assert_eq!(
        outcome.started.sandbox_profile,
        ExecutionSandboxProfile::Unconfined
    );
    assert!(outcome.started.egress_logging);
    assert!(!outcome.started.allow_secrets);
    assert_eq!(
        outcome.started.capability_digest,
        report.manifests[0]
            .capability_digest()
            .expect("capability digest should compute")
    );
    assert_eq!(
        outcome.started.manifest_hash,
        expected_manifest_digest(&manifest_path)
    );
    assert_eq!(outcome.finished.execution_id, outcome.started.execution_id);
    assert_eq!(
        outcome.finished.status,
        PluginHookExecutionStatus::Succeeded
    );
    assert_eq!(outcome.finished.stdout_bytes, 2);
    assert_eq!(outcome.finished.stderr_bytes, 0);
    assert_eq!(
        outcome.finished.execution_coverage,
        ExecutionCoverageLabel::LocalBackendEnforced
    );
    assert_eq!(
        outcome.finished.sandbox_profile,
        ExecutionSandboxProfile::Unconfined
    );
    assert!(outcome.finished.egress_logging);
    assert!(!outcome.finished.allow_secrets);
    assert_eq!(outcome.receipt.stdout, b"ok");
}

#[tokio::test]
async fn trusted_plugin_hook_runner_records_configured_sandbox_policy_evidence() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "context-pack"
event = "context"
kind = "context"
command = "hook-runner"
declared_effect = "read_only"
egress_logging = false
allow_secrets = true
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let backend = RecordingExecutionBackend {
        backend_kind: ExecutionBackendKind::MacosSeatbelt,
        capabilities: ExecutionBackendCapabilities {
            filesystem_isolation: true,
            process_isolation: true,
            ..ExecutionBackendCapabilities::default()
        },
        network: ExecutionNetworkReceipt::allowed("profile allows network access"),
        ..RecordingExecutionBackend::default()
    };
    let runner = PluginHookExecutionRunner::new_with_sandbox_profile(
        Arc::new(backend),
        ExecutionSandboxProfile::BuildNetworked,
    );

    let outcome = runner
        .execute(PluginHookExecutionRequest::new(
            report.registrations.hooks[0].clone(),
            workspace.path().to_path_buf(),
        ))
        .await
        .expect("hook execution should succeed");

    assert_eq!(outcome.started.backend, ExecutionBackendKind::MacosSeatbelt);
    assert!(outcome.started.backend_capabilities.filesystem_isolation);
    assert!(outcome.started.backend_capabilities.process_isolation);
    assert_eq!(
        outcome.started.execution_coverage,
        ExecutionCoverageLabel::LocalBackendEnforced
    );
    assert_eq!(
        outcome.started.sandbox_profile,
        ExecutionSandboxProfile::BuildNetworked
    );
    assert!(!outcome.started.egress_logging);
    assert!(outcome.started.allow_secrets);
    assert_eq!(
        outcome.finished.backend,
        ExecutionBackendKind::MacosSeatbelt
    );
    assert_eq!(
        outcome.finished.network.policy,
        sigil_kernel::ExecutionNetworkPolicy::Allowed
    );
    assert_eq!(
        outcome.finished.sandbox_profile,
        ExecutionSandboxProfile::BuildNetworked
    );
    assert!(!outcome.finished.egress_logging);
    assert!(outcome.finished.allow_secrets);
}

#[tokio::test]
async fn plugin_hook_network_deny_without_proven_isolation_is_zero_execute() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let registration = trusted_read_only_hook_registration(workspace.path());
    let backends = [
        RecordingExecutionBackend {
            backend_kind: ExecutionBackendKind::MacosSeatbelt,
            capabilities: ExecutionBackendCapabilities {
                filesystem_isolation: true,
                process_isolation: true,
                network_isolation: false,
                ..ExecutionBackendCapabilities::default()
            },
            ..RecordingExecutionBackend::default()
        },
        RecordingExecutionBackend {
            backend_kind: ExecutionBackendKind::LinuxBubblewrap,
            capabilities: ExecutionBackendCapabilities {
                filesystem_isolation: true,
                process_isolation: true,
                network_isolation: true,
                ..ExecutionBackendCapabilities::default()
            },
            network: ExecutionNetworkReceipt::allowed(
                "backend supports isolation but this instance allows network",
            ),
            ..RecordingExecutionBackend::default()
        },
    ];

    for backend in backends {
        let requests = backend.requests.clone();
        let runner = PluginHookExecutionRunner::new_with_sandbox_profile(
            Arc::new(backend),
            ExecutionSandboxProfile::BuildNetworked,
        )
        .with_network_admission(ExtensionProcessNetworkAdmission::new(
            NetworkPolicy::Deny,
            false,
        ));
        let error = runner
            .execute(PluginHookExecutionRequest::new(
                registration.clone(),
                workspace.path().to_path_buf(),
            ))
            .await
            .expect_err("unproven network denial must fail before execute");

        assert_eq!(
            error
                .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
                .map(|error| error.code),
            Some(sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkIsolationUnavailable)
        );
        assert!(requests.lock().expect("requests should lock").is_empty());
    }
}

#[tokio::test]
async fn plugin_hook_network_ask_without_approval_is_zero_execute() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let registration = trusted_read_only_hook_registration(workspace.path());
    let backend = RecordingExecutionBackend::default();
    let requests = backend.requests.clone();
    let runner = PluginHookExecutionRunner::new(Arc::new(backend)).with_network_admission(
        ExtensionProcessNetworkAdmission::new(NetworkPolicy::Ask, false),
    );

    let error = runner
        .execute(PluginHookExecutionRequest::new(
            registration,
            workspace.path().to_path_buf(),
        ))
        .await
        .expect_err("network ask without explicit approval must fail before execute");

    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkApprovalRequired)
    );
    assert!(requests.lock().expect("requests should lock").is_empty());
}

#[tokio::test]
async fn plugin_hook_network_ask_with_approval_executes() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let registration = trusted_read_only_hook_registration(workspace.path());
    let backend = RecordingExecutionBackend::default();
    let requests = backend.requests.clone();
    let runner = PluginHookExecutionRunner::new(Arc::new(backend)).with_network_admission(
        ExtensionProcessNetworkAdmission::new(NetworkPolicy::Ask, true),
    );

    let outcome = runner
        .execute(PluginHookExecutionRequest::new(
            registration,
            workspace.path().to_path_buf(),
        ))
        .await
        .expect("explicit network approval should admit plugin hook execution");

    assert_eq!(outcome.receipt.network, ExecutionNetworkReceipt::default());
    assert_eq!(requests.lock().expect("requests should lock").len(), 1);
}

#[tokio::test]
async fn plugin_hook_network_deny_with_proven_isolation_executes() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let registration = trusted_read_only_hook_registration(workspace.path());
    let backend = RecordingExecutionBackend {
        backend_kind: ExecutionBackendKind::LinuxBubblewrap,
        capabilities: ExecutionBackendCapabilities {
            filesystem_isolation: true,
            process_isolation: true,
            network_isolation: true,
            ..ExecutionBackendCapabilities::default()
        },
        network: ExecutionNetworkReceipt::denied("test backend proves isolated process tree"),
        ..RecordingExecutionBackend::default()
    };
    let requests = backend.requests.clone();
    let runner = PluginHookExecutionRunner::new_with_sandbox_profile(
        Arc::new(backend),
        ExecutionSandboxProfile::BuildNetworked,
    )
    .with_network_admission(ExtensionProcessNetworkAdmission::new(
        NetworkPolicy::Deny,
        false,
    ));

    let outcome = runner
        .execute(PluginHookExecutionRequest::new(
            registration,
            workspace.path().to_path_buf(),
        ))
        .await
        .expect("denied receipt plus process-tree isolation should admit execution");

    assert!(outcome.receipt.network.is_denied());
    assert_eq!(requests.lock().expect("requests should lock").len(), 1);
}

#[tokio::test]
async fn plugin_hook_local_backend_clears_ambient_environment() {
    if std::env::var("HOME").is_err() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "environment"
event = "context"
kind = "context"
command = "/bin/sh"
args = ["-c", "printf '%s|%s' \"${HOME-unset}\" \"${PATH-unset}\""]
declared_effect = "read_only"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted discovery should succeed");
    let runner =
        PluginHookExecutionRunner::new(Arc::new(sigil_tools_builtin::LocalExecutionBackend));

    let outcome = runner
        .execute(PluginHookExecutionRequest::new(
            report.registrations.hooks[0].clone(),
            workspace.path().to_path_buf(),
        ))
        .await
        .expect("isolated hook should execute");

    assert!(outcome.output.stdout.content.starts_with("unset|"));
    assert!(!outcome.output.stdout.content.ends_with("|unset"));
    assert_eq!(
        outcome.receipt.environment_policy,
        sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension
    );
}

#[tokio::test]
async fn plugin_hook_runner_rejects_untrusted_registration() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
event = "context"
kind = "context"
command = "hook-runner"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let mut registration = report.registrations.hooks[0].clone();
    registration.trust = PluginTrustDecision::NeedsReview;
    let runner = PluginHookExecutionRunner::new(Arc::new(RecordingExecutionBackend::default()));

    let error = runner
        .execute(PluginHookExecutionRequest::new(
            registration,
            workspace.path().to_path_buf(),
        ))
        .await
        .expect_err("untrusted hook should be rejected");

    assert!(error.to_string().contains("is not trusted"));
}

#[tokio::test]
async fn plugin_hook_output_envelope_bounds_redacts_and_caps_artifacts() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "context-pack"
event = "context"
kind = "context"
command = "hook-runner"
declared_effect = "read_only"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let stdout = format!("head-{}-tail", "x".repeat(200)).into_bytes();
    let retained_stdout_bytes = stdout.len() as u64;
    let backend = RecordingExecutionBackend {
        stdout,
        stderr: b"token=super-secret".to_vec(),
        output: Some(sigil_kernel::ExecutionOutputReceipt {
            schema_version: sigil_kernel::EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
            stdout: sigil_kernel::ExecutionStreamCapture {
                total_bytes: 10_000,
                returned_bytes: retained_stdout_bytes,
                omitted_bytes: 10_000 - retained_stdout_bytes,
                retained_head_bytes: retained_stdout_bytes / 2,
                retained_tail_bytes: retained_stdout_bytes - retained_stdout_bytes / 2,
                retained_limit_bytes: retained_stdout_bytes,
                hard_limit_bytes: 16_000,
                total_lines: 1,
                truncated: true,
            },
            stderr: sigil_kernel::ExecutionStreamCapture {
                total_bytes: 18,
                returned_bytes: 18,
                omitted_bytes: 0,
                retained_head_bytes: 18,
                retained_tail_bytes: 0,
                retained_limit_bytes: 18,
                hard_limit_bytes: 16_000,
                total_lines: 1,
                truncated: false,
            },
            combined_total_bytes: 10_018,
            combined_hard_limit_bytes: 32_000,
            termination: sigil_kernel::ExecutionTerminationCause::Exited,
        }),
        ..RecordingExecutionBackend::default()
    };
    let runner = PluginHookExecutionRunner::new(Arc::new(backend));
    let mut request = PluginHookExecutionRequest::new(
        report.registrations.hooks[0].clone(),
        workspace.path().to_path_buf(),
    );
    request.output_limit_bytes = 32;
    request.redactor = SecretRedactor::from_values(["super-secret"]);
    request.artifact_refs = (0..(MAX_PLUGIN_HOOK_ARTIFACT_REFS + 4))
        .map(|index| PluginHookOutputArtifactRef {
            artifact_id: format!("artifact-{index}"),
            label: format!("artifact {index}"),
            media_type: Some("text/plain".to_owned()),
            size_bytes: Some(index as u64),
            redaction_state: RedactionState::None,
        })
        .collect();

    let outcome = runner
        .execute(request)
        .await
        .expect("hook execution should succeed");

    assert!(outcome.output.stdout.truncated);
    assert_eq!(
        outcome.output.stdout.returned_bytes + outcome.output.stdout.omitted_bytes,
        outcome.output.stdout.total_bytes
    );
    assert!(outcome.output.stdout.returned_bytes <= 32);
    assert!(
        outcome
            .output
            .stdout
            .content
            .contains("hook output truncated")
    );
    assert!(!outcome.output.stdout.content.contains(&"x".repeat(200)));
    assert_eq!(
        outcome.output.stderr.redaction_state,
        RedactionState::Redacted
    );
    assert!(!outcome.output.stderr.content.contains("super-secret"));
    assert_eq!(outcome.output.redaction_state, RedactionState::Redacted);
    assert_eq!(
        outcome.output.artifact_refs.len(),
        MAX_PLUGIN_HOOK_ARTIFACT_REFS
    );
    assert!(outcome.output.artifact_refs_truncated);
    assert!(
        !outcome
            .output
            .model_visible_summary
            .contains("super-secret")
    );
    assert!(
        !outcome
            .output
            .model_visible_summary
            .contains(&"x".repeat(16))
    );
    assert_eq!(outcome.output.parse_error, None);
}

#[test]
fn plugin_hook_output_redacts_secrets_split_across_head_and_tail_boundaries() {
    let head = b"before token-alpha-lo";
    let tail = b"ng after";
    let mut bytes = head.to_vec();
    bytes.extend_from_slice(tail);
    let capture = sigil_kernel::ExecutionStreamCapture {
        total_bytes: bytes.len() as u64 + 32,
        returned_bytes: bytes.len() as u64,
        omitted_bytes: 32,
        retained_head_bytes: head.len() as u64,
        retained_tail_bytes: tail.len() as u64,
        retained_limit_bytes: bytes.len() as u64,
        hard_limit_bytes: 1024,
        total_lines: 1,
        truncated: true,
    };
    let stream = super::bounded_hook_output_stream(
        &bytes,
        &capture,
        1024,
        &SecretRedactor::from_values(["token-alpha-long"]),
    );

    assert!(stream.content.contains("[redacted]"));
    assert!(!stream.content.contains("token-alpha-lo"));
    assert!(!stream.content.contains("ng after"));
    assert_eq!(stream.redaction_state, RedactionState::Redacted);
    assert_eq!(
        stream.returned_bytes + stream.omitted_bytes,
        stream.total_bytes
    );
}

#[tokio::test]
async fn plugin_hook_runner_records_workspace_mutation_for_writing_hook() -> Result<()> {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let state = tempfile::tempdir().expect("state should create");
    fs::write(workspace.path().join("note.txt"), "old").expect("note should write");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "write-note"
event = "context"
kind = "context"
command = "hook-runner"
declared_effect = "workspace_write"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let store = JsonlSessionStore::new(state.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(
        store.clone(),
        state.path().join("mutation-artifacts"),
    );
    let backend = RecordingExecutionBackend {
        workspace_write: Some(("note.txt".to_owned(), "new".to_owned())),
        ..RecordingExecutionBackend::default()
    };
    let runner = PluginHookExecutionRunner::new(Arc::new(backend));

    let outcome = runner
        .execute(
            PluginHookExecutionRequest::new(
                report.registrations.hooks[0].clone(),
                workspace.path().to_path_buf(),
            )
            .with_mutation_recorder(recorder),
        )
        .await
        .expect("hook execution should succeed");

    assert_eq!(
        fs::read_to_string(workspace.path().join("note.txt")).expect("note should read"),
        "new"
    );
    let mutation_event_id = outcome
        .mutation_event_id
        .as_deref()
        .expect("workspace mutation should be recorded");
    let detection = workspace_mutation_detected(store.path(), mutation_event_id)?;
    assert_eq!(detection.tool_name, "plugin_hook:repo-review:write-note");
    assert_eq!(detection.tool_effect, ToolEffect::WorkspaceWrite);
    assert_eq!(
        detection.tool_call_id.as_deref(),
        Some(outcome.started.execution_id.as_str())
    );
    assert!(!detection.unknown_dirty);
    assert_ne!(
        detection.from_workspace_snapshot_id,
        detection.to_workspace_snapshot_id
    );
    Ok(())
}

#[tokio::test]
async fn mutating_plugin_hook_requires_mutation_recorder_before_execution() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "write-note"
event = "context"
kind = "context"
command = "hook-runner"
declared_effect = "workspace_write"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let backend = RecordingExecutionBackend::default();
    let requests = backend.requests.clone();
    let runner = PluginHookExecutionRunner::new(Arc::new(backend));

    let error = runner
        .execute(PluginHookExecutionRequest::new(
            report.registrations.hooks[0].clone(),
            workspace.path().to_path_buf(),
        ))
        .await
        .expect_err("mutating hook without recorder should fail closed");

    assert!(error.to_string().contains("requires mutation recorder"));
    assert!(
        requests.lock().expect("requests should lock").is_empty(),
        "mutating hook should not execute before mutation evidence can be recorded"
    );
}

#[tokio::test]
async fn read_only_plugin_hook_does_not_dirty_verification_scope() -> Result<()> {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let state = tempfile::tempdir().expect("state should create");
    fs::write(workspace.path().join("note.txt"), "same").expect("note should write");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "inspect-note"
event = "context"
kind = "context"
command = "hook-runner"
declared_effect = "read_only"
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");
    let store = JsonlSessionStore::new(state.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(
        store.clone(),
        state.path().join("mutation-artifacts"),
    );
    let runner = PluginHookExecutionRunner::new(Arc::new(RecordingExecutionBackend::default()));

    let outcome = runner
        .execute(
            PluginHookExecutionRequest::new(
                report.registrations.hooks[0].clone(),
                workspace.path().to_path_buf(),
            )
            .with_mutation_recorder(recorder),
        )
        .await
        .expect("hook execution should succeed");

    assert_eq!(outcome.mutation_event_id, None);
    assert!(
        workspace_mutation_events(store.path())?.is_empty(),
        "read-only hook should not append workspace mutation evidence"
    );
    Ok(())
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
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");

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

#[test]
fn missing_plugin_agent_entrypoint_is_reported_as_invalid_path() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[agents]]
path = "agents/reviewer/agent.toml"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidPath
            && warning
                .message
                .contains("failed to resolve plugin repo-review agent")
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

#[cfg(unix)]
#[test]
fn symlinked_agent_entrypoint_escape_is_rejected_as_invalid_path() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let outside = tempfile::tempdir().expect("outside should create");
    fs::create_dir_all(outside.path().join("agent")).expect("outside agent dir should create");
    fs::write(
        outside.path().join("agent/agent.toml"),
        "description = \"Outside\"",
    )
    .expect("outside agent should write");
    let link_parent = workspace.path().join(".sigil/plugins/repo-review/agents");
    fs::create_dir_all(&link_parent).expect("link parent should create");
    symlink(outside.path().join("agent"), link_parent.join("reviewer"))
        .expect("symlink should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[agents]]
path = "agents/reviewer/agent.toml"
"#,
    );

    let report =
        discover_workspace_plugins(workspace.path(), &[]).expect("plugin discovery should succeed");

    assert!(report.manifests.is_empty());
    assert!(report.registrations.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == PluginDiscoveryWarningKind::InvalidPath
            && warning.message.contains("agent path escapes plugin root")
    }));
}

#[test]
fn plugin_mcp_servers_merge_into_origin_preserving_declarations() -> Result<()> {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_plugin_manifest(
        workspace.path(),
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[mcp_servers]]
transport = "stdio"
name = "repo-tools"
command = "/definitely/missing/plugin-mcp-server"
startup = "lazy"
required = false
"#,
    );
    let pending = discover_workspace_plugins(workspace.path(), &[])
        .expect("initial discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
    let report = discover_workspace_plugins(workspace.path(), &[trust])
        .expect("trusted plugin discovery should succeed");

    let merged_mcp = merge_mcp_server_declarations(&[], &report.registrations.mcp_servers)?;
    assert_eq!(merged_mcp[0].declared_name(), "repo-tools");
    assert_eq!(merged_mcp[0].effective_name(), "repo-review.repo-tools");
    assert!(matches!(
        merged_mcp[0].origin(),
        McpConfigOrigin::PluginManifest { plugin_id, .. } if plugin_id == "repo-review"
    ));
    assert!(matches!(
        merged_mcp[0].execution_base(),
        McpExecutionBase::PluginRoot(_)
    ));
    assert!(merged_mcp[0].plugin_attestation().is_some());
    let mut invalid_registration = report.registrations.mcp_servers[0].clone();
    let sigil_kernel::McpServerTransportConfig::Stdio { inherit_env, .. } =
        &mut invalid_registration.server.transport
    else {
        panic!("plugin fixture should remain stdio");
    };
    *inherit_env = vec!["PLUGIN_TOKEN".to_owned()];
    let error = merge_mcp_server_declarations(&[], &[invalid_registration])
        .expect_err("programmatic plugin environment grant should fail merge");
    assert_eq!(error.code(), "plugin_mcp_environment_grant_not_supported");
    let conflicting_base = resolve_user_root_mcp_declarations(
        &[mcp_server_config! {
            name: "repo-review.repo-tools".to_owned(),
            command: "existing".to_owned(),
            ..sigil_kernel::McpServerConfig::default()
        }],
        workspace.path(),
    )?;
    let identity = "repo-review\0repo-tools";
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let deeply_conflicting_base = resolve_user_root_mcp_declarations(
        &[
            mcp_server_config! {
                name: "repo-review.repo-tools".to_owned(),
                command: "existing".to_owned(),
                ..sigil_kernel::McpServerConfig::default()
            },
            mcp_server_config! {
                name: format!("repo-review.repo-tools.{}", &hash[..8]),
                command: "existing-hash".to_owned(),
                ..sigil_kernel::McpServerConfig::default()
            },
        ],
        workspace.path(),
    )?;
    let conflict_merged =
        merge_mcp_server_declarations(&conflicting_base, &report.registrations.mcp_servers)?;
    assert_eq!(
        conflict_merged[0].effective_name(),
        "repo-review.repo-tools"
    );
    assert!(
        conflict_merged[1]
            .effective_name()
            .starts_with("repo-review.repo-tools.")
    );
    assert_ne!(
        conflict_merged[0].effective_name(),
        conflict_merged[1].effective_name()
    );
    assert_eq!(conflict_merged[1].declared_name(), "repo-tools");
    let deep_conflict_merged =
        merge_mcp_server_declarations(&deeply_conflicting_base, &report.registrations.mcp_servers)?;
    assert!(deep_conflict_merged[2].effective_name().ends_with(".1"));

    let mut reserved = report.registrations.mcp_servers[0].clone();
    reserved.original_name = "builtin:claimed".to_owned();
    reserved.server.name = reserved.original_name.clone();
    let error = merge_mcp_server_declarations(&[], &[reserved])
        .expect_err("plugin cannot claim builtin namespace through namespacing");
    assert_eq!(error.code(), "reserved_mcp_namespace");

    let mut forged_plugin_id = report.registrations.mcp_servers[0].clone();
    forged_plugin_id.plugin_id = "builtin:evil".to_owned();
    let error = merge_mcp_server_declarations(&[], &[forged_plugin_id])
        .expect_err("forged plugin id cannot create a builtin effective namespace");
    assert_eq!(error.code(), "reserved_mcp_namespace");

    let mut untrusted = report.registrations.mcp_servers[0].clone();
    untrusted.trust = PluginTrustDecision::NeedsReview;
    let error = merge_mcp_server_declarations(&[], &[untrusted])
        .expect_err("programmatic merge cannot bypass current plugin trust");
    assert_eq!(error.code(), "plugin_mcp_attestation_review_required");

    let mut mismatched_name = report.registrations.mcp_servers[0].clone();
    mismatched_name.server.name = "different-server".to_owned();
    let error = merge_mcp_server_declarations(&[], &[mismatched_name])
        .expect_err("registration source name must match the reviewed config");
    assert_eq!(error.code(), "plugin_origin_attestation_mismatch");
    Ok(())
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
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("trust should build");
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

#[derive(Clone)]
struct RecordingExecutionBackend {
    requests: Arc<Mutex<Vec<ExecutionRequest>>>,
    backend_kind: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: ExecutionNetworkReceipt,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    output: Option<sigil_kernel::ExecutionOutputReceipt>,
    workspace_write: Option<(String, String)>,
}

impl Default for RecordingExecutionBackend {
    fn default() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            backend_kind: ExecutionBackendKind::Local,
            capabilities: ExecutionBackendCapabilities::default(),
            network: ExecutionNetworkReceipt::default(),
            stdout: b"ok".to_vec(),
            stderr: Vec::new(),
            output: None,
            workspace_write: None,
        }
    }
}

impl ExecutionBackend for RecordingExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        self.backend_kind
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        self.capabilities
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        self.network.clone()
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        let requests = self.requests.clone();
        let stdout = self.stdout.clone();
        let stderr = self.stderr.clone();
        let output = self.output.clone();
        let workspace_write = self.workspace_write.clone();
        let backend_kind = self.backend_kind;
        let capabilities = self.capabilities;
        let network = self.network.clone();
        Box::pin(async move {
            let environment_policy = request.environment_policy;
            if let Some((relative_path, content)) = workspace_write {
                let workspace_root = request
                    .env
                    .get("SIGIL_WORKSPACE_ROOT")
                    .expect("workspace root env should be provided");
                let path = Path::new(workspace_root).join(relative_path);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).expect("workspace write parent should create");
                }
                fs::write(path, content).expect("workspace write should succeed");
            }
            requests.lock().expect("requests should lock").push(request);
            let output = output.unwrap_or_else(|| complete_output_receipt(&stdout, &stderr));
            Ok(ExecutionReceipt {
                backend: backend_kind,
                capabilities,
                network,
                resources: Default::default(),
                environment_policy,
                exit_code: Some(0),
                stdout,
                stderr,
                output,
                timed_out: false,
            })
        })
    }
}

fn complete_output_receipt(stdout: &[u8], stderr: &[u8]) -> sigil_kernel::ExecutionOutputReceipt {
    let stream_capture = |bytes: &[u8]| sigil_kernel::ExecutionStreamCapture {
        total_bytes: bytes.len() as u64,
        returned_bytes: bytes.len() as u64,
        omitted_bytes: 0,
        retained_head_bytes: bytes.len() as u64,
        retained_tail_bytes: 0,
        retained_limit_bytes: bytes.len() as u64,
        hard_limit_bytes: bytes.len() as u64,
        total_lines: bytes
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            .saturating_add(usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n")))
            as u64,
        truncated: false,
    };
    sigil_kernel::ExecutionOutputReceipt {
        schema_version: sigil_kernel::EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
        stdout: stream_capture(stdout),
        stderr: stream_capture(stderr),
        combined_total_bytes: stdout.len().saturating_add(stderr.len()) as u64,
        combined_hard_limit_bytes: stdout.len().saturating_add(stderr.len()) as u64,
        termination: sigil_kernel::ExecutionTerminationCause::Exited,
    }
}

fn workspace_mutation_detected(
    session_path: &Path,
    event_id: &str,
) -> Result<WorkspaceMutationDetected> {
    let events = workspace_mutation_events(session_path)?;
    events
        .into_iter()
        .find(|(id, _)| id == event_id)
        .map(|(_, payload)| payload)
        .ok_or_else(|| anyhow::anyhow!("workspace mutation event {event_id} not found"))
}

fn workspace_mutation_events(
    session_path: &Path,
) -> Result<Vec<(String, WorkspaceMutationDetected)>> {
    let mut events = Vec::new();
    for record in JsonlSessionStore::read_event_records(session_path)? {
        let event = record.into_stored_event();
        if event.event_type != "workspace_mutation_detected" {
            continue;
        }
        let payload = serde_json::from_value::<WorkspaceMutationDetected>(event.payload)?;
        events.push((event.event_id, payload));
    }
    Ok(events)
}

fn trusted_read_only_hook_registration(workspace: &Path) -> super::PluginHookRegistration {
    write_plugin_manifest(
        workspace,
        "repo-review",
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "context-pack"
event = "context"
kind = "context"
command = "hook-runner"
declared_effect = "read_only"
"#,
    );
    let pending = discover_workspace_plugins(workspace, &[])
        .expect("initial plugin discovery should succeed");
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)
            .expect("plugin trust should build");
    discover_workspace_plugins(workspace, &[trust])
        .expect("trusted plugin discovery should succeed")
        .registrations
        .hooks
        .into_iter()
        .next()
        .expect("trusted hook should register")
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

fn write_plugin_agent(workspace: &Path, plugin_id: &str, relative_path: &str, body: &str) {
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

fn expected_manifest_digest(path: &Path) -> String {
    let bytes = fs::read(path).expect("manifest should read");
    format!("sha256:{:x}", Sha256::digest(&bytes))
}
