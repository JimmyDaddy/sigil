use std::fs;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AssistantMessageKind, ControlEntry, DurableEventType, EventClass, ImageAttachment,
    ImageMimeType, JsonlSessionStore, ModelMessage, Session, SessionLogEntry, ToolCall,
};

use super::*;

fn finalized_session(path: &Path, prompt: &str) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    })?;
    session.append_user_message(ModelMessage::user(prompt))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("finished".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    session.append_assistant_message(assistant.clone())?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 0,
            "error": null
        }),
    )?;
    Ok(())
}

#[test]
fn local_session_catalog_projects_only_bounded_v2_direct_children() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let ready = sessions.join("session-ready.jsonl");
    finalized_session(&ready, "Explain the repository")?;
    fs::write(
        sessions.join("session-legacy.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("legacy")))?
        ),
    )?;
    let oversized = sessions.join("session-oversized.jsonl");
    fs::File::create(&oversized)?.set_len(4_097)?;
    fs::write(sessions.join("ignore.txt"), "not a session")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_limits(LocalSessionLifecycleLimits {
                max_catalog_entries: 16,
                max_stream_bytes: 4_096,
                max_total_validation_bytes: 1024 * 1024,
                max_export_messages: 100,
                max_export_bytes: 1024 * 1024,
            });

    let catalog = service.catalog()?;

    assert_eq!(catalog.entries.len(), 3);
    assert_eq!(catalog.truncated_entry_count, 0);
    let ready = catalog
        .entries
        .iter()
        .find(|entry| entry.path.ends_with("session-ready.jsonl"))
        .expect("ready entry");
    assert_eq!(ready.state, LocalSessionCatalogState::Ready);
    assert_eq!(ready.provider_name.as_deref(), Some("deepseek"));
    assert_eq!(ready.model_name.as_deref(), Some("deepseek-v4-flash"));
    assert_eq!(ready.title.as_deref(), Some("Explain the repository"));
    assert_eq!(ready.transcript_message_count, 2);
    assert_eq!(ready.finalized_turn_count, 1);
    assert!(ready.session_id.is_some());
    assert!(catalog.entries.iter().any(|entry| {
        entry.path.ends_with("session-legacy.jsonl")
            && entry.state == LocalSessionCatalogState::UnsupportedLegacy
    }));
    assert!(catalog.entries.iter().any(|entry| {
        entry.path.ends_with("session-oversized.jsonl")
            && entry.state == LocalSessionCatalogState::Oversized
    }));
    Ok(())
}

#[test]
fn session_reopen_resolves_current_ready_direct_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-ready.jsonl");
    finalized_session(&source, "resume this session")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let listed = service
        .catalog()?
        .entries
        .into_iter()
        .find(|entry| entry.path.ends_with("session-ready.jsonl"))
        .expect("ready session should be listed");
    let expected_session_id = listed
        .session_id
        .as_deref()
        .expect("ready session should have durable identity");

    let binding = service.resolve_session_for_reopen(&listed.session_ref, expected_session_id)?;

    assert_eq!(binding.session_ref, listed.session_ref);
    assert_eq!(binding.session_id, expected_session_id);
    assert_eq!(binding.session_log_path, source.canonicalize()?);
    Ok(())
}

#[test]
fn session_reopen_validates_a_direct_source_outside_the_catalog_entry_cap() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let older = sessions.join("z-older.jsonl");
    finalized_session(&older, "older durable history")?;
    let newer = sessions.join("a-newer.jsonl");
    finalized_session(&newer, "newer durable history")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_limits(LocalSessionLifecycleLimits {
                max_catalog_entries: 1,
                ..LocalSessionLifecycleLimits::default()
            });
    let older_records = JsonlSessionStore::read_event_records(&older)?;
    let expected_session_id = older_records
        .first()
        .expect("older durable stream should not be empty")
        .session_id()
        .to_owned();
    let older_ref = SessionRef::new_relative("z-older.jsonl")?;
    let catalog = service.catalog()?;

    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(catalog.truncated_entry_count, 1);
    assert!(
        catalog
            .entries
            .iter()
            .all(|entry| entry.session_ref != older_ref)
    );
    let binding = service.resolve_session_for_reopen(&older_ref, &expected_session_id)?;
    assert_eq!(binding.session_log_path, older.canonicalize()?);
    assert_eq!(binding.session_id, expected_session_id);
    Ok(())
}

