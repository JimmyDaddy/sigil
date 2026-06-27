use anyhow::Result;

use super::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetProjection, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk,
    ChangeSetValidation, ChangeSetValidationKind, ChangeSetValidationStatus,
};
use crate::{ControlEntry, SessionLogEntry};

#[test]
fn changeset_id_accepts_stable_values_and_rejects_path_unsafe_values() {
    assert_eq!(
        ChangeSetId::new("change-1").expect("valid id").as_str(),
        "change-1"
    );
    assert!(ChangeSetId::new("").is_err());
    assert!(ChangeSetId::new("..").is_err());
    assert!(ChangeSetId::new("dir/change").is_err());
    assert!(ChangeSetId::new("change 1").is_err());
    assert!(serde_json::from_str::<ChangeSetId>(r#""dir/change""#).is_err());
}

#[test]
fn changeset_control_entries_roundtrip_with_snake_case_payloads() -> Result<()> {
    let proposed = SessionLogEntry::Control(ControlEntry::ChangeSetProposed(sample_change_set()));
    let result = SessionLogEntry::Control(ControlEntry::ChangeSetApplied(sample_result(
        ChangeSetResultStatus::Applied,
    )));

    let proposed_json = serde_json::to_string(&proposed)?;
    let result_json = serde_json::to_string(&result)?;
    let restored_proposed: SessionLogEntry = serde_json::from_str(&proposed_json)?;
    let restored_result: SessionLogEntry = serde_json::from_str(&result_json)?;

    assert!(proposed_json.contains("change_set_proposed"));
    assert!(proposed_json.contains("before_hash"));
    assert!(result_json.contains("change_set_applied"));
    assert!(result_json.contains("file_results"));
    assert!(matches!(
        restored_proposed,
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(change_set))
            if change_set.id.as_str() == "change-1"
                && change_set.files[0].action == ChangeSetFileAction::Update
    ));
    assert!(matches!(
        restored_result,
        SessionLogEntry::Control(ControlEntry::ChangeSetApplied(result))
            if result.id.as_str() == "change-1"
                && result.status == ChangeSetResultStatus::Applied
    ));
    Ok(())
}

#[test]
fn changeset_control_entries_accept_legacy_pascal_case_aliases() -> Result<()> {
    let proposed_json = r#"{"control":{"ChangeSetProposed":{"id":"change-1","title":"Update README","summary":"Update project overview","risk":"low","files":[],"validations":[]}}}"#;
    let result_json = r#"{"control":{"ChangeSetApplied":{"id":"change-1","status":"applied","file_results":[]}}}"#;
    let restored_proposed: SessionLogEntry = serde_json::from_str(proposed_json)?;
    let restored_result: SessionLogEntry = serde_json::from_str(result_json)?;

    assert!(matches!(
        restored_proposed,
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(change_set))
            if change_set.id.as_str() == "change-1"
    ));
    assert!(matches!(
        restored_result,
        SessionLogEntry::Control(ControlEntry::ChangeSetApplied(result))
            if result.id.as_str() == "change-1"
                && result.status == ChangeSetResultStatus::Applied
    ));
    Ok(())
}

#[test]
fn changeset_projection_replays_proposal_and_result() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(sample_change_set())),
        SessionLogEntry::Control(ControlEntry::ChangeSetApplied(sample_result(
            ChangeSetResultStatus::Applied,
        ))),
    ];

    let projection = ChangeSetProjection::from_entries(&entries);
    let latest = projection.latest().expect("latest changeset");

    assert_eq!(
        projection.replay_order,
        vec![change_set_id(), change_set_id()]
    );
    assert_eq!(
        projection.latest_change_set_id.as_ref(),
        Some(&change_set_id())
    );
    assert!(latest.proposal.is_some());
    assert!(matches!(
        latest.result.as_ref(),
        Some(result) if result.status == ChangeSetResultStatus::Applied
    ));
}

#[test]
fn changeset_projection_keeps_result_without_prior_proposal() {
    let result = sample_result(ChangeSetResultStatus::Failed);
    let projection = ChangeSetProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::ChangeSetApplied(result),
    )]);
    let latest = projection.latest().expect("latest changeset");

    assert!(latest.proposal.is_none());
    assert!(matches!(
        latest.result.as_ref(),
        Some(result) if result.status == ChangeSetResultStatus::Failed
    ));
}

#[test]
fn changeset_projection_ignores_unrelated_control_entries() {
    let projection = ChangeSetProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "unrelated".to_owned(),
            data: serde_json::json!({"ignored": true}),
        }),
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(sample_change_set())),
    ]);

    let latest = projection.latest().expect("latest changeset");
    assert_eq!(
        projection.latest_change_set_id.as_ref(),
        Some(&change_set_id())
    );
    assert!(latest.proposal.is_some());
    assert!(latest.result.is_none());
}

