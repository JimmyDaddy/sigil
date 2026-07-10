use super::*;

#[test]
fn spawn_scope_overlap_warning_detects_parent_child_path_overlap() -> Result<()> {
    let mut session = Session::new("parent", "model");
    session.append_user_message(ModelMessage::user(
        "Review crates/sigil-kernel/src/permission.rs and approval flow",
    ))?;
    let parsed = SpawnAgentArgs {
        profile_id: AgentProfileId::new("explore")?,
        objective: "inspect crates/sigil-kernel/src/permission.rs".to_owned(),
        prompt: "read permission implementation".to_owned(),
        mode: AgentInvocationMode::JoinBeforeFinal,
        display_name_hint: None,
    };

    let warning = spawn_scope_overlap_warning(&session, &parsed)
        .expect("path overlap should produce a warning");

    assert!(warning.contains("crates/sigil-kernel/src/permission.rs"));
    Ok(())
}

#[test]
fn spawn_scope_overlap_warning_ignores_unrelated_scopes() -> Result<()> {
    let mut session = Session::new("parent", "model");
    session.append_user_message(ModelMessage::user("Review crates/sigil-tui/src/app.rs"))?;
    let parsed = SpawnAgentArgs {
        profile_id: AgentProfileId::new("explore")?,
        objective: "inspect crates/sigil-kernel/src/permission.rs".to_owned(),
        prompt: "read permission implementation".to_owned(),
        mode: AgentInvocationMode::JoinBeforeFinal,
        display_name_hint: None,
    };

    assert!(spawn_scope_overlap_warning(&session, &parsed).is_none());
    Ok(())
}
