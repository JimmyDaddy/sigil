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