#[test]
fn changeset_projection_new_proposal_supersedes_prior_result_for_same_id() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(sample_change_set())),
        SessionLogEntry::Control(ControlEntry::ChangeSetApplied(sample_result(
            ChangeSetResultStatus::Applied,
        ))),
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(ChangeSet {
            summary: "updated summary".to_owned(),
            ..sample_change_set()
        })),
    ];

    let projection = ChangeSetProjection::from_entries(&entries);
    let latest = projection.latest().expect("latest changeset");

    assert_eq!(
        projection.replay_order,
        vec![change_set_id(), change_set_id(), change_set_id()]
    );
    assert_eq!(
        latest
            .proposal
            .as_ref()
            .map(|proposal| proposal.summary.as_str()),
        Some("updated summary")
    );
    assert!(latest.result.is_none());
}

#[test]
fn changeset_labels_are_stable() {
    assert_eq!(ChangeSetFileAction::Create.as_str(), "create");
    assert_eq!(ChangeSetFileAction::Update.as_str(), "update");
    assert_eq!(ChangeSetFileAction::Delete.as_str(), "delete");
    assert_eq!(ChangeSetFileAction::Rename.as_str(), "rename");
    assert_eq!(ChangeSetRisk::Low.as_str(), "low");
    assert_eq!(ChangeSetRisk::Medium.as_str(), "medium");
    assert_eq!(ChangeSetRisk::High.as_str(), "high");
    assert_eq!(ChangeSetValidationKind::Path.as_str(), "path");
    assert_eq!(ChangeSetValidationKind::Hash.as_str(), "hash");
    assert_eq!(ChangeSetValidationKind::Mtime.as_str(), "mtime");
    assert_eq!(ChangeSetValidationKind::Snippet.as_str(), "snippet");
    assert_eq!(ChangeSetValidationKind::Symlink.as_str(), "symlink");
    assert_eq!(ChangeSetValidationKind::Binary.as_str(), "binary");
    assert_eq!(ChangeSetValidationKind::Permission.as_str(), "permission");
    assert_eq!(ChangeSetValidationKind::Custom.as_str(), "custom");
    assert_eq!(ChangeSetValidationStatus::Pending.as_str(), "pending");
    assert_eq!(ChangeSetValidationStatus::Passed.as_str(), "passed");
    assert_eq!(ChangeSetValidationStatus::Failed.as_str(), "failed");
    assert_eq!(ChangeSetValidationStatus::Skipped.as_str(), "skipped");
    assert_eq!(ChangeSetResultStatus::Applied.as_str(), "applied");
    assert_eq!(
        ChangeSetResultStatus::PartiallyApplied.as_str(),
        "partially_applied"
    );
    assert_eq!(ChangeSetResultStatus::Failed.as_str(), "failed");
    assert_eq!(ChangeSetResultStatus::Cancelled.as_str(), "cancelled");
    assert_eq!(ChangeSetFileResultStatus::Applied.as_str(), "applied");
    assert_eq!(ChangeSetFileResultStatus::Skipped.as_str(), "skipped");
    assert_eq!(ChangeSetFileResultStatus::Failed.as_str(), "failed");
}

fn change_set_id() -> ChangeSetId {
    ChangeSetId::new("change-1").expect("valid change set id")
}

fn sample_change_set() -> ChangeSet {
    ChangeSet {
        id: change_set_id(),
        title: "Update README".to_owned(),
        summary: "Update project overview".to_owned(),
        risk: ChangeSetRisk::Low,
        files: vec![ChangeSetFile {
            path: "README.md".to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Low,
            before_hash: Some("before".to_owned()),
            after_hash: Some("after".to_owned()),
            diff_hash: Some("diff".to_owned()),
            additions: 3,
            deletions: 1,
            validations: vec![ChangeSetValidation {
                kind: ChangeSetValidationKind::Path,
                status: ChangeSetValidationStatus::Passed,
                message: None,
            }],
        }],
        validations: vec![ChangeSetValidation {
            kind: ChangeSetValidationKind::Hash,
            status: ChangeSetValidationStatus::Pending,
            message: Some("validate before apply".to_owned()),
        }],
    }
}

fn sample_result(status: ChangeSetResultStatus) -> ChangeSetResult {
    ChangeSetResult {
        id: change_set_id(),
        status,
        file_results: vec![ChangeSetFileResult {
            path: "README.md".to_owned(),
            action: ChangeSetFileAction::Update,
            status: ChangeSetFileResultStatus::Applied,
            message: None,
            validations: vec![ChangeSetValidation {
                kind: ChangeSetValidationKind::Hash,
                status: ChangeSetValidationStatus::Passed,
                message: None,
            }],
        }],
        message: None,
    }
}
