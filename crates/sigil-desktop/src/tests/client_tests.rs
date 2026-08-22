use super::*;

#[test]
fn error_code_projection_accepts_only_bounded_machine_labels() {
    assert_eq!(
        safe_error_code("stale_cursor".to_owned()).as_deref(),
        Some("stale_cursor")
    );
    assert!(safe_error_code("contains space".to_owned()).is_none());
    assert!(safe_error_code("x".repeat(129)).is_none());
}

#[test]
fn event_stream_owner_revision_requires_exact_opaque_format() {
    assert!(validate_owner_revision(&format!("sha256:{}", "a".repeat(64))).is_ok());
    assert!(validate_owner_revision(&format!("sha256:{}", "A".repeat(64))).is_err());
    assert!(validate_owner_revision("sha256:short").is_err());
}

#[test]
fn intent_stack_projection_accepts_only_bounded_private_free_contract() {
    let state: DesktopIntentStackState = serde_json::from_value(serde_json::json!({
        "status": "available",
        "schema_version": 1,
        "stack": {
            "schema_version": 1,
            "stack_id": "stack-main",
            "stack_version": 7,
            "authority_state": "active",
            "plan_digest": format!("sha256:jcs-v1:{}", "a".repeat(64)),
            "intents": [{
                "intent_ref": {"intent_id": "intent-core", "version": 2},
                "title": "Implement exact drop",
                "statement": "Bind the renderer to a fresh exact preview.",
                "acceptance_criteria": [{
                    "criterion_id": "criterion-exact-binding",
                    "statement": "Only exact operation bindings cross IPC.",
                    "required": true
                }],
                "depends_on": [],
                "source": {"kind": "user_turn", "source_turn_id": "turn-17"},
                "definition_state": "accepted",
                "application_state": "applied",
                "exclusive_artifact_count": 1,
                "shared_artifact_count": 0,
                "unowned_artifact_count": 0,
                "drifted_artifact_count": 0,
                "unavailable_artifact_count": 0,
                "advisory_criterion_count": 0,
                "system_verified_criterion_count": 1,
                "artifacts": [{
                    "artifact_id": "artifact-client",
                    "artifact_kind": "file_hunk",
                    "ownership": "exclusive",
                    "availability": "available",
                    "normalized_relative_path": "apps/desktop/src/bridge.ts"
                }],
                "available_actions": ["drop"]
            }],
            "conflicts": []
        }
    }))
    .expect("bounded stack should decode");

    validate_intent_stack_state(&state).expect("bounded stack should validate");

    let with_private_field = serde_json::json!({
        "status": "not_created",
        "schema_version": 1,
        "safe_message": "No Intent Stack has been created.",
        "session_path": "/private/session.jsonl"
    });
    assert!(serde_json::from_value::<DesktopIntentStackState>(with_private_field).is_err());
}

#[test]
fn intent_drop_preview_rejects_path_escape_and_command_has_only_exact_binding() {
    let preview: DesktopIntentOperationPreview = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "operation_id": "operation-drop-core",
        "operation_kind": "drop",
        "stack_id": "stack-main",
        "stack_version": 7,
        "target_intents": [{"intent_id": "intent-core", "version": 2}],
        "target_is_leaf": true,
        "workspace_revision": 19,
        "file_effects": [{
            "normalized_relative_path": "../private-key",
            "action": "update",
            "artifact_ids": ["artifact-client"]
        }],
        "retained_intents": [],
        "verification_impacts": [],
        "conflicts": [],
        "preview_digest": format!("sha256:jcs-v1:{}", "b".repeat(64))
    }))
    .expect("wire shape should decode before path validation");
    assert!(validate_intent_drop_preview(&preview).is_err());

    let request = DesktopIntentDropRequest {
        operation_id: "operation-drop-core".to_owned(),
        stack_version: 7,
        preview_digest: format!("sha256:jcs-v1:{}", "b".repeat(64)),
    };
    validate_intent_drop_request(&request).expect("exact binding should validate");
    let value = serde_json::to_value(request).expect("request should encode");
    let object = value.as_object().expect("request should be an object");
    assert_eq!(object.len(), 3);
    assert!(object.contains_key("operation_id"));
    assert!(object.contains_key("stack_version"));
    assert!(object.contains_key("preview_digest"));
    assert!(!object.contains_key("path"));
    assert!(!object.contains_key("authority"));
    assert!(!object.contains_key("policy"));
}

#[test]
fn typed_client_debug_never_projects_transport_or_bearer_material() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );
    let debug = format!("{client:?}");

    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("3210"));
}

#[test]
fn run_context_decodes_exact_typed_server_contract() {
    let context: crate::DesktopRunContextView = serde_json::from_value(serde_json::json!({
        "model_ref": {
            "connection_id": "deepseek-default",
            "model_id": "deepseek-v4-flash"
        },
        "provider_name": "deepseek",
        "model_name": "deepseek-v4-flash",
            "model_selection": "same_session",
        "model_selection_binding": "model-binding",
        "model_options": [
            {
                "model_ref": {
                    "connection_id": "deepseek-default",
                    "model_id": "deepseek-v4-flash"
                },
                "display_name": "DeepSeek V4 Flash",
                "availability": "available",
                "recommendation": "recommended",
                "provenance": "bundled",
                "model_name": "deepseek-v4-flash",
                "available_reasoning_efforts": ["low", "medium", "high", "max"],
                "default_reasoning_effort": "max",
                "reasoning_effort_binding": "effort-binding-flash"
            },
            {
                "model_ref": {
                    "connection_id": "deepseek-default",
                    "model_id": "deepseek-v4-pro"
                },
                "display_name": "DeepSeek V4 Pro",
                "availability": "available",
                "recommendation": "standard",
                "provenance": "bundled",
                "model_name": "deepseek-v4-pro",
                "available_reasoning_efforts": ["low", "medium", "high", "max"],
                "default_reasoning_effort": "max",
                "reasoning_effort_binding": "effort-binding-pro"
            }
        ],
        "default_permission_mode": "manual",
        "available_permission_modes": ["read-only", "manual", "auto-edit", "danger-full-access"],
        "available_reasoning_efforts": ["low", "medium", "high", "max"],
        "default_reasoning_effort": "max",
        "reasoning_effort_binding": "effort-binding",
        "context_window_tokens": 1_000_000,
        "last_prompt_tokens": 42_000,
        "cache_usage": {
            "cache_read_tokens": 36_000,
            "cache_miss_tokens": 6_000,
            "cache_write_tokens": 1_024,
            "last_layout_mutation": "conversation_tail_appended",
            "provider_miss_without_local_mutation": false
        },
        "context_window_source": "provider",
        "extension_catalog": {
            "commands": [{
                "canonical": "/intents",
                "aliases": [],
                "label": "Intent Stack",
                "description": "Review durable intents",
                "argument_hint": null,
                "completes_with_space": false,
                "client_action": "open_intent_stack",
                "available": true,
                "unavailable_reason": null
            }],
            "skills": [],
            "agents": []
        }
    }))
    .expect("run context should decode");

    assert_eq!(context.model_ref.connection_id, "deepseek-default");
    assert_eq!(context.model_ref.model_id, "deepseek-v4-flash");
    assert_eq!(context.model_name, "deepseek-v4-flash");
    assert_eq!(context.last_prompt_tokens, Some(42_000));
    assert_eq!(
        context
            .cache_usage
            .as_ref()
            .map(|usage| usage.cache_read_tokens),
        Some(36_000)
    );
    assert_eq!(context.available_reasoning_efforts.len(), 4);
    assert_eq!(context.model_options.len(), 2);
    assert_eq!(
        context.model_options[1].reasoning_effort_binding.as_deref(),
        Some("effort-binding-pro")
    );
    assert_eq!(
        context.reasoning_effort_binding.as_deref(),
        Some("effort-binding")
    );
    assert_eq!(
        context.model_selection,
        crate::DesktopModelSelectionPolicy::SameSession
    );
    assert_eq!(
        context.extension_catalog.commands[0].client_action,
        Some(crate::DesktopApplicationClientAction::OpenIntentStack)
    );
}

