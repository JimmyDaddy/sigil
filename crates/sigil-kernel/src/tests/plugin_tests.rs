use anyhow::Result;

use crate::{
    ApprovalMode, ContextPackOptions, ContextSensitivity, ContextSource, ContextTrustLevel,
    ControlEntry, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCoverageLabel,
    ExecutionNetworkReceipt, ExecutionSandboxProfile, McpServerConfig, McpServerStartup,
    PluginAgentRef, PluginCapability, PluginHookContextOptions, PluginHookExecutionFinishedEntry,
    PluginHookExecutionStartedEntry, PluginHookExecutionStatus, PluginHookKind,
    PluginHookOutputEnvelope, PluginHookOutputStream, PluginHookRef, PluginManifest,
    PluginManifestSnapshot, PluginSkillRef, PluginStateProjection, PluginTrustDecision,
    PluginTrustEntry, RedactionState, SessionLogEntry, ToolEffect, pack_context_items,
    plugin_hook_output_context_items, plugin_manifest_digests_match, validate_plugin_id,
    validate_plugin_manifest_digest, validate_plugin_version,
};

const VALID_PLUGIN_DIGEST: &str =
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const VALID_PLUGIN_DIGEST_BARE: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn sample_manifest() -> PluginManifest {
    PluginManifest {
        id: "repo-review".to_owned(),
        name: "Repository Review".to_owned(),
        version: "0.1.0".to_owned(),
        description: Some("Reusable review workflows.".to_owned()),
        root: ".sigil/plugins/repo-review".into(),
        agents: vec![PluginAgentRef {
            path: "agents/reviewer/agent.toml".into(),
        }],
        skills: vec![PluginSkillRef {
            path: "skills/review/SKILL.md".into(),
        }],
        hooks: vec![PluginHookRef {
            id: Some("pre-tool-policy".to_owned()),
            event: "pre_tool_use".to_owned(),
            kind: PluginHookKind::Event,
            command: "scripts/check-tool-policy.sh".to_owned(),
            args: vec!["--strict".to_owned()],
            declared_effect: ToolEffect::ReadOnly,
            timeout_ms: 10_000,
            input_schema_digest: Some(VALID_PLUGIN_DIGEST.to_owned()),
            output_schema_digest: None,
            approval: ApprovalMode::Ask,
            egress_logging: true,
            allow_secrets: false,
        }],
        mcp_servers: vec![McpServerConfig {
            name: "repo-tools".to_owned(),
            command: "node".to_owned(),
            args: vec!["server.js".to_owned()],
            startup: McpServerStartup::Lazy,
            required: false,
            ..McpServerConfig::default()
        }],
    }
}

fn sample_snapshot() -> PluginManifestSnapshot {
    let manifest = sample_manifest();
    PluginManifestSnapshot {
        plugin_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
        manifest_hash: VALID_PLUGIN_DIGEST.to_owned(),
        capabilities: manifest.capabilities(),
        trust: PluginTrustDecision::NeedsReview,
    }
}

fn sample_trust(decision: PluginTrustDecision) -> PluginTrustEntry {
    PluginTrustEntry::for_snapshot(&sample_snapshot(), decision, 42)
        .expect("sample trust should build")
}

fn sample_hook_output(content: &str) -> PluginHookOutputEnvelope {
    PluginHookOutputEnvelope {
        execution_id: "hook-exec-1".to_owned(),
        plugin_id: "repo-review".to_owned(),
        hook_id: "context-rules".to_owned(),
        stdout: PluginHookOutputStream {
            content: content.to_owned(),
            total_bytes: content.len() as u64,
            returned_bytes: content.len() as u64,
            omitted_bytes: 0,
            total_lines: content.lines().count() as u64,
            returned_lines: content.lines().count() as u64,
            truncated: false,
            redaction_state: RedactionState::None,
        },
        stderr: PluginHookOutputStream {
            content: String::new(),
            total_bytes: 0,
            returned_bytes: 0,
            omitted_bytes: 0,
            total_lines: 0,
            returned_lines: 0,
            truncated: false,
            redaction_state: RedactionState::None,
        },
        artifact_refs: Vec::new(),
        artifact_refs_truncated: false,
        redaction_state: RedactionState::None,
        parse_error: None,
        model_visible_summary: "plugin hook context-rules finished succeeded".to_owned(),
    }
}

