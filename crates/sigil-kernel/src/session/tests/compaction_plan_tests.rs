use std::collections::BTreeSet;

use anyhow::Result;

use super::*;
use crate::{ModelMessage, ToolCall};

fn records(session: &Session) -> Result<Vec<SessionStreamRecord>> {
    let store = session
        .durable_store()
        .expect("safe-fold fixture must be store-backed");
    JsonlSessionStore::read_event_records(store.path())
}

fn store_backed_session() -> Result<(tempfile::TempDir, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    Ok((
        temp,
        Session::new("deepseek", "deepseek-v4-flash").with_store(store),
    ))
}

fn append_complete_turn(session: &mut Session, label: &str, payload_bytes: usize) -> Result<()> {
    session.append_user_message(ModelMessage::user(format!(
        "user-{label}-{}",
        "u".repeat(payload_bytes)
    )))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some(format!("assistant-{label}-{}", "a".repeat(payload_bytes))),
        Vec::new(),
    ))?;
    Ok(())
}

fn small_adaptive_policy() -> AdaptiveTailPolicyV3 {
    AdaptiveTailPolicyV3 {
        tail_min_complete_turns: 2,
        tail_target_min_tokens: 100,
        tail_target_max_tokens: 1_000,
        tail_recent_turn_p95_multiplier_ppm: 2_000_000,
        tail_max_usable_context_ratio_ppm: 250_000,
        recent_turn_sample_limit: 20,
    }
}

fn requested_tool_approval(call_id: &str) -> ControlEntry {
    ControlEntry::ToolApproval(ToolApprovalEntry::test_fixture(
        ToolApprovalAuditAction::Requested,
        call_id,
        "shell",
    ))
}

