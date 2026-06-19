use std::collections::BTreeSet;

use anyhow::Result;

use super::{
    SkillDescriptor, SkillIndexSnapshot, SkillLoadEntry, SkillRunMode, SkillSource,
    SkillStateProjection, SkillTrustState,
};
use crate::{ControlEntry, SessionLogEntry, ToolRegistryScope};

#[test]
fn skill_index_snapshot_sorts_descriptors_and_hashes_deterministically() -> Result<()> {
    let first = sample_descriptor("repo-review", SkillSource::Workspace);
    let second = sample_descriptor(
        "deploy",
        SkillSource::Plugin {
            plugin_id: "ops".to_owned(),
        },
    );
    let third = sample_descriptor("explain", SkillSource::User);
    let snapshot_a = SkillIndexSnapshot::new(vec![second.clone(), third.clone(), first.clone()])?;
    let snapshot_b = SkillIndexSnapshot::new(vec![first.clone(), second.clone(), third])?;

    assert_eq!(snapshot_a.fingerprint, snapshot_b.fingerprint);
    assert_ne!(snapshot_a.fingerprint, "none");
    assert_eq!(snapshot_a.descriptors[0].id, "deploy");
    assert_eq!(snapshot_a.descriptors[1].id, "explain");
    assert_eq!(snapshot_a.descriptors[2].id, "repo-review");

    let changed = SkillIndexSnapshot::new(vec![SkillDescriptor {
        description: "different".to_owned(),
        ..first.clone()
    }])?;
    assert_ne!(snapshot_a.fingerprint, changed.fingerprint);

    let path_only_change = SkillIndexSnapshot::new(vec![SkillDescriptor {
        root: "other/root".into(),
        entrypoint: "other/root/SKILL.md".into(),
        allowed_tools: ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        disallowed_tools: ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        path_patterns: vec!["other/**".to_owned()],
        ..first
    }])?;
    let original_single = SkillIndexSnapshot::new(vec![sample_descriptor(
        "repo-review",
        SkillSource::Workspace,
    )])?;
    assert_eq!(path_only_change.fingerprint, original_single.fingerprint);
    Ok(())
}

#[test]
fn skill_index_snapshot_refresh_fingerprint_sorts_and_rehashes() -> Result<()> {
    let mut snapshot = SkillIndexSnapshot {
        descriptors: vec![
            sample_descriptor("repo-review", SkillSource::Workspace),
            sample_descriptor("deploy", SkillSource::User),
        ],
        fingerprint: "stale".to_owned(),
    };

    snapshot.refresh_fingerprint()?;

    assert_ne!(snapshot.fingerprint, "stale");
    assert_eq!(snapshot.descriptors[0].id, "deploy");
    Ok(())
}

#[test]
fn skill_index_snapshot_empty_fingerprint_is_none() -> Result<()> {
    let snapshot = SkillIndexSnapshot::new(Vec::new())?;

    assert_eq!(snapshot.fingerprint, "none");
    assert!(snapshot.descriptors.is_empty());
    Ok(())
}

#[test]
fn skill_control_entries_roundtrip_with_snake_case_payloads() -> Result<()> {
    let snapshot = SkillIndexSnapshot::new(vec![sample_descriptor(
        "repo-review",
        SkillSource::Workspace,
    )])?;
    let loaded = sample_load_entry("repo-review");
    let captured = SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(snapshot.clone()));
    let loaded_entry = SessionLogEntry::Control(ControlEntry::SkillLoaded(loaded.clone()));

    let captured_json = serde_json::to_string(&captured)?;
    let loaded_json = serde_json::to_string(&loaded_entry)?;
    let restored_captured: SessionLogEntry = serde_json::from_str(&captured_json)?;
    let restored_loaded: SessionLogEntry = serde_json::from_str(&loaded_json)?;

    assert!(captured_json.contains("skill_index_captured"));
    assert!(captured_json.contains("model_invocable"));
    assert!(loaded_json.contains("skill_loaded"));
    assert!(loaded_json.contains("byte_count"));
    assert!(matches!(
        restored_captured,
        SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(restored))
            if restored == snapshot
    ));
    assert!(matches!(
        restored_loaded,
        SessionLogEntry::Control(ControlEntry::SkillLoaded(restored))
            if restored == loaded
    ));
    Ok(())
}

#[test]
fn skill_control_entries_accept_legacy_pascal_case_aliases() -> Result<()> {
    let captured_json = r#"{"control":{"SkillIndexCaptured":{"descriptors":[{"id":"repo-review","name":"Repo Review","description":"Review code","root":".sigil/skills/repo-review","entrypoint":".sigil/skills/repo-review/SKILL.md","source":{"kind":"workspace"},"sha256":"hash","enabled":true,"trust":"trusted","model_invocable":true,"user_invocable":false,"run_as":"inline","allowed_tools":{"names":["read_file"],"prefixes":[]},"disallowed_tools":{"names":[],"prefixes":[]},"path_patterns":["crates/**"]}],"fingerprint":"legacy-fingerprint"}}}"#;
    let loaded_json = r#"{"control":{"SkillLoaded":{"skill_id":"repo-review","sha256":"hash","source":{"kind":"workspace"},"entrypoint":".sigil/skills/repo-review/SKILL.md","byte_count":128,"line_count":7,"loaded_at_ms":42}}}"#;
    let restored_captured: SessionLogEntry = serde_json::from_str(captured_json)?;
    let restored_loaded: SessionLogEntry = serde_json::from_str(loaded_json)?;

    assert!(matches!(
        restored_captured,
        SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(snapshot))
            if snapshot.descriptors[0].id == "repo-review"
                && snapshot.descriptors[0].trust == SkillTrustState::Trusted
                && !snapshot.descriptors[0].user_invocable
                && snapshot.fingerprint != "legacy-fingerprint"
    ));
    assert!(matches!(
        restored_loaded,
        SessionLogEntry::Control(ControlEntry::SkillLoaded(entry))
            if entry.skill_id == "repo-review"
                && entry.byte_count == 128
                && entry.line_count == 7
    ));
    Ok(())
}

