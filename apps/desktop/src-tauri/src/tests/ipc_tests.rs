use super::*;

#[test]
fn run_context_projects_agent_name_from_existing_invocation_token() {
    let native = serde_json::from_value::<DesktopRunContextView>(serde_json::json!({
        "model_ref": {
            "connection_id": "deepseek-default",
            "model_id": "deepseek-v4-flash"
        },
        "provider_name": "deepseek",
        "model_name": "deepseek-v4-flash",
        "model_options": [],
        "model_selection": "fresh_session",
        "model_selection_binding": "model-binding",
        "default_permission_mode": "manual",
        "available_permission_modes": ["manual"],
        "available_reasoning_efforts": ["max"],
        "default_reasoning_effort": "max",
        "reasoning_effort_binding": "effort-binding",
        "context_window_source": "provider",
        "extension_catalog": {
            "commands": [],
            "skills": [],
            "agents": [{
                "id": "compat-agent-123",
                "invocation_token": "@正典提升员",
                "description": "Compatibility agent.",
                "source": "compatibility",
                "kind": "primary",
                "trust": "trusted",
                "enabled": true,
                "user_invocable": true,
                "available": true
            }]
        }
    }))
    .expect("run context should decode without a redundant agent name field");

    let projected = DesktopRunContext::from(native);
    assert_eq!(projected.extension_catalog.agents[0].name, "正典提升员");
}

#[test]
fn agent_display_name_falls_back_to_profile_id_for_empty_token() {
    assert_eq!(agent_display_name("@", "explore"), "explore");
}