fn append_suspended_user_input(
    session: &mut Session,
    suffix: &str,
) -> Result<(crate::UserInputRequestedV1, BTreeSet<String>)> {
    let user_message_id = format!("user-input-user-{suffix}");
    let assistant_message_id = format!("user-input-assistant-{suffix}");
    let tool_call_id = format!("user-input-call-{suffix}");
    let mut user = ModelMessage::user("ask before choosing the migration mode");
    user.id = user_message_id.clone();
    session.append_user_message(user)?;
    let mut assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: tool_call_id.clone(),
            name: crate::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    assistant.id = assistant_message_id.clone();
    session.append_assistant_message(assistant)?;
    let requested = crate::UserInputRequestedV1::new(crate::UserInputRequestV1 {
        schema_version: crate::USER_INPUT_SCHEMA_VERSION,
        identity: crate::UserInputIdentityV1 {
            session_scope_id: crate::SessionScopeId::new(session.session_scope_id())?,
            root_logical_run_id: crate::LogicalRunId::new(format!("compaction-root-{suffix}"))?,
            source_thread_id: crate::AgentThreadId::new("main")?,
            request_id: crate::UserInputRequestId::new(format!("compaction-request-{suffix}"))?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        source: crate::UserInputSourceV1::Agent,
        purpose: crate::UserInputPurposeV1::Clarification,
        prompt: "Choose one migration mode".to_owned(),
        questions: vec![crate::UserInputQuestionV1 {
            id: "mode".to_owned(),
            header: "Mode".to_owned(),
            question: "Which migration mode should be used?".to_owned(),
            description: None,
            required: true,
            field: crate::UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 128,
            },
        }],
        allowed_actions: vec![crate::UserInputActionV1::Submit],
        requested_at_unix_ms: 10,
        continuation: Some(crate::UserInputContinuationBindingV1 {
            assistant_message_id,
            tool_call_id,
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
    })?;
    session.append_user_input_lifecycle(vec![crate::UserInputLifecycleEntryV1::Requested(
        Box::new(requested.clone()),
    )])?;
    let event_ids = records(session)?
        .into_iter()
        .filter_map(|record| {
            let event_id = record.event_id().to_owned();
            match session_entry_from_stored_event(record.stored_event()) {
                Ok(Some(SessionLogEntry::User(message))) if message.id == user_message_id => {
                    Some(event_id)
                }
                Ok(Some(SessionLogEntry::Assistant(message)))
                    if message.id
                        == requested
                            .request
                            .continuation
                            .as_ref()
                            .expect("fixture has a continuation")
                            .assistant_message_id =>
                {
                    Some(event_id)
                }
                _ => None,
            }
        })
        .collect();
    Ok((requested, event_ids))
}

fn submitted_user_input_decision(
    requested: &crate::UserInputRequestedV1,
    suffix: &str,
) -> Result<crate::UserInputDecisionAcceptedV1> {
    crate::UserInputDecisionAcceptedV1::new(
        requested,
        crate::UserInputCommandId::new(format!("compaction-command-{suffix}"))?,
        crate::UserInputDecisionV1::Submitted {
            answers: vec![crate::UserInputAnswerV1 {
                question_id: "mode".to_owned(),
                value: crate::UserInputAnswerValueV1::Text {
                    value: "legacy".to_owned(),
                },
            }],
        },
        20,
    )
}
#[test]
fn adaptive_compaction_preview_is_read_only_and_reports_the_exact_fold_plan() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..3 {
        append_complete_turn(&mut session, &index.to_string(), 32)?;
    }
    let store = session
        .durable_store()
        .expect("safe-fold fixture must be store-backed");
    let before = std::fs::read(store.path())?;

    let preview = store
        .adaptive_compaction_preview(small_adaptive_policy(), u64::MAX / 4, None)?
        .expect("the earliest complete turn should be foldable");

    assert_eq!(preview.plan.folded_event_ids.len(), 2);
    assert_eq!(preview.plan.retained_event_ids.len(), 4);
    assert!(preview.plan.protected_events.is_empty());
    assert!(preview.active_compaction_id.is_none());
    assert_eq!(std::fs::read(store.path())?, before);
    Ok(())
}
#[test]
fn adaptive_tail_selects_complete_turns_by_p95_target() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..40 {
        append_complete_turn(&mut session, &index.to_string(), 128)?;
    }
    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &stream,
        AdaptiveTailPolicyV3::default(),
        64 * 1024,
        None,
    )?;
    let adaptive = &plan.adaptive_tail;

    assert_eq!(adaptive.policy.tail_min_complete_turns, 2);
    assert!(adaptive.recent_complete_turn_p95_tokens > 0);
    assert_eq!(
        adaptive.ordinary_target_tokens,
        DEFAULT_TAIL_TARGET_MIN_TOKENS
    );
    assert!(adaptive.retained_complete_turns >= 2);
    assert!(
        adaptive
            .retained_turns
            .iter()
            .all(|turn| { turn.state == TailTurnStateV3::Complete && turn.event_ids.len() == 2 })
    );
    assert!(plan.has_foldable_history());
    assert!(plan.validate_against(&stream).is_ok());
    Ok(())
}

#[test]
fn adaptive_tail_clamps_p95_to_usable_context_ratio_deterministically() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..5 {
        append_complete_turn(&mut session, &index.to_string(), 100)?;
    }
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records(&session)?,
        small_adaptive_policy(),
        1_000,
        None,
    )?;
    let adaptive = plan.adaptive_tail;

    assert_eq!(adaptive.ordinary_target_tokens, 250);
    assert_eq!(adaptive.retained_complete_turns, 2);
    assert!(adaptive.retained_token_upper_bound <= adaptive.exact_fit_limit_tokens);
    Ok(())
}

