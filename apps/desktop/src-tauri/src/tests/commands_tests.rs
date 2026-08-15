use super::*;
use sigil_desktop::{
    DesktopContinuityRecoveryAction, DesktopConversationDisplayAssistantPhase,
    DesktopConversationDisplayContent, DesktopConversationDisplayGapFact,
    DesktopConversationDisplayGapKind, DesktopConversationDisplayItem,
    DesktopConversationDisplayItemKind, DesktopConversationDisplayMessageRole,
    DesktopConversationDisplayOrder,
    DesktopConversationDisplayPage as NativeConversationDisplayPage,
    DesktopConversationDisplaySource, DesktopConversationDisplayStatus,
    DesktopConversationLiveProvisionalAnchor, DesktopConversationQueueBlockedReason,
    DesktopConversationQueueGeneration,
    DesktopConversationQueueItem as NativeConversationQueueItem, DesktopConversationQueueItemKind,
    DesktopConversationQueueItemStatus, DesktopConversationQueuePromptMaterial,
    DesktopConversationQueueView as NativeConversationQueueView, DesktopConversationTaskControl,
    DesktopConversationTaskLane, DesktopConversationTaskPlanStep,
    DesktopConversationTerminalFrontier, DesktopDurableSessionFrontier, DesktopForegroundRunOwner,
    DesktopPublicTaskPhase, DesktopSessionContinuityView, DesktopSessionSnapshot,
    DesktopSessionTranscriptMessage, DesktopSessionTranscriptPage,
    DesktopToolArtifactPage as NativeToolArtifactPage,
    DesktopToolArtifactPageEncoding as NativeToolArtifactPageEncoding,
    DesktopToolArtifactSelector as NativeToolArtifactSelector, DesktopTranscriptAssistantKind,
    DesktopTranscriptRole,
};

#[test]
fn workspace_identity_validation_is_strict_and_path_free() {
    assert!(valid_workspace_id("workspace-0123456789ab"));
    assert!(!valid_workspace_id("../workspace"));
    assert!(!valid_workspace_id("workspace/path"));
    assert!(!valid_workspace_id(""));
}

#[test]
fn bundled_runtime_name_cannot_collide_with_the_desktop_product_name() {
    let name = bundled_sigil_binary_name();
    assert!(name.starts_with("sigil-runtime"));
    assert_ne!(name.to_ascii_lowercase(), "sigil");
}

#[test]
fn debug_binary_resolution_prefers_the_current_developer_runtime() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let developer = temp
        .path()
        .join(if cfg!(windows) { "sigil.exe" } else { "sigil" });
    let bundled = temp.path().join(bundled_sigil_binary_name());
    std::fs::write(&developer, b"current").expect("developer runtime should create");
    std::fs::write(&bundled, b"stale").expect("bundled runtime should create");

    assert_eq!(
        resolve_sigil_binary_from_directory(temp.path(), true),
        Some(developer)
    );
    assert_eq!(
        resolve_sigil_binary_from_directory(temp.path(), false),
        Some(bundled)
    );
}

#[test]
fn incompatible_runtime_projection_is_actionable_and_path_free() {
    let projected = project_manager_error(DesktopWorkspaceManagerError::Launch(
        DesktopLaunchError::IncompatibleServer("schema version mismatch"),
    ));

    assert_eq!(projected.code, "workspace_server_incompatible");
    assert!(projected.message.contains("out of sync"));
    assert!(!projected.message.contains('/'));
    assert_eq!(
        projected.recovery_actions,
        [
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]
    );
}

#[test]
fn launch_failure_does_not_require_a_workspace_local_config() {
    let projected = project_manager_error(DesktopWorkspaceManagerError::Launch(
        DesktopLaunchError::InvalidRequest("configuration is not a file"),
    ));

    assert_eq!(projected.code, "workspace_server_start_failed");
    assert!(projected.message.contains("Sigil configuration"));
    assert!(!projected.message.contains("sigil.toml"));
}

#[test]
fn classified_startup_failures_expose_safe_actionable_causes() {
    let cases = [
        (
            DesktopStartupFailure::WorkspaceBusy,
            "workspace_server_busy",
            "Close the other process",
        ),
        (
            DesktopStartupFailure::AdapterStateInvalid,
            "workspace_server_state_invalid",
            "could not be repaired",
        ),
        (
            DesktopStartupFailure::LoopbackUnavailable,
            "workspace_server_loopback_unavailable",
            "loopback connection",
        ),
    ];

    for (failure, expected_code, expected_message) in cases {
        let projected = project_manager_error(DesktopWorkspaceManagerError::Launch(
            DesktopLaunchError::StartupRejected(failure),
        ));
        assert_eq!(projected.code, expected_code);
        assert!(projected.message.contains(expected_message));
        assert!(!projected.message.contains('/'));
        assert!(
            projected
                .recovery_actions
                .contains(&DesktopRecoveryAction::RetryCurrent)
        );
    }
}

