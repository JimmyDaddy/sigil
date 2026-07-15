use anyhow::Result;

use super::*;

fn source(session_scope_id: &str, remote_id: &str) -> Result<ExternalSourceRecord> {
    ExternalSourceRecord::from_remote_candidate(
        session_scope_id,
        Some(remote_id),
        ExternalEvidenceLevel::ProviderGroundingSource,
        "https://example.com/page?token=provider-secret",
        "provider_hosted",
        Some("Example".to_owned()),
        None,
        "2026-07-11T00:00:00Z",
        None,
        Some(1),
        SourceFreshness::Unknown,
        SourceCacheStatus::NotApplicable,
        ToolRestartPolicy::InterruptOnRestart,
    )
}

#[test]
fn external_source_constructor_sanitizes_signed_url_and_untrusted_title_before_context()
-> Result<()> {
    let mut session = crate::Session::new("provider", "model");
    let assistant = ModelMessage::assistant(Some("Safe evidence summary".to_owned()), Vec::new());
    session.append_assistant_message(assistant.clone())?;
    let source = ExternalSourceRecord::from_remote_candidate(
        session.session_scope_id(),
        Some("remote-secret-id"),
        ExternalEvidenceLevel::FetchedPage,
        "https://example.com/private/report?X-Amz-Signature=signed-secret",
        "web_fetch",
        Some(
            "\u{1b}]8;;https://evil.example/?token=title-secret\u{7}Click\u{202e} token=known-secret"
                .to_owned(),
        ),
        Some("2026-07-10T23:59:59Z".to_owned()),
        "2026-07-11T00:00:00.123Z",
        Some(sha256_hex(b"safe fetched text")),
        Some(1),
        SourceFreshness::Fresh,
        SourceCacheStatus::Miss,
        ToolRestartPolicy::InterruptOnRestart,
    )?;
    assert_eq!(
        source.safe_display_url,
        "https://example.com/private/report?[redacted]"
    );
    source.validate()?;
    session.append_external_provenance(ExternalProvenanceEntry {
        session_scope_id: session.session_scope_id().to_owned(),
        message_id: assistant.id,
        trust: ExternalTrust::ExternalUntrusted,
        sources: vec![source],
        citations: Vec::new(),
    })?;
    session.append_user_message(ModelMessage::user("Summarize the external evidence"))?;

    let temp = tempfile::tempdir()?;
    let request = session.build_request_with_transient_messages_and_context(
        temp.path(),
        &crate::MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
        &[],
        crate::RuntimeContextCandidates::default(),
    )?;
    let durable_json = serde_json::to_string(session.entries())?;
    let context_json = serde_json::to_string(&request)?;
    for unsafe_value in [
        "signed-secret",
        "title-secret",
        "known-secret",
        "remote-secret-id",
        "\u{1b}",
        "\u{7}",
        "\u{202e}",
    ] {
        assert!(
            !durable_json.contains(unsafe_value),
            "durable leak: {unsafe_value:?}"
        );
        assert!(
            !context_json.contains(unsafe_value),
            "context leak: {unsafe_value:?}"
        );
    }
    assert!(durable_json.contains("[redacted]"));
    assert!(context_json.contains("external_untrusted"));
    Ok(())
}

#[test]
fn external_source_validation_rejects_forged_raw_signed_display_and_invalid_metadata() -> Result<()>
{
    let mut signed_source = source("session-a", "remote-1")?;
    signed_source.safe_display_url = "https://example.com/page?signature=raw-secret".to_owned();
    signed_source.safe_display_url_sha256 = sha256_hex(signed_source.safe_display_url.as_bytes());
    assert!(signed_source.validate().is_err());

    let mut titled_source = source("session-a", "remote-2")?;
    titled_source.title = Some("unsafe\u{202e}title".to_owned());
    assert!(titled_source.validate().is_err());

    let mut hashed_source = source("session-a", "remote-3")?;
    hashed_source.content_sha256 = Some("ABC".to_owned());
    assert!(hashed_source.validate().is_err());
    Ok(())
}