#[test]
fn continuity_decodes_nested_owner_and_redacts_durable_scope_from_debug() {
    let continuity: crate::DesktopSessionContinuityView =
        serde_json::from_value(serde_json::json!({
            "durable_session_scope_id": "durable-private-scope",
            "durable_frontier": { "through_stream_sequence": 17 },
            "foreground_owner": {
                "run_id": "http-run-7",
                "owner_revision": format!("sha256:{}", "a".repeat(64))
            },
            "recovery_actions": ["retry_current", "continue_read_only"]
        }))
        .expect("continuity should decode");

    assert_eq!(continuity.durable_frontier.through_stream_sequence, 17);
    assert_eq!(
        continuity
            .foreground_owner
            .as_ref()
            .map(|owner| owner.run_id.as_str()),
        Some("http-run-7")
    );
    assert_eq!(continuity.recovery_actions.len(), 2);
    assert!(!format!("{continuity:?}").contains("durable-private-scope"));
}

#[test]
fn compaction_review_and_apply_action_preserve_exact_preview_binding() {
    let review: crate::DesktopCompactionReview = serde_json::from_value(serde_json::json!({
        "preview_id": "compact-preview-1",
        "folded_event_count": 8,
        "retained_event_count": 4,
        "policy": {
            "strategy": "cache_aware_v3",
            "phase": "prepare",
            "forecast_confidence": "medium",
            "admission_reason": "qualified_cost_savings",
            "native_carrier_available": true
        },
        "admission": {
            "kind": "ready",
            "economics": {
                "before_input_tokens": 12_000,
                "target_input_tokens": 4_000,
                "context_window_tokens": 128_000,
                "output_tokens": 8_000,
                "safety_buffer_tokens": 2_000,
                "savings_tokens": 8_000,
                "savings_ratio_ppm": 666_666,
                "minimum_savings_tokens": 1_000,
                "minimum_savings_ratio_ppm": 100_000,
                "summary_cache_read_tokens": 800,
                "summary_uncached_input_tokens": 200,
                "summary_output_tokens": 64,
                "summary_cost_nano_usd": 42
            }
        }
    }))
    .expect("compaction review should decode");

    assert_eq!(review.preview_id.as_deref(), Some("compact-preview-1"));
    assert_eq!(
        review
            .policy
            .as_ref()
            .map(|policy| policy.strategy.as_str()),
        Some("cache_aware_v3")
    );
    assert!(matches!(
        review.admission,
        crate::DesktopCompactionAdmission::Ready { .. }
    ));
    assert_eq!(
        serde_json::to_value(
            crate::DesktopConversationRecoveryCommandAction::ApplyCompaction {
                preview_id: "compact-preview-1".to_owned(),
            }
        )
        .expect("compaction action should encode"),
        serde_json::json!({
            "kind": "apply_compaction",
            "preview_id": "compact-preview-1"
        })
    );

    let prepared: crate::DesktopCompactionReview = serde_json::from_value(serde_json::json!({
        "preview_id": "compact-preview-local",
        "folded_event_count": 8,
        "retained_event_count": 4,
        "admission": {
            "kind": "prepared",
            "standalone_tool_output_shrink_available": true
        }
    }))
    .expect("local compaction review should decode");
    assert!(matches!(
        prepared.admission,
        crate::DesktopCompactionAdmission::Prepared {
            standalone_tool_output_shrink_available: true,
        }
    ));
    assert_eq!(
        serde_json::to_value(
            crate::DesktopConversationRecoveryCommandAction::PrepareCompaction {
                preview_id: "compact-preview-local".to_owned(),
            }
        )
        .expect("summary preparation action should encode"),
        serde_json::json!({
            "kind": "prepare_compaction",
            "preview_id": "compact-preview-local"
        })
    );
    assert_eq!(
        serde_json::to_value(
            crate::DesktopConversationRecoveryCommandAction::ApplyStandaloneToolOutputShrink {
                preview_id: "compact-preview-local".to_owned(),
            }
        )
        .expect("standalone shrink action should encode"),
        serde_json::json!({
            "kind": "apply_standalone_tool_output_shrink",
            "preview_id": "compact-preview-local"
        })
    );
}

#[test]
fn recovery_receipt_decodes_compaction_without_weakening_durable_identity() {
    let receipt: crate::DesktopConversationRecoveryCommandReceipt =
        serde_json::from_value(serde_json::json!({
            "command_id": "command-1",
            "client_id": "desktop-1",
            "session_id": "session-1",
            "action": "apply_compaction",
            "compaction": {
                "compaction_id": "compaction-1",
                "attempt_id": "attempt-1",
                "task_memory_id": "memory-1",
                "folded_event_count": 8,
                "tool_output_projection_recorded": true,
                "native_carrier_materialized": false
            },
            "recovery": {
                "checkpoints": [],
                "fork_points": [],
                "through_stream_sequence": 42
            },
            "correlation_id": "correlation-1",
            "replayed": false
        }))
        .expect("recovery receipt should decode");

    assert_eq!(
        receipt
            .compaction
            .as_ref()
            .map(|compaction| compaction.compaction_id.as_str()),
        Some("compaction-1")
    );
    assert_eq!(receipt.recovery.through_stream_sequence, 42);
    assert!(!receipt.replayed);
}

