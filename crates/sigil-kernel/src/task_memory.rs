use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, ChangeSetResultStatus, ContextBodyRef, ContextInclusionReason, ContextItem,
    ContextSensitivity, ContextSource, ContextTrustLevel, ControlEntry, DurableEventType, EventId,
    MutationCommitted, MutationSubject, ReceiptId, SessionLogEntry, SessionStreamRecord,
    TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskStepStatus, ToolExecutionStatus,
    WorkspaceSnapshotId,
};

pub type TaskMemoryId = String;
pub type BranchId = String;
pub type CommandReceiptId = ReceiptId;
pub type VerificationReceiptId = ReceiptId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SourcedFact {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_artifact_id: Option<ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_percent: Option<u8>,
    #[serde(default)]
    pub model_generated: bool,
    #[serde(default)]
    pub verified: bool,
}

impl SourcedFact {
    #[must_use]
    pub fn system_derived(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
            source_artifact_id: None,
            confidence_percent: None,
            model_generated: false,
            verified: false,
        }
    }

    #[must_use]
    pub fn model_inferred(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
            source_artifact_id: None,
            confidence_percent: None,
            model_generated: true,
            verified: false,
        }
    }

    #[must_use]
    pub fn model_inferred_with_confidence(
        text: impl Into<String>,
        source_event_id: impl Into<EventId>,
        confidence_percent: Option<u8>,
    ) -> Self {
        Self {
            confidence_percent,
            ..Self::model_inferred(text, source_event_id)
        }
    }

    /// Validates source metadata for a memory fact.
    ///
    /// # Errors
    ///
    /// Returns an error when the fact is empty, confidence is out of range, or a model-generated
    /// fact claims verified status without durable receipt/artifact backing.
    pub fn validate(&self) -> Result<()> {
        if self.text.trim().is_empty() {
            bail!("task memory fact is empty");
        }
        if self
            .confidence_percent
            .is_some_and(|confidence| confidence > 100)
        {
            bail!("task memory fact confidence must be 0..=100");
        }
        if self.verified && self.source_receipt_id.is_none() && self.source_artifact_id.is_none() {
            bail!("verified task memory fact requires durable receipt or artifact evidence");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SourcedDecision {
    pub decision: SourcedFact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<SourcedFact>,
}

impl SourcedDecision {
    /// Validates decision source metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the decision or rationale is invalid.
    pub fn validate(&self) -> Result<()> {
        self.decision.validate()?;
        if let Some(rationale) = &self.rationale {
            rationale.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct FileChangeRef {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_receipt_id: Option<ReceiptId>,
}

impl FileChangeRef {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            source_event_id: None,
            mutation_receipt_id: None,
        }
    }

    /// Validates that the file reference can be rendered and retrieved.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is empty.
    pub fn validate(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            bail!("task memory file change path is empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AttemptRef {
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// A source-bound view of the accepted plan for the currently active durable task.
///
/// This is a continuation aid, not an executable task graph: task lifecycle records remain the
/// authority for scheduling and status transitions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ActiveTaskPlanV1 {
    pub task_id: String,
    pub plan_version: u32,
    pub source_event_id: EventId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ActiveTaskPlanStepV1>,
}

impl ActiveTaskPlanV1 {
    fn validate(&self) -> Result<()> {
        if self.task_id.trim().is_empty() {
            bail!("active task plan task id is empty");
        }
        if self.source_event_id.trim().is_empty() {
            bail!("active task plan source event id is empty");
        }
        if self.steps.is_empty() {
            bail!("active task plan has no steps");
        }
        let mut step_ids = std::collections::BTreeSet::new();
        for step in &self.steps {
            step.validate()?;
            if !step_ids.insert(step.step_id.as_str()) {
                bail!("active task plan has duplicate step ids");
            }
        }
        Ok(())
    }
}

/// One source-bound step in [`ActiveTaskPlanV1`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ActiveTaskPlanStepV1 {
    pub step_id: String,
    pub title: String,
    pub status: TaskStepStatus,
    pub source_event_id: EventId,
}

impl ActiveTaskPlanStepV1 {
    fn validate(&self) -> Result<()> {
        if self.step_id.trim().is_empty() {
            bail!("active task plan step id is empty");
        }
        if self.title.trim().is_empty() {
            bail!("active task plan step title is empty");
        }
        if self.source_event_id.trim().is_empty() {
            bail!("active task plan step source event id is empty");
        }
        Ok(())
    }
}

impl AttemptRef {
    /// Validates attempt metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt id is empty.
    pub fn validate(&self) -> Result<()> {
        if self.attempt_id.trim().is_empty() {
            bail!("task memory attempt id is empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskMemoryV1 {
    pub memory_id: TaskMemoryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<BranchId>,
    pub valid_for_snapshot: WorkspaceSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<TaskMemoryId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_event_ids: Vec<EventId>,
    pub objective: String,
    /// Accepted task plan for the currently non-terminal task, reconstructed only from durable
    /// task lifecycle records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_plan: Option<ActiveTaskPlanV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<SourcedFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<SourcedDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_changed: Vec<FileChangeRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands_run: Vec<CommandReceiptId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_results: Vec<VerificationReceiptId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_attempts: Vec<AttemptRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<SourcedFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_issues: Vec<SourcedFact>,
}

impl TaskMemoryV1 {
    /// Validates task memory before attaching it to a compaction record.
    ///
    /// # Errors
    ///
    /// Returns an error when identity, snapshot binding, sourced facts, file refs, or attempt refs
    /// are malformed.
    pub fn validate(&self) -> Result<()> {
        if self.memory_id.trim().is_empty() {
            bail!("task memory id is empty");
        }
        if self.valid_for_snapshot.trim().is_empty() {
            bail!("task memory snapshot id is empty");
        }
        if self.objective.trim().is_empty() {
            bail!("task memory objective is empty");
        }
        if let Some(active_plan) = &self.active_plan {
            active_plan.validate()?;
        }
        for fact in self
            .constraints
            .iter()
            .chain(self.risks.iter())
            .chain(self.unresolved_issues.iter())
        {
            fact.validate()?;
        }
        for decision in &self.decisions {
            decision.validate()?;
        }
        for file in &self.files_changed {
            file.validate()?;
        }
        for attempt in &self.failed_attempts {
            attempt.validate()?;
        }
        Ok(())
    }

    pub fn merge_model_summary(&mut self, summary: ModelAssistedTaskMemorySummary) -> Result<()> {
        for fact in summary.constraints {
            self.constraints
                .push(model_summary_fact(fact, &summary.source_event_id));
        }
        for decision in summary.decisions {
            self.decisions.push(SourcedDecision {
                decision: model_summary_fact(decision.decision, &summary.source_event_id),
                rationale: decision
                    .rationale
                    .map(|rationale| model_summary_fact(rationale, &summary.source_event_id)),
            });
        }
        for fact in summary.risks {
            self.risks
                .push(model_summary_fact(fact, &summary.source_event_id));
        }
        for fact in summary.unresolved_issues {
            self.unresolved_issues
                .push(model_summary_fact(fact, &summary.source_event_id));
        }
        self.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ModelAssistedMemoryFact {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ModelAssistedMemoryDecision {
    pub decision: ModelAssistedMemoryFact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<ModelAssistedMemoryFact>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ModelAssistedTaskMemorySummary {
    pub source_event_id: EventId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ModelAssistedMemoryFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ModelAssistedMemoryDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<ModelAssistedMemoryFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_issues: Vec<ModelAssistedMemoryFact>,
}

fn model_summary_fact(fact: ModelAssistedMemoryFact, source_event_id: &str) -> SourcedFact {
    SourcedFact::model_inferred_with_confidence(
        fact.text,
        source_event_id.to_owned(),
        fact.confidence_percent,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskMemoryExtractionInput {
    pub memory_id: TaskMemoryId,
    pub valid_for_snapshot: WorkspaceSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<BranchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<TaskMemoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
}

/// Deterministically extracts typed task memory from durable stream records.
///
/// The extractor only uses structured durable/control payloads. It does not infer verification
/// success from model text and it does not create new evidence receipts.
///
/// # Errors
///
/// Returns an error when a structured session entry payload cannot be decoded or the resulting
/// memory is invalid.
pub fn extract_task_memory_from_stream_records(
    records: &[SessionStreamRecord],
    input: TaskMemoryExtractionInput,
) -> Result<TaskMemoryV1> {
    let mut builder = TaskMemoryExtractionBuilder::new(input);
    for record in records {
        if let Some(entry) = session_entry_from_stream_record(record)? {
            builder.apply_session_entry(record.event_id(), &entry);
        }
        let event = record.stored_event();
        if event.event_kind() == Some(DurableEventType::MutationCommitted) {
            let committed: MutationCommitted = serde_json::from_value(event.payload.clone())?;
            builder.apply_mutation_committed(record.event_id(), &committed);
        }
    }
    builder.finish()
}

pub fn task_memory_context_items(memory: &TaskMemoryV1) -> Result<Vec<ContextItem>> {
    memory.validate()?;
    let mut items = Vec::new();
    items.push(task_memory_context_item(
        format!("task-memory:{}:objective", memory.memory_id),
        memory.source_event_ids.first().cloned(),
        memory.objective.clone(),
        0.95,
    ));
    for (index, decision) in memory.decisions.iter().enumerate() {
        items.push(task_memory_context_item(
            format!("task-memory:{}:decision:{index}", memory.memory_id),
            decision.decision.source_event_id.clone(),
            decision.decision.text.clone(),
            0.85,
        ));
    }
    for (index, issue) in memory.unresolved_issues.iter().enumerate() {
        items.push(task_memory_context_item(
            format!("task-memory:{}:unresolved:{index}", memory.memory_id),
            issue.source_event_id.clone(),
            issue.text.clone(),
            0.8,
        ));
    }
    for (index, file) in memory.files_changed.iter().enumerate() {
        let body = format!("changed file: {}", file.path.display());
        items.push(task_memory_context_item(
            format!("task-memory:{}:file:{index}", memory.memory_id),
            file.source_event_id.clone(),
            body,
            0.7,
        ));
    }
    for item in &items {
        item.validate()?;
    }
    Ok(items)
}

fn task_memory_context_item(
    id: String,
    source_event_id: Option<EventId>,
    body: String,
    score: f32,
) -> ContextItem {
    ContextItem {
        id,
        source: ContextSource::TaskDigest,
        source_event_id: source_event_id.clone(),
        trust_level: ContextTrustLevel::ToolObservation,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: None,
        token_cost: crate::estimate_context_token_cost(&body),
        score: Some(score),
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: source_event_id
            .map(ContextBodyRef::DurableEvent)
            .unwrap_or_else(|| ContextBodyRef::inline(&body)),
    }
}

fn session_entry_from_stream_record(
    record: &SessionStreamRecord,
) -> Result<Option<SessionLogEntry>> {
    match record {
        SessionStreamRecord::Stored(event) => {
            let Some(value) = event.payload.get("session_log_entry") else {
                return Ok(None);
            };
            Ok(Some(serde_json::from_value(value.clone())?))
        }
    }
}

struct TaskMemoryExtractionBuilder {
    input: TaskMemoryExtractionInput,
    source_event_ids: Vec<EventId>,
    files_changed: Vec<FileChangeRef>,
    commands_run: Vec<CommandReceiptId>,
    verification_results: Vec<VerificationReceiptId>,
    failed_attempts: Vec<AttemptRef>,
    unresolved_issues: Vec<SourcedFact>,
    latest_user_objective: Option<(String, EventId)>,
    task_runs: BTreeMap<String, TaskRunObservation>,
    accepted_plans: BTreeMap<(String, u32), (TaskPlanEntry, EventId)>,
    latest_step_statuses: BTreeMap<(String, u32, String), (TaskStepStatus, EventId)>,
    next_task_run_order: usize,
}

#[derive(Clone)]
struct TaskRunObservation {
    entry: TaskRunEntry,
    source_event_id: EventId,
    order: usize,
}

impl TaskMemoryExtractionBuilder {
    fn new(input: TaskMemoryExtractionInput) -> Self {
        Self {
            input,
            source_event_ids: Vec::new(),
            files_changed: Vec::new(),
            commands_run: Vec::new(),
            verification_results: Vec::new(),
            failed_attempts: Vec::new(),
            unresolved_issues: Vec::new(),
            latest_user_objective: None,
            task_runs: BTreeMap::new(),
            accepted_plans: BTreeMap::new(),
            latest_step_statuses: BTreeMap::new(),
            next_task_run_order: 0,
        }
    }

    fn apply_session_entry(&mut self, event_id: &str, entry: &SessionLogEntry) {
        match entry {
            SessionLogEntry::User(message) => {
                if let Some(content) = message
                    .content
                    .as_deref()
                    .filter(|content| !content.trim().is_empty())
                {
                    self.latest_user_objective = Some((content.to_owned(), event_id.to_owned()));
                }
            }
            SessionLogEntry::Assistant(_) | SessionLogEntry::ToolResult(_) => {}
            SessionLogEntry::Control(control) => match control {
                ControlEntry::TaskRun(task) => {
                    self.push_source(event_id);
                    self.next_task_run_order = self.next_task_run_order.saturating_add(1);
                    self.task_runs.insert(
                        task.task_id.as_str().to_owned(),
                        TaskRunObservation {
                            entry: task.clone(),
                            source_event_id: event_id.to_owned(),
                            order: self.next_task_run_order,
                        },
                    );
                    if task.status.is_terminal()
                        && task.status != crate::TaskRunStatus::Completed
                        && let Some(reason) = &task.reason
                    {
                        self.push_failed_attempt(AttemptRef {
                            attempt_id: task.task_id.as_str().to_owned(),
                            source_event_id: Some(event_id.to_owned()),
                            summary: Some(reason.clone()),
                        });
                    }
                }
                ControlEntry::TaskPlan(plan) => {
                    self.push_source(event_id);
                    if plan.status == TaskPlanStatus::Accepted {
                        self.accepted_plans.insert(
                            (plan.task_id.as_str().to_owned(), plan.plan_version),
                            (plan.clone(), event_id.to_owned()),
                        );
                    }
                }
                ControlEntry::TaskStep(step) => {
                    self.push_source(event_id);
                    self.latest_step_statuses.insert(
                        (
                            step.task_id.as_str().to_owned(),
                            step.plan_version,
                            step.step_id.as_str().to_owned(),
                        ),
                        (step.status, event_id.to_owned()),
                    );
                    if matches!(
                        step.status,
                        crate::TaskStepStatus::Failed
                            | crate::TaskStepStatus::Blocked
                            | crate::TaskStepStatus::Interrupted
                    ) {
                        let summary = step
                            .reason
                            .clone()
                            .or_else(|| step.summary.clone())
                            .or_else(|| step.title.clone());
                        self.push_failed_attempt(AttemptRef {
                            attempt_id: format!(
                                "{}:{}",
                                step.task_id.as_str(),
                                step.step_id.as_str()
                            ),
                            source_event_id: Some(event_id.to_owned()),
                            summary,
                        });
                        if step.status == crate::TaskStepStatus::Blocked {
                            self.unresolved_issues.push(SourcedFact::system_derived(
                                step.reason
                                    .clone()
                                    .or_else(|| step.title.clone())
                                    .unwrap_or_else(|| "task step blocked".to_owned()),
                                event_id.to_owned(),
                            ));
                        }
                    }
                }
                ControlEntry::ToolExecution(execution) => {
                    self.push_source(event_id);
                    if execution.status == ToolExecutionStatus::Completed {
                        self.push_command(execution.call_id.clone());
                        for path in &execution.changed_files {
                            self.push_file_change(FileChangeRef {
                                path: path.into(),
                                source_event_id: Some(event_id.to_owned()),
                                mutation_receipt_id: None,
                            });
                        }
                    } else if matches!(
                        execution.status,
                        ToolExecutionStatus::Failed
                            | ToolExecutionStatus::Cancelled
                            | ToolExecutionStatus::Interrupted
                    ) {
                        self.push_failed_attempt(AttemptRef {
                            attempt_id: execution.call_id.clone(),
                            source_event_id: Some(event_id.to_owned()),
                            summary: execution.error.as_ref().map(|error| error.message.clone()),
                        });
                    }
                }
                ControlEntry::VerificationRecorded(entry) => {
                    self.push_source(event_id);
                    self.push_verification(entry.receipt.receipt.receipt_id.clone());
                }
                ControlEntry::ChangeSetApplied(result) => {
                    self.push_source(event_id);
                    for file in &result.file_results {
                        self.push_file_change(FileChangeRef {
                            path: file.path.clone().into(),
                            source_event_id: Some(event_id.to_owned()),
                            mutation_receipt_id: Some(result.id.as_str().to_owned()),
                        });
                    }
                    if !matches!(
                        result.status,
                        ChangeSetResultStatus::Applied | ChangeSetResultStatus::PartiallyApplied
                    ) {
                        self.push_failed_attempt(AttemptRef {
                            attempt_id: result.id.as_str().to_owned(),
                            source_event_id: Some(event_id.to_owned()),
                            summary: result.message.clone(),
                        });
                    }
                }
                _ => {}
            },
        }
    }

    fn apply_mutation_committed(&mut self, event_id: &str, committed: &MutationCommitted) {
        self.push_source(event_id);
        if let MutationSubject::File { path, .. } | MutationSubject::Directory { path } =
            &committed.committed_subject
        {
            self.push_file_change(FileChangeRef {
                path: path.clone(),
                source_event_id: Some(event_id.to_owned()),
                mutation_receipt_id: Some(committed.operation_id.clone()),
            });
        }
    }

    fn finish(mut self) -> Result<TaskMemoryV1> {
        let latest_task = self
            .task_runs
            .values()
            .max_by_key(|task| task.order)
            .cloned();
        let active_task = self
            .task_runs
            .values()
            .filter(|task| !task.entry.status.is_terminal())
            .max_by_key(|task| task.order)
            .cloned();
        let active_plan = active_task.as_ref().and_then(|task| {
            self.accepted_plans
                .iter()
                .filter(|((task_id, _), _)| task_id == task.entry.task_id.as_str())
                .max_by_key(|((_, version), _)| *version)
                .map(
                    |((task_id, plan_version), (plan, plan_event_id))| ActiveTaskPlanV1 {
                        task_id: task_id.clone(),
                        plan_version: *plan_version,
                        source_event_id: plan_event_id.clone(),
                        steps: plan
                            .steps
                            .iter()
                            .map(|step| {
                                let key = (
                                    task_id.clone(),
                                    *plan_version,
                                    step.step_id.as_str().to_owned(),
                                );
                                let (status, source_event_id) = self
                                    .latest_step_statuses
                                    .get(&key)
                                    .cloned()
                                    .unwrap_or((TaskStepStatus::Pending, plan_event_id.clone()));
                                ActiveTaskPlanStepV1 {
                                    step_id: step.step_id.as_str().to_owned(),
                                    title: step.title.clone(),
                                    status,
                                    source_event_id,
                                }
                            })
                            .collect(),
                    },
                )
        });
        let (objective, objective_source_event_id) =
            if let Some(objective) = self.input.objective.clone() {
                (objective, None)
            } else if let Some(task) = active_task {
                (task.entry.objective, Some(task.source_event_id))
            } else if let Some(task) = latest_task {
                (task.entry.objective, Some(task.source_event_id))
            } else if let Some((objective, source_event_id)) = self.latest_user_objective.clone() {
                (objective, Some(source_event_id))
            } else {
                ("No task objective recorded".to_owned(), None)
            };
        if let Some(source_event_id) = objective_source_event_id.as_deref() {
            self.push_source(source_event_id);
        }
        let memory = TaskMemoryV1 {
            memory_id: self.input.memory_id,
            branch_id: self.input.branch_id,
            valid_for_snapshot: self.input.valid_for_snapshot,
            supersedes: self.input.supersedes,
            source_event_ids: self.source_event_ids,
            objective,
            active_plan,
            constraints: Vec::new(),
            decisions: Vec::new(),
            files_changed: self.files_changed,
            commands_run: self.commands_run,
            verification_results: self.verification_results,
            failed_attempts: self.failed_attempts,
            risks: Vec::new(),
            unresolved_issues: self.unresolved_issues,
        };
        memory.validate()?;
        Ok(memory)
    }

    fn push_source(&mut self, event_id: &str) {
        push_unique(&mut self.source_event_ids, event_id.to_owned());
    }

    fn push_file_change(&mut self, file: FileChangeRef) {
        if !self.files_changed.iter().any(|existing| {
            existing.path == file.path && existing.mutation_receipt_id == file.mutation_receipt_id
        }) {
            self.files_changed.push(file);
        }
    }

    fn push_command(&mut self, command: CommandReceiptId) {
        push_unique(&mut self.commands_run, command);
    }

    fn push_verification(&mut self, receipt: VerificationReceiptId) {
        push_unique(&mut self.verification_results, receipt);
    }

    fn push_failed_attempt(&mut self, attempt: AttemptRef) {
        if !self
            .failed_attempts
            .iter()
            .any(|existing| existing.attempt_id == attempt.attempt_id)
        {
            self.failed_attempts.push(attempt);
        }
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
#[path = "tests/compaction_memory_tests.rs"]
mod tests;
