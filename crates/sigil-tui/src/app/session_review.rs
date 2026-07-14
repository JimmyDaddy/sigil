use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use sigil_kernel::{
    CheckpointRestored, ControlEntry, DomainEvent, JsonlSessionStore, MutationCommitted,
    MutationPrepared, MutationSubject, ReadinessEvaluatedEntry, RunStatus, SessionLogEntry,
    SessionStreamRecord, SnapshotCoverage, ToolExecutionStatus, VerificationVerdict,
    WorkspaceMutationDetected,
};

use super::formatting::truncate_session_view_text;

const REVIEW_FILE_LIMIT: usize = 3;

#[derive(Debug, Default)]
struct ReviewTurnSummary {
    index: usize,
    prompt: Option<String>,
    changed_files: BTreeSet<String>,
    tool_calls: usize,
    controlled_writes: usize,
    unknown_mutations: usize,
    checkpoint_restores: usize,
    precise_restore_available: bool,
    limited_restore_evidence: bool,
    latest_readiness: Option<(RunStatus, VerificationVerdict)>,
}

impl ReviewTurnSummary {
    fn new(index: usize, prompt: String) -> Self {
        Self {
            index,
            prompt: Some(prompt),
            ..Self::default()
        }
    }

    fn has_reviewable_evidence(&self) -> bool {
        self.tool_calls > 0
            || self.controlled_writes > 0
            || self.unknown_mutations > 0
            || self.checkpoint_restores > 0
            || !self.changed_files.is_empty()
            || self.latest_readiness.is_some()
    }
}

pub(super) fn session_review_sidebar_lines(
    session_log_path: &Path,
    entries: &[SessionLogEntry],
) -> Vec<String> {
    if entries.is_empty() && !session_log_path.exists() {
        return Vec::new();
    }

    let records = match JsonlSessionStore::read_event_records(session_log_path) {
        Ok(records) => records,
        Err(error) => {
            return vec![format!(
                "review: durable stream unavailable ({})",
                truncate_session_view_text(&error.to_string(), 48)
            )];
        }
    };

    session_review_sidebar_lines_from_records(&records, entries)
}

pub(super) fn checkpoint_verification_order(
    session_log_path: &Path,
) -> (Option<u64>, BTreeMap<sigil_kernel::EvidenceScope, u64>) {
    let Ok(records) = JsonlSessionStore::read_event_records(session_log_path) else {
        return (None, BTreeMap::new());
    };
    checkpoint_verification_order_from_records(&records)
}

fn verification_stale_after_checkpoint_from_records(records: &[SessionStreamRecord]) -> bool {
    let (latest_restore, readiness_by_scope) = checkpoint_verification_order_from_records(records);
    let latest_readiness = readiness_by_scope.values().max().copied();
    latest_restore.is_some_and(|restore| restore > latest_readiness.unwrap_or(0))
}

fn checkpoint_verification_order_from_records(
    records: &[SessionStreamRecord],
) -> (Option<u64>, BTreeMap<sigil_kernel::EvidenceScope, u64>) {
    let mut latest_restore = None;
    let mut readiness_by_scope = BTreeMap::new();
    for record in records {
        let event = record.stored_event();
        match event.event_kind() {
            Some(sigil_kernel::DurableEventType::CheckpointRestored) => {
                latest_restore = Some(event.stream_sequence);
            }
            Some(sigil_kernel::DurableEventType::ReadinessEvaluated) => {
                if let Some(SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness))) =
                    session_entry_from_stream_record(record)
                {
                    readiness_by_scope.insert(readiness.scope, event.stream_sequence);
                }
            }
            _ => {}
        }
    }
    (latest_restore, readiness_by_scope)
}

