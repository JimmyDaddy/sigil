use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use fs2::FileExt;

use super::*;
use crate::{
    AgentResultContinuationStatus, ConversationInputKind, ConversationInputQueueId,
    ConversationInputStatus, ConversationInputTarget, ConversationQueueMutation,
    ConversationQueueMutationCommand, ConversationQueueRevision, ConversationTurnRef, SessionRef,
    TaskGuidancePromotedEntry,
};

#[derive(Default)]
struct NoticeRecorder {
    notices: Mutex<Vec<ActiveProjectionNotice>>,
}

impl ActiveProjectionObserver for NoticeRecorder {
    fn active_projection_changed(&self, notice: ActiveProjectionNotice) {
        self.notices
            .lock()
            .expect("notice recorder lock is available")
            .push(notice);
    }
}

#[test]
fn canonical_store_clones_share_projection_and_observers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = JsonlSessionStore::new(&path)?;
    let second = JsonlSessionStore::new(&path)?;
    let initial = first.active_projection_snapshot()?;
    let recorder = Arc::new(NoticeRecorder::default());
    let subscription = second.register_active_projection_observer(recorder.clone());

    first.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "shared_projection_test".to_owned(),
        data: serde_json::json!({ "ordinal": 1 }),
    }))?;
    let from_first = first.active_projection_snapshot()?;
    let from_second = second.active_projection_snapshot()?;

    assert!(Arc::ptr_eq(&from_first.projection, &from_second.projection));
    assert_ne!(initial.frontier(), from_first.frontier());
    assert_eq!(
        recorder
            .notices
            .lock()
            .expect("notice recorder lock is available")
            .len(),
        1
    );

    drop(subscription);
    first.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "observer_removed".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    assert_eq!(
        recorder
            .notices
            .lock()
            .expect("notice recorder lock is available")
            .len(),
        1
    );
    Ok(())
}

#[test]
fn hot_append_and_ready_snapshot_do_not_increase_full_scan_count() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.active_projection_snapshot()?;
    let scans_after_seed = store.writer_full_scan_count()?;

    for ordinal in 0..32 {
        store.append(&SessionLogEntry::Control(ControlEntry::Note {
            kind: "hot_append".to_owned(),
            data: serde_json::json!({ "ordinal": ordinal }),
        }))?;
        store.active_projection_snapshot()?;
    }

    assert_eq!(store.writer_full_scan_count()?, scans_after_seed);
    Ok(())
}

#[test]
fn first_hot_append_seeds_a_new_stream_projection_without_a_second_scan() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());

    for ordinal in 0..64 {
        session.append_control(ControlEntry::Note {
            kind: "first_hot_append".to_owned(),
            data: serde_json::json!({"ordinal": ordinal}),
        })?;
    }

    assert_eq!(store.writer_full_scan_count()?, 1);
    assert_eq!(
        session
            .active_projection_snapshot()?
            .expect("store-backed session has a projection")
            .durable_session_entry_count(),
        64
    );
    assert_eq!(store.writer_full_scan_count()?, 1);
    Ok(())
}

#[test]
fn session_startup_reuses_the_single_reconciled_full_replay() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("startup.jsonl"))?;
    append_current_test_session_identity(&store)?;
    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "startup_fixture".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    let scans_before_load = store.writer_full_scan_count()?;

    let loaded = Session::load_from_store("mock", "model", store.clone())?;

    assert!(loaded.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::Note { kind, .. })
                if kind == "startup_fixture"
        )
    }));
    assert_eq!(
        store
            .writer_full_scan_count()?
            .saturating_sub(scans_before_load),
        1
    );
    Ok(())
}

#[test]
fn normal_queue_and_task_guidance_authority_appends_do_not_rescan_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("authority-cas.jsonl"))?;
    store.active_projection_snapshot()?;
    let scans_after_seed = store.writer_full_scan_count()?;
    let task_id = TaskId::new("authority_cas_task")?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskRun(
        TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")?,
            objective: "exercise active authority CAS".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        },
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskPlan(
        TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        },
    )))?;
    let queue_id = ConversationInputQueueId::new("authority_cas_queue")?;
    let prompt = crate::project_conversation_prompt_for_persistence("apply active CAS");
    let receipt = store.append_conversation_queue_mutation(ConversationQueueMutationCommand {
        expected_queue_revision: ConversationQueueRevision::initial(),
        mutation: ConversationQueueMutation::Enqueue {
            entry: ConversationInputQueuedEntry {
                queue_id: queue_id.clone(),
                target: ConversationInputTarget::Task {
                    task_id: task_id.clone(),
                },
                kind: ConversationInputKind::TaskGuidance,
                prompt_hash: prompt.prompt_hash.clone(),
                prompt: prompt.safe_prompt.clone(),
                reasoning_effort: None,
                created_at_ms: Some(1),
            },
        },
    })?;
    let frontier = store.active_projection_snapshot()?.frontier().clone();
    store.append_task_guidance_promoted_at(
        TaskGuidancePromotedEntry {
            queue_id: queue_id.clone(),
            expected_queue_revision: receipt.revision,
            task_id,
            plan_version: 1,
            source_turn: ConversationTurnRef::new(
                frontier.session_id(),
                "authority-cas-message",
                "authority-cas-run",
            )?,
            prompt_hash: prompt.prompt_hash,
            exact_prompt_required: prompt.exact_prompt_required,
            guidance: prompt.safe_prompt,
            dispatch_run_id: "authority-cas-dispatch".to_owned(),
            promoted_at_ms: 2,
        },
        &frontier,
    )?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Delivered,
            reason: None,
            updated_at_ms: Some(3),
        }),
    ))?;

    assert_eq!(store.writer_full_scan_count()?, scans_after_seed);
    Ok(())
}

