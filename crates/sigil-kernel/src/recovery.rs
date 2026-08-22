//! Provider-neutral recoverability facts shared by Plan, Task, tool and adapter owners.
//!
//! A blocker is durable evidence for a boundary that cannot continue yet. It is deliberately
//! distinct from a terminal failure: callers may narrow a blocker to their own boundary, but may
//! not infer a wider `Failed` state from its text or concrete error type.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{TaskId, TaskStepId, safe_persistence_text};

/// Current recovery/blocker payload schema.
pub const RECOVERY_BLOCKER_SCHEMA_VERSION: u16 = 1;

/// Product-neutral owner of a durable recovery blocker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryDomainV1 {
    PlanMaterialization,
    TaskExecution,
    WorkspaceMutation,
    EffectReconciliation,
    ProviderTurn,
    AdapterDelivery,
    Projection,
    LocalOperation,
}

/// Adapter that could not deliver an already committed public event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AdapterKindV1 {
    Http,
    Desktop,
    Tui,
    Cli,
}

/// Derived view whose rebuild can degrade without changing a domain terminal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionKindV1 {
    Session,
    Task,
    PublicEvent,
    DesktopTimeline,
}

/// Smallest durable owner that may be paused by a failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    rename_all = "snake_case",
    tag = "kind",
    content = "value",
    deny_unknown_fields
)]
pub enum FailureScopeV1 {
    LocalOperation {
        operation_id: String,
    },
    ToolEffect {
        task_id: Option<TaskId>,
        step_id: Option<TaskStepId>,
        effect_id: String,
    },
    Step {
        task_id: TaskId,
        step_id: TaskStepId,
    },
    Task {
        task_id: TaskId,
    },
    Run {
        logical_run_id: String,
    },
    AdapterDelivery {
        run_id: String,
        adapter: AdapterKindV1,
    },
    Projection {
        projection: ProjectionKindV1,
    },
}

/// Recovery path selected by the lowest boundary owner.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoverabilityV1 {
    Continue,
    RetrySameBoundary,
    RebaseWorkspace,
    Replan,
    AwaitUser,
    AwaitResource,
    ReconcileEffect,
    Cancelled,
    Irrecoverable,
}

/// Durable settlement state of the effect protected by a recovery decision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectSettlementV1 {
    NotStarted,
    ConfirmedNoEffect,
    Applied,
    PartiallyApplied,
    OutcomeUncertain,
}

/// Explicit safe actions that can be offered for a blocker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryActionV1 {
    Retry,
    RebaseWorkspace,
    Replan,
    ReconcileEffect,
    ProvideInput,
    RefreshResource,
    Cancel,
}

/// One bounded, durable reason why an owner is blocked from moving forward.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecoveryBlockerV1 {
    pub schema_version: u16,
    pub blocker_id: String,
    pub domain: RecoveryDomainV1,
    pub scope: FailureScopeV1,
    pub recoverability: RecoverabilityV1,
    pub settlement: EffectSettlementV1,
    pub reason_code: String,
    pub safe_summary: String,
    pub evidence_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_actions: Vec<RecoveryActionV1>,
    pub created_at_ms: u64,
}

impl RecoveryBlockerV1 {
    /// Ensures this boundary fact is bounded, safe to persist, and internally consistent.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != RECOVERY_BLOCKER_SCHEMA_VERSION {
            bail!("unsupported recovery blocker schema version");
        }
        validate_identity("recovery blocker id", &self.blocker_id)?;
        validate_scope(&self.scope)?;
        validate_reason_code(&self.reason_code)?;
        if self.safe_summary.is_empty()
            || self.safe_summary.len() > 1_024
            || safe_persistence_text(&self.safe_summary) != self.safe_summary
        {
            bail!("recovery blocker summary is not bounded safe text");
        }
        validate_digest(&self.evidence_digest)?;
        if let Some(effect_id) = self.effect_id.as_deref() {
            validate_identity("recovery effect id", effect_id)?;
        }
        if self.available_actions.len() > 8 {
            bail!("recovery blocker action count exceeds maximum");
        }
        for (index, action) in self.available_actions.iter().enumerate() {
            if self.available_actions[..index].contains(action) {
                bail!("recovery blocker repeats an available action");
            }
        }
        if self.created_at_ms == 0 {
            bail!("recovery blocker creation timestamp must be non-zero");
        }
        match (&self.scope, self.effect_id.as_deref()) {
            (FailureScopeV1::ToolEffect { effect_id, .. }, Some(bound)) if effect_id == bound => {}
            (FailureScopeV1::ToolEffect { .. }, _) => {
                bail!("tool effect recovery blocker must bind its exact effect id");
            }
            (_, Some(_)) => bail!("non-effect recovery blocker may not bind an effect id"),
            (_, None) => {}
        }
        Ok(())
    }

    /// Returns a product-safe view with no raw platform/provider diagnostic detail.
    #[must_use]
    pub fn public_view(&self) -> PublicRecoveryBlockerV1 {
        PublicRecoveryBlockerV1 {
            blocker_id: self.blocker_id.clone(),
            domain: self.domain,
            scope: self.scope.clone(),
            recoverability: self.recoverability,
            settlement: self.settlement,
            reason_code: self.reason_code.clone(),
            safe_summary: self.safe_summary.clone(),
            available_actions: self.available_actions.clone(),
        }
    }
}