pub(super) fn session_review_sidebar_lines_from_records(
    records: &[SessionStreamRecord],
    fallback_entries: &[SessionLogEntry],
) -> Vec<String> {
    let mut turns_seen = 0usize;
    let mut current = ReviewTurnSummary::default();
    let mut latest_completed = ReviewTurnSummary::default();

    for record in records {
        if let Some(entry) = session_entry_from_stream_record(record) {
            apply_session_entry(&entry, &mut current, &mut latest_completed, &mut turns_seen);
        }
        apply_durable_event(record, &mut current);
    }

    if current.index > 0 || current.has_reviewable_evidence() {
        latest_completed = std::mem::take(&mut current);
    }

    if records.is_empty() {
        for entry in fallback_entries {
            apply_session_entry(entry, &mut current, &mut latest_completed, &mut turns_seen);
        }
        if current.index > 0 || current.has_reviewable_evidence() {
            latest_completed = current;
        }
    }

    if turns_seen == 0 && !latest_completed.has_reviewable_evidence() {
        return Vec::new();
    }

    render_review_lines(
        turns_seen,
        &latest_completed,
        verification_stale_after_checkpoint_from_records(records),
    )
}

fn apply_session_entry(
    entry: &SessionLogEntry,
    current: &mut ReviewTurnSummary,
    latest_completed: &mut ReviewTurnSummary,
    turns_seen: &mut usize,
) {
    match entry {
        SessionLogEntry::User(message) => {
            if current.index > 0 || current.has_reviewable_evidence() {
                *latest_completed = std::mem::take(current);
            }
            *turns_seen = turns_seen.saturating_add(1);
            *current = ReviewTurnSummary::new(
                *turns_seen,
                message
                    .content
                    .as_deref()
                    .map(prompt_preview)
                    .unwrap_or_else(|| "(empty prompt)".to_owned()),
            );
        }
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) => {
            if execution.status == ToolExecutionStatus::Completed {
                current.tool_calls = current.tool_calls.saturating_add(1);
                current
                    .changed_files
                    .extend(execution.changed_files.iter().cloned());
            }
        }
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(preview)) => {
            current
                .changed_files
                .extend(preview.changed_files.iter().cloned());
        }
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness)) => {
            apply_readiness(readiness, current);
        }
        _ => {}
    }
}

fn apply_durable_event(record: &SessionStreamRecord, current: &mut ReviewTurnSummary) {
    let Ok(Some(domain_record)) = record.domain_event_record() else {
        return;
    };
    match domain_record.event {
        DomainEvent::MutationPrepared(payload) => {
            if let Ok(prepared) = serde_json::from_value::<MutationPrepared>(payload.payload) {
                current.controlled_writes = current.controlled_writes.saturating_add(1);
                match prepared.snapshot_coverage {
                    SnapshotCoverage::Captured(_) | SnapshotCoverage::NoPriorContent => {
                        current.precise_restore_available = true;
                    }
                    SnapshotCoverage::SkippedSensitive
                    | SnapshotCoverage::Unsupported
                    | SnapshotCoverage::Unavailable => {
                        current.limited_restore_evidence = true;
                    }
                }
                record_subject_path(&prepared.subject, &mut current.changed_files);
            }
        }
        DomainEvent::MutationCommitted(payload) => {
            if let Ok(committed) = serde_json::from_value::<MutationCommitted>(payload.payload) {
                record_subject_path(&committed.committed_subject, &mut current.changed_files);
            }
        }
        DomainEvent::WorkspaceMutationDetected(payload) => {
            if let Ok(detected) =
                serde_json::from_value::<WorkspaceMutationDetected>(payload.payload)
            {
                current.unknown_mutations = current.unknown_mutations.saturating_add(1);
                if detected.unknown_dirty {
                    current.limited_restore_evidence = true;
                }
            }
        }
        DomainEvent::CheckpointRestored(payload) => {
            if let Ok(restored) = serde_json::from_value::<CheckpointRestored>(payload.payload) {
                current.checkpoint_restores = current.checkpoint_restores.saturating_add(1);
                record_subject_path(&restored.restored_subject, &mut current.changed_files);
            }
        }
        DomainEvent::ReadinessEvaluated(payload) => {
            if let Ok(readiness) = serde_json::from_value::<ReadinessEvaluatedEntry>(
                payload
                    .payload
                    .get("session_log_entry")
                    .and_then(|entry| entry.get("control"))
                    .and_then(|control| control.get("readiness_evaluated"))
                    .cloned()
                    .unwrap_or(payload.payload),
            ) {
                apply_readiness(&readiness, current);
            }
        }
        _ => {}
    }
}