#[test]
fn adaptive_tail_keeps_a_tool_heavy_active_turn_atomic_and_extends_target() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..3 {
        append_complete_turn(&mut session, &index.to_string(), 32)?;
    }
    session.append_user_message(ModelMessage::user("active tool-heavy request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "active-call".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_test_tool_result(ToolResult::ok(
        "active-call",
        "shell",
        serde_json::json!({
            "status": "ok",
            "content": "x".repeat(4_000),
        })
        .to_string(),
        ToolResultMeta::default(),
    ))?;
    let stream = records(&session)?;
    let active_ids = stream
        .iter()
        .rev()
        .take(3)
        .map(|record| record.event_id().to_owned())
        .collect::<BTreeSet<_>>();
    let mut policy = small_adaptive_policy();
    policy.tail_target_min_tokens = 128;
    policy.tail_target_max_tokens = 128;
    let plan =
        CompactionFoldPlan::from_records_after_adaptive_tail(&stream, policy, 16 * 1024, None)?;
    let adaptive = &plan.adaptive_tail;
    let active_turn = adaptive
        .retained_turns
        .iter()
        .find(|turn| turn.state == TailTurnStateV3::Active)
        .expect("active turn remains raw");

    assert_eq!(
        active_turn
            .event_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        active_ids
    );
    assert!(adaptive.active_turn_extended);
    assert!(active_turn.token_upper_bound > adaptive.ordinary_target_tokens);
    assert!(
        active_ids
            .iter()
            .all(|id| plan.retained_event_ids.contains(id))
    );
    assert!(
        active_ids
            .iter()
            .all(|id| !plan.folded_event_ids.contains(id))
    );
    assert!(plan.has_foldable_history());
    Ok(())
}

#[test]
fn adaptive_tail_rejects_an_active_turn_beyond_the_exact_fit_limit() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    append_complete_turn(&mut session, "old-1", 8)?;
    append_complete_turn(&mut session, "old-2", 8)?;
    session.append_user_message(ModelMessage::user("x".repeat(2_000)))?;

    let error = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records(&session)?,
        small_adaptive_policy(),
        1_000,
        None,
    )
    .expect_err("oversized active turn must reject ordinary compaction");
    assert!(error.to_string().contains("exact-fit limit"));
    Ok(())
}

#[test]
fn adaptive_tail_never_splits_a_waiting_approval_from_its_active_turn() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..3 {
        append_complete_turn(&mut session, &index.to_string(), 16)?;
    }
    session.append_user_message(ModelMessage::user("approve the active call"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "approval-call".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_test_tool_result(ToolResult::ok(
        "approval-call",
        "shell",
        serde_json::json!({"status": "preview", "content": "pending"}).to_string(),
        ToolResultMeta::default(),
    ))?;
    session.append_control(requested_tool_approval("approval-call"))?;
    let stream = records(&session)?;
    let active_message_ids = stream
        .iter()
        .rev()
        .filter(|record| {
            matches!(
                session_entry_from_stored_event(record.stored_event()),
                Ok(Some(
                    SessionLogEntry::User(_)
                        | SessionLogEntry::Assistant(_)
                        | SessionLogEntry::ToolResultV3(_)
                ))
            )
        })
        .take(3)
        .map(|record| record.event_id().to_owned())
        .collect::<BTreeSet<_>>();

    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &stream,
        small_adaptive_policy(),
        8 * 1024,
        None,
    )?;
    let adaptive = &plan.adaptive_tail;

    assert!(active_message_ids.iter().all(|id| {
        plan.protected_events.iter().any(|entry| {
            entry.event.event_id == *id
                && entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval
        })
    }));
    assert!(
        active_message_ids
            .iter()
            .all(|id| !plan.folded_event_ids.contains(id))
    );
    assert!(
        adaptive
            .protected_tail_events
            .iter()
            .any(|entry| entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval)
    );
    Ok(())
}