#[test]
fn incremental_projection_matches_full_prefix_rebuild() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.active_projection_snapshot()?;
    let task_id = TaskId::new("mixed_projection_task")?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskRun(
        TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")?,
            objective: "mixed reducer equivalence".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        },
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskPlan(
        TaskPlanEntry {
            task_id,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        },
    )))?;
    let thread_id = AgentThreadId::new("mixed_projection_agent")?;
    for status in [
        AgentResultContinuationStatus::Pending,
        AgentResultContinuationStatus::Completed,
    ] {
        store.append(&SessionLogEntry::Control(
            ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                thread_id: thread_id.clone(),
                status,
                reason: None,
                updated_at_ms: Some(1),
            }),
        ))?;
    }
    let queue_id = ConversationInputQueueId::new("mixed_projection_queue")?;
    let prompt = crate::project_conversation_prompt_for_persistence("mixed queued input");
    let receipt = store.append_conversation_queue_mutation(ConversationQueueMutationCommand {
        expected_queue_revision: ConversationQueueRevision::initial(),
        mutation: ConversationQueueMutation::Enqueue {
            entry: ConversationInputQueuedEntry {
                queue_id: queue_id.clone(),
                target: ConversationInputTarget::MainThread,
                kind: ConversationInputKind::Chat,
                prompt_hash: prompt.prompt_hash,
                prompt: prompt.safe_prompt,
                reasoning_effort: None,
                created_at_ms: Some(1),
            },
        },
    })?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Cancelled,
            reason: None,
            updated_at_ms: Some(2),
        }),
    ))?;
    store.append_compaction_started(CompactionStartedEntry {
        attempt_id: "mixed-projection-compaction".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: receipt.revision.event_id,
        started_at_unix_ms: 3,
    })?;
    store.append_compaction_failed(CompactionFailureEntry {
        attempt_id: "mixed-projection-compaction".to_owned(),
        reason: CompactionFailureReason::ValidationFailed,
        failed_at_unix_ms: 4,
    })?;

    let published = store.active_projection_snapshot()?;
    let records = store.read_event_records_writer()?;
    let rebuilt = ActiveSessionProjection::from_records(&records, published.frontier().clone())?;

    for chunk_size in 1..=records.len() {
        let first_end = chunk_size.min(records.len());
        let mut incremental = ActiveSessionProjection::from_records(
            &records[..first_end],
            active_frontier(&records[..first_end]),
        )?;
        let mut applied_end = first_end;
        while applied_end < records.len() {
            let next_end = applied_end.saturating_add(chunk_size).min(records.len());
            incremental.apply_records(
                &records[applied_end..next_end],
                active_frontier(&records[..next_end]),
            )?;
            applied_end = next_end;
        }
        assert_eq!(incremental.cursor, rebuilt.cursor);
        assert_eq!(incremental.queue, rebuilt.queue);
        assert_eq!(incremental.task_guidance, rebuilt.task_guidance);
        assert_eq!(
            incremental.recent_terminal_tasks,
            rebuilt.recent_terminal_tasks
        );
        assert_eq!(incremental.compaction, rebuilt.compaction);
        assert_eq!(
            incremental.pending_agent_continuations,
            rebuilt.pending_agent_continuations
        );
        assert_eq!(
            incremental.active_terminal_tasks,
            rebuilt.active_terminal_tasks
        );
        assert_eq!(
            serde_json::to_value(&incremental.usage)?,
            serde_json::to_value(&rebuilt.usage)?
        );
        assert_eq!(incremental.latest_readiness, rebuilt.latest_readiness);
        assert_eq!(
            incremental.durable_session_entry_count,
            rebuilt.durable_session_entry_count
        );
    }
    Ok(())
}

#[test]
fn exhausted_session_locks_return_typed_busy_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("busy-session.jsonl");
    let owner = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    owner.lock_exclusive()?;
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)?;

    let read_error = super::super::store::lock_shared_with_retry(&contender, &path)
        .expect_err("an exhausted reader lock must be typed");
    let busy = read_error
        .downcast_ref::<SessionIoBusyError>()
        .expect("reader lock contention is downcastable");
    assert_eq!(busy.kind, SessionIoBusyKind::Reader);
    assert_eq!(busy.path, path);

    let write_error = super::super::store::lock_exclusive_with_retry(&contender, &busy.path)
        .expect_err("an exhausted writer lock must be typed");
    let busy = write_error
        .downcast_ref::<SessionIoBusyError>()
        .expect("writer lock contention is downcastable");
    assert_eq!(busy.kind, SessionIoBusyKind::Writer);
    owner.unlock()?;
    Ok(())
}

#[test]
fn durable_append_succeeds_when_incremental_reducer_invalidates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.active_projection_snapshot()?;
    let rebuilds_before = store.active_projection_metrics().full_rebuild_total;
    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "before_invalid".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    store.inject_active_projection_schema_mismatch()?;
    let recorder = Arc::new(NoticeRecorder::default());
    let _subscription = store.register_active_projection_observer(recorder.clone());

    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "durable_despite_invalid_reducer".to_owned(),
        data: serde_json::Value::Null,
    }))?;

    let notices = recorder
        .notices
        .lock()
        .expect("notice recorder lock is available");
    assert_eq!(notices.len(), 1);
    assert!(!notices[0].valid);
    drop(notices);
    assert_eq!(store.read_event_records_writer()?.len(), 2);
    assert_eq!(
        store
            .active_projection_snapshot()?
            .frontier()
            .cursor()
            .expect("rebuilt projection has a cursor")
            .last_applied_stream_sequence,
        2
    );
    assert_eq!(
        store
            .active_projection_metrics()
            .full_rebuild_total
            .saturating_sub(rebuilds_before),
        1
    );
    Ok(())
}

#[test]
fn session_facade_exposes_projection_only_for_store_backed_sessions() -> Result<()> {
    let in_memory = Session::new("mock", "model");
    assert!(in_memory.active_projection_snapshot()?.is_none());
    assert!(
        in_memory
            .register_active_projection_observer(Arc::new(NoticeRecorder::default()))?
            .is_none()
    );

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut durable = Session::new("mock", "model").with_store(store);
    let initial = durable
        .active_projection_snapshot()?
        .expect("store-backed session has an active projection");
    let recorder = Arc::new(NoticeRecorder::default());
    let _subscription = durable
        .register_active_projection_observer(recorder.clone())?
        .expect("store-backed session registers an observer");

    durable.append_control(ControlEntry::Note {
        kind: "facade_projection".to_owned(),
        data: serde_json::Value::Null,
    })?;
    let updated = durable
        .active_projection_snapshot()?
        .expect("store-backed session retains an active projection");
    assert_ne!(initial.frontier(), updated.frontier());
    assert_eq!(
        recorder
            .notices
            .lock()
            .expect("notice recorder lock is available")
            .len(),
        1
    );
    Ok(())
}

