use super::*;

#[test]
fn explicit_display_name_is_preserved() {
    let name = resolve_agent_display_name(AgentDisplayNameInput {
        display_name: Some("agent explore"),
        ..AgentDisplayNameInput::default()
    });

    assert_eq!(name.label, "agent explore");
}

#[test]
fn profile_is_preferred_before_thread_id() {
    let name = resolve_agent_display_name(AgentDisplayNameInput {
        profile_id: Some("kernel-explorer"),
        thread_id: Some("agent_chat_123"),
        ..AgentDisplayNameInput::default()
    });

    assert_eq!(name.label, "kernel explorer");
}

#[test]
fn role_thread_and_ordinal_fallbacks_are_human_readable() {
    let blank_display = resolve_agent_display_name(AgentDisplayNameInput {
        display_name: Some("   "),
        role: Some(AgentRole::Planner),
        ordinal: Some(2),
        ..AgentDisplayNameInput::default()
    });
    assert_eq!(blank_display.label, "planner 2");

    let executor = resolve_agent_display_name(AgentDisplayNameInput {
        role: Some(AgentRole::Executor),
        ordinal: Some(3),
        ..AgentDisplayNameInput::default()
    });
    assert_eq!(executor.label, "executor 3");

    let thread = resolve_agent_display_name(AgentDisplayNameInput {
        thread_id: Some(" agent_chat_123 "),
        ..AgentDisplayNameInput::default()
    });
    assert_eq!(thread.label, "agent_chat_123");

    let ordinal = resolve_agent_display_name(AgentDisplayNameInput {
        ordinal: Some(4),
        ..AgentDisplayNameInput::default()
    });
    assert_eq!(ordinal.label, "agent 4");
}