#[test]
fn session_reopen_rejects_missing_non_ready_and_changed_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let ready = sessions.join("session-ready.jsonl");
    finalized_session(&ready, "ready")?;
    fs::write(
        sessions.join("session-legacy.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("legacy")))?
        ),
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    assert!(matches!(
        service.resolve_session_for_reopen(
            &sigil_kernel::SessionRef::new_relative("missing.jsonl")?,
            "missing"
        ),
        Err(LocalSessionReopenError::NotFound)
    ));
    assert!(matches!(
        service.resolve_session_for_reopen(
            &sigil_kernel::SessionRef::new_relative("session-legacy.jsonl")?,
            "legacy"
        ),
        Err(LocalSessionReopenError::NotReady {
            state: LocalSessionCatalogState::UnsupportedLegacy
        })
    ));
    assert!(matches!(
        service.resolve_session_for_reopen(
            &sigil_kernel::SessionRef::new_relative("session-ready.jsonl")?,
            "stale-session-id"
        ),
        Err(LocalSessionReopenError::IdentityChanged)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_session_catalog_marks_symlink_and_scan_budget_entries_unavailable() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let first = sessions.join("session-first.jsonl");
    let second = sessions.join("session-second.jsonl");
    finalized_session(&first, "first")?;
    finalized_session(&second, "second")?;
    let external = temp.path().join("external.jsonl");
    finalized_session(&external, "external")?;
    symlink(&external, sessions.join("session-link.jsonl"))?;
    let first_bytes = fs::metadata(&first)?.len();
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_limits(LocalSessionLifecycleLimits {
                max_catalog_entries: 16,
                max_stream_bytes: DEFAULT_SESSION_CATALOG_MAX_STREAM_BYTES,
                max_total_validation_bytes: first_bytes,
                max_export_messages: 100,
                max_export_bytes: 1024 * 1024,
            });

    let catalog = service.catalog()?;

    assert!(catalog.entries.iter().any(|entry| {
        entry.path.ends_with("session-link.jsonl")
            && entry.state == LocalSessionCatalogState::Invalid
    }));
    assert_eq!(
        catalog
            .entries
            .iter()
            .filter(|entry| entry.state == LocalSessionCatalogState::Ready)
            .count(),
        1
    );
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| { entry.state == LocalSessionCatalogState::ScanBudgetExceeded })
    );
    let error = service
        .export_session(&sessions.join("session-link.jsonl"), None, 1234)
        .expect_err("symlink source must fail");
    assert!(error.to_string().contains("must not be a symlink"));
    Ok(())
}