#[test]
#[ignore = "release-profile long-session evidence"]
fn active_projection_long_session_evidence() -> Result<()> {
    const EVENT_COUNT: u64 = 10_000;
    const QUEUE_CYCLES: u64 = 100;

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("active-projection-10k.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let initial = store.active_projection_snapshot()?;
    let full_scans_after_seed = store.writer_full_scan_count()?;
    assert_eq!(full_scans_after_seed, 1);
    let io_locks_after_seed = crate::session_io_lock_metrics();
    let recorder = Arc::new(NoticeRecorder::default());
    let _subscription = store.register_active_projection_observer(recorder.clone());
    let started = Instant::now();

    let task_id = TaskId::new("active_projection_long_task")?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskRun(
        TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")?,
            objective: "exercise mixed active projection reducers".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        },
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskPlan(
        TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        },
    )))?;
    for ordinal in 0..QUEUE_CYCLES {
        let queue_id = ConversationInputQueueId::new(format!("evidence-guidance-{ordinal}"))?;
        let prompt = crate::project_conversation_prompt_for_persistence(&format!(
            "bounded guidance {ordinal}"
        ));
        let receipt =
            store.append_conversation_queue_mutation(ConversationQueueMutationCommand {
                expected_queue_revision: store
                    .active_projection_snapshot()?
                    .conversation_queue()
                    .current_revision(),
                mutation: ConversationQueueMutation::Enqueue {
                    entry: ConversationInputQueuedEntry {
                        queue_id: queue_id.clone(),
                        target: ConversationInputTarget::Task {
                            task_id: task_id.clone(),
                        },
                        kind: ConversationInputKind::TaskGuidance,
                        prompt_hash: prompt.prompt_hash.clone(),
                        prompt: prompt.safe_prompt.clone(),
                        reasoning_effort: None,
                        created_at_ms: Some(ordinal.saturating_add(1)),
                    },
                },
            })?;
        let frontier = store.active_projection_snapshot()?.frontier().clone();
        store.append_task_guidance_promoted_at(
            TaskGuidancePromotedEntry {
                queue_id: queue_id.clone(),
                expected_queue_revision: receipt.revision,
                task_id: task_id.clone(),
                plan_version: 1,
                source_turn: ConversationTurnRef::new(
                    frontier.session_id(),
                    format!("evidence-message-{ordinal}"),
                    format!("evidence-run-{ordinal}"),
                )?,
                prompt_hash: prompt.prompt_hash,
                exact_prompt_required: prompt.exact_prompt_required,
                guidance: prompt.safe_prompt,
                dispatch_run_id: format!("evidence-dispatch-{ordinal}"),
                promoted_at_ms: ordinal.saturating_add(1),
            },
            &frontier,
        )?;
        store.append(&SessionLogEntry::Control(
            ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
                queue_id,
                status: ConversationInputStatus::Delivered,
                reason: Some("evidence cycle complete".to_owned()),
                updated_at_ms: Some(ordinal.saturating_add(1)),
            }),
        ))?;
    }
    let emitted_before_plan_updates = 2_u64.saturating_add(QUEUE_CYCLES.saturating_mul(3));
    let remaining_plan_updates = EVENT_COUNT.saturating_sub(emitted_before_plan_updates);
    for ordinal in 0..remaining_plan_updates {
        store.append(&SessionLogEntry::Control(ControlEntry::TaskPlan(
            TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: u32::try_from(ordinal.saturating_add(2))?,
                status: TaskPlanStatus::Accepted,
                steps: Vec::new(),
                reason: None,
            },
        )))?;
    }

    let snapshot = store.active_projection_snapshot()?;
    let metrics = store.active_projection_metrics();
    let io_lock_delta = crate::session_io_lock_metrics().saturating_delta(io_locks_after_seed);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let full_scan_delta = store
        .writer_full_scan_count()?
        .saturating_sub(full_scans_after_seed);
    let notices = recorder
        .notices
        .lock()
        .expect("notice recorder lock is available");
    let wake_count = notices.len() as u64;
    let changed_family_count = notices
        .iter()
        .map(|notice| notice.changed_families.len() as u64)
        .sum::<u64>();

    assert_eq!(initial.frontier().cursor(), None);
    assert_eq!(full_scan_delta, 0);
    assert_eq!(wake_count, EVENT_COUNT);
    assert!(
        changed_family_count >= EVENT_COUNT,
        "mixed authority append evidence must exercise typed reducer families"
    );
    assert_eq!(
        snapshot
            .frontier()
            .cursor()
            .expect("10k projection has a durable cursor")
            .last_applied_stream_sequence,
        EVENT_COUNT
    );
    assert_eq!(snapshot.durable_session_entry_count(), EVENT_COUNT);
    assert_eq!(metrics.incremental_apply_total, EVENT_COUNT);
    assert_eq!(metrics.publication_total, EVENT_COUNT);
    assert!(io_lock_delta.exclusive_lock_attempt_total >= EVENT_COUNT);
    assert_eq!(io_lock_delta.contention_total, 0);
    assert_eq!(io_lock_delta.failure_total, 0);

    let invalidation_store = JsonlSessionStore::new(temp.path().join("forced-invalidation.jsonl"))?;
    invalidation_store.active_projection_snapshot()?;
    invalidation_store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "before_forced_invalidation".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    let rebuilds_before = invalidation_store
        .active_projection_metrics()
        .full_rebuild_total;
    invalidation_store.inject_active_projection_schema_mismatch()?;
    invalidation_store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "forced_invalidation".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    assert_eq!(
        invalidation_store
            .active_projection_metrics()
            .invalidation_total,
        1
    );
    invalidation_store.active_projection_snapshot()?;
    let forced_invalidation_rebuild_count = invalidation_store
        .active_projection_metrics()
        .full_rebuild_total
        .saturating_sub(rebuilds_before);
    assert_eq!(forced_invalidation_rebuild_count, 1);

    println!(
        "SIGIL_LONG_SESSION_EVIDENCE {}",
        serde_json::json!({
            "schema_version": 1,
            "scenario": "active_projection_10k",
            "scale": EVENT_COUNT,
            "elapsed_ms": elapsed_ms,
            "facts": {
                "startup_full_scan_count": full_scans_after_seed,
                "steady_state_full_scan_count": full_scan_delta,
                "incremental_append_count": EVENT_COUNT,
                "projection_notice_count": wake_count,
                "changed_family_count": changed_family_count,
                "durable_session_entry_count": snapshot.durable_session_entry_count(),
                "durable_bytes": std::fs::metadata(path)?.len(),
                "projection_estimated_bytes": snapshot.approximate_memory_bytes(),
                "snapshot_count": metrics.snapshot_total,
                "full_rebuild_count": metrics.full_rebuild_total,
                "incremental_apply_count": metrics.incremental_apply_total,
                "invalidation_count": metrics.invalidation_total,
                "coordinator_writer_lock_attempt_count": metrics.writer_lock_attempt_total,
                "os_shared_lock_attempt_count": io_lock_delta.shared_lock_attempt_total,
                "os_exclusive_lock_attempt_count": io_lock_delta.exclusive_lock_attempt_total,
                "os_lock_contention_count": io_lock_delta.contention_total,
                "os_lock_failure_count": io_lock_delta.failure_total,
                "forced_invalidation_rebuild_count": forced_invalidation_rebuild_count,
            }
        })
    );
    Ok(())
}