#[test]
fn adaptive_tail_pins_a_suspended_user_input_tool_turn() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..3 {
        append_complete_turn(&mut session, &index.to_string(), 16)?;
    }
    session.append_user_message(ModelMessage::user("ask before choosing the migration mode"))?;
    let mut assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "user-input-call".to_owned(),
            name: crate::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    assistant.id = "user-input-assistant".to_owned();
    session.append_assistant_message(assistant)?;
    let identity = crate::UserInputIdentityV1 {
        session_scope_id: crate::SessionScopeId::new(session.session_scope_id())?,
        root_logical_run_id: crate::LogicalRunId::new("compaction-root")?,
        source_thread_id: crate::AgentThreadId::new("main")?,
        request_id: crate::UserInputRequestId::new("compaction-request")?,
        generation: 1,
        source_binding_hash: format!("sha256:{}", "a".repeat(64)),
    };
    let request = crate::UserInputRequestedV1::new(crate::UserInputRequestV1 {
        schema_version: crate::USER_INPUT_SCHEMA_VERSION,
        identity,
        source: crate::UserInputSourceV1::Agent,
        purpose: crate::UserInputPurposeV1::Clarification,
        prompt: "Choose one migration mode".to_owned(),
        questions: vec![crate::UserInputQuestionV1 {
            id: "mode".to_owned(),
            header: "Mode".to_owned(),
            question: "Which migration mode should be used?".to_owned(),
            description: None,
            required: true,
            field: crate::UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 128,
            },
        }],
        allowed_actions: vec![crate::UserInputActionV1::Submit],
        requested_at_unix_ms: 10,
        continuation: Some(crate::UserInputContinuationBindingV1 {
            assistant_message_id: "user-input-assistant".to_owned(),
            tool_call_id: "user-input-call".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
    })?;
    session.append_user_input_lifecycle(vec![crate::UserInputLifecycleEntryV1::Requested(
        Box::new(request),
    )])?;

    let stream = records(&session)?;
    let active_message_ids = stream
        .iter()
        .rev()
        .filter(|record| {
            matches!(
                session_entry_from_stored_event(record.stored_event()),
                Ok(Some(
                    SessionLogEntry::User(_) | SessionLogEntry::Assistant(_)
                ))
            )
        })
        .take(2)
        .map(|record| record.event_id().to_owned())
        .collect::<BTreeSet<_>>();
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &stream,
        small_adaptive_policy(),
        8 * 1024,
        None,
    )?;

    assert!(active_message_ids.iter().all(|id| {
        plan.protected_events.iter().any(|entry| {
            entry.event.event_id == *id
                && entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval
        })
    }));
    assert!(
        active_message_ids
            .iter()
            .all(|id| !plan.folded_event_ids.contains(id))
    );
    Ok(())
}

#[test]
fn adaptive_tail_pins_every_durable_user_input_crash_frontier() -> Result<()> {
    for history_turns in [0_usize, 2, 8] {
        for lifecycle_stage in 0_u8..=3 {
            let suffix = format!("{history_turns}-{lifecycle_stage}");
            let (_temp, mut session) = store_backed_session()?;
            for index in 0..history_turns {
                append_complete_turn(&mut session, &format!("history-{index}"), 16)?;
            }
            let (requested, frontier_event_ids) =
                append_suspended_user_input(&mut session, &suffix)?;
            let decision = submitted_user_input_decision(&requested, &suffix)?;
            let claim_id = crate::UserInputClaimId::new(format!("compaction-claim-{suffix}"))?;
            let mut lifecycle = Vec::new();
            if lifecycle_stage >= 1 {
                lifecycle.push(crate::UserInputLifecycleEntryV1::DecisionAccepted(
                    Box::new(decision),
                ));
            }
            if lifecycle_stage >= 2 {
                lifecycle.push(crate::UserInputLifecycleEntryV1::ContinuationClaimed(
                    crate::UserInputContinuationClaimedV1 {
                        schema_version: crate::USER_INPUT_SCHEMA_VERSION,
                        identity: requested.request.identity.clone(),
                        request_hash: requested.request_hash.clone(),
                        claim_id: claim_id.clone(),
                        supervisor_instance_id: format!("compaction-supervisor-{suffix}"),
                        claimed_at_unix_ms: 30,
                    },
                ));
            }
            if lifecycle_stage >= 3 {
                lifecycle.push(crate::UserInputLifecycleEntryV1::ContinuationStarted(
                    crate::UserInputContinuationStartedV1 {
                        schema_version: crate::USER_INPUT_SCHEMA_VERSION,
                        identity: requested.request.identity.clone(),
                        request_hash: requested.request_hash.clone(),
                        claim_id,
                        continuation_logical_run_id: crate::LogicalRunId::new(format!(
                            "compaction-continuation-{suffix}"
                        ))?,
                        physical_attempt_id: format!("compaction-attempt-{suffix}"),
                        started_at_unix_ms: 40,
                    },
                ));
            }
            if !lifecycle.is_empty() {
                session.append_user_input_lifecycle(lifecycle)?;
            }
            let stream = records(&session)?;
            assert_eq!(frontier_event_ids.len(), 2);
            for usable_context_tokens in [4_096_u64, 8_192, 65_536] {
                let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
                    &stream,
                    small_adaptive_policy(),
                    usable_context_tokens,
                    None,
                )?;
                assert!(frontier_event_ids.iter().all(|event_id| {
                    plan.protected_events.iter().any(|entry| {
                        entry.event.event_id == *event_id
                            && entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval
                    })
                }));
                assert!(
                    frontier_event_ids
                        .iter()
                        .all(|event_id| !plan.folded_event_ids.contains(event_id))
                );
            }
        }
    }
    Ok(())
}

