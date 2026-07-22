use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sigil_kernel::{
    AgentRole, AssistantMessageKind, CandidateCheck, CheckCommand, CheckDiscoverySource,
    CheckPromotion, CheckSpecRecordedEntry, CompletionCriteria, ControlEntry, EvidenceScope,
    NetworkEffect, ReadinessEvaluatedEntry, ReadinessEvaluation, RequiredAction, RunStatus,
    SessionRef, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus, ToolAccess, ToolApproval, ToolCall,
    ToolCategory, ToolEffect, ToolPreviewCapability, ToolSpec, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationProductAction, VerificationVerdict,
    VisibleCompletionState, build_workspace_snapshot, stable_workspace_id,
};

use super::*;
use crate::{
    HttpConversationQueueBlockedReason, HttpConversationQueueCommandAction,
    HttpConversationQueueCommandRequest, HttpConversationQueueDriverCommand,
    HttpConversationQueueDriverError, HttpConversationQueueItemKind,
    HttpConversationQueuePromptMaterial, HttpDurableEgressDisclosureJournal,
    HttpDurableProtocolJournal, HttpForegroundRunOwner, HttpPermissionMode, HttpRunStartRequest,
    HttpRunStatus, HttpSessionCreateRequest, HttpSessionOpenRequest, HttpSessionSnapshot,
};

fn call() -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
    }
}

fn spec(access: ToolAccess, network_effect: Option<NetworkEffect>) -> ToolSpec {
    ToolSpec {
        name: "read_file".to_owned(),
        description: "read a file".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        network_effect,
        preview: ToolPreviewCapability::None,
    }
}

struct ControlledPreparation {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[derive(Default)]
struct FailingQueuedPreparation {
    queued_calls: AtomicUsize,
}

fn production_queue_driver(
    temp: &tempfile::TempDir,
    journal_suffix: &str,
) -> Arc<HttpProductionRunDriver> {
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("queue test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(
            temp.path().join(format!("protocol-{journal_suffix}.json")),
            16,
        )
        .expect("queue protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path()
                .join(format!("disclosures-{journal_suffix}.json")),
            16,
        )
        .expect("queue disclosure journal should initialize"),
    );
    Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production queue driver should initialize"),
    )
}

fn production_queue_session(temp: &tempfile::TempDir) -> HttpSessionSnapshot {
    production_queue_session_named(temp, "queue-session")
}

fn production_queue_session_named(temp: &tempfile::TempDir, name: &str) -> HttpSessionSnapshot {
    let session_path = temp.path().join(format!("{name}.jsonl"));
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)
        .expect("queue session store should initialize");
    let mut session = sigil_kernel::Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session
        .append_control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        })
        .expect("queue session identity should append");
    let durable_session_scope_id = session.session_scope_id().to_owned();
    drop(session);
    HttpSessionSnapshot {
        id: format!("adapter-{name}"),
        label: None,
        run_ids: Vec::new(),
        durable_session_scope_id,
        session_log_path: session_path.display().to_string(),
        foreground_run_id: None,
    }
}

fn queue_command(
    command_id: &str,
    expected_generation: crate::HttpConversationQueueGeneration,
    action: HttpConversationQueueCommandAction,
) -> HttpConversationQueueDriverCommand {
    HttpConversationQueueDriverCommand {
        command_id: command_id.to_owned(),
        client_id: "desktop-client-1".to_owned(),
        request: HttpConversationQueueCommandRequest {
            expected_generation,
            action,
        },
    }
}

fn queued_terminal_context(queued: &HttpQueuedRunPreparation) -> HttpQueuedRunTerminalContext {
    HttpQueuedRunTerminalContext {
        queue_id: queued.promotion.queue_id.clone(),
        dispatch_run_id: queued.promotion.dispatch_run_id.clone(),
        expected_queue_revision: queued.promotion.expected_queue_revision.clone(),
        prompt_hash: queued.promotion.prompt_hash.clone(),
        exact_prompt_key: queued.exact_prompt_key.clone(),
    }
}

fn production_queued_preparation(
    driver: &HttpProductionRunDriver,
    session: &mut HttpSessionSnapshot,
    admission: crate::HttpQueuedRunAdmission,
) -> HttpQueuedRunPreparation {
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approval_call_ids: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should revalidate admission");
    queued
}

#[async_trait]
impl HttpApplicationRunPreparer for ControlledPreparation {
    async fn prepare(
        &self,
        _request: ApplicationRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .map_err(|_| anyhow!("controlled preparation release closed"))?
            .forget();
        Err(anyhow!(
            "controlled preparation released after cancellation"
        ))
    }

    async fn prepare_queued(
        &self,
        _request: ApplicationQueuedRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .map_err(|_| anyhow!("controlled queued preparation release closed"))?
            .forget();
        Err(anyhow!(
            "controlled queued preparation released after cancellation"
        ))
    }
}

#[async_trait]
impl HttpApplicationRunPreparer for FailingQueuedPreparation {
    async fn prepare(
        &self,
        _request: ApplicationRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        Err(anyhow!("ordinary preparation is not expected in this test"))
    }

    async fn prepare_queued(
        &self,
        _request: ApplicationQueuedRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.queued_calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow!("controlled queued preparation failure"))
    }
}