#[test]
fn safe_session_export_redacts_text_omits_tool_calls_and_is_content_bound() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    let store = JsonlSessionStore::new(&source)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "chat".to_owned(),
    }))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "token=raw-secret https://example.com/private?sig=raw-secret",
    )))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        vec![ToolCall {
            id: "call-secret".to_owned(),
            name: "shell".to_owned(),
            args_json: "{\"token\":\"raw-secret\"}".to_owned(),
        }],
        AssistantMessageKind::FinalAnswer,
    );
    store.append(&SessionLogEntry::Assistant(assistant.clone()))?;
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({"run_status": "completed", "final_message_id": assistant.id}),
    )?;
    let exports = temp.path().join("exports");
    let service = LocalSessionLifecycleService::new("workspace-1", &sessions, &exports);

    let output = service.export_session(&source, None, 1234)?;

    assert!(output.path.starts_with(&exports));
    assert_eq!(output.message_count, 2);
    let bytes = fs::read(&output.path)?;
    let text = String::from_utf8(bytes.clone())?;
    assert!(!text.contains("raw-secret"));
    assert!(!text.contains("args_json"));
    assert!(!text.contains("tool_calls"));
    assert!(!text.contains("session_identity"));
    assert!(text.contains("token=[redacted]"));
    assert!(text.contains("https://example.com/private?[redacted]"));
    let artifact: SessionExportV1 = serde_json::from_slice(&bytes)?;
    artifact.validate_digest()?;
    assert_eq!(artifact.payload.workspace_id, "workspace-1");
    assert_eq!(artifact.payload.source_content_sha256.len(), 64);
    assert_eq!(artifact.payload_sha256, output.payload_sha256);
    assert_eq!(artifact.payload.messages.len(), 2);
    let records = service.lifecycle_records()?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[1].sequence, 2);
    assert_eq!(
        records[1].previous_record_sha256.as_deref(),
        Some(records[0].record_sha256.as_str())
    );
    assert!(matches!(
        records[0].event,
        LocalSessionLifecycleEvent::ExportPlanned(_)
    ));
    assert!(matches!(
        records[1].event,
        LocalSessionLifecycleEvent::ExportCompleted(_)
    ));
    assert_eq!(output.journal_sequence, 2);
    assert_eq!(
        service.lifecycle_recovery()?,
        vec![LocalSessionLifecycleRecoveryEntry {
            operation_id: output.operation_id,
            kind: LocalSessionLifecycleOperationKind::Export,
            status: LocalSessionLifecycleRecoveryStatus::Completed,
        }]
    );
    Ok(())
}

#[test]
fn safe_session_export_keeps_image_metadata_without_process_local_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-image.jsonl");
    let store = JsonlSessionStore::new(&source)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "openai".to_owned(),
        model_name: "gpt-5".to_owned(),
    }))?;
    let mut user = ModelMessage::user("inspect\n\n[Image attachment 1: image/png]");
    user.image_attachments.push(ImageAttachment::from_bytes(
        "image-1",
        ImageMimeType::Png,
        1,
        1,
        vec![1, 2, 3],
    )?);
    store.append(&SessionLogEntry::User(user))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    store.append(&SessionLogEntry::Assistant(assistant.clone()))?;
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({"run_status": "completed", "final_message_id": assistant.id}),
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let output = service.export_session(&source, None, 1234)?;
    let bytes = fs::read(&output.path)?;
    let text = String::from_utf8(bytes.clone())?;
    assert!(!text.contains("AQID"));
    assert!(!text.contains("resolved_bytes"));
    let artifact: SessionExportV1 = serde_json::from_slice(&bytes)?;
    let attachment = &artifact.payload.messages[0].image_attachments[0];
    assert_eq!(attachment.attachment_id, "image-1");
    assert!(!attachment.has_resolved_bytes());
    artifact.validate_digest()?;
    Ok(())
}

#[test]
fn safe_session_export_never_overwrites_existing_destination() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "hello")?;
    let destination = temp.path().join("existing.json");
    fs::write(&destination, "keep")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let error = service
        .export_session(&source, Some(&destination), 1234)
        .expect_err("existing destination must fail");

    assert!(error.to_string().contains("already exists"));
    assert_eq!(fs::read_to_string(destination)?, "keep");
    Ok(())
}

