use super::*;

#[test]
fn push_entry_deduplicates_and_keeps_tail() {
    let mut history = Vec::new();
    for index in 0..=100 {
        assert!(push_input_history_entry(
            &mut history,
            format!("prompt-{index}"),
            INPUT_HISTORY_LIMIT,
        ));
    }

    assert_eq!(history.len(), INPUT_HISTORY_LIMIT);
    assert_eq!(history.first().map(String::as_str), Some("prompt-1"));
    assert!(!push_input_history_entry(
        &mut history,
        "prompt-100".to_owned(),
        INPUT_HISTORY_LIMIT,
    ));
    assert_eq!(history.len(), INPUT_HISTORY_LIMIT);
}

#[test]
fn prompt_history_skips_control_commands() {
    for prompt in [
        "",
        "   ",
        "/quit",
        "/q",
        "/exit",
        "/new",
        "/feedback",
        "  /quit  ",
        "  /feedback  ",
    ] {
        assert!(!should_record_input_history_entry(prompt));
    }

    for prompt in [
        "normal prompt",
        "/plan review this",
        "/task investigate this",
        "@explore inspect crate",
    ] {
        assert!(should_record_input_history_entry(prompt));
    }
}

#[test]
fn store_round_trips_json_lines_and_ignores_invalid_rows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join(".sigil/input-history.jsonl");
    write_input_history(
        &path,
        &[
            "plain prompt".to_owned(),
            "/plan review workspace".to_owned(),
            "quoted \"prompt\"".to_owned(),
        ],
    )?;
    fs::write(
        &path,
        format!(
            "{}\nnot json\n{}\n{}\n",
            serde_json::to_string("plain prompt")?,
            serde_json::to_string("/plan review workspace")?,
            serde_json::to_string("/quit")?
        ),
    )?;

    let history = read_input_history(&path, INPUT_HISTORY_LIMIT)?;

    assert_eq!(
        history,
        vec![
            "plain prompt".to_owned(),
            "/plan review workspace".to_owned()
        ]
    );
    Ok(())
}

#[test]
fn store_projects_sensitive_prompt_without_changing_live_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("input-history.jsonl");
    let raw = "inspect https://example.com/private?signature=history-secret exactly";
    let live_history = vec![raw.to_owned()];

    write_input_history(&path, &live_history)?;

    assert_eq!(live_history, vec![raw.to_owned()]);
    let durable = fs::read_to_string(&path)?;
    assert!(!durable.contains("history-secret"));
    assert!(!durable.contains(raw));
    let restored = read_input_history(&path, INPUT_HISTORY_LIMIT)?;
    assert_eq!(restored, vec![sigil_kernel::safe_persistence_text(raw)]);
    Ok(())
}

#[test]
fn app_input_history_path_uses_resolved_state_file() {
    let config = crate::app::tests::common::test_config();
    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert_eq!(app.input_history_path(), app.sigil_paths.input_history_file);
}

#[test]
fn r71_tui_managed_leaves_reroute_session_and_history_round_trip() -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Ch;
    // Env: tests persist input history only when explicitly enabled.
    // SAFETY: test-scoped env var, restored before the test returns.
    unsafe { std::env::set_var("SIGIL_TUI_TEST_PERSIST_INPUT_HISTORY", "1") };
    let dir = tempfile::tempdir()?;
    let state = dir.path().join("state");
    for anchor in [&state, &state.join("cache"), &dir.path().join("exec")] {
        std::fs::create_dir_all(anchor)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(anchor, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let exec = dir.path().join("exec");
    let planner = std::sync::Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
        sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
    ));
    let manifest_hash = sigil_kernel::resource::CanonicalHash::from_bytes([0x44; 32]);
    let composition = std::sync::Arc::new(
        sigil_runtime::r71_authority_composition::compose_runtime_authority(
            &state,
            &exec,
            manifest_hash,
            planner,
            &[Ch::SessionLog, Ch::InputHistory],
        )?,
    );
    let config = crate::app::tests::common::test_config();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    // Pin paths to the tempdir (env-independent), keeping the session stem stable.
    let session_id = app.session_id.clone();
    app.session_log_dir = dir.path().join("sessions");
    std::fs::create_dir_all(&app.session_log_dir)?;
    app.session_log_path = app
        .session_log_dir
        .join(format!("session-{session_id}.jsonl"));
    app.sigil_paths.input_history_file = dir.path().join("history/input-history.jsonl");
    app.set_authority_composition(composition);

    // Session log reroutes to the managed named leaf; the store open is guarded.
    assert!(
        app.session_log_path
            .to_string_lossy()
            .contains("/managed/session-log/")
    );
    assert!(
        app.session_log_path
            .ends_with(format!("session-{session_id}/records.jsonl"))
    );
    // Input history reroutes to its managed leaf and persists through the writer.
    assert!(
        app.input_history_path()
            .ends_with("managed/input-history/records.jsonl")
    );
    app.record_input_history("managed-roundtrip-prompt".to_owned());
    let content = std::fs::read_to_string(app.input_history_path())?;
    assert!(content.contains("managed-roundtrip-prompt"));
    // SAFETY: restores the test-scoped env var.
    unsafe { std::env::remove_var("SIGIL_TUI_TEST_PERSIST_INPUT_HISTORY") };
    Ok(())
}