#[tokio::test]
async fn production_queue_mutations_are_durable_cas_guarded_and_owner_exact() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "cas");
    let session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    assert_eq!(initial.total_items, 0);

    let queued = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-safe-1",
                initial.generation.clone(),
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect Cargo.toml".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("safe prompt should enqueue");
    assert_eq!(queued.total_items, 1);
    assert_ne!(queued.generation, initial.generation);
    assert_eq!(
        queued.items[0].prompt_material,
        HttpConversationQueuePromptMaterial::PersistedSafe
    );
    assert!(queued.items[0].dispatchable);
    assert_eq!(
        queued.next_dispatchable_entry_id.as_deref(),
        Some(queued.items[0].entry_id.as_str())
    );
    assert_eq!(
        queued.items[0].entry_id,
        stable_http_queue_id(
            &session.durable_session_scope_id,
            "desktop-client-1",
            "enqueue-safe-1"
        )
        .expect("stable queue id should derive")
        .as_str()
    );

    let queued = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-safe-2",
                queued.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "then inspect README.md".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("second safe prompt should enqueue");
    assert_eq!(queued.total_items, 2);
    assert_eq!(
        queued.items[1].blocked_reason,
        Some(HttpConversationQueueBlockedReason::WaitingForTerminalFrontier)
    );
    assert!(!queued.items[1].dispatchable);

    let admission = driver
        .next_queued_run_admission(&session)
        .expect("queue admission should project")
        .expect("safe queued prompt should dispatch");
    assert_eq!(admission.entry_id, queued.items[0].entry_id);
    assert_eq!(admission.generation, queued.generation);
    assert_eq!(admission.prompt_preview, "inspect Cargo.toml");
    assert_eq!(
        driver
            .next_queued_run_admission(&session)
            .expect("repeat admission should project")
            .expect("repeat admission should remain available")
            .dispatch_run_id,
        admission.dispatch_run_id
    );

    let durable_after_enqueue =
        std::fs::read(&session.session_log_path).expect("queue session should read");
    assert_eq!(
        driver.mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "stale-pause",
                initial.generation,
                HttpConversationQueueCommandAction::Pause,
            ),
        ),
        Err(HttpConversationQueueDriverError::StaleGeneration)
    );
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("queue session should reread"),
        durable_after_enqueue
    );

    let owner = HttpForegroundRunOwner {
        run_id: "run-active".to_owned(),
        owner_revision: "owner-revision-1".to_owned(),
    };
    let wrong_owner = HttpForegroundRunOwner {
        run_id: owner.run_id.clone(),
        owner_revision: "owner-revision-stale".to_owned(),
    };
    let interrupt = queue_command(
        "interrupt-next",
        queued.generation.clone(),
        HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id: owner.run_id.clone(),
            foreground_owner_revision: owner.owner_revision.clone(),
        },
    );
    assert_eq!(
        driver.mutate_conversation_queue(&session, Some(&wrong_owner), &interrupt),
        Err(HttpConversationQueueDriverError::OwnerLost)
    );
    let owner_view = driver
        .mutate_conversation_queue(&session, Some(&owner), &interrupt)
        .expect("exact owner interrupt guard should be accepted without a durable mutation");
    assert_eq!(owner_view.generation, queued.generation);
    assert!(!owner_view.items[0].dispatchable);
    assert_eq!(
        owner_view.items[0].blocked_reason,
        Some(HttpConversationQueueBlockedReason::ForegroundRunActive)
    );
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("queue session should reread"),
        durable_after_enqueue
    );
}

#[tokio::test]
async fn production_queue_interrupt_requires_one_exact_dispatchable_next_item() {
    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-empty");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("empty queue should project");
        let owner = HttpForegroundRunOwner {
            run_id: "run-empty".to_owned(),
            owner_revision: "owner-empty".to_owned(),
        };
        let durable_before = std::fs::read(&session.session_log_path)
            .expect("empty queue durable stream should read");

        assert_eq!(
            driver.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-empty",
                    initial.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::Conflict)
        );
        assert_eq!(
            std::fs::read(&session.session_log_path)
                .expect("empty queue durable stream should reread"),
            durable_before
        );
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-unsupported");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("unsupported queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-plan-prompt",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "plan the next change".to_owned(),
                        kind: HttpConversationQueueItemKind::PlanPrompt,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("unsupported item should remain visible in the queue");
        let owner = HttpForegroundRunOwner {
            run_id: "run-unsupported".to_owned(),
            owner_revision: "owner-unsupported".to_owned(),
        };

        assert_eq!(
            driver.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-unsupported",
                    queued.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::Unsupported)
        );
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-paused");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("paused queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-paused-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("paused candidate should enqueue");
        let paused = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-pause",
                    queued.generation,
                    HttpConversationQueueCommandAction::Pause,
                ),
            )
            .expect("queue should pause");
        let owner = HttpForegroundRunOwner {
            run_id: "run-paused".to_owned(),
            owner_revision: "owner-paused".to_owned(),
        };

        assert_eq!(
            driver.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-paused",
                    paused.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::Conflict)
        );
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-reentry-before-restart");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("reentry queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-reentry-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect with authorization=process-local-secret".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("exact prompt should enqueue");
        drop(driver);
        let restarted = production_queue_driver(&temp, "interrupt-reentry-after-restart");
        let owner = HttpForegroundRunOwner {
            run_id: "run-reentry".to_owned(),
            owner_revision: "owner-reentry".to_owned(),
        };

        assert_eq!(
            restarted.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-reentry",
                    queued.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::RequiresReentry)
        );
    }
}

#[test]
fn production_queue_stable_identity_seed_has_no_delimiter_tuple_collision() {
    let left = stable_http_queue_id("scope:a", "client", "command")
        .expect("left stable queue id should derive");
    let right = stable_http_queue_id("scope", "a:client", "command")
        .expect("right stable queue id should derive");

    assert_ne!(left, right);
    assert_ne!(
        stable_http_identity_seed(&["scope:a", "client", "command"]),
        stable_http_identity_seed(&["scope", "a:client", "command"])
    );
}