#[test]
fn conversation_display_decodes_exact_decimal_text_and_opaque_cursor() {
    assert_eq!(
        serde_json::from_str::<crate::DesktopConversationDisplayStatus>("\"awaiting_user_input\"")
            .expect("durable user-input terminal status should decode"),
        crate::DesktopConversationDisplayStatus::AwaitingUserInput,
    );
    let page: crate::DesktopConversationDisplayPage = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "through_session_stream_sequence": "9007199254740993",
        "terminal_frontier": {
            "run_id": "run-1",
            "session_stream_sequence": "9007199254740994",
            "status": "succeeded"
        },
        "total_items": "9007199254740995",
        "items": [{
            "schema_version": 1,
            "display_id": "display-1",
            "display_order": {
                "session_stream_sequence": "9007199254740993",
                "subindex": 0
            },
            "source_event_id": "event-1",
            "kind": "assistant_message",
            "source": "durable_transcript",
            "run_id": "run-1",
            "run_sequence": "9007199254740996",
            "status": "completed",
            "content": {
                "type": "message",
                "role": "assistant",
                "text": "done",
                "assistant_phase": "final_answer",
                "image_attachment_count": 0,
                "truncated": false,
                "original_content_bytes": 4
            },
            "reconciles": ["live-1"]
        }],
        "next_cursor": "opaque_CURSOR-1",
        "has_more": true,
        "gap_facts": [{
            "kind": "retention",
            "after_session_stream_sequence": "9007199254740997"
        }],
        "live_provisional_anchor": {
            "durable_frontier": "9007199254740993",
            "run_id": "run-live",
            "run_sequence": "9007199254740998"
        },
        "task_control": {
            "schema_version": 1,
            "task_id": "task-restart-control",
            "phase": "execution",
            "status": "paused",
            "plan_version": 1,
            "plan_status": "accepted",
            "steps": [{
                "step_id": "inspect-code",
                "title": "Inspect code",
                "role": "subagent_read",
                "depends_on": [],
                "mode": "read",
                "isolation": "shared_read_only",
                "status": "interrupted"
            }],
            "steps_truncated": false,
            "active_children": 0,
            "completed_children": 0,
            "failed_children": 1,
            "lanes": [],
            "lanes_truncated": false,
            "can_continue": true
        }
    }))
    .expect("canonical display page should decode");

    assert_eq!(page.through_session_stream_sequence, "9007199254740993");
    assert_eq!(page.total_items, "9007199254740995");
    assert_eq!(
        page.items[0].display_order.session_stream_sequence,
        "9007199254740993"
    );
    assert_eq!(
        page.items[0].run_sequence.as_deref(),
        Some("9007199254740996")
    );
    assert_eq!(page.next_cursor.as_deref(), Some("opaque_CURSOR-1"));
    assert_eq!(
        page.live_provisional_anchor
            .as_ref()
            .map(|anchor| anchor.run_sequence.as_str()),
        Some("9007199254740998")
    );
    let task = page
        .task_control
        .as_ref()
        .expect("durable task control should decode");
    assert_eq!(task.task_id, "task-restart-control");
    assert_eq!(task.status, "paused");
    assert_eq!(task.steps[0].status.as_deref(), Some("interrupted"));
    assert!(task.can_continue);
    validate_conversation_display_page(&page, "http-session-1")
        .expect("bounded durable task control should validate");

    let mut typed_tool_page = page.clone();
    typed_tool_page.items[0].content = crate::DesktopConversationDisplayContent::Tool {
        call_id: Some("call-1".to_owned()),
        tool_name: Some("shell".to_owned()),
        output: Some("bounded preview".to_owned()),
        truncated: true,
        original_content_bytes: 8_363,
        artifact_ref: Some(format!("ta1_{}", "a".repeat(32))),
        artifact_availability: Some(crate::DesktopToolArtifactAvailability::Available),
        observed_bytes: Some(8_363),
        // Safe persistence projection may expand the stored representation, for example when
        // control material is replaced or escaped. These are independent truthful coordinates.
        persisted_bytes: Some(8_403),
        has_more: true,
        preview_truncated: true,
        truncation_reason: Some("initial_cap".to_owned()),
        capture_completeness: Some(
            "source=complete,policy=preserved,storage=truncated_at_limit".to_owned(),
        ),
    };
    validate_conversation_display_page(&typed_tool_page, "http-session-1")
        .expect("typed artifact metadata should validate");
    if let crate::DesktopConversationDisplayContent::Tool {
        artifact_availability,
        ..
    } = &mut typed_tool_page.items[0].content
    {
        *artifact_availability = None;
    }
    assert!(matches!(
        validate_conversation_display_page(&typed_tool_page, "http-session-1"),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut oversized = page.clone();
    let task = oversized
        .task_control
        .as_mut()
        .expect("task control should remain present");
    let template = task.steps[0].clone();
    while task.steps.len() <= MAX_CONVERSATION_TASK_CONTROL_ITEMS {
        task.steps.push(template.clone());
    }
    assert!(matches!(
        validate_conversation_display_page(&oversized, "http-session-1"),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut terminal = page;
    terminal
        .task_control
        .as_mut()
        .expect("task control should remain present")
        .status = "completed".to_owned();
    assert!(matches!(
        validate_conversation_display_page(&terminal, "http-session-1"),
        Err(DesktopClientError::InvalidResponse)
    ));
}

#[test]
fn conversation_display_rejects_noncanonical_decimal_text() {
    for invalid in ["01", "18446744073709551616", "-1", "1.0"] {
        let result =
            serde_json::from_value::<crate::DesktopConversationDisplayPage>(serde_json::json!({
                "schema_version": 1,
                "request_scope": "http-session-1",
                "through_session_stream_sequence": invalid,
                "total_items": "0",
                "items": [],
                "has_more": false,
                "gap_facts": []
            }));
        assert!(result.is_err(), "{invalid} must be rejected");
    }
}

#[test]
fn plan_review_surface_validates_bounded_typed_decisions() {
    let base = serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "through_session_stream_sequence": "12",
        "total_items": "0",
        "items": [],
        "has_more": false,
        "gap_facts": [],
        "plan_review": {
            "plan_id": "plan-review-1",
            "plan_hash": format!("sha256:{}", "a".repeat(64)),
            "status": "draft_ready",
            "summary": "Refactor the workspace snapshot binding",
            "summary_truncated": false,
            "step_count": 3,
            "target_path_count": 2,
            "suggested_check_count": 1,
            "risk": "touches the recovery path",
            "allowed_actions": ["run", "save", "revise", "reject"],
            "source": "automatic_conversation_route",
            "stale": false
        }
    });
    let page: crate::DesktopConversationDisplayPage =
        serde_json::from_value(base.clone()).expect("draft-ready plan review should decode");
    validate_conversation_display_page(&page, "http-session-1")
        .expect("draft-ready plan review should validate");

    let mut non_draft = page.clone();
    let non_draft_review = non_draft.plan_review.as_mut().expect("plan review present");
    non_draft_review.status = crate::DesktopPlanReviewStatus::Started;
    non_draft_review.allowed_actions = Vec::new();
    validate_conversation_display_page(&non_draft, "http-session-1").expect(
        "a Started attempt with draft details should still validate (status projects first)",
    );

    let mut partial_actions = page.clone();
    partial_actions
        .plan_review
        .as_mut()
        .expect("plan review present")
        .allowed_actions = vec![crate::DesktopPlanAction::Run];
    validate_conversation_display_page(&partial_actions, "http-session-1")
        .expect("saved-only plans may expose a strict action subset");

    let mut revision_running = page.clone();
    revision_running
        .plan_review
        .as_mut()
        .expect("plan review present")
        .allowed_actions = Vec::new();
    validate_conversation_display_page(&revision_running, "http-session-1")
        .expect("a revision-running base plan intentionally exposes no actions");

    let mut duplicate_actions = page.clone();
    duplicate_actions
        .plan_review
        .as_mut()
        .expect("plan review present")
        .allowed_actions = vec![crate::DesktopPlanAction::Run, crate::DesktopPlanAction::Run];
    assert!(matches!(
        validate_conversation_display_page(&duplicate_actions, "http-session-1"),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut bad_hash = page.clone();
    bad_hash
        .plan_review
        .as_mut()
        .expect("plan review present")
        .plan_hash = Some("not-a-hash".to_owned());
    assert!(matches!(
        validate_conversation_display_page(&bad_hash, "http-session-1"),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut stale = page;
    stale
        .plan_review
        .as_mut()
        .expect("plan review present")
        .stale = true;
    validate_conversation_display_page(&stale, "http-session-1")
        .expect("stale draft-ready plan review should still validate");

    let mut undecided = serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "through_session_stream_sequence": "13",
        "total_items": "0",
        "items": [],
        "has_more": false,
        "gap_facts": []
    });
    undecided["plan_review"] = serde_json::json!({
        "plan_id": "plan-review-2",
        "plan_hash": format!("sha256:{}", "b".repeat(64)),
        "status": "started",
        "summary": "Review in progress without a draft",
        "summary_truncated": false,
        "step_count": 0,
        "target_path_count": 0,
        "suggested_check_count": 0,
        "allowed_actions": [],
        "source": "explicit_plan_command",
        "stale": false
    });
    let page: crate::DesktopConversationDisplayPage =
        serde_json::from_value(undecided).expect("non-draft plan review should decode");
    validate_conversation_display_page(&page, "http-session-1")
        .expect("non-draft plan review with no actions should validate");

    // A durable attempt without any draft fields must decode and validate: the status projects
    // first and draft-specific details are absent, which is the reload/reconnect shape.
    let draft_less = serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "through_session_stream_sequence": "14",
        "total_items": "0",
        "items": [],
        "has_more": false,
        "gap_facts": [],
        "plan_review": {
            "plan_id": "plan-review-3",
            "status": "cancelled",
            "summary_truncated": false,
            "allowed_actions": [],
            "source": "automatic_conversation_route",
            "stale": false
        }
    });
    let page: crate::DesktopConversationDisplayPage =
        serde_json::from_value(draft_less).expect("draft-less plan review should decode");
    validate_conversation_display_page(&page, "http-session-1")
        .expect("draft-less terminal plan review should validate");
}

#[test]
fn tool_artifact_page_decodes_only_bounded_path_free_contract() {
    assert!(valid_tool_artifact_continuation(
        &crate::DesktopToolArtifactSelector::LinePage {
            start_line: 0,
            line_count: 1,
        },
        Some(&crate::DesktopToolArtifactSelector::ByteSlice {
            offset: DESKTOP_TOOL_ARTIFACT_MAX_PAGE_BYTES as u64,
            limit: DESKTOP_TOOL_ARTIFACT_MAX_PAGE_BYTES,
        }),
        false,
    ));
    let request = crate::DesktopToolArtifactReadRequest {
        artifact_ref: format!("ta1_{}", "a".repeat(32)),
        selector: crate::DesktopToolArtifactSelector::ByteSlice {
            offset: 0,
            limit: 16,
        },
    };
    let page: crate::DesktopToolArtifactPage = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "artifact_ref": format!("ta1_{}", "a".repeat(32)),
        "selector": {
            "kind": "byte_slice",
            "offset": 0,
            "limit": 16
        },
        "body": "bounded page",
        "body_encoding": "utf8",
        "returned_bytes": 12,
        "page_sha256": sha256_prefixed(b"bounded page"),
        "artifact_sha256": format!("sha256:{}", "c".repeat(64)),
        "eof": true,
        "match_count": 0
    }))
    .expect("bounded artifact page should decode");

    validate_tool_artifact_page(&page, "http-session-1", &request)
        .expect("bounded artifact page should validate");

    let with_path = serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "artifact_ref": format!("ta1_{}", "a".repeat(32)),
        "selector": {"kind": "byte_slice", "offset": 0, "limit": 16},
        "body": "bounded page",
        "body_encoding": "utf8",
        "returned_bytes": 12,
        "page_sha256": sha256_prefixed(b"bounded page"),
        "artifact_sha256": format!("sha256:{}", "c".repeat(64)),
        "eof": true,
        "match_count": 0,
        "artifact_path": "/private/session/artifacts/blob"
    });
    assert!(serde_json::from_value::<crate::DesktopToolArtifactPage>(with_path).is_err());

    let mut wrong_scope = page.clone();
    wrong_scope.request_scope = "other-session".to_owned();
    assert!(matches!(
        validate_tool_artifact_page(&wrong_scope, "http-session-1", &request),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut invalid_page_hash = page.clone();
    invalid_page_hash.page_sha256 = format!("sha256:{}", "b".repeat(64));
    assert!(matches!(
        validate_tool_artifact_page(&invalid_page_hash, "http-session-1", &request),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut invalid_continuation = page.clone();
    invalid_continuation.next_selector = Some(crate::DesktopToolArtifactSelector::ByteSlice {
        offset: 16,
        limit: 16,
    });
    assert!(matches!(
        validate_tool_artifact_page(&invalid_continuation, "http-session-1", &request),
        Err(DesktopClientError::InvalidResponse)
    ));

    let mut invalid_encoding = page;
    invalid_encoding.body_encoding = crate::DesktopToolArtifactPageEncoding::Base64;
    assert!(matches!(
        validate_tool_artifact_page(&invalid_encoding, "http-session-1", &request),
        Err(DesktopClientError::InvalidResponse)
    ));
}

#[test]
fn conversation_display_tool_decodes_typed_artifact_metadata() {
    let content: crate::DesktopConversationDisplayContent =
        serde_json::from_value(serde_json::json!({
            "type": "tool",
            "call_id": "call-1",
            "tool_name": "shell",
            "output": "bounded preview",
            "truncated": true,
            "original_content_bytes": 1048576,
            "artifact_ref": format!("ta1_{}", "d".repeat(32)),
            "artifact_availability": "available",
            "observed_bytes": 1048576,
            "persisted_bytes": 1048576,
            "has_more": true
        }))
        .expect("typed tool display metadata should decode");

    assert!(matches!(
        content,
        crate::DesktopConversationDisplayContent::Tool {
            artifact_ref: Some(_),
            has_more: true,
            ..
        }
    ));

    let invalid_availability =
        serde_json::from_value::<crate::DesktopConversationDisplayContent>(serde_json::json!({
            "type": "tool",
            "output": "bounded preview",
            "truncated": false,
            "original_content_bytes": 15,
            "artifact_availability": "/private/session/artifacts/blob",
            "has_more": false
        }));
    assert!(invalid_availability.is_err());
}

#[test]
fn conversation_queue_decodes_bounded_secret_free_rows() {
    let view: crate::DesktopConversationQueueView = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "session_id": "session-1",
        "generation": "queue-v1:17:event-1",
        "paused": false,
        "total_items": 1,
        "items": [{
            "entry_id": "queue-1",
            "order": 0,
            "kind": "chat",
            "status": "queued",
            "prompt_preview": "[redacted]",
            "prompt_preview_truncated": false,
            "prompt_material": "available_process_local",
            "dispatchable": false,
            "blocked_reason": "foreground_run_active",
            "created_at_ms": 10,
            "updated_at_ms": 11
        }],
        "truncated": false
    }))
    .expect("queue view should decode");

    validate_conversation_queue_view("session-1", &view)
        .expect("bounded queue view should validate");
    assert_eq!(view.generation.0, "queue-v1:17:event-1");
    assert_eq!(
        view.items[0].prompt_material,
        crate::DesktopConversationQueuePromptMaterial::AvailableProcessLocal
    );
    let debug = format!("{view:?}");
    assert!(!debug.contains("prompt_hash"));
    assert!(!debug.contains("exact private prompt"));
}

