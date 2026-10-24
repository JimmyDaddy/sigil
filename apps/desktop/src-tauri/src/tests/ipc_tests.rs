use super::*;

#[test]
fn composition_changed_session_recovery_ipc_preserves_read_only_actions() {
    let recovery: sigil_desktop::DesktopSessionRouteRecoveryView =
        serde_json::from_value(serde_json::json!({
            "code": "session_composition_changed",
            "allowed_actions": ["start_new_session", "back_to_session_library"],
            "recovery_binding": "session-composition:fixture-read-only-binding",
            "retryable": false
        }))
        .expect("typed read-only recovery");
    let ipc = serde_json::to_value(desktop_session_route_recovery_summary(recovery))
        .expect("safe recovery IPC");
    assert_eq!(ipc["code"], "session_composition_changed");
    assert_eq!(
        ipc["allowedActions"],
        serde_json::json!(["start_new_session", "back_to_session_library"])
    );
    assert_eq!(ipc["retryable"], false);
    assert_eq!(
        ipc["recoveryBinding"],
        "session-composition:fixture-read-only-binding"
    );
}

fn user_input_presentation_request_value() -> serde_json::Value {
    serde_json::json!({
        "identity": { "session_scope_id": "presentation-session", "root_logical_run_id": "presentation-run",
            "source_thread_id": "main", "request_id": "presentation-request", "generation": 1,
            "source_binding_hash": "a".repeat(64) },
        "request_hash": "b".repeat(64), "source": "agent", "purpose": "clarification",
        "prompt": "Provider-authored question", "questions": [{ "id": "choice", "question": "Choose a scope",
            "required": true, "options": [], "multiple": false }],
        "allowed_actions": ["submit", "decline", "cancel_run"], "requested_at_unix_ms": 1, "status": "requested"
    })
}

#[test]
fn user_input_presentation_ipc_preserves_typed_prompt_origin_in_camel_case() {
    for origin in ["question_text", "host_question_group"] {
        let mut value = user_input_presentation_request_value();
        value["prompt_origin"] = serde_json::json!(origin);
        let native: sigil_desktop::DesktopUserInputRequest =
            serde_json::from_value(value).expect("native request");
        let ipc = serde_json::to_value(DesktopUserInputRequestSummary::from(native))
            .expect("request IPC");
        assert_eq!(ipc["promptOrigin"], origin);
        assert_eq!(ipc["prompt"], "Provider-authored question");
        assert!(ipc.get("prompt_origin").is_none());
    }
}

#[test]
fn user_input_presentation_ipc_omits_absent_origin() {
    let native: sigil_desktop::DesktopUserInputRequest =
        serde_json::from_value(user_input_presentation_request_value()).expect("native request");
    let ipc =
        serde_json::to_value(DesktopUserInputRequestSummary::from(native)).expect("request IPC");
    assert!(ipc.get("promptOrigin").is_none());
}

#[test]
fn user_input_presentation_ipc_exposes_body_free_receipt_for_each_decision() {
    for decision in ["submitted", "declined", "run_cancelled"] {
        let native: NativeConversationDisplayContent = serde_json::from_value(serde_json::json!({
            "type": "tool", "tool_name": "request_user_input", "truncated": false,
            "original_content_bytes": 0, "user_input_receipt": { "decision": decision,
                "answered_count": 2, "answer_values": ["must-not-reach-renderer"] }
        }))
        .expect("native tool content");
        let ipc = serde_json::to_value(DesktopConversationDisplayContent::from(native))
            .expect("tool IPC");
        assert_eq!(
            ipc["userInputReceipt"],
            serde_json::json!({ "decision": decision, "answeredCount": 2 })
        );
        assert!(ipc.get("user_input_receipt").is_none());
        assert!(!ipc.to_string().contains("must-not-reach-renderer"));
    }
}