#[test]
fn plugin_manifest_validation_accepts_reviewable_capabilities() -> Result<()> {
    let manifest = sample_manifest();

    manifest.validate()?;
    let capabilities = manifest.capabilities();

    assert_eq!(capabilities.len(), 4);
    assert!(matches!(
        &capabilities[0],
        PluginCapability::Agent { path } if path == std::path::Path::new("agents/reviewer/agent.toml")
    ));
    assert!(matches!(
        &capabilities[1],
        PluginCapability::Skill { path } if path == std::path::Path::new("skills/review/SKILL.md")
    ));
    assert!(matches!(
        &capabilities[2],
        PluginCapability::Hook {
            id,
            event,
            hook_kind,
            command,
            args,
            declared_effect,
            timeout_ms,
            input_schema_digest,
            output_schema_digest,
            approval,
            egress_logging,
            allow_secrets,
        } if id == "pre-tool-policy"
            && event == "pre_tool_use"
            && *hook_kind == PluginHookKind::Event
            && command == "scripts/check-tool-policy.sh"
            && args == &vec!["--strict".to_owned()]
            && *declared_effect == ToolEffect::ReadOnly
            && *timeout_ms == 10_000
            && input_schema_digest.as_deref() == Some(VALID_PLUGIN_DIGEST)
            && output_schema_digest.is_none()
            && *approval == ApprovalMode::Ask
            && *egress_logging
            && !*allow_secrets
    ));
    assert!(matches!(
        &capabilities[3],
        PluginCapability::McpServer {
            name,
            command,
            args,
            startup,
            required,
            approval,
            egress_logging,
            allow_secrets,
        } if name == "repo-tools"
            && command == "node"
            && args == &vec!["server.js".to_owned()]
            && *startup == McpServerStartup::Lazy
            && !*required
            && *approval == ApprovalMode::Ask
            && *egress_logging
            && !*allow_secrets
    ));
    Ok(())
}

#[test]
fn plugin_capabilities_report_execution_coverage_boundaries() {
    let capabilities = sample_manifest().capabilities();

    let agent = capabilities[0].execution_coverage_summary();
    assert_eq!(agent.label, ExecutionCoverageLabel::PluginManaged);
    assert!(!agent.local_backend_controls_execution);
    assert!(agent.user_copy.contains("plugin trust"));
    assert!(agent.user_copy.contains("does not cover plugin code"));

    let hook = capabilities[2].execution_coverage_summary();
    assert_eq!(hook.label, ExecutionCoverageLabel::PluginManaged);
    assert!(!hook.local_backend_controls_execution);
    assert!(hook.user_copy.contains("plugin trust"));

    let skill = capabilities[1].execution_coverage_summary();
    assert_eq!(skill.label, ExecutionCoverageLabel::PluginManaged);
    assert!(!skill.local_backend_controls_execution);
    assert!(skill.user_copy.contains("plugin trust"));

    let mcp = capabilities[3].execution_coverage_summary();
    assert_eq!(mcp.label, ExecutionCoverageLabel::ExternalMcpServer);
    assert!(!mcp.local_backend_controls_execution);
    assert!(mcp.user_copy.contains("MCP tools run"));
    assert!(
        mcp.user_copy
            .contains("local shell sandbox does not cover them")
    );
}

