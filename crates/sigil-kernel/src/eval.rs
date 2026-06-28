//! RFC-0013 eval harness result model.
//!
//! This module intentionally contains only provider-neutral result types. The deterministic
//! harness, fixture runner, model runner, report writer, and product surfaces are separate slices.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ControlEntry, EventId, JsonlSessionStore, ProjectionCursor, RunStatus, SessionLogEntry,
    VerificationVerdict, VisibleCompletionState, session::SESSION_ENTRY_PROJECTION_SCHEMA_VERSION,
};

#[cfg(test)]
#[path = "tests/eval_tests.rs"]
mod tests;

pub type EvalCaseId = String;
pub type EvalRunId = String;
pub type EvalFixtureId = String;
pub type EvalEvidenceId = String;
pub type EvalStepId = String;
pub type EvalToolCallId = String;

/// Provider-neutral outcome bucket for one eval run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalOutcomeKind {
    VerifiedSuccess,
    Completed,
    CompletedUnverified,
    Blocked,
    Failed,
    FailedVerification,
    Cancelled,
    Interrupted,
    PermissionDenied,
    SandboxDenied,
}

impl EvalOutcomeKind {
    #[must_use]
    pub fn from_completion(
        run_status: RunStatus,
        verification_verdict: VerificationVerdict,
        failures: &[EvalFailure],
    ) -> Self {
        if failures
            .iter()
            .any(|failure| failure.kind == EvalFailureKind::SandboxDenied)
        {
            return Self::SandboxDenied;
        }
        if failures
            .iter()
            .any(|failure| failure.kind == EvalFailureKind::PermissionDenied)
        {
            return Self::PermissionDenied;
        }
        match VisibleCompletionState::derive(run_status, verification_verdict) {
            VisibleCompletionState::Verified => Self::VerifiedSuccess,
            VisibleCompletionState::Completed => Self::Completed,
            VisibleCompletionState::CompletedUnverified => Self::CompletedUnverified,
            VisibleCompletionState::FailedVerification => Self::FailedVerification,
            VisibleCompletionState::Failed => Self::Failed,
            VisibleCompletionState::Cancelled => Self::Cancelled,
            VisibleCompletionState::Interrupted => Self::Interrupted,
            VisibleCompletionState::NeedsUser | VisibleCompletionState::Paused => Self::Blocked,
            VisibleCompletionState::Running => Self::Blocked,
        }
    }
}

/// Stable failure taxonomy for deterministic and model-backed evals.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalFailureKind {
    Model,
    Tool,
    PermissionDenied,
    PathEscapeDenied,
    SandboxDenied,
    ApprovalDenied,
    VerificationFailed,
    VerificationMissing,
    VerificationStale,
    VerificationInconclusive,
    Integrity,
    Timeout,
    Interrupted,
    Cancelled,
    Harness,
    Unknown,
}

/// One failure or denial observed during an eval run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalFailure {
    pub kind: EvalFailureKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvalEvidenceRef>,
}

impl EvalFailure {
    #[must_use]
    pub fn new(kind: EvalFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            evidence: Vec::new(),
        }
    }
}

/// Kind of evidence referenced from an eval result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalEvidenceKind {
    DurableEvent,
    Receipt,
    Artifact,
    SessionLog,
    ToolCall,
    ProjectionCursor,
}

/// Stable pointer to durable evidence without embedding the evidence body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalEvidenceRef {
    pub kind: EvalEvidenceKind,
    pub id: EvalEvidenceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

impl EvalEvidenceRef {
    #[must_use]
    pub fn durable_event(id: impl Into<String>, event_id: impl Into<EventId>) -> Self {
        Self {
            kind: EvalEvidenceKind::DurableEvent,
            id: id.into(),
            event_id: Some(event_id.into()),
            artifact_ref: None,
        }
    }
}

/// Minimal tool-call summary recorded by eval output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalToolCallSummary {
    pub tool_call_id: EvalToolCallId,
    pub tool_name: String,
    pub status: EvalToolCallStatus,
}

/// Provider-neutral tool-call terminal status for eval reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalToolCallStatus {
    Succeeded,
    Failed,
    Denied,
    Interrupted,
}

/// Follow-up action required for an eval result to become verified or actionable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalRequiredAction {
    pub kind: EvalRequiredActionKind,
    pub message: String,
}