#[test]
fn safe_session_export_rejects_message_and_artifact_limits_without_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "hello")?;
    let destination = temp.path().join("limited.json");
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_limits(LocalSessionLifecycleLimits {
                max_catalog_entries: 16,
                max_stream_bytes: DEFAULT_SESSION_CATALOG_MAX_STREAM_BYTES,
                max_total_validation_bytes: DEFAULT_SESSION_CATALOG_MAX_TOTAL_VALIDATION_BYTES,
                max_export_messages: 1,
                max_export_bytes: 16,
            });

    let error = service
        .export_session(&source, Some(&destination), 1234)
        .expect_err("message limit must fail");

    assert!(error.to_string().contains("message limit"));
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn session_delete_preview_protects_current_and_apply_is_exact_and_audited() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "delete me")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let error = service
        .preview_delete(&source, std::slice::from_ref(&source))
        .expect_err("current session must be protected");
    assert!(error.to_string().contains("protected"));
    assert!(service.lifecycle_records()?.is_empty());
    let preview = service.preview_delete(&source, &[])?;
    assert_eq!(preview.source_bytes, fs::metadata(&source)?.len());
    assert_eq!(preview.source_content_sha256.len(), 64);

    let output = service.apply_delete(&preview, &[], 5678)?;

    assert!(!source.exists());
    assert_eq!(output.deleted_bytes, preview.source_bytes);
    let records = service.lifecycle_records()?;
    assert_eq!(records.len(), 2);
    assert!(matches!(
        records[0].event,
        LocalSessionLifecycleEvent::DeletePlanned(_)
    ));
    assert!(matches!(
        records[1].event,
        LocalSessionLifecycleEvent::DeleteCompleted(_)
    ));
    assert_eq!(
        service.lifecycle_recovery()?,
        vec![LocalSessionLifecycleRecoveryEntry {
            operation_id: output.operation_id,
            kind: LocalSessionLifecycleOperationKind::Delete,
            status: LocalSessionLifecycleRecoveryStatus::Completed,
        }]
    );
    Ok(())
}

#[test]
fn session_delete_rejects_preview_tamper_and_source_drift_before_journal_or_remove() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "keep me")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let preview = service.preview_delete(&source, &[])?;
    let mut tampered = preview.clone();
    tampered.source_bytes = tampered.source_bytes.saturating_add(1);

    let error = service
        .apply_delete(&tampered, &[], 5678)
        .expect_err("tampered preview must fail");
    assert!(error.to_string().contains("digest"));
    assert!(source.exists());
    assert!(service.lifecycle_records()?.is_empty());

    let store = JsonlSessionStore::new(&source)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("late append")))?;
    drop(store);
    let error = service
        .apply_delete(&preview, &[], 5678)
        .expect_err("source drift must fail");
    assert!(error.to_string().contains("changed"));
    assert!(source.exists());
    assert!(service.lifecycle_records()?.is_empty());
    Ok(())
}

#[test]
fn session_delete_rejects_an_active_writer_lease_before_planned_record() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "active")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let preview = service.preview_delete(&source, &[])?;
    let active_store = JsonlSessionStore::new(&source)?;
    active_store.append(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        Default::default(),
    )))?;
    let refreshed = service.preview_delete(&source, &[])?;

    let error = service
        .apply_delete(&refreshed, &[], 5678)
        .expect_err("active writer must fail");

    assert!(
        error
            .to_string()
            .contains("active or its writer lease is busy")
    );
    assert!(source.exists());
    assert!(service.lifecycle_records()?.is_empty());
    drop(active_store);
    assert_ne!(preview.preview_digest, refreshed.preview_digest);
    Ok(())
}

#[test]
fn lifecycle_recovery_distinguishes_not_applied_from_uncertain_delete() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "recover")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let preview = service.preview_delete(&source, &[])?;
    let binding = LocalSessionDeleteJournalBinding {
        source_session_ref: preview.source_session_ref.clone(),
        source_session_id: preview.source_session_id.clone(),
        source_content_sha256: preview.source_content_sha256.clone(),
        source_bytes: preview.source_bytes,
        source_modified_at_unix_ms: preview.source_modified_at_unix_ms,
        preview_digest: preview.preview_digest.clone(),
    };
    service.lifecycle_journal().append(
        "session-delete:incomplete",
        5678,
        LocalSessionLifecycleEvent::DeletePlanned(binding.clone()),
    )?;
    let mut mismatched = binding;
    mismatched.source_bytes = mismatched.source_bytes.saturating_add(1);
    let error = service
        .lifecycle_journal()
        .append(
            "session-delete:incomplete",
            5679,
            LocalSessionLifecycleEvent::DeleteCompleted(mismatched),
        )
        .expect_err("completion must match its exact plan");
    assert!(error.to_string().contains("exact planned binding"));

    assert_eq!(
        service.lifecycle_recovery()?,
        vec![LocalSessionLifecycleRecoveryEntry {
            operation_id: "session-delete:incomplete".to_owned(),
            kind: LocalSessionLifecycleOperationKind::Delete,
            status: LocalSessionLifecycleRecoveryStatus::NotApplied,
        }]
    );
    fs::remove_file(&source)?;
    assert_eq!(
        service.lifecycle_recovery()?[0].status,
        LocalSessionLifecycleRecoveryStatus::Uncertain
    );
    Ok(())
}

