use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_STORAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_approval_identity(call_id: &str) -> sigil_kernel::ApprovalRequestIdentityV2 {
    sigil_kernel::ApprovalRequestIdentityV2 {
        session_id: "session-tui-test".to_owned(),
        run_id: "run-tui-test".to_owned(),
        call_id: call_id.to_owned(),
        approval_request_id: format!("approval-{call_id}"),
        plan_hash: "plan-tui-test".to_owned(),
        policy_version: "policy-tui-test".to_owned(),
        execution_binding_hash: "binding-tui-test".to_owned(),
        expires_at_ms: u64::MAX,
    }
}

pub(crate) fn test_config() -> RootConfig {
    let storage_id = TEST_STORAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let storage_root = std::env::temp_dir().join(format!(
        "sigil-tui-test-storage-{}-{storage_id}",
        std::process::id()
    ));
    let skills = sigil_kernel::SkillConfig {
        user_skills: false,
        user_agents: false,
        compatibility_sources: Vec::new(),
        ..Default::default()
    };

    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: sigil_kernel::StorageConfig {
            state_root: sigil_kernel::StorageRoot::Path(
                storage_root.join("state").display().to_string(),
            ),
            cache_root: sigil_kernel::StorageRoot::Path(
                storage_root.join("cache").display().to_string(),
            ),
            ..Default::default()
        },
        session: SessionConfig::default(),
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: Some(
                sigil_kernel::ConnectionId::new("deepseek-default").expect("test connection id"),
            ),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(true),
        skills,
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        connections: std::collections::BTreeMap::from([(
            "deepseek-default".to_owned(),
            serde_json::json!({
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

pub(crate) fn v2_test_config() -> RootConfig {
    test_config()
}

pub(crate) fn resolved_session_log_dir(config: &RootConfig, workspace_root: &Path) -> PathBuf {
    sigil_runtime::resolve_sigil_paths(&config.storage, &config.session, workspace_root)
        .session_log_dir
}

pub(crate) fn restored_entries(provider_name: &str, model_name: &str) -> Vec<SessionLogEntry> {
    vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: provider_name.to_owned(),
            model_name: model_name.to_owned(),
            resolved_model_route: None,
        }),
        SessionLogEntry::User(ModelMessage::user("restored user prompt")),
        v2_tool_result_entry(
            "call-1",
            "test_tool",
            "restored tool output",
            ToolResultMeta::default(),
        ),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("restored assistant answer".to_owned()),
            Vec::new(),
        )),
    ]
}

pub(crate) fn v2_tool_result_entry(
    call_id: &str,
    tool_name: &str,
    content: impl Into<String>,
    metadata: ToolResultMeta,
) -> SessionLogEntry {
    let result = ToolResult::ok(call_id, tool_name, content, metadata);
    v2_tool_result_from_result(&result)
}

pub(crate) fn v2_tool_result_from_result(result: &ToolResult) -> SessionLogEntry {
    let (recorded, _) = sigil_kernel::ToolResultRecordedV2::capture(
        result,
        None,
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )
    .expect("bounded TUI test tool result must project to V2");
    SessionLogEntry::ToolResultV2(recorded)
}

pub(crate) fn adaptive_test_compaction_preview(
    store: &JsonlSessionStore,
) -> Result<sigil_kernel::V2CompactionPreview> {
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("current response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("newest request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("newest response".to_owned()),
        Vec::new(),
    )))?;
    store
        .adaptive_compaction_preview(
            sigil_kernel::AdaptiveTailPolicyV3 {
                tail_min_complete_turns: 2,
                tail_target_min_tokens: 64,
                tail_target_max_tokens: 128,
                tail_recent_turn_p95_multiplier_ppm: 1_000_000,
                tail_max_usable_context_ratio_ppm: 250_000,
                recent_turn_sample_limit: 20,
            },
            8 * 1024,
            None,
        )?
        .ok_or_else(|| anyhow::anyhow!("test fixture should have foldable history"))
}

