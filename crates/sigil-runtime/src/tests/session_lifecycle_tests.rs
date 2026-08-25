#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use serde_json::json;
use sigil_kernel::{
    AssistantMessageKind, ContextBodyRef, ContextInclusionReason, ContextItem, ContextSensitivity,
    ContextSource, ContextTrustLevel, ControlEntry, ConversationInputKind,
    ConversationInputPromotedEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputTarget, ConversationQueueDurableProjection, DurableEventType, EventClass,
    ImageAttachment, ImageMimeType, JsonlSessionStore, MemoryConfig, ModelMessage,
    RuntimeContextCandidates, Session, SessionLogEntry, ToolCall,
    conversation_promotion_capability_digest, project_conversation_prompt_for_persistence,
};

use super::*;

#[derive(Debug)]
struct TestScratchProvider {
    root: PathBuf,
}

#[derive(Debug)]
struct TestScratchLease;

impl sigil_tools_builtin::ScratchNamespaceProviderLease for TestScratchLease {}

impl sigil_tools_builtin::ScratchNamespaceProvider for TestScratchProvider {
    fn acquire(
        &self,
        _session_key: &str,
    ) -> Result<Box<dyn sigil_tools_builtin::ScratchNamespaceProviderLease>> {
        Ok(Box::new(TestScratchLease))
    }

    fn session_scratch_dir(&self, session_scope_id: Option<&str>) -> PathBuf {
        self.root
            .join("sessions")
            .join(sigil_tools_builtin::session_scratch_key(session_scope_id))
    }

    fn ensure_session_scratch(
        &self,
        _session_scope_id: Option<&str>,
        _quota: &sigil_tools_builtin::ScratchQuota,
    ) -> Result<sigil_tools_builtin::SessionScratchProvision> {
        Err(anyhow!("test provider does not provision scratch"))
    }

    fn measure_scratch_usage(
        &self,
        _session_key: &str,
    ) -> Result<sigil_tools_builtin::ScratchUsage> {
        Err(anyhow!("test provider does not measure scratch"))
    }

    fn gc_scratch_namespaces(
        &self,
        _control: &sigil_tools_builtin::ScratchNamespaceControl,
        _config: &sigil_tools_builtin::ScratchGcConfig,
        _now_ms: u64,
    ) -> Result<sigil_tools_builtin::ScratchGcReport> {
        Err(anyhow!("test provider does not gc scratch"))
    }

    fn delete_session_scratch_namespace(
        &self,
        session_scope_id: Option<&str>,
        control: &sigil_tools_builtin::ScratchNamespaceControl,
    ) -> Result<sigil_tools_builtin::ScratchDeleteOutcome> {
        let key = sigil_tools_builtin::session_scratch_key(session_scope_id);
        if control.namespaces.is_leased(&key) {
            return Ok(sigil_tools_builtin::ScratchDeleteOutcome::SkippedLeased);
        }
        let path = self.session_scratch_dir(session_scope_id);
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(sigil_tools_builtin::ScratchDeleteOutcome::Deleted),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(sigil_tools_builtin::ScratchDeleteOutcome::NotPresent)
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Default)]
struct ProjectionNoticeRecorder {
    notices: Mutex<Vec<sigil_kernel::session::ActiveProjectionNotice>>,
}

impl sigil_kernel::session::ActiveProjectionObserver for ProjectionNoticeRecorder {
    fn active_projection_changed(&self, notice: sigil_kernel::session::ActiveProjectionNotice) {
        self.notices
            .lock()
            .expect("projection notice recorder lock is available")
            .push(notice);
    }
}

fn finalized_session(path: &Path, prompt: &str) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
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

fn lifecycle_internal_context_fixture() -> RuntimeContextCandidates {
    let body = "export-internal context snapshot body";
    let mut candidates = RuntimeContextCandidates::new();
    candidates.items.push(ContextItem {
        id: "lifecycle-context-fixture".to_owned(),
        source: ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: Some("lifecycle-context-snapshot".to_owned()),
        token_cost: sigil_kernel::estimate_context_token_cost(body),
        score: Some(100.0),
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: ContextBodyRef::inline(body),
    });
    candidates
        .snippets
        .insert("lifecycle-context-fixture".to_owned(), body.to_owned());
    candidates
}

#[cfg(unix)]
#[test]
fn lifecycle_journal_creates_and_repairs_owner_only_data_and_lease_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    let exports = temp.path().join("exports");
    fs::create_dir(&sessions)?;
    let journal_path = temp.path().join("session-lifecycle-v1.jsonl");
    let lease_path = temp.path().join("session-lifecycle-v1.jsonl.writer-lock");
    fs::write(&journal_path, b"")?;
    fs::write(&lease_path, b"")?;
    fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o644))?;
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o664))?;
    let service = LocalSessionLifecycleService::new("workspace-1", &sessions, exports);

    service.lifecycle_journal().append(
        "session-pin:permissions",
        100,
        LocalSessionLifecycleEvent::PinChanged(LocalSessionPinJournalBinding {
            source_session_ref: sigil_kernel::SessionRef::new_relative("session-test.jsonl")?,
            source_session_id: "session-test".to_owned(),
            pinned: true,
        }),
    )?;

    assert_eq!(
        fs::metadata(&journal_path)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&lease_path)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

