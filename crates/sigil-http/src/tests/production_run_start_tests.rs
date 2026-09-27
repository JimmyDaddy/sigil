use super::*;

#[tokio::test]
async fn task_review_guidance_preserves_user_guidance_and_exact_recorded_source() -> Result<()> {
    use sigil_application::{ReviewAnnotation, ReviewDiffSide};
    use sigil_kernel::{
        ControlledCheckpointProjection, ModelMessage, MutationEventRecorder, ToolDiffBudget,
        ToolPreview, ToolPreviewFile, ToolPreviewSnapshot, write_file_with_mutation,
    };

    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    write_production_test_config(&config_path, "workspace");
    let session_path = temp.path().join("review.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("create a note")))?;
    let recorder =
        MutationEventRecorder::with_artifact_root(store.clone(), temp.path().join("artifacts"));
    let preview = ToolPreviewSnapshot::from_preview(
        "review-call",
        "write_file",
        &ToolPreview {
            title: "Note".to_owned(),
            summary: "Recorded creation".to_owned(),
            body: String::new(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "note.txt".to_owned(),
                diff: "--- /dev/null\n+++ note.txt\n@@ -0,0 +1 @@\n+original line\n".to_owned(),
            }],
        },
        ToolDiffBudget::default(),
        None,
    );
    store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPreviewCaptured(preview),
    ))?;
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "review-call",
        "note.txt",
        workspace.join("note.txt"),
        b"original line\n",
    )?;
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let scope = records[0].session_id();
    let checkpoints = ControlledCheckpointProjection::from_records(&records)?;
    let checkpoint = checkpoints.latest().context("checkpoint")?;
    let review = sigil_runtime::application_checkpoint_review(
        &session_path,
        scope,
        &workspace,
        &checkpoint.checkpoint_id,
        &checkpoint.checkpoint_digest,
    )?;
    let diff = review.diffs.first().context("recorded diff")?;
    let mut request =
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "", "review");
    request.session_path = Some(session_path.clone());
    request.review_annotations.push(ReviewAnnotation {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        checkpoint_digest: checkpoint.checkpoint_digest.clone(),
        source_call_id: diff.source_call_id.clone(),
        diff_digest: diff.diff_digest.clone(),
        path: diff.path.clone(),
        side: ReviewDiffSide::New,
        start_line: 1,
        end_line: 1,
        comment: SafeText::new("clarify this line")?,
    });
    std::fs::write(workspace.join("note.txt"), "a later edit\n")?;
    let before = std::fs::read(&session_path)?;
    let guidance = super::super::run_start::task_review_guidance(
        &request,
        scope,
        Some("Keep the existing Task goal.".to_owned()),
    )
    .await?
    .context("review guidance")?;
    assert!(guidance.starts_with("Keep the existing Task goal.\n"));
    assert!(guidance.contains("User review of recorded change"));
    assert!(guidance.contains("1: +original line"));
    assert!(guidance.contains("clarify this line"));
    assert!(guidance.contains("no file-write authority"));
    assert_eq!(std::fs::read(&session_path)?, before);
    assert_eq!(
        std::fs::read_to_string(workspace.join("note.txt"))?,
        "a later edit\n"
    );
    assert!(
        super::super::run_start::task_review_guidance(&request, "foreign-scope", None)
            .await
            .is_err()
    );

    // Ordinary continuation needs no additional configuration/session read for review.
    request.review_annotations.clear();
    request.config_path = temp.path().join("missing-config.toml");
    request.session_path = None;
    assert_eq!(
        super::super::run_start::task_review_guidance(
            &request,
            scope,
            Some("unchanged".to_owned())
        )
        .await?,
        Some("unchanged".to_owned())
    );
    Ok(())
}

fn start_request() -> HttpRunStartRequest {
    HttpRunStartRequest {
        review_annotations: Vec::new(),
        image_attachments: Vec::new(),
        prompt: "continue the bound session".to_owned(),
        permission_mode: Some(HttpPermissionMode::Manual),
        model_ref: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: None,
    }
}

#[tokio::test]
async fn production_run_start_preserves_session_route_when_global_default_is_missing() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "run-start-bound-route");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let config_path = temp.path().join("sigil.toml");
    let original = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        original.replace(
            "connection = \"local-test\"",
            "connection = \"missing-default\"",
        ),
    )?;
    let config = sigil_kernel::RootConfig::load(&config_path)?;
    assert!(sigil_runtime::provider_connections::resolve_default_model_route(&config).is_err());
    driver.admit_run_start(&session, &start_request())?;

    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "continue",
        "bound-route",
    );
    request.session_path = Some(PathBuf::from(&session.session_log_path));
    bind_run_start_route(&mut request, &session.durable_session_scope_id).await?;
    assert_eq!(
        request
            .model_connection_id
            .as_ref()
            .map(sigil_kernel::ConnectionId::as_str),
        Some("local-test")
    );
    assert_eq!(request.model_name.as_deref(), Some("gpt-test"));
    let model = sigil_kernel::ModelRef::new(
        request
            .model_connection_id
            .clone()
            .expect("bound connection"),
        request.model_name.clone().expect("bound model"),
    )?;
    assert!(sigil_runtime::provider_connections::resolve_model_route(&config, &model).is_ok());

    let before = std::fs::read(&session.session_log_path)?;
    assert!(
        bind_run_start_route(&mut request, "foreign-scope")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&session.session_log_path)?, before);
    assert_eq!(
        request
            .model_connection_id
            .as_ref()
            .map(sigil_kernel::ConnectionId::as_str),
        Some("local-test")
    );
    Ok(())
}

#[tokio::test]
async fn production_run_start_rechecks_recovery_and_actual_replacement_connection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "run-start-recovery");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let config_path = temp.path().join("sigil.toml");
    let original = std::fs::read_to_string(&config_path)?;
    std::fs::write(&config_path, original.replace("local-test", "replacement"))?;
    let context = application_run_start_view(
        &config_path,
        Path::new(&session.session_log_path),
        &session.durable_session_scope_id,
        None,
    )?;
    let mut request = start_request();
    request.model_selection_binding = Some(context.model_selection_binding);
    request.route_recovery_binding = context
        .route_recovery
        .map(|recovery| recovery.recovery_binding);
    request.model_ref = Some(crate::HttpProviderModelRef {
        connection_id: "missing".to_owned(),
        model_id: "gpt-test".to_owned(),
    });
    assert!(matches!(
        driver.admit_run_start(&session, &request),
        Err(HttpRunAdmissionError::RouteRecovery(_))
    ));
    request.model_ref = Some(crate::HttpProviderModelRef {
        connection_id: "replacement".to_owned(),
        model_id: "custom-unlisted-model".to_owned(),
    });
    driver.admit_run_start(&session, &request)?;
    std::fs::write(
        &config_path,
        original
            .replace("local-test", "replacement")
            .replace("127.0.0.1:1", "127.0.0.1:2"),
    )?;
    assert!(
        matches!(
            driver.admit_run_start(&session, &request),
            Err(HttpRunAdmissionError::RouteRecovery(_))
        ),
        "changed configuration must invalidate the previously accepted recovery binding"
    );
    assert!(registry.get_session(&session.id)?.run_ids.is_empty());
    Ok(())
}
