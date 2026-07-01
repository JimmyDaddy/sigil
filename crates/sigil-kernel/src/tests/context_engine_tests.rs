use std::path::PathBuf;

use anyhow::Result;

use crate::{
    CONTEXT_QUALITY_EVIDENCE_SCHEMA_VERSION, CONTEXT_QUALITY_REPORT_SCHEMA_VERSION, ContextBodyRef,
    ContextDigestText, ContextDigestTextKind, ContextDigestV0Builder, ContextInclusionReason,
    ContextItem, ContextPackOptions, ContextQualityFindingKind, ContextSensitivity, ContextSource,
    ContextTrustLevel, SessionArchive, SessionArchiveEntry, VerificationVerdict,
    build_context_quality_evidence_pack, estimate_context_token_cost, pack_context_items,
    validate_context_render_snippet, write_context_quality_evidence_artifacts,
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
fn context_item_external_inclusion_requires_egress_decision() {
    let included_external_error = context_item(
        "external",
        ContextSource::ToolObservation,
        ContextTrustLevel::ToolObservation,
        ContextSensitivity::External,
        ContextInclusionReason::RetrievalHit,
    )
    .validate()
    .expect_err("included external context must not bypass egress");
    assert!(
        included_external_error
            .to_string()
            .contains("included external context requires an egress decision")
    );

    let excluded_external = context_item(
        "blocked-external",
        ContextSource::ToolObservation,
        ContextTrustLevel::ToolObservation,
        ContextSensitivity::External,
        ContextInclusionReason::ExcludedEgressDenied,
    );
    excluded_external
        .validate()
        .expect("excluded external context can be represented without an egress decision");
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
fn context_render_snippet_rejects_underreported_budget_and_hash_mismatch() {
    let mut item = context_item(
        "repo-file:README.md",
        ContextSource::RepositoryFile,
        ContextTrustLevel::UntrustedRepositoryData,
        ContextSensitivity::Repository,
        ContextInclusionReason::RetrievalHit,
    );
    item.token_cost = 1;
    item.body_ref = ContextBodyRef::inline("one two three");

    let error = validate_context_render_snippet(&item, "one two three", 1024)
        .expect_err("snippet token cost must not exceed declared item budget");
    assert!(
        error
            .to_string()
            .contains("snippet token cost 3 exceeds declared token cost 1")
    );

    item.token_cost = 3;
    item.body_ref = ContextBodyRef::Inline {
        content_hash: "not-the-real-hash".to_owned(),
        byte_len: "one two three".len(),
    };
    let error = validate_context_render_snippet(&item, "one two three", 1024)
        .expect_err("inline body ref hash must match same-length snippet");
    assert!(
        error
            .to_string()
            .contains("snippet hash does not match inline body ref")
    );

    item.body_ref = ContextBodyRef::inline("one two three four");
    validate_context_render_snippet(&item, "one two three", 1024)
        .expect("shorter rendered snippets can reference a larger indexed inline body");
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
fn context_bm25_retrieves_cjk_session_archive_hits() {
    let archive = SessionArchive::new()
        .with_entry(SessionArchiveEntry::new(
            "zh-review-note",
            ContextSource::ToolObservation,
            "审查结论：解析器验证已经通过，剩余风险是长输出尾部召回。",
            ContextTrustLevel::ToolObservation,
            ContextSensitivity::Repository,
        ))
        .with_entry(SessionArchiveEntry::new(
            "unrelated",
            ContextSource::ToolObservation,
            "English-only renderer update without parser notes.",
            ContextTrustLevel::ToolObservation,
            ContextSensitivity::Repository,
        ));

    let hits = archive.search_bm25("解析器验证结论", 3);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.id, "session-archive:zh-review-note");
    assert!(hits[0].snippet.contains("解析器验证"));
}

#[test]
fn context_bm25_snippet_centers_late_query_match() {
    let body = format!(
        "{} parser_tail_failure explains the late validation error",
        "prefix noise ".repeat(120)
    );
    let archive = SessionArchive::new().with_entry(SessionArchiveEntry::new(
        "late-match",
        ContextSource::ToolObservation,
        body,
        ContextTrustLevel::ToolObservation,
        ContextSensitivity::Repository,
    ));

    let hits = archive.search_bm25("parser_tail_failure", 1);

    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("parser_tail_failure"));
    assert!(hits[0].snippet.starts_with("..."));
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
fn context_packer_excludes_budget_overflow_secret_and_external_without_egress() -> Result<()> {
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
    let mut external = pack_item(
        "external",
        ContextSource::ToolObservation,
        ContextInclusionReason::RetrievalHit,
        1,
        Some(0.8),
    );
    external.trust_level = ContextTrustLevel::ToolObservation;
    external.sensitivity = ContextSensitivity::External;

    let packed = pack_context_items(
        vec![expensive, secret, external, cheap, stable],
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
    let external = packed
        .excluded
        .iter()
        .find(|item| item.id == "external")
        .expect("external item excluded");
    assert_eq!(
        external.inclusion_reason,
        ContextInclusionReason::ExcludedEgressDenied
    );
    Ok(())
}

#[test]
fn context_quality_pack_reports_included_excluded_sources_and_budget_pressure() -> Result<()> {
    let high_score = pack_item(
        "dynamic-high",
        ContextSource::SessionArchive,
        ContextInclusionReason::RetrievalHit,
        2,
        Some(0.9),
    );
    let low_score = pack_item(
        "dynamic-low",
        ContextSource::LspSymbol,
        ContextInclusionReason::RetrievalHit,
        2,
        Some(0.2),
    );
    let expensive = pack_item(
        "dynamic-expensive",
        ContextSource::RepositoryFile,
        ContextInclusionReason::RetrievalHit,
        5,
        Some(0.8),
    );
    let packed = pack_context_items(
        vec![low_score, expensive, high_score],
        ContextPackOptions::new(4),
    )?;

    let report = build_context_quality_evidence_pack(
        "fixture-context-quality",
        "parser validation",
        &packed,
        vec![(
            "dynamic-expensive".to_owned(),
            crate::ContextTruncation {
                original_byte_len: 512,
                indexed_byte_len: 128,
                truncated: true,
            },
        )],
    );

    assert_eq!(
        report.schema_version,
        CONTEXT_QUALITY_EVIDENCE_SCHEMA_VERSION
    );
    assert_eq!(report.fixture_id, "fixture-context-quality");
    assert_eq!(report.query, "parser validation");
    assert_eq!(report.max_tokens, 4);
    assert_eq!(report.used_tokens, 4);
    assert_eq!(report.token_budget_remaining, 0);
    assert_eq!(
        report
            .included
            .iter()
            .map(|item| (item.rank, item.id.as_str()))
            .collect::<Vec<_>>(),
        vec![(Some(1), "dynamic-high"), (Some(2), "dynamic-low")]
    );
    assert_eq!(
        report.included_by_source.get("session_archive").copied(),
        Some(1)
    );
    assert_eq!(
        report.included_by_source.get("lsp_symbol").copied(),
        Some(1)
    );
    let excluded = report
        .excluded
        .iter()
        .find(|item| item.id == "dynamic-expensive")
        .expect("budget excluded item");
    assert_eq!(
        excluded.inclusion_reason,
        ContextInclusionReason::ExcludedTokenBudget
    );
    assert_eq!(
        excluded
            .truncation
            .as_ref()
            .map(|truncation| truncation.truncated),
        Some(true)
    );
    assert_eq!(
        report
            .excluded_by_reason
            .get("excluded_token_budget")
            .copied(),
        Some(1)
    );
    assert!(report.findings.iter().any(|finding| {
        finding.kind == ContextQualityFindingKind::TokenBudgetPressure
            && finding.item_ids == vec!["dynamic-expensive"]
    }));
    let json = serde_json::to_value(&report)?;
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["included"][0]["id"], "dynamic-high");
    assert_eq!(json["excluded_by_reason"]["excluded_token_budget"], 1);
    Ok(())
}

#[test]
fn context_quality_pack_distinguishes_safety_exclusions_ranking_and_recall_gaps() -> Result<()> {
    let no_candidates = pack_context_items(Vec::<ContextItem>::new(), ContextPackOptions::new(4))?;
    let recall = build_context_quality_evidence_pack(
        "fixture-empty-context",
        "unknown symbol",
        &no_candidates,
        Vec::new(),
    );
    assert!(recall.findings.iter().any(|finding| {
        finding.kind == ContextQualityFindingKind::RecallInsufficient && finding.item_ids.is_empty()
    }));

    let unscored = pack_item(
        "unscored-dynamic",
        ContextSource::RepositoryFile,
        ContextInclusionReason::RetrievalHit,
        1,
        None,
    );
    let mut secret = pack_item(
        "secret-hit",
        ContextSource::RepositoryFile,
        ContextInclusionReason::RetrievalHit,
        1,
        Some(1.0),
    );
    secret.sensitivity = ContextSensitivity::Secret;
    let mut external = pack_item(
        "external-hit",
        ContextSource::ToolObservation,
        ContextInclusionReason::RetrievalHit,
        1,
        Some(0.8),
    );
    external.trust_level = ContextTrustLevel::ToolObservation;
    external.sensitivity = ContextSensitivity::External;
    let packed = pack_context_items(
        vec![unscored, secret, external],
        ContextPackOptions::new(10),
    )?;
    let report = build_context_quality_evidence_pack(
        "fixture-safety-context",
        "secret external",
        &packed,
        Vec::new(),
    );

    assert_eq!(
        report.excluded_by_reason.get("excluded_secret").copied(),
        Some(1)
    );
    assert_eq!(
        report
            .excluded_by_reason
            .get("excluded_egress_denied")
            .copied(),
        Some(1)
    );
    assert!(report.findings.iter().any(|finding| {
        finding.kind == ContextQualityFindingKind::RankingInsufficient
            && finding.item_ids == vec!["unscored-dynamic"]
    }));
    assert!(report.findings.iter().any(|finding| {
        finding.kind == ContextQualityFindingKind::SafetyExclusion
            && finding.item_ids == vec!["external-hit", "secret-hit"]
    }));
    Ok(())
}

#[test]
fn context_quality_report_writes_evidence_artifacts() -> Result<()> {
    let default_temp = tempfile::tempdir()?;
    let output_dir = std::env::var_os("SIGIL_CONTEXT_QUALITY_REPORT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| default_temp.path().join("context-quality"));

    let mut workspace_instruction = pack_item(
        "workspace-instruction",
        ContextSource::WorkspaceInstruction,
        ContextInclusionReason::WorkspaceInstruction,
        1,
        Some(1.0),
    );
    workspace_instruction.trust_level = ContextTrustLevel::WorkspaceInstruction;

    let good_pack = build_context_quality_evidence_pack(
        "fixture-context-good",
        "parser validation",
        &pack_context_items(
            vec![
                workspace_instruction,
                pack_item(
                    "session-hit",
                    ContextSource::SessionArchive,
                    ContextInclusionReason::RetrievalHit,
                    1,
                    Some(0.9),
                ),
            ],
            ContextPackOptions::new(8),
        )?,
        Vec::new(),
    );
    let budget_pack = build_context_quality_evidence_pack(
        "fixture-context-budget-pressure",
        "large implementation",
        &pack_context_items(
            vec![
                pack_item(
                    "repo-small",
                    ContextSource::RepositoryFile,
                    ContextInclusionReason::RetrievalHit,
                    2,
                    Some(0.8),
                ),
                pack_item(
                    "repo-large",
                    ContextSource::RepositoryFile,
                    ContextInclusionReason::RetrievalHit,
                    8,
                    Some(0.7),
                ),
            ],
            ContextPackOptions::new(4),
        )?,
        Vec::new(),
    );
    let mut secret = pack_item(
        "secret-candidate",
        ContextSource::RepositoryFile,
        ContextInclusionReason::RetrievalHit,
        1,
        Some(1.0),
    );
    secret.sensitivity = ContextSensitivity::Secret;
    let safety_pack = build_context_quality_evidence_pack(
        "fixture-context-safety",
        "credential handling",
        &pack_context_items(vec![secret], ContextPackOptions::new(8))?,
        Vec::new(),
    );

    let artifacts = write_context_quality_evidence_artifacts(
        &output_dir,
        &[good_pack, budget_pack, safety_pack],
    )?;

    assert!(artifacts.evidence_jsonl_path.exists());
    assert!(artifacts.summary_path.exists());
    assert!(artifacts.manifest_path.exists());

    let jsonl = std::fs::read_to_string(&artifacts.evidence_jsonl_path)?;
    assert_eq!(jsonl.lines().count(), 3);
    assert!(jsonl.contains("\"fixture_id\":\"fixture-context-good\""));
    assert!(jsonl.contains("\"fixture_id\":\"fixture-context-budget-pressure\""));
    assert!(jsonl.contains("\"fixture_id\":\"fixture-context-safety\""));
    assert!(jsonl.contains("\"kind\":\"token_budget_pressure\""));
    assert!(jsonl.contains("\"kind\":\"safety_exclusion\""));

    let summary = std::fs::read_to_string(&artifacts.summary_path)?;
    assert!(summary.contains("# Sigil Context Quality Evidence"));
    assert!(summary.contains("Total packs: 3"));
    assert!(summary.contains("fixture-context-budget-pressure"));

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&artifacts.manifest_path)?)?;
    assert_eq!(
        manifest["report_schema_version"],
        CONTEXT_QUALITY_REPORT_SCHEMA_VERSION
    );
    assert_eq!(manifest["pack_count"], 3);
    assert_eq!(manifest["finding_counts"]["token_budget_pressure"], 1);
    assert_eq!(manifest["finding_counts"]["safety_exclusion"], 1);
    assert!(
        manifest["fixture_ids"]
            .as_array()
            .expect("manifest fixture_ids should be an array")
            .iter()
            .any(|value| value == "fixture-context-good")
    );

    Ok(())
}