#[test]
fn command_error_recovery_actions_are_bounded_deduplicated_and_camel_case() {
    let projected = DesktopCommandError::new("temporary", "Temporary failure")
        .with_recovery_actions([
            DesktopRecoveryAction::ShowDetails,
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::ShowDetails,
            DesktopRecoveryAction::OpenAnotherWorkspace,
        ]);

    let json = serde_json::to_value(projected).expect("command error should serialize");
    assert_eq!(
        json.get("recoveryActions"),
        Some(&serde_json::json!([
            "retry_current",
            "open_another_workspace",
            "show_details"
        ]))
    );
    assert!(json.get("recovery_actions").is_none());
}

#[test]
fn default_command_errors_do_not_invent_recovery_actions() {
    let json = serde_json::to_value(DesktopCommandError::new("fatal", "Fatal failure"))
        .expect("command error should serialize");

    assert!(json.get("recoveryActions").is_none());
}

#[test]
fn transient_and_terminal_workspace_errors_offer_only_safe_actions() {
    let transient = project_manager_error(DesktopWorkspaceManagerError::WorkspaceUnavailable);
    assert_eq!(
        transient.recovery_actions,
        [
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]
    );

    for terminal in [
        project_manager_error(DesktopWorkspaceManagerError::InvalidWorkspace),
        project_manager_error(DesktopWorkspaceManagerError::IdentityCollision),
        project_recent_error(RecentWorkspaceStoreError::UnknownWorkspace),
    ] {
        assert!(
            !terminal
                .recovery_actions
                .contains(&DesktopRecoveryAction::RetryCurrent)
        );
        assert!(
            terminal
                .recovery_actions
                .contains(&DesktopRecoveryAction::OpenAnotherWorkspace)
        );
        assert!(
            terminal
                .recovery_actions
                .contains(&DesktopRecoveryAction::ShowDetails)
        );
    }
}

#[test]
fn diagnostics_are_offered_only_after_a_workspace_client_route_exists() {
    let unavailable = project_manager_error(DesktopWorkspaceManagerError::WorkspaceUnavailable);
    assert!(
        !unavailable
            .recovery_actions
            .contains(&DesktopRecoveryAction::OpenDiagnostics)
    );

    let request = project_client_error(DesktopClientError::RequestFailed);
    assert!(
        request
            .recovery_actions
            .contains(&DesktopRecoveryAction::OpenDiagnostics)
    );
}

#[test]
fn session_management_validation_and_errors_are_actionable() {
    assert!(validate_display_name("Readable conversation").is_ok());
    assert_eq!(
        validate_display_name(" ")
            .expect_err("blank display name must fail")
            .code,
        "session_display_name_invalid"
    );
    let pinned = project_session_mutation_client_error(DesktopClientError::Rejected {
        status: 409,
        code: Some("durable_session_pinned".to_owned()),
        route_recovery: None,
    });
    assert_eq!(pinned.code, "session_pinned");
    assert!(pinned.message.contains("Unpin"));
}

#[test]
fn external_url_admission_accepts_only_bounded_credential_free_https() {
    assert_eq!(
        admit_external_https_url("https://example.com/docs?q=rust#install")
            .expect("HTTPS URL should be admitted"),
        "https://example.com/docs?q=rust#install"
    );
    for candidate in [
        "http://example.com",
        "file:///tmp/private",
        "javascript:alert(1)",
        "data:text/plain,secret",
        "https://user:password@example.com/private",
        "//example.com/path",
    ] {
        let error = admit_external_https_url(candidate)
            .expect_err("non-admitted URL must fail before the opener");
        assert_eq!(error.code, "external_url_invalid");
        assert!(!error.message.contains(candidate));
    }
    assert!(
        admit_external_https_url(&format!("https://example.com/{}", "x".repeat(2_048))).is_err()
    );
}

#[test]
fn workspace_display_name_never_returns_its_parent_path() {
    let name = workspace_display_name(Path::new("/private/canary/workspace"))
        .expect("basename should be accepted");
    assert_eq!(name, "workspace");
    assert!(!name.contains("canary"));
}

#[test]
fn projected_manager_errors_are_stable_and_path_free() {
    let projected = project_manager_error(DesktopWorkspaceManagerError::InvalidWorkspace);
    assert_eq!(projected.code, "workspace_invalid");
    assert!(!projected.message.contains('/'));
}

#[test]
fn catalog_query_validation_bounds_renderer_controlled_values() {
    let query = validate_catalog_request(DesktopCatalogRequest {
        limit: Some(30),
        query: Some("rust".to_owned()),
        state: Some(DesktopCatalogState::Ready),
        ..DesktopCatalogRequest::default()
    })
    .expect("bounded query should pass");
    assert_eq!(query.limit, Some(30));
    assert_eq!(query.query.as_deref(), Some("rust"));
    assert_eq!(query.state, Some(DesktopSessionCatalogState::Ready));

    assert!(
        validate_catalog_request(DesktopCatalogRequest {
            limit: Some(0),
            ..DesktopCatalogRequest::default()
        })
        .is_err()
    );
    assert!(
        validate_catalog_request(DesktopCatalogRequest {
            cursor: Some("x".repeat(4097)),
            ..DesktopCatalogRequest::default()
        })
        .is_err()
    );
}

