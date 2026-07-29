use anyhow::Result;
use sigil_kernel::{
    ConnectionId, ControlEntry, JsonlSessionStore, ModelMessage, ModelRef, Session,
    SessionLogEntry, ToolArtifactEncoding, ToolArtifactSensitivity, ToolCall, ToolResult,
    ToolResultMeta, ToolResultRecordedV2,
};

use super::*;

fn write_config(path: &Path, compaction_enabled: bool) -> Result<()> {
    std::fs::write(
        path,
        format!(
            r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[compaction]
enabled = {compaction_enabled}
tail_messages = 2
"#,
        ),
    )?;
    Ok(())
}

fn session_with_messages(path: &Path, messages: &[&str]) -> Result<String> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    })?;
    for message in messages {
        session.append_user_message(ModelMessage::user(*message))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("reply to {message}")),
            Vec::new(),
        ))?;
    }
    Ok(session.session_scope_id().to_owned())
}

#[tokio::test]
async fn preview_reports_no_foldable_history_without_creating_lifecycle_entries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    write_config(&config_path, true)?;
    let scope = session_with_messages(&session_path, &["hello"])?;
    let before = std::fs::read(&session_path)?;

    let (review, pending) =
        prepare_application_compaction(&config_path, temp.path(), &session_path, &scope).await?;

    assert!(pending.is_none());
    assert!(review.preview_id.is_none());
    assert!(matches!(
        review.admission,
        ApplicationCompactionAdmission::NoFoldableHistory {
            durable_message_count: 2,
            configured_tail_message_count: 2,
        }
    ));
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[tokio::test]
async fn preview_preserves_disabled_and_scope_failure_semantics() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    write_config(&config_path, false)?;
    let scope = session_with_messages(&session_path, &["one", "two"])?;

    let (review, pending) =
        prepare_application_compaction(&config_path, temp.path(), &session_path, &scope).await?;
    assert!(pending.is_none());
    assert!(matches!(
        review.admission,
        ApplicationCompactionAdmission::Unavailable { ref reason }
            if reason.contains("disabled")
    ));
    assert!(
        prepare_application_compaction(&config_path, temp.path(), &session_path, "another-scope",)
            .await
            .is_err()
    );
    Ok(())
}

#[test]
fn local_preview_builds_continuity_without_provider_or_durable_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("fixture.txt"), "stable workspace")?;
    write_config(&config_path, true)?;
    let large = format!("one {}", "history ".repeat(4_000));
    let scope = session_with_messages(
        &session_path,
        &[
            large.as_str(),
            large.as_str(),
            large.as_str(),
            large.as_str(),
        ],
    )?;
    let before = std::fs::read(&session_path)?;

    let (review, pending) =
        preview_application_compaction(&config_path, &workspace, &session_path, &scope)?;

    let pending = pending.expect("foldable local preview");
    assert_eq!(review.preview_id.as_deref(), Some(pending.preview_id()));
    assert!(matches!(
        review.admission,
        ApplicationCompactionAdmission::Prepared { .. }
    ));
    let details = review.details.expect("local continuity details");
    assert!(details.active_objective.starts_with("one history"));
    assert!(details.folded_complete_turn_count >= 1);
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[test]
fn local_preview_exposes_bounded_recoverable_and_redacted_tool_artifact_details() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("fixture.txt"), "stable workspace")?;
    let secret = "sigil-preview-secret-57";
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", secret);
    std::fs::write(
        &config_path,
        format!(
            r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "{secret}"

[compaction]
enabled = true
tail_messages = 2
"#
        ),
    )?;
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    })?;
    session.append_user_message(ModelMessage::user("inspect the old build log"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-build-log".to_owned(),
            name: "cargo_test".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    let legacy_inline_body = serde_json::json!({
        "status": "completed",
        "content": format!(
            "head {secret} {} tail {secret}",
            "large-build-output ".repeat(20_000)
        ),
    })
    .to_string();
    let artifact_store = session
        .tool_artifact_store()
        .expect("durable session exposes its artifact store");
    let descriptor = artifact_store.capture_policy_safe_bytes(
        "call-build-log",
        "cargo_test",
        legacy_inline_body.as_bytes(),
        legacy_inline_body.len() as u64,
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::Ordinary,
        0,
    )?;
    let result = ToolResult::ok(
        "call-build-log",
        "cargo_test",
        legacy_inline_body,
        ToolResultMeta::default(),
    )
    .with_captured_artifact(descriptor);
    let (recorded, _) = ToolResultRecordedV2::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    session.append(SessionLogEntry::ToolResultV2(recorded))?;
    // The newest bounded tool-token window is protected by design. Fill it with high-signal
    // failures so the original successful build log becomes the only ageable artifact.
    for index in 0..9 {
        let call_id = format!("call-recent-error-{index}");
        session.append_assistant_message(ModelMessage::assistant(
            None,
            vec![ToolCall {
                id: call_id.clone(),
                name: "recent_check".to_owned(),
                args_json: "{}".to_owned(),
            }],
        ))?;
        let error_result = ToolResult::ok(
            call_id,
            "recent_check",
            format!(
                "recent high-signal failure {index} {}",
                "detail ".repeat(3_000)
            ),
            ToolResultMeta {
                exit_code: Some(1),
                ..ToolResultMeta::default()
            },
        );
        let (recorded, _) = ToolResultRecordedV2::capture(
            &error_result,
            Some(&artifact_store),
            ToolArtifactSensitivity::Ordinary,
        )?;
        session.append(SessionLogEntry::ToolResultV2(recorded))?;
    }
    session.append_assistant_message(ModelMessage::assistant(
        Some("old build log inspected".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("continue with the current fix"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("current progress".to_owned()),
        Vec::new(),
    ))?;
    for turn in 0..4 {
        session.append_user_message(ModelMessage::user(format!(
            "continue current fix {turn} {}",
            "current-history ".repeat(4_000)
        )))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("current progress {turn}")),
            Vec::new(),
        ))?;
    }
    let scope = session.session_scope_id().to_owned();
    let before = std::fs::read(&session_path)?;

    let (review, pending) =
        preview_application_compaction(&config_path, &workspace, &session_path, &scope)?;

    assert!(pending.is_some());
    let details = review.details.expect("local preview details");
    assert_eq!(details.tool_artifact_count, 1, "{details:#?}");
    assert_eq!(details.tool_artifacts.len(), 1);
    let artifact = &details.tool_artifacts[0];
    assert_eq!(artifact.tool_name, "cargo_test");
    assert_eq!(artifact.tool_call_id, "call-build-log");
    assert!(
        artifact
            .content_sha256
            .strip_prefix("sha256:")
            .is_some_and(|digest| digest.len() == 64)
    );
    assert!(artifact.original_content_bytes > 8_192);
    assert!(artifact.original_content_token_upper_bound > 0);
    assert!(!artifact.head_excerpt.contains(secret));
    assert!(!artifact.tail_excerpt.contains(secret));
    assert!(
        artifact.head_excerpt.contains("[redacted]")
            || artifact.tail_excerpt.contains("[redacted]")
    );
    assert!(artifact.recovery_instruction.contains("read_tool_artifact"));
    assert!(artifact.recovery_instruction.contains("opaque ref"));
    assert_eq!(std::fs::read(&session_path)?, before);
    assert!(
        String::from_utf8(before)?.contains(secret),
        "preview redaction must not rewrite raw durable audit history"
    );
    Ok(())
}