fn append_v2_tool_artifact(
    path: &Path,
    call_id: &str,
    body: &str,
) -> Result<sigil_kernel::session::ToolArtifactDescriptorV1> {
    let store = JsonlSessionStore::new(path)?;
    let artifact_store = sigil_kernel::ToolArtifactStore::for_session_store(&store);
    let (recorded, _) = sigil_kernel::ToolResultRecordedV3::capture(
        &sigil_kernel::ToolResult::ok(
            call_id,
            "shell",
            body,
            sigil_kernel::ToolResultMeta::default(),
        ),
        Some(&artifact_store),
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let descriptor = recorded
        .artifact
        .descriptor()
        .context("published tool artifact")?
        .clone();
    store.append(&SessionLogEntry::ToolResultV3(recorded))?;
    let source_event_id = JsonlSessionStore::read_event_records(path)?
        .last()
        .context("tool result event")?
        .event_id()
        .to_owned();
    artifact_store.bind_source_event(&descriptor.artifact_ref, &source_event_id)?;
    Ok(descriptor)
}

#[test]
fn local_session_catalog_projects_only_bounded_v2_direct_children() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let ready = sessions.join("session-ready.jsonl");
    finalized_session(&ready, "Explain the repository")?;
    fs::write(
        sessions.join("session-invalid.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("invalid")))?
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
        entry.path.ends_with("session-invalid.jsonl")
            && entry.state == LocalSessionCatalogState::Invalid
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
        sessions.join("session-invalid.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("invalid")))?
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
            &sigil_kernel::SessionRef::new_relative("session-invalid.jsonl")?,
            "invalid"
        ),
        Err(LocalSessionReopenError::NotReady {
            state: LocalSessionCatalogState::Invalid
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
fn promoted_user_projects_once_into_catalog_and_export_without_user_message_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-promoted.jsonl");
    let store = JsonlSessionStore::new(&source)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "chat".to_owned(),
        resolved_model_route: None,
    }))?;
    let queue_id = ConversationInputQueueId::new("export-promoted")?;
    let prompt = project_conversation_prompt_for_persistence("Export promoted history");
    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: prompt.prompt_hash.clone(),
            prompt: prompt.safe_prompt.clone(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        }),
    ))?;
    let revision = ConversationQueueDurableProjection::from_records(
        &JsonlSessionStore::read_event_records(&source)?,
    )?
    .revision
    .expect("queued event advances revision");
    let mut durable_user_message = ModelMessage::user(prompt.safe_prompt.clone());
    durable_user_message.id = "export-promoted-message".to_owned();
    store.append_conversation_input_promoted(ConversationInputPromotedEntry {
        queue_id,
        expected_queue_revision: revision,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: false,
        durable_user_message: durable_user_message.clone(),
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "export-promoted-run".to_owned(),
        promoted_at_ms: 2,
    })?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("exported".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    store.append(&SessionLogEntry::Assistant(assistant.clone()))?;
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({"run_status": "completed", "final_message_id": assistant.id}),
    )?;
    let records = JsonlSessionStore::read_event_records(&source)?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0
    );

    let exports = temp.path().join("exports");
    let service = LocalSessionLifecycleService::new("workspace-1", &sessions, &exports);
    let catalog = service.catalog()?;
    let entry = catalog.entries.first().expect("catalog entry");
    assert_eq!(entry.title.as_deref(), Some("Export promoted history"));
    assert_eq!(entry.transcript_message_count, 2);
    let output = service.export_session(&source, None, 1234)?;
    assert_eq!(output.message_count, 2);
    let artifact: SessionExportV1 = serde_json::from_slice(&fs::read(output.path)?)?;
    assert_eq!(
        artifact
            .payload
            .messages
            .iter()
            .filter(|message| {
                message.role == sigil_kernel::MessageRole::User
                    && message.message_id == durable_user_message.id
                    && message.content == durable_user_message.content
            })
            .count(),
        1
    );
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
        resolved_model_route: None,
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
fn session_export_requires_explicit_artifact_completeness_policy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-artifact.jsonl");
    finalized_session(&source, "artifact export")?;
    let descriptor = append_v2_tool_artifact(&source, "call-export", "artifact body")?;
    let exports = temp.path().join("exports");
    let service = LocalSessionLifecycleService::new("workspace-1", &sessions, &exports);

    let bounded = service.export_session(&source, None, 1_234)?;
    let bounded_artifact: SessionExportV1 = serde_json::from_slice(&fs::read(&bounded.path)?)?;
    assert_eq!(
        bounded_artifact.payload.tool_artifacts.mode,
        SessionArtifactExportModeV1::BoundedTranscript
    );
    assert_eq!(
        bounded.artifact_completeness,
        SessionArtifactExportCompletenessV1::Incomplete
    );
    assert_eq!(
        bounded_artifact
            .payload
            .tool_artifacts
            .omitted_artifact_count,
        1
    );
    assert!(bounded_artifact.payload.tool_artifacts.artifacts.is_empty());

    let included = service.export_session_with_artifacts(
        &source,
        None,
        1_235,
        SessionArtifactExportModeV1::IncludeArtifacts,
    )?;
    let included_artifact: SessionExportV1 = serde_json::from_slice(&fs::read(&included.path)?)?;
    assert_eq!(
        included.artifact_completeness,
        SessionArtifactExportCompletenessV1::Complete
    );
    assert_eq!(included.included_artifact_count, 1);
    assert_eq!(
        included_artifact.payload.tool_artifacts.artifacts[0]
            .descriptor
            .artifact_ref,
        descriptor.artifact_ref
    );
    assert_eq!(
        included_artifact.payload.tool_artifacts.artifacts[0].body_base64,
        "YXJ0aWZhY3QgYm9keQ=="
    );

    let rejected = service
        .export_session_with_artifacts(
            &source,
            None,
            1_236,
            SessionArtifactExportModeV1::RejectIfIncomplete,
        )
        .expect_err("reject mode must not silently omit tool artifacts");
    assert!(rejected.to_string().contains("would be incomplete"));
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
        resolved_model_route: None,
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
fn safe_session_export_hides_provider_visible_context_v2_snapshots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-context-v2.jsonl");
    let store = JsonlSessionStore::new(&source)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.ensure_identity_entry()?;
    session.append_user_message(ModelMessage::user("inspect the export contract"))?;
    session.build_request_with_transient_messages_and_context(
        temp.path(),
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
        None,
        &[],
        lifecycle_internal_context_fixture(),
    )?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;

    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let output = service.export_session(&source, None, 1_234)?;
    let text = fs::read_to_string(&output.path)?;
    assert!(!text.contains("export-internal context snapshot body"));
    let artifact: SessionExportV1 = serde_json::from_str(&text)?;
    assert_eq!(artifact.payload.messages.len(), 2);
    assert_eq!(
        artifact.payload.messages[0].content.as_deref(),
        Some("inspect the export contract")
    );
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
fn session_delete_reclaims_scratch_namespace_under_lease_registry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source_a = sessions.join("session-scratch-a.jsonl");
    let source_b = sessions.join("session-scratch-b.jsonl");
    finalized_session(&source_a, "delete scratch a")?;
    finalized_session(&source_b, "delete scratch b")?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let control = sigil_tools_builtin::ScratchNamespaceControl::from_provider(Arc::new(
        TestScratchProvider {
            root: scratch_root.clone(),
        },
    ));
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_scratch_cleanup(control.clone());

    // Session A: a held lease keeps the namespace while deletion still succeeds.
    let preview_a = service.preview_delete(&source_a, &[])?;
    let namespace_a = scratch_root
        .join("sessions")
        .join(&preview_a.source_session_id);
    fs::create_dir_all(&namespace_a)?;
    fs::write(namespace_a.join("blob"), b"scratch-a")?;
    let lease = control
        .namespaces
        .acquire(&sigil_tools_builtin::session_scratch_key(Some(
            &preview_a.source_session_id,
        )));
    service.apply_delete(&preview_a, &[], 5678)?;
    assert!(!source_a.exists());
    assert!(
        namespace_a.exists(),
        "leased namespace must survive deletion"
    );
    drop(lease);

    // Session B: without a lease the namespace is reclaimed with the session.
    let preview_b = service.preview_delete(&source_b, &[])?;
    let namespace_b = scratch_root
        .join("sessions")
        .join(&preview_b.source_session_id);
    fs::create_dir_all(&namespace_b)?;
    fs::write(namespace_b.join("blob"), b"scratch-b")?;
    service.apply_delete(&preview_b, &[], 5679)?;
    assert!(!source_b.exists());
    assert!(
        !namespace_b.exists(),
        "deleted session scratch namespace must be reclaimed"
    );
    Ok(())
}