#[tokio::test]
async fn production_queue_exact_prompt_is_process_local_and_requires_reentry_after_restart() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "exact-owner-1");
    let session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    let raw_prompt = "inspect with authorization=super-secret-value";
    let queued = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-exact-1",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: raw_prompt.to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("exact prompt should enqueue with process-local material");
    assert_eq!(
        queued.items[0].prompt_material,
        HttpConversationQueuePromptMaterial::AvailableProcessLocal
    );
    assert!(queued.items[0].dispatchable);
    assert!(
        driver
            .next_queued_run_admission(&session)
            .expect("queue admission should project")
            .is_some()
    );
    let durable =
        std::fs::read_to_string(&session.session_log_path).expect("queue session should read");
    assert!(!durable.contains(raw_prompt));
    assert!(!durable.contains("super-secret-value"));

    drop(driver);
    let restarted = production_queue_driver(&temp, "exact-owner-2");
    let restarted_view = restarted
        .conversation_queue_view(&session, None)
        .expect("restarted owner should project durable queue");
    assert_eq!(
        restarted_view.items[0].prompt_material,
        HttpConversationQueuePromptMaterial::RequiresReentry
    );
    assert_eq!(
        restarted_view.items[0].blocked_reason,
        Some(HttpConversationQueueBlockedReason::RequiresReentry)
    );
    assert!(!restarted_view.items[0].dispatchable);
    assert!(restarted_view.next_dispatchable_entry_id.is_none());
    assert!(
        restarted
            .next_queued_run_admission(&session)
            .expect("restarted admission should project")
            .is_none()
    );
}

#[tokio::test]
async fn production_queue_session_delete_purges_only_matching_exact_prompt_material() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "delete-purge");
    let first_session = production_queue_session_named(&temp, "delete-first");
    let second_session = production_queue_session_named(&temp, "delete-second");
    for (session, command_id, secret) in [
        (&first_session, "delete-first-exact", "first-delete-secret"),
        (
            &second_session,
            "delete-second-exact",
            "second-delete-secret",
        ),
    ] {
        let initial = driver
            .conversation_queue_view(session, None)
            .expect("delete purge initial queue should project");
        driver
            .mutate_conversation_queue(
                session,
                None,
                &queue_command(
                    command_id,
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: format!("inspect with authorization={secret}"),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("delete purge exact prompt should enqueue");
    }
    assert_eq!(
        driver
            .exact_queue_prompts
            .lock()
            .expect("delete purge cache should lock")
            .len(),
        2
    );

    driver.purge_session_local_state(&first_session.durable_session_scope_id);

    let exact_prompts = driver
        .exact_queue_prompts
        .lock()
        .expect("delete purge cache should relock");
    assert_eq!(exact_prompts.len(), 1);
    assert!(
        exact_prompts
            .keys()
            .all(|key| key.session_scope_id == second_session.durable_session_scope_id)
    );
}

#[tokio::test]
async fn production_queue_unpromoted_terminal_consumes_only_the_admitted_item() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "unpromoted-terminal");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    let first = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-first",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect with authorization=first-secret".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("first prompt should enqueue");
    let queue = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-second",
                first.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect README.md next".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("second prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("admission should project")
        .expect("first prompt should admit");
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approval_call_ids: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should revalidate admission");
    let terminal = queued_terminal_context(&queued);

    finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
        .expect("preparation failure should consume the admitted item");
    evict_http_promoted_exact_prompt(&session, Some(&terminal), &driver.exact_queue_prompts)
        .expect("terminal item should evict process-local exact material");
    let projected = driver
        .conversation_queue_view(&session, None)
        .expect("terminal queue should project");
    assert_eq!(projected.total_items, 1);
    assert_eq!(
        projected.items[0].status,
        crate::HttpConversationQueueItemStatus::Queued
    );
    assert_eq!(
        projected.next_dispatchable_entry_id.as_deref(),
        Some(projected.items[0].entry_id.as_str())
    );
    assert_ne!(projected.items[0].entry_id, terminal.queue_id.as_str());
    assert_eq!(
        driver.mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "edit-terminal-item",
                projected.generation.clone(),
                HttpConversationQueueCommandAction::Edit {
                    entry_id: terminal.queue_id.as_str().to_owned(),
                    prompt: "must not revive a terminal item".to_owned(),
                    reasoning_effort: None,
                },
            ),
        ),
        Err(HttpConversationQueueDriverError::Terminal)
    );
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("terminal queue entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Rejected
            ))
    );
    assert!(
        !driver
            .exact_queue_prompts
            .lock()
            .expect("exact prompt cache should lock")
            .contains_key(&terminal.exact_prompt_key)
    );
    let next = driver
        .next_queued_run_admission(&session)
        .expect("next admission should project")
        .expect("unconsumed second item should remain dispatchable");
    assert_eq!(next.entry_id, queue.items[1].entry_id);
    assert_ne!(next.dispatch_run_id, terminal.dispatch_run_id);
}