pub(crate) fn integration_review_entries(
    artifact_ref: impl Into<String>,
    aggregate_diff_digest: impl Into<String>,
) -> Result<Vec<SessionLogEntry>> {
    use sigil_kernel::{
        AgentRole, ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk,
        EvidenceReceipt, EvidenceScope, ExecutionBackendCapabilities, ExecutionBackendKind,
        ExecutionNetworkReceipt, IntegrationBaseRepresentation, IntegrationContentClass,
        IntegrationEffect, IntegrationLaneCandidate, IntegrationLaneCleanupRecorded,
        IntegrationLaneCleanupStatus, IntegrationLaneMemberApplied, IntegrationLaneMemberEffect,
        IntegrationLanePrepared, IntegrationLaneStatus, IntegrationLaneTarget,
        IntegrationLaneTerminal, IntegrationLaneVerificationLinked, IntegrationPlanId,
        IntegrationPlanRecorded, IntegrationProjection, IntegrationProposalFacts,
        IntegrationProposalSpec, ReceiptStatus, RedactionState, SessionRef, TaskId, TaskPlanEntry,
        TaskPlanStatus, TaskPromotionLaneCandidate, TaskPromotionPreviewInput,
        TaskPromotionPreviewRecorded, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId,
        TaskStepSpec, TaskStepStatus, VerificationBinding, VerificationPolicy, VerificationReceipt,
        build_integration_plan, build_task_promotion_preview,
    };

    let task_id = TaskId::new("task_integration_review")?;
    let step_id = TaskStepId::new("implement")?;
    let change_set = ChangeSet {
        id: ChangeSetId::new("changeset-integration-review")?,
        title: "integration review".to_owned(),
        summary: "review exact aggregate diff".to_owned(),
        risk: ChangeSetRisk::Medium,
        files: vec![ChangeSetFile {
            path: "src/lib.rs".to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Medium,
            before_hash: Some("before-integration-review".to_owned()),
            after_hash: Some("after-integration-review".to_owned()),
            diff_hash: None,
            additions: 1,
            deletions: 1,
            validations: Vec::new(),
        }],
        validations: Vec::new(),
    };
    let facts = IntegrationProposalFacts::from_changeset(
        &change_set,
        IntegrationBaseRepresentation::CleanCommit {
            base_commit: "b".repeat(40),
        },
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        "artifact-proposal-integration-review",
        Vec::new(),
    )?;
    let proposal = IntegrationProposalSpec::from_changeset(
        &change_set,
        step_id.clone(),
        "snapshot-base".to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        "scope-integration-review",
        facts,
    )?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-integration-review")?,
        task_id.clone(),
        1,
        vec![proposal],
    )?;
    let lane = &plan.lanes[0];
    let candidate = IntegrationLaneCandidate::ManagedRef {
        private_ref: format!(
            "refs/sigil/integration/{}/{}",
            plan.plan_id.as_str(),
            lane.lane_id.as_str()
        ),
        base_commit: "b".repeat(40),
        candidate_commit: "a".repeat(40),
        workspace_snapshot_id: "snapshot-lane-ready".to_owned(),
    };
    let receipt = VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id: "receipt-integration-review".to_owned(),
            source_session_id: "session-integration-review".to_owned(),
            source_event_id: "event-integration-review".to_owned(),
            source_event_type: "check_finished".to_owned(),
            scope: EvidenceScope::Task(task_id.as_str().to_owned()),
            producer_tool_call: None,
            workspace_revision: Some(0),
            workspace_snapshot_id: Some("snapshot-lane-ready".to_owned()),
            policy_hash: Some("policy-integration-review".to_owned()),
            changeset_id: None,
            status: ReceiptStatus::Succeeded,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence: 1,
        },
        binding: VerificationBinding {
            workspace_id: "workspace-integration-review".to_owned(),
            workspace_snapshot_id: "snapshot-lane-ready".to_owned(),
            verification_scope_hash: lane.verification_scope_hashes[0].clone(),
            check_spec_hash: "check-integration-review".to_owned(),
            environment_fingerprint: "environment-integration-review".to_owned(),
            sandbox_profile_hash: "sandbox-integration-review".to_owned(),
            execution_backend: Some(ExecutionBackendKind::Local),
            execution_backend_capabilities: Some(ExecutionBackendCapabilities::default()),
            execution_network: ExecutionNetworkReceipt::unknown("test local backend"),
            workspace_trust_snapshot_id: "trust-integration-review".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: "check-integration-review".to_owned(),
        check_status: ReceiptStatus::Succeeded,
        failure_reason: None,
        mutates_verification_scope: false,
    };
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "Integrate verified changes".to_owned(),
            status: TaskRunStatus::Paused,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Implement".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id,
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("Implement".to_owned()),
            summary: Some("verified candidate ready".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
            IntegrationPlanRecorded { plan: plan.clone() },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(
            IntegrationLanePrepared {
                plan_id: plan.plan_id.clone(),
                lane_id: lane.lane_id.clone(),
                target: IntegrationLaneTarget::ManagedRef {
                    base_commit: "b".repeat(40),
                    expected_oid: "0".repeat(40),
                    private_ref: format!(
                        "refs/sigil/integration/{}/{}",
                        plan.plan_id.as_str(),
                        lane.lane_id.as_str()
                    ),
                },
                owned_workspace_id: "workspace-lane-integration-review".to_owned(),
                ordered_members: lane.proposals.clone(),
                prepared_at_unix_ms: 1,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneMemberApplied(
            IntegrationLaneMemberApplied {
                plan_id: plan.plan_id.clone(),
                lane_id: lane.lane_id.clone(),
                change_set_id: lane.proposals[0].clone(),
                member_index: 0,
                effect: IntegrationLaneMemberEffect::ManagedRefAdvanced {
                    expected_old_oid: "0".repeat(40),
                    new_oid: "a".repeat(40),
                    candidate_snapshot_id: "snapshot-lane-ready".to_owned(),
                },
                applied_at_unix_ms: 2,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneVerificationLinked(
            IntegrationLaneVerificationLinked {
                plan_id: plan.plan_id.clone(),
                lane_id: lane.lane_id.clone(),
                candidate: candidate.clone(),
                verification_check_ids: vec![receipt.check_spec_id.clone()],
                verification_scope_hashes: lane.verification_scope_hashes.clone(),
                verification_receipts: vec![receipt],
                linked_at_unix_ms: 3,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneTerminal(
            IntegrationLaneTerminal {
                plan_id: plan.plan_id.clone(),
                lane_id: lane.lane_id.clone(),
                status: IntegrationLaneStatus::Ready,
                candidate: Some(candidate.clone()),
                reason: None,
                terminal_at_unix_ms: 4,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneCleanupRecorded(
            IntegrationLaneCleanupRecorded {
                plan_id: plan.plan_id.clone(),
                lane_id: lane.lane_id.clone(),
                owned_workspace_id: "workspace-lane-integration-review".to_owned(),
                status: IntegrationLaneCleanupStatus::Removed,
                recorded_at_unix_ms: 5,
            },
        )),
    ];
    let projection = IntegrationProjection::from_entries(&entries);
    let preview = build_task_promotion_preview(
        projection.latest().expect("ready integration plan"),
        TaskPromotionPreviewInput {
            aggregate_diff_artifact_ref: artifact_ref.into(),
            aggregate_diff_digest: aggregate_diff_digest.into(),
            target: sigil_kernel::IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: plan.base_snapshot_id.clone(),
                expected_revision: 0,
            },
            verification_invalidation: vec!["scope-integration-review".to_owned()],
            intent_binding: None,
            policy_digest: VerificationPolicy::no_checks_required("scope-integration-review")
                .stable_hash()?,
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: 10,
        },
    )?;
    assert_eq!(
        preview.ordered_lane_candidates,
        vec![TaskPromotionLaneCandidate {
            lane_id: lane.lane_id.clone(),
            candidate,
            verification_receipt_ids: vec!["receipt-integration-review".to_owned()],
        }]
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionPreviewRecorded(TaskPromotionPreviewRecorded { preview }),
    ));
    let projection = sigil_kernel::TaskStateProjection::from_entries(&entries);
    assert_eq!(
        projection
            .latest_task()
            .map(|task| task.status)
            .expect("task projection"),
        TaskRunStatus::Paused
    );
    let product = sigil_kernel::task_integration_review_product(&entries)
        .expect("integration review product");
    assert_eq!(
        product.preview.ordered_lane_candidates.len(),
        1,
        "fixture must expose one review lane"
    );
    Ok(entries)
}

pub(crate) fn select_root_slash_command(app: &mut AppState, command: &str) -> Result<()> {
    let index = app
        .slash_selector_rows()
        .iter()
        .position(|(label, _)| label == command)
        .ok_or_else(|| anyhow::anyhow!("slash command {command} not found"))?;
    for _ in 0..index {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    Ok(())
}

pub(crate) fn write_session_log(path: &Path, entries: &[SessionLogEntry]) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    for entry in entries {
        store.append(entry)?;
    }
    Ok(())
}

pub(crate) fn child_agent_entries(
    display_name: Option<&str>,
    thread_status: sigil_kernel::AgentThreadStatus,
    child_session_ref: sigil_kernel::SessionRef,
) -> Result<Vec<SessionLogEntry>> {
    child_agent_entries_with(
        "review workspace",
        "Inspect repository",
        display_name,
        "step_1",
        "child_1",
        child_session_ref,
        "subagent_read",
        thread_status,
    )
}

pub(crate) fn child_agent_entries_with(
    objective: &str,
    step_title: &str,
    display_name: Option<&str>,
    step_id: &str,
    child_id: &str,
    child_session_ref: sigil_kernel::SessionRef,
    profile_id: &str,
    thread_status: sigil_kernel::AgentThreadStatus,
) -> Result<Vec<SessionLogEntry>> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new(step_id)?;
    let child_task_id = sigil_kernel::TaskId::new(child_id)?;
    let thread_id = sigil_kernel::AgentThreadId::new(child_id)?;
    let profile_id = sigil_kernel::AgentProfileId::new(profile_id)?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new(format!("snapshot_{}", child_id))?;
    let task_child_status = match thread_status {
        sigil_kernel::AgentThreadStatus::Completed => {
            sigil_kernel::TaskChildSessionStatus::Completed
        }
        sigil_kernel::AgentThreadStatus::Failed => sigil_kernel::TaskChildSessionStatus::Failed,
        sigil_kernel::AgentThreadStatus::Cancelled => {
            sigil_kernel::TaskChildSessionStatus::Cancelled
        }
        sigil_kernel::AgentThreadStatus::Interrupted => {
            sigil_kernel::TaskChildSessionStatus::Interrupted
        }
        sigil_kernel::AgentThreadStatus::Unavailable => {
            sigil_kernel::TaskChildSessionStatus::Unavailable
        }
        _ => sigil_kernel::TaskChildSessionStatus::Started,
    };

    Ok(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: objective.to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: step_title.to_owned(),
                display_name: display_name.map(ToOwned::to_owned),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id,
                child_session_ref: child_session_ref.clone(),
                role: sigil_kernel::AgentRole::SubagentRead,
                status: task_child_status,
                summary_hash: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::System,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: child_session_ref,
                profile_id,
                profile_snapshot_id: snapshot_id.clone(),
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: snapshot_id,
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    model_ref: None,
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new("/tmp/workspace")?,
                    effective_tool_scope_hash: "sha256:tools".to_owned(),
                    effective_permission_policy_hash: "sha256:permissions".to_owned(),
                    effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                    provider_capability_hash: "sha256:provider".to_owned(),
                    model_visible_agent_index_hash: Some("sha256:index".to_owned()),
                    budget_policy_hash: "sha256:budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: objective.to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Background,
                invocation_source: sigil_kernel::AgentInvocationSource::Task,
                display_name: display_name.map(ToOwned::to_owned),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id,
                status: thread_status,
                reason: None,
                updated_at_ms: None,
            },
        )),
    ])
}