#[test]
fn external_provenance_enforces_source_and_citation_boundaries() -> Result<()> {
    let session_scope_id = "session-a";
    let message = ModelMessage::assistant(Some("safe claim".to_owned()), Vec::new());
    let template = source(session_scope_id, "remote-template")?;
    let sources = (0..MAX_EXTERNAL_PROVENANCE_SOURCES)
        .map(|index| {
            let mut source = template.clone();
            source.source_id = format!("src_{index:032x}");
            source
        })
        .collect::<Vec<_>>();
    let at_limit = ExternalProvenanceEntry {
        session_scope_id: session_scope_id.to_owned(),
        message_id: message.id.clone(),
        trust: ExternalTrust::ExternalUntrusted,
        sources: sources.clone(),
        citations: Vec::new(),
    };
    at_limit.validate_against_message(&message)?;
    let mut over_limit = at_limit.clone();
    let mut overflow_source = template.clone();
    overflow_source.source_id = format!("src_{:032x}", MAX_EXTERNAL_PROVENANCE_SOURCES);
    over_limit.sources.push(overflow_source);
    assert!(over_limit.validate_against_message(&message).is_err());

    let citation = CitationSupport::for_final_safe_text(
        session_scope_id,
        &message.id,
        &sources[0].source_id,
        "safe claim",
        0,
        4,
    )
    .ok_or_else(|| anyhow::anyhow!("citation should build"))?;
    let citation_limit = ExternalProvenanceEntry {
        session_scope_id: session_scope_id.to_owned(),
        message_id: message.id.clone(),
        trust: ExternalTrust::ExternalUntrusted,
        sources: vec![sources[0].clone()],
        citations: vec![citation.clone(); MAX_EXTERNAL_PROVENANCE_CITATIONS],
    };
    citation_limit.validate_against_message(&message)?;
    let mut citation_overflow = citation_limit;
    citation_overflow.citations.push(citation);
    assert!(
        citation_overflow
            .validate_against_message(&message)
            .is_err()
    );
    Ok(())
}

#[test]
fn external_source_title_accepts_limit_and_rejects_limit_plus_one() -> Result<()> {
    let mut at_limit = source("session-a", "remote-title-limit")?;
    at_limit.title = Some("x".repeat(EXTERNAL_TITLE_MAX_BYTES));
    at_limit.validate()?;

    at_limit.title = Some("x".repeat(EXTERNAL_TITLE_MAX_BYTES + 1));
    assert!(at_limit.validate().is_err());
    Ok(())
}

#[test]
fn session_recovery_quarantines_tampered_external_sidecar_and_url_descriptor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut original = crate::Session::new("provider", "model").with_store(store.clone());
    let assistant = ModelMessage::assistant(Some("safe answer".to_owned()), Vec::new());
    original.append_assistant_message(assistant.clone())?;
    let user = ModelMessage::user("safe follow up");
    original.append_user_message(user.clone())?;
    store.append(&crate::SessionLogEntry::Control(
        crate::ControlEntry::WebUrlCapabilityDescriptor(crate::WebUrlCapabilityDescriptor {
            session_scope_id: original.session_scope_id().to_owned(),
            source_id: "src_fedcba9876543210fedcba9876543210".to_owned(),
            durable_entry_id: assistant.id.clone(),
            safe_display_url: "https://example.com/public?[redacted]".to_owned(),
            restart_policy: ToolRestartPolicy::InterruptOnRestart,
            replayable_canonical_url: None,
            originating_call_id: Some("call-web-search".to_owned()),
            provenance: crate::WebUrlProvenanceKind::WebSearchResult,
            issued_at_ms: 1,
            expires_at_ms: u64::MAX,
        }),
    ))?;

    let mut tampered_source = source(original.session_scope_id(), "remote-tampered")?;
    tampered_source.safe_display_url =
        "https://example.com/private?signature=recovery-secret".to_owned();
    tampered_source.safe_display_url_sha256 =
        sha256_hex(tampered_source.safe_display_url.as_bytes());
    tampered_source.title = Some("unsafe\u{1b}]title\u{202e}".to_owned());
    store.append(&crate::SessionLogEntry::Control(
        crate::ControlEntry::ExternalProvenance(ExternalProvenanceEntry {
            session_scope_id: original.session_scope_id().to_owned(),
            message_id: assistant.id,
            trust: ExternalTrust::ExternalUntrusted,
            sources: vec![tampered_source],
            citations: Vec::new(),
        }),
    ))?;
    store.append(&crate::SessionLogEntry::Control(
        crate::ControlEntry::WebUrlCapabilityDescriptor(crate::WebUrlCapabilityDescriptor {
            session_scope_id: original.session_scope_id().to_owned(),
            source_id: "src_0123456789abcdef0123456789abcdef".to_owned(),
            durable_entry_id: user.id,
            safe_display_url: "https://example.com/private?signature=descriptor-recovery-secret"
                .to_owned(),
            restart_policy: ToolRestartPolicy::InterruptOnRestart,
            replayable_canonical_url: None,
            originating_call_id: None,
            provenance: crate::WebUrlProvenanceKind::UserMessage,
            issued_at_ms: 1,
            expires_at_ms: u64::MAX,
        }),
    ))?;
    drop(original);

    let mut recovered = crate::Session::load_from_store("provider", "model", store)?;
    let recovered_json = serde_json::to_string(recovered.entries())?;
    for secret in [
        "recovery-secret",
        "descriptor-recovery-secret",
        "unsafe\\u001b",
        "\\u202e",
    ] {
        assert!(!recovered_json.contains(secret), "recovered leak: {secret}");
    }
    assert!(!recovered.entries().iter().any(|entry| matches!(
        entry,
        crate::SessionLogEntry::Control(crate::ControlEntry::ExternalProvenance(_))
    )));
    assert_eq!(
        recovered
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                crate::SessionLogEntry::Control(
                    crate::ControlEntry::WebUrlCapabilityDescriptor(descriptor)
                ) if descriptor.provenance == crate::WebUrlProvenanceKind::WebSearchResult
            ))
            .count(),
        1
    );
    assert!(recovered.entries().iter().any(|entry| matches!(
        entry,
        crate::SessionLogEntry::Control(crate::ControlEntry::ContextAssemblySkipped(audit))
            if audit.reason == "recovery skipped unsafe external persistence control"
    )));
    let request = recovered.build_request_with_transient_messages_and_context(
        temp.path(),
        &crate::MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
        &[],
        crate::RuntimeContextCandidates::default(),
    )?;
    let request_json = serde_json::to_string(&request)?;
    assert!(!request_json.contains("recovery-secret"));
    assert!(!request_json.contains("descriptor-recovery-secret"));
    Ok(())
}