#[tokio::test]
async fn production_queue_unpromoted_terminal_does_not_consume_mutation_drift() {
    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "terminal-edit-drift");
        let mut session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("edit drift initial queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "edit-drift-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("edit drift prompt should enqueue");
        let admission = driver
            .next_queued_run_admission(&session)
            .expect("edit drift admission should project")
            .expect("edit drift prompt should admit");
        let original_dispatch_run_id = admission.dispatch_run_id.clone();
        let preparation = production_queued_preparation(&driver, &mut session, admission);
        let terminal = queued_terminal_context(&preparation);
        let edited = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "edit-drift-mutation",
                    queued.generation,
                    HttpConversationQueueCommandAction::Edit {
                        entry_id: terminal.queue_id.as_str().to_owned(),
                        prompt: "inspect the workspace manifest instead".to_owned(),
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("queued edit should win its durable CAS");
        finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
            .expect("stale preparation terminal should become a zero-write race outcome");
        session.foreground_run_id = None;
        let projected = driver
            .conversation_queue_view(&session, None)
            .expect("edited queue should remain active");
        assert_eq!(projected.generation, edited.generation);
        assert_eq!(projected.total_items, 1);
        assert!(
            projected.items[0]
                .prompt_preview
                .contains("workspace manifest")
        );
        assert!(
            JsonlSessionStore::read_entries(&session.session_log_path)
                .expect("edit drift entries should read")
                .iter()
                .all(|entry| !matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                        if status.queue_id == terminal.queue_id
                            && status.status == ConversationInputStatus::Rejected
                ))
        );
        let retry = driver
            .next_queued_run_admission(&session)
            .expect("edited prompt retry admission should project")
            .expect("edited prompt should remain dispatchable");
        assert_eq!(retry.entry_id, terminal.queue_id.as_str());
        assert_ne!(retry.dispatch_run_id, original_dispatch_run_id);
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "terminal-remove-drift");
        let mut session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("remove drift initial queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "remove-drift-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("remove drift prompt should enqueue");
        let admission = driver
            .next_queued_run_admission(&session)
            .expect("remove drift admission should project")
            .expect("remove drift prompt should admit");
        let preparation = production_queued_preparation(&driver, &mut session, admission);
        let terminal = queued_terminal_context(&preparation);
        driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "remove-drift-mutation",
                    queued.generation,
                    HttpConversationQueueCommandAction::Remove {
                        entry_id: terminal.queue_id.as_str().to_owned(),
                    },
                ),
            )
            .expect("queued removal should win its durable CAS");
        finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
            .expect("removed queue item should not receive a second terminal");
        let statuses =
            JsonlSessionStore::read_entries(&session.session_log_path)
                .expect("remove drift entries should read")
                .into_iter()
                .filter_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(
                        status,
                    )) if status.queue_id == terminal.queue_id => Some(status.status),
                    _ => None,
                })
                .collect::<Vec<_>>();
        assert_eq!(statuses, vec![ConversationInputStatus::Cancelled]);
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "terminal-reorder-drift");
        let mut session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("reorder drift initial queue should project");
        let first = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "reorder-drift-first",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml first".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("reorder drift first prompt should enqueue");
        let second = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "reorder-drift-second",
                    first.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect README.md second".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("reorder drift second prompt should enqueue");
        let admission = driver
            .next_queued_run_admission(&session)
            .expect("reorder drift admission should project")
            .expect("reorder drift first prompt should admit");
        let preparation = production_queued_preparation(&driver, &mut session, admission);
        let terminal = queued_terminal_context(&preparation);
        let second_entry_id = second.items[1].entry_id.clone();
        driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "reorder-drift-mutation",
                    second.generation,
                    HttpConversationQueueCommandAction::Reorder {
                        entry_id: terminal.queue_id.as_str().to_owned(),
                        after_entry_id: Some(second_entry_id.clone()),
                    },
                ),
            )
            .expect("queued reorder should win its durable CAS");
        finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
            .expect("reordered queue item should not receive a stale terminal");
        session.foreground_run_id = None;
        let retry = driver
            .next_queued_run_admission(&session)
            .expect("reordered queue admission should project")
            .expect("new FIFO head should remain dispatchable");
        assert_eq!(retry.entry_id, second_entry_id);
        assert!(
            JsonlSessionStore::read_entries(&session.session_log_path)
                .expect("reorder drift entries should read")
                .iter()
                .all(|entry| !matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                        if status.queue_id == terminal.queue_id
                            && status.status == ConversationInputStatus::Rejected
                ))
        );
    }
}

#[tokio::test]
async fn production_queue_cancel_before_promotion_is_terminal_without_replay() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "unpromoted-cancel");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    let queue = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-cancelled",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "cancel this queued run".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("admission should project")
        .expect("prompt should admit");
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approval_call_ids: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should revalidate admission");
    let terminal = queued_terminal_context(&queued);

    finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Cancelled)
        .expect("pre-promotion cancellation should become terminal");
    let projected = driver
        .conversation_queue_view(&session, None)
        .expect("cancelled queue should project");
    assert_eq!(queue.items[0].entry_id, terminal.queue_id.as_str());
    assert_eq!(projected.total_items, 0);
    assert!(projected.items.is_empty());
    assert!(projected.next_dispatchable_entry_id.is_none());
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("cancelled queue entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Cancelled
            ))
    );
    assert!(
        driver
            .next_queued_run_admission(&session)
            .expect("post-cancel admission should project")
            .is_none()
    );
}

#[tokio::test]
async fn production_queue_promotion_evicts_exact_material_and_terminal_uses_attempt_evidence() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "promoted-terminal");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-promoted",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect with authorization=promotion-secret".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("exact prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("admission should project")
        .expect("prompt should admit");
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approval_call_ids: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should materialize promotion input");
    let terminal = queued_terminal_context(&queued);
    let store = JsonlSessionStore::new(&session.session_log_path)
        .expect("queue session store should reopen");
    store
        .append_conversation_input_promoted(queued.promotion)
        .expect("promotion should commit under the durable queue CAS");

    evict_http_promoted_exact_prompt(&session, Some(&terminal), &driver.exact_queue_prompts)
        .expect("promotion should evict process-local exact material");
    assert!(
        driver
            .exact_queue_prompts
            .lock()
            .expect("exact prompt cache should lock")
            .is_empty()
    );
    finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
        .expect("missing physical attempt should reject promoted queue item");

    let entries = JsonlSessionStore::read_entries(&session.session_log_path)
        .expect("promoted queue entries should read");
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion))
            if promotion.queue_id == terminal.queue_id
                && promotion.dispatch_run_id == terminal.dispatch_run_id
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.queue_id == terminal.queue_id
                && status.status == ConversationInputStatus::Rejected
    )));
    assert!(
        entries
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_)))
    );
}