#[test]
#[ignore = "requires SIGIL_R58_LIVE_SESSION and writes only to a temporary byte-for-byte copy"]
fn latest_session_copy_active_projection_smoke() -> Result<()> {
    const SNAPSHOT_COUNT: u64 = 10_000;

    let source_path = std::env::var_os("SIGIL_R58_LIVE_SESSION")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("SIGIL_R58_LIVE_SESSION is required"))?;
    let source_metadata = std::fs::metadata(&source_path)?;
    let temp = tempfile::tempdir()?;
    let copied_path = temp.path().join("latest-session-copy.jsonl");
    let copied_bytes = std::fs::copy(&source_path, &copied_path)?;
    if copied_bytes != source_metadata.len() {
        anyhow::bail!("live session copy length changed during the smoke test");
    }

    let store = JsonlSessionStore::new(&copied_path)?;
    let initial = store.active_projection_snapshot()?;
    let full_scans_after_seed = store.writer_full_scan_count()?;
    if full_scans_after_seed != 1 {
        anyhow::bail!(
            "live session projection startup scanned {} times instead of once",
            full_scans_after_seed
        );
    }
    let recorder = Arc::new(NoticeRecorder::default());
    let _subscription = store.register_active_projection_observer(recorder.clone());
    let snapshot_started = Instant::now();
    for _ in 0..SNAPSHOT_COUNT {
        store.active_projection_snapshot()?;
    }
    let snapshot_elapsed_ms = snapshot_started.elapsed().as_millis() as u64;
    let steady_state_full_scan_count = store
        .writer_full_scan_count()?
        .saturating_sub(full_scans_after_seed);
    if steady_state_full_scan_count != 0 {
        anyhow::bail!("ready active projection unexpectedly rescanned the copied session");
    }

    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "rfc_0058_latest_session_copy_smoke".to_owned(),
        data: serde_json::json!({ "source_bytes": source_metadata.len() }),
    }))?;
    let updated = store.active_projection_snapshot()?;
    if updated.durable_session_entry_count()
        != initial.durable_session_entry_count().saturating_add(1)
    {
        anyhow::bail!("temporary durable append did not advance the session-entry frontier once");
    }
    let notices = recorder
        .notices
        .lock()
        .expect("notice recorder lock is available");
    if notices.len() != 1 {
        anyhow::bail!(
            "temporary durable append produced {} projection notices instead of one",
            notices.len()
        );
    }

    println!(
        "SIGIL_R58_LIVE_SMOKE {}",
        serde_json::json!({
            "schema_version": 1,
            "scenario": "latest_session_copy_active_projection",
            "source_bytes": source_metadata.len(),
            "source_session_entry_count": initial.durable_session_entry_count(),
            "startup_full_scan_count": full_scans_after_seed,
            "snapshot_count": SNAPSHOT_COUNT,
            "snapshot_elapsed_ms": snapshot_elapsed_ms,
            "steady_state_full_scan_count": steady_state_full_scan_count,
            "durable_append_count": 1,
            "projection_notice_count": notices.len(),
        })
    );
    Ok(())
}

#[test]
fn task_guidance_snapshot_exposes_minimal_plan_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store);
    let task_id = TaskId::new("active_projection_task")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "bounded task state".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 7,
        status: TaskPlanStatus::Accepted,
        steps: Vec::new(),
        reason: None,
    }))?;

    let snapshot = session
        .active_projection_snapshot()?
        .expect("store-backed session has a projection");
    let task = snapshot
        .task_guidance_state(&task_id)
        .expect("task guidance state is projected");
    assert_eq!(task.status(), TaskRunStatus::Paused);
    assert_eq!(task.latest_plan_version(), Some(7));
    assert_eq!(task.accepted_plan_version(), Some(7));
    Ok(())
}

#[test]
fn stable_compaction_snapshot_requires_exact_live_entry_frontier() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_control(ControlEntry::Note {
        kind: "stable_entry".to_owned(),
        data: serde_json::Value::Null,
    })?;
    let frontier = session
        .active_projection_snapshot()?
        .expect("store-backed session has a projection")
        .frontier()
        .clone();
    let stable = session
        .stable_compaction_snapshot(&frontier)?
        .expect("live entries exactly reach the durable frontier");
    assert_eq!(stable.frontier(), &frontier);
    assert_eq!(stable.entries().len(), 1);
    let mut materialized = stable
        .materialize_compaction_session()?
        .expect("unchanged snapshot materializes a store-backed session");
    assert_eq!(materialized.session_scope_id(), session.session_scope_id());
    materialized.append_control(ControlEntry::Note {
        kind: "materialized_append".to_owned(),
        data: serde_json::Value::Null,
    })?;
    assert_eq!(store.read_event_records_writer()?.len(), 2);
    assert!(stable.materialize_compaction_session()?.is_none());
    session.record_durably_appended_control(ControlEntry::Note {
        kind: "materialized_append".to_owned(),
        data: serde_json::Value::Null,
    });
    let materialized_frontier = store.active_projection_snapshot()?.frontier().clone();
    assert!(
        session
            .stable_compaction_snapshot(&materialized_frontier)?
            .is_some()
    );

    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "external_append".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    let externally_advanced = store.active_projection_snapshot()?.frontier().clone();
    assert!(
        session
            .stable_compaction_snapshot(&externally_advanced)?
            .is_none()
    );

    session.record_durably_appended_control(ControlEntry::Note {
        kind: "external_append".to_owned(),
        data: serde_json::Value::Null,
    });
    assert!(
        session
            .stable_compaction_snapshot(&externally_advanced)?
            .is_some()
    );
    Ok(())
}

#[test]
fn stable_snapshot_recovers_after_active_run_adopts_detached_controls() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_control(ControlEntry::Note {
        kind: "run_started".to_owned(),
        data: serde_json::Value::Null,
    })?;

    let first_detached = ControlEntry::Note {
        kind: "queued_while_running".to_owned(),
        data: serde_json::json!({"ordinal": 1}),
    };
    let second_detached = ControlEntry::Note {
        kind: "queued_while_running".to_owned(),
        data: serde_json::json!({"ordinal": 2}),
    };
    store.append(&SessionLogEntry::Control(first_detached.clone()))?;
    store.append(&SessionLogEntry::Control(second_detached.clone()))?;

    // The active run continues to append through its live Session after the detached queue path
    // advanced the durable stream. Its consumed-entry counter must remain recoverable, but the
    // session is not stable until canonical reconciliation adopts the detached durable suffix.
    session.append_control(ControlEntry::Note {
        kind: "run_finished".to_owned(),
        data: serde_json::Value::Null,
    })?;
    let frontier = store.active_projection_snapshot()?.frontier().clone();
    assert!(session.stable_compaction_snapshot(&frontier)?.is_none());

    session.record_durably_appended_controls([first_detached]);
    let stable = session
        .stable_compaction_snapshot(&frontier)?
        .expect("canonical adoption rebuilds the exact durable live-entry order");
    let note_kinds = stable
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::Note { kind, .. }) => Some(kind.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        note_kinds,
        [
            "run_started",
            "queued_while_running",
            "queued_while_running",
            "run_finished"
        ]
    );
    // A later duplicate receipt is idempotent and cannot duplicate or reorder the durable suffix.
    session.record_durably_appended_controls([second_detached]);
    let restabilized = session
        .stable_compaction_snapshot(&frontier)?
        .expect("duplicate detached receipt remains stable");
    assert_eq!(
        serde_json::to_value(restabilized.entries())?,
        serde_json::to_value(stable.entries())?
    );
    Ok(())
}