#[test]
fn session_projection_drops_server_private_durable_fields() {
    let summary = DesktopSessionSummary::from(DesktopSessionSnapshot {
        id: "http-session-1".to_owned(),
        label: Some("Conversation".to_owned()),
        run_ids: vec!["run-1".to_owned()],
        durable_session_scope_id: "durable-secret-scope".to_owned(),
        foreground_run_id: None,
        route_transition: None,
        route_recovery: None,
    });
    let projection = serde_json::to_string(&summary).expect("summary should serialize");
    assert!(!projection.contains("durable-secret-scope"));
    assert_eq!(summary.run_count, 1);
}

#[test]
fn continuity_projection_drops_private_scope_and_preserves_exact_owner_revision() {
    let owner_revision = format!("sha256:{}", "a".repeat(64));
    let projected = DesktopConversationContinuity::try_from(DesktopSessionContinuityView {
        durable_session_scope_id: "durable-secret-scope".to_owned(),
        durable_frontier: DesktopDurableSessionFrontier {
            through_stream_sequence: 42,
        },
        foreground_owner: Some(DesktopForegroundRunOwner {
            run_id: "http-run-1".to_owned(),
            owner_revision: owner_revision.clone(),
        }),
        retained_terminal_runs: Vec::new(),
        recovery_actions: vec![
            DesktopContinuityRecoveryAction::RetryCurrent,
            DesktopContinuityRecoveryAction::ContinueReadOnly,
        ],
    })
    .expect("continuity should project");
    let json = serde_json::to_value(projected).expect("continuity should serialize");

    assert_eq!(json["durableFrontier"]["throughStreamSequence"], 42);
    assert_eq!(json["foregroundOwner"]["runId"], "http-run-1");
    assert_eq!(json["foregroundOwner"]["ownerRevision"], owner_revision);
    assert_eq!(
        json["recoveryActions"],
        serde_json::json!(["retry_current", "continue_read_only"])
    );
    assert!(!json.to_string().contains("durable-secret-scope"));
}

#[test]
fn owner_revision_validation_requires_the_exact_opaque_format() {
    assert!(validate_owner_revision(&format!("sha256:{}", "a".repeat(64))).is_ok());

    for invalid in [
        "",
        "sha256:abcd",
        &format!("sha256:{}", "A".repeat(64)),
        &format!("sha256:{}", "g".repeat(64)),
        &format!("sha512:{}", "a".repeat(64)),
    ] {
        let error = validate_owner_revision(invalid).expect_err("invalid revision must fail");
        assert_eq!(error.code, "run_owner_revision_invalid");
    }
}

#[test]
fn transcript_projection_drops_scope_and_preserves_safe_pagination_fields() {
    let projected = crate::ipc::DesktopTranscriptPage::from(DesktopSessionTranscriptPage {
        session_scope_id: "durable-secret-scope".to_owned(),
        total_messages: 2,
        messages: vec![DesktopSessionTranscriptMessage {
            ordinal: 2,
            message_id: "message-2".to_owned(),
            role: DesktopTranscriptRole::Assistant,
            content: Some("done".to_owned()),
            assistant_kind: Some(DesktopTranscriptAssistantKind::FinalAnswer),
            tool_name: None,
            image_attachment_count: 0,
            truncated: false,
            original_content_bytes: 4,
        }],
        next_before: Some(2),
    });
    let json = serde_json::to_value(projected).expect("transcript should serialize");

    assert_eq!(json["totalMessages"], 2);
    assert_eq!(json["messages"][0]["assistantKind"], "final_answer");
    assert_eq!(json["nextBefore"], 2);
    assert!(!json.to_string().contains("durable-secret-scope"));
}