#[tokio::test]
async fn production_queue_restart_reconciles_orphan_dispatch_before_next_admission() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "orphan-owner-before-restart");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("orphan initial queue should project");
    let first = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "orphan-first",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect Cargo.toml first".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("orphan first prompt should enqueue");
    let second = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "orphan-second",
                first.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect README.md second".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("orphan second prompt should enqueue");
    let second_entry_id = second.items[1].entry_id.clone();
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("orphan first admission should project")
        .expect("orphan first prompt should admit");
    let preparation = production_queued_preparation(&driver, &mut session, admission);
    let terminal = queued_terminal_context(&preparation);
    JsonlSessionStore::new(&session.session_log_path)
        .expect("orphan queue store should reopen")
        .append_conversation_input_promoted(preparation.promotion)
        .expect("orphan promotion should commit");
    session.foreground_run_id = None;
    drop(driver);

    let restarted = production_queue_driver(&temp, "orphan-owner-after-restart");
    let durable_before_get = std::fs::read(&session.session_log_path)
        .expect("orphan durable stream should read before GET");
    let blocked = restarted
        .conversation_queue_view(&session, None)
        .expect("orphan queue GET should remain a pure projection");
    assert_eq!(blocked.total_items, 2);
    assert_eq!(
        blocked.items[0].status,
        crate::HttpConversationQueueItemStatus::Dispatching
    );
    assert_eq!(
        blocked.items[0].blocked_reason,
        Some(HttpConversationQueueBlockedReason::Conflict)
    );
    assert!(!blocked.items[1].dispatchable);
    assert_eq!(
        blocked.items[1].blocked_reason,
        Some(HttpConversationQueueBlockedReason::WaitingForTerminalFrontier)
    );
    assert!(blocked.next_dispatchable_entry_id.is_none());
    assert_eq!(
        std::fs::read(&session.session_log_path)
            .expect("orphan durable stream should read after GET"),
        durable_before_get,
        "queue GET must not reconcile durable orphan state"
    );

    let next = restarted
        .next_queued_run_admission(&session)
        .expect("scheduler admission should reconcile orphan dispatch evidence")
        .expect("second prompt should admit after orphan terminal convergence");
    assert_eq!(next.entry_id, second_entry_id);
    let projected = restarted
        .conversation_queue_view(&session, None)
        .expect("reconciled queue should project");
    assert_eq!(projected.total_items, 1);
    assert_eq!(projected.items[0].entry_id, second_entry_id);
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("orphan terminal entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Rejected
            ))
    );
}