#[test]
fn external_provenance_rewrites_remote_id_and_binds_utf8_citation() -> Result<()> {
    let session_scope_id = "session-1";
    let remote_id = "https://remote.example/?token=low-entropy";
    let source = source(session_scope_id, remote_id)?;
    assert_ne!(source.source_id, remote_id);
    let json = serde_json::to_string(&source)?;
    assert!(!json.contains(remote_id));
    assert!(!json.contains("low-entropy"));

    let message = ModelMessage {
        id: "assistant-1".to_owned(),
        role: crate::MessageRole::Assistant,
        content: Some("依据：你好".to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: Some(crate::AssistantMessageKind::FinalAnswer),
        image_attachments: Vec::new(),
    };
    let start = "依据：".len();
    let citation = CitationSupport::for_final_safe_text(
        session_scope_id,
        &message.id,
        &source.source_id,
        message.content.as_deref().unwrap_or_default(),
        start,
        message.content.as_deref().unwrap_or_default().len(),
    )
    .ok_or_else(|| anyhow::anyhow!("valid citation should be constructed"))?;
    let provenance = ExternalProvenanceEntry {
        session_scope_id: session_scope_id.to_owned(),
        message_id: message.id.clone(),
        trust: ExternalTrust::ExternalUntrusted,
        sources: vec![source],
        citations: vec![citation],
    };
    provenance.validate_against_message(&message)?;
    Ok(())
}

#[test]
fn external_provenance_does_not_invent_invalid_utf8_span() -> Result<()> {
    let message = "a你b";
    assert!(
        CitationSupport::for_final_safe_text(
            "session-1",
            "assistant-1",
            "src_0123456789abcdef0123456789abcdef",
            message,
            2,
            message.len(),
        )
        .is_none()
    );
    assert!(
        CitationSupport::for_final_safe_text(
            "session-1",
            "assistant-1",
            "src_0123456789abcdef0123456789abcdef",
            message,
            0,
            0,
        )
        .is_none()
    );
    Ok(())
}

#[test]
fn external_provenance_rejects_cross_session_source_and_citation() -> Result<()> {
    let source = source("session-a", "remote-1")?;
    let message = ModelMessage::assistant(Some("safe".to_owned()), Vec::new());
    let citation = CitationSupport::for_final_safe_text(
        "session-b",
        &message.id,
        &source.source_id,
        "safe",
        0,
        4,
    )
    .ok_or_else(|| anyhow::anyhow!("citation construction failed"))?;
    let provenance = ExternalProvenanceEntry {
        session_scope_id: "session-b".to_owned(),
        message_id: message.id.clone(),
        trust: ExternalTrust::ExternalUntrusted,
        sources: vec![source],
        citations: vec![citation],
    };
    assert!(provenance.validate_against_message(&message).is_err());
    Ok(())
}