#[test]
fn plugin_capabilities_map_to_normal_tool_secret_egress_policy() {
    let capabilities = sample_manifest().capabilities();

    let agent = capabilities[0].policy_summary();
    assert_eq!(agent.tool_access, None);
    assert!(!agent.execution_backend_required);
    assert!(!agent.egress_logging);
    assert!(!agent.allow_secrets);

    let hook = capabilities[2].policy_summary();
    assert_eq!(hook.tool_category, Some(crate::ToolCategory::Custom));
    assert_eq!(hook.tool_access, Some(crate::ToolAccess::Execute));
    assert_eq!(hook.approval_default, Some(ApprovalMode::Ask));
    assert!(hook.execution_backend_required);
    assert!(hook.egress_logging);
    assert!(!hook.allow_secrets);
    assert_eq!(hook.mutation_effect, ToolEffect::ReadOnly);

    let mcp = capabilities[3].policy_summary();
    assert_eq!(mcp.tool_category, Some(crate::ToolCategory::Mcp));
    assert_eq!(mcp.tool_access, Some(crate::ToolAccess::Network));
    assert_eq!(mcp.approval_default, Some(ApprovalMode::Ask));
    assert!(mcp.execution_backend_required);
    assert!(mcp.egress_logging);
    assert!(!mcp.allow_secrets);
    assert_eq!(mcp.mutation_effect, crate::ToolEffect::Unknown);
}

#[test]
fn plugin_hook_execution_control_entries_round_trip() {
    let started = PluginHookExecutionStartedEntry {
        execution_id: "plugin_hook_1".to_owned(),
        plugin_id: "repo-review".to_owned(),
        manifest_hash: VALID_PLUGIN_DIGEST.to_owned(),
        capability_digest: VALID_PLUGIN_DIGEST.to_owned(),
        hook_id: "context-pack".to_owned(),
        hook_kind: PluginHookKind::Context,
        command: vec!["hook-runner".to_owned(), "--json".to_owned()],
        declared_effect: ToolEffect::ReadOnly,
        timeout_ms: 30_000,
        backend: ExecutionBackendKind::Local,
        backend_capabilities: ExecutionBackendCapabilities::default(),
        execution_coverage: ExecutionCoverageLabel::LocalBackendEnforced,
        sandbox_profile: ExecutionSandboxProfile::Unconfined,
        egress_logging: true,
        allow_secrets: false,
    };
    let finished = PluginHookExecutionFinishedEntry {
        execution_id: started.execution_id.clone(),
        plugin_id: started.plugin_id.clone(),
        manifest_hash: started.manifest_hash.clone(),
        capability_digest: started.capability_digest.clone(),
        hook_id: started.hook_id.clone(),
        hook_kind: started.hook_kind,
        status: PluginHookExecutionStatus::Succeeded,
        exit_code: Some(0),
        stdout_bytes: 2,
        stderr_bytes: 0,
        timed_out: false,
        backend: ExecutionBackendKind::Local,
        backend_capabilities: ExecutionBackendCapabilities::default(),
        execution_coverage: ExecutionCoverageLabel::LocalBackendEnforced,
        sandbox_profile: ExecutionSandboxProfile::Unconfined,
        egress_logging: true,
        allow_secrets: false,
        network: ExecutionNetworkReceipt::default(),
        resources: Default::default(),
    };

    let started_json = serde_json::to_string(&SessionLogEntry::Control(
        ControlEntry::PluginHookExecutionStarted(started.clone()),
    ))
    .expect("started entry should encode");
    let finished_json = serde_json::to_string(&SessionLogEntry::Control(
        ControlEntry::PluginHookExecutionFinished(finished.clone()),
    ))
    .expect("finished entry should encode");

    match serde_json::from_str::<SessionLogEntry>(&started_json)
        .expect("started entry should decode")
    {
        SessionLogEntry::Control(ControlEntry::PluginHookExecutionStarted(restored)) => {
            assert_eq!(restored, started);
        }
        other => panic!("unexpected started entry: {other:?}"),
    }
    match serde_json::from_str::<SessionLogEntry>(&finished_json)
        .expect("finished entry should decode")
    {
        SessionLogEntry::Control(ControlEntry::PluginHookExecutionFinished(restored)) => {
            assert_eq!(restored, finished);
        }
        other => panic!("unexpected finished entry: {other:?}"),
    }
}