#[test]
fn conversation_display_projection_preserves_decimal_text_and_drops_private_identity() {
    let projected =
        crate::ipc::DesktopConversationDisplayPage::from(NativeConversationDisplayPage {
            schema_version: 1,
            request_scope: "http-session-safe".to_owned(),
            through_session_stream_sequence: "9007199254740993".to_owned(),
            terminal_frontier: Some(DesktopConversationTerminalFrontier {
                run_id: "run-1".to_owned(),
                session_stream_sequence: "9007199254740994".to_owned(),
                status: DesktopConversationDisplayStatus::Succeeded,
            }),
            total_items: "9007199254740995".to_owned(),
            items: vec![DesktopConversationDisplayItem {
                schema_version: 1,
                display_id: "display-1".to_owned(),
                display_order: DesktopConversationDisplayOrder {
                    session_stream_sequence: "9007199254740993".to_owned(),
                    subindex: 0,
                },
                source_event_id: "event-1".to_owned(),
                kind: DesktopConversationDisplayItemKind::AssistantMessage,
                source: DesktopConversationDisplaySource::DurableTranscript,
                run_id: Some("run-1".to_owned()),
                run_sequence: Some("9007199254740996".to_owned()),
                status: DesktopConversationDisplayStatus::Completed,
                content: DesktopConversationDisplayContent::Message {
                    role: DesktopConversationDisplayMessageRole::Assistant,
                    text: Some("done".to_owned()),
                    skill: None,
                    assistant_phase: Some(DesktopConversationDisplayAssistantPhase::FinalAnswer),
                    image_attachment_count: 0,
                    truncated: false,
                    original_content_bytes: 4,
                },
                reconciles: Some(vec!["live-1".to_owned()]),
            }],
            next_cursor: Some("opaque_CURSOR-1".to_owned()),
            has_more: true,
            gap_facts: vec![DesktopConversationDisplayGapFact {
                kind: DesktopConversationDisplayGapKind::Retention,
                after_session_stream_sequence: "9007199254740997".to_owned(),
            }],
            live_provisional_anchor: Some(DesktopConversationLiveProvisionalAnchor {
                durable_frontier: "9007199254740993".to_owned(),
                run_id: "run-live".to_owned(),
                run_sequence: "9007199254740998".to_owned(),
            }),
            task_control: Some(DesktopConversationTaskControl {
                schema_version: 1,
                task_id: "task-restart".to_owned(),
                phase: DesktopPublicTaskPhase::Execution,
                status: "paused".to_owned(),
                plan_version: Some(2),
                plan_status: Some("accepted".to_owned()),
                steps: vec![DesktopConversationTaskPlanStep {
                    step_id: "step-safe".to_owned(),
                    title: "Resume the bounded task".to_owned(),
                    role: "executor".to_owned(),
                    depends_on: Vec::new(),
                    mode: "write".to_owned(),
                    isolation: "worktree".to_owned(),
                    status: Some("interrupted".to_owned()),
                }],
                steps_truncated: false,
                active_children: 0,
                completed_children: 1,
                failed_children: 0,
                lanes: vec![DesktopConversationTaskLane {
                    lane_id: "lane-safe".to_owned(),
                    plan_id: Some("plan-safe".to_owned()),
                    status: "ready".to_owned(),
                    conflicts: Vec::new(),
                }],
                lanes_truncated: false,
                can_continue: true,
            }),
            plan_review: None,
            user_inputs: Vec::new(),
            user_input: None,
        });
    let json = serde_json::to_value(projected).expect("display page should serialize");

    assert_eq!(
        json["throughSessionStreamSequence"],
        serde_json::json!("9007199254740993")
    );
    assert_eq!(
        json["terminalFrontier"]["sessionStreamSequence"],
        serde_json::json!("9007199254740994")
    );
    assert_eq!(json["totalItems"], serde_json::json!("9007199254740995"));
    assert_eq!(
        json["items"][0]["displayOrder"]["sessionStreamSequence"],
        serde_json::json!("9007199254740993")
    );
    assert_eq!(
        json["items"][0]["runSequence"],
        serde_json::json!("9007199254740996")
    );
    assert_eq!(json["items"][0]["content"]["type"], "message");
    assert_eq!(
        json["items"][0]["content"]["assistantPhase"],
        "final_answer"
    );
    assert_eq!(json["items"][0]["reconciles"][0], "live-1");
    assert_eq!(json["nextCursor"], "opaque_CURSOR-1");
    assert_eq!(json["taskControl"]["taskId"], "task-restart");
    assert_eq!(json["taskControl"]["phase"], "execution");
    assert_eq!(json["taskControl"]["steps"][0]["status"], "interrupted");
    assert!(json.get("durableSessionScopeId").is_none());
    assert!(json.get("sessionLogPath").is_none());
    assert!(json.get("bearer").is_none());
    assert!(json.get("checksum").is_none());
    assert!(json["items"][0]["content"].get("assistant_phase").is_none());
}

#[test]
fn conversation_display_query_validation_bounds_renderer_values() {
    assert!(
        validate_conversation_display_request(&DesktopConversationDisplayRequest {
            cursor: Some("opaque_CURSOR-1".to_owned()),
            limit: Some(100),
        })
        .is_ok()
    );

    for request in [
        DesktopConversationDisplayRequest {
            cursor: Some(String::new()),
            limit: Some(50),
        },
        DesktopConversationDisplayRequest {
            cursor: Some("bad\ncursor".to_owned()),
            limit: Some(50),
        },
        DesktopConversationDisplayRequest {
            cursor: Some("x".repeat(4_097)),
            limit: Some(50),
        },
        DesktopConversationDisplayRequest {
            cursor: None,
            limit: Some(0),
        },
        DesktopConversationDisplayRequest {
            cursor: None,
            limit: Some(101),
        },
    ] {
        let error = validate_conversation_display_request(&request)
            .expect_err("unbounded display request must fail");
        assert_eq!(error.code, "conversation_display_query_invalid");
    }
}

#[test]
fn tool_artifact_projection_is_bounded_and_path_free() {
    let projected = crate::ipc::DesktopToolArtifactPage::from(NativeToolArtifactPage {
        schema_version: 1,
        request_scope: "http-session-safe".to_owned(),
        artifact_ref: format!("ta1_{}", "a".repeat(32)),
        selector: NativeToolArtifactSelector::LinePage {
            start_line: 120,
            line_count: 80,
        },
        body: "bounded page\n".to_owned(),
        body_encoding: NativeToolArtifactPageEncoding::Utf8,
        returned_bytes: 13,
        page_sha256: format!("sha256:{}", "b".repeat(64)),
        artifact_sha256: format!("sha256:{}", "c".repeat(64)),
        eof: false,
        match_count: 0,
        next_selector: Some(NativeToolArtifactSelector::LinePage {
            start_line: 200,
            line_count: 80,
        }),
    });
    let json = serde_json::to_value(projected).expect("artifact page should serialize");

    assert_eq!(json["artifactRef"], format!("ta1_{}", "a".repeat(32)));
    assert_eq!(json["selector"]["kind"], "line_page");
    assert_eq!(json["selector"]["startLine"], 120);
    assert_eq!(json["bodyEncoding"], "utf8");
    assert_eq!(json["nextSelector"]["lineCount"], 80);
    assert!(json.get("sessionLogPath").is_none());
    assert!(json.get("artifactPath").is_none());
}