impl EvalRequiredAction {
    /// Creates one provider-neutral required action.
    #[must_use]
    pub fn new(kind: EvalRequiredActionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Provider-neutral required action taxonomy for eval reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalRequiredActionKind {
    RunCheck,
    ConfigureVerification,
    ApproveWorkspace,
    InspectEvidence,
}

/// Metadata needed to compare eval runs over time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalRunMetadata {
    pub case_id: EvalCaseId,
    pub run_id: EvalRunId,
    pub fixture_id: EvalFixtureId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_fixture_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigil_version: Option<String>,
    pub provider: String,
    pub model: String,
    pub model_parameters_hash: String,
    pub tool_schema_digest: String,
    pub config_hash: String,
    pub sandbox_backend: String,
    pub os_toolchain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl EvalRunMetadata {
    #[must_use]
    pub fn deterministic(
        case_id: impl Into<String>,
        run_id: impl Into<String>,
        fixture_id: impl Into<String>,
    ) -> Self {
        Self {
            case_id: case_id.into(),
            run_id: run_id.into(),
            fixture_id: fixture_id.into(),
            repo_fixture_commit: None,
            sigil_version: None,
            provider: "fake".to_owned(),
            model: "deterministic".to_owned(),
            model_parameters_hash: "sha256:deterministic".to_owned(),
            tool_schema_digest: "sha256:deterministic".to_owned(),
            config_hash: "sha256:deterministic".to_owned(),
            sandbox_backend: "none".to_owned(),
            os_toolchain: "deterministic".to_owned(),
            seed: None,
        }
    }
}

/// Structured result for one eval case run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalResult {
    pub metadata: EvalRunMetadata,
    pub outcome: EvalOutcomeKind,
    pub run_status: RunStatus,
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<EvalToolCallSummary>,
    #[serde(default)]
    pub approval_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_log_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_stream_cursor: Option<ProjectionCursor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvalEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_actions: Vec<EvalRequiredAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<EvalFailure>,
}

impl EvalResult {
    #[must_use]
    pub fn from_completion(
        metadata: EvalRunMetadata,
        run_status: RunStatus,
        verification_verdict: VerificationVerdict,
        failures: Vec<EvalFailure>,
    ) -> Self {
        let visible_state = VisibleCompletionState::derive(run_status, verification_verdict);
        let outcome = EvalOutcomeKind::from_completion(run_status, verification_verdict, &failures);
        Self {
            metadata,
            outcome,
            run_status,
            verification_verdict,
            visible_state,
            changed_files: Vec::new(),
            tool_calls: Vec::new(),
            approval_count: 0,
            session_log_path: None,
            durable_stream_cursor: None,
            evidence: Vec::new(),
            required_actions: Vec::new(),
            failures,
        }
    }
}

/// One deterministic eval case runnable without a real provider or network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalCase {
    pub metadata: EvalRunMetadata,
    pub prompt: String,
    pub fixture: EvalWorkspaceFixture,
    pub script: EvalProviderScript,
}

impl EvalCase {
    /// Creates a deterministic eval case from explicit fixture and provider script material.
    #[must_use]
    pub fn deterministic(
        case_id: impl Into<String>,
        prompt: impl Into<String>,
        fixture: EvalWorkspaceFixture,
        script: EvalProviderScript,
    ) -> Self {
        let case_id = case_id.into();
        let fixture_id = fixture.fixture_id.clone();
        let run_id = format!("{case_id}-run");
        Self {
            metadata: EvalRunMetadata::deterministic(case_id, run_id, fixture_id),
            prompt: prompt.into(),
            fixture,
            script,
        }
    }
}

/// In-memory file fixture materialized into a temporary eval workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalWorkspaceFixture {
    pub fixture_id: EvalFixtureId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<PathBuf, String>,
}

impl EvalWorkspaceFixture {
    /// Creates an empty fixture with a stable id.
    #[must_use]
    pub fn new(fixture_id: impl Into<String>) -> Self {
        Self {
            fixture_id: fixture_id.into(),
            files: BTreeMap::new(),
        }
    }

    /// Adds one workspace-relative text file to the fixture.
    #[must_use]
    pub fn with_file(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.files.insert(path.into(), content.into());
        self
    }
}

/// Explicit fake-provider turn script consumed by [`EvalCaseRunner`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalProviderScript {
    pub steps: Vec<EvalProviderStep>,
}

