use std::path::PathBuf;

use anyhow::{Context, Result};

use super::*;
use crate::{IntentCriterionId, IntentId, JsonlSessionStore};

fn canonical_fixture() -> Result<(
    VerificationPolicy,
    CheckSpec,
    CheckSpec,
    WorkspaceSnapshotManifestV1,
)> {
    let check_without_intent = CheckSpec::new(
        "check-no-intent",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned(), "--locked".to_owned()],
            cwd: Some(PathBuf::from("crates/sigil-kernel")),
        },
        ToolEffect::ReadOnly,
        "scope-plain",
    );
    let check_with_intent = CheckSpec::new(
        "check-验证",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "--package".to_owned(),
                "sigil-kernel\nquote=\\\" slash=\\\\".to_owned(),
            ],
            cwd: Some(PathBuf::from("crates/组件")),
        },
        ToolEffect::ReadOnly,
        "scope-验证",
    )
    .with_intent_scope(
        IntentVersionRef::new(IntentId::new("intent-verify")?, u64::MAX)?,
        vec![
            IntentCriterionId::new("criterion-beta")?,
            IntentCriterionId::new("criterion-alpha")?,
        ],
    )?;
    let policy = VerificationPolicy {
        required_checks: vec![check_with_intent.clone()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope {
            scope_hash: "scope-验证\nquote=\\\" slash=\\\\".to_owned(),
            include: vec!["src/**".to_owned(), "文档/**".to_owned()],
            exclude: vec!["target/**".to_owned(), "临时/**".to_owned()],
            tracked_files_only: false,
            max_file_bytes: u64::MAX,
            generated_roots: vec![PathBuf::from("生成/目录"), PathBuf::from("缓存")],
        },
        sandbox_profile: SandboxProfileRequirement::Sandboxed,
        workspace_trust_requirement: WorkspaceTrustRequirement::Trusted,
        allow_unverified_completion: false,
        timeout_ms: Some(u64::MAX),
        auto_run: VerificationAutoRunPolicy::TrustedOnly,
    };
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id: "workspace-验证".to_owned(),
        scope_hash: "scope-验证\nquote=\\\" slash=\\\\".to_owned(),
        entries: vec![
            WorkspaceSnapshotEntry {
                normalized_path: PathBuf::from("链接/目录"),
                file_type: FileType::Symlink,
                content_hash: None,
                mode: Some(u32::MAX),
                file_metadata: Some(FileMetadataEvidence {
                    platform: FileMetadataPlatform::Unix,
                    readonly: true,
                    unix_mode: Some(u32::MAX),
                }),
                symlink_target: Some(PathBuf::from("目标/文件")),
                state: SnapshotEntryState::Present,
            },
            WorkspaceSnapshotEntry {
                normalized_path: PathBuf::from("src/界.rs"),
                file_type: FileType::File,
                content_hash: Some("sha256:内容-验证".to_owned()),
                mode: Some(u32::MAX),
                file_metadata: Some(FileMetadataEvidence {
                    platform: FileMetadataPlatform::Unix,
                    readonly: false,
                    unix_mode: Some(u32::MAX),
                }),
                symlink_target: None,
                state: SnapshotEntryState::Present,
            },
            WorkspaceSnapshotEntry {
                normalized_path: PathBuf::from("missing/不存在.rs"),
                file_type: FileType::File,
                content_hash: None,
                mode: Some(0),
                file_metadata: Some(FileMetadataEvidence {
                    platform: FileMetadataPlatform::Other,
                    readonly: false,
                    unix_mode: None,
                }),
                symlink_target: None,
                state: SnapshotEntryState::Missing,
            },
        ],
    };
    Ok((policy, check_without_intent, check_with_intent, manifest))
}

const POLICY_CANONICAL_JSON: &str = r#"{"allow_unverified_completion":false,"auto_run":"trusted_only","completion_criteria":"all_required_checks","required_checks":[{"check_spec_hash":"sha256:c8415c8a15db1715ab6872bab12b6cac975a2d8e86195f21d17a399d53ac01ba","check_spec_id":"check-验证","command":{"args":["test","--package","sigil-kernel\nquote=\\\" slash=\\\\"],"command":"cargo","cwd":"crates/组件"},"effect":"read_only","intent_scopes":[{"criterion_ids":["criterion-alpha","criterion-beta"],"intent_ref":{"intent_id":"intent-verify","version":18446744073709551615}}],"verification_scope_hash":"scope-验证"}],"sandbox_profile":"sandboxed","timeout_ms":18446744073709551615,"verification_scope":{"exclude":["target/**","临时/**"],"generated_roots":["生成/目录","缓存"],"include":["src/**","文档/**"],"max_file_bytes":18446744073709551615,"scope_hash":"scope-验证\nquote=\\\" slash=\\\\","tracked_files_only":false},"workspace_trust_requirement":"trusted"}"#;
const INTENT_SCOPE_CANONICAL_JSON: &str = r#"[{"criterion_ids":["criterion-alpha","criterion-beta"],"intent_ref":{"intent_id":"intent-verify","version":18446744073709551615}}]"#;
const MANIFEST_CANONICAL_JSON: &str = r#"{"entries":[{"file_metadata":{"platform":"unix","readonly":true,"unix_mode":4294967295},"file_type":"symlink","mode":4294967295,"normalized_path":"链接/目录","state":"present","symlink_target":"目标/文件"},{"content_hash":"sha256:内容-验证","file_metadata":{"platform":"unix","readonly":false,"unix_mode":4294967295},"file_type":"file","mode":4294967295,"normalized_path":"src/界.rs","state":"present"},{"file_metadata":{"platform":"other","readonly":false},"file_type":"file","mode":0,"normalized_path":"missing/不存在.rs","state":"missing"}],"scope_hash":"scope-验证\nquote=\\\" slash=\\\\","workspace_id":"workspace-验证"}"#;
const POLICY_HASH: &str =
    "sha256:jcs-v1:48c324990c6846e7c060abf77afb48328dcb1756dab2c89ab2dc42662e932efa";