#[test]
fn user_input_presentation_ipc_ordinary_tools_omit_receipt() {
    let native: NativeConversationDisplayContent = serde_json::from_value(serde_json::json!({
        "type": "tool", "tool_name": "read_file", "output": "ordinary output",
        "truncated": false, "original_content_bytes": 15
    }))
    .expect("native tool content");
    let ipc =
        serde_json::to_value(DesktopConversationDisplayContent::from(native)).expect("tool IPC");
    assert!(ipc.get("userInputReceipt").is_none());
    assert_eq!(ipc["output"], "ordinary output");
}

#[test]
fn legacy_catalog_navigation_is_allowlisted_and_preserves_unknown_source_state() {
    for state in ["present", "unverified"] {
        let native: DesktopSessionCatalogPage = serde_json::from_value(serde_json::json!({
            "workspace_id": "new-workspace", "generation": 1, "reconciled_at_unix_ms": 2,
            "degraded_source_count": 0, "identity_conflict_count": 0, "truncated_source_count": 0,
            "entries": [], "legacy_shared_history": {
                "workspace_id": "original-workspace", "workspace_label": "原工作区",
                "source_state": state, "workspace_path": "/must-not-reach-renderer"
            }
        }))
        .expect("typed catalog");
        let safe = serde_json::to_value(DesktopCatalogPage::from(native)).expect("safe projection");
        assert_eq!(
            safe["legacySharedHistory"]["workspaceId"],
            "original-workspace"
        );
        assert_eq!(safe["legacySharedHistory"]["workspaceLabel"], "原工作区");
        assert_eq!(safe["legacySharedHistory"]["sourceState"], state);
        assert!(!safe.to_string().contains("must-not-reach-renderer"));
    }
}

#[test]
fn legacy_catalog_navigation_is_optional_for_existing_servers() {
    let native: DesktopSessionCatalogPage = serde_json::from_value(serde_json::json!({
        "workspace_id": "workspace", "generation": 1, "reconciled_at_unix_ms": 2,
        "degraded_source_count": 0, "identity_conflict_count": 0, "truncated_source_count": 0,
        "entries": []
    }))
    .expect("existing catalog");
    let safe = serde_json::to_value(DesktopCatalogPage::from(native)).expect("safe projection");
    assert!(safe.get("legacySharedHistory").is_none());
}

#[test]
fn approval_receipt_projection_preserves_route_identity_and_revision() {
    let native = serde_json::from_value::<NativeApprovalCommandReceipt>(serde_json::json!({
        "command_id": "approval-command-1",
        "client_id": "desktop-1",
        "session_id": "session-1",
        "run_id": "run-1",
        "call_id": "call-1",
        "approval_request_id": "approval-1",
        "expected_stream_sequence": 7,
        "correlation_id": "event-1",
        "decision": {
            "run_id": "run-1",
            "call_id": "call-1",
            "decision": "approved_for_session"
        },
        "route_state": "decision_accepted",
        "registry_revision": 8,
        "replayed": true
    }))
    .expect("native approval receipt should decode");

    let projected = serde_json::to_value(DesktopApprovalDecisionSummary::from(native))
        .expect("approval projection should serialize");
    assert_eq!(projected["approvalRequestId"], "approval-1");
    assert_eq!(projected["decision"], "approved_for_session");
    assert_eq!(projected["routeState"], "decision_accepted");
    assert_eq!(projected["registryRevision"], 8);
    assert_eq!(projected["replayed"], true);
}

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
        "model_selection": "same_session",
        "model_selection_binding": "model-binding",
        "default_permission_mode": "manual",
        "available_permission_modes": ["manual"],
        "available_reasoning_efforts": ["max"],
        "default_reasoning_effort": "max",
        "reasoning_effort_binding": "effort-binding",
        "cache_usage": {
            "cache_read_tokens": 900,
            "cache_miss_tokens": 100,
            "last_layout_mutation": "conversation_tail_appended",
            "provider_miss_without_local_mutation": false
        },
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
    assert_eq!(
        projected
            .cache_usage
            .as_ref()
            .map(|usage| usage.cache_read_tokens),
        Some(900)
    );
}