#[test]
fn conversation_queue_rejects_unbounded_or_inconsistent_server_views() {
    let item = crate::DesktopConversationQueueItem {
        entry_id: "queue-1".to_owned(),
        order: 0,
        kind: crate::DesktopConversationQueueItemKind::Chat,
        status: crate::DesktopConversationQueueItemStatus::Queued,
        prompt_preview: "safe".to_owned(),
        prompt_preview_truncated: false,
        prompt_material: crate::DesktopConversationQueuePromptMaterial::PersistedSafe,
        dispatchable: true,
        blocked_reason: None,
        created_at_ms: None,
        updated_at_ms: None,
    };
    let base = crate::DesktopConversationQueueView {
        schema_version: 1,
        session_id: "session-1".to_owned(),
        generation: crate::DesktopConversationQueueGeneration("queue-v1:0:initial".to_owned()),
        paused: false,
        total_items: 1,
        items: vec![item.clone()],
        truncated: false,
        next_dispatchable_entry_id: Some("queue-1".to_owned()),
    };
    validate_conversation_queue_view("session-1", &base).expect("consistent view should validate");

    let mut oversized = base.clone();
    oversized.total_items = 101;
    oversized.items = vec![item; 101];
    assert!(validate_conversation_queue_view("session-1", &oversized).is_err());

    let mut missing_block = base;
    missing_block.items[0].dispatchable = false;
    assert!(validate_conversation_queue_view("session-1", &missing_block).is_err());

    let truncated = crate::DesktopConversationQueueView {
        schema_version: 1,
        session_id: "session-1".to_owned(),
        generation: crate::DesktopConversationQueueGeneration("queue-v1:0:initial".to_owned()),
        paused: false,
        total_items: 2,
        items: vec![crate::DesktopConversationQueueItem {
            entry_id: "queue-1".to_owned(),
            order: 0,
            kind: crate::DesktopConversationQueueItemKind::Chat,
            status: crate::DesktopConversationQueueItemStatus::Queued,
            prompt_preview: "redacted".to_owned(),
            prompt_preview_truncated: false,
            prompt_material: crate::DesktopConversationQueuePromptMaterial::RequiresReentry,
            dispatchable: false,
            blocked_reason: Some(crate::DesktopConversationQueueBlockedReason::RequiresReentry),
            created_at_ms: None,
            updated_at_ms: None,
        }],
        truncated: true,
        next_dispatchable_entry_id: Some("queue-2".to_owned()),
    };
    validate_conversation_queue_view("session-1", &truncated)
        .expect("a bounded view may point at the next row beyond its returned window");
}

#[test]
fn conversation_queue_command_serializes_cas_and_owner_binding() {
    let request = crate::DesktopConversationQueueCommandRequest {
        expected_generation: crate::DesktopConversationQueueGeneration(
            "queue-v1:17:event-1".to_owned(),
        ),
        action: crate::DesktopConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id: "run-7".to_owned(),
            foreground_owner_revision: format!("sha256:{}", "a".repeat(64)),
        },
    };
    validate_conversation_queue_command(&request).expect("exact owner binding should validate");
    let value = serde_json::to_value(&request).expect("queue command should serialize");
    assert_eq!(value["expected_generation"], "queue-v1:17:event-1");
    assert_eq!(value["action"]["action"], "interrupt_and_run_next");
    assert_eq!(value["action"]["foreground_run_id"], "run-7");

    let invalid = crate::DesktopConversationQueueCommandRequest {
        expected_generation: crate::DesktopConversationQueueGeneration(
            "queue-v1:17:event-1".to_owned(),
        ),
        action: crate::DesktopConversationQueueCommandAction::Reorder {
            entry_id: "queue-1".to_owned(),
            after_entry_id: Some("queue-1".to_owned()),
        },
    };
    assert!(validate_conversation_queue_command(&invalid).is_err());
}