#[test]
fn tool_artifact_input_validation_enforces_opaque_ref_and_selector_caps() {
    let valid = DesktopToolArtifactReadInput {
        artifact_ref: format!("ta1_{}", "a".repeat(32)),
        selector: DesktopToolArtifactSelector::SearchLiteral {
            query: "needle".to_owned(),
            start_offset: 0,
            max_matches: 20,
            context_lines: 3,
        },
    };
    validate_tool_artifact_read_input(&valid).expect("bounded selector should validate");

    for invalid in [
        DesktopToolArtifactReadInput {
            artifact_ref: "/private/artifact".to_owned(),
            selector: DesktopToolArtifactSelector::ByteSlice {
                offset: 0,
                limit: 16,
            },
        },
        DesktopToolArtifactReadInput {
            artifact_ref: format!("ta1_{}", "a".repeat(32)),
            selector: DesktopToolArtifactSelector::ByteSlice {
                offset: 16_777_217,
                limit: 16,
            },
        },
        DesktopToolArtifactReadInput {
            artifact_ref: format!("ta1_{}", "a".repeat(32)),
            selector: DesktopToolArtifactSelector::ByteSlice {
                offset: 0,
                limit: 16_385,
            },
        },
        DesktopToolArtifactReadInput {
            artifact_ref: format!("ta1_{}", "a".repeat(32)),
            selector: DesktopToolArtifactSelector::SearchLiteral {
                query: String::new(),
                start_offset: 0,
                max_matches: 1,
                context_lines: 0,
            },
        },
    ] {
        let error =
            validate_tool_artifact_read_input(&invalid).expect_err("invalid input must fail");
        assert_eq!(error.code, "tool_artifact_request_invalid");
    }
}

#[test]
fn tool_artifact_errors_are_specific_and_path_free() {
    let unavailable = project_tool_artifact_client_error(DesktopClientError::Rejected {
        status: 404,
        code: Some("tool_artifact_unavailable".to_owned()),
        route_recovery: None,
    });
    assert_eq!(unavailable.code, "tool_artifact_unavailable");
    assert!(!unavailable.message.contains('/'));

    let corrupt = project_tool_artifact_client_error(DesktopClientError::Rejected {
        status: 409,
        code: Some("tool_artifact_corrupt".to_owned()),
        route_recovery: None,
    });
    assert_eq!(corrupt.code, "tool_artifact_corrupt");
    assert!(
        corrupt
            .recovery_actions
            .contains(&DesktopRecoveryAction::OpenDiagnostics)
    );
    assert!(!corrupt.message.contains('/'));
}

#[test]
fn conversation_display_errors_are_distinct_from_catalog_pagination() {
    let catalog = project_client_error(DesktopClientError::Rejected {
        status: 409,
        code: Some("stale_cursor".to_owned()),
        route_recovery: None,
    });
    assert_eq!(catalog.code, "catalog_stale");

    let display = project_conversation_display_client_error(DesktopClientError::Rejected {
        status: 409,
        code: Some("display_cursor_stale".to_owned()),
        route_recovery: None,
    });
    assert_eq!(display.code, "conversation_display_stale");
    assert_ne!(display.code, catalog.code);

    let invalid = project_conversation_display_client_error(DesktopClientError::Rejected {
        status: 400,
        code: Some("invalid_display_cursor".to_owned()),
        route_recovery: None,
    });
    assert_eq!(invalid.code, "conversation_display_cursor_invalid");

    let unavailable = project_conversation_display_client_error(DesktopClientError::Rejected {
        status: 503,
        code: Some("conversation_display_unavailable".to_owned()),
        route_recovery: None,
    });
    assert_eq!(unavailable.code, "conversation_display_unavailable");
    assert!(
        unavailable
            .recovery_actions
            .contains(&DesktopRecoveryAction::OpenDiagnostics)
    );
}

#[test]
fn conversation_queue_projection_is_bounded_camel_case_and_secret_free() {
    let projected = crate::ipc::DesktopConversationQueueView::from(NativeConversationQueueView {
        schema_version: 1,
        session_id: "session-queue".to_owned(),
        generation: DesktopConversationQueueGeneration("queue-v1:8:event-8".to_owned()),
        paused: false,
        total_items: 1,
        items: vec![NativeConversationQueueItem {
            entry_id: "queue-entry-1".to_owned(),
            order: 0,
            kind: DesktopConversationQueueItemKind::Chat,
            status: DesktopConversationQueueItemStatus::Queued,
            prompt_preview: "Prompt must be re-entered".to_owned(),
            prompt_preview_truncated: true,
            prompt_material: DesktopConversationQueuePromptMaterial::RequiresReentry,
            dispatchable: false,
            blocked_reason: Some(DesktopConversationQueueBlockedReason::RequiresReentry),
            created_at_ms: Some(1_784_419_200_000),
            updated_at_ms: None,
        }],
        truncated: false,
        next_dispatchable_entry_id: None,
    });

    let json = serde_json::to_value(projected).expect("queue view should serialize");
    let encoded = serde_json::to_string(&json).expect("queue view should encode");
    assert_eq!(json["generation"], "queue-v1:8:event-8");
    assert_eq!(json["items"][0]["promptMaterial"], "requires_reentry");
    assert_eq!(json["items"][0]["blockedReason"], "requires_reentry");
    assert!(json["items"][0].get("prompt_material").is_none());
    for forbidden in ["exactPrompt", "promptHash", "sessionLogPath", "bearer"] {
        assert!(!encoded.contains(forbidden));
    }
}