#[tokio::test]
async fn production_queue_orphan_reconciliation_retries_frontier_drift() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "orphan-frontier-drift");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("frontier drift initial queue should project");
    driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "frontier-drift-enqueue",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect Cargo.toml".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("frontier drift prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("frontier drift admission should project")
        .expect("frontier drift prompt should admit");
    let preparation = production_queued_preparation(&driver, &mut session, admission);
    let terminal = queued_terminal_context(&preparation);
    JsonlSessionStore::new(&session.session_log_path)
        .expect("frontier drift store should reopen")
        .append_conversation_input_promoted(preparation.promotion)
        .expect("frontier drift promotion should commit");
    session.foreground_run_id = None;

    let mut injected_drift = false;
    driver
        .reconcile_orphaned_queued_dispatches_with(&session, |store| {
            if !injected_drift {
                injected_drift = true;
                store
                    .append(&SessionLogEntry::Assistant(ModelMessage::assistant(
                        Some(
                            "unrelated durable event advances the terminal evidence frontier"
                                .to_owned(),
                        ),
                        Vec::new(),
                    )))
                    .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            }
            Ok(())
        })
        .expect("orphan reconciliation should retry one exact frontier drift");
    assert!(injected_drift);
    assert!(
        driver
            .conversation_queue_view(&session, None)
            .expect("frontier drift queue should converge")
            .items
            .is_empty()
    );
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("frontier drift terminal entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Rejected
            ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_queue_scheduler_uses_supervisor_and_terminalizes_preparation_failure_once() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("queue scheduler config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("scheduler-protocol.json"), 16)
            .expect("queue scheduler protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("scheduler-disclosures.json"),
            16,
        )
        .expect("queue scheduler disclosure journal should initialize"),
    );
    let preparer = Arc::new(FailingQueuedPreparation::default());
    let driver = Arc::new(
        HttpProductionRunDriver::new_with_preparer(
            HttpProductionRunDriverOptions::new(config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
            preparer.clone(),
        )
        .expect("production queue scheduler driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("scheduler-commands.json"), 16)
            .expect("queue scheduler command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production queue scheduler registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("queue scheduler session should bind");
    let initial = registry
        .conversation_queue(&session.id)
        .expect("queue scheduler initial projection should read");
    let command = crate::HttpCommandEnvelope::new(
        "scheduler-enqueue-1",
        "desktop-client-1",
        &session.id,
        HttpConversationQueueCommandRequest {
            expected_generation: initial.generation,
            action: HttpConversationQueueCommandAction::Enqueue {
                prompt: "inspect Cargo.toml".to_owned(),
                kind: HttpConversationQueueItemKind::Chat,
                reasoning_effort: None,
            },
        },
    );
    let receipt = registry
        .command_conversation_queue(&session.id, command)
        .expect("queue scheduler enqueue should commit before admission");
    assert_eq!(receipt.queue.total_items, 1);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let queue = registry
                .conversation_queue(&session.id)
                .expect("queue scheduler terminal projection should read");
            if preparer.queued_calls.load(Ordering::SeqCst) == 1
                && queue.total_items == 0
                && driver
                    .active_run_count()
                    .expect("queue scheduler active runs should read")
                    == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued preparation failure should terminalize without a scheduler hot loop");
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(preparer.queued_calls.load(Ordering::SeqCst), 1);
    let run_ids = registry
        .get_session(&session.id)
        .expect("queue scheduler session should remain bound")
        .run_ids;
    assert_eq!(run_ids.len(), 1);
    assert_eq!(
        registry
            .get_run(&run_ids[0])
            .expect("queue scheduler failed run should remain inspectable")
            .status,
        HttpRunStatus::Failed
    );
}

#[test]
fn approval_broker_routes_one_explicit_decision_with_stable_guards() {
    let broker = Arc::new(HttpApprovalBroker::default());
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            Duration::from_secs(1),
            false,
        )
        .expect("approval should register");
    assert_eq!(pending.policy_version, HTTP_APPROVAL_POLICY_VERSION);
    assert!(pending.approval_request_id.starts_with("http-approval-v1:"));
    assert_eq!(pending.tool_call_hash.len(), 64);

    broker
        .resolve(
            "call-1",
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
            },
        )
        .expect("decision should resolve");
    let outcome = broker
        .wait_for_decision("call-1")
        .expect("resolved wait should finish");

    assert!(!outcome.expired);
    assert!(matches!(
        outcome.decision,
        Some(HttpApprovalDecisionRecord {
            decision: ToolApprovalUserDecision::Approved,
            ..
        })
    ));
}

#[test]
fn approval_broker_expires_and_cleans_up_without_fabricating_a_decision() {
    let broker = HttpApprovalBroker::default();
    broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            Duration::ZERO,
            false,
        )
        .expect("approval should register");

    let outcome = broker
        .wait_for_decision("call-1")
        .expect("expiry should be a typed denial path");

    assert!(outcome.expired);
    assert!(outcome.decision.is_none());
    assert!(
        broker
            .pending
            .lock()
            .expect("broker should lock")
            .is_empty()
    );
}

#[test]
fn approval_handler_only_resolves_explicit_broker_decisions() {
    let broker = Arc::new(HttpApprovalBroker::default());
    broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Write, None),
            Duration::from_secs(1),
            false,
        )
        .expect("approval should register");
    broker
        .resolve(
            "call-1",
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
            },
        )
        .expect("decision should resolve");
    let mut handler = HttpProductionApprovalHandler {
        run_id: "run-1".to_owned(),
        registry: Weak::new(),
        broker,
    };

    assert!(matches!(
        handler
            .approve_tool_call(&call(), &spec(ToolAccess::Write, None))
            .expect("explicit decision should resolve"),
        ToolApproval::Approve
    ));
    assert!(handler.approval_is_explicit_user_action());
}

#[test]
fn approval_handler_preserves_bounded_session_decisions() {
    let broker = Arc::new(HttpApprovalBroker::default());
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            Duration::from_secs(1),
            true,
        )
        .expect("approval should register");
    assert!(pending.session_grant_available);
    broker
        .resolve(
            "call-1",
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::ApprovedForSession,
                reason: None,
            },
        )
        .expect("session decision should resolve");
    let mut handler = HttpProductionApprovalHandler {
        run_id: "run-1".to_owned(),
        registry: Weak::new(),
        broker,
    };

    assert!(matches!(
        handler
            .approve_tool_call(&call(), &spec(ToolAccess::Read, None))
            .expect("session decision should reach the kernel"),
        ToolApproval::ApproveForSession
    ));
}

