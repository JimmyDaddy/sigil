use anyhow::Result;

use crate::{
    ApprovalMode, ControlEntry, McpServerConfig, McpServerStartup, PluginAgentRef,
    PluginCapability, PluginHookRef, PluginManifest, PluginManifestSnapshot, PluginSkillRef,
    PluginStateProjection, PluginTrustDecision, PluginTrustEntry, SessionLogEntry,
    validate_plugin_id,
};

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
            event: "pre_tool_use".to_owned(),
            command: "scripts/check-tool-policy.sh".to_owned(),
            args: vec!["--strict".to_owned()],
            approval: ApprovalMode::Ask,
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
        manifest_hash: "sha256:manifest".to_owned(),
        capabilities: manifest.capabilities(),
        trust: PluginTrustDecision::NeedsReview,
    }
}

fn sample_trust(decision: PluginTrustDecision) -> PluginTrustEntry {
    PluginTrustEntry {
        plugin_id: "repo-review".to_owned(),
        manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
        manifest_hash: "sha256:manifest".to_owned(),
        decision,
        reviewed_at_ms: 42,
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
            event,
            command,
            args,
            approval,
        } if event == "pre_tool_use"
            && command == "scripts/check-tool-policy.sh"
            && args == &vec!["--strict".to_owned()]
            && *approval == ApprovalMode::Ask
    ));
    assert!(matches!(
        &capabilities[3],
        PluginCapability::McpServer {
            name,
            command,
            args,
            startup,
            required,
        } if name == "repo-tools"
            && command == "node"
            && args == &vec!["server.js".to_owned()]
            && *startup == McpServerStartup::Lazy
            && !*required
    ));
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

    let mut empty_hook = sample_manifest();
    empty_hook.hooks[0].command = "  ".to_owned();
    assert!(empty_hook.validate().is_err());

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
            event: " ".to_owned(),
            command: "scripts/hook.sh".to_owned(),
            args: Vec::new(),
            approval: ApprovalMode::Ask,
        }
        .validate()
        .is_err()
    );
    assert!(
        PluginCapability::Hook {
            event: "pre_tool_use".to_owned(),
            command: " ".to_owned(),
            args: Vec::new(),
            approval: ApprovalMode::Ask,
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
        }
        .validate()
        .is_err()
    );

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.version.clear();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.manifest_path.clear();
    assert!(invalid_snapshot.validate().is_err());

    let mut invalid_snapshot = sample_snapshot();
    invalid_snapshot.manifest_hash = " ".to_owned();
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
    let captured_json = r#"{"control":{"PluginManifestCaptured":{"plugin_id":"repo-review","name":"Repository Review","version":"0.1.0","manifest_path":".sigil/plugins/repo-review/plugin.toml","manifest_hash":"sha256:manifest","capabilities":[{"kind":"skill","path":"skills/review/SKILL.md"}],"trust":"needs_review"}}}"#;
    let trusted_json = r#"{"control":{"PluginTrustDecision":{"plugin_id":"repo-review","manifest_path":".sigil/plugins/repo-review/plugin.toml","manifest_hash":"sha256:manifest","decision":"trusted","reviewed_at_ms":42}}}"#;
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
    snapshot.manifest_hash = "sha256:new-manifest".to_owned();
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