#[test]
fn conversation_queue_action_validation_rejects_lossy_or_stale_shapes() {
    let valid: crate::ipc::DesktopConversationQueueCommandInput =
        serde_json::from_value(serde_json::json!({
            "sessionId": "session-queue",
            "expectedGeneration": "queue-v1:8:event-8",
            "action": {
                "action": "enqueue",
                "prompt": "Run focused tests",
                "kind": "chat",
                "reasoningEffort": "high"
            }
        }))
        .expect("bounded queue input should decode");
    validate_queue_generation(&valid.expected_generation)
        .expect("bounded generation should validate");
    validate_queue_action(&valid.action).expect("chat enqueue should validate");

    let unknown_kind: crate::ipc::DesktopConversationQueueCommandInput =
        serde_json::from_value(serde_json::json!({
            "sessionId": "session-queue",
            "expectedGeneration": "queue-v1:8:event-8",
            "action": {
                "action": "enqueue",
                "prompt": "Run focused tests",
                "kind": "unknown"
            }
        }))
        .expect("unknown input kind should decode before validation");
    assert_eq!(
        validate_queue_action(&unknown_kind.action)
            .expect_err("unknown queue kind must fail")
            .code,
        "conversation_queue_action_invalid"
    );

    let self_reorder: crate::ipc::DesktopConversationQueueCommandInput =
        serde_json::from_value(serde_json::json!({
            "sessionId": "session-queue",
            "expectedGeneration": "queue-v1:8:event-8",
            "action": {
                "action": "reorder",
                "entryId": "queue-entry-1",
                "afterEntryId": "queue-entry-1"
            }
        }))
        .expect("reorder shape should decode before validation");
    assert_eq!(
        validate_queue_action(&self_reorder.action)
            .expect_err("self reorder must fail")
            .code,
        "conversation_queue_action_invalid"
    );

    let unknown_field = serde_json::from_value::<crate::ipc::DesktopConversationQueueCommandInput>(
        serde_json::json!({
            "sessionId": "session-queue",
            "expectedGeneration": "queue-v1:8:event-8",
            "unexpected": "private",
            "action": { "action": "pause" }
        }),
    );
    assert!(unknown_field.is_err());
}

#[test]
fn session_open_reference_rejects_path_shaped_input() {
    assert!(validate_session_reference("session.jsonl", "session-1").is_ok());
    assert!(validate_session_reference("../session.jsonl", "session-1").is_err());
    assert!(validate_session_reference("nested/session.jsonl", "session-1").is_err());
    assert!(validate_session_reference("nested\\session.jsonl", "session-1").is_err());
}

#[test]
fn verification_rerun_requires_one_bounded_exact_binding() {
    let valid = DesktopVerificationRerunInput {
        session_id: "http-session-1".to_owned(),
        request: crate::ipc::DesktopVerificationRerunBinding {
            request_id: format!("verification-rerun-{}", "a".repeat(64)),
            task_id: "task_1".to_owned(),
            plan_version: 1,
            step_id: "verify_1".to_owned(),
            check_spec_id: "cargo-test".to_owned(),
            check_spec_hash: "check-hash".to_owned(),
            policy_hash: "policy-hash".to_owned(),
            workspace_snapshot_id: "snapshot-1".to_owned(),
        },
    };
    assert!(validate_verification_rerun(&valid).is_ok());

    let mut invalid = valid;
    invalid.request.request_id = "verification-rerun-not-a-digest".to_owned();
    assert!(validate_verification_rerun(&invalid).is_err());

    invalid.request.request_id = format!("verification-rerun-{}", "a".repeat(64));
    invalid.request.plan_version = 0;
    assert!(validate_verification_rerun(&invalid).is_err());

    invalid.request.plan_version = 1;
    invalid.request.policy_hash.clear();
    assert!(validate_verification_rerun(&invalid).is_err());
}

#[test]
fn task_continuation_requires_exact_task_and_optional_nonempty_guidance() {
    let valid = crate::ipc::DesktopTaskContinuationInput {
        session_id: "http-session-1".to_owned(),
        task_id: "task_1".to_owned(),
        guidance: Some("Keep the public API compatible.".to_owned()),
        permission_mode: sigil_desktop::DesktopPermissionMode::Manual,
    };
    assert!(validate_task_continuation(&valid).is_ok());

    let empty_guidance = crate::ipc::DesktopTaskContinuationInput {
        guidance: Some("   ".to_owned()),
        ..valid
    };
    assert_eq!(
        validate_task_continuation(&empty_guidance)
            .expect_err("empty guidance must fail")
            .code,
        "task_continuation_invalid"
    );

    let unknown_field =
        serde_json::from_value::<crate::ipc::DesktopTaskContinuationInput>(serde_json::json!({
            "sessionId": "http-session-1",
            "taskId": "task_1",
            "permissionMode": "manual",
            "prompt": "must not become a chat turn"
        }));
    assert!(unknown_field.is_err());
}

