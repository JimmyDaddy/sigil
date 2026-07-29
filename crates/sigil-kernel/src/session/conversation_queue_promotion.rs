use anyhow::{Context, Result};

use super::*;
use crate::{
    ConversationInputPromotedEntry, ConversationQueueDurableProjection, EventId,
    TaskGuidancePromotedEntry, TaskPlanStatus, TaskRunStatus, TaskStateProjection,
    stable_event_uuid,
};

impl JsonlSessionStore {
    /// Appends the single critical event that atomically promotes a queued main-thread input.
    ///
    /// The compare-and-swap predicate runs under the JSONL single-writer lease. A stale queue
    /// revision, pause, edit, cancellation, reordering, or previous promotion leaves the stream
    /// unchanged and returns an error.
    pub fn append_conversation_input_promoted(
        &self,
        entry: ConversationInputPromotedEntry,
    ) -> Result<StoredEvent> {
        let snapshot = self.active_projection_snapshot()?;
        let frontier = snapshot.frontier().clone();
        self.append_conversation_input_promoted_at(entry, &frontier)
    }

    /// Appends a main-thread promotion only if `expected_frontier` remains current.
    pub fn append_conversation_input_promoted_at(
        &self,
        entry: ConversationInputPromotedEntry,
        expected_frontier: &ActiveProjectionFrontier,
    ) -> Result<StoredEvent> {
        let session_id = expected_frontier.session_id().to_owned();
        entry.validate_for_session(&session_id)?;
        let event_id = conversation_input_promotion_event_id(&session_id, &entry);
        let payload = serde_json::to_value(&entry)
            .context("failed to encode conversation input promoted event")?;
        let event = self.append_event_if_active_projection(
            DurableEventType::ConversationInputPromoted,
            payload,
            event_id.clone(),
            Some(event_id),
            None,
            Some(expected_frontier),
            |projection| {
                projection.queue().validate_promotion(&entry)?;
                Ok(true)
            },
        )?;
        event.context("conversation input promotion frontier is stale")
    }

    /// Atomically binds one queued task-guidance item to an accepted task plan version.
    ///
    /// Queue ownership and task-plan eligibility are checked together under the JSONL
    /// single-writer lease. A stale queue revision, terminal task, superseded plan, or duplicate
    /// promotion leaves the stream unchanged.
    pub fn append_task_guidance_promoted(
        &self,
        entry: TaskGuidancePromotedEntry,
    ) -> Result<StoredEvent> {
        let snapshot = self.active_projection_snapshot()?;
        let frontier = snapshot.frontier().clone();
        self.append_task_guidance_promoted_at(entry, &frontier)
    }

    /// Appends task guidance only if `expected_frontier` and task-plan eligibility remain current.
    pub fn append_task_guidance_promoted_at(
        &self,
        entry: TaskGuidancePromotedEntry,
        expected_frontier: &ActiveProjectionFrontier,
    ) -> Result<StoredEvent> {
        let session_id = expected_frontier.session_id().to_owned();
        entry.validate_for_session(&session_id)?;
        let event_id = task_guidance_promotion_event_id(&session_id, &entry);
        let session_entry =
            SessionLogEntry::Control(ControlEntry::TaskGuidancePromoted(entry.clone()));
        let payload = serde_json::json!({ "session_log_entry": session_entry });
        let snapshot = self.active_projection_snapshot()?;
        let event = if snapshot.frontier() != expected_frontier {
            None
        } else if snapshot
            .projection
            .task_guidance_requires_canonical_validation()
        {
            self.append_event_if_records_at_active_frontier(
                DurableEventType::TaskGuidancePromoted,
                payload,
                event_id.clone(),
                Some(event_id),
                None,
                expected_frontier,
                |records| {
                    let queue = ConversationQueueDurableProjection::from_records(records)?;
                    queue.validate_task_guidance_promotion(&entry)?;
                    validate_task_guidance_plan(records, &entry)?;
                    Ok(true)
                },
            )?
        } else {
            self.append_event_if_active_projection(
                DurableEventType::TaskGuidancePromoted,
                payload,
                event_id.clone(),
                Some(event_id),
                None,
                Some(expected_frontier),
                |projection| {
                    projection.validate_task_guidance_promotion(&entry)?;
                    Ok(true)
                },
            )?
        };
        event.context("task guidance promotion frontier is stale")
    }
}

fn conversation_input_promotion_event_id(
    session_id: &str,
    entry: &ConversationInputPromotedEntry,
) -> EventId {
    stable_event_uuid(
        "sigil-conversation-input-promotion",
        &format!(
            "{session_id}:{}:{}:{}",
            entry.queue_id.as_str(),
            entry.expected_queue_revision.event_id,
            entry.dispatch_run_id
        ),
    )
}

fn task_guidance_promotion_event_id(
    session_id: &str,
    entry: &TaskGuidancePromotedEntry,
) -> EventId {
    stable_event_uuid(
        "sigil-task-guidance-promotion",
        &format!(
            "{session_id}:{}:{}:{}:{}",
            entry.queue_id.as_str(),
            entry.expected_queue_revision.event_id,
            entry.task_id.as_str(),
            entry.dispatch_run_id
        ),
    )
}

fn validate_task_guidance_plan(
    records: &[SessionStreamRecord],
    entry: &TaskGuidancePromotedEntry,
) -> Result<()> {
    let entries = records
        .iter()
        .filter_map(|record| record.session_log_entry().transpose())
        .collect::<Result<Vec<_>>>()?;
    let projection = TaskStateProjection::from_entries(&entries);
    let task = projection
        .tasks
        .get(&entry.task_id)
        .context("task guidance promotion references an unknown task")?;
    if matches!(
        task.status,
        TaskRunStatus::Completed | TaskRunStatus::Cancelled
    ) {
        anyhow::bail!("task guidance promotion cannot target a completed or cancelled task");
    }
    if task.latest_plan_version != Some(entry.plan_version)
        || !task
            .plans
            .get(&entry.plan_version)
            .is_some_and(|plan| plan.status == TaskPlanStatus::Accepted)
    {
        anyhow::bail!("task guidance promotion plan version is not the accepted task plan");
    }
    Ok(())
}