const CHECK_WITHOUT_INTENT_HASH: &str =
    "sha256:2442efa05d17970f2951c693a57dd19d7219743e6f33d7dc71d1473409e259a8";
const CHECK_WITH_INTENT_HASH: &str =
    "sha256:c8415c8a15db1715ab6872bab12b6cac975a2d8e86195f21d17a399d53ac01ba";
const SNAPSHOT_ID: &str =
    "sha256:jcs-v1:53817dd8f6845eb8c5dd06401f71376b620813da61b59a785581b66f9c357a90";

#[test]
fn verification_canonical_fixed_typed_dto_bytes_and_hashes() -> Result<()> {
    let (policy, check_without_intent, check_with_intent, manifest) = canonical_fixture()?;
    let legacy_policy: VerificationPolicy = serde_json::from_str(POLICY_CANONICAL_JSON)?;
    let legacy_manifest: WorkspaceSnapshotManifestV1 =
        serde_json::from_str(MANIFEST_CANONICAL_JSON)?;

    check_without_intent.validate_shape()?;
    check_with_intent.validate_shape()?;
    assert_eq!(legacy_policy, policy);
    legacy_policy
        .required_checks
        .first()
        .context("legacy policy fixture has no check spec")?
        .validate_shape()?;

    assert_eq!(
        crate::event::canonical_json_bytes(&serde_json::to_value(&policy)?)?,
        POLICY_CANONICAL_JSON.as_bytes()
    );
    assert_eq!(
        crate::event::canonical_json_bytes(&serde_json::to_value(
            &check_with_intent.intent_scopes
        )?)?,
        INTENT_SCOPE_CANONICAL_JSON.as_bytes()
    );
    assert_eq!(
        crate::event::canonical_json_bytes(&serde_json::to_value(&manifest)?)?,
        MANIFEST_CANONICAL_JSON.as_bytes()
    );

    assert_eq!(policy.stable_hash()?, POLICY_HASH);
    assert_eq!(legacy_policy.stable_hash()?, POLICY_HASH);
    assert_eq!(
        check_without_intent.check_spec_hash,
        CHECK_WITHOUT_INTENT_HASH
    );
    assert_eq!(check_with_intent.check_spec_hash, CHECK_WITH_INTENT_HASH);
    assert_eq!(manifest.workspace_snapshot_id()?, SNAPSHOT_ID);
    assert_eq!(legacy_manifest.workspace_snapshot_id()?, SNAPSHOT_ID);
    Ok(())
}

#[test]
fn verification_canonical_current_schema_jsonl_reopen_preserves_receipt_hashes() -> Result<()> {
    let (policy, _, check_with_intent, manifest) = canonical_fixture()?;
    let policy_entry = VerificationPolicyChangedEntry::new(
        EvidenceScope::Task("task-canonical".to_owned()),
        policy,
        "event-policy-canonical",
    )?;
    let check_entry = CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task-canonical".to_owned()),
        TrustedCheckSpec {
            check_spec: check_with_intent,
            source: CheckDiscoverySource::UserExplicitConfig,
            workspace_trust_snapshot_id: "trust-canonical".to_owned(),
            promoted_by: CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config-canonical".to_owned(),
            },
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        "event-check-canonical",
    );
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("verification-canonical.jsonl"))?;
    let mut session = Session::load_from_store("provider", "model", store.clone())?;
    session.append_control(ControlEntry::VerificationPolicyChanged(policy_entry))?;
    session.append_control(ControlEntry::CheckSpecRecorded(check_entry))?;
    // Snapshots have no standalone store; the manifest is intentionally visible as a durable,
    // non-critical control payload and is revalidated through the ordinary JSONL reader.
    session.append_control(ControlEntry::Note {
        kind: "verification_canonical_snapshot_manifest".to_owned(),
        data: serde_json::to_value(&manifest)?,
    })?;
    drop(session);

    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(records.len() >= 4);
    let reloaded = Session::load_from_store("provider", "model", store)?;
    let mut reloaded_policy_hash = None;
    let mut reloaded_check_hash = None;
    let mut reloaded_manifest = None;
    for entry in reloaded.entries() {
        match entry {
            SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(entry)) => {
                reloaded_policy_hash = Some(entry.policy_hash.as_str());
                assert_eq!(entry.policy.stable_hash()?, POLICY_HASH);
            }
            SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(entry)) => {
                entry.trusted_check.check_spec.validate_shape()?;
                reloaded_check_hash = Some(entry.trusted_check.check_spec.check_spec_hash.as_str());
            }
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "verification_canonical_snapshot_manifest" =>
            {
                reloaded_manifest = Some(serde_json::from_value::<WorkspaceSnapshotManifestV1>(
                    data.clone(),
                )?);
            }
            _ => {}
        }
    }

    assert_eq!(
        reloaded_policy_hash.context("policy receipt was not replayed")?,
        POLICY_HASH
    );
    assert_eq!(
        reloaded_check_hash.context("check spec receipt was not replayed")?,
        CHECK_WITH_INTENT_HASH
    );
    assert_eq!(
        reloaded_manifest
            .context("embedded snapshot manifest was not replayed")?
            .workspace_snapshot_id()?,
        SNAPSHOT_ID
    );
    Ok(())
}