#[test]
fn conversation_queue_receipt_echoes_cas_and_exact_interrupt_owner() {
    let receipt: crate::DesktopConversationQueueCommandReceipt =
        serde_json::from_value(serde_json::json!({
            "command_id": "command-queue-1",
            "client_id": "desktop-1",
            "session_id": "session-1",
            "action": "interrupt_and_run_next",
            "expected_generation": "queue-v1:17:event-1",
            "generation": "queue-v1:18:event-2",
            "interrupt_owner": {
                "run_id": "run-7",
                "owner_revision": format!("sha256:{}", "a".repeat(64))
            },
            "queue": {
                "schema_version": 1,
                "session_id": "session-1",
                "generation": "queue-v1:18:event-2",
                "paused": false,
                "total_items": 0,
                "items": [],
                "truncated": false,
                "next_dispatchable_entry_id": null
            },
            "correlation_id": "event-2",
            "replayed": false
        }))
        .expect("queue receipt should decode");

    assert_eq!(receipt.expected_generation.0, "queue-v1:17:event-1");
    let owner = receipt
        .interrupt_owner
        .as_ref()
        .expect("interrupt receipt should bind the exact foreground owner");
    assert_eq!(owner.run_id, "run-7");
    assert_eq!(owner.owner_revision, format!("sha256:{}", "a".repeat(64)));
    validate_conversation_queue_view("session-1", &receipt.queue)
        .expect("receipt queue projection should remain bounded and consistent");
}

#[test]
fn approval_receipt_preserves_exact_route_identity_and_revision() {
    let receipt: crate::DesktopApprovalCommandReceipt = serde_json::from_value(serde_json::json!({
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
            "decision": "approved_for_session",
            "reason": "approved in Desktop"
        },
        "route_state": "decision_accepted",
        "registry_revision": 8,
        "replayed": false
    }))
    .expect("approval receipt should decode");

    assert_eq!(receipt.approval_request_id, "approval-1");
    assert_eq!(
        receipt.route_state,
        crate::DesktopApprovalRouteState::DecisionAccepted
    );
    assert_eq!(receipt.registry_revision, 8);
    assert_eq!(
        receipt.decision.decision,
        crate::DesktopApprovalRecordedDecision::ApprovedForSession
    );
}

#[test]
fn agent_activity_decodes_bounded_result_handoff_without_storage_identity() {
    let activity: crate::DesktopAgentActivityView = serde_json::from_value(serde_json::json!({
        "total_agents": 1,
        "active_agents": 0,
        "terminal_agents": 1,
        "items": [{
            "thread_id": "agent_review",
            "profile_id": "explore",
            "display_name": "Repository review",
            "objective": "Inspect the architecture",
            "status": "completed",
            "handoff_status": "returned",
            "result_summary": "The bounded result reached the parent conversation.",
            "result_summary_truncated": false,
            "usage": {
                "input_tokens": 240,
                "output_tokens": 80,
                "total_tokens": 320,
                "cached_tokens": 40
            }
        }]
    }))
    .expect("agent activity should decode");

    assert_eq!(activity.total_agents, 1);
    assert_eq!(activity.items[0].thread_id, "agent_review");
    assert_eq!(
        activity.items[0].handoff_status,
        crate::DesktopAgentHandoffStatus::Returned
    );
    assert_eq!(
        activity.items[0]
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(320)
    );
    let debug = format!("{activity:?}");
    assert!(!debug.contains("session_ref"));
    assert!(!debug.contains("output_hash"));
    assert!(!debug.contains("changed_paths"));
}

#[test]
fn task_continuation_serializes_as_an_exact_non_chat_run_start() {
    let request = crate::DesktopRunStartRequest {
        prompt: String::new(),
        permission_mode: crate::DesktopPermissionMode::Manual,
        model_ref: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: Some(crate::DesktopTaskContinuationRequest {
            task_id: "task_1".to_owned(),
            guidance: Some("Prefer the smaller compatibility fix.".to_owned()),
        }),
    };

    assert_eq!(
        serde_json::to_value(request).expect("Task continuation should encode"),
        serde_json::json!({
            "prompt": "",
            "permission_mode": "manual",
            "task_continuation": {
                "task_id": "task_1",
                "guidance": "Prefer the smaller compatibility fix."
            }
        })
    );
}

#[test]
fn run_start_serializes_an_exact_same_session_model_route() {
    let request = crate::DesktopRunStartRequest {
        prompt: "continue here".to_owned(),
        permission_mode: crate::DesktopPermissionMode::Manual,
        model_ref: Some(crate::DesktopProviderModelRef {
            connection_id: "gateway-team".to_owned(),
            model_id: "gpt-5".to_owned(),
        }),
        model_selection_binding: Some("selection-binding".to_owned()),
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        task_continuation: None,
    };

    assert_eq!(
        serde_json::to_value(request).expect("model route should encode"),
        serde_json::json!({
            "prompt": "continue here",
            "permission_mode": "manual",
            "model_ref": {
                "connection_id": "gateway-team",
                "model_id": "gpt-5"
            },
            "model_selection_binding": "selection-binding"
        })
    );
}

#[test]
fn task_pause_request_identity_matches_the_shared_content_binding() {
    let request = desktop_task_pause_request(
        "task_1",
        crate::DesktopTaskExecutionBinding::Plan { plan_version: 3 },
    );

    assert_eq!(
        request.request_id,
        "task-pause-8255cff6d4b82d53e2f95ed642e5cac61db6837bb1428027a4e1fb48f854ce0b"
    );
    assert_eq!(
        serde_json::to_value(request).expect("Task pause should encode"),
        serde_json::json!({
            "request_id": "task-pause-8255cff6d4b82d53e2f95ed642e5cac61db6837bb1428027a4e1fb48f854ce0b",
            "task_id": "task_1",
            "execution": { "kind": "plan", "plan_version": 3 }
        })
    );
}

#[test]
fn terminal_task_cancel_contract_is_exact_generation_bound_and_bounded() {
    let request = crate::DesktopTerminalTaskCancelRequest {
        task_id: "terminal_1".to_owned(),
        expected_generation: 4,
    };
    assert_eq!(
        serde_json::to_value(request).expect("terminal cancel should encode"),
        serde_json::json!({
            "task_id": "terminal_1",
            "expected_generation": 4
        })
    );

    let receipt: crate::DesktopTerminalTaskCancelCommandReceipt =
        serde_json::from_value(serde_json::json!({
            "command_id": "command-1",
            "client_id": "desktop-1",
            "session_id": "session-1",
            "run_id": "run-1",
            "terminal_task": {
                "task_id": "terminal_1",
                "generation": 5,
                "status": {"state": "cancelled"},
                "readiness": {"state": "ready", "kind": "output_contains", "ready_at_ms": 12},
                "total_output_bytes": 24,
                "emitted_at_ms": 13
            },
            "replayed": false
        }))
        .expect("terminal cancel receipt should decode");
    assert_eq!(receipt.run_id, "run-1");
    assert_eq!(receipt.terminal_task.generation, 5);
    assert_eq!(receipt.terminal_task.task_id, "terminal_1");
    assert_eq!(receipt.expected_stream_sequence, None);
}

#[test]
fn task_integration_review_validates_exact_diff_and_private_ref_free_projection() {
    let aggregate_diff = "diff --git a/src/lib.rs b/src/lib.rs\n+safe\n";
    let preview_digest = format!("sha256:{}", "b".repeat(64));
    let review: crate::DesktopTaskIntegrationReviewView =
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "request": {
                "request_id": format!("integration-review-{}", "a".repeat(64)),
                "task_id": "task_1",
                "plan_id": "integration_plan_1",
                "plan_version": 2,
                "preview_digest": preview_digest
            },
            "aggregate_diff": aggregate_diff,
            "aggregate_diff_digest": sha256_prefixed(aggregate_diff.as_bytes()),
            "preview_digest": preview_digest,
            "policy_digest": format!("sha256:{}", "c".repeat(64)),
            "target_kind": "workspace_apply",
            "lanes": [{
                "lane_id": "lane_1",
                "candidate_kind": "managed_ref",
                "proposal_count": 2,
                "verification_receipt_count": 1
            }],
            "child_verification_receipt_count": 2,
            "lane_verification_receipt_count": 1,
            "conflict_reasons": ["path_overlap"],
            "verification_invalidation_count": 1,
            "parent_verification_pending": true
        }))
        .expect("integration review should decode");

    validate_task_integration_review_view(&review).expect("exact review should validate");
    let debug = format!("{review:?}");
    assert!(!debug.contains("refs/sigil"));
    assert!(!debug.contains("worktree_path"));
    assert!(!debug.contains("artifact_ref"));

    let mut substituted = review;
    substituted.aggregate_diff.push_str("+substituted\n");
    assert!(validate_task_integration_review_view(&substituted).is_err());
}