pub(crate) fn sample_approval_preview() -> ToolPreview {
    ToolPreview {
        title: "Update note.txt".to_owned(),
        summary: "Preview summary".to_owned(),
        body: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma".to_owned(),
        changed_files: vec!["note.txt".to_owned()],
        file_diffs: vec![sigil_kernel::ToolPreviewFile {
            path: "note.txt".to_owned(),
            diff: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma".to_owned(),
        }],
    }
}

pub(crate) fn sample_delete_approval_preview() -> ToolPreview {
    ToolPreview {
        title: "Delete note.txt".to_owned(),
        summary: "Delete 2 lines from note.txt".to_owned(),
        body: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +0,0 @@\n-alpha\n-beta"
            .to_owned(),
        changed_files: vec!["note.txt".to_owned()],
        file_diffs: vec![sigil_kernel::ToolPreviewFile {
            path: "note.txt".to_owned(),
            diff: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +0,0 @@\n-alpha\n-beta"
                .to_owned(),
        }],
    }
}

pub(crate) fn multi_file_approval_preview() -> ToolPreview {
    ToolPreview {
        title: "Update multiple files".to_owned(),
        summary: "Multi-file preview".to_owned(),
        body: String::new(),
        changed_files: vec!["note-a.txt".to_owned(), "note-b.txt".to_owned()],
        file_diffs: vec![
            sigil_kernel::ToolPreviewFile {
                path: "note-a.txt".to_owned(),
                diff: "--- current/note-a.txt\n+++ proposed/note-a.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n@@ -5,2 +5,2 @@\n delta\n-epsilon\n+zeta".to_owned(),
            },
            sigil_kernel::ToolPreviewFile {
                path: "note-b.txt".to_owned(),
                diff: "--- current/note-b.txt\n+++ proposed/note-b.txt\n@@ -1,1 +1,1 @@\n-old\n+new".to_owned(),
            },
        ],
    }
}

pub(crate) fn inject_write_file_approval(app: &mut AppState, preview: ToolPreview) -> Result<()> {
    app.handle(RunEvent::ToolApprovalRequested {
        approval_identity: test_approval_identity("call-1"),
        effects: std::collections::BTreeSet::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        call: ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"note.txt","content":"hello"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write a file".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(preview),
    })
}