#[test]
fn plugin_hook_output_context_items_preserve_extension_provenance() -> Result<()> {
    let output =
        sample_hook_output("Use `cargo test -p sigil-runtime context` for context checks.");
    let context = plugin_hook_output_context_items(
        &output,
        PluginHookContextOptions::new("event-plugin-hook-finished"),
    )?;

    assert_eq!(context.items.len(), 1);
    let item = &context.items[0];
    assert_eq!(
        item.id,
        "plugin-hook:repo-review:context-rules:hook-exec-1:stdout"
    );
    assert_eq!(item.source, ContextSource::ExtensionProvided);
    assert_eq!(
        item.source_event_id.as_deref(),
        Some("event-plugin-hook-finished")
    );
    assert_eq!(item.trust_level, ContextTrustLevel::ExtensionProvided);
    assert_eq!(item.sensitivity, ContextSensitivity::Repository);
    assert!(item.egress_decision.is_none());
    assert_eq!(
        context.snippets.get(&item.id).map(String::as_str),
        Some("Use `cargo test -p sigil-runtime context` for context checks.")
    );

    let packed = pack_context_items(context.items, ContextPackOptions::new(128))?;
    assert_eq!(packed.dynamic_suffix.len(), 1);
    assert!(packed.stable_prefix.is_empty());
    Ok(())
}

#[test]
fn plugin_hook_output_context_items_keep_redacted_output_under_egress_policy() -> Result<()> {
    let mut output = sample_hook_output("token=[redacted]");
    output.stdout.redaction_state = RedactionState::Redacted;
    output.redaction_state = RedactionState::Redacted;

    let context = plugin_hook_output_context_items(
        &output,
        PluginHookContextOptions::new("event-plugin-hook-finished"),
    )?;
    assert_eq!(
        context.items[0].sensitivity,
        ContextSensitivity::PotentialSecret
    );

    let packed = pack_context_items(context.items, ContextPackOptions::new(128))?;
    assert!(packed.dynamic_suffix.is_empty());
    assert_eq!(packed.excluded.len(), 1);
    assert_eq!(
        packed.excluded[0].inclusion_reason,
        crate::ContextInclusionReason::ExcludedSecret
    );
    Ok(())
}

#[test]
fn plugin_manifest_validation_rejects_unsafe_edges() {
    assert!(validate_plugin_id("repo-review_1.2").is_ok());
    assert!(validate_plugin_id("bad plugin").is_err());

    let mut empty_name = sample_manifest();
    empty_name.name = "  ".to_owned();
    assert!(empty_name.validate().is_err());

    let mut empty_version = sample_manifest();
    empty_version.version.clear();
    assert!(empty_version.validate().is_err());

    let mut invalid_version = sample_manifest();
    invalid_version.version = "bad version".to_owned();
    assert!(invalid_version.validate().is_err());

    let mut invalid_id = sample_manifest();
    invalid_id.id = "bad plugin".to_owned();
    assert!(invalid_id.validate().is_err());

    let mut empty_skill_path = sample_manifest();
    empty_skill_path.skills[0].path.clear();
    assert!(empty_skill_path.validate().is_err());

    let mut empty_agent_path = sample_manifest();
    empty_agent_path.agents[0].path.clear();
    assert!(empty_agent_path.validate().is_err());

    let mut escaping_agent = sample_manifest();
    escaping_agent.agents[0].path = "../escape/agent.toml".into();
    assert!(escaping_agent.validate().is_err());

    let mut escaping_skill = sample_manifest();
    escaping_skill.skills[0].path = "../escape/SKILL.md".into();
    assert!(escaping_skill.validate().is_err());

    let mut absolute_skill = sample_manifest();
    absolute_skill.skills[0].path = "/tmp/SKILL.md".into();
    assert!(absolute_skill.validate().is_err());

    let mut empty_hook_event = sample_manifest();
    empty_hook_event.hooks[0].event = "  ".to_owned();
    assert!(empty_hook_event.validate().is_err());

    let mut invalid_hook_id = sample_manifest();
    invalid_hook_id.hooks[0].id = Some("bad hook id".to_owned());
    assert!(invalid_hook_id.validate().is_err());

    let mut empty_hook = sample_manifest();
    empty_hook.hooks[0].command = "  ".to_owned();
    assert!(empty_hook.validate().is_err());

    let mut invalid_hook_arg = sample_manifest();
    invalid_hook_arg.hooks[0].args = vec!["bad\narg".to_owned()];
    assert!(invalid_hook_arg.validate().is_err());

    let mut invalid_hook_timeout = sample_manifest();
    invalid_hook_timeout.hooks[0].timeout_ms = 0;
    assert!(invalid_hook_timeout.validate().is_err());

    let mut invalid_hook_schema = sample_manifest();
    invalid_hook_schema.hooks[0].input_schema_digest = Some("sha256:not-a-digest".to_owned());
    assert!(invalid_hook_schema.validate().is_err());

    let mut invalid_mcp_name = sample_manifest();
    invalid_mcp_name.mcp_servers[0].name = "bad server".to_owned();
    assert!(invalid_mcp_name.validate().is_err());

    let mut empty_mcp_command = sample_manifest();
    empty_mcp_command.mcp_servers[0].command.clear();
    assert!(empty_mcp_command.validate().is_err());
}