#[test]
fn session_delete_tombstones_artifacts_before_grace_prune() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-artifact.jsonl");
    finalized_session(&source, "delete artifact session")?;
    let descriptor = append_v2_tool_artifact(&source, "call-delete", "retained evidence")?;
    let artifact_store = sigil_kernel::ToolArtifactStore::for_session_path(&source);
    assert_eq!(artifact_store.read_all(&descriptor)?, b"retained evidence");
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let preview = service.preview_delete(&source, &[])?;
    assert!(preview.resource_bytes > 0);
    let lock_name = format!("{}.lock", descriptor.artifact_ref.artifact_id);
    #[cfg(not(windows))]
    let active_read = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(artifact_store.root().join("locks").join(&lock_name))?;
    #[cfg(not(windows))]
    active_read.try_lock_shared()?;

    let output = service.apply_delete(&preview, &[], 5_678)?;

    let tombstone = sessions.join(".session-trash").join(&output.tombstone_id);
    // Windows cannot rename a directory while an open handle exists below it. Acquire the
    // equivalent read lease after the atomic tombstone move there; pruning must still honor it.
    #[cfg(windows)]
    let active_read = std::fs::OpenOptions::new().read(true).write(true).open(
        tombstone
            .join("resources")
            .join("artifacts")
            .join("locks")
            .join(&lock_name),
    )?;
    #[cfg(windows)]
    active_read.try_lock_shared()?;
    assert!(!source.exists());
    assert!(tombstone.join("session.jsonl").is_file());
    assert!(
        tombstone
            .join("resources")
            .join("artifacts")
            .join("refs")
            .join(format!("{}.json", descriptor.artifact_ref.artifact_id))
            .is_file()
    );
    assert_eq!(output.tombstoned_resource_bytes, preview.resource_bytes);
    let early = service.prune_delete_tombstones(5_678)?;
    assert_eq!(early.removed_tombstones, 0);
    assert!(tombstone.exists());
    let active = service.prune_delete_tombstones(u64::MAX)?;
    assert_eq!(active.removed_tombstones, 0);
    assert!(tombstone.exists());
    drop(active_read);
    let pruned = service.prune_delete_tombstones(u64::MAX)?;
    assert_eq!(pruned.removed_tombstones, 1);
    assert_eq!(
        pruned.removed_bytes,
        preview.source_bytes + preview.resource_bytes
    );
    assert!(!tombstone.exists());
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

    assert!(error.to_string().contains("session writer lease is busy"));
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
        resource_tree_sha256: preview.resource_tree_sha256.clone(),
        resource_bytes: preview.resource_bytes,
        tombstone_id: "session-delete-incomplete".to_owned(),
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
fn session_pin_waits_for_brief_internal_maintenance_instead_of_failing_busy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "pin after maintenance")?;
    let journal = temp.path().join("lifecycle.jsonl");
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_lifecycle_journal_path(&journal);
    let maintenance_path = temp.path().join("lifecycle.jsonl.maintenance-lock");
    let maintenance = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(maintenance_path)?;
    maintenance.try_lock()?;
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        drop(maintenance);
    });

    service.set_session_pin(&source, true, 100)?;
    release
        .join()
        .map_err(|_| anyhow!("maintenance release thread panicked"))?;
    assert!(service.catalog()?.entries[0].pinned);
    Ok(())
}