#[test]
fn task_integration_acceptance_command_preserves_exact_request_identity() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );
    let request = crate::DesktopTaskIntegrationReviewRequest {
        request_id: format!("integration-review-{}", "a".repeat(64)),
        task_id: "task_1".to_owned(),
        plan_id: "integration_plan_1".to_owned(),
        plan_version: 2,
        preview_digest: format!("sha256:{}", "b".repeat(64)),
    };

    let command = client.command("http-session-1", None, request.clone());
    assert_eq!(command.session_id, "http-session-1");
    assert_eq!(command.expected_stream_sequence, None);
    assert_eq!(command.payload, request);
    assert!(command.command_id.starts_with("desktop-command-"));
}

#[test]
fn support_report_decodes_only_the_path_free_contract() {
    let report: crate::DesktopSupportDoctorReport = serde_json::from_value(serde_json::json!({
        "generated_at_unix_ms": 123,
        "version": "0.0.1-test",
        "commit": "abc123",
        "target": "aarch64-apple-darwin",
        "profile": "debug",
        "environment": {
            "os": "macos",
            "architecture": "aarch64",
            "terminal_family": "other"
        },
        "summary": { "overall_status": "warn", "ok": 4, "warn": 1, "error": 0 },
        "checks": [{
            "status": "warn",
            "name": "configuration",
            "summary": "review one setting",
            "remediation": "update configuration"
        }],
        "privacy": {
            "included": ["build metadata"],
            "excluded": ["local paths"],
            "review_before_sharing": true
        }
    }))
    .expect("support report should decode");

    assert_eq!(report.summary.warn, 1);
    assert_eq!(report.checks[0].name, "configuration");
    assert_eq!(report.privacy.excluded, ["local paths"]);
}

#[test]
fn provider_connection_inventory_decodes_only_the_secret_free_native_contract() {
    let inventory: crate::DesktopProviderConnectionInventory =
        serde_json::from_value(serde_json::json!({
            "config_mode": "v2",
            "default_model": {
                "connection_id": "openai-personal",
                "model_id": "gpt-4.1"
            },
            "connections": [{
                "id": "openai-personal",
                "label": "OpenAI personal",
                "provider_label": "OpenAI",
                "protocol_label": "Responses",
                "endpoint_display": "api.openai.com",
                "credential_source": "stored",
                "readiness": "ready",
                "default_model": {
                    "connection_id": "openai-personal",
                    "model_id": "gpt-4.1"
                }
            }],
            "issues": []
        }))
        .expect("provider connection inventory should decode");

    assert_eq!(inventory.config_mode, crate::DesktopProviderConfigMode::V2);
    assert_eq!(
        inventory
            .default_model
            .as_ref()
            .map(|route| { (route.connection_id.as_str(), route.model_id.as_str()) }),
        Some(("openai-personal", "gpt-4.1"))
    );
    assert_eq!(
        inventory.connections[0].credential_source,
        crate::DesktopProviderCredentialSource::Stored
    );
}

#[tokio::test]
async fn plan_decision_revise_accepts_the_supervised_revision_run_identity() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("loopback listener should expose its address");
    let plan_hash = format!("sha256:{}", "d".repeat(64));
    let server_plan_hash = plan_hash.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let mut buffer = vec![0_u8; 8_192];
        let read = stream.read(&mut buffer).await.expect("request should read");
        assert!(read > 0, "request closed before it completed");
        let request_text = String::from_utf8_lossy(&buffer[..read]).into_owned();
        let request_body = request_text
            .split("\r\n\r\n")
            .nth(1)
            .expect("request should carry a JSON body");
        let request_json: serde_json::Value =
            serde_json::from_str(request_body).expect("request body should be JSON");
        let response_body = serde_json::json!({
            "command_id": request_json["command_id"],
            "client_id": request_json["client_id"],
            "session_id": "desktop-session-1",
            "plan_id": "plan-review-1",
            "plan_hash": server_plan_hash,
            "action": "revise",
            "task_id": null,
            "revision_run_id": "plan-review-abc-123",
            "replayed": false
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
    });

    let client = DesktopHttpClient::new(
        Client::new(),
        address,
        Arc::new(DesktopBearerToken::generate().expect("token should generate")),
    );
    let receipt = client
        .plan_decision(
            "desktop-session-1",
            "plan-review-1",
            &plan_hash,
            crate::DesktopPlanDecisionAction::Revise,
        )
        .await
        .expect("the HTTP Revise success response must decode through the strict DTO");
    assert_eq!(receipt.action, crate::DesktopPlanDecisionAction::Revise);
    assert_eq!(
        receipt.revision_run_id.as_deref(),
        Some("plan-review-abc-123"),
        "the supervised revision run identity must cross the native trust boundary"
    );
    server.await.expect("server task should complete");
}

#[tokio::test]
async fn save_provider_default_model_uses_the_exact_put_route_and_compound_identity() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("loopback listener should expose its address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1_024];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.expect("request should read");
            assert!(read > 0, "request closed before its headers completed");
            request.extend_from_slice(&buffer[..read]);
            if let Some(offset) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end]).expect("headers should be UTF-8");
        assert!(
            headers.starts_with("PUT /settings/provider-connections/default-model HTTP/1.1\r\n")
        );
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer ")
        );
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::parse::<usize>)
            })
            .expect("request should include a valid content length")
            .expect("content length should be numeric");
        while request.len() - header_end < content_length {
            let read = stream.read(&mut buffer).await.expect("body should read");
            assert!(read > 0, "request closed before its body completed");
            request.extend_from_slice(&buffer[..read]);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&request[header_end..header_end + content_length])
                .expect("request body should be JSON");
        assert_eq!(
            body,
            serde_json::json!({
                "model_ref": {
                    "connection_id": "gateway-team",
                    "model_id": "deepseek-v4-flash"
                }
            })
        );

        let response_body = serde_json::json!({
            "default_model": {
                "connection_id": "gateway-team",
                "model_id": "deepseek-v4-flash"
            },
            "inventory": {
                "config_mode": "v2",
                "default_model": {
                    "connection_id": "gateway-team",
                    "model_id": "deepseek-v4-flash"
                },
                "connections": [],
                "issues": []
            },
            "save_warning": false
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
    });

    let client = DesktopHttpClient::new(
        Client::new(),
        address,
        Arc::new(DesktopBearerToken::generate().expect("token should generate")),
    );
    let result = client
        .save_provider_default_model(DesktopProviderDefaultModelSaveRequest {
            model_ref: crate::DesktopProviderModelRef {
                connection_id: "gateway-team".to_owned(),
                model_id: "deepseek-v4-flash".to_owned(),
            },
            context_window_tokens: None,
        })
        .await
        .expect("exact default route should save");

    assert_eq!(result.default_model.connection_id, "gateway-team");
    assert_eq!(result.default_model.model_id, "deepseek-v4-flash");
    assert!(!result.save_warning);
    server.await.expect("server task should complete");
}