#[test]
fn plugin_snapshot_capability_and_trust_validation_reject_required_edges() {
    assert_eq!(PluginTrustDecision::Disabled.as_str(), "disabled");

    assert!(
        PluginCapability::Hook {
            id: " ".to_owned(),
            event: " ".to_owned(),
            hook_kind: PluginHookKind::Event,
            command: "scripts/hook.sh".to_owned(),
            args: Vec::new(),
            declared_effect: ToolEffect::Unknown,
            timeout_ms: 30_000,
            input_schema_digest: None,
            output_schema_digest: None,
            approval: ApprovalMode::Ask,
            egress_logging: true,
            allow_secrets: false,
        }
        .validate()
        .is_err()
    );
    assert!(
        PluginCapability::Hook {
            id: "pre-tool-policy".to_owned(),
            event: "pre_tool_use".to_owned(),
            hook_kind: PluginHookKind::Event,
            command: " ".to_owned(),
            args: Vec::new(),
            declared_effect: ToolEffect::Unknown,
            timeout_ms: 30_000,
            input_schema_digest: None,
            output_schema_digest: None,
            approval: ApprovalMode::Ask,
            egress_logging: true,
            allow_secrets: false,
        }
        .validate()
        .is_err()
    );
    assert!(
        PluginCapability::McpServer {
            name: "repo-tools".to_owned(),
            command: " ".to_owned(),
            args: Vec::new(),
            startup: McpServerStartup::Lazy,
            required: false,
            approval: ApprovalMode::Ask,
            egress_logging: true,
            allow_secrets: false,
        }
        .validate()
        .is_err()
    );

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.version.clear();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.version = "../0.1.0".to_owned();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.manifest_path.clear();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.manifest_hash = " ".to_owned();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.manifest_hash = "sha256:not-a-digest".to_owned();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.capabilities = vec![PluginCapability::Skill {
        path: "../x".into(),
    }];
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_trust = sample_trust(PluginTrustDecision::Trusted);
    invalid_trust.manifest_path.clear();
    assert!(invalid_trust.validate().is_err());

    let mut invalid_trust = sample_trust(PluginTrustDecision::Trusted);
    invalid_trust.manifest_hash.clear();
    assert!(invalid_trust.validate().is_err());

    let mut invalid_trust = sample_trust(PluginTrustDecision::Trusted);
    invalid_trust.manifest_hash = "md5:0123456789abcdef0123456789abcdef".to_owned();
    assert!(invalid_trust.validate().is_err());
}