#[test]
fn task_integration_acceptance_echoes_one_bounded_exact_binding() {
    let valid = crate::ipc::DesktopTaskIntegrationAcceptInput {
        session_id: "http-session-1".to_owned(),
        request: crate::ipc::DesktopTaskIntegrationReviewBinding {
            request_id: format!("integration-review-{}", "a".repeat(64)),
            task_id: "task_1".to_owned(),
            plan_id: "integration_plan_1".to_owned(),
            plan_version: 3,
            preview_digest: format!("sha256:{}", "b".repeat(64)),
        },
    };
    assert!(validate_task_integration_acceptance(&valid).is_ok());

    let mut invalid = valid;
    invalid.request.plan_version = 0;
    assert_eq!(
        validate_task_integration_acceptance(&invalid)
            .expect_err("zero plan version must fail")
            .code,
        "task_integration_request_invalid"
    );

    invalid.request.plan_version = 3;
    invalid.request.preview_digest.clear();
    assert!(validate_task_integration_acceptance(&invalid).is_err());
}

#[test]
fn task_integration_ipc_projection_is_camel_case_and_private_ref_free() {
    let review = sigil_desktop::DesktopTaskIntegrationReviewView {
        schema_version: 1,
        request: sigil_desktop::DesktopTaskIntegrationReviewRequest {
            request_id: "integration-review-request".to_owned(),
            task_id: "task_1".to_owned(),
            plan_id: "plan_1".to_owned(),
            plan_version: 1,
            preview_digest: "sha256:preview".to_owned(),
        },
        aggregate_diff: "diff --git a/src/lib.rs b/src/lib.rs\n".to_owned(),
        aggregate_diff_digest: "sha256:diff".to_owned(),
        preview_digest: "sha256:preview".to_owned(),
        policy_digest: "sha256:policy".to_owned(),
        target_kind: sigil_desktop::DesktopIntegrationPromotionTargetKind::WorkspaceApply,
        lanes: vec![sigil_desktop::DesktopTaskIntegrationLaneView {
            lane_id: "lane_1".to_owned(),
            candidate_kind: sigil_desktop::DesktopIntegrationLaneCandidateKind::SnapshotWorkspace,
            proposal_count: 1,
            verification_receipt_count: 2,
        }],
        child_verification_receipt_count: 1,
        lane_verification_receipt_count: 2,
        conflict_reasons: vec!["path_overlap".to_owned()],
        verification_invalidation_count: 1,
        parent_verification_pending: true,
    };
    let projected = serde_json::to_value(crate::ipc::DesktopTaskIntegrationReviewSummary::from(
        review,
    ))
    .expect("integration review should project");
    assert_eq!(projected["request"]["taskId"], "task_1");
    assert_eq!(projected["targetKind"], "workspace_apply");
    assert_eq!(projected["lanes"][0]["candidateKind"], "snapshot_workspace");
    let encoded = projected.to_string();
    assert!(!encoded.contains("worktree"));
    assert!(!encoded.contains("managed_ref"));
    assert!(!encoded.contains("artifact_ref"));
}

#[test]
fn intent_stack_command_bindings_are_exact_and_fail_closed() {
    let valid_ref = crate::ipc::DesktopIntentVersionBinding {
        intent_id: "intent-core".to_owned(),
        version: 2,
    };
    validate_intent_version_binding(&valid_ref).expect("valid intent version should pass");
    assert!(
        validate_intent_version_binding(&crate::ipc::DesktopIntentVersionBinding {
            intent_id: "../intent".to_owned(),
            version: 2,
        })
        .is_err()
    );
    assert!(
        validate_intent_version_binding(&crate::ipc::DesktopIntentVersionBinding {
            intent_id: "intent-core".to_owned(),
            version: 0,
        })
        .is_err()
    );

    let request = crate::ipc::DesktopIntentDropBinding {
        operation_id: "operation-drop-core".to_owned(),
        stack_version: 7,
        preview_digest: format!("sha256:jcs-v1:{}", "b".repeat(64)),
    };
    validate_intent_drop_binding(&request).expect("exact binding should validate");
    let json = serde_json::to_value(&request).expect("binding should serialize");
    let object = json.as_object().expect("binding should be an object");
    assert_eq!(object.len(), 3);
    assert!(object.contains_key("operationId"));
    assert!(object.contains_key("stackVersion"));
    assert!(object.contains_key("previewDigest"));
    assert!(!object.contains_key("path"));
    assert!(!object.contains_key("authority"));
    assert!(!object.contains_key("policy"));

    assert!(
        validate_intent_drop_binding(&crate::ipc::DesktopIntentDropBinding {
            preview_digest: "sha256:jcs-v1:not-a-digest".to_owned(),
            ..request
        })
        .is_err()
    );
}