#[test]
fn stable_snapshot_rejects_reversed_detached_control_adoption() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    session.append_control(ControlEntry::Note {
        kind: "prefix".to_owned(),
        data: serde_json::Value::Null,
    })?;
    let first = ControlEntry::Note {
        kind: "detached".to_owned(),
        data: serde_json::json!({"ordinal": 1}),
    };
    let second = ControlEntry::Note {
        kind: "detached".to_owned(),
        data: serde_json::json!({"ordinal": 2}),
    };
    store.append(&SessionLogEntry::Control(first.clone()))?;
    store.append(&SessionLogEntry::Control(second.clone()))?;
    let frontier = store.active_projection_snapshot()?.frontier().clone();

    session.record_durably_appended_controls([second, first]);

    assert!(
        session.stable_compaction_snapshot(&frontier)?.is_none(),
        "count equality alone must not authorize a snapshot after reversed adoption"
    );
    Ok(())
}

#[test]
fn terminal_task_history_does_not_exhaust_active_guidance_capacity() -> Result<()> {
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    let mut records = Vec::new();
    let mut sequence = 1_u64;
    for ordinal in 0..=MAX_ACTIVE_TASK_GUIDANCE_STATES {
        let task_id = TaskId::new(format!("completed_task_{ordinal}"))?;
        for status in [TaskRunStatus::Started, TaskRunStatus::Completed] {
            records.push(session_entry_record(
                sequence,
                SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
                    task_id: task_id.clone(),
                    parent_session_ref: parent_session_ref.clone(),
                    objective: "terminal history".to_owned(),
                    title: None,
                    status,
                    reason: None,
                })),
            )?);
            sequence = sequence.saturating_add(1);
        }
    }
    let live_task_id = TaskId::new("task_after_large_terminal_history")?;
    records.push(session_entry_record(
        sequence,
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: live_task_id.clone(),
            parent_session_ref,
            objective: "still admitted".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
    )?);
    let last = records
        .last()
        .expect("terminal history produces a final record");
    let frontier = ActiveProjectionFrontier {
        writer_generation: "test-generation".to_owned(),
        session_id: last.session_id().to_owned(),
        durable_end_offset: 0,
        cursor: Some(last.projection_cursor(ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION)),
    };
    let projection = ActiveSessionProjection::from_records(&records, frontier)?;
    assert!(projection.task_guidance.contains_key(&live_task_id));
    assert_eq!(projection.task_guidance.len(), 1);
    Ok(())
}

#[test]
fn active_guidance_capacity_keeps_durable_append_authoritative() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("active-guidance-overflow.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.active_projection_snapshot()?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    let entries = (0..=MAX_ACTIVE_TASK_GUIDANCE_STATES)
        .map(|ordinal| {
            Ok(SessionLogEntry::Control(ControlEntry::TaskRun(
                TaskRunEntry {
                    task_id: TaskId::new(format!("active_task_{ordinal}"))?,
                    parent_session_ref: parent_session_ref.clone(),
                    objective: "bounded active task guidance".to_owned(),
                    title: None,
                    status: TaskRunStatus::Paused,
                    reason: None,
                },
            )))
        })
        .collect::<Result<Vec<_>>>()?;
    store.append_session_entry_events(&entries)?;
    let snapshot = store.active_projection_snapshot()?;
    assert!(snapshot.task_guidance_may_be_incomplete());
    assert_eq!(
        snapshot.projection.task_guidance.len(),
        MAX_ACTIVE_TASK_GUIDANCE_STATES
    );
    assert_eq!(
        JsonlSessionStore::read_event_records(&path)?.len(),
        MAX_ACTIVE_TASK_GUIDANCE_STATES + 1
    );
    assert_eq!(
        snapshot
            .frontier()
            .cursor()
            .expect("overflow append advances the durable frontier")
            .last_applied_stream_sequence,
        (MAX_ACTIVE_TASK_GUIDANCE_STATES + 1) as u64
    );
    Ok(())
}

#[test]
fn active_task_plan_decision_matches_canonical_projection() -> Result<()> {
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    for decision in [TaskPlanStatus::Rejected, TaskPlanStatus::Proposed] {
        let task_id = TaskId::new(format!("plan_decision_{decision:?}"))?;
        let entries = vec![
            SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: "keep active task guidance exact".to_owned(),
                title: None,
                status: TaskRunStatus::Paused,
                reason: None,
            })),
            SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                status: TaskPlanStatus::Accepted,
                steps: Vec::new(),
                reason: None,
            })),
            SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                status: decision,
                steps: Vec::new(),
                reason: None,
            })),
        ];
        let records = entries
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, entry)| session_entry_record(index as u64 + 1, entry))
            .collect::<Result<Vec<_>>>()?;
        let mut incremental =
            ActiveSessionProjection::from_records(&records[..2], active_frontier(&records[..2]))?;
        incremental.apply_records(&records[2..], active_frontier(&records))?;
        let rebuilt = ActiveSessionProjection::from_records(&records, active_frontier(&records))?;
        let canonical = TaskStateProjection::from_entries(&entries);
        let canonical_task = canonical
            .tasks
            .get(&task_id)
            .expect("canonical task projection retains the task");
        let canonical_accepted = canonical_task.latest_plan_version.filter(|plan_version| {
            canonical_task
                .plans
                .get(plan_version)
                .is_some_and(|plan| plan.status == TaskPlanStatus::Accepted)
        });
        let incremental_task = incremental
            .task_guidance
            .get(&task_id)
            .expect("incremental active projection retains the task hint");
        let rebuilt_task = rebuilt
            .task_guidance
            .get(&task_id)
            .expect("rebuilt active projection retains the task hint");

        assert_eq!(canonical_accepted, None);
        assert_eq!(incremental_task.accepted_plan_version(), canonical_accepted);
        assert_eq!(incremental_task, rebuilt_task);
    }
    Ok(())
}