#[test]
fn plugin_manifest_digest_validation_accepts_canonical_and_legacy_sha256() {
    assert!(validate_plugin_manifest_digest("repo-review", VALID_PLUGIN_DIGEST).is_ok());
    assert!(validate_plugin_manifest_digest("repo-review", VALID_PLUGIN_DIGEST_BARE).is_ok());
    assert!(plugin_manifest_digests_match(
        VALID_PLUGIN_DIGEST,
        VALID_PLUGIN_DIGEST_BARE
    ));
    assert!(plugin_manifest_digests_match(
        "sha256:ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789",
        "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
    ));
    assert!(validate_plugin_manifest_digest("repo-review", "sha256:too-short").is_err());
    assert!(
        validate_plugin_manifest_digest(
            "repo-review",
            "sha256:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_err()
    );
    assert!(!plugin_manifest_digests_match(
        "sha256:not-a-digest",
        "sha256:not-a-digest"
    ));
}

#[test]
fn plugin_version_validation_rejects_path_like_or_unreviewable_versions() {
    assert!(validate_plugin_version("repo-review", "0.1.0").is_ok());
    assert!(validate_plugin_version("repo-review", "2026.06.29-alpha").is_ok());
    assert!(validate_plugin_version("repo-review", " ").is_err());
    assert!(validate_plugin_version("repo-review", "bad version").is_err());
    assert!(validate_plugin_version("repo-review", "../0.1.0").is_err());
    assert!(validate_plugin_version("repo-review", "0.1.0\nnext").is_err());
}

#[test]
fn plugin_manifest_snapshot_and_trust_entries_roundtrip() -> Result<()> {
    let snapshot = sample_snapshot();
    let trust = sample_trust(PluginTrustDecision::Trusted);

    snapshot.validate()?;
    trust.validate()?;
    assert!(trust.matches_snapshot(&snapshot));

    let captured = SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(snapshot.clone()));
    let trusted = SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trust.clone()));
    let captured_json = serde_json::to_string(&captured)?;
    let trusted_json = serde_json::to_string(&trusted)?;

    assert!(captured_json.contains("plugin_manifest_captured"));
    assert!(captured_json.contains("manifest_hash"));
    assert!(trusted_json.contains("plugin_trust_decision"));
    assert!(trusted_json.contains("reviewed_at_ms"));

    let restored_captured: SessionLogEntry = serde_json::from_str(&captured_json)?;
    let restored_trusted: SessionLogEntry = serde_json::from_str(&trusted_json)?;

    assert!(matches!(
        restored_captured,
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(restored))
            if restored == snapshot
    ));
    assert!(matches!(
        restored_trusted,
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(restored))
            if restored == trust
    ));
    Ok(())
}

#[test]
fn plugin_control_entries_accept_legacy_pascal_case_aliases() -> Result<()> {
    let captured_json = r#"{"control":{"PluginManifestCaptured":{"plugin_id":"repo-review","name":"Repository Review","version":"0.1.0","manifest_path":".sigil/plugins/repo-review/plugin.toml","manifest_hash":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","capabilities":[{"kind":"skill","path":"skills/review/SKILL.md"}],"trust":"needs_review"}}}"#;
    let trusted_json = r#"{"control":{"PluginTrustDecision":{"plugin_id":"repo-review","manifest_path":".sigil/plugins/repo-review/plugin.toml","manifest_hash":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","decision":"trusted","reviewed_at_ms":42}}}"#;
    let restored_captured: SessionLogEntry = serde_json::from_str(captured_json)?;
    let restored_trusted: SessionLogEntry = serde_json::from_str(trusted_json)?;

    assert!(matches!(
        restored_captured,
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(snapshot))
            if snapshot.plugin_id == "repo-review"
                && snapshot.capabilities.len() == 1
                && snapshot.trust == PluginTrustDecision::NeedsReview
    ));
    assert!(matches!(
        restored_trusted,
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(entry))
            if entry.plugin_id == "repo-review"
                && entry.decision == PluginTrustDecision::Trusted
                && entry.reviewed_at_ms == 42
    ));
    Ok(())
}