#[test]
fn lifecycle_journal_hash_chain_rejects_tampering() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "export")?;
    let exports = temp.path().join("exports");
    let journal = temp.path().join("lifecycle.jsonl");
    let service = LocalSessionLifecycleService::new("workspace-1", &sessions, &exports)
        .with_lifecycle_journal_path(&journal);
    service.export_session(&source, None, 1234)?;
    let bytes = fs::read_to_string(&journal)?;
    let tampered = bytes.replacen("\"message_count\":2", "\"message_count\":3", 1);
    assert_ne!(tampered, bytes);
    fs::write(&journal, tampered)?;

    let error = service
        .lifecycle_records()
        .expect_err("tampered hash chain must fail");

    assert!(error.to_string().contains("record hash does not match"));
    Ok(())
}

#[test]
fn session_pin_is_identity_bound_and_blocks_direct_delete_until_unpinned() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "pin me")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    service.set_session_pin(&source, true, 100)?;
    let catalog = service.catalog()?;
    assert!(catalog.entries[0].pinned);
    let error = service
        .preview_delete(&source, &[])
        .expect_err("pinned session must not preview delete");
    assert!(error.to_string().contains("pinned"));

    service.set_session_pin(&source, false, 101)?;
    assert!(!service.catalog()?.entries[0].pinned);
    service.preview_delete(&source, &[])?;
    Ok(())
}

#[test]
fn session_pin_projection_requires_the_recorded_stream_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "identity")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let entry = service.catalog()?.entries.remove(0);
    let session_id = entry.session_id.clone().expect("stream identity");
    service.lifecycle_journal().append(
        "session-pin:mismatched",
        100,
        LocalSessionLifecycleEvent::PinChanged(LocalSessionPinJournalBinding {
            source_session_ref: entry.session_ref,
            source_session_id: format!("{session_id}-different"),
            pinned: true,
        }),
    )?;

    assert!(!service.catalog()?.entries[0].pinned);
    service.preview_delete(&source, &[])?;
    Ok(())
}

#[test]
fn session_display_name_is_identity_bound_and_durably_projected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "Original prompt")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let listed = service.catalog()?.entries.remove(0);
    let session_id = listed.session_id.expect("durable identity");

    let record = service.rename_session(
        &listed.session_ref,
        &session_id,
        "Readable conversation name",
        200,
    )?;

    assert!(matches!(
        record.event,
        LocalSessionLifecycleEvent::DisplayNameChanged(
            LocalSessionDisplayNameJournalBinding { ref display_name, .. }
        ) if display_name == "Readable conversation name"
    ));
    assert_eq!(
        service.catalog()?.entries[0].title.as_deref(),
        Some("Readable conversation name")
    );
    assert!(matches!(
        service.rename_session(&listed.session_ref, "different", "Changed", 201),
        Err(LocalSessionMutationError::IdentityChanged)
    ));
    assert!(matches!(
        service.rename_session(&listed.session_ref, &session_id, "  ", 202),
        Err(LocalSessionMutationError::InvalidRequest)
    ));
    Ok(())
}