#[tokio::test]
async fn approval_retry_reuses_the_exact_command_envelope_after_a_lost_response() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn read_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1_024];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.expect("request should read");
            assert!(read > 0, "request closed before its headers completed");
            request.extend_from_slice(&buffer[..read]);
            if let Some(offset) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end]).expect("headers should be UTF-8");
        assert!(
            headers.starts_with("POST /runs/run-1/approvals/call-1 HTTP/1.1\r\n"),
            "unexpected approval route: {headers}"
        );
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::parse::<usize>)
            })
            .expect("request should include content length")
            .expect("content length should be numeric");
        while request.len() - header_end < content_length {
            let read = stream.read(&mut buffer).await.expect("body should read");
            assert!(read > 0, "request closed before its body completed");
            request.extend_from_slice(&buffer[..read]);
        }
        serde_json::from_slice(&request[header_end..header_end + content_length])
            .expect("request body should be JSON")
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("loopback listener should expose its address");
    let server = tokio::spawn(async move {
        let (mut first, _) = listener
            .accept()
            .await
            .expect("first request should connect");
        let first_body = read_request(&mut first).await;
        drop(first);

        let (mut second, _) = listener.accept().await.expect("retry should connect");
        let second_body = read_request(&mut second).await;
        assert_eq!(
            second_body, first_body,
            "retry must reuse the exact envelope"
        );

        let command_id = first_body["command_id"]
            .as_str()
            .expect("command id should be present");
        let client_id = first_body["client_id"]
            .as_str()
            .expect("client id should be present");
        let response_body = serde_json::json!({
            "command_id": command_id,
            "client_id": client_id,
            "session_id": "session-1",
            "run_id": "run-1",
            "call_id": "call-1",
            "approval_request_id": "approval-request-1",
            "expected_stream_sequence": 7,
            "correlation_id": "event-8",
            "decision": {
                "run_id": "run-1",
                "call_id": "call-1",
                "decision": "approved",
                "family_pattern": "cargo test*",
                "reason": "approved in Desktop"
            },
            "route_state": "decision_accepted",
            "registry_revision": 8,
            "replayed": true
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        second
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
    });

    let client = DesktopHttpClient::new(
        Client::new(),
        address,
        Arc::new(DesktopBearerToken::generate().expect("token should generate")),
    );
    let receipt = client
        .resolve_approval(
            "session-1",
            "run-1",
            "call-1",
            7,
            DesktopApprovalDecisionRequest {
                approval_request_id: "approval-request-1".to_owned(),
                tool_call_hash: "a".repeat(64),
                policy_version: "permission-policy-v2".to_owned(),
                expires_at_ms: 10_000,
                decision: crate::DesktopApprovalDecision::Approve,
                family_pattern: None,
                reason: Some("approved in Desktop".to_owned()),
            },
        )
        .await
        .expect("lost response should be recovered by exact replay");

    assert!(receipt.replayed);
    assert_eq!(receipt.registry_revision, 8);
    assert_eq!(
        receipt.decision.family_pattern.as_deref(),
        Some("cargo test*")
    );
    server.await.expect("server task should complete");
}

#[tokio::test]
async fn user_input_detail_requires_the_exact_request_hash_etag() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("loopback listener should expose its address");
    let expected_request_hash = format!("sha256:{}", "b".repeat(64));
    let response_request_hash = expected_request_hash.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1_024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("request should read");
            assert!(read > 0, "request closed before headers completed");
            request.extend_from_slice(&buffer[..read]);
        }
        let headers = String::from_utf8(request).expect("request should be UTF-8");
        assert!(
            headers.starts_with("GET /sessions/session-1/user-input/request-1?generation=1&expected_request_hash=sha256%3A"),
            "unexpected user-input detail route: {headers}"
        );
        let response_body = serde_json::json!({
            "identity": {
                "session_scope_id": "session-1",
                "root_logical_run_id": "root-run-1",
                "source_thread_id": "main",
                "request_id": "request-1",
                "generation": 1,
                "source_binding_hash": format!("sha256:{}", "a".repeat(64))
            },
            "request_hash": response_request_hash,
            "source": "agent",
            "purpose": "clarification",
            "prompt": "Which workspace should I inspect?",
            "questions": [{
                "id": "workspace",
                "header": "Workspace",
                "question": "Which workspace should I inspect?",
                "required": true,
                "field": {"kind": "text", "multiline": false, "max_chars": 512}
            }],
            "allowed_actions": ["submit", "decline", "cancel_run"],
            "requested_at_unix_ms": 100,
            "status": "requested"
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"{response_request_hash}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
    });

    let client = DesktopHttpClient::new(
        Client::new(),
        address,
        Arc::new(DesktopBearerToken::generate().expect("token should generate")),
    );
    let request = client
        .user_input_request("session-1", "request-1", 1, &expected_request_hash)
        .await
        .expect("exact ETag-bound request should decode");

    assert_eq!(request.request_hash, expected_request_hash);
    server.await.expect("server task should complete");
}

#[tokio::test]
async fn user_input_retry_reuses_the_exact_command_envelope_after_a_lost_response() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn read_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1_024];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.expect("request should read");
            assert!(read > 0, "request closed before its headers completed");
            request.extend_from_slice(&buffer[..read]);
            if let Some(offset) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end]).expect("headers should be UTF-8");
        assert!(
            headers
                .starts_with("POST /sessions/session-1/user-input/request-1/decision HTTP/1.1\r\n"),
            "unexpected user-input route: {headers}"
        );
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::parse::<usize>)
            })
            .expect("request should include content length")
            .expect("content length should be numeric");
        while request.len() - header_end < content_length {
            let read = stream.read(&mut buffer).await.expect("body should read");
            assert!(read > 0, "request closed before its body completed");
            request.extend_from_slice(&buffer[..read]);
        }
        serde_json::from_slice(&request[header_end..header_end + content_length])
            .expect("request body should be JSON")
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("loopback listener should expose its address");
    let expected_request_hash = format!("sha256:{}", "b".repeat(64));
    let response_request_hash = expected_request_hash.clone();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener
            .accept()
            .await
            .expect("first request should connect");
        let first_body = read_request(&mut first).await;
        drop(first);

        let (mut second, _) = listener.accept().await.expect("retry should connect");
        let second_body = read_request(&mut second).await;
        assert_eq!(
            second_body, first_body,
            "retry must reuse the exact answer command envelope"
        );

        let command_id = first_body["command_id"]
            .as_str()
            .expect("command id should be present");
        let client_id = first_body["client_id"]
            .as_str()
            .expect("client id should be present");
        let response_body = serde_json::json!({
            "command_id": command_id,
            "client_id": client_id,
            "session_id": "session-1",
            "request": {
                "identity": {
                    "session_scope_id": "session-1",
                    "root_logical_run_id": "root-run-1",
                    "source_thread_id": "main",
                    "request_id": "request-1",
                    "generation": 1,
                    "source_binding_hash": format!("sha256:{}", "a".repeat(64))
                },
                "request_hash": response_request_hash,
                "source": "agent",
                "purpose": "clarification",
                "prompt": "Which workspace should I inspect?",
                "questions": [{
                    "id": "workspace",
                    "header": "Workspace",
                    "question": "Which workspace should I inspect?",
                    "required": true,
                    "field": {"kind": "text", "multiline": false, "max_chars": 512}
                }],
                "allowed_actions": ["submit", "decline", "cancel_run"],
                "requested_at_unix_ms": 100,
                "status": "decision_accepted",
                "answer_receipt": {
                    "command_id": command_id,
                    "decision": "submitted",
                    "answer_hash": format!("sha256:{}", "b".repeat(64)),
                    "answered_question_ids": ["workspace"]
                }
            },
            "continuation_run_id": "user-input-continuation-1",
            "replayed": true
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        second
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
    });

    let client = DesktopHttpClient::new(
        Client::new(),
        address,
        Arc::new(DesktopBearerToken::generate().expect("token should generate")),
    );
    let receipt = client
        .user_input_decision(
            "session-1",
            "request-1",
            1,
            &expected_request_hash,
            crate::DesktopUserInputDecision::Submitted {
                answers: vec![crate::DesktopUserInputAnswer {
                    question_id: "workspace".to_owned(),
                    value: crate::DesktopUserInputAnswerValue::Text {
                        value: "crates/sigil-kernel".to_owned(),
                    },
                }],
            },
            None,
        )
        .await
        .expect("lost response should be recovered by exact replay");

    assert!(receipt.replayed);
    assert_eq!(
        receipt.continuation_run_id.as_deref(),
        Some("user-input-continuation-1")
    );
    server.await.expect("server task should complete");
}

#[test]
fn approval_decision_contract_decodes_approve_for_family() {
    // The desktop typed client must decode a real server receipt for an approve_for_family
    // decision, including the persisted pattern.
    let record = serde_json::from_str::<crate::DesktopApprovalDecisionRecord>(
        r#"{
            "run_id": "run-1",
            "call_id": "call-1",
            "decision": "approved",
            "family_pattern": "cargo test*",
            "reason": "approved command family"
        }"#,
    )
    .expect("record should decode");
    assert_eq!(
        record.decision,
        crate::DesktopApprovalRecordedDecision::Approved
    );
    assert_eq!(record.family_pattern.as_deref(), Some("cargo test*"));
}