impl EvalProviderScript {
    /// Creates a script from explicit provider steps.
    #[must_use]
    pub fn new(steps: Vec<EvalProviderStep>) -> Self {
        Self { steps }
    }
}

/// One fake-provider behavior step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalProviderStep {
    AssistantText {
        text: String,
    },
    ToolCall {
        tool_call_id: EvalToolCallId,
        tool_name: String,
        #[serde(default)]
        args_json: String,
    },
    ToolResultContinuation {
        tool_call_id: EvalToolCallId,
        text: String,
    },
    FinalAnswer {
        text: String,
    },
    ProviderError {
        message: String,
    },
}

/// Deterministic fake tool registry used by eval cases.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalFakeToolRegistry {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, EvalFakeToolAction>,
}

impl EvalFakeToolRegistry {
    /// Creates an empty fake tool registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one fake tool action.
    #[must_use]
    pub fn with_tool(mut self, name: impl Into<String>, action: EvalFakeToolAction) -> Self {
        self.tools.insert(name.into(), action);
        self
    }

    fn action_for(&self, name: &str) -> EvalFakeToolAction {
        self.tools
            .get(name)
            .cloned()
            .unwrap_or_else(|| EvalFakeToolAction::ToolError {
                message: format!("fake tool {name} is not registered"),
            })
    }
}

/// Deterministic result produced by one fake tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalFakeToolAction {
    ReadOnlySuccess {
        output: String,
    },
    ControlledWriteSuccess {
        path: PathBuf,
        content: String,
    },
    CheckSuccess {
        check_id: String,
    },
    CheckFailure {
        check_id: String,
        message: String,
    },
    PermissionDenied {
        message: String,
    },
    ToolError {
        message: String,
    },
    UnknownMutationMarker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
}

/// Options for deterministic eval execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalCaseRunnerOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
}

impl EvalCaseRunnerOptions {
    /// Uses a process tempdir path derived from the eval run id.
    #[must_use]
    pub fn temp_workspace() -> Self {
        Self {
            workspace_root: None,
        }
    }

    /// Uses a caller-provided workspace path.
    #[must_use]
    pub fn with_workspace_root(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: Some(workspace_root.into()),
        }
    }
}

impl Default for EvalCaseRunnerOptions {
    fn default() -> Self {
        Self::temp_workspace()
    }
}

/// In-memory deterministic eval runner skeleton.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalCaseRunner {
    pub tools: EvalFakeToolRegistry,
    pub options: EvalCaseRunnerOptions,
}

impl EvalCaseRunner {
    /// Creates a runner from a fake tool registry.
    #[must_use]
    pub fn new(tools: EvalFakeToolRegistry) -> Self {
        Self {
            tools,
            options: EvalCaseRunnerOptions::default(),
        }
    }

    /// Overrides runner options.
    #[must_use]
    pub fn with_options(mut self, options: EvalCaseRunnerOptions) -> Self {
        self.options = options;
        self
    }