#[test]
fn retention_preview_is_read_only_deterministic_and_respects_pin_and_protection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let alpha = sessions.join("session-alpha.jsonl");
    let beta = sessions.join("session-beta.jsonl");
    let gamma = sessions.join("session-gamma.jsonl");
    let current = sessions.join("session-current.jsonl");
    finalized_session(&alpha, "alpha")?;
    finalized_session(&beta, "beta")?;
    finalized_session(&gamma, "gamma")?;
    finalized_session(&current, "current")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    service.set_session_pin(&beta, true, 100)?;
    let journal_before = service.lifecycle_records()?;
    let policy = SessionRetentionPolicy {
        max_sessions: Some(2),
        max_bytes: None,
        expire_older_than_ms: None,
    };

    let first = service.preview_retention(
        policy.clone(),
        std::slice::from_ref(&current),
        1_000_000_000_000_000,
    )?;
    let second = service.preview_retention(
        policy,
        std::slice::from_ref(&current),
        1_000_000_000_000_000,
    )?;

    assert_eq!(first, second);
    assert_eq!(service.lifecycle_records()?, journal_before);
    assert_eq!(first.total_ready_sessions, 4);
    assert_eq!(first.protected_sessions, 1);
    assert_eq!(first.pinned_sessions, 1);
    assert_eq!(first.candidates.len(), 2);
    assert!(first.constraints_satisfied);
    assert!(first.candidates.iter().all(|candidate| {
        candidate.reasons == vec![SessionRetentionReason::Count]
            && candidate.delete_preview.source_path != beta
            && candidate.delete_preview.source_path != current
    }));
    Ok(())
}

#[test]
fn retention_preview_selects_age_and_bytes_candidates_with_explicit_reasons() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let alpha = sessions.join("session-alpha.jsonl");
    let beta = sessions.join("session-beta.jsonl");
    finalized_session(&alpha, "alpha")?;
    finalized_session(&beta, "beta")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let catalog = service.catalog()?;
    let generated_at = catalog
        .entries
        .iter()
        .map(|entry| entry.modified_at_unix_ms)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let age = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: None,
            max_bytes: None,
            expire_older_than_ms: Some(0),
        },
        &[],
        generated_at,
    )?;
    assert_eq!(age.candidates.len(), 2);
    assert!(age.constraints_satisfied);
    assert!(
        age.candidates
            .iter()
            .all(|candidate| { candidate.reasons == vec![SessionRetentionReason::Age] })
    );

    let bytes = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: None,
            max_bytes: Some(0),
            expire_older_than_ms: None,
        },
        &[],
        generated_at,
    )?;
    assert_eq!(bytes.candidates.len(), 2);
    assert!(bytes.constraints_satisfied);
    assert!(
        bytes
            .candidates
            .iter()
            .all(|candidate| { candidate.reasons == vec![SessionRetentionReason::Bytes] })
    );
    Ok(())
}

#[test]
fn retention_apply_preflights_the_whole_batch_then_audits_each_delete() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let alpha = sessions.join("session-alpha.jsonl");
    let beta = sessions.join("session-beta.jsonl");
    let current = sessions.join("session-current.jsonl");
    finalized_session(&alpha, "alpha")?;
    finalized_session(&beta, "beta")?;
    finalized_session(&current, "current")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let preview = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: Some(1),
            max_bytes: None,
            expire_older_than_ms: None,
        },
        std::slice::from_ref(&current),
        1_000_000_000_000_000,
    )?;

    let output = service.apply_retention(&preview, std::slice::from_ref(&current), 5678)?;

    assert_eq!(output.deleted_sessions, 2);
    assert_eq!(output.deleted_bytes, preview.selected_bytes);
    assert!(!alpha.exists());
    assert!(!beta.exists());
    assert!(current.exists());
    let records = service.lifecycle_records()?;
    assert!(matches!(
        records.first().map(|record| &record.event),
        Some(LocalSessionLifecycleEvent::RetentionBatchPlanned(_))
    ));
    assert!(matches!(
        records.last().map(|record| &record.event),
        Some(LocalSessionLifecycleEvent::RetentionBatchCompleted(_))
    ));
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record.event, LocalSessionLifecycleEvent::DeleteCompleted(_)))
            .count(),
        2
    );
    let recovery = service.lifecycle_recovery()?;
    assert!(recovery.iter().any(|entry| {
        entry.operation_id == output.operation_id
            && entry.kind == LocalSessionLifecycleOperationKind::Retention
            && entry.status == LocalSessionLifecycleRecoveryStatus::Completed
    }));
    Ok(())
}

