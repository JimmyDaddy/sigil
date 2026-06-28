use std::path::PathBuf;

use anyhow::Result;

use crate::{
    ContextBodyRef, ContextDigestText, ContextDigestTextKind, ContextDigestV0Builder,
    ContextInclusionReason, ContextItem, ContextPackOptions, ContextSensitivity, ContextSource,
    ContextTrustLevel, SessionArchive, SessionArchiveEntry, VerificationVerdict,
    estimate_context_token_cost, pack_context_items,
};

fn context_item(
    id: &str,
    source: ContextSource,
    trust_level: ContextTrustLevel,
    sensitivity: ContextSensitivity,
    inclusion_reason: ContextInclusionReason,
) -> ContextItem {
    ContextItem {
        id: id.to_owned(),
        source,
        source_event_id: Some(format!("event-{id}")),
        trust_level,
        sensitivity,
        egress_decision: None,
        repo_revision: Some("rev-1".to_owned()),
        token_cost: 3,
        score: Some(1.0),
        inclusion_reason,
        body_ref: ContextBodyRef::inline("context body"),
    }
}

fn pack_item(
    id: &str,
    source: ContextSource,
    inclusion_reason: ContextInclusionReason,
    token_cost: usize,
    score: Option<f32>,
) -> ContextItem {
    ContextItem {
        id: id.to_owned(),
        source,
        source_event_id: Some(format!("event-{id}")),
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: None,
        token_cost,
        score,
        inclusion_reason,
        body_ref: ContextBodyRef::inline("context body"),
    }
}

#[test]
fn context_digest_orders_active_files_and_dedupes_command_receipts() -> Result<()> {
    let digest = ContextDigestV0Builder::new()
        .objective(ContextDigestText::user_provided(
            "fix the parser",
            "event-user-1",
        ))
        .active_file("src/z.rs")
        .active_file("src/a.rs")
        .active_file("src/z.rs")
        .recent_command("receipt-cargo-test")
        .recent_command("receipt-cargo-test")
        .recent_command("receipt-cargo-clippy")
        .verification_state(
            VerificationVerdict::Passed,
            Some("receipt-cargo-test".to_owned()),
        )
        .build()?;

    assert_eq!(
        digest.active_files,
        vec![PathBuf::from("src/a.rs"), PathBuf::from("src/z.rs")]
    );
    assert_eq!(
        digest.recent_commands,
        vec!["receipt-cargo-test", "receipt-cargo-clippy"]
    );
    assert_eq!(digest.verification_state, VerificationVerdict::Passed);
    assert_eq!(
        digest.verification_receipt_id.as_deref(),
        Some("receipt-cargo-test")
    );
    Ok(())
}

#[test]
fn context_digest_rejects_passed_verification_without_receipt_reference() {
    let error = ContextDigestV0Builder::new()
        .verification_state(VerificationVerdict::Passed, None)
        .build()
        .expect_err("digest cannot create verification evidence");

    assert!(
        error
            .to_string()
            .contains("passed verification without a receipt reference")
    );
}

#[test]
fn context_item_requires_workspace_instruction_trust_to_match_source() {
    let error = context_item(
        "repo-file",
        ContextSource::RepositoryFile,
        ContextTrustLevel::WorkspaceInstruction,
        ContextSensitivity::Repository,
        ContextInclusionReason::WorkspaceInstruction,
    )
    .validate()
    .expect_err("trusted workspace instruction must come from workspace instruction source");
    assert!(
        error
            .to_string()
            .contains("workspace instruction trust requires workspace instruction source")
    );

    let error = context_item(
        "workspace-instruction",
        ContextSource::WorkspaceInstruction,
        ContextTrustLevel::UntrustedRepositoryData,
        ContextSensitivity::Repository,
        ContextInclusionReason::ExcludedUntrustedWorkspace,
    )
    .validate()
    .expect_err("workspace instruction source must not be mislabeled");
    assert!(
        error
            .to_string()
            .contains("workspace instruction source must carry workspace instruction trust")
    );
}