    /// Runs one deterministic eval case and returns a structured result.
    ///
    /// # Errors
    ///
    /// Returns an error only for harness infrastructure failures such as workspace or session-log
    /// I/O. Provider and tool failures from the script are represented inside [`EvalResult`].
    pub fn run(&self, case: EvalCase) -> Result<EvalResult> {
        let workspace_root = self.workspace_root_for(&case.metadata.run_id);
        prepare_workspace(&workspace_root, &case.fixture)?;
        let session_log_path = workspace_root.join("session.jsonl");
        let mut capture = EvalEventCapture::new(&session_log_path)?;
        capture.record(
            "case_started",
            json!({
                "case_id": &case.metadata.case_id,
                "fixture_id": &case.metadata.fixture_id,
                "prompt": &case.prompt,
            }),
        )?;

        let mut run_status = RunStatus::Running;
        let mut verification_verdict = VerificationVerdict::NotApplicable;
        let mut changed_files = Vec::new();
        let mut tool_calls = Vec::new();
        let mut failures = Vec::new();
        let mut required_actions = Vec::new();

        for step in &case.script.steps {
            match step {
                EvalProviderStep::AssistantText { text } => {
                    capture.append_assistant(text)?;
                }
                EvalProviderStep::ToolResultContinuation { tool_call_id, text } => {
                    capture.record(
                        "tool_result_continuation",
                        json!({ "tool_call_id": tool_call_id, "text": text }),
                    )?;
                }
                EvalProviderStep::ToolCall {
                    tool_call_id,
                    tool_name,
                    args_json,
                } => {
                    capture.record(
                        "tool_call_started",
                        json!({
                            "tool_call_id": tool_call_id,
                            "tool_name": tool_name,
                            "args_json": args_json,
                        }),
                    )?;
                    let action = self.tools.action_for(tool_name);
                    apply_fake_tool_action(
                        &workspace_root,
                        tool_call_id,
                        tool_name,
                        action,
                        &mut changed_files,
                        &mut tool_calls,
                        &mut failures,
                        &mut verification_verdict,
                        &mut capture,
                    )?;
                    if let Some(terminal) = terminal_status_from_failures(&failures) {
                        run_status = terminal;
                        break;
                    }
                }
                EvalProviderStep::FinalAnswer { text } => {
                    capture.record("final_answer", json!({ "text": text }))?;
                    run_status = RunStatus::Completed;
                    break;
                }
                EvalProviderStep::ProviderError { message } => {
                    failures.push(EvalFailure::new(EvalFailureKind::Model, message.clone()));
                    capture.record("provider_error", json!({ "message": message }))?;
                    run_status = RunStatus::Failed;
                    break;
                }
            }
        }

        if run_status == RunStatus::Running {
            failures.push(EvalFailure::new(
                EvalFailureKind::Harness,
                "fake provider script ended without final answer",
            ));
            run_status = RunStatus::Interrupted;
        }
        if !changed_files.is_empty()
            && matches!(
                verification_verdict,
                VerificationVerdict::NotApplicable | VerificationVerdict::NotEvaluated
            )
        {
            verification_verdict = VerificationVerdict::Missing;
        }
        if !changed_files.is_empty()
            && matches!(
                verification_verdict,
                VerificationVerdict::Missing | VerificationVerdict::Stale
            )
        {
            required_actions.push(EvalRequiredAction::new(
                EvalRequiredActionKind::RunCheck,
                "run a verification check for the current workspace snapshot",
            ));
        }

        let mut result =
            EvalResult::from_completion(case.metadata, run_status, verification_verdict, failures);
        result.changed_files = changed_files;
        result.tool_calls = tool_calls;
        result.session_log_path = Some(session_log_path);
        result.durable_stream_cursor = capture.cursor;
        result.evidence = capture.evidence;
        result.required_actions = required_actions;
        Ok(result)
    }

    fn workspace_root_for(&self, run_id: &str) -> PathBuf {
        self.options.workspace_root.clone().unwrap_or_else(|| {
            std::env::temp_dir()
                .join("sigil-eval")
                .join(sanitize_path_component(run_id))
        })
    }
}

fn prepare_workspace(root: &Path, fixture: &EvalWorkspaceFixture) -> Result<()> {
    if root.exists() {
        fs::remove_dir_all(root)
            .with_context(|| format!("failed to reset eval workspace {}", root.display()))?;
    }
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create eval workspace {}", root.display()))?;
    for (path, content) in &fixture.files {
        let full_path = root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&full_path, content)
            .with_context(|| format!("failed to write fixture file {}", full_path.display()))?;
    }
    Ok(())
}