#[tokio::test]
async fn production_driver_rejects_an_in_memory_only_event_bus() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );

    assert!(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new("sigil.toml", "."),
            disclosure_journal,
            Arc::new(HttpLiveEventBus::new(8)),
            tokio::runtime::Handle::current(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn production_driver_session_reopen_revalidates_lifecycle_and_durable_truth() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let session_path = sessions.join("session-history.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)
        .expect("durable session store should open");
    let mut session = sigil_kernel::Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session
        .append_user_message(sigil_kernel::ModelMessage::user("history"))
        .expect("durable message should append");
    session
        .append_assistant_message(sigil_kernel::ModelMessage::assistant_with_kind(
            Some("durable answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ))
        .expect("durable assistant should append");
    let durable_session_id = session.session_scope_id().to_owned();
    drop(session);

    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        "workspace-1",
        &sessions,
        temp.path().join("exports"),
    );
    let options = HttpProductionRunDriverOptions::new(&config_path, temp.path())
        .with_session_lifecycle(lifecycle);
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            options,
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 8)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let request = HttpSessionOpenRequest {
        session_ref: "session-history.jsonl".to_owned(),
        session_id: durable_session_id.clone(),
        label: Some("History".to_owned()),
    };

    let opened = registry
        .open_session(request.clone())
        .expect("current durable source should reopen");

    assert_eq!(opened.durable_session_scope_id, durable_session_id);
    let transcript = registry
        .transcript_page(&opened.id, None, 50)
        .expect("production transcript should project");
    assert_eq!(transcript.session_scope_id, durable_session_id);
    assert_eq!(transcript.total_messages, 2);
    assert_eq!(
        transcript.messages[1].content.as_deref(),
        Some("durable answer")
    );
    assert_eq!(
        transcript.messages[1].assistant_kind,
        Some(crate::HttpTranscriptAssistantKind::FinalAnswer)
    );
    let display = registry
        .conversation_display_page(&opened.id, None, 50)
        .expect("production canonical display should project");
    assert_eq!(display.request_scope, opened.id);
    assert_eq!(display.through_session_stream_sequence, "3");
    assert_eq!(display.total_items, "2");
    assert_eq!(display.items.len(), 2);
    assert_eq!(display.items[1].display_order.session_stream_sequence, "2");
    assert!(display.live_provisional_anchor.is_none());
    assert!(
        !serde_json::to_string(&display)
            .expect("canonical display should serialize")
            .contains(&durable_session_id)
    );
    assert_eq!(
        registry.conversation_display_page(&opened.id, Some("e30"), 50),
        Err(crate::HttpRegistryError::ConversationDisplayCursorInvalid)
    );
    assert_eq!(
        std::path::Path::new(&opened.session_log_path),
        session_path
            .canonicalize()
            .expect("session path should resolve")
    );
    assert_eq!(
        registry
            .open_session(request)
            .expect("duplicate reopen should be idempotent")
            .id,
        opened.id
    );
    assert_eq!(
        registry.open_session(HttpSessionOpenRequest {
            session_ref: "session-history.jsonl".to_owned(),
            session_id: "stale-id".to_owned(),
            label: None,
        }),
        Err(crate::HttpRegistryError::DurableSessionIdentityChanged)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_driver_projects_and_executes_real_verification_rerun() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace should create");
    std::fs::write(workspace.join("note.txt"), "current\n").expect("fixture should write");
    let workspace = workspace.canonicalize().expect("workspace should resolve");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "workspace"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 8)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let adapter_session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let store = sigil_kernel::JsonlSessionStore::new(&adapter_session.session_log_path)
        .expect("session store should open");
    let mut session =
        sigil_kernel::Session::load_from_store("deepseek", "deepseek-v4-flash", store)
            .expect("session should load");
    let task_id = TaskId::new("task_1").expect("task id");
    let step_id = TaskStepId::new("verify_1").expect("step id");
    let scope = EvidenceScope::Step("task_1:verify_1".to_owned());
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("session ref"),
            objective: "verify workspace".to_owned(),
            status: TaskRunStatus::Paused,
            reason: None,
        }))
        .expect("task run should append");
    session
        .append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "verify".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Verify),
                isolation: None,
            }],
            reason: None,
        }))
        .expect("task plan should append");
    session
        .append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Blocked,
            title: Some("verify".to_owned()),
            summary: None,
            reason: None,
        }))
        .expect("task step should append");
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )
    .expect("configured check should promote");
    let check_spec = trusted.check_spec.clone();
    session
        .append_control(ControlEntry::CheckSpecRecorded(
            CheckSpecRecordedEntry::new(
                EvidenceScope::Task(task_id.as_str().to_owned()),
                trusted,
                "event-config",
            ),
        ))
        .expect("check spec should append");
    let mut policy = VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = vec![check_spec.clone()];
    policy.completion_criteria = CompletionCriteria::AllRequiredChecks;
    policy.allow_unverified_completion = false;
    policy.timeout_ms = Some(60_000);
    let policy_entry = VerificationPolicyChangedEntry::new(
        EvidenceScope::Task(task_id.as_str().to_owned()),
        policy.clone(),
        "event-policy",
    )
    .expect("policy should hash");
    let policy_hash = policy_entry.policy_hash.clone();
    session
        .append_control(ControlEntry::VerificationPolicyChanged(policy_entry))
        .expect("policy should append");
    let workspace_id = stable_workspace_id(&workspace).expect("workspace id");
    let snapshot =
        build_workspace_snapshot(&workspace, workspace_id, &policy.verification_scope, 0)
            .expect("workspace should snapshot");
    let snapshot_id = snapshot
        .workspace_snapshot_id
        .expect("snapshot should have identity");
    session
        .append_control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
            scope,
            evaluation: ReadinessEvaluation {
                run_status: RunStatus::Completed,
                verification_verdict: VerificationVerdict::Missing,
                visible_state: VisibleCompletionState::CompletedUnverified,
                reasons: Vec::new(),
                required_actions: vec![RequiredAction::RunCheck {
                    check_spec_id: check_spec.check_spec_id.clone(),
                }],
            },
            policy_hash: Some(policy_hash),
            workspace_snapshot_id: Some(snapshot_id),
        }))
        .expect("readiness should append");
    drop(session);

    let rendered = registry
        .verification_view(&adapter_session.id)
        .expect("verification should project")
        .expect("verification should exist");
    let VerificationProductAction::Rerun(request) = rendered.action.expect("rerun action") else {
        panic!("expected exact rerun action");
    };
    let command = crate::HttpCommandEnvelope::new(
        "verification-real-1",
        "desktop-test",
        &adapter_session.id,
        request,
    );
    let registry_for_rerun = Arc::clone(&registry);
    let session_id = adapter_session.id.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        registry_for_rerun.rerun_verification_command(&session_id, command)
    })
    .await
    .expect("rerun worker should join")
    .expect("real verification should execute");

    assert_eq!(receipt.verification.status, "passed");
    assert!(receipt.verification.action.is_none());
    assert_eq!(
        receipt.verification.evidence.check_status,
        Some(sigil_kernel::VerificationCheckRunStatus::Succeeded)
    );
    assert!(receipt.verification.evidence.receipt_id.is_some());
    assert!(
        receipt
            .verification
            .evidence
            .workspace_snapshot_id
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparation_deadline_quarantines_before_ack_and_retains_the_owner_for_reaping() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut options = HttpProductionRunDriverOptions::new(&config_path, temp.path());
    options.cancellation_timeout = Duration::from_millis(40);
    let driver = Arc::new(
        HttpProductionRunDriver::new_with_preparer(
            options,
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
            Arc::new(ControlledPreparation {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 16)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "wait in preparation".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_name: None,
                model_selection_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
            },
        )
        .expect("run should start");
    started
        .acquire()
        .await
        .expect("preparation should start")
        .forget();

    let cancel_registry = Arc::clone(&registry);
    let run_id = run.id.clone();
    let cancel = tokio::task::spawn_blocking(move || cancel_registry.cancel_run(&run_id));
    let result = tokio::time::timeout(Duration::from_millis(400), cancel)
        .await
        .expect("cancel caller must return at the configured deadline")
        .expect("cancel worker should join");
    assert!(matches!(
        result,
        Err(crate::HttpRegistryError::DriverRejected {
            operation: "cancel",
            ..
        })
    ));
    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::ExecutionUncertain
    );
    assert_eq!(
        driver
            .active_run_count()
            .expect("active owners should remain observable"),
        1,
        "the timed-out preparation owner must remain held until it is reaped"
    );

    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if driver.active_run_count().expect("active runs should read") == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released preparation should be reaped");
    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::ExecutionUncertain
    );
}