#[test]
fn session_pin_becomes_whole_store_hold_for_manifest_only_artifact_gc() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(&source, "pin artifact")?;
    let source_ref = sigil_kernel::SessionRef::new_relative("session-source.jsonl")?;
    let source_session_id = JsonlSessionStore::read_event_records(&source)?
        .first()
        .context("session identity")?
        .session_id()
        .to_owned();
    let store = sigil_kernel::ToolArtifactStore::for_session_path(&source);
    let orphan = store.capture_text(
        "call-orphan",
        "shell",
        "orphan until pinned session is released",
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    service.set_session_pin(&source, true, 100)?;

    let pinned_report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;

    assert_eq!(pinned_report.tombstoned_manifests, 0);
    assert_eq!(
        store.read_all(&orphan)?,
        b"orphan until pinned session is released"
    );
    service.set_session_pin(&source, false, 101)?;
    let released_report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(released_report.tombstoned_manifests, 1);
    assert!(store.resolve(&orphan.artifact_ref).is_err());
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
fn manual_session_name_always_wins_over_generated_title() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-source.jsonl");
    finalized_session(
        &source,
        "Please investigate the stale desktop conversation header",
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let listed = service.catalog()?.entries.remove(0);
    let session_id = listed.session_id.expect("durable identity");

    let generated = service.record_generated_title(
        &listed.session_ref,
        &session_id,
        "修复桌面会话标题同步",
        "deepseek",
        "deepseek-v4-flash",
        Some(128),
        Some(12),
        200,
    )?;
    assert!(matches!(
        generated.event,
        LocalSessionLifecycleEvent::GeneratedTitleChanged(
            LocalSessionGeneratedTitleJournalBinding { ref title, .. }
        ) if title == "修复桌面会话标题同步"
    ));
    assert_eq!(
        service.catalog()?.entries[0].title.as_deref(),
        Some("修复桌面会话标题同步")
    );

    service.rename_session(&listed.session_ref, &session_id, "Desktop title bug", 201)?;
    service.record_generated_title(
        &listed.session_ref,
        &session_id,
        "稍后完成的自动标题",
        "deepseek",
        "deepseek-v4-flash",
        Some(128),
        Some(8),
        202,
    )?;
    assert_eq!(
        service.catalog()?.entries[0].title.as_deref(),
        Some("Desktop title bug")
    );
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

#[test]
fn artifact_gc_appends_durable_disable_before_delete_and_expired_after() -> Result<()> {
    // RFC-0062 9.4: the lifecycle GC writes generation-guarded availability transitions around
    // the tombstone so no crash window advertises a retrievable artifact whose body is gone.
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-disable.jsonl");
    finalized_session(&source, "disable-before-delete")?;
    let source_ref = sigil_kernel::SessionRef::new_relative("session-disable.jsonl")?;
    let source_session_id = JsonlSessionStore::read_event_records(&source)?
        .first()
        .context("session identity")?
        .session_id()
        .to_owned();
    let store = sigil_kernel::ToolArtifactStore::for_session_path(&source);
    let orphan = store.capture_text(
        "call-gc-disable",
        "shell",
        "grace expired orphan",
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.tombstoned_manifests, 1);
    assert_eq!(report.tombstoned_refs.len(), 1);
    assert_eq!(report.tombstoned_refs[0], orphan.artifact_ref);

    let records = JsonlSessionStore::read_event_records(&source)?;
    let entries = records
        .iter()
        .map(|record| record.session_log_entry())
        .collect::<Result<Vec<_>>>()?;
    let plans = entries
        .iter()
        .filter_map(|entry| match entry {
            Some(sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::ToolArtifactTombstonePlan(plan),
            )) => Some(plan),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(plans.len(), 1, "one durable tombstone plan before deletion");
    assert_eq!(plans[0].expected_generation, 1);
    assert_eq!(plans[0].artifact_ref, orphan.artifact_ref);
    let transitions = entries
        .iter()
        .filter_map(|entry| match entry {
            Some(sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::ToolArtifactAvailabilityChanged(change),
            )) => Some(change),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transitions.len(), 2, "disable then expired");
    assert_eq!(transitions[0].generation, 1);
    assert_eq!(
        transitions[0].previous,
        sigil_kernel::ToolArtifactAvailabilityStateV1::Available
    );
    assert_eq!(
        transitions[0].next,
        sigil_kernel::ToolArtifactAvailabilityStateV1::DisabledPendingDelete
    );
    assert_eq!(transitions[1].generation, 2);
    assert_eq!(
        transitions[1].next,
        sigil_kernel::ToolArtifactAvailabilityStateV1::Expired
    );
    assert!(store.resolve(&orphan.artifact_ref).is_err());
    Ok(())
}

#[test]
fn noop_artifact_gc_does_not_publish_a_projection_change() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-noop-gc.jsonl");
    finalized_session(&source, "noop-gc")?;
    let source_ref = sigil_kernel::SessionRef::new_relative("session-noop-gc.jsonl")?;
    let source_store = JsonlSessionStore::new(&source)?;
    let source_session_id = source_store
        .active_projection_snapshot()?
        .frontier()
        .session_id()
        .to_owned();
    let artifact_store = sigil_kernel::ToolArtifactStore::for_session_store(&source_store);
    let retained = artifact_store.capture_text(
        "call-retained",
        "shell",
        "still reachable",
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let recorder = Arc::new(ProjectionNoticeRecorder::default());
    let _subscription = source_store.register_active_projection_observer(recorder.clone());
    let publications_before = source_store.active_projection_metrics().publication_total;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1 {
            active_result_refs: [retained.artifact_ref].into_iter().collect(),
            ..sigil_kernel::ToolArtifactGcRootsV1::default()
        },
        u64::MAX,
    )?;

    assert_eq!(report.tombstoned_manifests, 0);
    assert!(
        recorder
            .notices
            .lock()
            .expect("projection notice recorder lock is available")
            .is_empty(),
        "maintenance loading an unchanged canonical session must not re-arm GC"
    );
    assert_eq!(
        source_store.active_projection_metrics().publication_total,
        publications_before
    );
    Ok(())
}

#[test]
fn artifact_gc_accepts_and_retires_a_complete_zero_byte_manifest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-empty-artifact.jsonl");
    finalized_session(&source, "empty-artifact")?;
    let source_ref = sigil_kernel::SessionRef::new_relative("session-empty-artifact.jsonl")?;
    let source_session_id = JsonlSessionStore::read_event_records(&source)?
        .first()
        .context("session identity")?
        .session_id()
        .to_owned();
    let artifact_store = sigil_kernel::ToolArtifactStore::for_session_path(&source);
    let empty = artifact_store.capture_text(
        "call-empty-gc",
        "shell",
        "",
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;

    assert_eq!(report.tombstoned_manifests, 1);
    assert_eq!(report.tombstoned_refs, vec![empty.artifact_ref]);
    Ok(())
}

fn gc_crash_fixture(
    temp: &tempfile::TempDir,
    stem: &str,
) -> Result<(
    std::path::PathBuf,
    sigil_kernel::SessionRef,
    String,
    sigil_kernel::ToolArtifactStore,
    sigil_kernel::ToolArtifactRefV1,
)> {
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join(format!("{stem}.jsonl"));
    finalized_session(&source, stem)?;
    let source_ref = sigil_kernel::SessionRef::new_relative(format!("{stem}.jsonl"))?;
    let source_session_id = JsonlSessionStore::read_event_records(&source)?
        .first()
        .context("session identity")?
        .session_id()
        .to_owned();
    let store = sigil_kernel::ToolArtifactStore::for_session_path(&source);
    let artifact = store.capture_text(
        &format!("call-{stem}"),
        "shell",
        "crash-window body",
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    Ok((
        source,
        source_ref,
        source_session_id,
        store,
        artifact.artifact_ref,
    ))
}

fn append_durable_disable_with_plan(
    session_path: &std::path::Path,
    artifact_ref: &sigil_kernel::ToolArtifactRefV1,
    now_unix_ms: u64,
) -> Result<()> {
    // Mirrors the runtime GC prefix: the disable transition and its tombstone plan land in one
    // atomic durable batch BEFORE any physical move.
    let store = JsonlSessionStore::new(session_path)?;
    let mut session = Session::load_from_store("session", "model", store)?;
    session.append_availability_transitions_with_tombstone_plans(
        vec![(
            artifact_ref.clone(),
            sigil_kernel::ToolArtifactAvailabilityStateV1::Available,
            sigil_kernel::ToolArtifactAvailabilityStateV1::DisabledPendingDelete,
            sigil_kernel::ToolArtifactAvailabilityReasonV1::GcDisable,
        )],
        vec![sigil_kernel::ToolArtifactTombstonePlannedV1 {
            schema_version: sigil_kernel::TOOL_ARTIFACT_TOMBSTONE_PLAN_SCHEMA_VERSION,
            artifact_ref: artifact_ref.clone(),
            expected_generation: 1,
            planned_at_ms: now_unix_ms,
        }],
        now_unix_ms,
    )
}

fn availability_and_plan_counts(session_path: &std::path::Path) -> Result<(usize, usize)> {
    let records = JsonlSessionStore::read_event_records(session_path)?;
    let mut transitions = 0usize;
    let mut plans = 0usize;
    for record in &records {
        if let Some(entry) = record.session_log_entry()? {
            match entry {
                SessionLogEntry::Control(ControlEntry::ToolArtifactAvailabilityChanged(_)) => {
                    transitions += 1;
                }
                SessionLogEntry::Control(ControlEntry::ToolArtifactTombstonePlan(_)) => {
                    plans += 1;
                }
                _ => {}
            }
        }
    }
    Ok((transitions, plans))
}

fn move_to_trash(
    store: &sigil_kernel::ToolArtifactStore,
    artifact_ref: &sigil_kernel::ToolArtifactRefV1,
    include_blob: bool,
) -> Result<()> {
    // Simulates the physical phase of the store GC: the manifest (and optionally the blob) are
    // moved into trash while the ledger still says DisabledPendingDelete.
    let trash = store.root().join("trash").join("simulated-crash");
    fs::create_dir_all(&trash)?;
    let descriptor = if include_blob {
        Some(store.resolve(artifact_ref)?)
    } else {
        None
    };
    let manifest = store
        .root()
        .join("refs")
        .join(format!("{}.json", artifact_ref.artifact_id));
    fs::rename(
        &manifest,
        trash.join(format!("{}.json", artifact_ref.artifact_id)),
    )?;
    if let Some(descriptor) = descriptor {
        let digest = descriptor
            .content_sha256
            .strip_prefix("sha256:")
            .context("content hash")?;
        let blob = store
            .root()
            .join("blobs")
            .join(&digest[..2])
            .join(format!("{digest}.blob"));
        fs::rename(&blob, trash.join(format!("{digest}.blob")))?;
    }
    Ok(())
}

fn run_gc_recovery_twice(
    service: &LocalSessionLifecycleService,
    source_ref: &sigil_kernel::SessionRef,
    source_session_id: &str,
    source: &std::path::Path,
) -> Result<()> {
    // Every crash point must converge on the first recovery run and stay terminal across two
    // further retries: no new transitions, no new plans, no error.
    let baseline = availability_and_plan_counts(source)?;
    service.garbage_collect_session_artifacts(
        source_ref,
        source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    service.garbage_collect_session_artifacts(
        source_ref,
        source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(
        availability_and_plan_counts(source)?,
        baseline,
        "recovery retries must be idempotent"
    );
    Ok(())
}

#[test]
fn artifact_gc_recovers_after_crash_post_disable_append() -> Result<()> {
    // RFC-0062 16.2 crash point 1: the durable disable + tombstone plan landed, the physical
    // move never ran. Recovery must resume deletion and complete Expired.
    let temp = tempfile::tempdir()?;
    let (source, source_ref, source_session_id, store, artifact_ref) =
        gc_crash_fixture(&temp, "session-crash-post-disable")?;
    append_durable_disable_with_plan(&source, &artifact_ref, u64::MAX)?;
    let service = LocalSessionLifecycleService::new(
        "workspace-1",
        temp.path().join("sessions"),
        temp.path().join("exports"),
    );

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.tombstoned_manifests, 1);
    assert_eq!(report.tombstoned_refs.len(), 1);
    assert_eq!(
        availability_and_plan_counts(&source)?,
        (2, 1),
        "disable then expired, with exactly one durable plan"
    );
    assert!(store.resolve(&artifact_ref).is_err());
    run_gc_recovery_twice(&service, &source_ref, &source_session_id, &source)?;
    Ok(())
}

#[test]
fn artifact_gc_recovers_after_crash_post_manifest_tombstone() -> Result<()> {
    // RFC-0062 16.2 crash point 2: the manifest moved into trash, the blob is still live, and
    // Expired was never appended. Recovery must complete the terminal transition from the plan.
    let temp = tempfile::tempdir()?;
    let (source, source_ref, source_session_id, store, artifact_ref) =
        gc_crash_fixture(&temp, "session-crash-post-manifest")?;
    append_durable_disable_with_plan(&source, &artifact_ref, u64::MAX)?;
    move_to_trash(&store, &artifact_ref, false)?;
    let service = LocalSessionLifecycleService::new(
        "workspace-1",
        temp.path().join("sessions"),
        temp.path().join("exports"),
    );

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.tombstoned_manifests, 0, "manifest was already moved");
    assert_eq!(
        availability_and_plan_counts(&source)?,
        (2, 1),
        "reconciliation completed Expired from the durable plan"
    );
    assert!(store.resolve(&artifact_ref).is_err());
    run_gc_recovery_twice(&service, &source_ref, &source_session_id, &source)?;
    Ok(())
}

#[test]
fn artifact_gc_recovers_after_crash_between_body_delete_and_expired_append() -> Result<()> {
    // RFC-0062 16.2 crash point 3: manifest AND body are gone while the ledger still says
    // DisabledPendingDelete. Without the durable plan the ledger would be stuck; recovery must
    // append the terminal Expired.
    let temp = tempfile::tempdir()?;
    let (source, source_ref, source_session_id, store, artifact_ref) =
        gc_crash_fixture(&temp, "session-crash-post-body-delete")?;
    append_durable_disable_with_plan(&source, &artifact_ref, u64::MAX)?;
    move_to_trash(&store, &artifact_ref, true)?;
    let service = LocalSessionLifecycleService::new(
        "workspace-1",
        temp.path().join("sessions"),
        temp.path().join("exports"),
    );

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.tombstoned_manifests, 0, "manifest was already moved");
    assert_eq!(
        availability_and_plan_counts(&source)?,
        (2, 1),
        "reconciliation completed Expired from the durable plan"
    );
    assert!(store.resolve(&artifact_ref).is_err());
    run_gc_recovery_twice(&service, &source_ref, &source_session_id, &source)?;
    Ok(())
}

#[test]
fn artifact_gc_recovers_after_crash_post_expired_append() -> Result<()> {
    // RFC-0062 16.2 crash point 4: the terminal Expired is already durable, only the lifecycle
    // journal completion is missing. Recovery must not re-append or regress the ledger.
    let temp = tempfile::tempdir()?;
    let (source, source_ref, source_session_id, store, artifact_ref) =
        gc_crash_fixture(&temp, "session-crash-post-expired")?;
    append_durable_disable_with_plan(&source, &artifact_ref, u64::MAX)?;
    move_to_trash(&store, &artifact_ref, true)?;
    {
        let store = JsonlSessionStore::new(&source)?;
        let mut session = Session::load_from_store("session", "model", store)?;
        session.append_artifact_availability_transition(
            &artifact_ref,
            1,
            sigil_kernel::ToolArtifactAvailabilityStateV1::DisabledPendingDelete,
            sigil_kernel::ToolArtifactAvailabilityStateV1::Expired,
            sigil_kernel::ToolArtifactAvailabilityReasonV1::GcExpired,
            u64::MAX,
        )?;
    }
    let service = LocalSessionLifecycleService::new(
        "workspace-1",
        temp.path().join("sessions"),
        temp.path().join("exports"),
    );

    service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(
        availability_and_plan_counts(&source)?,
        (2, 1),
        "already terminal ledger must not grow"
    );
    assert!(store.resolve(&artifact_ref).is_err());
    run_gc_recovery_twice(&service, &source_ref, &source_session_id, &source)?;
    Ok(())
}

#[test]
fn artifact_gc_waits_for_active_reader_lease_before_deletion() -> Result<()> {
    // RFC-0062 9.4: an old-generation reader lease must drain before the body is deleted; the
    // disable event already rejects new readers. GC skips the locked ref, then completes once
    // the lease is released.
    let temp = tempfile::tempdir()?;
    let (source, source_ref, source_session_id, store, artifact_ref) =
        gc_crash_fixture(&temp, "session-gc-active-reader")?;
    let service = LocalSessionLifecycleService::new(
        "workspace-1",
        temp.path().join("sessions"),
        temp.path().join("exports"),
    );

    let locks_dir = store.root().join("locks");
    fs::create_dir_all(&locks_dir)?;
    let lock_path = locks_dir.join(format!("{}.lock", artifact_ref.artifact_id));
    let reader = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    fs2::FileExt::try_lock_shared(&reader).context("reader lease")?;

    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.skipped_active_reads, 1);
    assert_eq!(report.tombstoned_manifests, 0);
    assert_eq!(
        availability_and_plan_counts(&source)?,
        (1, 1),
        "only the disable landed; Expired must wait for the reader drain"
    );
    assert!(store.resolve(&artifact_ref).is_ok(), "body must survive");

    reader.unlock().context("release reader lease")?;
    drop(reader);
    let report = service.garbage_collect_session_artifacts(
        &source_ref,
        &source_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.tombstoned_manifests, 1);
    assert_eq!(
        availability_and_plan_counts(&source)?,
        (2, 1),
        "deletion and Expired complete after the lease drains"
    );
    assert!(store.resolve(&artifact_ref).is_err());
    run_gc_recovery_twice(&service, &source_ref, &source_session_id, &source)?;
    Ok(())
}

#[test]
fn artifact_gc_fails_closed_when_durable_disable_cannot_be_written() -> Result<()> {
    // RFC-0062 9.4: if the session cannot be loaded for the durable disable transition, the
    // physical GC must abort and the artifact body must survive.
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let source = sessions.join("session-corrupt.jsonl");
    // Valid enough for the store to find artifacts, invalid for session load.
    fs::write(&source, b"{not valid jsonl\n")?;
    let source_ref = sigil_kernel::SessionRef::new_relative("session-corrupt.jsonl")?;
    let store = sigil_kernel::ToolArtifactStore::for_session_path(&source);
    let artifact = store.capture_text(
        "call-gc-fail-closed",
        "shell",
        "must survive a failed durable disable",
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"));
    let error = service
        .garbage_collect_session_artifacts(
            &source_ref,
            "any-session",
            sigil_kernel::ToolArtifactGcRootsV1::default(),
            u64::MAX,
        )
        .expect_err("GC must fail closed when durable disable cannot be written");
    assert!(
        format!("{error:#}").contains("artifact GC")
            || format!("{error:#}").contains("session identity"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        store.read_all(&artifact)?,
        b"must survive a failed durable disable",
        "artifact body must survive an aborted GC"
    );
    Ok(())
}

#[test]
fn managed_lifecycle_journal_round_trips_under_admitted_namespace() -> Result<()> {
    use crate::managed_storage_writer::{
        ManagedStorageWriterAdapterV1, StorageWriterChannelV1, grant_for_channel_with_context,
    };
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
    use sigil_kernel::managed_storage::ManagedStorageServiceV1;
    use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };

    let temp = tempfile::tempdir()?;
    let anchor = temp.path().join("state");
    fs::create_dir(&anchor)?;
    #[cfg(unix)]
    fs::set_permissions(&anchor, fs::Permissions::from_mode(0o700))?;
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant_for_channel_with_context(
        StorageWriterChannelV1::SessionLifecycleLog,
        0x76,
        AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0x68; 32]),
        },
        CanonicalHash::from_bytes([0x69; 32]),
    ))?;
    let storage: Arc<dyn ManagedStorageServiceV1> =
        Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([0x68; 32]),
            },
        ));
    let writer = Arc::new(ManagedStorageWriterAdapterV1::with_storage_issuer(
        storage,
        anchor.clone(),
        CanonicalHash::from_bytes([0x69; 32]),
        Arc::new(KernelCapabilityBrokerV1::new()),
    ));
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let service =
        LocalSessionLifecycleService::new("workspace-1", &sessions, temp.path().join("exports"))
            .with_managed_writer(writer, "workspace-1")?;

    service.journal_append(
        "session-pin:managed",
        100,
        LocalSessionLifecycleEvent::PinChanged(LocalSessionPinJournalBinding {
            source_session_ref: sigil_kernel::SessionRef::new_relative("session-test.jsonl")?,
            source_session_id: "session-test".to_owned(),
            pinned: true,
        }),
    )?;

    let records = service.lifecycle_records()?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].operation_id, "session-pin:managed");
    assert!(
        anchor
            .join("managed")
            .join("session-lifecycle-log")
            .join("workspace-1")
            .join("session-lifecycle-v1.jsonl")
            .is_file()
    );
    Ok(())
}