#[test]
fn context_item_secret_inclusion_requires_egress_decision() {
    let included_secret_error = context_item(
        "secret",
        ContextSource::RepositoryFile,
        ContextTrustLevel::UntrustedRepositoryData,
        ContextSensitivity::Secret,
        ContextInclusionReason::RetrievalHit,
    )
    .validate()
    .expect_err("included secret context must not bypass egress");
    assert!(
        included_secret_error
            .to_string()
            .contains("included secret context requires an egress decision")
    );

    let excluded_secret = context_item(
        "blocked-secret",
        ContextSource::RepositoryFile,
        ContextTrustLevel::UntrustedRepositoryData,
        ContextSensitivity::Secret,
        ContextInclusionReason::ExcludedSecret,
    );
    excluded_secret
        .validate()
        .expect("excluded secret can be represented without an egress decision");
}

#[test]
fn context_digest_preserves_inferred_text_marking_without_creating_evidence() -> Result<()> {
    let digest = ContextDigestV0Builder::new()
        .unresolved(ContextDigestText::model_inferred(
            "possible flaky check",
            "event-assistant-1",
        ))
        .verification_state(VerificationVerdict::Missing, None)
        .build()?;

    assert_eq!(
        digest.unresolved[0].kind,
        ContextDigestTextKind::ModelInferred
    );
    assert_eq!(digest.verification_state, VerificationVerdict::Missing);
    assert!(digest.verification_receipt_id.is_none());
    Ok(())
}

#[test]
fn context_digest_accepts_valid_provenance_item_and_stable_token_cost() -> Result<()> {
    let mut item = context_item(
        "trusted",
        ContextSource::WorkspaceInstruction,
        ContextTrustLevel::WorkspaceInstruction,
        ContextSensitivity::Repository,
        ContextInclusionReason::WorkspaceInstruction,
    );
    item.egress_decision = Some("egress-1".to_owned());

    let digest = ContextDigestV0Builder::new().context_item(item)?.build()?;

    assert_eq!(digest.context_items.len(), 1);
    assert_eq!(digest.context_items[0].token_cost, 3);
    assert_eq!(estimate_context_token_cost("one two\nthree"), 3);
    assert_eq!(estimate_context_token_cost("   "), 1);
    Ok(())
}

#[test]
fn context_bm25_ranks_session_archive_hits_with_labels() {
    let archive = SessionArchive::new()
        .with_entry(
            SessionArchiveEntry::new(
                "tool-observation-1",
                ContextSource::ToolObservation,
                "cargo test verification failed because the parser rejected note input",
                ContextTrustLevel::ToolObservation,
                ContextSensitivity::Repository,
            )
            .source_event_id("event-tool-1"),
        )
        .with_entry(SessionArchiveEntry::new(
            "tool-observation-2",
            ContextSource::ToolObservation,
            "theme color changed in tui renderer",
            ContextTrustLevel::ToolObservation,
            ContextSensitivity::Repository,
        ));

    let hits = archive.search_bm25("verification cargo parser", 2);

    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.item.id, "session-archive:tool-observation-1");
    assert_eq!(hit.item.source, ContextSource::SessionArchive);
    assert_eq!(hit.item.source_event_id.as_deref(), Some("event-tool-1"));
    assert_eq!(hit.item.trust_level, ContextTrustLevel::ToolObservation);
    assert_eq!(hit.item.sensitivity, ContextSensitivity::Repository);
    assert_eq!(
        hit.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert!(hit.item.score.expect("bm25 score") > 0.0);
    assert!(hit.snippet.contains("cargo test verification"));
    assert!(!hit.truncation.truncated);
    hit.item.validate().expect("retrieval hit is valid context");
}

