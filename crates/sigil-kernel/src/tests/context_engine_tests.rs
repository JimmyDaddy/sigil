use std::path::PathBuf;

use anyhow::Result;

use crate::{
    ContextBodyRef, ContextDigestText, ContextDigestTextKind, ContextDigestV0Builder,
    ContextInclusionReason, ContextItem, ContextSensitivity, ContextSource, ContextTrustLevel,
    VerificationVerdict, estimate_context_token_cost,
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