#[test]
fn managed_session_log_source_is_cataloged_and_reopened() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_dir = temp.path().join("legacy-sessions");
    let managed_root = temp.path().join("state/managed/session-log");
    let session_key = "session-managed";
    let managed_session = managed_root.join(session_key).join("records.jsonl");
    fs::create_dir_all(managed_session.parent().expect("managed session parent"))?;
    finalized_session(&managed_session, "managed source")?;
    let expected_session_id = JsonlSessionStore::read_event_records(&managed_session)?
        .first()
        .expect("managed session has a record")
        .session_id()
        .to_owned();

    let service = LocalSessionLifecycleService::new(
        "workspace-managed",
        &session_dir,
        temp.path().join("exports"),
    )
    .with_managed_session_log_root(&managed_root)?;
    let catalog = service.catalog()?;
    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(
        catalog.entries[0].session_ref,
        sigil_kernel::SessionRef::new_relative("session-managed.jsonl")?
    );
    assert_eq!(
        catalog.entries[0].session_id.as_deref(),
        Some(expected_session_id.as_str())
    );

    let binding = service.resolve_session_for_reopen(
        &sigil_kernel::SessionRef::new_relative("session-managed.jsonl")?,
        &expected_session_id,
    )?;
    assert_eq!(binding.session_log_path, managed_session.canonicalize()?);
    Ok(())
}