#[test]
fn context_bm25_marks_secret_hits_excluded_without_egress_and_tracks_truncation() {
    let archive = SessionArchive::new()
        .with_max_index_bytes(32)
        .with_entry(SessionArchiveEntry::new(
            "secret-observation",
            ContextSource::ToolObservation,
            "aws secret token should not enter provider context without approval",
            ContextTrustLevel::ToolObservation,
            ContextSensitivity::Secret,
        ))
        .with_entry(
            SessionArchiveEntry::new(
                "approved-secret-observation",
                ContextSource::ToolObservation,
                "aws secret token approved for a controlled provider egress test",
                ContextTrustLevel::ToolObservation,
                ContextSensitivity::Secret,
            )
            .egress_decision("egress-approved-1"),
        );

    let hits = archive.search_bm25("aws secret token", 5);

    assert_eq!(hits.len(), 2);
    let blocked = hits
        .iter()
        .find(|hit| hit.item.id == "session-archive:secret-observation")
        .expect("blocked secret hit");
    assert_eq!(
        blocked.item.inclusion_reason,
        ContextInclusionReason::ExcludedSecret
    );
    assert!(blocked.item.egress_decision.is_none());
    assert!(blocked.truncation.truncated);
    assert_eq!(blocked.truncation.indexed_byte_len, 32);
    blocked
        .item
        .validate()
        .expect("excluded secret hit remains representable");

    let approved = hits
        .iter()
        .find(|hit| hit.item.id == "session-archive:approved-secret-observation")
        .expect("approved secret hit");
    assert_eq!(
        approved.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert_eq!(
        approved.item.egress_decision.as_deref(),
        Some("egress-approved-1")
    );
    approved
        .item
        .validate()
        .expect("approved secret hit has egress decision");
}

#[test]
fn context_packer_keeps_stable_prefix_before_dynamic_suffix_with_stable_ordering() -> Result<()> {
    let mut system = pack_item(
        "system",
        ContextSource::SystemPrompt,
        ContextInclusionReason::StablePrompt,
        3,
        None,
    );
    system.trust_level = ContextTrustLevel::System;
    let mut workspace = pack_item(
        "workspace",
        ContextSource::WorkspaceInstruction,
        ContextInclusionReason::WorkspaceInstruction,
        2,
        None,
    );
    workspace.trust_level = ContextTrustLevel::WorkspaceInstruction;
    let low_score = pack_item(
        "dynamic-low",
        ContextSource::SessionArchive,
        ContextInclusionReason::RetrievalHit,
        2,
        Some(0.2),
    );
    let high_score = pack_item(
        "dynamic-high",
        ContextSource::LspSymbol,
        ContextInclusionReason::RetrievalHit,
        2,
        Some(0.9),
    );

    let packed = pack_context_items(
        vec![low_score, workspace, high_score, system],
        ContextPackOptions::new(12),
    )?;

    assert_eq!(
        packed
            .stable_prefix
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["system", "workspace"]
    );
    assert_eq!(
        packed
            .dynamic_suffix
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["dynamic-high", "dynamic-low"]
    );
    assert_eq!(packed.used_tokens, 9);
    assert!(packed.excluded.is_empty());
    Ok(())
}

#[test]
fn context_packer_excludes_budget_overflow_and_secret_without_egress() -> Result<()> {
    let stable = pack_item(
        "stable",
        ContextSource::SystemPrompt,
        ContextInclusionReason::StablePrompt,
        3,
        None,
    );
    let expensive = pack_item(
        "expensive",
        ContextSource::SessionArchive,
        ContextInclusionReason::RetrievalHit,
        4,
        Some(0.9),
    );
    let cheap = pack_item(
        "cheap",
        ContextSource::RepositoryFile,
        ContextInclusionReason::RetrievalHit,
        2,
        Some(0.1),
    );
    let mut secret = pack_item(
        "secret",
        ContextSource::RepositoryFile,
        ContextInclusionReason::RetrievalHit,
        1,
        Some(1.0),
    );
    secret.sensitivity = ContextSensitivity::Secret;

    let packed = pack_context_items(
        vec![expensive, secret, cheap, stable],
        ContextPackOptions::new(5),
    )?;

    assert_eq!(
        packed
            .stable_prefix
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["stable"]
    );
    assert_eq!(
        packed
            .dynamic_suffix
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["cheap"]
    );
    assert_eq!(packed.used_tokens, 5);
    let expensive = packed
        .excluded
        .iter()
        .find(|item| item.id == "expensive")
        .expect("expensive item excluded");
    assert_eq!(
        expensive.inclusion_reason,
        ContextInclusionReason::ExcludedTokenBudget
    );
    let secret = packed
        .excluded
        .iter()
        .find(|item| item.id == "secret")
        .expect("secret item excluded");
    assert_eq!(
        secret.inclusion_reason,
        ContextInclusionReason::ExcludedSecret
    );
    Ok(())
}