#[test]
fn recently_terminal_task_ignores_stale_non_final_status() -> Result<()> {
    let task_id = TaskId::new("recently_completed_task")?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: "remain final".to_owned(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: "remain final".to_owned(),
            title: None,
            status: TaskRunStatus::Completed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref,
            objective: "stale reopen".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
    ];
    let records = entries
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, entry)| session_entry_record(index as u64 + 1, entry))
        .collect::<Result<Vec<_>>>()?;
    let mut incremental =
        ActiveSessionProjection::from_records(&records[..2], active_frontier(&records[..2]))?;
    incremental.apply_records(&records[2..], active_frontier(&records))?;
    let rebuilt = ActiveSessionProjection::from_records(&records, active_frontier(&records))?;
    let canonical = TaskStateProjection::from_entries(&entries);

    assert_eq!(
        canonical
            .tasks
            .get(&task_id)
            .expect("canonical projection retains the task")
            .status,
        TaskRunStatus::Completed
    );
    assert!(!incremental.task_guidance.contains_key(&task_id));
    assert_eq!(
        incremental.recent_terminal_tasks,
        rebuilt.recent_terminal_tasks
    );
    Ok(())
}

#[test]
fn final_task_guidance_cas_rejects_reopen_after_terminal_hint_eviction() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("terminal-history-cas.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.active_projection_snapshot()?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    let stale_task_id = TaskId::new("completed_task_0")?;
    let mut terminal_history = Vec::new();
    for ordinal in 0..=MAX_RECENT_TERMINAL_TASK_IDS {
        let task_id = TaskId::new(format!("completed_task_{ordinal}"))?;
        for status in [TaskRunStatus::Started, TaskRunStatus::Completed] {
            terminal_history.push(SessionLogEntry::Control(ControlEntry::TaskRun(
                TaskRunEntry {
                    task_id: task_id.clone(),
                    parent_session_ref: parent_session_ref.clone(),
                    objective: "terminal history".to_owned(),
                    title: None,
                    status,
                    reason: None,
                },
            )));
        }
    }
    store.append_session_entry_events(&terminal_history)?;
    assert!(
        !store
            .active_projection_snapshot()?
            .is_recent_terminal_task(&stale_task_id),
        "the oldest terminal task must be outside the bounded active hint"
    );

    store.append_session_entry_events(&[
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: stale_task_id.clone(),
            parent_session_ref,
            objective: "stale reopen".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: stale_task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        })),
    ])?;
    let active = store.active_projection_snapshot()?;
    let active_task = active
        .task_guidance_state(&stale_task_id)
        .expect("bounded active hint may treat an evicted task id as live");
    assert_eq!(active_task.status(), TaskRunStatus::Running);
    assert_eq!(active_task.accepted_plan_version(), Some(1));

    let prompt =
        crate::project_conversation_prompt_for_persistence("do not revive the completed task");
    let queue_id = ConversationInputQueueId::new("evicted-terminal-guidance")?;
    let receipt = store.append_conversation_queue_mutation(ConversationQueueMutationCommand {
        expected_queue_revision: ConversationQueueRevision::initial(),
        mutation: ConversationQueueMutation::Enqueue {
            entry: ConversationInputQueuedEntry {
                queue_id: queue_id.clone(),
                target: ConversationInputTarget::Task {
                    task_id: stale_task_id.clone(),
                },
                kind: ConversationInputKind::TaskGuidance,
                prompt_hash: prompt.prompt_hash.clone(),
                prompt: prompt.safe_prompt.clone(),
                reasoning_effort: None,
                created_at_ms: Some(1),
            },
        },
    })?;
    let frontier = store.active_projection_snapshot()?.frontier().clone();
    let promotion = TaskGuidancePromotedEntry {
        queue_id,
        expected_queue_revision: receipt.revision,
        task_id: stale_task_id,
        plan_version: 1,
        source_turn: ConversationTurnRef::new(
            frontier.session_id(),
            "evicted-terminal-message",
            "evicted-terminal-run",
        )?,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: prompt.exact_prompt_required,
        guidance: prompt.safe_prompt,
        dispatch_run_id: "evicted-terminal-run".to_owned(),
        promoted_at_ms: 1,
    };
    let before = std::fs::read(&path)?;
    let error = store
        .append_task_guidance_promoted_at(promotion, &frontier)
        .expect_err("canonical final CAS must reject an evicted completed task");

    assert!(format!("{error:#}").contains("completed or cancelled task"));
    assert_eq!(std::fs::read(path)?, before);
    Ok(())
}

fn session_entry_record(
    stream_sequence: u64,
    entry: SessionLogEntry,
) -> Result<SessionStreamRecord> {
    let event_type = super::super::store::session_entry_event_type(&entry);
    let event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known session entry event has a class"),
        format!("session-entry-{stream_sequence}"),
        "active-session-entry-history".to_owned(),
        stream_sequence,
        serde_json::json!({ "session_log_entry": entry }),
    )?;
    Ok(SessionStreamRecord::Stored(event))
}

#[test]
fn terminal_compaction_history_retains_only_bounded_summary() -> Result<()> {
    let mut summary = ActiveCompactionSummary::default();
    let mut sequence = 1_u64;
    for ordinal in 0..600 {
        let attempt_id = format!("attempt-{ordinal}");
        let started_event_id = format!("started-{ordinal}");
        let started = CompactionStartedEntry {
            attempt_id: attempt_id.clone(),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: CompactionInitiation::IdleAutomatic {
                scope_fingerprint: format!("scope-{ordinal}"),
                circuit_scope: Some(CompactionCircuitScopeV1 {
                    source_cursor_event_id: format!("source-{ordinal}"),
                    layout_hash: format!("layout-{ordinal}"),
                    route_fingerprint: "route".to_owned(),
                }),
            },
            base_projection_revision: format!("revision-{ordinal}"),
            started_at_unix_ms: sequence,
        };
        let started_event = compaction_event(
            DurableEventType::CompactionStarted,
            &started_event_id,
            sequence,
            serde_json::to_value(started)?,
            Some(&started_event_id),
            None,
        )?;
        summary.apply_record(&SessionStreamRecord::Stored(started_event))?;
        sequence = sequence.saturating_add(1);

        let failed = CompactionFailureEntry {
            attempt_id,
            reason: CompactionFailureReason::ValidationFailed,
            failed_at_unix_ms: sequence,
        };
        let failed_event = compaction_event(
            DurableEventType::CompactionFailed,
            &format!("failed-{ordinal}"),
            sequence,
            serde_json::to_value(failed)?,
            Some(&started_event_id),
            Some(&started_event_id),
        )?;
        summary.apply_record(&SessionStreamRecord::Stored(failed_event))?;
        sequence = sequence.saturating_add(1);
    }
    assert_eq!(summary.open_attempt_count(), 0);
    assert_eq!(
        summary.recent_idle_attempts.len(),
        MAX_RECENT_IDLE_COMPACTION_ATTEMPTS
    );
    Ok(())
}