#[test]
fn approval_protocol_event_exposes_the_exact_guard_required_by_the_endpoint() {
    let bus = HttpLiveEventBus::new(8);
    let call = call();
    let spec = spec(ToolAccess::Write, None);
    let pending = HttpPendingApproval {
        call_id: call.id.clone(),
        tool_name: spec.name.clone(),
        approval_request_id: format!("http-approval-v1:{}", "a".repeat(64)),
        tool_call_hash: "b".repeat(64),
        policy_version: HTTP_APPROVAL_POLICY_VERSION.to_owned(),
        expires_at_ms: 10,
        session_grant_available: false,
    };
    let event = PublicRunEvent::new(
        "durable-session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            call,
            spec,
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    );

    let published = bus
        .publish_run_event_with_approval(event, Some(pending.clone()))
        .expect("matching HTTP approval guard should publish");

    assert_eq!(published.approval_request, Some(pending));
    assert!(matches!(
        published.view(),
        crate::HttpProtocolEventView::Durable(crate::HttpDurableEventView {
            approval_request: Some(_),
            ..
        })
    ));
}

#[test]
fn approval_protocol_event_rejects_guard_for_another_call() {
    let bus = HttpLiveEventBus::new(8);
    let call = call();
    let spec = spec(ToolAccess::Write, None);
    let event = PublicRunEvent::new(
        "durable-session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            call,
            spec,
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    );
    let wrong = HttpPendingApproval {
        call_id: "call-other".to_owned(),
        tool_name: "read_file".to_owned(),
        approval_request_id: format!("http-approval-v1:{}", "a".repeat(64)),
        tool_call_hash: "b".repeat(64),
        policy_version: HTTP_APPROVAL_POLICY_VERSION.to_owned(),
        expires_at_ms: 10,
        session_grant_available: false,
    };

    assert!(matches!(
        bus.publish_run_event_with_approval(event, Some(wrong)),
        Err(crate::HttpEventPublishError::ApprovalMetadata)
    ));
}

#[tokio::test]
async fn production_driver_uses_shared_runtime_preparation_and_records_typed_failure() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("test config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 16)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "hello".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_name: None,
                model_selection_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
            },
        )
        .expect("owned production supervisor should accept the run");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = registry
                .get_run(&run.id)
                .expect("run should remain addressable")
                .status;
            if status.is_terminal() {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("preparation failure should terminate promptly");

    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::Failed
    );
    assert!(session.session_log_path.ends_with(".jsonl"));
    let replay = event_bus
        .replay_run_after(&session.durable_session_scope_id, &run.id, None)
        .expect("typed preparation failure should be durable");
    assert!(matches!(
        replay.last().map(|event| &event.run_event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
}

#[tokio::test]
async fn production_cancel_returns_only_after_supervisor_acknowledges_activation() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(temp.path().join("sigil.toml"), temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let (cancel_sender, mut cancel_receiver) = mpsc::unbounded_channel();
    driver
        .active_runs
        .lock()
        .expect("active runs should lock")
        .insert(
            "run-1".to_owned(),
            Arc::new(HttpProductionActiveRun {
                session_id: "session-1".to_owned(),
                broker: Arc::new(HttpApprovalBroker::default()),
                cancel_sender,
            }),
        );
    let (finished, finished_rx) = std_mpsc::channel();
    let cancel_driver = Arc::clone(&driver);
    let caller = std::thread::spawn(move || {
        let result = cancel_driver.cancel_run(HttpRunDriverCancel {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            reason: Some("user requested stop".to_owned()),
        });
        finished
            .send(())
            .expect("completion signal should be delivered");
        result
    });
    let command = cancel_receiver
        .recv()
        .await
        .expect("supervisor should receive cancellation");

    assert_eq!(command.reason, "user requested stop");
    assert!(finished_rx.try_recv().is_err());
    command
        .acknowledgement
        .send(Ok(()))
        .expect("durable activation acknowledgement should send");
    finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("driver call should finish after acknowledgement");
    caller
        .join()
        .expect("cancel caller should join")
        .expect("acknowledged cancellation should succeed");
}