#[test]
fn retention_apply_detects_late_candidate_drift_before_deleting_any_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let alpha = sessions.join("session-alpha.jsonl");
    let beta = sessions.join("session-beta.jsonl");
    finalized_session(&alpha, "alpha")?;
    finalized_session(&beta, "beta")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let preview = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: Some(0),
            max_bytes: None,
            expire_older_than_ms: None,
        },
        &[],
        1_000_000_000_000_000,
    )?;
    let drifted = preview
        .candidates
        .last()
        .expect("two candidates")
        .delete_preview
        .source_path
        .clone();
    let store = JsonlSessionStore::new(&drifted)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("late")))?;
    drop(store);

    let error = service
        .apply_retention(&preview, &[], 5678)
        .expect_err("batch drift must fail before delete");

    assert!(error.to_string().contains("changed"));
    assert!(alpha.exists());
    assert!(beta.exists());
    assert!(service.lifecycle_records()?.is_empty());
    Ok(())
}

#[test]
fn retention_apply_rejects_a_tampered_batch_preview_before_journal_or_delete() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "keep")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let mut preview = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: Some(0),
            max_bytes: None,
            expire_older_than_ms: None,
        },
        &[],
        1_000_000_000_000_000,
    )?;
    preview.selected_bytes = preview.selected_bytes.saturating_add(1);

    let error = service
        .apply_retention(&preview, &[], 5678)
        .expect_err("tampered batch preview must fail");

    assert!(error.to_string().contains("digest"));
    assert!(source.exists());
    assert!(service.lifecycle_records()?.is_empty());
    Ok(())
}

#[test]
fn retention_preview_reports_unsatisfied_quota_when_every_session_is_protected_or_pinned()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let pinned = sessions.join("session-pinned.jsonl");
    let current = sessions.join("session-current.jsonl");
    finalized_session(&pinned, "pinned")?;
    finalized_session(&current, "current")?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    service.set_session_pin(&pinned, true, 100)?;

    let preview = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: Some(0),
            max_bytes: Some(0),
            expire_older_than_ms: Some(0),
        },
        std::slice::from_ref(&current),
        1_000_000_000_000_000,
    )?;

    assert!(preview.candidates.is_empty());
    assert!(!preview.constraints_satisfied);
    assert_eq!(preview.pinned_sessions, 1);
    assert_eq!(preview.protected_sessions, 1);
    Ok(())
}

#[test]
fn retention_preview_excludes_a_session_with_an_active_writer_lease() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let active = sessions.join("session-active.jsonl");
    let inactive = sessions.join("session-inactive.jsonl");
    finalized_session(&active, "active")?;
    finalized_session(&inactive, "inactive")?;
    let active_store = JsonlSessionStore::new(&active)?;
    active_store.append(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        Default::default(),
    )))?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let preview = service.preview_retention(
        SessionRetentionPolicy {
            max_sessions: Some(0),
            max_bytes: None,
            expire_older_than_ms: None,
        },
        &[],
        1_000_000_000_000_000,
    )?;

    assert_eq!(preview.protected_sessions, 1);
    assert_eq!(preview.candidates.len(), 1);
    assert_eq!(
        preview.candidates[0].delete_preview.source_path,
        fs::canonicalize(&inactive)?
    );
    assert!(!preview.constraints_satisfied);
    assert!(active.exists());
    drop(active_store);
    Ok(())
}
