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

#[test]
fn compaction_projection_preserves_prepared_stage_and_bounded_tool_artifact() {
    let native = serde_json::from_value::<NativeCompactionReview>(serde_json::json!({
        "preview_id": "preview-1",
        "folded_event_count": 4,
        "retained_event_count": 2,
        "policy": null,
        "details": {
            "active_objective": "finish RFC-0057",
            "objective_source_event_id": "event-objective",
            "active_constraints": [],
            "folded_complete_turn_count": 2,
            "folded_token_upper_bound": 1024,
            "retained_complete_turn_count": 1,
            "retained_token_upper_bound": 256,
            "tool_artifact_count": 1,
            "tool_artifacts": [{
                "source_event_id": "event-tool",
                "content_sha256": format!("sha256:{}", "a".repeat(64)),
                "tool_name": "read_file",
                "tool_call_id": "call-1",
                "status": "completed",
                "original_content_bytes": 4096,
                "original_content_token_upper_bound": 1024,
                "head_excerpt": "head",
                "tail_excerpt": "tail",
                "reason": "large historical output",
                "recovery_instruction": "read event-tool"
            }],
            "pending_work_count": 0,
            "unresolved_question_count": 0,
            "recoverable_attachment_count": 0,
            "protected_control_event_count": 0,
            "protected_active_tool_or_approval_count": 0,
            "current_cache_read_tokens": 800,
            "break_even_turns": 2
        },
        "admission": {
            "kind": "prepared",
            "standalone_tool_output_shrink_available": true
        }
    }))
    .expect("native compaction review should decode");

    let projected =
        serde_json::to_value(DesktopCompactionReview::from(native)).expect("projection serializes");

    assert_eq!(projected["admission"]["kind"], "prepared");
    assert_eq!(
        projected["admission"]["standaloneToolOutputShrinkAvailable"],
        true
    );
    assert_eq!(
        projected["details"]["toolArtifacts"][0]["contentSha256"],
        format!("sha256:{}", "a".repeat(64))
    );
    assert_eq!(
        projected["details"]["toolArtifacts"][0]["recoveryInstruction"],
        "read event-tool"
    );
}

#[test]
fn conversation_recovery_action_labels_cover_two_stage_compaction() {
    assert_eq!(
        conversation_recovery_action_kind_label(
            NativeConversationRecoveryCommandActionKind::PrepareCompaction
        ),
        "prepare_compaction"
    );
    assert_eq!(
        conversation_recovery_action_kind_label(
            NativeConversationRecoveryCommandActionKind::ApplyStandaloneToolOutputShrink
        ),
        "apply_standalone_tool_output_shrink"
    );
}