#[test]
fn active_compaction_decision_matches_canonical_lifecycle_projection() -> Result<()> {
    let mut records = Vec::new();
    for ordinal in 0..2_u64 {
        let attempt_id = format!("semantic-{ordinal}");
        let started_event_id = format!("semantic-started-{ordinal}");
        let started_sequence = ordinal.saturating_mul(2).saturating_add(1);
        let scope = CompactionCircuitScopeV1 {
            source_cursor_event_id: format!("source-{ordinal}"),
            layout_hash: format!("layout-{ordinal}"),
            route_fingerprint: "shared-route".to_owned(),
        };
        records.push(SessionStreamRecord::Stored(compaction_event(
            DurableEventType::CompactionStarted,
            &started_event_id,
            started_sequence,
            serde_json::to_value(CompactionStartedEntry {
                attempt_id: attempt_id.clone(),
                fallback_parent: CompactionFallbackParent::Root,
                initiation: CompactionInitiation::IdleAutomatic {
                    scope_fingerprint: format!("scope-{ordinal}"),
                    circuit_scope: Some(scope),
                },
                base_projection_revision: "projection-r1".to_owned(),
                started_at_unix_ms: started_sequence,
            })?,
            Some(&started_event_id),
            None,
        )?));
        records.push(SessionStreamRecord::Stored(compaction_event(
            DurableEventType::CompactionFailed,
            &format!("semantic-failed-{ordinal}"),
            started_sequence.saturating_add(1),
            serde_json::to_value(CompactionFailureEntry {
                attempt_id,
                reason: CompactionFailureReason::SemanticSummaryTimeout,
                failed_at_unix_ms: started_sequence.saturating_add(1),
            })?,
            Some(&started_event_id),
            Some(&started_event_id),
        )?));
    }
    let projection = ActiveSessionProjection::from_records(&records, active_frontier(&records))?;
    let canonical = CompactionLifecycleProjection::from_records(&records)?;
    let input = CompactionCircuitBreakerInputV1 {
        scope: CompactionCircuitScopeV1 {
            source_cursor_event_id: "current-source".to_owned(),
            layout_hash: "current-layout".to_owned(),
            route_fingerprint: "shared-route".to_owned(),
        },
        latest_completed_real_turn_sequence: None,
        emergency: false,
        post_activation_emergency_layer: None,
        manual_retry: false,
    };

    assert!(
        projection
            .compaction
            .has_failed_idle_automatic_scope("scope-1")
    );
    assert_eq!(
        projection.compaction.circuit_breaker_decision(&input)?,
        canonical.circuit_breaker_decision(&input)?
    );
    Ok(())
}

#[test]
fn idle_compaction_lifecycle_appends_keep_active_projection_ready() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.active_projection_snapshot()?;
    let scans_after_seed = store.writer_full_scan_count()?;
    let circuit_scope = CompactionCircuitScopeV1 {
        source_cursor_event_id: "source-cursor".to_owned(),
        layout_hash: "layout-hash".to_owned(),
        route_fingerprint: "route-fingerprint".to_owned(),
    };
    store.append_compaction_started(CompactionStartedEntry {
        attempt_id: "idle-failure".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::IdleAutomatic {
            scope_fingerprint: "failed-scope".to_owned(),
            circuit_scope: Some(circuit_scope.clone()),
        },
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    })?;
    store.append_compaction_failed(CompactionFailureEntry {
        attempt_id: "idle-failure".to_owned(),
        reason: CompactionFailureReason::ValidationFailed,
        failed_at_unix_ms: 2,
    })?;

    let scans_after_appends = store.writer_full_scan_count()?;
    let active = store.active_projection_snapshot()?;
    assert_eq!(scans_after_appends, scans_after_seed);
    assert_eq!(store.writer_full_scan_count()?, scans_after_appends);
    assert!(
        active
            .compaction()
            .has_failed_idle_automatic_scope("failed-scope")
    );
    assert_eq!(
        active
            .compaction()
            .circuit_breaker_decision(&CompactionCircuitBreakerInputV1 {
                scope: circuit_scope,
                latest_completed_real_turn_sequence: None,
                emergency: false,
                post_activation_emergency_layer: None,
                manual_retry: false,
            })?,
        CompactionCircuitBreakerDecisionV1::SameCursorAndLayoutFailed
    );
    Ok(())
}

#[test]
fn active_compaction_overflow_stays_bounded_and_canonically_validated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("compaction-overflow.jsonl"))?;
    store.active_projection_snapshot()?;
    for ordinal in 0..=MAX_OPEN_COMPACTION_ATTEMPTS {
        store.append_compaction_started(CompactionStartedEntry {
            attempt_id: format!("open-attempt-{ordinal}"),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: CompactionInitiation::Manual,
            base_projection_revision: format!("projection-{ordinal}"),
            started_at_unix_ms: ordinal as u64 + 1,
        })?;
    }

    let active = store.active_projection_snapshot()?;
    assert_eq!(
        active.compaction().open_attempt_count(),
        MAX_OPEN_COMPACTION_ATTEMPTS
    );
    assert_eq!(
        store.read_event_records_writer()?.len(),
        MAX_OPEN_COMPACTION_ATTEMPTS + 1
    );
    Ok(())
}

#[test]
fn compaction_overflow_delta_appends_then_invalidates_on_canonical_lineage_failure() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("compaction-overflow-invalid.jsonl"))?;
    store.active_projection_snapshot()?;
    for ordinal in 0..=MAX_OPEN_COMPACTION_ATTEMPTS {
        store.append_compaction_started(CompactionStartedEntry {
            attempt_id: format!("open-attempt-{ordinal}"),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: CompactionInitiation::Manual,
            base_projection_revision: format!("projection-{ordinal}"),
            started_at_unix_ms: ordinal as u64 + 1,
        })?;
    }
    let record_count_before = store.read_event_records_writer()?.len();
    store.append_event(
        DurableEventType::CompactionFailed,
        DurableEventType::CompactionFailed
            .expected_event_class()
            .expect("compaction failure has an event class"),
        serde_json::to_value(CompactionFailureEntry {
            attempt_id: "open-attempt-16".to_owned(),
            reason: CompactionFailureReason::ValidationFailed,
            failed_at_unix_ms: 99,
        })?,
    )?;

    assert_eq!(
        store.read_event_records_writer()?.len(),
        record_count_before + 1,
        "durable append remains authoritative even when projection validation fails"
    );
    assert!(
        store.active_projection_snapshot().is_err(),
        "canonical rebuild must fail closed on the forged terminal lineage"
    );
    assert_eq!(store.active_projection_metrics().invalidation_total, 1);
    Ok(())
}

