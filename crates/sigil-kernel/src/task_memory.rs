use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, ChangeSetResultStatus, ControlEntry, DurableEventType, EventId, MutationCommitted,
    MutationSubject, ReceiptId, SessionLogEntry, SessionStreamRecord, ToolExecutionStatus,
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
        if self.model_generated
            && self.verified
            && self.source_receipt_id.is_none()
            && self.source_artifact_id.is_none()
        {
            bail!("model-generated task memory fact cannot be verified without durable evidence");
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
        if let SessionStreamRecord::Stored(event) = record
            && event.event_kind() == Some(DurableEventType::MutationCommitted)
        {
            let committed: MutationCommitted = serde_json::from_value(event.payload.clone())?;
            builder.apply_mutation_committed(record.event_id(), &committed);
        }
    }
    builder.finish()
}

fn session_entry_from_stream_record(
    record: &SessionStreamRecord,
) -> Result<Option<SessionLogEntry>> {
    match record {
        SessionStreamRecord::Legacy { entry, .. } => Ok(Some((**entry).clone())),
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
    objective: Option<String>,
    files_changed: Vec<FileChangeRef>,
    commands_run: Vec<CommandReceiptId>,
    verification_results: Vec<VerificationReceiptId>,
    failed_attempts: Vec<AttemptRef>,
    unresolved_issues: Vec<SourcedFact>,
}

impl TaskMemoryExtractionBuilder {
    fn new(input: TaskMemoryExtractionInput) -> Self {
        Self {
            objective: input.objective.clone(),
            input,
            source_event_ids: Vec::new(),
            files_changed: Vec::new(),
            commands_run: Vec::new(),
            verification_results: Vec::new(),
            failed_attempts: Vec::new(),
            unresolved_issues: Vec::new(),
        }
    }

    fn apply_session_entry(&mut self, event_id: &str, entry: &SessionLogEntry) {
        let SessionLogEntry::Control(control) = entry else {
            return;
        };
        match control {
            ControlEntry::TaskRun(task) => {
                self.push_source(event_id);
                if self.objective.is_none() && !task.objective.trim().is_empty() {
                    self.objective = Some(task.objective.clone());
                }
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
            ControlEntry::TaskStep(step) => {
                self.push_source(event_id);
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
                        attempt_id: format!("{}:{}", step.task_id.as_str(), step.step_id.as_str()),
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

    fn finish(self) -> Result<TaskMemoryV1> {
        let memory = TaskMemoryV1 {
            memory_id: self.input.memory_id,
            branch_id: self.input.branch_id,
            valid_for_snapshot: self.input.valid_for_snapshot,
            supersedes: self.input.supersedes,
            source_event_ids: self.source_event_ids,
            objective: self
                .objective
                .unwrap_or_else(|| "No task objective recorded".to_owned()),
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
