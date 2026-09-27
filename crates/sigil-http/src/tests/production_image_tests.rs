use super::*;
use base64::Engine as _;

fn png_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAIAAAA2iEnWAAAAEElEQVR4nGP4z8AARAwoFABE0AX7pM/egAAAAABJRU5ErkJggg==").expect("valid PNG fixture")
}

fn admit_image_run(
    driver: &HttpProductionRunDriver,
    session: &HttpSessionSnapshot,
    request: &HttpRunStartRequest,
) -> Result<(), HttpRunAdmissionError> {
    // Match application dispatch's joined synchronous owner. Provider construction rejects
    // invocation from a Tokio context, including a spawn_blocking worker with an entered handle.
    std::thread::scope(|scope| {
        scope
            .spawn(|| driver.admit_run_start(session, request))
            .join()
            .expect("image admission worker should complete")
    })
}

async fn fetch_image_route(
    address: std::net::SocketAddr,
    path: &str,
) -> Result<(u16, serde_json::Value)> {
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer image-test-token\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let mut bytes = Vec::new();
    stream.take(64 * 1024).read_to_end(&mut bytes).await?;
    let response = std::str::from_utf8(&bytes)?;
    let (header, body) = response.split_once("\r\n\r\n").context("HTTP response")?;
    let status = header
        .split_whitespace()
        .nth(1)
        .context("HTTP status")?
        .parse()?;
    Ok((status, serde_json::from_str(body)?))
}

#[tokio::test]
async fn image_run_admission_rejects_unsupported_actual_model_and_invalid_cached_bytes()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "image-admission");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let image = registry.ingest_image(png_bytes())?;
    let same_bytes_other_draft = registry.ingest_image(png_bytes())?;
    assert_ne!(image.attachment_id, same_bytes_other_draft.attachment_id);
    assert_eq!(image.artifact_ref, same_bytes_other_draft.artifact_ref);
    let request = HttpRunStartRequest {
        image_attachments: vec![image.clone()],
        prompt: String::new(),
        permission_mode: Some(HttpPermissionMode::Manual),
        model_ref: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: None,
    };
    assert!(matches!(
        admit_image_run(&driver, &session, &request),
        Err(HttpRunAdmissionError::ImageInputUnsupported)
    ));
    assert!(registry.ingest_image(b"not an image".to_vec()).is_err());
    std::fs::write(
        driver.image_cache()?.root().join(&image.artifact_ref),
        b"changed",
    )?;
    assert!(matches!(
        admit_image_run(&driver, &session, &request),
        Err(HttpRunAdmissionError::ImageAttachmentInvalid)
    ));
    Ok(())
}

#[tokio::test]
async fn image_only_run_admission_and_history_recovery_use_actual_vision_route_and_exact_record()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "image-recovery");
    let config_path = temp.path().join("sigil.toml");
    let config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        config
            .replace("gpt-test", "gpt-4.1")
            .replace("chat_completions", "responses"),
    )?;
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let bytes = png_bytes();
    let image = registry.ingest_image(bytes.clone())?;
    let request = HttpRunStartRequest {
        image_attachments: vec![image.clone()],
        prompt: String::new(),
        permission_mode: Some(HttpPermissionMode::Manual),
        model_ref: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: None,
    };
    admit_image_run(&driver, &session, &request)?;
    let skill_path = temp.path().join(".sigil/skills/review-image/SKILL.md");
    std::fs::create_dir_all(skill_path.parent().expect("skill parent"))?;
    std::fs::write(
        &skill_path,
        "---\nname: review-image\ndescription: Review attached image.\ntrust: trusted\nrun-as: inline\nuser-invocable: true\n---\nReview the image.\n",
    )?;
    let catalog = registry.run_context_view(&session.id)?.extension_catalog;
    let mut skill_request = request.clone();
    skill_request.skill_binding = Some(
        catalog
            .skills
            .iter()
            .find(|skill| skill.id == "review-image")
            .and_then(|skill| skill.binding.clone())
            .expect("exact available skill binding"),
    );
    admit_image_run(&driver, &session, &skill_request)?;
    let store = JsonlSessionStore::new(&session.session_log_path)?;
    let mut writer = sigil_kernel::Session::load_from_store("custom", "gpt-4.1", store)?;
    writer.try_attach_image_attachment_resolver(Arc::new(driver.image_cache()?))?;
    let mut message = sigil_kernel::ModelMessage::user("");
    message.image_attachments = vec![image.clone()];
    writer.append_user_message(message)?;
    drop(writer);
    let page = registry.conversation_display_page(&session.id, None, 20)?;
    let item = page.items.first().expect("durable image-only user message");
    let budget = sigil_kernel::SessionReadBudget::default();
    let (mime, recovered) =
        registry.message_image(&session.id, &item.display_id, &image.attachment_id, &budget)?;
    assert_eq!(mime, "image/png");
    assert_eq!(recovered, bytes);
    // Create the foreign session while admission is open; shutting down the real listener
    // closes the shared registry to new commands. The read-only checks below remain valid.
    let other = registry.create_session(HttpSessionCreateRequest::default())?;
    let server = crate::HttpLocalServer::bind(
        crate::HttpServerConfig::default(),
        Some("image-test-token"),
        registry.clone(),
    )
    .await?;
    let address = server.local_addr()?;
    let (shutdown, receiver) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve_until_shutdown(async {
        let _ = receiver.await;
    }));
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("display_id", &item.display_id)
        .append_pair("attachment_id", &image.attachment_id)
        .finish();
    let path = format!("/sessions/{}/message-image?{query}", session.id);
    let results: Result<_> = async {
        let future_field =
            fetch_image_route(address, &format!("{path}&future_field=allowed")).await?;
        let duplicate_display =
            fetch_image_route(address, &format!("{path}&display_id=other")).await?;
        let duplicate_image =
            fetch_image_route(address, &format!("{path}&attachment_id=other")).await?;
        Ok((future_field, duplicate_display, duplicate_image))
    }
    .await;
    let _ = shutdown.send(());
    serving.await??;
    let (future_field, duplicate_display, duplicate_image) = results?;
    assert_eq!(future_field.0, 200);
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(
            future_field.1["data_base64"]
                .as_str()
                .context("image content")?
        )?,
        bytes
    );
    for response in [duplicate_display, duplicate_image] {
        assert_eq!(response.0, 400);
        assert_eq!(response.1["error"]["code"], "invalid_query");
    }
    assert!(
        registry
            .message_image(&session.id, &item.display_id, "unrelated", &budget)
            .is_err()
    );
    assert!(
        registry
            .message_image(&other.id, &item.display_id, &image.attachment_id, &budget)
            .is_err()
    );
    std::fs::write(
        driver.image_cache()?.root().join(&image.artifact_ref),
        b"changed",
    )?;
    assert!(
        registry
            .message_image(&session.id, &item.display_id, &image.attachment_id, &budget)
            .is_err()
    );
    Ok(())
}