#[test]
fn session_management_contract_is_exact_and_path_free() {
    let rename = DesktopSessionRenameRequest {
        session_ref: "managed.jsonl".to_owned(),
        session_id: "durable-managed".to_owned(),
        display_name: "Readable name".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(rename).expect("rename should encode"),
        serde_json::json!({
            "session_ref": "managed.jsonl",
            "session_id": "durable-managed",
            "display_name": "Readable name"
        })
    );
    let receipt = serde_json::from_value::<DesktopSessionMutationReceipt>(serde_json::json!({
        "session_ref": "managed.jsonl",
        "session_id": "durable-managed",
        "operation_id": "session-display-name:1",
        "projection_generation": 2
    }))
    .expect("receipt should decode");
    assert_eq!(receipt.projection_generation, Some(2));
    assert!(!format!("{receipt:?}").contains('/'));

    let quarantine = DesktopSessionQuarantineRequest {
        session_ref: "broken.jsonl".to_owned(),
        source_bytes: 17,
        source_modified_at_unix_ms: 42,
    };
    assert_eq!(
        serde_json::to_value(quarantine).expect("quarantine should encode"),
        serde_json::json!({
            "session_ref": "broken.jsonl",
            "source_bytes": 17,
            "source_modified_at_unix_ms": 42
        })
    );
    let quarantine_receipt =
        serde_json::from_value::<DesktopSessionQuarantineReceipt>(serde_json::json!({
            "session_ref": "broken.jsonl",
            "operation_id": "session-quarantine:1",
            "quarantine_name": "1--broken.jsonl",
            "projection_generation": 3
        }))
        .expect("quarantine receipt should decode");
    assert_eq!(quarantine_receipt.projection_generation, Some(3));

    let delete_invalid = DesktopSessionInvalidSourceDeleteRequest {
        session_ref: "broken.jsonl".to_owned(),
        source_bytes: 17,
        source_modified_at_unix_ms: 42,
    };
    assert_eq!(
        serde_json::to_value(delete_invalid).expect("invalid source delete should encode"),
        serde_json::json!({
            "session_ref": "broken.jsonl",
            "source_bytes": 17,
            "source_modified_at_unix_ms": 42
        })
    );
    let delete_invalid_receipt =
        serde_json::from_value::<DesktopSessionInvalidSourceDeleteReceipt>(serde_json::json!({
            "session_ref": "broken.jsonl",
            "operation_id": "invalid-source-delete:1",
            "projection_generation": 4
        }))
        .expect("invalid source delete receipt should decode");
    assert_eq!(delete_invalid_receipt.projection_generation, Some(4));
}

#[tokio::test]
async fn transcript_query_rejects_unbounded_renderer_values_before_transport() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );

    assert!(matches!(
        client
            .transcript(
                "session-1",
                &DesktopTranscriptQuery {
                    before: None,
                    limit: Some(101),
                },
            )
            .await,
        Err(DesktopClientError::InvalidRoute)
    ));
    assert!(matches!(
        client
            .transcript(
                "session-1",
                &DesktopTranscriptQuery {
                    before: Some(0),
                    limit: Some(50),
                },
            )
            .await,
        Err(DesktopClientError::InvalidRoute)
    ));
}

#[tokio::test]
async fn conversation_display_query_rejects_unbounded_values_before_transport() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );

    for query in [
        DesktopConversationDisplayQuery {
            cursor: Some(String::new()),
            limit: Some(50),
        },
        DesktopConversationDisplayQuery {
            cursor: Some("bad\ncursor".to_owned()),
            limit: Some(50),
        },
        DesktopConversationDisplayQuery {
            cursor: Some("x".repeat(4_097)),
            limit: Some(50),
        },
        DesktopConversationDisplayQuery {
            cursor: None,
            limit: Some(0),
        },
        DesktopConversationDisplayQuery {
            cursor: None,
            limit: Some(101),
        },
    ] {
        assert!(matches!(
            client.conversation_display("session-1", &query).await,
            Err(DesktopClientError::InvalidRoute)
        ));
    }
}

#[tokio::test]
async fn tool_artifact_request_rejects_unbounded_values_before_transport() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );
    let artifact_ref = format!("ta1_{}", "a".repeat(32));
    for request in [
        crate::DesktopToolArtifactReadRequest {
            artifact_ref: "../private".to_owned(),
            selector: crate::DesktopToolArtifactSelector::ByteSlice {
                offset: 0,
                limit: 16,
            },
        },
        crate::DesktopToolArtifactReadRequest {
            artifact_ref: artifact_ref.clone(),
            selector: crate::DesktopToolArtifactSelector::ByteSlice {
                offset: DESKTOP_TOOL_ARTIFACT_MAX_COORDINATE + 1,
                limit: 16,
            },
        },
        crate::DesktopToolArtifactReadRequest {
            artifact_ref: artifact_ref.clone(),
            selector: crate::DesktopToolArtifactSelector::ByteSlice {
                offset: 0,
                limit: DESKTOP_TOOL_ARTIFACT_MAX_PAGE_BYTES + 1,
            },
        },
        crate::DesktopToolArtifactReadRequest {
            artifact_ref: artifact_ref.clone(),
            selector: crate::DesktopToolArtifactSelector::LinePage {
                start_line: 0,
                line_count: DESKTOP_TOOL_ARTIFACT_MAX_LINES + 1,
            },
        },
        crate::DesktopToolArtifactReadRequest {
            artifact_ref: artifact_ref.clone(),
            selector: crate::DesktopToolArtifactSelector::SearchLiteral {
                query: "x".repeat(DESKTOP_TOOL_ARTIFACT_MAX_QUERY_BYTES + 1),
                start_offset: 0,
                max_matches: 1,
                context_lines: 0,
            },
        },
    ] {
        assert!(matches!(
            client.tool_artifact_page("session-1", &request).await,
            Err(DesktopClientError::InvalidRoute)
        ));
    }
}

#[test]
fn sse_decoder_accepts_durable_and_transient_frames_and_rejects_gaps() {
    let durable = br#"id: sigil-http-run-v1:session-1:run-1:1
event: run_event
data: {"schema_version":2,"event_class":"durable","replay_id":"sigil-http-run-v1:session-1:run-1:1","run_event":{"schema_version":2,"session_id":"session-1","run_id":"run-1","sequence":1,"event":{"type":"run_started","prompt":"hello"}}}
"#;
    let decoded = decode_sse_frame(durable, "session-1", "run-1")
        .expect("frame should decode")
        .expect("frame should contain an event");
    assert_eq!(decoded.run_event.sequence, 1);

    let transient = br#"event: run_event
data: {"schema_version":2,"event_class":"transient","run_event":{"schema_version":2,"session_id":"session-1","run_id":"run-1","sequence":2,"event":{"type":"text_delta","text":"live"}}}
"#;
    let decoded = decode_sse_frame(transient, "session-1", "run-1")
        .expect("frame should decode")
        .expect("frame should contain an event");
    assert_eq!(decoded.event_class, DesktopProtocolEventClass::Transient);

    let gap = br#"event: stream_gap
data: {"dropped_live_events":1}
"#;
    assert!(matches!(
        decode_sse_frame(gap, "session-1", "run-1"),
        Err(DesktopClientError::EventStreamGap)
    ));
}

#[test]
fn sse_decoder_rejects_cursor_or_stream_mismatch() {
    let mismatched_cursor = br#"id: cursor-other
event: run_event
data: {"schema_version":2,"event_class":"durable","replay_id":"sigil-http-run-v1:session-1:run-1:1","run_event":{"schema_version":2,"session_id":"session-1","run_id":"run-1","sequence":1,"event":{"type":"run_started","prompt":"hello"}}}
"#;
    assert!(matches!(
        decode_sse_frame(mismatched_cursor, "session-1", "run-1"),
        Err(DesktopClientError::InvalidEventStream)
    ));

    let wrong_run = br#"event: run_event
data: {"schema_version":2,"event_class":"transient","run_event":{"schema_version":2,"session_id":"session-1","run_id":"run-other","sequence":2,"event":{"type":"text_delta","text":"live"}}}
"#;
    assert!(matches!(
        decode_sse_frame(wrong_run, "session-1", "run-1"),
        Err(DesktopClientError::ProtocolEvent(
            DesktopProtocolEventError::WrongStream
        ))
    ));
}