#[test]
fn managed_session_artifact_gc_uses_authority_roots_without_legacy_sibling() -> Result<()> {
    use crate::managed_storage_writer::{
        ManagedStorageWriterAdapterV1, StorageWriterChannelV1, grant_for_channel_with_context,
    };
    use sigil_kernel::managed_storage::ManagedStorageServiceV1;
    use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };

    let temp = tempfile::tempdir()?;
    let state_anchor = temp.path().join("state");
    fs::create_dir(&state_anchor)?;
    let state_anchor = fs::canonicalize(state_anchor)?;
    let session_dir = temp.path().join("legacy-sessions");
    let managed_root = state_anchor.join("managed/session-log");
    let artifact_store_root = state_anchor.join("managed/artifact-store");
    let artifact_staging_root = state_anchor.join("managed/artifact-staging");
    let managed_session = managed_root.join("session-gc").join("records.jsonl");
    fs::create_dir_all(managed_session.parent().expect("managed session parent"))?;
    finalized_session(&managed_session, "managed artifact gc")?;
    let expected_session_id = JsonlSessionStore::read_event_records(&managed_session)?
        .first()
        .expect("managed session has a record")
        .session_id()
        .to_owned();
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x68; 32]),
    };
    let cutover_manifest_hash = CanonicalHash::from_bytes([0x69; 32]);
    let staging_grant = grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStaging,
        0x76,
        generation,
        cutover_manifest_hash,
    );
    let store_grant = grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStore,
        0x76,
        generation,
        cutover_manifest_hash,
    );
    let lifecycle_grant = grant_for_channel_with_context(
        StorageWriterChannelV1::SessionLifecycleLog,
        0x76,
        generation,
        cutover_manifest_hash,
    );
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(staging_grant.clone())?;
    table.register(store_grant.clone())?;
    table.register(lifecycle_grant)?;
    let storage: Arc<dyn ManagedStorageServiceV1> =
        Arc::new(AuthorityManagedStorageServiceV1::new(table, generation));
    let writer = Arc::new(
        ManagedStorageWriterAdapterV1::new(storage, state_anchor.clone(), cutover_manifest_hash)
            .with_artifact_retire_authority(Arc::new(
                sigil_resource_authority::maintenance::ArtifactRetireAuthorityV1::new(
                    generation,
                    staging_grant.grant_hash,
                    store_grant.grant_hash,
                ),
            )),
    );
    let service = LocalSessionLifecycleService::new(
        "workspace-managed-artifacts",
        &session_dir,
        temp.path().join("exports"),
    )
    .with_managed_writer(writer, "workspace-managed-artifacts")?
    .with_managed_session_log_root(&managed_root)?
    .with_managed_artifact_roots(&artifact_store_root, &artifact_staging_root)?;

    let report = service.garbage_collect_session_artifacts(
        &sigil_kernel::SessionRef::new_relative("session-gc.jsonl")?,
        &expected_session_id,
        sigil_kernel::ToolArtifactGcRootsV1::default(),
        u64::MAX,
    )?;
    assert_eq!(report.scanned_manifests, 0);
    assert!(artifact_store_root.join("session-gc").is_dir());
    assert!(artifact_staging_root.join("session-gc").is_dir());
    assert!(
        !managed_session
            .parent()
            .expect("managed session parent")
            .join("artifacts")
            .exists()
    );
    Ok(())
}