#[test]
fn resolved_user_input_frontier_can_age_into_compaction() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..8 {
        append_complete_turn(&mut session, &format!("before-{index}"), 16)?;
    }
    let (requested, frontier_event_ids) = append_suspended_user_input(&mut session, "resolved")?;
    let decision = submitted_user_input_decision(&requested, "resolved")?;
    let claim_id = crate::UserInputClaimId::new("compaction-claim-resolved")?;
    session.append_user_input_lifecycle(vec![
        crate::UserInputLifecycleEntryV1::DecisionAccepted(Box::new(decision)),
        crate::UserInputLifecycleEntryV1::ContinuationClaimed(
            crate::UserInputContinuationClaimedV1 {
                schema_version: crate::USER_INPUT_SCHEMA_VERSION,
                identity: requested.request.identity.clone(),
                request_hash: requested.request_hash.clone(),
                claim_id: claim_id.clone(),
                supervisor_instance_id: "compaction-supervisor-resolved".to_owned(),
                claimed_at_unix_ms: 30,
            },
        ),
        crate::UserInputLifecycleEntryV1::ContinuationStarted(
            crate::UserInputContinuationStartedV1 {
                schema_version: crate::USER_INPUT_SCHEMA_VERSION,
                identity: requested.request.identity.clone(),
                request_hash: requested.request_hash.clone(),
                claim_id,
                continuation_logical_run_id: crate::LogicalRunId::new(
                    "compaction-continuation-resolved",
                )?,
                physical_attempt_id: "compaction-attempt-resolved".to_owned(),
                started_at_unix_ms: 40,
            },
        ),
    ])?;
    let binding = requested
        .request
        .continuation
        .as_ref()
        .expect("fixture has a continuation");
    session.append_test_tool_result(ToolResult::ok(
        binding.tool_call_id.clone(),
        crate::REQUEST_USER_INPUT_TOOL_NAME,
        r#"{"status":"submitted"}"#,
        ToolResultMeta::default(),
    ))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("The migration mode is now resolved.".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_input_lifecycle(vec![crate::UserInputLifecycleEntryV1::Resolved(
        crate::UserInputResolvedV1 {
            schema_version: crate::USER_INPUT_SCHEMA_VERSION,
            identity: requested.request.identity,
            request_hash: requested.request_hash,
            resolution: crate::UserInputResolutionV1::Consumed,
            resolved_at_unix_ms: 50,
        },
    )])?;
    for index in 0..20 {
        append_complete_turn(&mut session, &format!("after-{index}"), 16)?;
    }

    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records(&session)?,
        small_adaptive_policy(),
        8 * 1024,
        None,
    )?;
    assert_eq!(frontier_event_ids.len(), 2);
    assert!(frontier_event_ids.iter().all(|event_id| {
        !plan.protected_events.iter().any(|entry| {
            entry.event.event_id == *event_id
                && entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval
        })
    }));
    assert!(
        frontier_event_ids
            .iter()
            .all(|event_id| plan.folded_event_ids.contains(event_id))
    );
    Ok(())
}