#[test]
fn intent_stack_ipc_projection_is_camel_case_and_host_private_free() {
    let projected = serde_json::to_value(crate::ipc::DesktopIntentStackSummary::from(
        sigil_desktop::DesktopIntentStackState::Available {
            schema_version: 1,
            stack: sigil_desktop::DesktopIntentStack {
                schema_version: 1,
                stack_id: "stack-main".to_owned(),
                stack_version: 7,
                authority_state: sigil_desktop::DesktopIntentAuthorityState::Active,
                plan_digest: format!("sha256:jcs-v1:{}", "a".repeat(64)),
                intents: Vec::new(),
                conflicts: Vec::new(),
            },
        },
    ))
    .expect("Intent Stack should project");

    assert_eq!(projected["schemaVersion"], 1);
    assert_eq!(projected["stack"]["stackVersion"], 7);
    assert_eq!(projected["stack"]["authorityState"], "active");
    let encoded = projected.to_string();
    assert!(!encoded.contains("workspaceRoot"));
    assert!(!encoded.contains("sessionPath"));
    assert!(!encoded.contains("bearer"));
    assert!(!encoded.contains("policyDigest"));
    assert!(!encoded.contains("approvalAuthority"));
}

#[test]
fn support_bundle_validation_and_private_write_are_bounded() {
    assert!(validate_support_bundle("sigil-support-123.json", "{\"schema_version\":1}").is_ok());
    assert!(validate_support_bundle("../support.json", "{}").is_err());
    assert!(validate_support_bundle("sigil-support-123.json", "not-json").is_err());
    assert!(
        validate_support_bundle(
            "sigil-support-123.json",
            &format!("\"{}\"", "x".repeat(MAX_SUPPORT_BUNDLE_BYTES)),
        )
        .is_err()
    );

    let temp = tempfile::tempdir().expect("temporary directory should open");
    let destination = temp.path().join("sigil-support-123.json");
    write_private_support_bundle(&destination, "{\"schema_version\":1}")
        .expect("private support bundle should write");
    assert_eq!(
        std::fs::read_to_string(destination).expect("support bundle should read"),
        "{\"schema_version\":1}"
    );
}

#[cfg(unix)]
#[test]
fn private_support_write_rejects_symbolic_link_targets() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary directory should open");
    let target = temp.path().join("target.json");
    std::fs::write(&target, "private").expect("target should write");
    let link = temp.path().join("sigil-support-123.json");
    symlink(&target, &link).expect("symlink should create");

    let error = write_private_support_bundle(&link, "{}").expect_err("symlink should fail");
    assert_eq!(error.code, "support_save_invalid");
    assert_eq!(
        std::fs::read_to_string(target).expect("target should remain readable"),
        "private"
    );
}

#[test]
fn direct_compaction_sequences_prepare_then_apply_without_user_review() {
    let prepared: NativeCompactionReview = serde_json::from_value(serde_json::json!({
        "preview_id": "preview-local",
        "folded_event_count": 4,
        "retained_event_count": 2,
        "admission": {
            "kind": "prepared",
            "standalone_tool_output_shrink_available": true
        }
    }))
    .expect("prepared review should decode");
    assert_eq!(
        direct_compaction_step(&prepared).expect("prepared review should be actionable"),
        DirectCompactionStep::Prepare("preview-local".to_owned())
    );

    let ready: NativeCompactionReview = serde_json::from_value(serde_json::json!({
        "preview_id": "preview-summary",
        "folded_event_count": 4,
        "retained_event_count": 2,
        "admission": {
            "kind": "ready",
            "economics": {
                "before_input_tokens": 200,
                "target_input_tokens": 100,
                "context_window_tokens": 1000,
                "output_tokens": 20,
                "safety_buffer_tokens": 10,
                "savings_tokens": 100,
                "savings_ratio_ppm": 500000,
                "minimum_savings_tokens": 10,
                "minimum_savings_ratio_ppm": 10000,
                "summary_cache_read_tokens": 80,
                "summary_uncached_input_tokens": 20,
                "summary_output_tokens": 8
            }
        }
    }))
    .expect("ready review should decode");
    assert_eq!(
        direct_compaction_step(&ready).expect("ready review should be actionable"),
        DirectCompactionStep::Apply("preview-summary".to_owned())
    );
}

#[test]
fn direct_compaction_handles_noop_and_unavailable_states_without_applying() {
    let nothing: NativeCompactionReview = serde_json::from_value(serde_json::json!({
        "folded_event_count": 0,
        "retained_event_count": 2,
        "admission": {
            "kind": "no_foldable_history",
            "durable_message_count": 2,
            "minimum_tail_turn_count": 2
        }
    }))
    .expect("no-op review should decode");
    assert_eq!(
        direct_compaction_step(&nothing).expect("no-op should remain successful"),
        DirectCompactionStep::NothingToCompact
    );

    let unavailable: NativeCompactionReview = serde_json::from_value(serde_json::json!({
        "folded_event_count": 0,
        "retained_event_count": 2,
        "admission": {
            "kind": "unavailable",
            "reason": "provider unavailable"
        }
    }))
    .expect("unavailable review should decode");
    assert_eq!(
        direct_compaction_step(&unavailable)
            .expect_err("unavailable compaction should fail")
            .code,
        "compaction_unavailable"
    );
}