#[test]
fn incremental_compaction_accepts_fallback_after_more_than_recent_window() -> Result<()> {
    let mut prefix = Vec::new();
    for ordinal in 0..=MAX_RECENT_IDLE_COMPACTION_ATTEMPTS {
        let attempt_id = format!("failed-{ordinal}");
        let started_event_id = format!("failed-start-{ordinal}");
        let started_sequence = (ordinal as u64).saturating_mul(2).saturating_add(1);
        prefix.push(SessionStreamRecord::Stored(compaction_event(
            DurableEventType::CompactionStarted,
            &started_event_id,
            started_sequence,
            serde_json::to_value(CompactionStartedEntry {
                attempt_id: attempt_id.clone(),
                fallback_parent: CompactionFallbackParent::Root,
                initiation: CompactionInitiation::Manual,
                base_projection_revision: "projection-r1".to_owned(),
                started_at_unix_ms: started_sequence,
            })?,
            Some(&started_event_id),
            None,
        )?));
        prefix.push(SessionStreamRecord::Stored(compaction_event(
            DurableEventType::CompactionFailed,
            &format!("failed-terminal-{ordinal}"),
            started_sequence.saturating_add(1),
            serde_json::to_value(CompactionFailureEntry {
                attempt_id,
                reason: CompactionFailureReason::ValidationFailed,
                failed_at_unix_ms: started_sequence.saturating_add(1),
            })?,
            Some(&started_event_id),
            Some(&started_event_id),
        )?));
    }
    let mut incremental = ActiveSessionProjection::from_records(&prefix, active_frontier(&prefix))?;
    let fallback_sequence = (prefix.len() as u64).saturating_add(1);
    let fallback_event_id = "fallback-after-window";
    let fallback = SessionStreamRecord::Stored(compaction_event(
        DurableEventType::CompactionStarted,
        fallback_event_id,
        fallback_sequence,
        serde_json::to_value(CompactionStartedEntry {
            attempt_id: "fallback-attempt".to_owned(),
            fallback_parent: CompactionFallbackParent::InitiatedAttempt {
                attempt_id: "failed-0".to_owned(),
            },
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "projection-r1".to_owned(),
            started_at_unix_ms: fallback_sequence,
        })?,
        Some(fallback_event_id),
        None,
    )?);
    let mut full_records = prefix;
    full_records.push(fallback.clone());

    incremental.apply_records(
        std::slice::from_ref(&fallback),
        active_frontier(&full_records),
    )?;
    let rebuilt =
        ActiveSessionProjection::from_records(&full_records, active_frontier(&full_records))?;
    assert_eq!(
        incremental.compaction.open_attempt_count(),
        rebuilt.compaction.open_attempt_count()
    );
    assert_eq!(incremental.cursor, rebuilt.cursor);
    Ok(())
}

#[test]
fn incremental_compaction_accepts_applied_parent_that_is_not_latest() -> Result<()> {
    let mut prefix = Vec::new();
    append_applied_attempt(&mut prefix, "attempt-1", "compaction-1", None)?;
    append_applied_attempt(&mut prefix, "attempt-2", "compaction-2", None)?;
    let mut incremental = ActiveSessionProjection::from_records(&prefix, active_frontier(&prefix))?;
    let mut suffix = Vec::new();
    append_applied_attempt(
        &mut suffix,
        "attempt-3",
        "compaction-3",
        Some("compaction-1"),
    )?;
    let sequence_offset = prefix.len() as u64;
    for (index, record) in suffix.iter_mut().enumerate() {
        let SessionStreamRecord::Stored(event) = record;
        event.stream_sequence = sequence_offset
            .saturating_add(index as u64)
            .saturating_add(1);
        if event.event_kind() == Some(DurableEventType::CompactionAppliedV2) {
            let mut applied: CompactionAppliedV2 = serde_json::from_value(event.payload.clone())?;
            applied.folded_through.through_stream_sequence =
                event.stream_sequence.saturating_sub(1);
            applied.folded_through.through_event_id = "attempt-3-started".to_owned();
            event.payload = serde_json::to_value(applied)?;
        }
        event.record_checksum = event.compute_record_checksum()?;
    }
    let mut full_records = prefix;
    full_records.extend(suffix.clone());

    incremental.apply_records(&suffix, active_frontier(&full_records))?;
    let rebuilt =
        ActiveSessionProjection::from_records(&full_records, active_frontier(&full_records))?;
    assert_eq!(
        incremental.compaction.latest_applied_compaction_id(),
        Some("compaction-3")
    );
    assert_eq!(
        incremental.compaction.latest_applied_compaction_id(),
        rebuilt.compaction.latest_applied_compaction_id()
    );
    assert_eq!(incremental.cursor, rebuilt.cursor);
    Ok(())
}

fn append_applied_attempt(
    records: &mut Vec<SessionStreamRecord>,
    attempt_id: &str,
    compaction_id: &str,
    parent_compaction_id: Option<&str>,
) -> Result<()> {
    let started_sequence = (records.len() as u64).saturating_add(1);
    let started_event_id = format!("{attempt_id}-started");
    records.push(SessionStreamRecord::Stored(compaction_event(
        DurableEventType::CompactionStarted,
        &started_event_id,
        started_sequence,
        serde_json::to_value(CompactionStartedEntry {
            attempt_id: attempt_id.to_owned(),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "projection-r1".to_owned(),
            started_at_unix_ms: started_sequence,
        })?,
        Some(&started_event_id),
        None,
    )?));
    records.push(SessionStreamRecord::Stored(compaction_event(
        DurableEventType::CompactionAppliedV2,
        &format!("{attempt_id}-applied"),
        started_sequence.saturating_add(1),
        serde_json::to_value(CompactionAppliedV2 {
            compaction_id: compaction_id.to_owned(),
            attempt_id: attempt_id.to_owned(),
            parent_compaction_id: parent_compaction_id.map(str::to_owned),
            branch_id: None,
            valid_for_snapshot: None,
            task_memory_id: None,
            checkpoint: ContinuationCheckpointV1::empty(),
            base_projection_revision: "projection-r1".to_owned(),
            folded_through: CompactionCursor {
                session_id: "active-compaction-session".to_owned(),
                through_stream_sequence: started_sequence,
                through_event_id: started_event_id.clone(),
            },
            applied_at_unix_ms: started_sequence.saturating_add(1),
        })?,
        Some(&started_event_id),
        Some(&started_event_id),
    )?));
    Ok(())
}

fn active_frontier(records: &[SessionStreamRecord]) -> ActiveProjectionFrontier {
    let last = records.last().expect("test projection has records");
    ActiveProjectionFrontier {
        writer_generation: "test-generation".to_owned(),
        session_id: last.session_id().to_owned(),
        durable_end_offset: records.len() as u64,
        cursor: Some(last.projection_cursor(ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION)),
    }
}

fn compaction_event(
    event_type: DurableEventType,
    event_id: &str,
    stream_sequence: u64,
    payload: serde_json::Value,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> Result<StoredEvent> {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known compaction event has a class"),
        event_id.to_owned(),
        "active-compaction-session".to_owned(),
        stream_sequence,
        payload,
    )?;
    event.correlation_id = correlation_id.map(str::to_owned);
    event.causation_id = causation_id.map(str::to_owned);
    event.record_checksum = event.compute_record_checksum()?;
    Ok(event)
}