/// Renderer and machine-protocol safe blocker view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicRecoveryBlockerV1 {
    pub blocker_id: String,
    pub domain: RecoveryDomainV1,
    pub scope: FailureScopeV1,
    pub recoverability: RecoverabilityV1,
    pub settlement: EffectSettlementV1,
    pub reason_code: String,
    pub safe_summary: String,
    pub available_actions: Vec<RecoveryActionV1>,
}

/// Typed durable blocker lifecycle entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecoveryBlockerRaisedV1 {
    pub blocker: RecoveryBlockerV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecoveryBlockerResolutionStartedV1 {
    pub blocker_id: String,
    pub action: RecoveryActionV1,
    pub attempt_id: String,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecoveryBlockerResolvedV1 {
    pub blocker_id: String,
    pub resolution_receipt_digest: String,
    pub resolved_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecoveryBlockerSupersededV1 {
    pub blocker_id: String,
    pub successor_blocker_id: String,
    pub superseded_at_ms: u64,
}

impl RecoveryBlockerRaisedV1 {
    pub fn validate(&self) -> Result<()> {
        self.blocker.validate()
    }
}

impl RecoveryBlockerResolutionStartedV1 {
    pub fn validate(&self) -> Result<()> {
        validate_identity("recovery blocker id", &self.blocker_id)?;
        validate_identity("recovery resolution attempt id", &self.attempt_id)?;
        if self.started_at_ms == 0 {
            bail!("recovery resolution start timestamp must be non-zero");
        }
        Ok(())
    }
}

impl RecoveryBlockerResolvedV1 {
    pub fn validate(&self) -> Result<()> {
        validate_identity("recovery blocker id", &self.blocker_id)?;
        validate_digest(&self.resolution_receipt_digest)?;
        if self.resolved_at_ms == 0 {
            bail!("recovery resolution timestamp must be non-zero");
        }
        Ok(())
    }
}

impl RecoveryBlockerSupersededV1 {
    pub fn validate(&self) -> Result<()> {
        validate_identity("recovery blocker id", &self.blocker_id)?;
        validate_identity("recovery successor blocker id", &self.successor_blocker_id)?;
        if self.blocker_id == self.successor_blocker_id {
            bail!("recovery blocker may not supersede itself");
        }
        if self.superseded_at_ms == 0 {
            bail!("recovery supersession timestamp must be non-zero");
        }
        Ok(())
    }
}

/// Durable evidence for an interrupted owner. This is not an irrecoverable failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct InterruptionReceiptV1 {
    pub scope: FailureScopeV1,
    pub reason_code: String,
    pub receipt_digest: String,
}

/// Durable evidence required before a domain owner may become failed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IrrecoverableFailureV1 {
    pub scope: FailureScopeV1,
    pub reason_code: String,
    pub evidence_digest: String,
    pub safe_summary: String,
}

/// Classified result from the lowest owner allowed to decide a recovery boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryOutcomeV1<T> {
    Completed(T),
    Blocked(RecoveryBlockerV1),
    Interrupted(InterruptionReceiptV1),
    Failed(IrrecoverableFailureV1),
}

fn validate_scope(scope: &FailureScopeV1) -> Result<()> {
    match scope {
        FailureScopeV1::LocalOperation { operation_id } => {
            validate_identity("local operation id", operation_id)
        }
        FailureScopeV1::ToolEffect {
            task_id,
            step_id,
            effect_id,
        } => {
            validate_identity("tool effect id", effect_id)?;
            if step_id.is_some() && task_id.is_none() {
                bail!("tool effect step scope requires a task id");
            }
            Ok(())
        }
        FailureScopeV1::Step { .. }
        | FailureScopeV1::Task { .. }
        | FailureScopeV1::Projection { .. } => Ok(()),
        FailureScopeV1::Run { logical_run_id } => {
            validate_identity("logical run id", logical_run_id)
        }
        FailureScopeV1::AdapterDelivery { run_id, .. } => {
            validate_identity("adapter run id", run_id)
        }
    }
}

fn validate_identity(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 192
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        bail!("{label} is not a bounded stable identity");
    }
    Ok(())
}

fn validate_reason_code(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!("recovery reason code is invalid");
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<()> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("recovery evidence digest is not sha256");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/recovery_tests.rs"]
mod tests;