#[test]
fn skill_descriptor_missing_optional_fields_defaults_safely() -> Result<()> {
    let descriptor: SkillDescriptor = serde_json::from_str(r#"{"id":"repo-review"}"#)?;

    assert_eq!(descriptor.id, "repo-review");
    assert!(descriptor.enabled);
    assert!(descriptor.model_invocable);
    assert!(descriptor.user_invocable);
    assert_eq!(descriptor.source, SkillSource::Workspace);
    assert_eq!(descriptor.trust, SkillTrustState::NeedsReview);
    assert_eq!(descriptor.run_as, SkillRunMode::Inline);
    assert!(descriptor.allowed_tools.is_empty());
    assert!(descriptor.disallowed_tools.is_empty());
    Ok(())
}

#[test]
fn skill_index_snapshot_fingerprint_is_recomputed_on_restore() -> Result<()> {
    let restored: SkillIndexSnapshot = serde_json::from_str(
        r#"{"descriptors":[{"id":"repo-review","name":"Repo Review","description":"Review code"}],"fingerprint":"legacy-fingerprint"}"#,
    )?;
    let expected = SkillIndexSnapshot::new(vec![SkillDescriptor {
        id: "repo-review".to_owned(),
        name: "Repo Review".to_owned(),
        description: "Review code".to_owned(),
        ..serde_json::from_str::<SkillDescriptor>(r#"{"id":"repo-review"}"#)?
    }])?;

    assert_eq!(restored.fingerprint, expected.fingerprint);
    assert_ne!(restored.fingerprint, "legacy-fingerprint");
    Ok(())
}

#[test]
fn skill_state_projection_tracks_latest_index_and_loaded_entries() -> Result<()> {
    let first_snapshot = SkillIndexSnapshot::new(vec![sample_descriptor(
        "repo-review",
        SkillSource::Workspace,
    )])?;
    let second_snapshot =
        SkillIndexSnapshot::new(vec![sample_descriptor("deploy", SkillSource::User)])?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "ignored".to_owned(),
            data: serde_json::Value::Null,
        }),
        SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(first_snapshot)),
        SessionLogEntry::Control(ControlEntry::SkillLoaded(sample_load_entry("repo-review"))),
        SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(second_snapshot.clone())),
        SessionLogEntry::Control(ControlEntry::SkillLoaded(sample_load_entry("deploy"))),
    ];

    let projection = SkillStateProjection::from_entries(&entries);
    let latest_loaded = projection.latest_loaded().expect("latest loaded skill");

    assert_eq!(projection.latest_index, Some(second_snapshot));
    assert_eq!(
        projection.load_replay_order,
        vec!["repo-review".to_owned(), "deploy".to_owned()]
    );
    assert_eq!(projection.latest_loaded_skill_id.as_deref(), Some("deploy"));
    assert_eq!(latest_loaded.entry.skill_id, "deploy");
    assert!(projection.loaded_skills.contains_key("repo-review"));
    Ok(())
}

#[test]
fn skill_labels_are_stable() {
    assert_eq!(SkillSource::Workspace.as_str(), "workspace");
    assert_eq!(SkillSource::User.as_str(), "user");
    assert_eq!(
        SkillSource::Plugin {
            plugin_id: "ops".to_owned(),
        }
        .as_str(),
        "plugin"
    );
    assert_eq!(SkillTrustState::Trusted.as_str(), "trusted");
    assert_eq!(SkillTrustState::NeedsReview.as_str(), "needs_review");
    assert_eq!(SkillTrustState::Disabled.as_str(), "disabled");
    assert_eq!(SkillRunMode::Inline.as_str(), "inline");
    assert_eq!(SkillRunMode::ChildSession.as_str(), "child_session");
}

fn sample_descriptor(id: &str, source: SkillSource) -> SkillDescriptor {
    SkillDescriptor {
        id: id.to_owned(),
        name: title(id),
        description: "Reusable workflow".to_owned(),
        when_to_use: Some("Use when the repository needs this workflow.".to_owned()),
        root: format!(".sigil/skills/{id}").into(),
        entrypoint: format!(".sigil/skills/{id}/SKILL.md").into(),
        source,
        sha256: format!("sha256-{id}"),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::Inline,
        agent: None,
        argument_hint: Some("<path>".to_owned()),
        allowed_tools: ToolRegistryScope {
            allow_all: false,
            names: BTreeSet::from(["read_file".to_owned()]),
            prefixes: Vec::new(),
        },
        disallowed_tools: ToolRegistryScope {
            allow_all: false,
            names: BTreeSet::from(["bash".to_owned()]),
            prefixes: Vec::new(),
        },
        path_patterns: vec!["crates/**".to_owned()],
    }
}

fn sample_load_entry(skill_id: &str) -> SkillLoadEntry {
    SkillLoadEntry {
        skill_id: skill_id.to_owned(),
        sha256: format!("sha256-{skill_id}"),
        source: SkillSource::Workspace,
        entrypoint: format!(".sigil/skills/{skill_id}/SKILL.md").into(),
        run_id: Some("run-1".to_owned()),
        call_id: Some("call-1".to_owned()),
        byte_count: 128,
        line_count: 7,
        loaded_at_ms: 42,
    }
}

fn title(id: &str) -> String {
    id.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
