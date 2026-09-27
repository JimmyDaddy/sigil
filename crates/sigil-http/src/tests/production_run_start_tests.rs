use super::*;

fn start_request() -> HttpRunStartRequest {
    HttpRunStartRequest {
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