#[test]
fn agent_display_name_falls_back_to_profile_id_for_empty_token() {
    assert_eq!(agent_display_name("@", "explore"), "explore");
}

#[test]
fn workspace_selection_keeps_recent_index_degradation_separate_from_open_success() {
    let selection = DesktopWorkspaceSelection {
        cancelled: false,
        workspace: Some(DesktopWorkspaceSummary {
            id: "workspace-1".to_owned(),
            display_name: "Workspace".to_owned(),
            location_label: Some("Parent · Ancestor".to_owned()),
            server_version: "0.0.1".to_owned(),
            state: sigil_desktop::DesktopConnectionState::Ready,
        }),
        recent_persistence_degraded: Some(true),
    };
    let projected = serde_json::to_value(selection).expect("workspace selection serializes");
    assert_eq!(projected["cancelled"], false);
    assert_eq!(projected["workspace"]["locationLabel"], "Parent · Ancestor");
    assert_eq!(projected["workspace"]["id"], "workspace-1");
    assert_eq!(projected["recentPersistenceDegraded"], true);
}

#[test]
fn run_start_input_preserves_the_exact_same_session_model_route() {
    let input = serde_json::from_value::<DesktopRunStartInput>(serde_json::json!({
        "sessionId": "session-1",
        "prompt": "continue here",
        "permissionMode": "manual",
        "modelRef": {
            "connectionId": "gateway-team",
            "modelId": "gpt-5"
        },
        "modelSelectionBinding": "selection-binding"
    }))
    .expect("desktop run input should decode");

    let model_ref = input.model_ref.expect("exact route should be present");
    assert_eq!(model_ref.connection_id, "gateway-team");
    assert_eq!(model_ref.model_id, "gpt-5");
    assert_eq!(
        input.model_selection_binding.as_deref(),
        Some("selection-binding")
    );
}

#[test]
fn user_input_select_answers_decode_renderer_camel_case_fields() {
    let single = serde_json::from_value::<DesktopUserInputAnswerInput>(serde_json::json!({
        "questionId": "scope",
        "value": {
            "kind": "single_select",
            "optionId": "workspace",
            "other": null
        }
    }))
    .expect("single-select answer should decode from the renderer contract");
    assert!(matches!(
        single.value,
        DesktopUserInputAnswerValueInput::SingleSelect {
            option_id: Some(ref option_id),
            other: None,
        } if option_id == "workspace"
    ));

    let multiple = serde_json::from_value::<DesktopUserInputAnswerInput>(serde_json::json!({
        "questionId": "checks",
        "value": {
            "kind": "multi_select",
            "optionIds": ["fmt", "test"]
        }
    }))
    .expect("multi-select answer should decode from the renderer contract");
    assert!(matches!(
        multiple.value,
        DesktopUserInputAnswerValueInput::MultiSelect { ref option_ids, .. }
            if option_ids == &["fmt".to_owned(), "test".to_owned()]
    ));
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

#[test]
fn provider_connection_credential_update_projects_only_exact_identity_and_key() {
    let input: DesktopProviderConnectionCredentialUpdateInput =
        serde_json::from_value(serde_json::json!({
            "connectionId":"gateway-beta", "apiKey":"fixture-only-secret",
            "endpoint":"https://must-not-be-used.invalid", "defaultModel":"must-not-be-used",
        }))
        .expect("credential input");
    let native = input.into_native();
    assert_eq!(native.connection_id, "gateway-beta");
    assert_eq!(native.api_key, "fixture-only-secret");
    assert_eq!(
        serde_json::to_value(native).expect("native transport"),
        serde_json::json!({
            "connection_id":"gateway-beta", "api_key":"fixture-only-secret"
        })
    );
    assert!(
        serde_json::from_value::<DesktopProviderConnectionCredentialUpdateInput>(
            serde_json::json!({
                "connectionId":"gateway-beta"
            })
        )
        .is_err()
    );
}