#[test]
fn plugin_state_projection_tracks_manifest_and_matching_trust() {
    let snapshot = sample_snapshot();
    let trusted = sample_trust(PluginTrustDecision::Trusted);
    let projection = PluginStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(snapshot.clone())),
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trusted.clone())),
    ]);

    let latest_manifest = projection
        .latest_manifest()
        .expect("latest manifest should exist");
    let latest_trust = projection
        .latest_trust()
        .expect("latest trust should exist");

    assert_eq!(latest_manifest.plugin_id, "repo-review");
    assert_eq!(latest_manifest.trust, PluginTrustDecision::Trusted);
    assert_eq!(latest_trust, &trusted);
    assert_eq!(projection.manifest_replay_order, vec!["repo-review"]);
    assert_eq!(projection.trust_replay_order, vec!["repo-review"]);
}

#[test]
fn plugin_state_projection_ignores_manifest_snapshot_trust_without_entry() {
    let mut snapshot = sample_snapshot();
    snapshot.trust = PluginTrustDecision::Trusted;
    let projection = PluginStateProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::PluginManifestCaptured(snapshot),
    )]);

    let latest_manifest = projection
        .latest_manifest()
        .expect("latest manifest should exist");

    assert_eq!(latest_manifest.trust, PluginTrustDecision::NeedsReview);
    assert!(projection.latest_trust().is_none());
}

#[test]
fn plugin_state_projection_does_not_apply_trust_for_changed_manifest_hash() {
    let mut snapshot = sample_snapshot();
    snapshot.manifest_hash =
        "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210".to_owned();
    snapshot.trust = PluginTrustDecision::Trusted;
    let trusted = sample_trust(PluginTrustDecision::Trusted);
    let projection = PluginStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trusted)),
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(snapshot)),
    ]);

    let latest_manifest = projection
        .latest_manifest()
        .expect("latest manifest should exist");

    assert_eq!(latest_manifest.trust, PluginTrustDecision::NeedsReview);
}

#[test]
fn plugin_state_projection_invalidates_trust_when_version_changes_even_with_same_digest() {
    let trusted = sample_trust(PluginTrustDecision::Trusted);
    let mut changed = sample_snapshot();
    changed.version = "0.2.0".to_owned();

    let projection = PluginStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trusted)),
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(changed)),
    ]);

    let latest_manifest = projection
        .latest_manifest()
        .expect("latest manifest should exist");
    assert_eq!(latest_manifest.trust, PluginTrustDecision::NeedsReview);
}

#[test]
fn plugin_state_projection_invalidates_trust_when_capabilities_change_even_with_same_digest() {
    let trusted = sample_trust(PluginTrustDecision::Trusted);
    let mut changed = sample_snapshot();
    changed.capabilities.push(PluginCapability::Skill {
        path: "skills/extra/SKILL.md".into(),
    });

    let projection = PluginStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trusted)),
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(changed)),
    ]);

    let latest_manifest = projection
        .latest_manifest()
        .expect("latest manifest should exist");
    assert_eq!(latest_manifest.trust, PluginTrustDecision::NeedsReview);
}

#[test]
fn plugin_state_projection_invalidates_trust_when_hook_contract_changes() {
    let trusted = sample_trust(PluginTrustDecision::Trusted);
    let mut changed = sample_snapshot();
    let PluginCapability::Hook {
        command,
        declared_effect,
        timeout_ms,
        output_schema_digest,
        ..
    } = &mut changed.capabilities[2]
    else {
        panic!("sample capability should be a hook");
    };
    *command = "scripts/new-policy.sh".to_owned();
    *declared_effect = ToolEffect::WorkspaceWrite;
    *timeout_ms = 60_000;
    *output_schema_digest = Some(VALID_PLUGIN_DIGEST.to_owned());

    let projection = PluginStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trusted)),
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(changed)),
    ]);

    let latest_manifest = projection
        .latest_manifest()
        .expect("latest manifest should exist");
    assert_eq!(latest_manifest.trust, PluginTrustDecision::NeedsReview);
}