fn apply_readiness(readiness: &ReadinessEvaluatedEntry, current: &mut ReviewTurnSummary) {
    current.latest_readiness = Some((
        readiness.evaluation.run_status,
        readiness.evaluation.verification_verdict,
    ));
}

fn session_entry_from_stream_record(record: &SessionStreamRecord) -> Option<SessionLogEntry> {
    match record {
        SessionStreamRecord::Stored(event) => event
            .payload
            .get("session_log_entry")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    }
}

fn record_subject_path(subject: &MutationSubject, changed_files: &mut BTreeSet<String>) {
    match subject {
        MutationSubject::File { path, .. } | MutationSubject::Directory { path } => {
            changed_files.insert(path_label(path));
        }
        MutationSubject::Workspace { .. }
        | MutationSubject::External { .. }
        | MutationSubject::Unknown => {}
    }
}

fn render_review_lines(
    turns_seen: usize,
    latest: &ReviewTurnSummary,
    verification_stale_after_checkpoint: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let turn_index = if latest.index > 0 {
        latest.index
    } else {
        turns_seen
    };
    let prompt = latest.prompt.as_deref().unwrap_or("no prompt");
    lines.push(format!(
        "review: turn {turn_index}/{turns_seen} · {}",
        truncate_session_view_text(prompt, 40)
    ));
    lines.push(format!(
        "changes: {} · tools {} · writes {}",
        changed_files_label(&latest.changed_files),
        latest.tool_calls,
        latest.controlled_writes
    ));
    if verification_stale_after_checkpoint {
        lines.push("verification: stale after checkpoint restore".to_owned());
    } else if let Some((run_status, verdict)) = latest.latest_readiness {
        lines.push(format!(
            "verification: run {} · {}",
            run_status_label(run_status),
            verification_verdict_label(verdict)
        ));
    } else {
        lines.push("verification: not evaluated".to_owned());
    }
    lines.push(checkpoint_label(latest));
    if latest.precise_restore_available
        || latest.limited_restore_evidence
        || latest.unknown_mutations > 0
    {
        lines.push("actions: Ctrl-R open restore dialog".to_owned());
    }
    lines
}

fn changed_files_label(files: &BTreeSet<String>) -> String {
    if files.is_empty() {
        return "no files".to_owned();
    }
    let mut listed = files
        .iter()
        .take(REVIEW_FILE_LIMIT)
        .map(|path| truncate_session_view_text(path, 24))
        .collect::<Vec<_>>();
    let hidden = files.len().saturating_sub(listed.len());
    if hidden > 0 {
        listed.push(format!("+{hidden}"));
    }
    listed.join(", ")
}

fn checkpoint_label(latest: &ReviewTurnSummary) -> String {
    if latest.checkpoint_restores > 0 {
        return format!(
            "rewind: {} restore events recorded",
            latest.checkpoint_restores
        );
    }
    if latest.unknown_mutations > 0 {
        return format!(
            "rewind: unknown write{} need git/manual restore",
            plural(latest.unknown_mutations)
        );
    }
    if latest.precise_restore_available {
        return "rewind: controlled checkpoint available".to_owned();
    }
    if latest.limited_restore_evidence {
        return "rewind: checkpoint limited by sensitivity/support".to_owned();
    }
    "rewind: no controlled checkpoint".to_owned()
}

fn prompt_preview(content: &str) -> String {
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("(empty prompt)");
    let rest = lines.count();
    if rest == 0 {
        truncate_session_view_text(first, 64)
    } else {
        format!("{} +{} more", truncate_session_view_text(first, 48), rest)
    }
}

fn path_label(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Paused => "paused",
        RunStatus::Blocked => "blocked",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Interrupted => "interrupted",
    }
}

fn verification_verdict_label(verdict: VerificationVerdict) -> &'static str {
    match verdict {
        VerificationVerdict::NotEvaluated => "not_evaluated",
        VerificationVerdict::NotApplicable => "not_applicable",
        VerificationVerdict::Pending => "pending",
        VerificationVerdict::Passed => "passed",
        VerificationVerdict::Failed => "failed",
        VerificationVerdict::Missing => "missing",
        VerificationVerdict::Inconclusive => "inconclusive",
        VerificationVerdict::Stale => "stale",
        VerificationVerdict::Skipped => "skipped",
    }
}