fn apply_fake_tool_action(
    workspace_root: &Path,
    tool_call_id: &str,
    tool_name: &str,
    action: EvalFakeToolAction,
    changed_files: &mut Vec<PathBuf>,
    tool_calls: &mut Vec<EvalToolCallSummary>,
    failures: &mut Vec<EvalFailure>,
    verification_verdict: &mut VerificationVerdict,
    capture: &mut EvalEventCapture,
) -> Result<()> {
    match action {
        EvalFakeToolAction::ReadOnlySuccess { output } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            capture.record(
                "tool_result",
                json!({ "tool_call_id": tool_call_id, "status": "succeeded", "output": output }),
            )?;
        }
        EvalFakeToolAction::ControlledWriteSuccess { path, content } => {
            let full_path = workspace_root.join(&path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&full_path, content)
                .with_context(|| format!("failed to write {}", full_path.display()))?;
            changed_files.push(path.clone());
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            let write_evidence = capture.record(
                "controlled_write",
                json!({
                    "tool_call_id": tool_call_id,
                    "path": &path,
                }),
            )?;
            if *verification_verdict == VerificationVerdict::Passed {
                *verification_verdict = VerificationVerdict::Stale;
                let mut failure = EvalFailure::new(
                    EvalFailureKind::VerificationStale,
                    "controlled write invalidated the previous verification receipt",
                );
                failure.evidence.push(write_evidence);
                failures.push(failure);
            } else {
                *verification_verdict = VerificationVerdict::Missing;
            }
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "changed_file": &path,
                }),
            )?;
        }
        EvalFakeToolAction::CheckSuccess { check_id } => {
            *verification_verdict = VerificationVerdict::Passed;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "check_id": check_id,
                    "workspace_snapshot_id": format!("snapshot-{}", changed_files.len()),
                }),
            )?;
        }
        EvalFakeToolAction::CheckFailure { check_id, message } => {
            *verification_verdict = VerificationVerdict::Failed;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Failed,
            ));
            failures.push(EvalFailure::new(
                EvalFailureKind::VerificationFailed,
                message.clone(),
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "failed",
                    "check_id": check_id,
                    "message": message,
                }),
            )?;
        }
        EvalFakeToolAction::PermissionDenied { message } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Denied,
            ));
            failures.push(EvalFailure::new(
                EvalFailureKind::PermissionDenied,
                message.clone(),
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "denied",
                    "message": message,
                }),
            )?;
        }
        EvalFakeToolAction::ToolError { message } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Failed,
            ));
            failures.push(EvalFailure::new(EvalFailureKind::Tool, message.clone()));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "failed",
                    "message": message,
                }),
            )?;
        }
        EvalFakeToolAction::UnknownMutationMarker { path } => {
            if let Some(path) = path {
                changed_files.push(path.clone());
            }
            *verification_verdict = VerificationVerdict::Stale;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            failures.push(EvalFailure::new(
                EvalFailureKind::VerificationStale,
                "unknown workspace mutation invalidated verification",
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "unknown_mutation": true,
                }),
            )?;
        }
    }
    Ok(())
}

fn tool_summary(
    tool_call_id: &str,
    tool_name: &str,
    status: EvalToolCallStatus,
) -> EvalToolCallSummary {
    EvalToolCallSummary {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        status,
    }
}

fn terminal_status_from_failures(failures: &[EvalFailure]) -> Option<RunStatus> {
    failures.last().and_then(|failure| match failure.kind {
        EvalFailureKind::PermissionDenied => Some(RunStatus::Blocked),
        EvalFailureKind::Tool | EvalFailureKind::VerificationFailed => Some(RunStatus::Failed),
        _ => None,
    })
}

struct EvalEventCapture {
    store: JsonlSessionStore,
    evidence: Vec<EvalEvidenceRef>,
    cursor: Option<ProjectionCursor>,
}

impl EvalEventCapture {
    fn new(session_log_path: &Path) -> Result<Self> {
        Ok(Self {
            store: JsonlSessionStore::new(session_log_path)?,
            evidence: Vec::new(),
            cursor: None,
        })
    }

    fn append_assistant(&mut self, text: &str) -> Result<()> {
        self.record("assistant_text", json!({ "text": text }))
            .map(|_| ())
    }

    fn record(&mut self, label: &str, payload: serde_json::Value) -> Result<EvalEvidenceRef> {
        let event = self
            .store
            .append_session_entry_event(&SessionLogEntry::Control(ControlEntry::Note {
                kind: "eval_harness".to_owned(),
                data: json!({
                    "label": label,
                    "payload": payload,
                }),
            }))?;
        Ok(self.record_event(label, event))
    }

    fn record_event(&mut self, label: &str, event: crate::StoredEvent) -> EvalEvidenceRef {
        self.cursor = Some(ProjectionCursor {
            session_id: event.session_id.clone(),
            projection_schema_version: SESSION_ENTRY_PROJECTION_SCHEMA_VERSION,
            last_applied_stream_sequence: event.stream_sequence,
            last_applied_event_id: event.event_id.clone(),
            last_applied_record_checksum: event.record_checksum.clone(),
        });
        let evidence = EvalEvidenceRef::durable_event(label, event.event_id);
        self.evidence.push(evidence.clone());
        evidence
    }
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "run".to_owned()
    } else {
        sanitized
    }
}