#[tokio::test]
async fn preview_uses_the_persisted_connection_instead_of_the_current_default() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let session_path = temp.path().join("session.jsonl");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "default-a"
model = "deepseek-v4-flash"

[connections.default-a]
label = "Default A"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[connections.session-b]
label = "Persisted B"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://deepseek-proxy.example.test"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[compaction]
enabled = true
tail_messages = 2
"#,
    )?;
    let root = RootConfig::load(&config_path)?;
    assert_eq!(root.config_version, Some(2));
    assert_eq!(
        root.agent.connection.as_ref().map(ConnectionId::as_str),
        Some("default-a")
    );
    let loaded = crate::provider_connections::load_provider_connections(&root);
    assert_eq!(
        loaded.mode,
        crate::provider_connections::ConfigMode::V2,
        "{:?}",
        loaded.issues
    );
    assert!(loaded.default_model.is_some(), "{:?}", loaded.issues);
    let model_ref = ModelRef::new(ConnectionId::new("session-b")?, "deepseek-v4-flash")?;
    let (provider_name, persisted_route) =
        crate::provider_connections::resolve_model_route(&root, &model_ref)?;
    assert_eq!(provider_name, "deepseek");

    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::new(provider_name, "deepseek-v4-flash").with_store(store.clone());
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: Some(persisted_route),
    })?;
    for index in 0..4 {
        let message = format!("user turn {index}: {}", "u".repeat(20_000));
        session.append_user_message(ModelMessage::user(message.clone()))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("reply to {message}")),
            Vec::new(),
        ))?;
    }
    let (selected_session, selected_model_ref) = load_application_compaction_session(&root, store)?;
    assert_eq!(
        selected_model_ref.as_ref(),
        Some(&model_ref),
        "compaction must retain the durable connection instead of the configured default"
    );
    assert_eq!(
        selected_session
            .resolved_model_route()
            .expect("selected durable route")
            .model_ref,
        model_ref
    );

    let error = prepare_application_compaction(
        &config_path,
        temp.path(),
        &session_path,
        session.session_scope_id(),
    )
    .await
    .expect_err("persisted custom DeepSeek transport must fail the pinned target proof");
    assert!(
        format!("{error:#}").contains("custom routes"),
        "compaction validated the default connection instead of persisted B: {error:#}"
    );
    let reloaded = Session::load_from_store_with_route(
        "deepseek",
        "deepseek-v4-flash",
        None,
        JsonlSessionStore::new(&session_path)?,
    )?;
    assert_eq!(
        reloaded
            .resolved_model_route()
            .expect("persisted route")
            .model_ref,
        model_ref
    );
    Ok(())
}