#[test]
fn managed_session_catalog_cold_starts_and_rebuilds_many_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managed_root = temp.path().join("state/managed/session-log");
    for index in 0..64 {
        let path = managed_root
            .join(format!("session-{index:03}"))
            .join("records.jsonl");
        fs::create_dir_all(path.parent().expect("managed session parent"))?;
        finalized_session(&path, &format!("managed source {index}"))?;
    }
    let session_dir = temp.path().join("legacy-sessions");
    let cold = LocalSessionLifecycleService::new(
        "workspace-cold",
        &session_dir,
        temp.path().join("cold-exports"),
    )
    .with_managed_session_log_root(temp.path().join("missing-managed-root"))?;
    assert!(cold.catalog()?.entries.is_empty());

    let lifecycle = LocalSessionLifecycleService::new(
        "workspace-many",
        &session_dir,
        temp.path().join("exports"),
    )
    .with_managed_session_log_root(&managed_root)?;
    let projection = SessionCatalogProjectionService::new(
        lifecycle,
        temp.path().join("catalog/session-catalog.sqlite"),
    );
    let report = projection.rebuild()?;
    assert_eq!(report.scanned_source_count, 64);
    assert_eq!(report.indexed_source_count, 64);
    assert_eq!(projection.list_workspace_entries()?.len(), 64);
    Ok(())
}
