use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    Agent, AgentRunDisposition, AgentRunInput, AgentRunOptions, AgentRunPurpose, ControlEntry,
    ConversationRoute, ConversationRouteDecisionProjection, ConversationTurnRef, EventHandler,
    JsonlSessionStore, ModelMessage, PlanApprovalPermission, PlanApprovalScope, PlanCompileInputV1,
    PlanDecision, PlanDecisionActor, PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanId,
    PlanPermissionGrantedEntry, PlanReviewAttemptEntry, PlanReviewAttemptId,
    PlanReviewAttemptStatus, PlanReviewId, PlanReviewProjection, PlanReviewSource,
    PlanReviewTerminalReason, PlanSourceRef, PlanTaskStartMode, ProviderPhysicalAttemptOutcome,
    PublicEventOutboxEntryV1, PublicRunEvent, PublicRunEventKind, RunEvent, Session,
    SessionLogEntry, SessionRef, StartPlanReviewAction, TaskCreatedFromPlanEntry, TaskId,
    TaskRunEntry, TaskRunStatus, TaskStepId, build_workspace_snapshot,
    plain_text_plan_draft_entry_with_plan_id, plan_review_attempt_id_for_review,
    plan_review_attempt_id_for_revision_ordinal, plan_review_child_session_ref,
    plan_review_finalizer_session_ref, plan_review_id_for_explicit_command,
    plan_review_no_draft_retry_contract_material, plan_review_plan_id_for_attempt,
    plan_review_system_prompt_contract_material, plan_task_input_from_draft, plan_text_hash,
    safe_persistence_text, stable_event_uuid, stable_workspace_id, task_id_from_plan_draft,
};

use sigil_kernel::ApprovalHandler;

#[cfg(test)]
use sigil_kernel::{
    IntentAcceptanceAuthorityV1, IntentAdmissionContextV1, IntentStackId, PlanApprovalExpiry,
    TaskPlanEntry, TaskPlanStatus, admit_suggested_decomposition,
    append_task_intent_plan_admission_with_step_contracts, bind_task_plan_intents,
    task_plan_from_plan_draft,
};

use crate::managed_artifact_store::ManagedArtifactStoreLeaseV1;
use crate::managed_storage_writer::{
    ManagedExistingSessionLogMutationLeaseV1, ManagedExistingSessionLogReadLeaseV1,
    ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
};
use crate::{RootConfig, attach_session_url_capability_store};

const PLAN_REVIEW_FINALIZATION_MAX_MODEL_TURNS: usize = 1;
const MAX_MANAGED_PLAN_REVIEW_RECOVERY_LOG_BYTES: usize = 16 * 1024 * 1024;

/// Host-owned outcome of one plan review run.
#[derive(Debug, Clone)]
pub enum PlanReviewRunOutcome {
    DraftReady {
        draft: Box<PlanDraftCreatedEntry>,
    },
    AwaitingUserInput {
        request: Box<sigil_kernel::PublicUserInputRequestV1>,
    },
    CompletedWithoutDraft,
    Cancelled,
    Interrupted(String),
    Blocked(String),
    Paused(String),
    Failed(String),
    SubmitOnlyProtocolViolation(String),
}

/// Keeps child-owned controls in the child session while retaining the parent's existing
/// forwarding policy for non-control run events.
///
/// A plan-review child has a distinct writer and session scope. Forwarding `commit_controls` to
/// the parent application's bridge would claim an atomic source/outbox bundle across those two
/// durable stores, which is not an authority this coordinator has.
struct PlanReviewChildEventHandler<'a, H> {
    inner: &'a mut H,
}

impl<H> EventHandler for PlanReviewChildEventHandler<'_, H>
where
    H: EventHandler,
{
    fn begin_live_attempt(&mut self, physical_attempt_id: &str) -> Result<()> {
        self.inner.begin_live_attempt(physical_attempt_id)
    }

    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::Control(_) => Ok(()),
            event => self.inner.handle(event),
        }
    }
}

/// Host-bound request describing one read-only plan review run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewRunRequest {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source: PlanReviewSource,
    pub source_turn: ConversationTurnRef,
    pub route_decision_id: Option<sigil_kernel::ConversationRouteDecisionId>,
    pub child_session_ref: SessionRef,
    pub finalizer_session_ref: SessionRef,
    pub revision_request_id: Option<sigil_kernel::UserInputRequestId>,
    pub attempt_ordinal: u32,
    pub base_plan_id: Option<PlanId>,
    pub base_plan_hash: Option<String>,
    /// Original safe objective for an explicit `/plan` lifecycle. Automatic plan reviews retain
    /// their objective exclusively in the durable source user turn.
    pub explicit_objective: Option<String>,
    pub objective: String,
    /// Exact workspace snapshot the draft will be bound to; direct promotion requires the
    /// workspace to be unchanged between review and `Run plan`.
    pub workspace_snapshot_id: Option<String>,
}

/// Exact compare-and-swap admission for resuming a terminal Plan review attempt.
///
/// Adapters may carry this value opaquely, but the runtime rechecks every binding against the
/// durable parent before allocating a successor. A candidate hash is required when the current
/// terminal attempt has a complete preserved candidate; a retry cannot silently replace that
/// body with a newly generated result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewRetryCommand {
    pub command_id: String,
    pub plan_review_id: PlanReviewId,
    pub expected_attempt_id: PlanReviewAttemptId,
    pub expected_status: PlanReviewAttemptStatus,
    pub expected_durable_frontier: u64,
    pub expected_context_digest: String,
    pub candidate_hash: Option<String>,
    pub blocker_id: Option<String>,
    pub workspace_snapshot_id: Option<String>,
}

/// Receipt for one retry CAS. Replaying the same command returns the same successor request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewRetryReceipt {
    pub command_id: String,
    pub predecessor_attempt_id: PlanReviewAttemptId,
    pub successor_attempt_id: PlanReviewAttemptId,
    pub request: PlanReviewRunRequest,
    pub idempotent_replay: bool,
}

/// Current-schema child resource scope. Parent session stores, artifact stores and writer leases
/// are intentionally not exposed to the child; this bundle is the only resource surface passed
/// across the plan-review boundary.
pub struct CurrentSchemaPlanReviewChildResourceBundleV1 {
    session_log_path: PathBuf,
    scope_id: String,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
    artifact_store: sigil_kernel::ToolArtifactStore,
    tool_authority: Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>,
    session_log_lease: ManagedPlanReviewSessionLogLeaseV1,
    artifact_lease: ManagedArtifactStoreLeaseV1,
}

/// Recovery-only handle for an already existing `research/0` child session.
///
/// Unlike [`CurrentSchemaPlanReviewChildResourceBundleV1`], this handle cannot initialize a
/// namespace, write a session record, or open an artifact store.  It is intentionally usable
/// only by runtime composition, where its opaque child bytes are checked against the parent
/// attempt before the recovery admission is settled.
pub struct CurrentSchemaPlanReviewRecoveredSessionV1 {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    session_log_lease: Option<ManagedExistingSessionLogReadLeaseV1>,
}

impl std::fmt::Debug for CurrentSchemaPlanReviewRecoveredSessionV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CurrentSchemaPlanReviewRecoveredSessionV1")
            .field("session_log", &"<opaque>")
            .finish_non_exhaustive()
    }
}

impl CurrentSchemaPlanReviewRecoveredSessionV1 {
    fn entries(&self, expected_session_scope_id: &str) -> Result<Vec<SessionLogEntry>> {
        let lease = self
            .session_log_lease
            .as_ref()
            .context("managed plan-review recovery session lease was already settled")?;
        let bytes = self
            .writer
            .read_existing_session_log_bytes(lease, MAX_MANAGED_PLAN_REVIEW_RECOVERY_LOG_BYTES)
            .map_err(|error| {
                anyhow!("failed to read managed plan-review recovery session: {error}")
            })?;
        let records = JsonlSessionStore::read_event_records_from_validated_bytes(&bytes)
            .context("managed plan-review recovery session has an invalid durable stream")?;
        if records
            .iter()
            .any(|record| record.session_id() != expected_session_scope_id)
        {
            bail!("managed plan-review recovery envelope belongs to another child scope");
        }
        records
            .iter()
            .map(sigil_kernel::SessionStreamRecord::session_log_entry)
            .filter_map(|entry| entry.transpose())
            .collect()
    }

    fn finish(mut self) -> Result<()> {
        let lease = self
            .session_log_lease
            .take()
            .context("managed plan-review recovery session lease was already settled")?;
        self.writer
            .finalize_existing_session_log_recovery(lease)
            .map(|_| ())
            .map_err(|error| {
                anyhow!("plan-review child recovery session settlement failed: {error}")
            })
    }
}

impl Drop for CurrentSchemaPlanReviewRecoveredSessionV1 {
    fn drop(&mut self) {
        let Some(lease) = self.session_log_lease.take() else {
            return;
        };
        if let Err(error) = self.writer.finalize_existing_session_log_recovery(lease) {
            tracing::error!(%error, "failed to settle plan-review child recovery session admission");
        }
    }
}

/// Mutation-only handle for an already existing `research/0` child session.
///
/// This is intentionally narrower than the normal plan-review resource bundle: accepting an
/// already requested input requires its authoritative SessionLog, but not a new namespace,
/// artifact store, marker, or URL capability.  The same require-existing admission remains live
/// through the child append and its explicit settlement.
pub struct CurrentSchemaPlanReviewExistingResearchSessionV1 {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    session_log_lease: Option<ManagedExistingSessionLogMutationLeaseV1>,
}

impl std::fmt::Debug for CurrentSchemaPlanReviewExistingResearchSessionV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CurrentSchemaPlanReviewExistingResearchSessionV1")
            .field("session_log", &"<opaque>")
            .finish_non_exhaustive()
    }
}

impl CurrentSchemaPlanReviewExistingResearchSessionV1 {
    /// Opens, verifies, and mutates the exact child session only while the writer retains the
    /// existing authority physical lock. The Session itself cannot escape this callback; the
    /// parent receives only the typed decision receipt after the child write has completed.
    fn with_child_session(
        &self,
        parent: &Session,
        expected_session_scope_id: &str,
        operation: impl FnOnce(&mut Session) -> Result<sigil_kernel::UserInputDecisionReceiptV1>,
    ) -> Result<sigil_kernel::UserInputDecisionReceiptV1> {
        let lease = self
            .session_log_lease
            .as_ref()
            .context("managed plan-review child mutation lease was already settled")?;
        self.writer
            .with_existing_session_log_mutation(lease, |store| {
                let records = store
                    .read_event_records_writer()
                    .context("failed to recover managed plan-review child session tail")?;
                if records.is_empty() {
                    bail!("managed plan-review child session log is empty");
                }
                if records
                    .iter()
                    .any(|record| record.session_id() != expected_session_scope_id)
                {
                    bail!(
                        "managed plan-review child mutation envelope belongs to another child scope"
                    );
                }
                let mut session = Session::load_from_store(
                    parent.provider_name(),
                    parent.model_name(),
                    store.clone(),
                )
                .context("failed to load managed plan-review child session")?;
                if session.session_scope_id() != expected_session_scope_id {
                    bail!(
                        "managed plan-review child session scope does not match the pending input"
                    );
                }
                operation(&mut session)
            })
            .context("failed to reopen managed plan-review child session")
    }

    fn finish(mut self) -> Result<()> {
        let lease = self
            .session_log_lease
            .take()
            .context("managed plan-review child mutation lease was already settled")?;
        self.writer
            .finalize_existing_session_log_mutation(lease)
            .map(|_| ())
            .map_err(|error| anyhow!("plan-review child mutation settlement failed: {error}"))
    }
}

impl Drop for CurrentSchemaPlanReviewExistingResearchSessionV1 {
    fn drop(&mut self) {
        let Some(lease) = self.session_log_lease.take() else {
            return;
        };
        if let Err(error) = self.writer.finalize_existing_session_log_mutation(lease) {
            tracing::error!(%error, "failed to settle plan-review child mutation admission");
        }
    }
}

impl std::fmt::Debug for CurrentSchemaPlanReviewChildResourceBundleV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CurrentSchemaPlanReviewChildResourceBundleV1")
            .field("session_log_path", &"<opaque>")
            .finish_non_exhaustive()
    }
}

impl CurrentSchemaPlanReviewChildResourceBundleV1 {
    /// Returns the managed child-session path for runtime-owned composition only.
    ///
    /// This is intentionally crate-private: product surfaces receive neither this path nor the
    /// underlying authority.  The coordinator uses it solely after a provisioned bundle has
    /// bound the exact scope.
    pub(crate) fn session_log_path(&self) -> &Path {
        &self.session_log_path
    }

    fn artifact_store(&self) -> sigil_kernel::ToolArtifactStore {
        self.artifact_store.clone()
    }

    /// Opaque child scope used to bind both the child session log and artifact store.
    #[must_use]
    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    /// Exact authority generation captured at child admission.
    #[must_use]
    pub const fn authority_generation(&self) -> sigil_kernel::resource::AuthorityGeneration {
        self.authority_generation
    }

    fn tool_authority(&self) -> Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1> {
        Arc::clone(&self.tool_authority)
    }

    /// Verifies that every capability in this bundle belongs to the same deterministic child
    /// admission.  The coordinator must not let a custom provisioner substitute a session log,
    /// artifact facade, or authority generation from another child scope.
    fn validate_for(
        &self,
        request: &PlanReviewRunRequest,
        kind: PlanReviewChildResourceKindV1,
        ordinal: u32,
    ) -> Result<()> {
        validate_managed_plan_review_request_binding(request, kind, ordinal)?;
        validate_authority_generation(self.authority_generation)?;

        let expected_scope_id = format!("{}-{}", request.child_logical_run_id(), kind.tag());
        if self.scope_id != expected_scope_id {
            bail!(
                "managed plan-review child bundle scope does not match its request: expected {expected_scope_id}"
            );
        }

        let expected_key = plan_review_child_resource_key(request, kind, ordinal);
        let expected_session_log_dir = self
            .session_log_lease
            .writer
            .managed_named_leaf_path(StorageWriterChannelV1::SessionLog, &expected_key)
            .map_err(|error| {
                anyhow!("managed child session-log path validation failed: {error}")
            })?;
        let expected_session_log_path = self.session_log_lease.path().join("records.jsonl");
        if self.session_log_path != expected_session_log_path
            || self.session_log_lease.path() != expected_session_log_dir
        {
            bail!("managed plan-review child bundle session-log path is not authority-bound");
        }
        if self.artifact_store.session_log_path() != self.session_log_path {
            bail!("managed plan-review child artifact store is bound to another session log");
        }
        if self.artifact_store.session_scope_id_hash()
            != sigil_kernel::stable_event_hash(self.scope_id.as_bytes())
        {
            bail!("managed plan-review child artifact store scope does not match its bundle");
        }
        if !Arc::ptr_eq(
            &self.session_log_lease.writer,
            &self.artifact_lease.writer(),
        ) {
            bail!("managed plan-review child resources use different authority writers");
        }
        Ok(())
    }

    /// Settles both child namespaces explicitly. `Drop` remains only a last-resort fallback for
    /// cancellation, panic or process teardown; normal coordinator exits must surface settlement
    /// failure to the caller so the parent can record a typed terminal failure.
    pub(crate) fn finish(self) -> Result<()> {
        let artifact_result = self.artifact_lease.finalize();
        let session_result = self.session_log_lease.finish();
        match (artifact_result, session_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(artifact), Ok(())) => {
                Err(artifact.context("plan-review child artifact namespace settlement failed"))
            }
            (Ok(()), Err(session)) => {
                Err(session.context("plan-review child session-log settlement failed"))
            }
            (Err(artifact), Err(session)) => Err(anyhow!(
                "plan-review child resource settlement failed: artifact={artifact:#}; session-log={session:#}"
            )),
        }
    }
}

fn combine_child_resource_settlement<T>(result: Result<T>, settlement: Result<()>) -> Result<T> {
    match (result, settlement) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(value), Err(error)) => {
            // The Plan outcome is the domain owner. Releasing a child lease is a separate
            // resource concern and must not rewrite a confirmed draft/no-plan result into a
            // provider failure. The resource owner retains the failure in its own diagnostics.
            tracing::error!(%error, "plan-review child resource settlement failed after domain outcome");
            Ok(value)
        }
        (Err(error), Err(settlement_error)) => Err(error.context(format!(
            "plan-review child resource settlement also failed: {settlement_error:#}"
        ))),
    }
}

/// Guard for the child session-log namespace. Explicit `finish` is the normal path; `Drop` is only
/// the last-resort cleanup when cancellation, panic or process teardown interrupts the owner.
struct ManagedPlanReviewSessionLogLeaseV1 {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    lease: Option<ManagedStorageWriterLeaseV1>,
}

impl ManagedPlanReviewSessionLogLeaseV1 {
    fn acquire(writer: Arc<ManagedStorageWriterAdapterV1>, key: &str) -> Result<Self> {
        let lease = writer
            .acquire_named(StorageWriterChannelV1::SessionLog, key)
            .map_err(|error| anyhow!("plan-review child session-log admission failed: {error}"))?;
        Ok(Self {
            writer,
            lease: Some(lease),
        })
    }

    fn path(&self) -> &Path {
        self.lease
            .as_ref()
            .expect("child session-log lease must remain live")
            .path()
    }

    fn finish(mut self) -> Result<()> {
        let lease = self
            .lease
            .take()
            .context("plan-review child session-log lease was already settled")?;
        self.writer
            .finalize(lease)
            .map(|_| ())
            .map_err(|error| anyhow!("plan-review child session-log finalize failed: {error}"))
    }
}

impl Drop for ManagedPlanReviewSessionLogLeaseV1 {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        if let Err(error) = self.writer.finalize(lease) {
            tracing::error!(%error, "failed to finalize plan-review child session-log namespace");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PlanReviewChildResourceKindV1 {
    Research,
    Finalizer,
}

impl PlanReviewChildResourceKindV1 {
    const fn tag(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Finalizer => "finalizer",
        }
    }
}

/// Runtime-owned provisioning port for current-schema plan-review child scopes.
pub trait PlanReviewChildResourceProvisionerV1: Send + Sync {
    fn provision(
        &self,
        request: &PlanReviewRunRequest,
        kind: PlanReviewChildResourceKindV1,
        ordinal: u32,
    ) -> Result<CurrentSchemaPlanReviewChildResourceBundleV1>;

    /// Reopens only an already-admitted `research/0` child session for recovery.
    ///
    /// This is deliberately separate from [`Self::provision`]: a missing or unsafe child must
    /// fail closed instead of being initialized as a fresh session during attention recovery.
    fn recover_research_session(
        &self,
        request: &PlanReviewRunRequest,
    ) -> Result<CurrentSchemaPlanReviewRecoveredSessionV1>;

    /// Reopens only an already-admitted `research/0` child SessionLog for an accepted input
    /// mutation.  It must not create a missing child, artifact namespace, or admission marker.
    fn mutate_research_session(
        &self,
        request: &PlanReviewRunRequest,
    ) -> Result<CurrentSchemaPlanReviewExistingResearchSessionV1>;
}

/// Production implementation backed by the same composed writer, artifact authority and kernel
/// tool authority as the parent application.
pub struct RuntimePlanReviewChildResourceProvisionerV1 {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    tool_authority: Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>,
    authority_generation: sigil_kernel::resource::AuthorityGeneration,
}

impl RuntimePlanReviewChildResourceProvisionerV1 {
    pub(crate) fn new_with_generation(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        tool_authority: Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>,
        authority_generation: sigil_kernel::resource::AuthorityGeneration,
    ) -> Self {
        Self {
            writer,
            tool_authority,
            authority_generation,
        }
    }
}

impl PlanReviewChildResourceProvisionerV1 for RuntimePlanReviewChildResourceProvisionerV1 {
    fn provision(
        &self,
        request: &PlanReviewRunRequest,
        kind: PlanReviewChildResourceKindV1,
        ordinal: u32,
    ) -> Result<CurrentSchemaPlanReviewChildResourceBundleV1> {
        validate_managed_plan_review_request_binding(request, kind, ordinal)?;
        validate_authority_generation(self.authority_generation)?;
        let key = plan_review_child_resource_key(request, kind, ordinal);
        let session_log_lease =
            ManagedPlanReviewSessionLogLeaseV1::acquire(Arc::clone(&self.writer), &key)?;
        let session_log_path = session_log_lease.path().join("records.jsonl");
        let scope_id = format!("{}-{}", request.child_logical_run_id(), kind.tag());
        let artifact_lease = ManagedArtifactStoreLeaseV1::acquire_with_session_path(
            Arc::clone(&self.writer),
            &key,
            &scope_id,
            session_log_path.clone(),
        );
        let artifact_lease = match artifact_lease {
            Ok(lease) => lease,
            Err(error) => {
                // The bundle is not constructed yet, so its explicit finish method cannot run.
                // Settle the already-admitted session-log namespace here and surface both errors
                // instead of relying on Drop to silently leave a pending admission behind.
                let session_settlement = session_log_lease.finish();
                return match session_settlement {
                    Ok(()) => Err(anyhow!(
                        "plan-review child artifact admission failed: {error}"
                    )),
                    Err(settlement) => Err(anyhow!(
                        "plan-review child artifact admission failed: {error}; session-log settlement also failed: {settlement:#}"
                    )),
                };
            }
        };
        let bundle = CurrentSchemaPlanReviewChildResourceBundleV1 {
            session_log_path,
            scope_id,
            authority_generation: self.authority_generation,
            artifact_store: artifact_lease.store(),
            tool_authority: Arc::clone(&self.tool_authority),
            session_log_lease,
            artifact_lease,
        };
        if let Err(error) = bundle.validate_for(request, kind, ordinal) {
            return combine_child_resource_settlement(Err(error), bundle.finish());
        }
        Ok(bundle)
    }

    fn recover_research_session(
        &self,
        request: &PlanReviewRunRequest,
    ) -> Result<CurrentSchemaPlanReviewRecoveredSessionV1> {
        validate_managed_plan_review_request_binding(
            request,
            PlanReviewChildResourceKindV1::Research,
            0,
        )?;
        let key =
            plan_review_child_resource_key(request, PlanReviewChildResourceKindV1::Research, 0);
        let session_log_lease = self
            .writer
            .acquire_existing_session_log_for_recovery(&key)
            .map_err(|error| anyhow!("plan-review child recovery admission failed: {error}"))?;
        Ok(CurrentSchemaPlanReviewRecoveredSessionV1 {
            writer: Arc::clone(&self.writer),
            session_log_lease: Some(session_log_lease),
        })
    }

    fn mutate_research_session(
        &self,
        request: &PlanReviewRunRequest,
    ) -> Result<CurrentSchemaPlanReviewExistingResearchSessionV1> {
        validate_managed_plan_review_request_binding(
            request,
            PlanReviewChildResourceKindV1::Research,
            0,
        )?;
        let key =
            plan_review_child_resource_key(request, PlanReviewChildResourceKindV1::Research, 0);
        let session_log_lease = self
            .writer
            .acquire_existing_session_log_for_mutation(&key)
            .map_err(|error| anyhow!("plan-review child mutation admission failed: {error}"))?;
        Ok(CurrentSchemaPlanReviewExistingResearchSessionV1 {
            writer: Arc::clone(&self.writer),
            session_log_lease: Some(session_log_lease),
        })
    }
}

fn plan_review_child_resource_key(
    request: &PlanReviewRunRequest,
    kind: PlanReviewChildResourceKindV1,
    ordinal: u32,
) -> String {
    format!(
        "pr-{}-{}-{}",
        request.attempt_id.as_str(),
        kind.tag(),
        ordinal
    )
}

fn validate_authority_generation(
    generation: sigil_kernel::resource::AuthorityGeneration,
) -> Result<()> {
    if generation.epoch == 0
        || !generation
            .instance_hash
            .as_bytes()
            .iter()
            .any(|byte| *byte != 0)
    {
        bail!("managed plan-review child bundle has an incomplete authority generation");
    }
    Ok(())
}

fn validate_managed_plan_review_request_binding(
    request: &PlanReviewRunRequest,
    kind: PlanReviewChildResourceKindV1,
    ordinal: u32,
) -> Result<()> {
    let expected_child_session_ref =
        plan_review_child_session_ref(&request.plan_review_id, &request.attempt_id);
    if request.child_session_ref != expected_child_session_ref {
        bail!("managed plan-review request has a non-canonical child session reference");
    }
    let expected_finalizer_session_ref =
        plan_review_finalizer_session_ref(&request.plan_review_id, &request.attempt_id, 1);
    if request.finalizer_session_ref != expected_finalizer_session_ref {
        bail!("managed plan-review request has a non-canonical finalizer session reference");
    }
    match kind {
        PlanReviewChildResourceKindV1::Research if ordinal == 0 => Ok(()),
        PlanReviewChildResourceKindV1::Finalizer if ordinal > 0 => Ok(()),
        PlanReviewChildResourceKindV1::Research => {
            bail!("managed plan-review research bundle must use ordinal zero")
        }
        PlanReviewChildResourceKindV1::Finalizer => {
            bail!("managed plan-review finalizer bundle must use a positive ordinal")
        }
    }
}

impl PlanReviewRunRequest {
    /// Builds the durable source reference bound into the plan artifact.
    #[must_use]
    pub fn plan_source_ref(&self) -> PlanSourceRef {
        PlanSourceRef {
            source_turn: Some(self.source_turn.clone()),
            route_decision_id: self.route_decision_id.clone(),
            plan_review_id: Some(self.plan_review_id.clone()),
            ..PlanSourceRef::default()
        }
    }

    /// Derives the retry-stable logical run id for the plan review child run.
    #[must_use]
    pub fn child_logical_run_id(&self) -> String {
        format!(
            "plan-review-{}-{}",
            self.plan_review_id.as_str(),
            self.attempt_id.as_str()
        )
    }
}

/// Shared application service for the read-only PlanReview lifecycle.
///
/// Explicit `/plan`, automatic `PlanReview` route decisions, and revisions all enter through this
/// coordinator. It owns the durable attempt lifecycle, the retry-stable child session, the typed
/// `submit_plan_draft` draft commit, and the RFC-0018 Plan-to-Task decision commands.
#[derive(Debug, Clone, Default)]
pub struct PlanReviewCoordinator;

/// Typed plan decision command shared by TUI, HTTP, and Desktop surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanDecisionCommand {
    pub plan_id: String,
    pub expected_plan_hash: String,
    pub decision: PlanDecision,
}

/// Typed create-task-from-plan command shared by TUI, HTTP, and Desktop surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg(test)]
pub struct CreateTaskFromPlanRequest {
    pub plan_id: String,
    pub expected_plan_hash: String,
    pub start_mode: PlanTaskStartMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_grant: Option<PlanApprovalPermission>,
}

/// Result of creating a durable task from an accepted plan.
#[derive(Debug, Clone)]
#[cfg(test)]
pub struct CreatedTaskFromPlan {
    pub task_id: TaskId,
    pub task_id_value: String,
    pub objective: String,
    pub entry: TaskCreatedFromPlanEntry,
    pub start_mode: PlanTaskStartMode,
    pub entries: Vec<SessionLogEntry>,
}

/// Result of recording a plan rejection.
#[derive(Debug, Clone)]
pub struct RejectedPlan {
    pub entry: PlanDecisionRecordedEntry,
    pub entries: Vec<SessionLogEntry>,
}

impl PlanReviewCoordinator {
    /// Maps one revision-domain final outcome to the exact public terminal payload.
    ///
    /// `AwaitingUserInput` deliberately has no value here: the same review attempt may resume
    /// from `WaitingForInput`, so it is not a revision finalizer and remains on the nonterminal
    /// delivery boundary.
    #[must_use]
    pub fn revision_terminal_public_event(
        outcome: &PlanReviewRunOutcome,
    ) -> Option<PublicRunEventKind> {
        match outcome {
            PlanReviewRunOutcome::DraftReady { draft } => Some(PublicRunEventKind::RunFinished {
                final_text: format!("Plan ready: {}", draft.summary),
            }),
            PlanReviewRunOutcome::CompletedWithoutDraft => Some(PublicRunEventKind::RunFinished {
                final_text: "Plan review closed without a draft; no task was created.".to_owned(),
            }),
            PlanReviewRunOutcome::AwaitingUserInput { .. } => None,
            PlanReviewRunOutcome::Cancelled => Some(PublicRunEventKind::RunCancelled),
            PlanReviewRunOutcome::Interrupted(reason) => Some(PublicRunEventKind::RunInterrupted {
                reason: reason.clone(),
            }),
            PlanReviewRunOutcome::Blocked(reason) => Some(PublicRunEventKind::RunBlocked {
                reason: reason.clone(),
            }),
            PlanReviewRunOutcome::Paused(reason) => Some(PublicRunEventKind::RunPaused {
                reason: reason.clone(),
            }),
            PlanReviewRunOutcome::Failed(error)
            | PlanReviewRunOutcome::SubmitOnlyProtocolViolation(error) => {
                Some(PublicRunEventKind::RunFailed {
                    error: error.clone(),
                })
            }
        }
    }

    /// Reconstructs the only displayable revision outcome from a strict, durable terminal pair.
    ///
    /// A current provider error or an in-memory outcome cannot supersede an already committed
    /// revision terminal.  Callers must therefore use this after commit/recovery before updating
    /// an adapter or product surface.
    ///
    /// # Errors
    ///
    /// Returns an error when the outbox does not belong to this exact revision lineage or when
    /// the public terminal payload and durable attempt disagree.
    pub fn revision_outcome_from_terminal(
        session: &Session,
        request: &PlanReviewRunRequest,
        outbox: &PublicEventOutboxEntryV1,
    ) -> Result<PlanReviewRunOutcome> {
        if outbox.run_id != request.child_logical_run_id()
            || outbox.event.session_id != session.session_scope_id()
            || outbox.event.run_id != outbox.run_id
        {
            bail!("durable plan-review terminal outbox belongs to another revision run");
        }
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        let attempt = review_projection
            .latest_attempt(&request.plan_review_id)
            .filter(|attempt| attempt.attempt_id == request.attempt_id)
            .context("durable plan-review terminal lost its matching attempt")?;
        if attempt.plan_review_id != request.plan_review_id
            || attempt.plan_id != request.plan_id
            || attempt.source != request.source
            || attempt.source_turn != request.source_turn
            || attempt.route_decision_id != request.route_decision_id
            || attempt.child_session_ref != request.child_session_ref
            || attempt.finalizer_session_ref.as_ref() != Some(&request.finalizer_session_ref)
            || attempt.revision_request_id != request.revision_request_id
            || attempt.attempt_ordinal != request.attempt_ordinal
            || attempt.base_plan_id != request.base_plan_id
            || attempt.base_plan_hash != request.base_plan_hash
            || attempt.workspace_snapshot_id != request.workspace_snapshot_id
        {
            bail!(
                "durable plan-review terminal attempt does not match the requested revision lineage"
            );
        }
        match &outbox.event.event {
            PublicRunEventKind::RunFinished { .. }
                if attempt.status == PlanReviewAttemptStatus::DraftReady =>
            {
                let draft = session
                    .plan_artifact_projection()
                    .plans
                    .get(&request.plan_id)
                    .cloned()
                    .context("durable plan-review success lost its draft")?;
                Ok(PlanReviewRunOutcome::DraftReady {
                    draft: Box::new(draft),
                })
            }
            PublicRunEventKind::RunFinished { .. }
                if attempt.status == PlanReviewAttemptStatus::CompletedWithoutDraft =>
            {
                Ok(PlanReviewRunOutcome::CompletedWithoutDraft)
            }
            PublicRunEventKind::RunCancelled
                if attempt.status == PlanReviewAttemptStatus::Cancelled =>
            {
                Ok(PlanReviewRunOutcome::Cancelled)
            }
            PublicRunEventKind::RunInterrupted { reason }
                if attempt.status == PlanReviewAttemptStatus::Interrupted =>
            {
                Ok(PlanReviewRunOutcome::Interrupted(reason.clone()))
            }
            PublicRunEventKind::RunBlocked { reason }
                if attempt.status == PlanReviewAttemptStatus::Blocked =>
            {
                Ok(PlanReviewRunOutcome::Blocked(reason.clone()))
            }
            PublicRunEventKind::RunPaused { reason }
                if attempt.status == PlanReviewAttemptStatus::Paused =>
            {
                Ok(PlanReviewRunOutcome::Paused(reason.clone()))
            }
            PublicRunEventKind::RunFailed { error }
                if attempt.status == PlanReviewAttemptStatus::Failed =>
            {
                Ok(PlanReviewRunOutcome::Failed(error.clone()))
            }
            _ => bail!("durable plan-review terminal payload does not match its attempt status"),
        }
    }

    /// Reconstructs a resumable revision suspension from its exact durable attempt/outbox pair.
    ///
    /// Unlike a terminal result, this only remains recoverable while the same attempt is still
    /// `WaitingForInput`. A later answer or finalizer must not let an old suspension reopen the
    /// review.
    ///
    /// # Errors
    ///
    /// Returns an error when the outbox, request lineage, attempt, or input binding disagree.
    pub fn revision_waiting_outcome_from_outbox(
        session: &Session,
        request: &PlanReviewRunRequest,
        outbox: &PublicEventOutboxEntryV1,
    ) -> Result<PlanReviewRunOutcome> {
        if outbox.run_id != request.child_logical_run_id()
            || outbox.event.session_id != session.session_scope_id()
            || outbox.event.run_id != outbox.run_id
        {
            bail!("durable plan-review waiting outbox belongs to another revision run");
        }
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        let attempt = review_projection
            .latest_attempt(&request.plan_review_id)
            .filter(|attempt| attempt.attempt_id == request.attempt_id)
            .context("durable plan-review waiting lost its matching attempt")?;
        validate_revision_attempt_request_binding(attempt, request)?;
        if attempt.status != PlanReviewAttemptStatus::WaitingForInput {
            bail!("durable plan-review waiting attempt is no longer resumable");
        }
        let pending = attempt
            .pending_user_input
            .as_deref()
            .context("durable plan-review waiting lost its input request")?;
        if !matches!(
            &outbox.event.event,
            PublicRunEventKind::RunAwaitingUserInput {
                request_id,
                generation,
                request_hash,
            } if request_id == pending.identity.request_id.as_str()
                && *generation == pending.identity.generation
                && request_hash == &pending.request_hash
        ) {
            bail!("durable plan-review waiting payload does not match its input request");
        }
        Ok(PlanReviewRunOutcome::AwaitingUserInput {
            request: Box::new(pending.clone()),
        })
    }

    /// Atomically records a revision `WaitingForInput` attempt and its exact public outbox
    /// notification. The session owns both durable facts; runtime only delivers the returned
    /// event after this call succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error when this is not a revision, the request/input binding conflicts, or the
    /// writer cannot confirm the atomic pair.
    pub fn commit_revision_waiting_with_outbox(
        parent: &mut Session,
        request: &PlanReviewRunRequest,
        pending: &sigil_kernel::PublicUserInputRequestV1,
        event: PublicRunEvent,
        now_ms: u64,
    ) -> Result<PublicEventOutboxEntryV1> {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(parent, request)?;
        if request.revision_request_id.is_none() {
            bail!("non-revision plan review cannot commit a revision waiting bundle");
        }
        if event.session_id != parent.session_scope_id()
            || event.run_id != request.child_logical_run_id()
        {
            bail!("plan-review waiting public event does not match the parent session or attempt");
        }
        if !matches!(
            &event.event,
            PublicRunEventKind::RunAwaitingUserInput {
                request_id,
                generation,
                request_hash,
            } if request_id == pending.identity.request_id.as_str()
                && *generation == pending.identity.generation
                && request_hash == &pending.request_hash
        ) {
            bail!("plan-review waiting public event does not match its input request");
        }
        if let Some(outbox) =
            parent.reconcile_plan_review_revision_waiting(&request.child_logical_run_id())?
        {
            Self::revision_waiting_outcome_from_outbox(parent, request, &outbox)?;
            if serde_json::to_value(&outbox.event)? != serde_json::to_value(&event)? {
                bail!("plan-review waiting retry conflicts with its original public event");
            }
            return Ok(outbox);
        }
        let mut attempt = plan_review_attempt_status_entry(
            parent,
            request,
            PlanReviewAttemptStatus::WaitingForInput,
            None,
            now_ms,
        )?
        .context("plan-review waiting transition was already recorded without its public outbox")?;
        attempt.pending_user_input = Some(Box::new(pending.clone()));
        let expected_event = event.clone();
        match parent.append_plan_review_revision_waiting(attempt, event) {
            Ok(outbox) => Ok(outbox),
            Err(error) => parent
                .reconcile_plan_review_revision_waiting(&request.child_logical_run_id())?
                .ok_or(error)
                .and_then(|outbox| {
                    Self::revision_waiting_outcome_from_outbox(parent, request, &outbox)?;
                    if serde_json::to_value(&outbox.event)?
                        != serde_json::to_value(&expected_event)?
                    {
                        bail!(
                            "plan-review waiting recovery conflicts with its original public event"
                        );
                    }
                    Ok(outbox)
                }),
        }
    }

    /// Atomically records a terminal revision outcome and its exact public outbox entry.
    ///
    /// The parent `Session` owns the only durable PlanReview attempt/draft/decision facts. This
    /// helper only applies to revision attempts; normal automatic and explicit plan reviews keep
    /// their existing lifecycle path. The kernel commits the supplied final facts and outbox as
    /// one crash-safe bundle, and rejects a historical half-pair rather than deriving a new
    /// terminal from a stale in-memory projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is not a revision, the outcome is a resumable input
    /// suspension, the durable attempt lineage conflicts, or the kernel rejects the final bundle.
    pub fn commit_revision_terminal_with_outbox(
        parent: &mut Session,
        request: &PlanReviewRunRequest,
        outcome: &PlanReviewRunOutcome,
        event: PublicRunEvent,
        now_ms: u64,
    ) -> Result<PublicEventOutboxEntryV1> {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(parent, request)?;
        if request.revision_request_id.is_none() {
            bail!("non-revision plan review cannot commit a revision terminal bundle");
        }
        if let Some(outbox) =
            parent.reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
        {
            return Ok(outbox);
        }
        let (base_plan_id, base_plan_hash) = request
            .base_plan_id
            .as_ref()
            .zip(request.base_plan_hash.as_ref())
            .context("plan review terminal bundle requires a revision base-plan binding")?;
        let expected_run_id = request.child_logical_run_id();
        if event.session_id != parent.session_scope_id() || event.run_id != expected_run_id {
            bail!("plan review terminal public event does not match the parent session or attempt");
        }
        let Some(expected_event) = Self::revision_terminal_public_event(outcome) else {
            bail!("waiting plan-review input must not finalize the revision lifecycle");
        };
        if serde_json::to_value(&event.event)? != serde_json::to_value(&expected_event)? {
            bail!("plan review terminal public event does not match its durable outcome");
        }

        let (attempt, draft, decision) = match outcome {
            PlanReviewRunOutcome::DraftReady { draft } => {
                validate_plan_review_child_draft(draft, request)?;
                let draft_missing = match parent
                    .plan_artifact_projection()
                    .plans
                    .get(&request.plan_id)
                {
                    Some(existing) if existing == draft.as_ref() => false,
                    Some(_) => {
                        bail!(
                            "plan {} already has conflicting durable facts",
                            request.plan_id.as_str()
                        );
                    }
                    None => true,
                };
                let attempt = revision_terminal_attempt_entry(
                    parent,
                    request,
                    PlanReviewAttemptStatus::DraftReady,
                    None,
                    now_ms,
                )?;
                let decision = PlanDecisionRecordedEntry {
                    plan_id: base_plan_id.clone(),
                    plan_hash: base_plan_hash.clone(),
                    decision: PlanDecision::RevisionSucceeded,
                    decided_by: PlanDecisionActor::System,
                    decided_at_ms: now_ms,
                    reason: Some(format!(
                        "superseded by revised plan {}",
                        request.plan_id.as_str()
                    )),
                };
                (attempt, draft_missing.then(|| (**draft).clone()), decision)
            }
            PlanReviewRunOutcome::CompletedWithoutDraft => revision_failure_terminal_entries(
                parent,
                request,
                base_plan_id,
                base_plan_hash,
                PlanReviewAttemptStatus::CompletedWithoutDraft,
                Some(PlanReviewTerminalReason::NoDraftAfterRetry),
                "revision completed without draft",
                now_ms,
            )?,
            PlanReviewRunOutcome::Cancelled => revision_failure_terminal_entries(
                parent,
                request,
                base_plan_id,
                base_plan_hash,
                PlanReviewAttemptStatus::Cancelled,
                Some(PlanReviewTerminalReason::UserCancelled),
                "revision cancelled",
                now_ms,
            )?,
            PlanReviewRunOutcome::Interrupted(_) => revision_failure_terminal_entries(
                parent,
                request,
                base_plan_id,
                base_plan_hash,
                PlanReviewAttemptStatus::Interrupted,
                Some(PlanReviewTerminalReason::RunInterrupted),
                "revision interrupted",
                now_ms,
            )?,
            PlanReviewRunOutcome::Blocked(_) => revision_failure_terminal_entries(
                parent,
                request,
                base_plan_id,
                base_plan_hash,
                PlanReviewAttemptStatus::Blocked,
                Some(PlanReviewTerminalReason::RunBlocked),
                "revision blocked",
                now_ms,
            )?,
            PlanReviewRunOutcome::Paused(_) => revision_failure_terminal_entries(
                parent,
                request,
                base_plan_id,
                base_plan_hash,
                PlanReviewAttemptStatus::Paused,
                Some(PlanReviewTerminalReason::RunPaused),
                "revision paused",
                now_ms,
            )?,
            PlanReviewRunOutcome::Failed(_) => revision_failure_terminal_entries(
                parent,
                request,
                base_plan_id,
                base_plan_hash,
                PlanReviewAttemptStatus::Failed,
                Some(PlanReviewTerminalReason::RunFailed),
                "revision failed",
                now_ms,
            )?,
            PlanReviewRunOutcome::SubmitOnlyProtocolViolation(_) => {
                revision_failure_terminal_entries(
                    parent,
                    request,
                    base_plan_id,
                    base_plan_hash,
                    PlanReviewAttemptStatus::Failed,
                    Some(PlanReviewTerminalReason::SubmitOnlyProtocolViolation),
                    "revision submit-only protocol violation",
                    now_ms,
                )?
            }
            PlanReviewRunOutcome::AwaitingUserInput { .. } => {
                unreachable!("waiting input was rejected before terminal facts were constructed")
            }
        };
        match parent.append_plan_review_revision_terminal(attempt, draft, decision, event) {
            Ok(outbox) => Ok(outbox),
            Err(error) => parent
                .reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
                .ok_or(error),
        }
    }

    /// Prepares the plan review run for an accepted automatic `PlanReview` route decision.
    ///
    /// Validates the durable route decision and returns the host-bound run request. The run
    /// executor owns the `Started` append so an application bridge can commit it with its public
    /// outbox in one parent-session bundle. The model never supplies identity, timestamps, or
    /// authority.
    ///
    /// # Errors
    ///
    /// Returns an error when the decision is missing or conflicts, when the review is already
    /// terminal, or when the source turn is missing from the durable session.
    pub fn prepare_automatic_plan_review(
        session: &mut Session,
        action: &StartPlanReviewAction,
        workspace_snapshot_id: Option<String>,
        _now_ms: u64,
    ) -> Result<PlanReviewRunRequest> {
        let decision_projection =
            ConversationRouteDecisionProjection::from_entries(session.entries());
        if decision_projection.has_conflicts() {
            bail!("conversation route decision projection contains conflicts");
        }
        let decision = decision_projection
            .decision(&action.decision_id)
            .ok_or_else(|| {
                anyhow!(
                    "plan review decision {} is not durable in this session",
                    action.decision_id.as_str()
                )
            })?;
        if decision.route != ConversationRoute::PlanReview {
            bail!(
                "route decision {} is not a plan review decision",
                action.decision_id.as_str()
            );
        }
        if decision.source_turn != action.source_turn {
            bail!("plan review action source turn conflicts with its route decision");
        }
        let objective = session
            .source_user_message(&action.source_turn.message_id)
            .map(|message| message.content.clone().unwrap_or_default())
            .ok_or_else(|| {
                anyhow!(
                    "plan review source user turn {} is not present",
                    action.source_turn.message_id
                )
            })?;
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        if review_projection.is_terminal(&action.plan_review_id) {
            bail!(
                "plan review {} is already terminal",
                action.plan_review_id.as_str()
            );
        }
        let attempt_id = plan_review_attempt_id_for_review(&action.plan_review_id);
        let child_session_ref = plan_review_child_session_ref(&action.plan_review_id, &attempt_id);
        let finalizer_session_ref =
            plan_review_finalizer_session_ref(&action.plan_review_id, &attempt_id, 1);
        let request = PlanReviewRunRequest {
            plan_review_id: action.plan_review_id.clone(),
            attempt_id,
            plan_id: action.plan_id.clone(),
            source: PlanReviewSource::AutomaticConversationRoute,
            source_turn: action.source_turn.clone(),
            route_decision_id: Some(action.decision_id.clone()),
            workspace_snapshot_id,
            child_session_ref,
            finalizer_session_ref,
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            explicit_objective: None,
            objective,
        };
        Ok(request)
    }

    /// Prepares the plan review run for an explicit `/plan` command.
    ///
    /// Explicit plan commands have no persisted provider-visible user turn, so the source turn is
    /// a host-derived identity bound to the session and the root logical run.
    ///
    /// # Errors
    ///
    /// Returns an error when the review is already terminal or the objective is empty.
    pub fn prepare_explicit_plan_review(
        session: &mut Session,
        prompt: &str,
        root_logical_run_id: &str,
        workspace_snapshot_id: Option<String>,
        _now_ms: u64,
    ) -> Result<PlanReviewRunRequest> {
        let explicit_objective = safe_persistence_text(prompt);
        if explicit_objective.trim().is_empty() {
            bail!("explicit plan review objective is empty");
        }
        let plan_review_id =
            plan_review_id_for_explicit_command(session.session_scope_id(), root_logical_run_id);
        let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
        let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
        let source_turn = ConversationTurnRef::new(
            session.session_scope_id(),
            format!("plan-review:{}", plan_review_id.as_str()),
            root_logical_run_id,
        )?;
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        if review_projection.is_terminal(&plan_review_id) {
            bail!(
                "plan review {} is already terminal",
                plan_review_id.as_str()
            );
        }
        let request = PlanReviewRunRequest {
            plan_review_id: plan_review_id.clone(),
            attempt_id: attempt_id.clone(),
            plan_id: plan_id.clone(),
            source: PlanReviewSource::ExplicitPlanCommand,
            source_turn,
            route_decision_id: None,
            child_session_ref: plan_review_child_session_ref(&plan_review_id, &attempt_id),
            finalizer_session_ref: plan_review_finalizer_session_ref(
                &plan_review_id,
                &attempt_id,
                1,
            ),
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            explicit_objective: Some(explicit_objective.clone()),
            objective: explicit_objective,
            workspace_snapshot_id,
        };
        Ok(request)
    }

    /// Runs the read-only plan review child session.
    ///
    /// The child uses the read-only tool registry and keeps provider/tool controls isolated. A
    /// complete untyped response is copied as bounded candidate evidence to the parent, while a
    /// typed draft is committed later by the normal parent coordinator. Research uses the
    /// caller's ordinary `AgentRunOptions` budget without a Plan-specific turn clamp. If research
    /// finishes without a typed draft or loses a provider stream at a typed recoverable boundary,
    /// the host starts one submit-only finalization turn. An unclassified finalization pauses with
    /// its candidate available for explicit adoption.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(clippy::too_many_arguments)]
    pub async fn run_plan_review<H, A>(
        parent_session: &mut Session,
        request: &PlanReviewRunRequest,
        agent: &Agent<impl sigil_kernel::Provider>,
        options: AgentRunOptions,
        tool_registry: sigil_kernel::ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
        cancellation: sigil_kernel::RunCancellationHandle,
    ) -> Result<PlanReviewRunOutcome>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        Self::run_plan_review_inner(
            parent_session,
            request,
            agent,
            options,
            tool_registry,
            handler,
            approval_handler,
            cancellation,
            None,
        )
        .await
    }

    /// Current-schema production entry point. Child session log, artifact store and tool
    /// authority are mandatory; absence is rejected before the provider/tool loop starts.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_plan_review_with_resource_provisioner<H, A>(
        parent_session: &mut Session,
        request: &PlanReviewRunRequest,
        agent: &Agent<impl sigil_kernel::Provider>,
        options: AgentRunOptions,
        tool_registry: sigil_kernel::ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
        cancellation: sigil_kernel::RunCancellationHandle,
        provisioner: Arc<dyn PlanReviewChildResourceProvisionerV1>,
    ) -> Result<PlanReviewRunOutcome>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        Self::run_plan_review_inner(
            parent_session,
            request,
            agent,
            options,
            tool_registry,
            handler,
            approval_handler,
            cancellation,
            Some(provisioner),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_plan_review_inner<H, A>(
        parent_session: &mut Session,
        request: &PlanReviewRunRequest,
        agent: &Agent<impl sigil_kernel::Provider>,
        options: AgentRunOptions,
        tool_registry: sigil_kernel::ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
        cancellation: sigil_kernel::RunCancellationHandle,
        child_resource_provisioner: Option<Arc<dyn PlanReviewChildResourceProvisionerV1>>,
    ) -> Result<PlanReviewRunOutcome>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        if child_resource_provisioner.is_some() {
            validate_managed_plan_review_request_binding(
                request,
                PlanReviewChildResourceKindV1::Research,
                0,
            )?;
            if request.source_turn.session_scope_id != parent_session.session_scope_id() {
                return Err(anyhow::Error::new(
                    sigil_kernel::SessionContextPrefixError::ConflictingBoundary,
                )
                .context(
                    "managed plan-review request source turn belongs to another parent session",
                ));
            }
        }

        // Admit and validate every child capability before recording the parent Started state.
        // A failed managed admission must not leave a parent attempt claiming that execution
        // began, and must never reach provider/tool dispatch.
        let child_bundle = child_resource_provisioner
            .as_ref()
            .map(|provisioner| {
                provisioner.provision(request, PlanReviewChildResourceKindV1::Research, 0)
            })
            .transpose()?;
        let child_bundle = match child_bundle {
            Some(bundle) => {
                if let Err(error) =
                    bundle.validate_for(request, PlanReviewChildResourceKindV1::Research, 0)
                {
                    return combine_child_resource_settlement(Err(error), bundle.finish());
                }
                Some(bundle)
            }
            None => None,
        };

        // A revision's `Started` record remains owned by its revision execution protocol. An
        // ordinary parent attempt instead crosses the supplied handler commit boundary so an
        // application bridge can atomically append its source and public outbox entry.
        let started = if request.revision_request_id.is_some() {
            Self::ensure_revision_attempt_started(parent_session, request, now_ms())
        } else {
            Self::ensure_attempt_started(parent_session, request, handler, now_ms())
        };
        if let Err(error) = started {
            return combine_child_resource_settlement(
                Err(error),
                child_bundle.map(|bundle| bundle.finish()).unwrap_or(Ok(())),
            );
        }
        // The host owns plan acceptance authority: the plan review run is always read-only,
        // regardless of the enclosing run's permission mode.
        let mut options = options;
        options.permission_config.mode = sigil_kernel::PermissionMode::ReadOnly;
        let outcome = async {
        // Started is committed before this first capture; Waiting continuations reconstruct the
        // same first boundary. Prefix failures still settle the admitted child resource bundle.
        let parent_context = plan_review_parent_context(parent_session, request)?;
        let mut child_session =
            build_plan_review_child_session(parent_session, request, child_bundle.as_ref())?;
        let draft_context = sigil_kernel::PlanReviewDraftContext {
            plan_review_id: request.plan_review_id.clone(),
            attempt_id: request.attempt_id.clone(),
            plan_id: request.plan_id.clone(),
            source: request.plan_source_ref(),
            workspace_snapshot_id: request.workspace_snapshot_id.clone(),
            candidate_content: None,
        };
        if cancellation.is_cancel_requested() {
            return Ok(PlanReviewRunOutcome::Cancelled);
        }

        let existing_research_input = {
            let projection = child_session.user_input_projection()?;
            projection
                .public_requests()
                .into_iter()
                .filter(|candidate| {
                    matches!(
                        &candidate.source,
                        sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                            plan_review_id,
                            attempt_id,
                        } if plan_review_id == &request.plan_review_id
                            && attempt_id == &request.attempt_id
                    )
                })
                .max_by_key(|candidate| candidate.requested_at_unix_ms)
                .and_then(|candidate| projection.request(&candidate.identity).cloned())
        };
        let mut research_candidate_text =
            plan_review_candidate_content(&child_session, request)?;
        if let Some(state) = existing_research_input.as_ref()
            && state.status == sigil_kernel::UserInputStatusV1::Requested
        {
            return complete_plan_review_run(
                &cancellation,
                PlanReviewRunOutcome::AwaitingUserInput {
                    request: Box::new(state.public_view()),
                },
            );
        }
        if existing_research_input.as_ref().is_some_and(|state| {
            matches!(
                state.resolution.as_ref().map(|entry| &entry.resolution),
                Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
            )
        }) {
            return complete_plan_review_run(&cancellation, PlanReviewRunOutcome::Cancelled);
        }

        let mut research_options = options.clone();
        if let Some(bundle) = child_bundle.as_ref() {
            research_options = research_options.with_tool_authority(bundle.tool_authority());
        }
        let mut continuation_resolution = None;
        let mut research_logical_run_id = request.child_logical_run_id();
        let skip_research_cause = existing_research_input.as_ref().and_then(|state| {
            match state.resolution.as_ref().map(|entry| &entry.resolution) {
                Some(sigil_kernel::UserInputResolutionV1::Declined) => {
                    Some("the user declined a plan-research clarification".to_owned())
                }
                Some(sigil_kernel::UserInputResolutionV1::Consumed) => Some(
                    "a recovered plan-research continuation completed without a committed draft"
                        .to_owned(),
                ),
                Some(sigil_kernel::UserInputResolutionV1::Failed { failure_class, .. }) => Some(
                    format!("the prior plan-research continuation failed: {failure_class}"),
                ),
                _ => None,
            }
        });
        if skip_research_cause.is_some()
            && let Some(draft) = child_session
                .plan_artifact_projection()
                .plans
                .get(&request.plan_id)
                .cloned()
        {
            return complete_plan_review_run(
                &cancellation,
                PlanReviewRunOutcome::DraftReady {
                    draft: Box::new(draft),
                },
            );
        }
        let research = if skip_research_cause.is_some() {
            None
        } else {
            let research_input = if let Some(state) = existing_research_input.as_ref()
                && matches!(
                    state.status,
                    sigil_kernel::UserInputStatusV1::DecisionAccepted
                        | sigil_kernel::UserInputStatusV1::ContinuationClaimed
                        | sigil_kernel::UserInputStatusV1::ContinuationStarted
                ) {
                let physical_attempt_id = sigil_kernel::new_provider_physical_attempt_id();
                let prepared = sigil_kernel::prepare_user_input_continuation(
                    &mut child_session,
                    &state.requested.request.identity,
                    &state.requested.request_hash,
                    "plan-review-supervisor-v1",
                    &physical_attempt_id,
                    now_ms(),
                )?;
                research_logical_run_id = prepared
                    .continuation
                    .continuation_logical_run_id
                    .as_str()
                    .to_owned();
                continuation_resolution = Some((
                    state.requested.request.identity.clone(),
                    state.requested.request_hash.clone(),
                ));
                plan_review_continuation_input(
                    request,
                    &draft_context,
                    &cancellation,
                    &prepared.continuation,
                    &parent_context,
                )
            } else {
                plan_review_run_input(
                    request,
                    &draft_context,
                    &cancellation,
                    &parent_context,
                    None,
                    None,
                    None,
                    0,
                )
            };
            let mut child_handler = PlanReviewChildEventHandler { inner: handler };
            Some(
                agent
                    .run_with_approval_input_and_tool_registry(
                        &mut child_session,
                        research_input,
                        research_options,
                        tool_registry,
                        &mut child_handler,
                        approval_handler,
                    )
                    .await,
            )
        };
        if let Some((identity, request_hash)) = continuation_resolution.as_ref() {
            if research.as_ref().is_some_and(Result::is_ok) {
                child_session.append_user_input_lifecycle(vec![
                    sigil_kernel::UserInputLifecycleEntryV1::Resolved(
                        sigil_kernel::UserInputResolvedV1 {
                            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                            identity: identity.clone(),
                            request_hash: request_hash.clone(),
                            resolution: sigil_kernel::UserInputResolutionV1::Consumed,
                            resolved_at_unix_ms: now_ms(),
                        },
                    ),
                ])?;
            } else {
                sigil_kernel::reconcile_user_input_continuation_after_failed_run(
                    &mut child_session,
                    identity,
                    request_hash,
                    now_ms(),
                )?;
            }
        }
        let recovery_cause = match research {
            None => skip_research_cause,
            Some(research) => {
                match research {
                    Ok(_) if cancellation.is_cancel_requested() => {
                        return Ok(PlanReviewRunOutcome::Cancelled);
                    }
                    Ok(output) => {
                        if output.disposition == AgentRunDisposition::FinalAnswer
                            && !output.result.final_text.trim().is_empty()
                        {
                            let source_event_id = output.result.final_message_id.clone();
                            research_candidate_text = match record_plan_review_candidate(
                                &mut child_session,
                                request,
                                &output.result.final_text,
                                source_event_id.clone(),
                                sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown,
                                now_ms(),
                            ) {
                                Ok(candidate) => candidate,
                                Err(error) => {
                                    handler.handle(RunEvent::Notice(format!(
                                        "Plan review candidate could not be durably preserved: {error:#}"
                                    )))?;
                                    return complete_plan_review_run(
                                        &cancellation,
                                        PlanReviewRunOutcome::Paused(
                                            "Plan review candidate requires durable managed storage before confirmation"
                                                .to_owned(),
                                        ),
                                    );
                                }
                            };
                            if let Some(content) = research_candidate_text.as_deref()
                                && let Err(error) = record_plan_review_candidate_in_parent(
                                    parent_session,
                                    request,
                                    content,
                                    source_event_id,
                                    sigil_kernel::PlanReviewCandidateCompletenessV1::Unknown,
                                    handler,
                                    now_ms(),
                                )
                            {
                                handler.handle(RunEvent::Notice(format!(
                                    "Plan review candidate could not be mirrored to the parent session: {error:#}"
                                )))?;
                                return complete_plan_review_run(
                                    &cancellation,
                                    PlanReviewRunOutcome::Paused(
                                        "Plan review candidate requires durable parent storage before confirmation"
                                            .to_owned(),
                                    ),
                                );
                            }
                        }
                        if let Some(reason) = plan_review_no_plan_reason(&child_session) {
                            record_plan_review_candidate_in_parent(
                                parent_session,
                                request,
                                &reason,
                                None,
                                sigil_kernel::PlanReviewCandidateCompletenessV1::Complete,
                                handler,
                                now_ms(),
                            )?;
                            handler.handle(RunEvent::Notice(format!(
                                "Plan review completed without a draft: {reason}"
                            )))?;
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::CompletedWithoutDraft,
                            );
                        }
                        match output.disposition {
                        AgentRunDisposition::PlanReviewDraftSubmitted(action) => {
                            let outcome =
                                plan_review_draft_ready_outcome(&child_session, &action.plan_id)?;
                            return complete_plan_review_run(&cancellation, outcome);
                        }
                        AgentRunDisposition::FinalAnswer => None,
                        AgentRunDisposition::AwaitingUserInput(request_ref) => {
                            let request = child_session
                        .user_input_projection()?
                        .request(&request_ref.identity)
                        .filter(|state| {
                            state.requested.request_hash == request_ref.request_hash
                                && matches!(
                                    state.requested.request.source,
                                    sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                                        ref plan_review_id,
                                        ref attempt_id,
                                    } if plan_review_id == &request.plan_review_id
                                        && attempt_id == &request.attempt_id
                                )
                        })
                        .map(sigil_kernel::UserInputRequestStateV1::public_view)
                        .context("plan review research suspension lost its exact durable request")?;
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::AwaitingUserInput {
                                    request: Box::new(request),
                                },
                            );
                        }
                        AgentRunDisposition::Interrupted => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Interrupted(
                                    "plan review run was interrupted before a draft".to_owned(),
                                ),
                            );
                        }
                        AgentRunDisposition::Blocked => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Blocked(
                                    "plan review run was blocked before a draft".to_owned(),
                                ),
                            );
                        }
                        AgentRunDisposition::StartDurableTask(_)
                        | AgentRunDisposition::ContinueDurableTask(_)
                        | AgentRunDisposition::RunPendingPlan(_)
                        | AgentRunDisposition::PendingPlanDecisionRequired(_)
                        | AgentRunDisposition::TaskPlanAccepted => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Failed(
                                    "plan review run attempted an out-of-scope handoff".to_owned(),
                                ),
                            );
                        }
                        AgentRunDisposition::StartPlanReview(_) => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Failed(
                                    "plan review run requested a nested plan review".to_owned(),
                                ),
                            );
                        }
                        }
                    }
                    Err(error) => {
                        if cancellation.is_cancel_requested() {
                            return Ok(PlanReviewRunOutcome::Cancelled);
                        }
                        let recovery_allowed =
                            match plan_review_provider_terminal_allows_submit_only_recovery(
                                &child_session,
                                &research_logical_run_id,
                            ) {
                                Ok(recovery_allowed) => recovery_allowed,
                                Err(projection_error) => {
                                    return Err(error.context(format!(
                        "plan review provider failure could not be classified from durable physical-attempt evidence: {projection_error:#}"
                    )));
                                }
                            };
                        if !recovery_allowed {
                            return Err(error);
                        }
                        handler.handle(RunEvent::Notice(
                    "Plan review provider stream ended before a durable result; continuing with one submit-only finalization turn from the recorded read-only evidence."
                        .to_owned(),
                ))?;
                        Some(format!("{error:#}"))
                    }
                }
            }
        };

        if cancellation.is_cancel_requested() {
            return Ok(PlanReviewRunOutcome::Cancelled);
        }
        if request.revision_request_id.is_some() {
            append_revision_attempt_status(
                parent_session,
                request,
                PlanReviewAttemptStatus::Finalizing,
                None,
                now_ms(),
            )?;
        } else {
            append_attempt_status(
                parent_session,
                request,
                handler,
                PlanReviewAttemptStatus::Finalizing,
                None,
                now_ms(),
            )?;
        }
        let evidence = plan_review_finalizer_evidence_bundle(request, &child_session);
        let research_model_turns = child_session
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::Assistant(_)))
            .count();
        if options
            .max_turns
            .is_some_and(|max_turns| research_model_turns >= max_turns)
        {
            return complete_plan_review_run(
                &cancellation,
                PlanReviewRunOutcome::Paused(
                    "plan review reached the configured model-turn budget before result confirmation"
                        .to_owned(),
                ),
            );
        }
        let mut last_violation = None;
        let mut last_violation_digest = None;
        for corrective_ordinal in 1..=2 {
            if cancellation.is_cancel_requested() {
                return Ok(PlanReviewRunOutcome::Cancelled);
            }
            let finalizer_bundle = child_resource_provisioner
                .as_ref()
                .map(|provisioner| {
                    provisioner.provision(
                        request,
                        PlanReviewChildResourceKindV1::Finalizer,
                        corrective_ordinal,
                    )
                })
                .transpose()?;
            let finalization = async {
            let mut finalizer_session = build_plan_review_finalizer_session(
                parent_session,
                request,
                corrective_ordinal,
                finalizer_bundle.as_ref(),
            )?;
            if let Some(draft) = finalizer_session
                .plan_artifact_projection()
                .plans
                .get(&request.plan_id)
                .cloned()
            {
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::DraftReady {
                        draft: Box::new(draft),
                    },
                ).map(Some);
            }
            let mut finalization_options = options.clone();
            if let Some(bundle) = finalizer_bundle.as_ref() {
                finalization_options =
                    finalization_options.with_tool_authority(bundle.tool_authority());
            }
            finalization_options.max_turns = Some(
                options
                    .max_turns
                    .map(|max_turns| {
                        max_turns
                            .saturating_sub(research_model_turns)
                            .min(PLAN_REVIEW_FINALIZATION_MAX_MODEL_TURNS)
                    })
                    .unwrap_or(PLAN_REVIEW_FINALIZATION_MAX_MODEL_TURNS),
            );
            let finalization_input = plan_review_run_input(
                request,
                &draft_context,
                &cancellation,
                &parent_context,
                Some(&evidence),
                research_candidate_text.as_deref(),
                last_violation.as_deref(),
                corrective_ordinal,
            );
            let mut child_handler = PlanReviewChildEventHandler { inner: handler };
            let finalization = agent
                .run_with_approval_input_and_tool_registry(
                    &mut finalizer_session,
                    finalization_input,
                    finalization_options,
                    sigil_kernel::ToolRegistry::new(),
                    &mut child_handler,
                    approval_handler,
                )
                .await;
            let output = match finalization {
                Ok(_) if cancellation.is_cancel_requested() => {
                    return Ok(Some(PlanReviewRunOutcome::Cancelled));
                }
                Ok(output) => output,
                Err(_) if cancellation.is_cancel_requested() => {
                    return Ok(Some(PlanReviewRunOutcome::Cancelled));
                }
                Err(error) => {
                    let context = recovery_cause.as_deref().map_or_else(
                        || "plan review submit-only finalization failed".to_owned(),
                        |cause| {
                            format!(
                                "plan review submit-only recovery failed after research stream ended early ({cause})"
                            )
                        },
                    );
                    return Err(error.context(context));
                }
            };
            if output.disposition == AgentRunDisposition::FinalAnswer
                && !output.result.final_text.trim().is_empty()
            {
                let source_event_id = output.result.final_message_id.clone();
                if let Err(error) = record_plan_review_candidate(
                    &mut finalizer_session,
                    request,
                    &output.result.final_text,
                    source_event_id.clone(),
                    sigil_kernel::PlanReviewCandidateCompletenessV1::Complete,
                    now_ms(),
                ) {
                    handler.handle(RunEvent::Notice(format!(
                        "Plan review finalizer candidate could not be durably preserved: {error:#}"
                    )))?;
                    return complete_plan_review_run(
                        &cancellation,
                        PlanReviewRunOutcome::Paused(
                            "Plan review candidate requires durable managed storage before confirmation"
                                .to_owned(),
                        ),
                    )
                    .map(Some);
                }
                if let Err(error) = record_plan_review_candidate_in_parent(
                    parent_session,
                    request,
                    &output.result.final_text,
                    source_event_id,
                    sigil_kernel::PlanReviewCandidateCompletenessV1::Complete,
                    handler,
                    now_ms(),
                ) {
                    handler.handle(RunEvent::Notice(format!(
                        "Plan review finalizer candidate could not be mirrored to the parent session: {error:#}"
                    )))?;
                    return complete_plan_review_run(
                        &cancellation,
                        PlanReviewRunOutcome::Paused(
                            "Plan review candidate requires durable parent storage before confirmation"
                                .to_owned(),
                        ),
                    )
                    .map(Some);
                }
            }
            if let Some(reason) = plan_review_no_plan_reason(&finalizer_session) {
                record_plan_review_candidate_in_parent(
                    parent_session,
                    request,
                    &reason,
                    None,
                    sigil_kernel::PlanReviewCandidateCompletenessV1::Complete,
                    handler,
                    now_ms(),
                )?;
                handler.handle(RunEvent::Notice(format!(
                    "Plan review completed without a draft: {reason}"
                )))?;
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::CompletedWithoutDraft,
                )
                .map(Some);
            }
            if finalizer_session.entries().iter().any(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Assistant(message)
                        if message.tool_calls.iter().any(|call| {
                        call.name != sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME
                                && call.name != sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME
                                && call.name != sigil_kernel::CONFIRM_PLAN_REVIEW_CANDIDATE_TOOL_NAME
                        })
                )
            }) {
                let reason = "submit-only finalizer attempted a non-submit tool".to_owned();
                last_violation = Some(reason.clone());
                if corrective_ordinal == 1 {
                    handler.handle(RunEvent::Notice(
                        "Plan finalization attempted an unavailable research tool; retrying once in a fresh submit-only context."
                            .to_owned(),
                    ))?;
                    return Ok(None);
                }
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::SubmitOnlyProtocolViolation(reason),
                ).map(Some);
            }
            if let Some(reason) = invalid_plan_draft_submission_reason(&finalizer_session) {
                let digest = sigil_kernel::stable_event_hash(reason.as_bytes());
                if last_violation_digest.as_deref() == Some(digest.as_str()) {
                    return complete_plan_review_run(
                        &cancellation,
                        PlanReviewRunOutcome::Paused(
                            "plan finalization repeated the same validation error; retry requires an explicit new allowance"
                                .to_owned(),
                        ),
                    )
                    .map(Some);
                }
                last_violation_digest = Some(digest);
                last_violation = Some(reason.clone());
                if corrective_ordinal == 1 {
                    handler.handle(RunEvent::Notice(
                        "Plan finalization produced an invalid typed draft; retrying once in a fresh submit-only context."
                            .to_owned(),
                    ))?;
                    return Ok(None);
                }
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::SubmitOnlyProtocolViolation(reason),
                ).map(Some);
            }
            let outcome = match output.disposition {
                AgentRunDisposition::PlanReviewDraftSubmitted(action) => {
                    plan_review_draft_ready_outcome(&finalizer_session, &action.plan_id)?
                }
                AgentRunDisposition::FinalAnswer => {
                    PlanReviewRunOutcome::Paused(
                        "a complete Plan candidate was preserved and awaits explicit confirmation"
                            .to_owned(),
                    )
                }
                AgentRunDisposition::AwaitingUserInput(_) => PlanReviewRunOutcome::Failed(
                    "plan review finalization exposed an unsupported user-input suspension"
                        .to_owned(),
                ),
                AgentRunDisposition::Interrupted => PlanReviewRunOutcome::Interrupted(
                    "plan review finalization was interrupted before a draft".to_owned(),
                ),
                AgentRunDisposition::Blocked => PlanReviewRunOutcome::Blocked(
                    "plan review finalization was blocked before a draft".to_owned(),
                ),
                AgentRunDisposition::StartDurableTask(_)
                | AgentRunDisposition::ContinueDurableTask(_)
                | AgentRunDisposition::RunPendingPlan(_)
                | AgentRunDisposition::PendingPlanDecisionRequired(_)
                | AgentRunDisposition::TaskPlanAccepted => PlanReviewRunOutcome::Failed(
                    "plan review finalization attempted an out-of-scope handoff".to_owned(),
                ),
                AgentRunDisposition::StartPlanReview(_) => PlanReviewRunOutcome::Failed(
                    "plan review finalization requested a nested plan review".to_owned(),
                ),
            };
            complete_plan_review_run(&cancellation, outcome).map(Some)
            }.await;
            let finalization = combine_child_resource_settlement(
                finalization,
                finalizer_bundle
                    .map(|bundle| bundle.finish())
                    .unwrap_or(Ok(())),
            )?;
            if let Some(outcome) = finalization {
                return Ok(outcome);
            }
        }
        complete_plan_review_run(
            &cancellation,
            PlanReviewRunOutcome::SubmitOnlyProtocolViolation(
                last_violation.unwrap_or_else(|| "submit-only finalizer failed".to_owned()),
            ),
        )
        }.await;
        combine_child_resource_settlement(
            outcome,
            child_bundle.map(|bundle| bundle.finish()).unwrap_or(Ok(())),
        )
    }

    /// Commits a validated draft from the plan review child session into the parent session.
    ///
    /// The readable draft is the complete review authority. No Task candidate is compiled here:
    /// structured fields may improve presentation, but cannot become an execution prerequisite.
    ///
    /// # Errors
    ///
    /// Returns an error when the draft conflicts with durable facts or the attempt transition is
    /// invalid.
    pub fn commit_draft_from_child<H>(
        parent: &mut Session,
        draft: &PlanDraftCreatedEntry,
        request: &PlanReviewRunRequest,
        _compile_input: &PlanCompileInputV1,
        handler: &mut H,
        now_ms: u64,
    ) -> Result<()>
    where
        H: EventHandler + ?Sized,
    {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(parent, request)?;
        if request.revision_request_id.is_some() {
            bail!(
                "revision draft completion requires the atomic revision terminal and public outbox"
            );
        }
        validate_plan_review_child_draft(draft, request)?;
        let controls = Self::plan_review_draft_terminal_controls(parent, draft, request, now_ms)?;
        if !controls.is_empty() {
            handler.commit_controls(parent, controls)?;
        }
        Ok(())
    }

    /// Builds the exact durable controls for a successful ordinary Plan review draft. The
    /// returned vector is pure with respect to persistence: callers can include it in a larger
    /// terminal writer bundle without first committing a partial attempt transition.
    pub fn plan_review_draft_terminal_controls(
        parent: &Session,
        draft: &PlanDraftCreatedEntry,
        request: &PlanReviewRunRequest,
        now_ms: u64,
    ) -> Result<Vec<ControlEntry>> {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(parent, request)?;
        if request.revision_request_id.is_some() {
            bail!(
                "revision draft completion requires the atomic revision terminal and public outbox"
            );
        }
        validate_plan_review_child_draft(draft, request)?;
        let projection = parent.plan_artifact_projection();
        let draft_missing = match projection.plans.get(&request.plan_id) {
            Some(existing) if existing == draft => false,
            Some(_) => {
                bail!(
                    "plan {} already has conflicting durable facts",
                    request.plan_id.as_str()
                );
            }
            None => true,
        };
        let mut controls = Vec::new();
        if draft_missing {
            controls.push(ControlEntry::PlanDraftCreated(draft.clone()));
        }
        if let Some(resolution) =
            plan_review_resolution_for_draft_candidate(parent, request, draft, now_ms)?
        {
            controls.push(ControlEntry::PlanReviewResolutionRecordedV1(resolution));
        }
        if let Some(attempt_entry) = plan_review_attempt_status_entry(
            parent,
            request,
            PlanReviewAttemptStatus::DraftReady,
            None,
            now_ms,
        )? {
            controls.push(ControlEntry::PlanReviewAttempt(attempt_entry));
        }
        Ok(controls)
    }

    /// Builds the pure, deterministic compile input for one plan review attempt (RFC-0067 7.2).
    ///
    /// The contract hashes prove which planner/task/intent/config contract generation produced
    /// the candidate. They are evidence, not runtime permission.
    ///
    /// # Errors
    ///
    /// Returns an error when the stable workspace identity cannot be derived.
    pub fn plan_compile_input(
        session: &Session,
        root_config: &RootConfig,
        workspace_root: &Path,
        request: &PlanReviewRunRequest,
    ) -> Result<PlanCompileInputV1> {
        Ok(PlanCompileInputV1 {
            source_attempt_id: request.attempt_id.as_str().to_owned(),
            source_turn_id: request.source_turn.message_id.clone(),
            task_config_contract_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-task-config-v1",
                &format!("max_plan_steps={}", root_config.task.max_plan_steps),
            ),
            planner_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-planner-schema-v1",
                "submit_plan_draft-v2",
            ),
            task_contract_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-task-contract-schema-v1",
                "task-step-contract-v2",
            ),
            intent_schema_hash: Some(sigil_kernel::stable_event_uuid(
                "sigil-intent-schema-v1",
                "intent-contract-v1",
            )),
            max_plan_steps: root_config.task.max_plan_steps,
            workspace_id: stable_workspace_id(workspace_root).ok(),
            session_scope_id: Some(session.session_scope_id().to_owned()),
        })
    }

    /// Durably closes a plan review run that terminated without a committed draft.
    ///
    /// `Cancelled` and `Failed` outcomes append the exact terminal attempt status instead of
    /// leaving a dangling `Started` attempt that recovery would later guess as `Interrupted`.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt transition is invalid or conflicts with durable facts.
    pub fn close_plan_review_run<H>(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        outcome: &PlanReviewRunOutcome,
        handler: &mut H,
        now_ms: u64,
    ) -> Result<()>
    where
        H: EventHandler + ?Sized,
    {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(session, request)?;
        if request.revision_request_id.is_some() {
            bail!("revision close requires an atomic terminal or waiting attempt/outbox bundle");
        }
        let closed = match outcome {
            PlanReviewRunOutcome::DraftReady { .. }
            | PlanReviewRunOutcome::CompletedWithoutDraft => Ok(()),
            PlanReviewRunOutcome::AwaitingUserInput { request: pending } => {
                append_attempt_status_with_pending_input(
                    session,
                    request,
                    handler,
                    PlanReviewAttemptStatus::WaitingForInput,
                    Some((**pending).clone()),
                    now_ms,
                )
            }
            PlanReviewRunOutcome::Cancelled => append_attempt_status(
                session,
                request,
                handler,
                PlanReviewAttemptStatus::Cancelled,
                Some(PlanReviewTerminalReason::UserCancelled),
                now_ms,
            ),
            PlanReviewRunOutcome::Interrupted(_) => append_attempt_status(
                session,
                request,
                handler,
                PlanReviewAttemptStatus::Interrupted,
                Some(PlanReviewTerminalReason::RunInterrupted),
                now_ms,
            ),
            PlanReviewRunOutcome::Blocked(_) => append_attempt_status(
                session,
                request,
                handler,
                PlanReviewAttemptStatus::Blocked,
                Some(PlanReviewTerminalReason::RunBlocked),
                now_ms,
            ),
            PlanReviewRunOutcome::Paused(_) => append_attempt_status(
                session,
                request,
                handler,
                PlanReviewAttemptStatus::Paused,
                Some(PlanReviewTerminalReason::RunPaused),
                now_ms,
            ),
            PlanReviewRunOutcome::Failed(_) => append_attempt_status(
                session,
                request,
                handler,
                PlanReviewAttemptStatus::Failed,
                Some(PlanReviewTerminalReason::RunFailed),
                now_ms,
            ),
            PlanReviewRunOutcome::SubmitOnlyProtocolViolation(_) => append_attempt_status(
                session,
                request,
                handler,
                PlanReviewAttemptStatus::Failed,
                Some(PlanReviewTerminalReason::SubmitOnlyProtocolViolation),
                now_ms,
            ),
        };
        closed?;
        Ok(())
    }

    /// Closes a plan review run unless the attempt is already terminal or was never started.
    ///
    /// Used by executors on the error path of a run that failed before producing an outcome:
    /// a dangling `Started` attempt would otherwise be misread by recovery as a crash. When the
    /// attempt already carries a terminal status (e.g. a concurrent closer won), this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt is open and its terminal transition fails.
    pub fn close_plan_review_run_if_open<H>(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        outcome: &PlanReviewRunOutcome,
        handler: &mut H,
        now_ms: u64,
    ) -> Result<()>
    where
        H: EventHandler + ?Sized,
    {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(session, request)?;
        if request.revision_request_id.is_some() {
            bail!(
                "revision terminal recovery requires the atomic revision terminal and public outbox"
            );
        }
        let projection = PlanReviewProjection::from_entries(session.entries());
        let Some(existing) = projection.latest_attempt(&request.plan_review_id) else {
            return Ok(());
        };
        if existing.attempt_id == request.attempt_id && existing.status.is_terminal() {
            return Ok(());
        }
        Self::close_plan_review_run(session, request, outcome, handler, now_ms)
    }

    /// Closes an automatic plan review that produced no draft.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt transition is invalid.
    pub fn complete_without_draft<H>(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        handler: &mut H,
        now_ms: u64,
    ) -> Result<()>
    where
        H: EventHandler + ?Sized,
    {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(session, request)?;
        if request.revision_request_id.is_some() {
            bail!("revision completion requires the atomic revision terminal and public outbox");
        }
        let controls = Self::plan_review_no_draft_terminal_controls(session, request, now_ms)?;
        if !controls.is_empty() {
            handler.commit_controls(session, controls)?;
        }
        Ok(())
    }

    /// Builds the exact durable controls for an ordinary Plan review that completed without a
    /// draft, allowing the attempt closure to share the enclosing conversation terminal batch.
    pub fn plan_review_no_draft_terminal_controls(
        session: &Session,
        request: &PlanReviewRunRequest,
        now_ms: u64,
    ) -> Result<Vec<ControlEntry>> {
        validate_plan_review_request_lineage(request)?;
        validate_plan_review_request_objective(session, request)?;
        if request.revision_request_id.is_some() {
            bail!("revision completion requires the atomic revision terminal and public outbox");
        }
        let mut controls = Vec::new();
        if let Some(resolution) =
            plan_review_resolution_for_no_plan_candidate(session, request, now_ms)?
        {
            controls.push(ControlEntry::PlanReviewResolutionRecordedV1(resolution));
        }
        if let Some(attempt) = plan_review_attempt_status_entry(
            session,
            request,
            PlanReviewAttemptStatus::CompletedWithoutDraft,
            Some(PlanReviewTerminalReason::NoDraftAfterRetry),
            now_ms,
        )? {
            controls.push(ControlEntry::PlanReviewAttempt(attempt));
        }
        Ok(controls)
    }

    /// Prepares a revision attempt for an existing plan review lifecycle.
    ///
    /// Creates or replays the host-owned question that must precede every first revision run.
    pub fn request_plan_revision_guidance(
        session: &mut Session,
        plan_id: &PlanId,
        expected_plan_hash: &str,
        now_ms: u64,
    ) -> Result<sigil_kernel::UserInputRequestedV1> {
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                expected_plan_hash,
                draft.plan_hash
            );
        }
        let request_id = sigil_kernel::UserInputRequestId::new(stable_event_uuid(
            "sigil-plan-revision-request-v1",
            &format!("{}|{}", plan_id.as_str(), expected_plan_hash),
        ))?;
        let user_input = session.user_input_projection()?;
        if let Some(existing) = user_input
            .public_requests()
            .into_iter()
            .filter(|request| request.identity.request_id == request_id)
            .max_by_key(|request| request.identity.generation)
            && existing.status == sigil_kernel::UserInputStatusV1::Requested
        {
            return session
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::UserInputRequested(requested))
                        if requested.request.identity == existing.identity =>
                    {
                        Some((**requested).clone())
                    }
                    _ => None,
                })
                .context("pending revision guidance lost its durable request");
        }
        ensure_plan_action_allowed(
            session,
            plan_id,
            expected_plan_hash,
            sigil_kernel::PublicPlanAction::Revise,
        )?;
        if projection.plan_is_rejected(plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        if projection.task_created_for_plan(plan_id) {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(existing) = projection.latest_decision(plan_id)
            && !matches!(
                existing.decision,
                PlanDecision::RevisionFailed | PlanDecision::SavedOnly
            )
        {
            bail!(
                "plan {} already has decision {}",
                plan_id.as_str(),
                existing.decision.as_str()
            );
        }
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let previous = review_projection
            .attempt_for_plan(plan_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "plan {} is not bound to a plan review lifecycle",
                    plan_id.as_str()
                )
            })?;
        let generation = user_input
            .public_requests()
            .into_iter()
            .filter(|request| request.identity.request_id == request_id)
            .map(|request| request.identity.generation)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let root_logical_run_id = sigil_kernel::LogicalRunId::new(stable_event_uuid(
            "sigil-plan-revision-root-run-v1",
            previous.plan_review_id.as_str(),
        ))?;
        let source_thread_id = sigil_kernel::AgentThreadId::new("main")?;
        let source_binding_hash = sigil_kernel::stable_event_hash(format!(
            "{}|{}|{}|{}",
            session.session_scope_id(),
            plan_id.as_str(),
            expected_plan_hash,
            previous.attempt_id.as_str()
        ));
        let requested =
            sigil_kernel::UserInputRequestedV1::new(sigil_kernel::UserInputRequestV1 {
                schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                identity: sigil_kernel::UserInputIdentityV1 {
                    session_scope_id: sigil_kernel::SessionScopeId::new(
                        session.session_scope_id(),
                    )?,
                    root_logical_run_id,
                    source_thread_id,
                    request_id,
                    generation,
                    source_binding_hash,
                },
                source: sigil_kernel::UserInputSourceV1::PlanRevision {
                    base_plan_id: plan_id.clone(),
                    base_plan_hash: expected_plan_hash.to_owned(),
                },
                purpose: sigil_kernel::UserInputPurposeV1::RevisionGuidance,
                prompt: "What should change in this plan?".to_owned(),
                questions: vec![sigil_kernel::UserInputQuestionV1 {
                    id: "revision_guidance".to_owned(),
                    header: "Revision guidance".to_owned(),
                    question: "Describe the changes you want before a new plan is prepared."
                        .to_owned(),
                    description: Some(
                        "The original plan remains available until a revised draft succeeds."
                            .to_owned(),
                    ),
                    required: true,
                    field: sigil_kernel::UserInputFieldKindV1::Text {
                        multiline: true,
                        max_chars: 2_000,
                    },
                }],
                allowed_actions: vec![
                    sigil_kernel::UserInputActionV1::Submit,
                    sigil_kernel::UserInputActionV1::Decline,
                ],
                requested_at_unix_ms: now_ms,
                continuation: None,
            })?;
        session.append_user_input_lifecycle(vec![
            sigil_kernel::UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())),
        ])?;
        Ok(requested)
    }

    fn plan_review_revision_request(
        session: &Session,
        base_plan_id: &PlanId,
        base_plan_hash: &str,
        revision_request_id: sigil_kernel::UserInputRequestId,
        guidance: &str,
        workspace_snapshot_id: Option<String>,
    ) -> Result<PlanReviewRunRequest> {
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let base = review_projection
            .attempt_for_plan(base_plan_id)
            .cloned()
            .context("revision base plan is not bound to a plan review lifecycle")?;
        let latest = review_projection
            .latest_attempt(&base.plan_review_id)
            .cloned()
            .context("revision lifecycle has no attempt")?;
        let ordinal = if latest.revision_request_id.as_ref() == Some(&revision_request_id) {
            latest.attempt_ordinal.saturating_add(1)
        } else {
            1
        };
        let attempt_id = plan_review_attempt_id_for_revision_ordinal(
            &base.plan_review_id,
            &revision_request_id,
            ordinal,
        );
        let next_plan_id = plan_review_plan_id_for_attempt(&base.plan_review_id, &attempt_id);
        let original_objective = plan_review_original_objective_from_attempt(session, &base)?;
        let objective = format!(
            "{original_objective}\n\nUser revision guidance:\n{}",
            safe_persistence_text(guidance)
        );
        Ok(PlanReviewRunRequest {
            plan_review_id: base.plan_review_id.clone(),
            attempt_id: attempt_id.clone(),
            plan_id: next_plan_id,
            source: base.source,
            source_turn: base.source_turn,
            route_decision_id: base.route_decision_id,
            child_session_ref: plan_review_child_session_ref(&base.plan_review_id, &attempt_id),
            finalizer_session_ref: plan_review_finalizer_session_ref(
                &base.plan_review_id,
                &attempt_id,
                1,
            ),
            revision_request_id: Some(revision_request_id),
            attempt_ordinal: ordinal,
            base_plan_id: Some(base_plan_id.clone()),
            base_plan_hash: Some(base_plan_hash.to_owned()),
            explicit_objective: base.explicit_objective.clone(),
            objective,
            workspace_snapshot_id,
        })
    }

    /// Atomically accepts host-owned revision guidance and records the matching base-plan
    /// revision decision before returning any executable run authority.
    pub fn accept_plan_revision_guidance(
        session: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<(
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PlanReviewRunRequest>,
    )> {
        let projection = session.user_input_projection()?;
        if let Some(previous) = projection.request_for_command(&command.command_id).cloned() {
            let receipt = sigil_kernel::accept_user_input_decision(session, command, now_ms)?;
            let revision_request = Self::recover_unstarted_revision_request(
                session,
                &previous,
                workspace_snapshot_id,
            )?;
            return Ok((receipt, revision_request));
        }
        let state = projection
            .request(&command.identity)
            .cloned()
            .context("revision guidance decision references an unknown request")?;
        if state.requested.request_hash != command.request_hash {
            bail!("revision guidance decision does not bind the exact request hash");
        }
        let (base_plan_id, base_plan_hash) = match &state.requested.request.source {
            sigil_kernel::UserInputSourceV1::PlanRevision {
                base_plan_id,
                base_plan_hash,
            } => (base_plan_id.clone(), base_plan_hash.clone()),
            _ => bail!("user-input request is not plan revision guidance"),
        };
        let accepted = sigil_kernel::UserInputDecisionAcceptedV1::new(
            &state.requested,
            command.command_id,
            command.decision,
            now_ms,
        )?;
        let (resolution, revision_request, plan_decision) = match &accepted.decision {
            sigil_kernel::UserInputDurableDecisionV1::Submitted {
                answers: Some(answers),
                ..
            } => {
                let guidance = answers
                    .iter()
                    .find(|answer| answer.question_id == "revision_guidance")
                    .and_then(|answer| match &answer.value {
                        sigil_kernel::UserInputAnswerValueV1::Text { value } => {
                            Some(value.as_str())
                        }
                        _ => None,
                    })
                    .context("revision guidance answer is missing its text value")?;
                let plan_projection = session.plan_artifact_projection();
                let draft = plan_projection
                    .plans
                    .get(&base_plan_id)
                    .context("revision guidance base plan is missing")?;
                if draft.plan_hash != base_plan_hash {
                    bail!("revision guidance base plan hash is stale");
                }
                if let Some(existing) = plan_projection.latest_decision(&base_plan_id)
                    && !matches!(
                        existing.decision,
                        PlanDecision::RevisionFailed | PlanDecision::SavedOnly
                    )
                {
                    bail!(
                        "revision guidance base plan already has decision {}",
                        existing.decision.as_str()
                    );
                }
                let revision_request = Self::plan_review_revision_request(
                    session,
                    &base_plan_id,
                    &base_plan_hash,
                    state.requested.request.identity.request_id.clone(),
                    guidance,
                    workspace_snapshot_id,
                )?;
                (
                    sigil_kernel::UserInputResolutionV1::Consumed,
                    Some(revision_request),
                    Some(PlanDecisionRecordedEntry {
                        plan_id: base_plan_id.clone(),
                        plan_hash: base_plan_hash.clone(),
                        decision: PlanDecision::RevisionRequested,
                        decided_by: PlanDecisionActor::User,
                        decided_at_ms: now_ms,
                        reason: Some(safe_persistence_text(guidance)),
                    }),
                )
            }
            sigil_kernel::UserInputDurableDecisionV1::Submitted { answers: None, .. } => {
                bail!("revision guidance answer values must be persisted")
            }
            sigil_kernel::UserInputDurableDecisionV1::Declined => {
                (sigil_kernel::UserInputResolutionV1::Declined, None, None)
            }
            sigil_kernel::UserInputDurableDecisionV1::RunCancelled => (
                sigil_kernel::UserInputResolutionV1::RunCancelled,
                None,
                None,
            ),
        };
        let resolved = sigil_kernel::UserInputResolvedV1 {
            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
            identity: state.requested.request.identity.clone(),
            request_hash: state.requested.request_hash.clone(),
            resolution,
            resolved_at_unix_ms: now_ms,
        };
        let mut controls = vec![
            sigil_kernel::UserInputLifecycleEntryV1::DecisionAccepted(Box::new(accepted))
                .into_control(),
        ];
        if let Some(plan_decision) = plan_decision {
            controls.push(ControlEntry::PlanDecisionRecorded(plan_decision));
        }
        controls.push(sigil_kernel::UserInputLifecycleEntryV1::Resolved(resolved).into_control());
        session.append_controls(controls)?;
        let current = session
            .user_input_projection()?
            .request(&state.requested.request.identity)
            .cloned()
            .context("accepted revision guidance lost its durable request")?;
        Ok((
            sigil_kernel::UserInputDecisionReceiptV1 {
                request: current.public_view(),
                idempotent_replay: false,
                continuation_required: false,
            },
            revision_request,
        ))
    }

    fn recover_unstarted_revision_request(
        session: &Session,
        state: &sigil_kernel::UserInputRequestStateV1,
        workspace_snapshot_id: Option<String>,
    ) -> Result<Option<PlanReviewRunRequest>> {
        let (base_plan_id, base_plan_hash) = match &state.requested.request.source {
            sigil_kernel::UserInputSourceV1::PlanRevision {
                base_plan_id,
                base_plan_hash,
            } => (base_plan_id, base_plan_hash),
            _ => return Ok(None),
        };
        if !session
            .plan_artifact_projection()
            .latest_decision(base_plan_id)
            .is_some_and(|decision| {
                decision.decision == PlanDecision::RevisionRequested
                    && decision.plan_hash == *base_plan_hash
            })
        {
            return Ok(None);
        }
        let request_id = &state.requested.request.identity.request_id;
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        let base_attempt = review_projection
            .attempt_for_plan(base_plan_id)
            .context("revision replay lost its base review attempt")?;
        if review_projection
            .review(&base_attempt.plan_review_id)
            .is_some_and(|review| {
                review
                    .attempts
                    .iter()
                    .any(|attempt| attempt.revision_request_id.as_ref() == Some(request_id))
            })
        {
            return Ok(None);
        }
        let guidance = state
            .decision
            .as_ref()
            .and_then(|decision| match &decision.decision {
                sigil_kernel::UserInputDurableDecisionV1::Submitted {
                    answers: Some(answers),
                    ..
                } => answers.iter().find_map(|answer| {
                    (answer.question_id == "revision_guidance")
                        .then_some(&answer.value)
                        .and_then(|value| match value {
                            sigil_kernel::UserInputAnswerValueV1::Text { value } => {
                                Some(value.as_str())
                            }
                            _ => None,
                        })
                }),
                _ => None,
            })
            .context("revision replay lost its accepted guidance")?;
        Self::plan_review_revision_request(
            session,
            base_plan_id,
            base_plan_hash,
            request_id.clone(),
            guidance,
            workspace_snapshot_id,
        )
        .map(Some)
    }

    /// Reuses already accepted revision guidance while allocating a fresh physical attempt.
    pub fn retry_plan_revision(
        session: &mut Session,
        base_plan_id: &PlanId,
        base_plan_hash: &str,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<Option<PlanReviewRunRequest>> {
        let plan_projection = session.plan_artifact_projection();
        if !plan_projection
            .latest_decision(base_plan_id)
            .is_some_and(|decision| {
                decision.decision == PlanDecision::RevisionFailed
                    && decision.plan_hash == base_plan_hash
            })
        {
            return Ok(None);
        }
        let latest = session
            .user_input_projection()?
            .public_requests()
            .into_iter()
            .filter(|request| {
                matches!(
                    &request.source,
                    sigil_kernel::UserInputSourceV1::PlanRevision {
                        base_plan_id: candidate_id,
                        base_plan_hash: candidate_hash,
                    } if candidate_id == base_plan_id && candidate_hash == base_plan_hash
                )
            })
            .max_by_key(|request| request.identity.generation);
        let Some(latest) = latest else {
            return Ok(None);
        };
        let accepted = session
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(accepted))
                    if accepted.identity == latest.identity =>
                {
                    Some((**accepted).clone())
                }
                _ => None,
            });
        let Some(accepted) = accepted else {
            return Ok(None);
        };
        let guidance = match accepted.decision {
            sigil_kernel::UserInputDurableDecisionV1::Submitted {
                answers: Some(answers),
                ..
            } => answers.into_iter().find_map(|answer| {
                (answer.question_id == "revision_guidance")
                    .then_some(answer.value)
                    .and_then(|value| match value {
                        sigil_kernel::UserInputAnswerValueV1::Text { value } => Some(value),
                        _ => None,
                    })
            }),
            _ => None,
        };
        let Some(guidance) = guidance else {
            return Ok(None);
        };
        let request = Self::plan_review_revision_request(
            session,
            base_plan_id,
            base_plan_hash,
            latest.identity.request_id,
            &guidance,
            workspace_snapshot_id,
        )?;
        session.append_control(ControlEntry::PlanDecisionRecorded(
            PlanDecisionRecordedEntry {
                plan_id: base_plan_id.clone(),
                plan_hash: base_plan_hash.to_owned(),
                decision: PlanDecision::RevisionRequested,
                decided_by: PlanDecisionActor::User,
                decided_at_ms: now_ms,
                reason: Some(safe_persistence_text(&guidance)),
            },
        ))?;
        Ok(Some(request))
    }

    /// Records a confirmed failure to start an accepted revision before its attempt exists.
    ///
    /// The caller owns the rejected dispatch. This restores the original Plan without creating
    /// an execution attempt or changing a running/terminal attempt's outcome.
    ///
    /// # Errors
    ///
    /// Rejects stale guidance, changed base Plans, and any revision that already has an attempt.
    pub fn record_unstarted_plan_revision_failure(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        reason: &str,
        now_ms: u64,
    ) -> Result<PlanDecisionRecordedEntry> {
        validate_plan_review_request_objective(session, request)?;
        let request_id = request
            .revision_request_id
            .as_ref()
            .context("revision start failure requires an exact revision request")?;
        let base_plan_id = request
            .base_plan_id
            .as_ref()
            .context("revision start failure requires its base Plan")?;
        let base_plan_hash = request
            .base_plan_hash
            .as_deref()
            .context("revision start failure requires its base Plan hash")?;
        let reviews = PlanReviewProjection::from_entries(session.entries());
        if reviews.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        if reviews
            .reviews()
            .flat_map(|review| review.attempts.iter())
            .any(|attempt| {
                attempt.revision_request_id.as_ref() == Some(request_id)
                    && attempt.attempt_ordinal >= request.attempt_ordinal
            })
        {
            bail!("revision start failure cannot replace an existing attempt");
        }
        let inputs = session.user_input_projection()?;
        let current_request = inputs.public_requests().into_iter().filter(|input| {
            matches!(&input.source,
                sigil_kernel::UserInputSourceV1::PlanRevision { base_plan_id: id, base_plan_hash: hash }
                if id == base_plan_id && hash == base_plan_hash)
        }).max_by_key(|input| input.identity.generation)
            .context("revision start failure lost its guidance request")?;
        if current_request.identity.request_id != *request_id
            || current_request.status != sigil_kernel::UserInputStatusV1::Resolved
            || current_request.resolution != Some(sigil_kernel::UserInputResolutionV1::Consumed)
        {
            bail!("revision start failure does not bind the current accepted guidance");
        }
        let expected = Self::plan_review_revision_request(
            session,
            base_plan_id,
            base_plan_hash,
            request_id.clone(),
            &accepted_revision_guidance_for_request(session, request)?,
            request.workspace_snapshot_id.clone(),
        )?;
        if expected != *request {
            bail!("revision start failure does not bind the expected unstarted attempt");
        }
        let plans = session.plan_artifact_projection();
        if plans
            .plans
            .get(base_plan_id)
            .is_none_or(|draft| draft.plan_hash != base_plan_hash)
        {
            bail!("revision start failure base Plan is stale");
        }
        let decision = plans
            .latest_decision(base_plan_id)
            .filter(|decision| decision.plan_hash == base_plan_hash)
            .context("revision start failure lost its exact base decision")?;
        if decision.decision == PlanDecision::RevisionFailed {
            return Ok(decision.clone());
        }
        if decision.decision != PlanDecision::RevisionRequested {
            bail!("revision start failure cannot replace the current Plan decision");
        }
        let failure = PlanDecisionRecordedEntry {
            plan_id: base_plan_id.clone(),
            plan_hash: base_plan_hash.to_owned(),
            decision: PlanDecision::RevisionFailed,
            decided_by: PlanDecisionActor::System,
            decided_at_ms: now_ms,
            reason: Some(safe_persistence_text(reason)),
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(failure.clone()))?;
        Ok(failure)
    }

    /// Resumes an ordinary or revision Plan review through one exact terminal-frontier CAS.
    ///
    /// The predecessor remains terminal forever. The deterministic successor identity is derived
    /// from the command receipt, so a replay returns the same request while a concurrent command
    /// with a different receipt observes a stale frontier and cannot dispatch a second owner.
    pub fn retry_plan_review(
        session: &mut Session,
        command: &PlanReviewRetryCommand,
        now_ms: u64,
    ) -> Result<PlanReviewRetryReceipt> {
        if command.command_id.trim().is_empty()
            || safe_persistence_text(&command.command_id) != command.command_id
        {
            bail!("plan review retry command id is not bounded safe text");
        }
        let projection = PlanReviewProjection::from_entries(session.entries());
        if projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let latest = projection
            .latest_attempt(&command.plan_review_id)
            .cloned()
            .context("plan review retry has no durable attempt")?;
        let successor_id = sigil_kernel::plan_review_attempt_id_for_retry(
            &command.plan_review_id,
            &command.expected_attempt_id,
            &command.command_id,
        );
        if latest.attempt_id == successor_id {
            let request = plan_review_request_from_attempt(session, &latest)?;
            return Ok(PlanReviewRetryReceipt {
                command_id: command.command_id.clone(),
                predecessor_attempt_id: command.expected_attempt_id.clone(),
                successor_attempt_id: successor_id,
                request,
                idempotent_replay: true,
            });
        }
        if command.expected_durable_frontier != session.durable_frontier_sequence() {
            bail!(
                "plan review retry frontier is stale (expected {}, current {})",
                command.expected_durable_frontier,
                session.durable_frontier_sequence()
            );
        }
        if latest.attempt_id != command.expected_attempt_id {
            bail!(
                "plan review retry expected attempt {} but current attempt is {}",
                command.expected_attempt_id.as_str(),
                latest.attempt_id.as_str()
            );
        }
        if latest.status != command.expected_status {
            bail!(
                "plan review retry expected status {} but current status is {}",
                command.expected_status.as_str(),
                latest.status.as_str()
            );
        }
        if !matches!(
            latest.status,
            PlanReviewAttemptStatus::Paused
                | PlanReviewAttemptStatus::Blocked
                | PlanReviewAttemptStatus::Interrupted
                | PlanReviewAttemptStatus::Failed
        ) {
            bail!(
                "plan review retry requires a terminal paused, blocked, interrupted, or failed attempt"
            );
        }
        let request = plan_review_request_from_attempt(session, &latest)?;
        let context_digest = plan_review_context_digest(session, &latest, &request.objective)?;
        if context_digest != command.expected_context_digest {
            bail!("plan review retry context binding is stale");
        }
        let candidate = session.entries().iter().rev().find_map(|entry| {
            let SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) =
                entry
            else {
                return None;
            };
            (candidate.plan_review_id == latest.plan_review_id
                && candidate.attempt_id == latest.attempt_id
                && candidate.plan_id == latest.plan_id)
                .then(|| (**candidate).clone())
        });
        if let Some(candidate_hash) = command.candidate_hash.as_deref() {
            let Some(candidate) = candidate.as_ref() else {
                bail!("plan review retry supplied a candidate hash but no candidate is durable");
            };
            if candidate.content_hash != candidate_hash {
                bail!("plan review retry candidate hash is stale");
            }
        } else if candidate.as_ref().is_some_and(|candidate| {
            candidate.completeness == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete
        }) {
            bail!("plan review retry must bind the complete preserved candidate hash");
        }
        let successor_plan_id =
            plan_review_plan_id_for_attempt(&latest.plan_review_id, &successor_id);
        let successor_request = PlanReviewRunRequest {
            plan_review_id: latest.plan_review_id.clone(),
            attempt_id: successor_id.clone(),
            plan_id: successor_plan_id,
            source: latest.source,
            source_turn: latest.source_turn.clone(),
            route_decision_id: latest.route_decision_id.clone(),
            child_session_ref: plan_review_child_session_ref(&latest.plan_review_id, &successor_id),
            finalizer_session_ref: plan_review_finalizer_session_ref(
                &latest.plan_review_id,
                &successor_id,
                1,
            ),
            revision_request_id: latest.revision_request_id.clone(),
            attempt_ordinal: latest.attempt_ordinal.saturating_add(1),
            base_plan_id: latest.base_plan_id.clone(),
            base_plan_hash: latest.base_plan_hash.clone(),
            explicit_objective: latest.explicit_objective.clone(),
            objective: request.objective,
            workspace_snapshot_id: command.workspace_snapshot_id.clone(),
        };
        validate_plan_review_request_lineage(&successor_request)?;
        let successor_attempt = PlanReviewAttemptEntry {
            plan_review_id: successor_request.plan_review_id.clone(),
            attempt_id: successor_request.attempt_id.clone(),
            plan_id: successor_request.plan_id.clone(),
            source: successor_request.source,
            source_turn: successor_request.source_turn.clone(),
            route_decision_id: successor_request.route_decision_id.clone(),
            child_session_ref: successor_request.child_session_ref.clone(),
            finalizer_session_ref: Some(successor_request.finalizer_session_ref.clone()),
            revision_request_id: successor_request.revision_request_id.clone(),
            attempt_ordinal: successor_request.attempt_ordinal,
            base_plan_id: successor_request.base_plan_id.clone(),
            base_plan_hash: successor_request.base_plan_hash.clone(),
            explicit_objective: successor_request.explicit_objective.clone(),
            workspace_snapshot_id: successor_request.workspace_snapshot_id.clone(),
            pending_user_input: None,
            status: PlanReviewAttemptStatus::Started,
            terminal_reason: None,
            recorded_at_ms: now_ms,
        };
        projection.validate_append(&successor_attempt)?;
        session.append_controls(vec![ControlEntry::PlanReviewAttempt(successor_attempt)])?;
        Ok(PlanReviewRetryReceipt {
            command_id: command.command_id.clone(),
            predecessor_attempt_id: command.expected_attempt_id.clone(),
            successor_attempt_id: successor_id,
            request: successor_request,
            idempotent_replay: false,
        })
    }

    /// Builds and applies a retry CAS for the latest terminal attempt bound to one public Plan
    /// identity. Product adapters that only retain the bounded public `plan_id` use this helper;
    /// it derives the private review/attempt binding and candidate requirement from the durable
    /// session before appending the deterministic successor.
    pub fn retry_plan_review_for_plan(
        session: &mut Session,
        plan_id: &PlanId,
        expected_candidate_hash: Option<&str>,
        now_ms: u64,
    ) -> Result<PlanReviewRetryReceipt> {
        let projection = PlanReviewProjection::from_entries(session.entries());
        if projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let attempt = projection
            .attempt_for_plan(plan_id)
            .cloned()
            .or_else(|| {
                projection
                    .reviews()
                    .flat_map(|review| review.attempts.iter().rev())
                    .find(|attempt| {
                        attempt.base_plan_id.as_ref() == Some(plan_id)
                            && attempt.status.is_terminal()
                    })
                    .cloned()
            })
            .context("plan review retry has no durable attempt for the requested plan")?;
        let candidate = session.entries().iter().rev().find_map(|entry| {
            let SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) =
                entry
            else {
                return None;
            };
            (candidate.plan_review_id == attempt.plan_review_id
                && candidate.attempt_id == attempt.attempt_id
                && candidate.plan_id == attempt.plan_id)
                .then(|| (**candidate).clone())
        });
        let candidate_hash = match (candidate.as_ref(), expected_candidate_hash) {
            (Some(candidate), Some(expected)) if candidate.content_hash == expected => {
                Some(expected.to_owned())
            }
            (Some(candidate), Some(expected)) => {
                bail!(
                    "plan review retry candidate hash is stale (expected {}, current {})",
                    expected,
                    candidate.content_hash
                )
            }
            (Some(candidate), None)
                if candidate.completeness
                    == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete =>
            {
                bail!("plan review retry must bind the complete preserved candidate hash")
            }
            (_, Some(expected)) if !expected.trim().is_empty() => {
                bail!("plan review retry supplied a candidate hash but no candidate is durable")
            }
            _ => None,
        };
        let command_id = sigil_kernel::stable_event_uuid(
            "sigil-plan-review-retry-command-v1",
            &format!(
                "{}|{}|{}|{}",
                session.session_scope_id(),
                attempt.plan_review_id.as_str(),
                attempt.attempt_id.as_str(),
                expected_candidate_hash.unwrap_or_default()
            ),
        );
        let command = PlanReviewRetryCommand {
            command_id,
            plan_review_id: attempt.plan_review_id.clone(),
            expected_attempt_id: attempt.attempt_id.clone(),
            expected_status: attempt.status,
            expected_durable_frontier: session.durable_frontier_sequence(),
            expected_context_digest: plan_review_context_digest_for_attempt(session, &attempt)?,
            candidate_hash,
            blocker_id: None,
            workspace_snapshot_id: attempt.workspace_snapshot_id.clone(),
        };
        Self::retry_plan_review(session, &command, now_ms)
    }

    /// Accepts a user-input decision owned by the read-only research child and returns the exact
    /// plan-review attempt that must be resumed under a supervisor.
    ///
    /// The parent only carries a public-safe mirror while suspended. The authoritative request,
    /// answer, tool settlement, and continuation claim remain in the child session, so a caller
    /// cannot redirect an answer to a different session or attempt.
    #[cfg(test)]
    pub fn accept_plan_review_research_input(
        parent: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        now_ms: u64,
    ) -> Result<(
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PlanReviewRunRequest>,
        Option<PublicEventOutboxEntryV1>,
    )> {
        let (request, current_waiting_attempt) =
            plan_review_research_input_target(parent, &command)?;
        let mut child = build_plan_review_child_session(parent, &request, None)?;
        let receipt =
            Self::accept_plan_review_research_input_in_child(&mut child, command, now_ms)?;
        Self::finalize_plan_review_research_input(
            parent,
            receipt,
            now_ms,
            request,
            current_waiting_attempt,
        )
    }

    /// Accepts plan-research input through its managed child resource bundle. A cancelled
    /// revision derives its terminal sequence only from the parent's durable public outbox while
    /// committing the exact terminal bundle; adapters never inject a competing live-journal
    /// sequence.
    pub fn accept_plan_review_research_input_with_resources(
        parent: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        now_ms: u64,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<(
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PlanReviewRunRequest>,
        Option<PublicEventOutboxEntryV1>,
    )> {
        let (request, current_waiting_attempt) =
            plan_review_research_input_target(parent, &command)?;
        let expected_scope = command.identity.session_scope_id.as_str().to_owned();
        let bundle = provisioner.mutate_research_session(&request)?;
        let result = bundle.with_child_session(parent, &expected_scope, |child| {
            Self::accept_plan_review_research_input_in_child(child, command, now_ms)
        });
        let receipt = combine_child_resource_settlement(result, bundle.finish())?;
        Self::finalize_plan_review_research_input(
            parent,
            receipt,
            now_ms,
            request,
            current_waiting_attempt,
        )
    }

    /// Reconstructs one exact accepted plan-research input from its managed child session.
    ///
    /// The parent retains only a public-safe Waiting mirror.  Recovery must therefore use the
    /// deterministic `research/0` existing-only managed read admission and inspect its
    /// authoritative child receipt; it must never resolve the legacy parent-relative child
    /// reference or initialize a missing child.  A terminal child cancel or decline remains
    /// recoverable only while the exact parent attempt is still Waiting, because its parent
    /// settlement may have failed after the child accepted the decision.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed lineage, a missing or unreadable managed child log, scope
    /// mismatch, omitted submitted answer values, or multiple foreground recoveries.  The caller
    /// must retain the parent attention surface on error rather than fabricate a decision.
    pub fn recover_managed_plan_review_research_input(
        parent: &Session,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<Option<sigil_kernel::UserInputDecisionCommandV1>> {
        let projection = PlanReviewProjection::from_entries(parent.entries());
        if projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }

        let candidates = projection
            .reviews()
            .filter_map(|review| review.latest_attempt())
            .filter(|attempt| attempt.status == PlanReviewAttemptStatus::WaitingForInput)
            .cloned()
            .collect::<Vec<_>>();
        let mut recovered = Vec::new();
        for attempt in candidates {
            let request = plan_review_request_from_attempt(parent, &attempt)?;
            if let Some(command) = Self::recover_managed_plan_review_research_input_for_attempt(
                &request,
                &attempt,
                provisioner,
            )? {
                recovered.push(command);
            }
        }

        match recovered.len() {
            0 => Ok(None),
            1 => Ok(recovered.pop()),
            _ => bail!("session contains multiple plan-review answers awaiting parent settlement"),
        }
    }

    /// Returns whether the exact suspended revision has a managed child receipt that permits its
    /// worker to resume. This is an executor-side admission check: the parent remains Waiting
    /// until the worker owns the actual `Started` transition, so a crash before worker admission
    /// remains recoverable from the original child receipt.
    ///
    /// # Errors
    ///
    /// Returns an error for a conflicted parent projection or a malformed/missing managed child
    /// receipt. A child cancellation is not a resume permit because its parent terminal is owned
    /// by the existing input-settlement path.
    pub(crate) fn managed_plan_review_research_input_allows_resume(
        parent: &Session,
        request: &PlanReviewRunRequest,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<bool> {
        let projection = PlanReviewProjection::from_entries(parent.entries());
        if projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let Some(attempt) = projection
            .latest_attempt(&request.plan_review_id)
            .filter(|attempt| attempt.attempt_id == request.attempt_id)
        else {
            return Ok(false);
        };
        validate_revision_attempt_request_binding(attempt, request)?;
        if attempt.status != PlanReviewAttemptStatus::WaitingForInput {
            return Ok(false);
        }
        let Some(command) = Self::recover_managed_plan_review_research_input_for_attempt(
            request,
            attempt,
            provisioner,
        )?
        else {
            return Ok(false);
        };
        Ok(matches!(
            command.decision,
            sigil_kernel::UserInputDecisionV1::Submitted { .. }
                | sigil_kernel::UserInputDecisionV1::Declined
        ))
    }

    fn recover_managed_plan_review_research_input_for_attempt(
        request: &PlanReviewRunRequest,
        attempt: &PlanReviewAttemptEntry,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<Option<sigil_kernel::UserInputDecisionCommandV1>> {
        validate_revision_attempt_request_binding(attempt, request)?;
        let pending = attempt
            .pending_user_input
            .as_deref()
            .context("waiting plan-review attempt lost its public input request")?;
        if !matches!(
            &pending.source,
            sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                plan_review_id,
                attempt_id,
            } if plan_review_id == &attempt.plan_review_id && attempt_id == &attempt.attempt_id
        ) {
            bail!("waiting plan-review attempt has an invalid research input binding");
        }
        if pending.identity.root_logical_run_id.as_str() != request.child_logical_run_id() {
            bail!("waiting plan-review input belongs to another logical child run");
        }
        let bundle = provisioner.recover_research_session(request)?;
        let result = (|| {
            let child_entries = bundle.entries(pending.identity.session_scope_id.as_str())?;
            if child_entries.is_empty() {
                bail!("managed plan-review research session log is empty");
            }
            let child_projection =
                sigil_kernel::UserInputProjectionV1::from_session_entries(&child_entries)?;
            let state = child_projection
                .request(&pending.identity)
                .filter(|state| {
                    state.requested.request.identity == pending.identity
                        && state.requested.request_hash == pending.request_hash
                        && matches!(
                            &state.requested.request.source,
                            sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                                plan_review_id,
                                attempt_id,
                            } if plan_review_id == &attempt.plan_review_id
                                && attempt_id == &attempt.attempt_id
                        )
                })
                .context("managed plan-review research child lost its exact input receipt")?;
            recover_plan_review_research_decision_from_child_state(state)
        })();
        combine_child_resource_settlement(result, bundle.finish())
    }

    fn accept_plan_review_research_input_in_child(
        child: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        now_ms: u64,
    ) -> Result<sigil_kernel::UserInputDecisionReceiptV1> {
        if child.session_scope_id() != command.identity.session_scope_id.as_str() {
            bail!("plan-review input belongs to a different child session");
        }
        sigil_kernel::accept_user_input_decision(child, command, now_ms)
    }

    fn finalize_plan_review_research_input(
        parent: &mut Session,
        receipt: sigil_kernel::UserInputDecisionReceiptV1,
        now_ms: u64,
        request: PlanReviewRunRequest,
        current_waiting_attempt: bool,
    ) -> Result<(
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PlanReviewRunRequest>,
        Option<PublicEventOutboxEntryV1>,
    )> {
        if !current_waiting_attempt {
            if receipt.idempotent_replay {
                // Historical requests remain queryable and their exact command receipt is
                // replayable, but an old Waiting fact must never reopen or finalize a newer
                // durable attempt.
                return Ok((receipt, None, None));
            }
            bail!("plan-review input decision no longer binds the current waiting attempt");
        }
        if matches!(
            receipt.request.resolution,
            Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
        ) {
            if request.revision_request_id.is_some() {
                let event = PublicRunEvent::new(
                    parent.session_scope_id(),
                    request.child_logical_run_id(),
                    parent.next_plan_review_public_sequence(&request.child_logical_run_id())?,
                    PublicRunEventKind::RunCancelled,
                );
                let outbox = Self::commit_revision_terminal_with_outbox(
                    parent,
                    &request,
                    &PlanReviewRunOutcome::Cancelled,
                    event,
                    now_ms,
                )?;
                return Ok((receipt, None, Some(outbox)));
            }
            append_command_owned_plan_review_cancellation(parent, &request, now_ms)?;
            return Ok((receipt, None, None));
        }
        Ok((receipt, Some(request), None))
    }

    /// Records a typed user decision for one plan artifact.
    ///
    /// Decisions bind the exact plan id and hash; stale hashes, duplicate acceptance, and
    /// post-task decisions fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, a missing plan, or a conflicting decision.
    pub fn record_plan_decision(
        session: &mut Session,
        command: &PlanDecisionCommand,
        now_ms: u64,
    ) -> Result<PlanDecisionRecordedEntry> {
        if command.decision == PlanDecision::Accepted {
            bail!(
                "Accepted decisions must use PlanExecutionService::approve so Plan approval and the stable Task shell commit together"
            );
        }
        let plan_id = PlanId::new(command.plan_id.clone())
            .map_err(|error| anyhow!("invalid plan id for decision: {error}"))?;
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(&plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != command.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                command.expected_plan_hash,
                draft.plan_hash
            );
        }
        if let Some(existing) = projection.latest_decision(&plan_id)
            && existing.decision == command.decision
            && existing.plan_hash == draft.plan_hash
        {
            return Ok(existing.clone());
        }
        if command.decision == PlanDecision::SavedOnly {
            ensure_plan_action_allowed(
                session,
                &plan_id,
                &command.expected_plan_hash,
                sigil_kernel::PublicPlanAction::Save,
            )?;
        }
        if projection.plan_is_rejected(&plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        if command.decision == PlanDecision::Accepted && projection.task_created_for_plan(&plan_id)
        {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(existing) = projection.latest_decision(&plan_id) {
            if matches!(
                existing.decision,
                PlanDecision::RevisionFailed | PlanDecision::TaskCreationFailed
            ) {
                // The preceding host action never started; the original plan remains actionable.
            } else {
                bail!(
                    "plan {} already has decision {}",
                    plan_id.as_str(),
                    existing.decision.as_str()
                );
            }
        }
        let entry = PlanDecisionRecordedEntry {
            plan_id,
            plan_hash: draft.plan_hash,
            decision: command.decision,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms,
            reason: None,
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))?;
        Ok(entry)
    }

    /// Explicitly adopts one complete, immutable plain-text candidate preserved by a Plan review.
    ///
    /// Adoption is a user-owned presentation decision. It creates the normal Plan draft and
    /// DraftReady attempt transition, but never approves execution or creates a Task.
    pub fn adopt_plan_review_candidate<H>(
        session: &mut Session,
        plan_id: &PlanId,
        expected_candidate_hash: &str,
        handler: &mut H,
        now_ms: u64,
    ) -> Result<PlanDraftCreatedEntry>
    where
        H: EventHandler + ?Sized,
    {
        let candidate = session
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(
                    candidate,
                )) if &candidate.plan_id == plan_id
                    && candidate.content_hash == expected_candidate_hash
                    && candidate.completeness
                        == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete =>
                {
                    Some((**candidate).clone())
                }
                _ => None,
            })
            .context("the requested Plan review candidate is not available")?;
        if let Some(existing) = session.plan_artifact_projection().plans.get(plan_id) {
            if existing.plan_hash == expected_candidate_hash {
                return Ok(existing.clone());
            }
            bail!("plan {} already has a durable draft", plan_id.as_str());
        }
        let projection = PlanReviewProjection::from_entries(session.entries());
        let attempt = projection
            .latest_attempt(&candidate.plan_review_id)
            .filter(|attempt| {
                attempt.attempt_id == candidate.attempt_id && attempt.plan_id == *plan_id
            })
            .cloned()
            .context("the requested Plan review candidate lost its attempt")?;
        if !attempt.status.is_terminal() {
            bail!(
                "the requested Plan review candidate belongs to active attempt {}",
                attempt.attempt_id.as_str()
            );
        }
        let candidate_content = candidate_body_from_record(session, &candidate)?;
        let draft = plain_text_plan_draft_entry_with_plan_id(
            plan_id.clone(),
            &candidate_content,
            candidate.source,
            now_ms,
            attempt.workspace_snapshot_id.clone(),
        )?
        .context("the requested Plan review candidate is not valid Plan text")?;
        // Candidate adoption is a user-owned resolution of a terminal physical attempt. Keep the
        // original attempt immutable and append a deterministic successor so a replayed command
        // cannot rewrite the old paused/blocked/unknown lifecycle into DraftReady in place.
        let command_id = sigil_kernel::stable_event_uuid(
            "sigil-plan-review-candidate-adopt-command-v1",
            &format!(
                "{}|{}|{}|{}",
                session.session_scope_id(),
                candidate.plan_review_id.as_str(),
                candidate.attempt_id.as_str(),
                expected_candidate_hash
            ),
        );
        let successor_attempt_id = sigil_kernel::plan_review_attempt_id_for_retry(
            &candidate.plan_review_id,
            &candidate.attempt_id,
            &command_id,
        );
        let mut ready_attempt = attempt.clone();
        ready_attempt.attempt_id = successor_attempt_id;
        ready_attempt.attempt_ordinal = attempt.attempt_ordinal.saturating_add(1);
        ready_attempt.child_session_ref = sigil_kernel::plan_review_child_session_ref(
            &candidate.plan_review_id,
            &ready_attempt.attempt_id,
        );
        ready_attempt.finalizer_session_ref = None;
        ready_attempt.status = PlanReviewAttemptStatus::DraftReady;
        ready_attempt.terminal_reason = None;
        ready_attempt.recorded_at_ms = now_ms;
        let resolution = sigil_kernel::PlanReviewResolutionRecordedV1 {
            schema_version: sigil_kernel::PLAN_REVIEW_RESOLUTION_SCHEMA_VERSION,
            plan_review_id: candidate.plan_review_id.clone(),
            attempt_id: candidate.attempt_id.clone(),
            candidate_hash: expected_candidate_hash.to_owned(),
            outcome: sigil_kernel::PlanReviewResultOutcome::Draft,
            actor: sigil_kernel::PlanReviewResolutionActorV1::User,
            receipt_id: command_id,
            recorded_at_ms: now_ms,
        };
        handler.commit_controls(
            session,
            vec![
                ControlEntry::PlanDraftCreated(draft.clone()),
                ControlEntry::PlanReviewResolutionRecordedV1(resolution),
                ControlEntry::PlanReviewAttempt(ready_attempt),
            ],
        )?;
        Ok(draft)
    }

    /// Creates a durable task from an accepted plan through the shared RFC-0018 handoff.
    ///
    /// This is the single Plan-to-Task promotion path used by TUI, HTTP, and Desktop. The function
    /// is idempotent: retries reconcile the deterministic prefix; conflicting facts fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, missing/rejected plan, step-limit violation, unsafe
    /// promotion, or conflicting durable prefix facts.
    #[cfg(test)]
    pub fn create_task_from_plan(
        session: &mut Session,
        root_config: &RootConfig,
        workspace_root: &Path,
        parent_session_ref: SessionRef,
        request: &CreateTaskFromPlanRequest,
    ) -> Result<CreatedTaskFromPlan> {
        let result = Self::create_task_from_plan_inner(
            session,
            root_config,
            workspace_root,
            parent_session_ref,
            request,
        );
        if let Err(error) = &result
            && let Err(record_error) = Self::record_task_creation_failure(
                session,
                request,
                &format!("{error:#}"),
                now_ms(),
            )
        {
            return Err(anyhow!(
                "{error:#}; failed to record task creation failure: {record_error:#}"
            ));
        }
        result
    }

    #[cfg(test)]
    fn create_task_from_plan_inner(
        session: &mut Session,
        root_config: &RootConfig,
        workspace_root: &Path,
        parent_session_ref: SessionRef,
        request: &CreateTaskFromPlanRequest,
    ) -> Result<CreatedTaskFromPlan> {
        let plan_id = PlanId::new(request.plan_id.clone())
            .map_err(|error| anyhow!("invalid plan id for task creation: {error}"))?;
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(&plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != request.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                request.expected_plan_hash,
                draft.plan_hash
            );
        }
        let exact_prefix_exists = projection
            .latest_decision(&plan_id)
            .is_some_and(|decision| {
                decision.decision == PlanDecision::Accepted
                    && decision.plan_hash == request.expected_plan_hash
            })
            || projection
                .tasks_created
                .get(&plan_id)
                .and_then(|entries| entries.last())
                .is_some_and(|created| created.plan_hash == request.expected_plan_hash);
        if !exact_prefix_exists {
            ensure_plan_action_allowed(
                session,
                &plan_id,
                &request.expected_plan_hash,
                sigil_kernel::PublicPlanAction::Run,
            )?;
        }
        if projection.plan_is_rejected(&plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        let current_workspace_snapshot_id =
            plan_handoff_workspace_snapshot_id(root_config, workspace_root)?;
        let stale_reason = plan_handoff_stale_reason(
            draft.workspace_snapshot_id.as_deref(),
            current_workspace_snapshot_id.as_deref(),
        );
        let task_id = task_id_from_plan_draft(&draft)?;
        let task_id_value = task_id.as_str().to_owned();
        let objective = plan_task_input_from_draft(&draft);
        let decision = PlanDecisionRecordedEntry {
            plan_id: plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            decision: PlanDecision::Accepted,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms(),
            reason: Some("created task from plan".to_owned()),
        };
        if draft.steps.len() > root_config.task.max_plan_steps {
            bail!(
                "plan {} has {} steps, exceeding task.max_plan_steps={}",
                plan_id.as_str(),
                draft.steps.len(),
                root_config.task.max_plan_steps
            );
        }
        let promoted = if stale_reason.is_none() {
            task_plan_from_plan_draft(&draft, task_id.clone(), 1)?
        } else {
            None
        };
        let (task_plan, step_contracts, step_mapping, intent_admission) = match promoted {
            Some(promotion) => {
                let step_contracts = promotion.step_contracts;
                let mut task_plan = promotion.task_plan;
                let intent_admission = match draft.intent_proposal.as_ref() {
                    Some(proposal) => {
                        let workspace_id = stable_workspace_id(workspace_root)
                            .map_err(|error| anyhow!("failed to scope IntentPlan: {error}"))?;
                        let stack_id = IntentStackId::new(stable_event_uuid(
                            "sigil-plan-intent-stack-v1",
                            &format!("{}:{}", draft.plan_id.as_str(), draft.plan_hash),
                        ))?;
                        let context = IntentAdmissionContextV1::initial(
                            stack_id,
                            workspace_id,
                            session.session_scope_id().to_owned(),
                        )?;
                        let authority_event_id = stable_event_uuid(
                            "sigil-plan-intent-acceptance-v1",
                            &format!(
                                "{}:{}:{}",
                                draft.plan_id.as_str(),
                                draft.plan_hash,
                                task_id.as_str()
                            ),
                        );
                        let authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
                            proposal.source_turn_id.clone(),
                            authority_event_id,
                            proposal.proposal_digest.clone(),
                        )?;
                        let admission =
                            admit_suggested_decomposition(&context, proposal, &authority)?;
                        task_plan = bind_task_plan_intents(
                            &admission,
                            task_plan,
                            &promotion.intent_alias_bindings,
                        )?;
                        Some(admission)
                    }
                    None => {
                        if !promotion.intent_alias_bindings.is_empty() {
                            bail!(
                                "plan {} carries intent aliases without a digest-bound proposal",
                                plan_id.as_str()
                            );
                        }
                        None
                    }
                };
                (
                    Some(task_plan),
                    step_contracts,
                    promotion.step_mapping,
                    intent_admission,
                )
            }
            None => (None, Vec::new(), Vec::new(), None),
        };
        let existing_accepted_plan = session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .and_then(|task| {
                task.plans
                    .values()
                    .find(|plan| plan.status == TaskPlanStatus::Accepted)
            })
            .cloned();
        if task_plan.is_none()
            && let Some(existing_plan) = existing_accepted_plan
        {
            session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: existing_plan.plan_version,
                status: TaskPlanStatus::Superseded,
                steps: existing_plan.steps,
                reason: Some(
                    "workspace drift invalidated a crash-interrupted plan promotion".to_owned(),
                ),
            }))?;
            let existing_task = session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .cloned()
                .ok_or_else(|| anyhow!("stale promoted task prefix is missing its task run"))?;
            session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: existing_task.parent_session_ref,
                objective: existing_task.objective,
                title: None,
                status: TaskRunStatus::Cancelled,
                reason: Some(
                    "plan creation cancelled because the workspace changed before commit"
                        .to_owned(),
                ),
            }))?;
            bail!(
                "plan {} creation prefix conflicts with current workspace drift; refusing to execute an earlier promoted task plan",
                plan_id.as_str()
            );
        }
        let task_created = TaskCreatedFromPlanEntry {
            plan_id: plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            task_id: task_id.clone(),
            task_plan_version: task_plan.as_ref().map_or(0, |plan| plan.plan_version),
            step_mapping: step_mapping.clone(),
            stale_reason,
            created_at_ms: now_ms(),
        };
        let permission_grant = match request.permission_grant {
            Some(permission) => {
                if draft.target_paths.is_empty() {
                    bail!(
                        "plan {} has no concrete target paths for scoped edits",
                        plan_id.as_str()
                    );
                }
                Some(PlanPermissionGrantedEntry {
                    plan_id: plan_id.clone(),
                    plan_hash: draft.plan_hash.clone(),
                    task_id: task_id.clone(),
                    workspace_snapshot_id: current_workspace_snapshot_id,
                    permission,
                    scope: PlanApprovalScope {
                        summary: format!("scoped edits for task {}", task_id.as_str()),
                        workspace_paths: draft.target_paths.clone(),
                    },
                    expires: PlanApprovalExpiry::Session,
                    granted_at_ms: now_ms(),
                })
            }
            None => None,
        };

        let desired_task_status = if request.start_mode == PlanTaskStartMode::CreatePaused {
            TaskRunStatus::Paused
        } else {
            TaskRunStatus::Started
        };
        let safe_objective = safe_persistence_text(&objective);
        let existing_task = session.task_state_projection().tasks.get(&task_id).cloned();
        match existing_task {
            Some(existing)
                if existing.parent_session_ref == parent_session_ref
                    && existing.objective == safe_objective
                    && existing.status == desired_task_status => {}
            Some(existing)
                if existing.parent_session_ref == parent_session_ref
                    && existing.objective == safe_objective
                    && existing.status == TaskRunStatus::Paused
                    && desired_task_status == TaskRunStatus::Started
                    && existing.participant_attempts.is_empty()
                    && existing.steps.is_empty() =>
            {
                session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                    task_id: task_id.clone(),
                    parent_session_ref: parent_session_ref.clone(),
                    objective: safe_objective.clone(),
                    title: Some(sigil_kernel::task_semantic_title(&draft.summary)),
                    status: TaskRunStatus::Started,
                    reason: Some(format!(
                        "resumed crash-interrupted creation from plan {}",
                        plan_id.as_str()
                    )),
                }))?;
            }
            Some(_) => {
                bail!(
                    "plan {} task prefix conflicts with the requested task facts",
                    plan_id.as_str()
                );
            }
            None => session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: safe_objective,
                title: Some(sigil_kernel::task_semantic_title(&draft.summary)),
                status: desired_task_status,
                reason: Some(format!("created from plan {}", plan_id.as_str())),
            }))?,
        }

        if let Some(task_plan) = task_plan {
            let existing_plan = session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .and_then(|task| task.plans.get(&task_plan.plan_version))
                .cloned();
            let plan_already_exists = match existing_plan {
                Some(existing)
                    if existing.plan_version == task_plan.plan_version
                        && existing.status == task_plan.status
                        && existing.steps == task_plan.steps
                        && existing.reason == task_plan.reason =>
                {
                    true
                }
                Some(_) => {
                    bail!(
                        "plan {} task-plan prefix conflicts with direct promotion",
                        plan_id.as_str()
                    );
                }
                None => false,
            };
            if let Some(admission) = intent_admission.as_ref() {
                append_task_intent_plan_admission_with_step_contracts(
                    session,
                    admission,
                    task_plan.clone(),
                    step_contracts.clone(),
                )?;
            } else if !plan_already_exists {
                let mut controls = Vec::with_capacity(step_contracts.len().saturating_add(2));
                controls.push(ControlEntry::TaskPlan(task_plan.clone()));
                controls.extend(
                    step_contracts
                        .iter()
                        .cloned()
                        .map(ControlEntry::TaskStepContractBoundV2),
                );
                controls.push(ControlEntry::TaskPlanContractSetCommittedV2(
                    sigil_kernel::TaskPlanContractSetCommittedV2::new(&task_plan, &step_contracts)?,
                ));
                session.append_controls(controls)?;
            } else {
                let task_projection = session.task_state_projection();
                let existing_contracts = task_projection
                    .tasks
                    .get(&task_id)
                    .and_then(|task| task.plans.get(&task_plan.plan_version))
                    .map(|plan| &plan.step_contracts)
                    .context("directly promoted task plan disappeared during contract replay")?;
                let mut missing = Vec::new();
                for step_contract in &step_contracts {
                    match existing_contracts.get(&step_contract.step_id) {
                        Some(contract) if contract == &step_contract.contract => {}
                        Some(_) => bail!(
                            "plan {} task-step contract conflicts with direct promotion",
                            plan_id.as_str()
                        ),
                        None => missing
                            .push(ControlEntry::TaskStepContractBoundV2(step_contract.clone())),
                    }
                }
                let committed = task_projection
                    .tasks
                    .get(&task_id)
                    .and_then(|task| task.plans.get(&task_plan.plan_version))
                    .is_some_and(|plan| plan.contract_set_committed_v2);
                if !committed {
                    missing.push(ControlEntry::TaskPlanContractSetCommittedV2(
                        sigil_kernel::TaskPlanContractSetCommittedV2::new(
                            &task_plan,
                            &step_contracts,
                        )?,
                    ));
                }
                if !missing.is_empty() {
                    session.append_controls(missing)?;
                }
            }
        }

        if let Some(grant) = permission_grant {
            let existing_grants = session
                .plan_artifact_projection()
                .permission_grants
                .get(&plan_id)
                .cloned()
                .unwrap_or_default();
            if existing_grants.iter().any(|existing| {
                existing.plan_hash == grant.plan_hash
                    && existing.task_id == grant.task_id
                    && existing.workspace_snapshot_id == grant.workspace_snapshot_id
                    && existing.permission == grant.permission
                    && existing.scope == grant.scope
                    && existing.expires == grant.expires
            }) {
                // The crash-prefix retry already persisted this exact grant.
            } else if existing_grants
                .iter()
                .any(|existing| existing.task_id == task_id)
            {
                bail!(
                    "plan {} already has a conflicting permission grant for this task",
                    plan_id.as_str()
                );
            } else {
                session.append_control(ControlEntry::PlanPermissionGranted(grant))?;
            }
        }

        let existing_created = session
            .plan_artifact_projection()
            .tasks_created
            .get(&plan_id)
            .and_then(|entries| entries.last())
            .cloned();
        match existing_created {
            Some(existing)
                if existing.plan_id == task_created.plan_id
                    && existing.plan_hash == task_created.plan_hash
                    && existing.task_id == task_created.task_id
                    && existing.task_plan_version == task_created.task_plan_version
                    && existing.step_mapping == task_created.step_mapping
                    && existing.stale_reason == task_created.stale_reason => {}
            Some(_) => {
                bail!(
                    "plan {} already has a conflicting task-created anchor",
                    plan_id.as_str()
                );
            }
            None => {
                session.append_control(ControlEntry::TaskCreatedFromPlan(task_created.clone()))?
            }
        }

        let existing_decision = session
            .plan_artifact_projection()
            .latest_decision(&plan_id)
            .cloned();
        match existing_decision {
            Some(existing)
                if existing.decision == PlanDecision::Accepted
                    && existing.plan_hash == draft.plan_hash => {}
            Some(existing) if existing.decision == PlanDecision::Accepted => {
                bail!(
                    "plan {} already has an accepted decision for another hash",
                    plan_id.as_str()
                );
            }
            _ => session.append_control(ControlEntry::PlanDecisionRecorded(decision))?,
        }

        let entries = session.entries().to_vec();
        Ok(CreatedTaskFromPlan {
            task_id,
            task_id_value,
            objective,
            entry: task_created,
            start_mode: request.start_mode,
            entries,
        })
    }

    /// Records a failed Run action without consuming the immutable plan.
    ///
    /// Invalid ids, stale hashes and already-created tasks do not acquire new durable authority,
    /// so they remain ordinary request errors. Exact pending plans receive a bounded system
    /// settlement that survives reload and permits a later retry.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact pending plan exists but its failure settlement conflicts
    /// with durable decision state or cannot be appended.
    #[cfg(test)]
    pub fn record_task_creation_failure(
        session: &mut Session,
        request: &CreateTaskFromPlanRequest,
        reason: &str,
        now_ms: u64,
    ) -> Result<Option<PlanDecisionRecordedEntry>> {
        let Ok(plan_id) = PlanId::new(request.plan_id.clone()) else {
            return Ok(None);
        };
        let projection = session.plan_artifact_projection();
        let Some(draft) = projection.plans.get(&plan_id) else {
            return Ok(None);
        };
        if draft.plan_hash != request.expected_plan_hash
            || projection.task_created_for_plan(&plan_id)
        {
            return Ok(None);
        }
        let safe_reason = safe_persistence_text(reason);
        let reason = safe_reason.chars().take(512).collect::<String>();
        if let Some(existing) = projection.latest_decision(&plan_id) {
            match existing.decision {
                PlanDecision::TaskCreationFailed if existing.reason.as_deref() == Some(&reason) => {
                    return Ok(Some(existing.clone()));
                }
                PlanDecision::SavedOnly
                | PlanDecision::RevisionFailed
                | PlanDecision::TaskCreationFailed => {}
                PlanDecision::Accepted
                | PlanDecision::Rejected
                | PlanDecision::RevisionRequested
                | PlanDecision::RevisionSucceeded => return Ok(None),
            }
        }
        let entry = PlanDecisionRecordedEntry {
            plan_id,
            plan_hash: draft.plan_hash.clone(),
            decision: PlanDecision::TaskCreationFailed,
            decided_by: PlanDecisionActor::System,
            decided_at_ms: now_ms,
            reason: Some(reason),
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))?;
        Ok(Some(entry))
    }

    /// Discards a plan durably.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, a missing plan, an existing task, or an existing
    /// decision.
    pub fn reject_plan(session: &mut Session, request: &RejectPlanRequest) -> Result<RejectedPlan> {
        let plan_id = PlanId::new(request.plan_id.clone())
            .map_err(|error| anyhow!("invalid plan id for rejection: {error}"))?;
        let projection = session.plan_artifact_projection();
        let draft = projection
            .plans
            .get(&plan_id)
            .ok_or_else(|| anyhow!("plan {} is not present in this session", plan_id.as_str()))?;
        if draft.plan_hash != request.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                request.expected_plan_hash,
                draft.plan_hash
            );
        }
        if let Some(decision) = projection.latest_decision(&plan_id)
            && decision.decision == PlanDecision::Rejected
            && decision.plan_hash == draft.plan_hash
        {
            return Ok(RejectedPlan {
                entry: decision.clone(),
                entries: session.entries().to_vec(),
            });
        }
        ensure_plan_action_allowed(
            session,
            &plan_id,
            &request.expected_plan_hash,
            sigil_kernel::PublicPlanAction::Reject,
        )?;
        if projection.task_created_for_plan(&plan_id) {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(decision) = projection.latest_decision(&plan_id) {
            match decision.decision {
                PlanDecision::SavedOnly
                | PlanDecision::RevisionFailed
                | PlanDecision::TaskCreationFailed => {}
                _ => bail!(
                    "plan {} already has decision {}",
                    plan_id.as_str(),
                    decision.decision.as_str()
                ),
            }
        }
        let entry = PlanDecisionRecordedEntry {
            plan_id,
            plan_hash: draft.plan_hash.clone(),
            decision: PlanDecision::Rejected,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms(),
            reason: Some("discarded plan".to_owned()),
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))?;
        let entries = session.entries().to_vec();
        Ok(RejectedPlan { entry, entries })
    }
}

fn invalid_plan_draft_submission_reason(session: &Session) -> Option<String> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::ToolResultV3(result) = entry else {
            return None;
        };
        if result.tool_name != sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME
            && result.tool_name != sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME
            && result.tool_name != sigil_kernel::CONFIRM_PLAN_REVIEW_CANDIDATE_TOOL_NAME
            || result.facts.status != "error"
        {
            return None;
        }
        let detail = result
            .facts
            .error
            .as_ref()
            .map(|error| error.message.as_str())
            .unwrap_or("typed draft validation failed");
        Some(format!(
            "submit-only finalizer produced an invalid draft: {detail}"
        ))
    })
}

/// Reads a successful typed `no_plan` result from the child session's durable tool projection.
/// The result content is a bounded JSON envelope, so recovery never infers the outcome from
/// natural-language text or from an in-memory disposition alone.
fn plan_review_no_plan_reason(session: &Session) -> Option<String> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::ToolResultV3(result) = entry else {
            return None;
        };
        if result.tool_name != sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME
            || result.facts.status != "ok"
        {
            return None;
        }
        let envelope =
            sigil_kernel::decode_plan_review_result(&result.initial_model_view.preview).ok()?;
        (envelope.outcome == sigil_kernel::PlanReviewResultOutcome::NoPlan)
            .then_some(envelope.content)
    })
}

fn plan_review_candidate_content(
    session: &Session,
    request: &PlanReviewRunRequest,
) -> Result<Option<String>> {
    let candidate = session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) =
            entry
        else {
            return None;
        };
        (candidate.plan_review_id == request.plan_review_id
            && candidate.attempt_id == request.attempt_id
            && candidate.plan_id == request.plan_id
            && candidate.completeness == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete)
            .then(|| (**candidate).clone())
    });
    candidate
        .as_ref()
        .map(|candidate| candidate_body_from_record(session, candidate))
        .transpose()
}

/// Builds the model-resolution receipt when a typed draft confirms a previously preserved
/// complete candidate. Typed drafts that have no candidate are already self-describing and do
/// not need a second evidence record; candidate-backed drafts must retain the exact hash and
/// source receipt so recovery can distinguish confirmation from regeneration.
fn plan_review_resolution_for_draft_candidate(
    parent: &Session,
    request: &PlanReviewRunRequest,
    draft: &PlanDraftCreatedEntry,
    recorded_at_ms: u64,
) -> Result<Option<sigil_kernel::PlanReviewResolutionRecordedV1>> {
    let candidate = parent.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) =
            entry
        else {
            return None;
        };
        (candidate.plan_review_id == request.plan_review_id
            && candidate.attempt_id == request.attempt_id
            && candidate.plan_id == request.plan_id
            && candidate.completeness == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete
            && candidate.content_hash == draft.plan_hash)
            .then(|| (**candidate).clone())
    });
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    if parent.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewResolutionRecordedV1(existing))
                if existing.plan_review_id == request.plan_review_id
                    && existing.attempt_id == request.attempt_id
                    && existing.candidate_hash == candidate.content_hash
                    && existing.outcome == sigil_kernel::PlanReviewResultOutcome::Draft
        )
    }) {
        return Ok(None);
    }
    let receipt_id = candidate.source_event_id.clone().unwrap_or_else(|| {
        sigil_kernel::stable_event_uuid(
            "sigil-plan-review-model-resolution-v1",
            &format!("{}|{}", request.attempt_id.as_str(), candidate.content_hash),
        )
    });
    Ok(Some(sigil_kernel::PlanReviewResolutionRecordedV1 {
        schema_version: sigil_kernel::PLAN_REVIEW_RESOLUTION_SCHEMA_VERSION,
        plan_review_id: request.plan_review_id.clone(),
        attempt_id: request.attempt_id.clone(),
        candidate_hash: candidate.content_hash,
        outcome: sigil_kernel::PlanReviewResultOutcome::Draft,
        actor: sigil_kernel::PlanReviewResolutionActorV1::Model,
        receipt_id,
        recorded_at_ms,
    }))
}

fn plan_review_resolution_for_no_plan_candidate(
    parent: &Session,
    request: &PlanReviewRunRequest,
    recorded_at_ms: u64,
) -> Result<Option<sigil_kernel::PlanReviewResolutionRecordedV1>> {
    let candidate = parent.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) =
            entry
        else {
            return None;
        };
        (candidate.plan_review_id == request.plan_review_id
            && candidate.attempt_id == request.attempt_id
            && candidate.plan_id == request.plan_id
            && candidate.completeness == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete)
            .then(|| (**candidate).clone())
    });
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    if parent.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewResolutionRecordedV1(existing))
                if existing.plan_review_id == request.plan_review_id
                    && existing.attempt_id == request.attempt_id
                    && existing.candidate_hash == candidate.content_hash
                    && existing.outcome == sigil_kernel::PlanReviewResultOutcome::NoPlan
        )
    }) {
        return Ok(None);
    }
    let receipt_id = candidate.source_event_id.clone().unwrap_or_else(|| {
        sigil_kernel::stable_event_uuid(
            "sigil-plan-review-model-no-plan-resolution-v1",
            &format!("{}|{}", request.attempt_id.as_str(), candidate.content_hash),
        )
    });
    Ok(Some(sigil_kernel::PlanReviewResolutionRecordedV1 {
        schema_version: sigil_kernel::PLAN_REVIEW_RESOLUTION_SCHEMA_VERSION,
        plan_review_id: request.plan_review_id.clone(),
        attempt_id: request.attempt_id.clone(),
        candidate_hash: candidate.content_hash,
        outcome: sigil_kernel::PlanReviewResultOutcome::NoPlan,
        actor: sigil_kernel::PlanReviewResolutionActorV1::Model,
        receipt_id,
        recorded_at_ms,
    }))
}

fn candidate_body_from_record(
    session: &Session,
    candidate: &sigil_kernel::PlanReviewCandidateRecordedV1,
) -> Result<String> {
    let content = if let Some(artifact) = candidate.content_artifact.as_ref() {
        let store = session
            .tool_artifact_store()
            .context("plan review candidate body requires its managed artifact store")?;
        let bytes = store.read_all(artifact)?;
        String::from_utf8(bytes).context("plan review candidate artifact is not UTF-8")?
    } else {
        candidate.content.clone()
    };
    let content = safe_persistence_text(content.trim());
    if plan_text_hash(&content) != candidate.content_hash {
        bail!("plan review candidate body hash does not match its durable record");
    }
    Ok(content)
}

fn candidate_preview(content: &str) -> String {
    let mut preview = String::new();
    for character in content.chars() {
        if preview.len() + character.len_utf8()
            > sigil_kernel::PLAN_REVIEW_CANDIDATE_PREVIEW_MAX_BYTES
        {
            break;
        }
        preview.push(character);
    }
    preview
}

fn candidate_record_for_session(
    session: &Session,
    request: &PlanReviewRunRequest,
    content: &str,
    source_event_id: Option<String>,
    completeness: sigil_kernel::PlanReviewCandidateCompletenessV1,
    recorded_at_ms: u64,
) -> Result<sigil_kernel::PlanReviewCandidateRecordedV1> {
    let safe = safe_persistence_text(content.trim());
    if safe.len() <= sigil_kernel::PLAN_REVIEW_CANDIDATE_PREVIEW_MAX_BYTES {
        return sigil_kernel::plan_review_candidate_recorded_entry(
            request.plan_review_id.clone(),
            request.attempt_id.clone(),
            request.plan_id.clone(),
            request.plan_source_ref(),
            source_event_id,
            &safe,
            completeness,
            recorded_at_ms,
        );
    }
    let store = session.tool_artifact_store().context(
        "plan review candidate exceeds inline limit and requires managed artifact storage",
    )?;
    let artifact = store.capture_text(
        &format!("plan-review-candidate:{}", request.attempt_id.as_str()),
        "plan_review_candidate",
        &safe,
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    if !matches!(
        artifact.completeness,
        sigil_kernel::ToolArtifactCompleteness::Complete
    ) {
        bail!("plan review candidate managed artifact was truncated");
    }
    sigil_kernel::plan_review_candidate_recorded_entry_with_artifact(
        request.plan_review_id.clone(),
        request.attempt_id.clone(),
        request.plan_id.clone(),
        request.plan_source_ref(),
        source_event_id,
        &candidate_preview(&safe),
        artifact,
        completeness,
        recorded_at_ms,
    )
}

fn record_plan_review_candidate(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    content: &str,
    source_event_id: Option<String>,
    completeness: sigil_kernel::PlanReviewCandidateCompletenessV1,
    recorded_at_ms: u64,
) -> Result<Option<String>> {
    let safe_content = safe_persistence_text(content.trim());
    if safe_content.is_empty() {
        return Ok(None);
    }
    let content_hash = plan_text_hash(&safe_content);
    if let Some(existing) = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate))
                if candidate.plan_review_id == request.plan_review_id
                    && candidate.attempt_id == request.attempt_id
                    && candidate.plan_id == request.plan_id
                    && candidate.content_hash == content_hash
                    && candidate.completeness == completeness =>
            {
                Some((**candidate).clone())
            }
            _ => None,
        })
    {
        return (completeness == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete)
            .then(|| candidate_body_from_record(session, &existing))
            .transpose();
    }
    let candidate = candidate_record_for_session(
        session,
        request,
        &safe_content,
        source_event_id,
        completeness,
        recorded_at_ms,
    )?;
    if !session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(existing))
                if existing.plan_review_id == request.plan_review_id
                    && existing.attempt_id == request.attempt_id
                    && existing.content_hash == candidate.content_hash
        )
    }) {
        session.append_control(ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
            candidate.clone(),
        )))?;
    }
    (completeness == sigil_kernel::PlanReviewCandidateCompletenessV1::Complete)
        .then(|| candidate_body_from_record(session, &candidate))
        .transpose()
}

fn record_plan_review_candidate_in_parent<H>(
    parent: &mut Session,
    request: &PlanReviewRunRequest,
    content: &str,
    source_event_id: Option<String>,
    completeness: sigil_kernel::PlanReviewCandidateCompletenessV1,
    handler: &mut H,
    recorded_at_ms: u64,
) -> Result<()>
where
    H: EventHandler + ?Sized,
{
    let safe_content = safe_persistence_text(content.trim());
    if safe_content.is_empty() {
        return Ok(());
    }
    let content_hash = plan_text_hash(&safe_content);
    if parent.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(existing))
                if existing.plan_review_id == request.plan_review_id
                    && existing.attempt_id == request.attempt_id
                    && existing.plan_id == request.plan_id
                    && existing.content_hash == content_hash
                    && existing.completeness == completeness
        )
    }) {
        return Ok(());
    }
    let candidate = candidate_record_for_session(
        parent,
        request,
        &safe_content,
        source_event_id,
        completeness,
        recorded_at_ms,
    )?;
    handler
        .commit_controls(
            parent,
            vec![ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
                candidate,
            ))],
        )
        .map(|_| ())
}

/// Typed plan rejection command shared by TUI, HTTP, and Desktop surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct RejectPlanRequest {
    pub plan_id: String,
    pub expected_plan_hash: String,
}

impl PlanReviewCoordinator {
    /// Commits an ordinary attempt `Started` record through the parent event handler.
    ///
    /// The record is owned by the run executor, not by the prepare step: a persisted `Started`
    /// without an in-process run would be misread as a crashed run by recovery (which closes it
    /// as `Interrupted`) when the executor reloads the session across a process boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt already carries a different status or the transition is
    /// invalid.
    pub fn ensure_attempt_started<H>(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        handler: &mut H,
        now_ms: u64,
    ) -> Result<()>
    where
        H: EventHandler + ?Sized,
    {
        if request.revision_request_id.is_some() {
            bail!("revision started requires its dedicated execution owner");
        }
        if let Some(entry) = plan_review_started_attempt_entry(session, request, now_ms)? {
            handler.commit_controls(session, vec![ControlEntry::PlanReviewAttempt(entry)])?;
        }
        Ok(())
    }

    /// Ensures a revision attempt has its executor-owned `Started` record.
    ///
    /// Revision Waiting and terminal publication remain governed by their dedicated kernel
    /// bundles, so this path must not use the ordinary application public-control hook.
    pub fn ensure_revision_attempt_started(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        now_ms: u64,
    ) -> Result<()> {
        if request.revision_request_id.is_none() {
            bail!("non-revision started must use the application control commit hook");
        }
        if let Some(entry) = plan_review_started_attempt_entry(session, request, now_ms)? {
            session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
        }
        Ok(())
    }
}

fn plan_review_started_attempt_entry(
    session: &Session,
    request: &PlanReviewRunRequest,
    now_ms: u64,
) -> Result<Option<PlanReviewAttemptEntry>> {
    validate_plan_review_request_lineage(request)?;
    validate_plan_review_request_objective(session, request)?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    if let Some(existing) = projection.latest_attempt(&request.plan_review_id) {
        if existing.attempt_id == request.attempt_id {
            validate_revision_attempt_request_binding(existing, request)?;
        }
        if existing.attempt_id == request.attempt_id
            && existing.status == PlanReviewAttemptStatus::Started
        {
            return Ok(None);
        }
        if existing.attempt_id == request.attempt_id
            && existing.status != PlanReviewAttemptStatus::WaitingForInput
        {
            bail!(
                "plan review attempt {} already has status {}",
                request.attempt_id.as_str(),
                existing.status.as_str()
            );
        }
    }
    let entry = PlanReviewAttemptEntry {
        plan_review_id: request.plan_review_id.clone(),
        attempt_id: request.attempt_id.clone(),
        plan_id: request.plan_id.clone(),
        source: request.source,
        source_turn: request.source_turn.clone(),
        route_decision_id: request.route_decision_id.clone(),
        child_session_ref: request.child_session_ref.clone(),
        finalizer_session_ref: Some(request.finalizer_session_ref.clone()),
        revision_request_id: request.revision_request_id.clone(),
        attempt_ordinal: request.attempt_ordinal,
        base_plan_id: request.base_plan_id.clone(),
        base_plan_hash: request.base_plan_hash.clone(),
        explicit_objective: request.explicit_objective.clone(),
        workspace_snapshot_id: request.workspace_snapshot_id.clone(),
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: now_ms,
    };
    projection.validate_append(&entry)?;
    Ok(Some(entry))
}

fn append_attempt_status<H>(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    handler: &mut H,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    now_ms: u64,
) -> Result<()>
where
    H: EventHandler + ?Sized,
{
    if let Some(entry) =
        plan_review_attempt_status_entry(session, request, status, terminal_reason, now_ms)?
    {
        let projection = PlanReviewProjection::from_entries(session.entries());
        projection.validate_append(&entry)?;
        handler.commit_controls(session, vec![ControlEntry::PlanReviewAttempt(entry)])?;
    }
    Ok(())
}

fn append_revision_attempt_status(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    now_ms: u64,
) -> Result<()> {
    if request.revision_request_id.is_none() {
        bail!("ordinary plan-review status must use the application control commit hook");
    }
    if let Some(entry) =
        plan_review_attempt_status_entry(session, request, status, terminal_reason, now_ms)?
    {
        let projection = PlanReviewProjection::from_entries(session.entries());
        projection.validate_append(&entry)?;
        session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
    }
    Ok(())
}

/// Records the parent cancellation fact owned by a user-input command, which resumes without an
/// active application run or public bridge. This is deliberately not an executor transition.
fn append_command_owned_plan_review_cancellation(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    now_ms: u64,
) -> Result<()> {
    if request.revision_request_id.is_some() {
        bail!("revision cancellation requires the atomic revision terminal bundle");
    }
    if let Some(entry) = plan_review_attempt_status_entry(
        session,
        request,
        PlanReviewAttemptStatus::Cancelled,
        Some(PlanReviewTerminalReason::UserCancelled),
        now_ms,
    )? {
        let projection = PlanReviewProjection::from_entries(session.entries());
        projection.validate_append(&entry)?;
        session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
    }
    Ok(())
}

fn append_attempt_status_with_pending_input<H>(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    handler: &mut H,
    status: PlanReviewAttemptStatus,
    pending_user_input: Option<sigil_kernel::PublicUserInputRequestV1>,
    now_ms: u64,
) -> Result<()>
where
    H: EventHandler + ?Sized,
{
    let mut entry = plan_review_attempt_status_entry(session, request, status, None, now_ms)?
        .context("plan review pending-input transition was already recorded")?;
    entry.pending_user_input = pending_user_input.map(Box::new);
    let projection = PlanReviewProjection::from_entries(session.entries());
    projection.validate_append(&entry)?;
    handler
        .commit_controls(session, vec![ControlEntry::PlanReviewAttempt(entry)])
        .map(|_| ())
}

/// A revision is identified by its accepted guidance request and exact base-plan binding.
/// Ordinary plan reviews must never carry those fields: allowing that malformed hybrid through
/// would revive the removed split revision settlement path.
fn validate_plan_review_request_lineage(request: &PlanReviewRunRequest) -> Result<()> {
    match (
        request.revision_request_id.is_some(),
        request.base_plan_id.is_some(),
        request.base_plan_hash.is_some(),
    ) {
        (true, true, true) | (false, false, false) => {}
        (true, _, _) => bail!("revision plan review request is missing its base-plan binding"),
        (false, _, _) => bail!("non-revision plan review request carries a revision base binding"),
    }
    match (request.source, request.explicit_objective.as_deref()) {
        (PlanReviewSource::ExplicitPlanCommand, Some(objective))
            if !objective.trim().is_empty() =>
        {
            Ok(())
        }
        (PlanReviewSource::ExplicitPlanCommand, _) => Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::MissingSource,
        )
        .context("explicit plan review request lost its durable objective")),
        (PlanReviewSource::AutomaticConversationRoute, None) => Ok(()),
        (PlanReviewSource::AutomaticConversationRoute, Some(_)) => Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::ConflictingSource,
        )
        .context("automatic plan review request carries an explicit objective")),
    }
}

/// Verifies that the transient execution objective is exactly derived from the immutable
/// durable source binding. It is deliberately checked at every state transition entry point so
/// a caller cannot retain the same `explicit_objective` while substituting a different combined
/// revision objective.
fn validate_plan_review_request_objective(
    session: &Session,
    request: &PlanReviewRunRequest,
) -> Result<()> {
    if request.source_turn.session_scope_id != session.session_scope_id() {
        return Err(sigil_kernel::SessionContextPrefixError::ConflictingBoundary.into());
    }
    let original = plan_review_original_objective_from_request(session, request)?;
    let expected = match request.revision_request_id.as_ref() {
        None => original,
        Some(_) => format!(
            "{original}\n\nUser revision guidance:\n{}",
            accepted_revision_guidance_for_request(session, request)?
        ),
    };
    if request.objective != expected {
        return Err(
            anyhow::Error::new(sigil_kernel::SessionContextPrefixError::ConflictingSource)
                .context("plan review request objective conflicts with its durable source binding"),
        );
    }
    Ok(())
}

fn revision_failure_terminal_entries(
    parent: &Session,
    request: &PlanReviewRunRequest,
    base_plan_id: &PlanId,
    base_plan_hash: &str,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    decision_reason: &str,
    now_ms: u64,
) -> Result<(
    PlanReviewAttemptEntry,
    Option<PlanDraftCreatedEntry>,
    PlanDecisionRecordedEntry,
)> {
    let attempt =
        revision_terminal_attempt_entry(parent, request, status, terminal_reason, now_ms)?;
    Ok((
        attempt,
        None,
        PlanDecisionRecordedEntry {
            plan_id: base_plan_id.clone(),
            plan_hash: base_plan_hash.to_owned(),
            decision: PlanDecision::RevisionFailed,
            decided_by: PlanDecisionActor::System,
            decided_at_ms: now_ms,
            reason: Some(decision_reason.to_owned()),
        },
    ))
}

fn revision_terminal_attempt_entry(
    parent: &Session,
    request: &PlanReviewRunRequest,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    now_ms: u64,
) -> Result<PlanReviewAttemptEntry> {
    if let Some(entry) =
        plan_review_attempt_status_entry(parent, request, status, terminal_reason, now_ms)?
    {
        return Ok(entry);
    }
    PlanReviewProjection::from_entries(parent.entries())
        .latest_attempt(&request.plan_review_id)
        .filter(|entry| entry.attempt_id == request.attempt_id && entry.status == status)
        .cloned()
        .context("plan review terminal retry lost its matching durable attempt")
}

fn validate_revision_attempt_request_binding(
    attempt: &PlanReviewAttemptEntry,
    request: &PlanReviewRunRequest,
) -> Result<()> {
    if attempt.plan_review_id != request.plan_review_id
        || attempt.attempt_id != request.attempt_id
        || attempt.plan_id != request.plan_id
        || attempt.source != request.source
        || attempt.source_turn != request.source_turn
        || attempt.route_decision_id != request.route_decision_id
        || attempt.child_session_ref != request.child_session_ref
        || attempt.finalizer_session_ref.as_ref() != Some(&request.finalizer_session_ref)
        || attempt.revision_request_id != request.revision_request_id
        || attempt.attempt_ordinal != request.attempt_ordinal
        || attempt.base_plan_id != request.base_plan_id
        || attempt.base_plan_hash != request.base_plan_hash
        || attempt.explicit_objective != request.explicit_objective
        || attempt.workspace_snapshot_id != request.workspace_snapshot_id
    {
        return Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::ConflictingBoundary,
        )
        .context("durable plan-review attempt does not match the requested revision lineage"));
    }
    Ok(())
}

fn plan_review_attempt_status_entry(
    session: &Session,
    request: &PlanReviewRunRequest,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    now_ms: u64,
) -> Result<Option<PlanReviewAttemptEntry>> {
    let projection = PlanReviewProjection::from_entries(session.entries());
    if let Some(existing) = projection.latest_attempt(&request.plan_review_id) {
        if existing.attempt_id == request.attempt_id && existing.status == status {
            return Ok(None);
        }
        if existing.attempt_id != request.attempt_id {
            bail!(
                "plan review {} has a different active attempt {}",
                request.plan_review_id.as_str(),
                existing.attempt_id.as_str()
            );
        }
    }
    let entry = PlanReviewAttemptEntry {
        plan_review_id: request.plan_review_id.clone(),
        attempt_id: request.attempt_id.clone(),
        plan_id: request.plan_id.clone(),
        source: request.source,
        source_turn: request.source_turn.clone(),
        route_decision_id: request.route_decision_id.clone(),
        child_session_ref: request.child_session_ref.clone(),
        finalizer_session_ref: Some(request.finalizer_session_ref.clone()),
        revision_request_id: request.revision_request_id.clone(),
        attempt_ordinal: request.attempt_ordinal,
        base_plan_id: request.base_plan_id.clone(),
        base_plan_hash: request.base_plan_hash.clone(),
        explicit_objective: request.explicit_objective.clone(),
        workspace_snapshot_id: request.workspace_snapshot_id.clone(),
        pending_user_input: None,
        status,
        terminal_reason,
        recorded_at_ms: now_ms,
    };
    Ok(Some(entry))
}

/// Reconstructs the first committed attempt prefix before any model-context projection.
fn plan_review_parent_context(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
) -> Result<Vec<ModelMessage>> {
    let first_started = parent_session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.attempt_id == request.attempt_id
                    && attempt.status == PlanReviewAttemptStatus::Started =>
            {
                Some(attempt)
            }
            _ => None,
        })
        .ok_or(sigil_kernel::SessionContextPrefixError::MissingBoundary)?;
    validate_revision_attempt_request_binding(first_started, request)?;
    let source_id = (request.source == PlanReviewSource::AutomaticConversationRoute)
        .then_some(request.source_turn.message_id.as_str());
    Ok(parent_session
        .context_projection_before_control(
            &ControlEntry::PlanReviewAttempt(first_started.clone()),
            source_id,
        )?
        .model_messages())
}

fn plan_review_run_input(
    request: &PlanReviewRunRequest,
    draft_context: &sigil_kernel::PlanReviewDraftContext,
    cancellation: &sigil_kernel::RunCancellationHandle,
    parent_context: &[ModelMessage],
    finalizer_evidence: Option<&str>,
    candidate_text: Option<&str>,
    validation_feedback: Option<&str>,
    finalizer_ordinal: u32,
) -> AgentRunInput {
    let mut transient = vec![ModelMessage::system(
        plan_review_system_prompt_contract_material(),
    )];
    let mut initial_context = Vec::new();
    if !parent_context.is_empty() {
        initial_context.push(ModelMessage::system(
            "Frozen parent conversation context follows; preserve its roles and ordering when forming the Plan.",
        ));
        initial_context.extend(parent_context.iter().cloned());
    }
    // Keep the source objective in the fixed prefix. Subsequent provider turns append retained
    // child history after this boundary instead of re-inserting the objective at the tail.
    initial_context.push(ModelMessage::user(request.objective.clone()));
    if let Some(evidence) = finalizer_evidence {
        transient.push(ModelMessage::system(
            plan_review_no_draft_retry_contract_material(),
        ));
        initial_context.push(ModelMessage::user(format!(
            "Bounded host evidence bundle (do not perform more research):\n{evidence}"
        )));
    }
    if let Some(candidate) = candidate_text {
        initial_context.push(ModelMessage::user(format!(
            "Complete Plan candidate from the read-only research phase (preserve this text exactly while classifying or repairing it):\n{candidate}"
        )));
    }
    if let Some(feedback) = validation_feedback {
        initial_context.push(ModelMessage::user(format!(
            "Previous submit-only attempt was rejected by host validation. Correct this exact issue and submit one valid typed result; do not use research tools:\n{feedback}"
        )));
    }
    let input = AgentRunInput::without_persisted_user_message(transient)
        .with_initial_context(initial_context)
        .with_logical_run_id(if finalizer_evidence.is_some() {
            format!(
                "{}-finalizer-{finalizer_ordinal}",
                request.child_logical_run_id()
            )
        } else {
            request.child_logical_run_id()
        })
        .with_child_cancellation(cancellation.clone())
        .with_run_purpose(AgentRunPurpose::PlanReview(
            sigil_kernel::PlanReviewPurposeContext {
                plan_review_id: request.plan_review_id.clone(),
                attempt_id: request.attempt_id.clone(),
                plan_id: request.plan_id.clone(),
                source_turn: request.source_turn.clone(),
                route_decision_id: request.route_decision_id.clone(),
            },
        ))
        .with_plan_review_draft({
            let mut context = draft_context.clone();
            context.candidate_content = candidate_text.map(ToOwned::to_owned);
            context
        });
    if finalizer_evidence.is_some() {
        input.with_plan_review_submit_only()
    } else {
        input.with_soft_checkpoint(
            8,
            "Reassess the evidence gathered so far. Decide whether it supports a typed Plan result or a clarification request; continue using the advertised read-only tools when more evidence is needed.",
        )
    }
}

fn plan_review_continuation_input(
    request: &PlanReviewRunRequest,
    draft_context: &sigil_kernel::PlanReviewDraftContext,
    cancellation: &sigil_kernel::RunCancellationHandle,
    continuation: &sigil_kernel::UserInputContinuationStartedV1,
    parent_context: &[ModelMessage],
) -> AgentRunInput {
    plan_review_run_input(
        request,
        draft_context,
        cancellation,
        parent_context,
        None,
        None,
        None,
        0,
    )
    .with_logical_run_id(continuation.continuation_logical_run_id.as_str())
    .with_user_input_continuation_context(
        continuation.identity.root_logical_run_id.as_str(),
        continuation.identity.source_thread_id.clone(),
    )
    .with_initial_provider_physical_attempt_id(continuation.physical_attempt_id.clone())
}

fn plan_review_draft_ready_outcome(
    child_session: &Session,
    plan_id: &PlanId,
) -> Result<PlanReviewRunOutcome> {
    let draft = child_session
        .plan_artifact_projection()
        .plans
        .get(plan_id)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "plan review draft {} is missing from its child session",
                plan_id.as_str()
            )
        })?;
    Ok(PlanReviewRunOutcome::DraftReady {
        draft: Box::new(draft),
    })
}

fn complete_plan_review_run(
    cancellation: &sigil_kernel::RunCancellationHandle,
    outcome: PlanReviewRunOutcome,
) -> Result<PlanReviewRunOutcome> {
    if !cancellation.is_naturally_finalized() && !cancellation.try_finalize_naturally() {
        if cancellation.is_cancel_requested() {
            return Ok(PlanReviewRunOutcome::Cancelled);
        }
        bail!("run cancellation won before plan review completion");
    }
    Ok(outcome)
}

fn plan_review_provider_terminal_allows_submit_only_recovery(
    child_session: &Session,
    logical_run_id: &str,
) -> Result<bool> {
    let projection = child_session.provider_physical_attempt_projection()?;
    let outcome = projection
        .attempts_for_logical_run_id(logical_run_id)
        .last()
        .and_then(|attempt| attempt.terminal.as_ref())
        .map(|terminal| terminal.outcome);
    Ok(matches!(
        outcome,
        Some(ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput)
    ))
}

fn plan_review_research_input_target(
    parent: &Session,
    command: &sigil_kernel::UserInputDecisionCommandV1,
) -> Result<(PlanReviewRunRequest, bool)> {
    let projection = PlanReviewProjection::from_entries(parent.entries());
    if projection.has_conflicts() {
        bail!("plan review projection contains conflicts");
    }
    let attempt = projection
        .attempt_for_pending_user_input(&command.identity, &command.request_hash)
        .cloned()
        .context("plan-review input decision does not bind a suspended attempt")?;
    let current_waiting_attempt = projection
        .latest_attempt(&attempt.plan_review_id)
        .filter(|current| {
            current.attempt_id == attempt.attempt_id
                && current.status == PlanReviewAttemptStatus::WaitingForInput
        })
        .is_some();
    let request = plan_review_request_from_attempt(parent, &attempt)?;
    Ok((request, current_waiting_attempt))
}

fn recover_plan_review_research_decision_from_child_state(
    state: &sigil_kernel::UserInputRequestStateV1,
) -> Result<Option<sigil_kernel::UserInputDecisionCommandV1>> {
    let Some(accepted) = state.decision.as_ref() else {
        if state.status == sigil_kernel::UserInputStatusV1::Requested {
            return Ok(None);
        }
        bail!("managed plan-review research input state is missing its accepted decision");
    };
    let decision = match (&state.status, &accepted.decision) {
        (
            sigil_kernel::UserInputStatusV1::DecisionAccepted
            | sigil_kernel::UserInputStatusV1::ContinuationClaimed
            | sigil_kernel::UserInputStatusV1::ContinuationStarted,
            sigil_kernel::UserInputDurableDecisionV1::Submitted {
                answers: Some(answers),
                ..
            },
        ) => sigil_kernel::UserInputDecisionV1::Submitted {
            answers: answers.clone(),
        },
        (
            sigil_kernel::UserInputStatusV1::Resolved,
            sigil_kernel::UserInputDurableDecisionV1::Declined,
        ) => sigil_kernel::UserInputDecisionV1::Declined,
        (
            sigil_kernel::UserInputStatusV1::Resolved,
            sigil_kernel::UserInputDurableDecisionV1::RunCancelled,
        ) => sigil_kernel::UserInputDecisionV1::RunCancelled,
        (
            sigil_kernel::UserInputStatusV1::DecisionAccepted
            | sigil_kernel::UserInputStatusV1::ContinuationClaimed
            | sigil_kernel::UserInputStatusV1::ContinuationStarted,
            sigil_kernel::UserInputDurableDecisionV1::Submitted { answers: None, .. },
        ) => bail!("accepted plan-review research answer omitted durable recovery values"),
        _ => bail!("managed plan-review research receipt is not recoverable for its child state"),
    };
    Ok(Some(sigil_kernel::UserInputDecisionCommandV1 {
        identity: accepted.identity.clone(),
        request_hash: accepted.request_hash.clone(),
        command_id: accepted.command_id.clone(),
        decision,
    }))
}

fn build_plan_review_child_session(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
    resource_bundle: Option<&CurrentSchemaPlanReviewChildResourceBundleV1>,
) -> Result<Session> {
    if let Some(bundle) = resource_bundle {
        let store = sigil_kernel::JsonlSessionStore::new(bundle.session_log_path())?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        session.attach_tool_artifact_store_override(bundle.artifact_store());
        attach_session_url_capability_store(&mut session)?;
        crate::session_composition::inherit_session_composition(parent_session, &mut session)?;
        return Ok(session);
    }
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store =
            sigil_kernel::JsonlSessionStore::new(request.child_session_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        attach_session_url_capability_store(&mut session)?;
        crate::session_composition::inherit_session_composition(parent_session, &mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    attach_session_url_capability_store(&mut session)?;
    crate::session_composition::inherit_session_composition(parent_session, &mut session)?;
    Ok(session)
}

fn plan_review_request_from_attempt(
    parent_session: &Session,
    attempt: &PlanReviewAttemptEntry,
) -> Result<PlanReviewRunRequest> {
    let mut objective = plan_review_original_objective_from_attempt(parent_session, attempt)?;
    if attempt.revision_request_id.is_some() {
        let guidance = accepted_revision_guidance_for_attempt(parent_session, attempt)?;
        objective.push_str("\n\nUser revision guidance:\n");
        objective.push_str(&guidance);
    }
    Ok(PlanReviewRunRequest {
        plan_review_id: attempt.plan_review_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        plan_id: attempt.plan_id.clone(),
        source: attempt.source,
        source_turn: attempt.source_turn.clone(),
        route_decision_id: attempt.route_decision_id.clone(),
        child_session_ref: attempt.child_session_ref.clone(),
        finalizer_session_ref: attempt.finalizer_session_ref.clone().context(
            "unsupported Plan attempt format: executable attempt has no finalizer binding",
        )?,
        revision_request_id: attempt.revision_request_id.clone(),
        attempt_ordinal: attempt.attempt_ordinal,
        base_plan_id: attempt.base_plan_id.clone(),
        base_plan_hash: attempt.base_plan_hash.clone(),
        explicit_objective: attempt.explicit_objective.clone(),
        objective,
        workspace_snapshot_id: attempt.workspace_snapshot_id.clone(),
    })
}

/// Computes the retry binding digest for one exact durable attempt and objective.
///
/// This is intentionally derived from stable identity and source fields rather than transient
/// provider state. Callers must capture it together with the durable frontier immediately before
/// issuing a retry command.
pub fn plan_review_context_digest_for_attempt(
    session: &Session,
    attempt: &PlanReviewAttemptEntry,
) -> Result<String> {
    let request = plan_review_request_from_attempt(session, attempt)?;
    plan_review_context_digest(session, attempt, &request.objective)
}

fn plan_review_context_digest(
    session: &Session,
    attempt: &PlanReviewAttemptEntry,
    objective: &str,
) -> Result<String> {
    let payload = serde_json::json!({
        "session_scope_id": session.session_scope_id(),
        "plan_review_id": attempt.plan_review_id,
        "attempt_id": attempt.attempt_id,
        "plan_id": attempt.plan_id,
        "source": attempt.source,
        "source_turn": attempt.source_turn,
        "route_decision_id": attempt.route_decision_id,
        "revision_request_id": attempt.revision_request_id,
        "attempt_ordinal": attempt.attempt_ordinal,
        "base_plan_id": attempt.base_plan_id,
        "base_plan_hash": attempt.base_plan_hash,
        "explicit_objective": attempt.explicit_objective,
        "objective": objective,
        "workspace_snapshot_id": attempt.workspace_snapshot_id,
    });
    Ok(sigil_kernel::stable_event_hash(&serde_json::to_vec(
        &payload,
    )?))
}

/// Reads the immutable initial objective from the one durable source that owns it. Explicit
/// `/plan` attempts have no provider-visible user message, while automatic reviews must never
/// persist a duplicate copy of their user turn. Missing or mixed-source records are corrupt and
/// remain unrecoverable rather than receiving a guessed objective.
fn plan_review_original_objective_from_attempt(
    parent_session: &Session,
    attempt: &PlanReviewAttemptEntry,
) -> Result<String> {
    match (attempt.source, attempt.explicit_objective.as_deref()) {
        (PlanReviewSource::ExplicitPlanCommand, Some(objective))
            if !objective.trim().is_empty() =>
        {
            Ok(objective.to_owned())
        }
        (PlanReviewSource::ExplicitPlanCommand, _) => Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::MissingSource,
        )
        .context("explicit plan review attempt lost its durable source objective")),
        (PlanReviewSource::AutomaticConversationRoute, None) => parent_session
            .source_user_message(&attempt.source_turn.message_id)
            .map(|message| message.content.clone().unwrap_or_default())
            .filter(|objective| !objective.trim().is_empty())
            .ok_or_else(|| {
                anyhow::Error::new(sigil_kernel::SessionContextPrefixError::MissingSource)
            })
            .context("plan review attempt lost its durable source objective"),
        (PlanReviewSource::AutomaticConversationRoute, Some(_)) => Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::ConflictingSource,
        )
        .context("automatic plan review attempt carries an explicit objective")),
    }
}

fn plan_review_original_objective_from_request(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
) -> Result<String> {
    match (request.source, request.explicit_objective.as_deref()) {
        (PlanReviewSource::ExplicitPlanCommand, Some(objective))
            if !objective.trim().is_empty() =>
        {
            Ok(objective.to_owned())
        }
        (PlanReviewSource::ExplicitPlanCommand, _) => Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::MissingSource,
        )
        .context("explicit plan review request lost its durable source objective")),
        (PlanReviewSource::AutomaticConversationRoute, None) => parent_session
            .source_user_message(&request.source_turn.message_id)
            .map(|message| message.content.clone().unwrap_or_default())
            .filter(|objective| !objective.trim().is_empty())
            .ok_or_else(|| {
                anyhow::Error::new(sigil_kernel::SessionContextPrefixError::MissingSource)
            })
            .context("plan review request lost its durable source objective"),
        (PlanReviewSource::AutomaticConversationRoute, Some(_)) => Err(anyhow::Error::new(
            sigil_kernel::SessionContextPrefixError::ConflictingSource,
        )
        .context("automatic plan review request carries an explicit objective")),
    }
}

/// Reconstructs revision guidance from the accepted user-input fact that actually authorized
/// this attempt. The mutable base-plan decision may already be `RevisionFailed` after the first
/// execution, so it is not a stable source for an idempotent historical command replay.
fn accepted_revision_guidance_for_attempt(
    parent_session: &Session,
    attempt: &PlanReviewAttemptEntry,
) -> Result<String> {
    let request_id = attempt
        .revision_request_id
        .as_ref()
        .context("revision attempt is missing its guidance request identity")?;
    let base_plan_id = attempt
        .base_plan_id
        .as_ref()
        .context("revision attempt lost its base plan identity")?;
    let base_plan_hash = attempt
        .base_plan_hash
        .as_deref()
        .context("revision attempt lost its base plan hash")?;
    accepted_revision_guidance_for_lineage(
        parent_session,
        &attempt.plan_review_id,
        request_id,
        base_plan_id,
        base_plan_hash,
    )
}

fn accepted_revision_guidance_for_request(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
) -> Result<String> {
    let request_id = request
        .revision_request_id
        .as_ref()
        .context("revision plan review request is missing its guidance request identity")?;
    let base_plan_id = request
        .base_plan_id
        .as_ref()
        .context("revision plan review request lost its base plan identity")?;
    let base_plan_hash = request
        .base_plan_hash
        .as_deref()
        .context("revision plan review request lost its base plan hash")?;
    accepted_revision_guidance_for_lineage(
        parent_session,
        &request.plan_review_id,
        request_id,
        base_plan_id,
        base_plan_hash,
    )
}

fn accepted_revision_guidance_for_lineage(
    parent_session: &Session,
    plan_review_id: &PlanReviewId,
    request_id: &sigil_kernel::UserInputRequestId,
    base_plan_id: &PlanId,
    base_plan_hash: &str,
) -> Result<String> {
    let expected_root_logical_run_id = sigil_kernel::LogicalRunId::new(stable_event_uuid(
        "sigil-plan-revision-root-run-v1",
        plan_review_id.as_str(),
    ))?;
    let requested = parent_session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::UserInputRequested(requested))
                if requested.request.identity.request_id == *request_id
                    && requested.request.identity.session_scope_id.as_str()
                        == parent_session.session_scope_id()
                    && requested.request.identity.root_logical_run_id
                        == expected_root_logical_run_id
                    && matches!(
                        &requested.request.source,
                        sigil_kernel::UserInputSourceV1::PlanRevision {
                            base_plan_id: candidate_id,
                            base_plan_hash: candidate_hash,
                        } if candidate_id == base_plan_id && candidate_hash == base_plan_hash
                    ) =>
            {
                Some((**requested).clone())
            }
            _ => None,
        })
        .context("revision attempt lost its durable guidance request")?;
    let accepted = parent_session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(accepted))
                if accepted.identity == requested.request.identity
                    && accepted.request_hash == requested.request_hash =>
            {
                Some((**accepted).clone())
            }
            _ => None,
        })
        .context("revision attempt lost its accepted durable guidance")?;
    match accepted.decision {
        sigil_kernel::UserInputDurableDecisionV1::Submitted {
            answers: Some(answers),
            ..
        } => answers
            .into_iter()
            .find_map(|answer| {
                (answer.question_id == "revision_guidance")
                    .then_some(answer.value)
                    .and_then(|value| match value {
                        sigil_kernel::UserInputAnswerValueV1::Text { value } => Some(value),
                        _ => None,
                    })
            })
            .context("revision attempt accepted guidance is missing its text value"),
        _ => bail!("revision attempt guidance was not accepted as submitted text"),
    }
}

fn build_plan_review_finalizer_session(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
    corrective_ordinal: u32,
    resource_bundle: Option<&CurrentSchemaPlanReviewChildResourceBundleV1>,
) -> Result<Session> {
    if let Some(bundle) = resource_bundle {
        let store = sigil_kernel::JsonlSessionStore::new(bundle.session_log_path())?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        session.attach_tool_artifact_store_override(bundle.artifact_store());
        attach_session_url_capability_store(&mut session)?;
        crate::session_composition::inherit_session_composition(parent_session, &mut session)?;
        return Ok(session);
    }
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let child_ref = if corrective_ordinal == 1 {
            request.finalizer_session_ref.clone()
        } else {
            plan_review_finalizer_session_ref(
                &request.plan_review_id,
                &request.attempt_id,
                corrective_ordinal,
            )
        };
        let store = sigil_kernel::JsonlSessionStore::new(child_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        attach_session_url_capability_store(&mut session)?;
        crate::session_composition::inherit_session_composition(parent_session, &mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    attach_session_url_capability_store(&mut session)?;
    crate::session_composition::inherit_session_composition(parent_session, &mut session)?;
    Ok(session)
}

fn plan_review_finalizer_evidence_bundle(
    request: &PlanReviewRunRequest,
    research_session: &Session,
) -> String {
    const MAX_EVIDENCE_BYTES: usize = 24 * 1024;
    const MAX_RESULTS: usize = 12;
    let mut lines = vec![
        format!("plan_review_id: {}", request.plan_review_id.as_str()),
        format!("attempt_id: {}", request.attempt_id.as_str()),
        format!("workspace_snapshot: {:?}", request.workspace_snapshot_id),
    ];
    if let (Some(base_id), Some(base_hash)) = (
        request.base_plan_id.as_ref(),
        request.base_plan_hash.as_ref(),
    ) {
        lines.push(format!("base_plan: {} @ {}", base_id.as_str(), base_hash));
    }
    let results = research_session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) => Some(result),
            _ => None,
        })
        .rev()
        .take(MAX_RESULTS)
        .collect::<Vec<_>>();
    for result in results.into_iter().rev() {
        let artifact = result
            .initial_model_view
            .artifact_ref
            .as_ref()
            .map(|reference| format!(" artifact={}", reference.artifact_id))
            .unwrap_or_default();
        lines.push(format!(
            "tool {} call={} hash={}{}\n{}",
            result.tool_name,
            result.call_id,
            result.artifact_hash,
            artifact,
            result.initial_model_view.preview
        ));
    }
    let mut evidence = lines.join("\n\n");
    if evidence.len() > MAX_EVIDENCE_BYTES {
        let mut end = MAX_EVIDENCE_BYTES.saturating_sub("\n...[evidence truncated]".len());
        while !evidence.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        evidence.truncate(end);
        evidence.push_str("\n...[evidence truncated]");
    }
    evidence
}

fn validate_plan_review_child_draft(
    draft: &PlanDraftCreatedEntry,
    request: &PlanReviewRunRequest,
) -> Result<()> {
    if draft.plan_id != request.plan_id
        || draft.source.source_turn.as_ref() != Some(&request.source_turn)
        || draft.source.route_decision_id != request.route_decision_id
        || draft.source.plan_review_id.as_ref() != Some(&request.plan_review_id)
        || draft.workspace_snapshot_id != request.workspace_snapshot_id
    {
        bail!(
            "plan review child draft {} does not match its bound attempt lineage \
             (plan_id_match={}, source_turn_match={}, route_decision_match={}, \
             plan_review_id_match={}, workspace_snapshot_match={})",
            draft.plan_id.as_str(),
            draft.plan_id == request.plan_id,
            draft.source.source_turn.as_ref() == Some(&request.source_turn),
            draft.source.route_decision_id == request.route_decision_id,
            draft.source.plan_review_id.as_ref() == Some(&request.plan_review_id),
            draft.workspace_snapshot_id == request.workspace_snapshot_id,
        );
    }
    Ok(())
}

fn ensure_plan_action_allowed(
    session: &Session,
    plan_id: &PlanId,
    expected_plan_hash: &str,
    action: sigil_kernel::PublicPlanAction,
) -> Result<()> {
    let review =
        crate::conversation_display::public_plan_review_from_entries(session.entries(), None)?
            .context("plan action has no canonical review projection")?;
    if review.plan_id != plan_id.as_str()
        || review.plan_hash.as_deref() != Some(expected_plan_hash)
        || review.status != sigil_kernel::PublicPlanReviewStatus::DraftReady
        || !review.allowed_actions.contains(&action)
    {
        bail!(
            "plan {} action {} is unavailable in the current review state \
             (projected_plan={}, projected_hash={}, status={:?}, allowed_actions={:?})",
            plan_id.as_str(),
            match action {
                sigil_kernel::PublicPlanAction::Run => "run",
                sigil_kernel::PublicPlanAction::Save => "save",
                sigil_kernel::PublicPlanAction::Revise => "revise",
                sigil_kernel::PublicPlanAction::Reject => "reject",
                sigil_kernel::PublicPlanAction::AdoptCandidate => "adopt_candidate",
                sigil_kernel::PublicPlanAction::RetryReview => "retry_review",
            },
            review.plan_id,
            review.plan_hash.as_deref().unwrap_or("none"),
            review.status,
            review.allowed_actions,
        );
    }
    Ok(())
}

/// Builds the current workspace snapshot id bound to plan handoff artifacts.
pub fn plan_handoff_workspace_snapshot_id(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Result<Option<String>> {
    let workspace_id = stable_workspace_id(workspace_root)?;
    let scope = root_config
        .verification
        .scope_for_hash(sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(workspace_root, workspace_id, &scope, 0)?;
    Ok(snapshot.workspace_snapshot_id)
}

pub fn plan_handoff_stale_reason(
    base_workspace_snapshot_id: Option<&str>,
    current_workspace_snapshot_id: Option<&str>,
) -> Option<String> {
    match (base_workspace_snapshot_id, current_workspace_snapshot_id) {
        (Some(base), Some(current)) => (base != current).then(|| {
            format!(
                "plan may be stale: workspace changed since plan was created (base={}, current={})",
                truncate_plan_snapshot_id(base),
                truncate_plan_snapshot_id(current)
            )
        }),
        (Some(base), None) => Some(format!(
            "plan may be stale: current workspace snapshot is unavailable (base={})",
            truncate_plan_snapshot_id(base)
        )),
        (None, _) => Some(
            "plan cannot be direct-promoted: its base workspace snapshot is unavailable".to_owned(),
        ),
    }
}

fn truncate_plan_snapshot_id(snapshot_id: &str) -> String {
    snapshot_id.chars().take(24).collect()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Typed plan decision command for one application surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationPlanDecisionCommand {
    pub plan_id: String,
    pub expected_plan_hash: String,
    pub action: ApplicationPlanAction,
    /// Exact preserved candidate hash required when retrying a terminal review that captured a
    /// complete plain-text candidate. Terminal attempts with no candidate or only a
    /// partial/unknown candidate may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_candidate_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_grant: Option<PlanApprovalPermission>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationPlanAction {
    Run,
    Save,
    Revise,
    Reject,
    AdoptCandidate,
    RetryReview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationPlanDecisionReceipt {
    pub plan_id: String,
    pub plan_hash: String,
    pub action: ApplicationPlanAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// RFC-0067: semantic Task title shown immediately after a Run receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    /// RFC-0067: adopted candidate hash for receipt idempotency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_hash: Option<String>,
    /// RFC-0067: durable Task phase right after admission (Preparing/Ready/Blocked/Paused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_phase: Option<sigil_kernel::TaskExecutionPhaseV1>,
    /// RFC-0067: typed blocker when admission held the Task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_blocker: Option<sigil_kernel::TaskBlockerV1>,
    /// Durable host-owned guidance request created by a first `Revise` action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input_request: Option<sigil_kernel::PublicUserInputRequestV1>,
    /// Prepared revision run the caller must execute so `Revise` does not leave a dangling
    /// `Started` attempt. `None` for every other action.
    #[serde(skip)]
    pub revision_request: Option<PlanReviewRunRequest>,
}

/// Applies one typed plan decision on an application surface session.
///
/// `Run` creates the durable RFC-0018 task prefix and returns its stable task id; execution
/// continues through the existing application Task path. `Save`, `Revise`, and `Reject` record
/// durable decisions only.
/// Application service for atomic Plan approval and direct Task execution.
///
/// TUI keyboard/mouse, Desktop IPC, HTTP commands, CLI automation and the model-selected
/// `run_pending_plan` route all construct a [`PlanRunCommandV1`] and drive this service. The
/// service never calls the provider, reads the workspace, parses prose, enumerates the tool
/// registry or starts a child process. Approval atomically creates a stable Task plus first-class
/// direct execution authority. Typed Plan steps may seed a display-only checklist, but never
/// execution authority.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanExecutionService;

/// Durable result of approving readable Plan text and creating its stable Task shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanApprovalReceiptV2 {
    pub command_id: String,
    pub receipt_id: String,
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub task_id: TaskId,
    pub task_title: String,
    pub start_mode: PlanTaskStartMode,
    pub permission_grant: Option<PlanApprovalPermission>,
    pub approved_at_ms: u64,
    pub already_approved: bool,
}

impl PlanExecutionService {
    /// Commits exact Plan approval together with a stable, directly executable Task.
    ///
    /// This is the product authority boundary. It never asks a model to compile a DAG, activates
    /// intents, probes the workspace, invokes a provider or starts a child. Retrying the same
    /// approved plan returns the same Task identity and exact direct-execution authority.
    pub fn approve(
        session: &mut Session,
        parent_session_ref: SessionRef,
        command: &sigil_kernel::PlanRunCommandV1,
        now_ms: u64,
    ) -> std::result::Result<PlanApprovalReceiptV2, sigil_kernel::PlanRunRejectionV1> {
        if command.session_id != session.session_scope_id() {
            return Err(sigil_kernel::PlanRunRejectionV1::CommandIdentityConflict);
        }
        let projection = session.plan_artifact_projection();
        let draft = projection
            .plans
            .get(&command.plan_id)
            .cloned()
            .ok_or(sigil_kernel::PlanRunRejectionV1::PlanMissing)?;
        if draft.plan_hash != command.expected_plan_hash {
            return Err(sigil_kernel::PlanRunRejectionV1::PlanHashStale {
                expected: command.expected_plan_hash.clone(),
                current: draft.plan_hash,
            });
        }
        if projection.plan_is_rejected(&command.plan_id) {
            return Err(sigil_kernel::PlanRunRejectionV1::PlanRejected);
        }
        // Current Plan approval requires the durable DraftReady review. Advisory precompile
        // markers do not grant approval authority.
        let reviewable = PlanReviewProjection::from_entries(session.entries())
            .attempt_for_plan(&command.plan_id)
            .is_some_and(|attempt| attempt.status == PlanReviewAttemptStatus::DraftReady);
        if !reviewable
            && projection
                .latest_decision(&command.plan_id)
                .is_none_or(|decision| decision.decision != PlanDecision::Accepted)
        {
            return Err(sigil_kernel::PlanRunRejectionV1::PlanNotReady {
                plan_state: projection.plan_ready_state(&command.plan_id),
            });
        }
        let permission_grant = match command.permission {
            sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy => None,
            sigil_kernel::PlanRunPermissionChoiceV1::GrantScopedEditsOnce => {
                if draft.target_paths.is_empty() {
                    return Err(
                        sigil_kernel::PlanRunRejectionV1::PermissionChoiceUnavailable {
                            reason: "the approved plan has no concrete target paths".to_owned(),
                        },
                    );
                }
                Some(PlanApprovalPermission::WorkspaceEdits)
            }
        };
        let task_id = task_id_from_plan_draft(&draft).map_err(|error| {
            sigil_kernel::PlanRunRejectionV1::SessionWriterUnavailable {
                reason: format!("failed to derive approved Task identity: {error:#}"),
            }
        })?;
        let task_title = sigil_kernel::task_semantic_title(&draft.summary);
        let objective = safe_persistence_text(&plan_task_input_from_draft(&draft));
        let grant_time_ms = projection
            .latest_decision(&command.plan_id)
            .filter(|decision| decision.decision == PlanDecision::Accepted)
            .map_or(now_ms, |decision| decision.decided_at_ms);
        let direct_execution = sigil_kernel::TaskDirectExecutionAdmittedV1::approved_plan(
            task_id.clone(),
            &objective,
            draft.plan_id.clone(),
            draft.plan_hash.clone(),
            grant_time_ms,
        );
        let checklist = sigil_kernel::task_checklist_from_plan_steps(task_id.clone(), &draft.steps);
        let permission_grant_entry =
            permission_grant.map(|permission| PlanPermissionGrantedEntry {
                plan_id: draft.plan_id.clone(),
                plan_hash: draft.plan_hash.clone(),
                task_id: task_id.clone(),
                workspace_snapshot_id: draft.workspace_snapshot_id.clone(),
                permission,
                scope: PlanApprovalScope {
                    summary: format!("scoped edits for task {}", task_id.as_str()),
                    workspace_paths: draft.target_paths.clone(),
                },
                expires: sigil_kernel::PlanApprovalExpiry::Session,
                granted_at_ms: grant_time_ms,
            });
        let existing_task = session.task_state_projection().tasks.get(&task_id).cloned();
        let accepted = projection
            .latest_decision(&command.plan_id)
            .filter(|decision| decision.decision == PlanDecision::Accepted);
        let existing_link = projection
            .tasks_created
            .get(&command.plan_id)
            .and_then(|entries| entries.first());
        if let (Some(decision), Some(task)) = (accepted, existing_task.as_ref()) {
            if decision.plan_hash == command.expected_plan_hash
                && task.parent_session_ref == parent_session_ref
                && task.objective == objective
                && existing_link
                    .is_some_and(|link| link.task_id == task_id && link.task_plan_version == 0)
                && task.direct_execution_admission.as_ref() == Some(&direct_execution)
                && task.checklist.as_ref() == checklist.as_ref()
                && projection
                    .permission_grants
                    .get(&command.plan_id)
                    .and_then(|grants| grants.iter().find(|grant| grant.task_id == task_id))
                    == permission_grant_entry.as_ref()
            {
                return Ok(Self::approval_receipt(
                    command,
                    task_id,
                    task_title,
                    permission_grant,
                    decision.decided_at_ms,
                    true,
                ));
            }
            return Err(sigil_kernel::PlanRunRejectionV1::CommandIdentityConflict);
        }
        if accepted.is_some() || existing_task.is_some() || existing_link.is_some() {
            return Err(sigil_kernel::PlanRunRejectionV1::CommandIdentityConflict);
        }
        let decision = PlanDecisionRecordedEntry {
            plan_id: command.plan_id.clone(),
            plan_hash: command.expected_plan_hash.clone(),
            decision: PlanDecision::Accepted,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms,
            reason: Some(format!(
                "approved for direct Task execution via command {}",
                command.command_id
            )),
        };
        let task_run = TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref,
            objective,
            title: Some(task_title.clone()),
            status: if command.start_mode == PlanTaskStartMode::CreatePaused {
                TaskRunStatus::Paused
            } else {
                TaskRunStatus::Started
            },
            reason: Some(format!(
                "directly executable Task created from approved plan {}",
                command.plan_id.as_str()
            )),
        };
        let task_link = TaskCreatedFromPlanEntry {
            plan_id: command.plan_id.clone(),
            plan_hash: command.expected_plan_hash.clone(),
            task_id: task_id.clone(),
            task_plan_version: 0,
            step_mapping: Vec::new(),
            stale_reason: None,
            created_at_ms: now_ms,
        };
        let commit = sigil_kernel::append_plan_approval_task_shell_at_frontier(
            session,
            &decision,
            &task_run,
            &task_link,
            &direct_execution,
            checklist.as_ref(),
            permission_grant_entry.as_ref(),
            command.expected_durable_frontier,
        )
        .map_err(
            |error| sigil_kernel::PlanRunRejectionV1::SessionWriterUnavailable {
                reason: format!("{error:#}"),
            },
        )?;
        if commit == sigil_kernel::PlanExecutionAdoptionCommit::CasSkipped {
            let current = session.plan_artifact_projection();
            let current_task = session.task_state_projection();
            if current
                .latest_decision(&command.plan_id)
                .is_some_and(|entry| {
                    entry.decision == PlanDecision::Accepted
                        && entry.plan_hash == command.expected_plan_hash
                })
                && current_task.tasks.get(&task_id).is_some_and(|task| {
                    task.parent_session_ref == task_run.parent_session_ref
                        && task.objective == task_run.objective
                })
                && current
                    .tasks_created
                    .get(&command.plan_id)
                    .and_then(|entries| entries.first())
                    .is_some_and(|link| link.task_id == task_id && link.task_plan_version == 0)
                && current_task.tasks.get(&task_id).is_some_and(|task| {
                    task.direct_execution_admission.as_ref() == Some(&direct_execution)
                        && task.checklist.as_ref() == checklist.as_ref()
                })
                && current
                    .permission_grants
                    .get(&command.plan_id)
                    .and_then(|grants| grants.iter().find(|grant| grant.task_id == task_id))
                    == permission_grant_entry.as_ref()
            {
                return Ok(Self::approval_receipt(
                    command,
                    task_id,
                    task_title,
                    permission_grant,
                    now_ms,
                    true,
                ));
            }
            return Err(sigil_kernel::PlanRunRejectionV1::FrontierStale {
                expected: command.expected_durable_frontier,
                current: session.durable_frontier_sequence(),
            });
        }
        Ok(Self::approval_receipt(
            command,
            task_id,
            task_title,
            permission_grant,
            now_ms,
            false,
        ))
    }

    /// Returns the immediate transport-facing outcome for an atomically approved direct Task.
    ///
    /// No environment probe or model-authored contract is needed at this boundary. Concrete
    /// provider, permission and tool failures remain recoverable within the normal runner.
    #[must_use]
    pub fn direct_execution_outcome(
        approval: &PlanApprovalReceiptV2,
        now_ms: u64,
    ) -> sigil_kernel::TaskAdmissionOutcomeV1 {
        if approval.start_mode == PlanTaskStartMode::CreatePaused {
            return sigil_kernel::TaskAdmissionOutcomeV1::Paused(
                sigil_kernel::TaskPauseReasonV1::CreatePaused,
            );
        }
        sigil_kernel::TaskAdmissionOutcomeV1::Ready(sigil_kernel::TaskRuntimeLeaseBindingV1 {
            lease_id: sigil_kernel::stable_event_uuid(
                "sigil-direct-task-execution-lease-v1",
                &format!(
                    "{}:{}:{}",
                    approval.task_id.as_str(),
                    approval.plan_hash,
                    approval.command_id
                ),
            ),
            granted_at_ms: now_ms,
        })
    }

    fn approval_receipt(
        command: &sigil_kernel::PlanRunCommandV1,
        task_id: TaskId,
        task_title: String,
        permission_grant: Option<PlanApprovalPermission>,
        approved_at_ms: u64,
        already_approved: bool,
    ) -> PlanApprovalReceiptV2 {
        PlanApprovalReceiptV2 {
            command_id: command.command_id.clone(),
            receipt_id: sigil_kernel::stable_event_uuid(
                "sigil-plan-approval-receipt-v2",
                &format!(
                    "{}:{}:{}",
                    command.command_id,
                    command.expected_plan_hash,
                    task_id.as_str()
                ),
            ),
            plan_id: command.plan_id.clone(),
            plan_hash: command.expected_plan_hash.clone(),
            task_id,
            task_title,
            start_mode: command.start_mode,
            permission_grant,
            approved_at_ms,
            already_approved,
        }
    }

    /// Builds historical RFC-0067 adoption fixtures for domain regression tests.
    ///
    /// Idempotency: retrying the same `command_id` returns the same receipt; adopting the same
    /// candidate with another command returns the same Task identity with `already_adopted`.
    /// Typed rejections leave the Plan actionable.
    ///
    /// # Errors
    ///
    /// Returns `Err` only for rejections that are not `PlanRunRejectionV1`-typed (for example an
    /// adoption payload that cannot be serialized).
    #[cfg(test)]
    pub fn adopt(
        session: &mut Session,
        parent_session_ref: SessionRef,
        command: &sigil_kernel::PlanRunCommandV1,
        now_ms: u64,
    ) -> std::result::Result<sigil_kernel::PlanRunReceiptV1, sigil_kernel::PlanRunRejectionV1> {
        // RFC-0067 9.1: the command must be bound to the exact durable session it is executed
        // against; adapters must not be able to adopt across session boundaries.
        if command.session_id != session.session_scope_id() {
            return Err(sigil_kernel::PlanRunRejectionV1::CommandIdentityConflict);
        }
        let projection = session.plan_artifact_projection();
        let draft = projection
            .plans
            .get(&command.plan_id)
            .ok_or(sigil_kernel::PlanRunRejectionV1::PlanMissing)?;
        if draft.plan_hash != command.expected_plan_hash {
            return Err(sigil_kernel::PlanRunRejectionV1::PlanHashStale {
                expected: command.expected_plan_hash.clone(),
                current: draft.plan_hash.clone(),
            });
        }
        let plan_state = projection.plan_ready_state(&command.plan_id);
        if plan_state != sigil_kernel::PlanReadyStateV1::Ready {
            return Err(sigil_kernel::PlanRunRejectionV1::PlanNotReady { plan_state });
        }
        if projection.plan_is_rejected(&command.plan_id) {
            return Err(sigil_kernel::PlanRunRejectionV1::PlanRejected);
        }
        let candidate = projection
            .latest_candidate(&command.plan_id)
            .cloned()
            .ok_or(sigil_kernel::PlanRunRejectionV1::CandidateMissing)?;
        if candidate.candidate_hash != command.expected_candidate_hash {
            return Err(sigil_kernel::PlanRunRejectionV1::CandidateHashMismatch {
                expected: command.expected_candidate_hash.clone(),
                current: candidate.candidate_hash.clone(),
            });
        }
        let permission_grant = match command.permission {
            sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy => None,
            sigil_kernel::PlanRunPermissionChoiceV1::GrantScopedEditsOnce => {
                if candidate.permission_scope_candidate.is_none() {
                    return Err(
                        sigil_kernel::PlanRunRejectionV1::PermissionChoiceUnavailable {
                            reason: "the plan candidate has no concrete target paths".to_owned(),
                        },
                    );
                }
                Some(PlanApprovalPermission::WorkspaceEdits)
            }
        };
        let adoption = sigil_kernel::PlanExecutionAdoptedV1Entry {
            command_id: command.command_id.clone(),
            plan_id: command.plan_id.clone(),
            plan_hash: command.expected_plan_hash.clone(),
            candidate_hash: command.expected_candidate_hash.clone(),
            task_id: candidate.task_id.clone(),
            task_title: candidate.semantic_title.clone(),
            parent_session_ref,
            start_mode: command.start_mode,
            permission_grant,
            adopted_candidate: Box::new(candidate),
            execution_segments: None,
            initial_phase: sigil_kernel::TaskExecutionPhaseV1::Preparing,
            adopted_at_ms: now_ms,
        };
        let commit = sigil_kernel::append_plan_execution_adoption_at_frontier(
            session,
            &adoption,
            command.expected_durable_frontier,
        )
        .map_err(
            |error| sigil_kernel::PlanRunRejectionV1::SessionWriterUnavailable {
                reason: format!("{error:#}"),
            },
        )?;
        let projection = session.plan_artifact_projection();
        match commit {
            sigil_kernel::PlanExecutionAdoptionCommit::Appended => {
                Ok(Self::receipt_from_adoption(adoption, now_ms, false))
            }
            sigil_kernel::PlanExecutionAdoptionCommit::CasSkipped => {
                if let Some(existing) = projection.adoption_for_command(&command.command_id) {
                    return Ok(Self::receipt_from_adoption(existing.clone(), now_ms, true));
                }
                if let Some(existing) = projection
                    .adoptions
                    .values()
                    .flatten()
                    .find(|existing| existing.candidate_hash == command.expected_candidate_hash)
                {
                    return Ok(Self::receipt_from_adoption(existing.clone(), now_ms, true));
                }
                Err(sigil_kernel::PlanRunRejectionV1::FrontierStale {
                    expected: command.expected_durable_frontier,
                    current: session.durable_frontier_sequence(),
                })
            }
        }
    }

    #[cfg(test)]
    fn receipt_from_adoption(
        adoption: sigil_kernel::PlanExecutionAdoptedV1Entry,
        now_ms: u64,
        already_adopted: bool,
    ) -> sigil_kernel::PlanRunReceiptV1 {
        let command_id = adoption.command_id.clone();
        let candidate_hash = adoption.candidate_hash.clone();
        sigil_kernel::PlanRunReceiptV1 {
            command_id: command_id.clone(),
            receipt_id: sigil_kernel::stable_event_uuid(
                "sigil-plan-run-receipt-v1",
                &format!("{command_id}:{candidate_hash}"),
            ),
            plan_id: adoption.plan_id,
            plan_hash: adoption.plan_hash,
            candidate_hash: adoption.candidate_hash,
            task_id: adoption.task_id,
            task_title: adoption.task_title,
            initial_phase: adoption.initial_phase,
            accepted_at_ms: now_ms,
            already_adopted,
        }
    }
}

/// Environment probes supplied by the surface that will actually execute the Task.
#[derive(Debug, Clone, Default)]
pub struct TaskAdmissionProbeContext {
    /// Exact tool/agent/MCP registry capability contracts; `None` when the surface cannot prove
    /// registry capabilities (the probe then reports no missing capabilities).
    pub tool_contracts: Option<Vec<sigil_kernel::ToolRuntimeContract>>,
    pub provider_route_available: bool,
    pub credential_available: bool,
    pub permission_profile_ok: bool,
    pub disk_space_bytes: Option<u64>,
    pub verification_runner_available: bool,
    pub external_writer_active: bool,
}

/// Minimum free disk bytes admission accepts before reporting `disk_space_exhausted`.
pub const TASK_ADMISSION_MIN_DISK_SPACE_BYTES: u64 = 64 * 1024 * 1024;

/// Builds honest environment probes for one Task admission attempt (RFC-0067 6.3, 10.3).
///
/// Every probe observes the current environment instead of assuming availability:
/// - the provider route comes from the configured default connection shape, and credential
///   availability resolves the exact credential source (environment variable or stored record)
///   the provider build would use;
/// - free disk space is measured on the workspace filesystem;
/// - the permission profile only blocks when the candidate actually requires workspace writes
///   and the mode is `read_only`;
/// - verification checks require both `verification.auto_run != "never"` and a registered tool
///   carrying the `verification_run` capability (when the registry is observable);
/// - an active exclusive write lease owned by another actor means an external writer holds the
///   workspace. Leases owned by this Task's own steps (task:<id>:...) are not external writers.
///   Session-local lease evidence cannot observe writers in other sessions/processes; that
///   boundary is documented here and remains a limitation of the durable evidence available.
pub fn build_task_admission_probes(
    root_config: &RootConfig,
    workspace_root: &Path,
    tool_contracts: Option<Vec<sigil_kernel::ToolRuntimeContract>>,
    session: &Session,
    task_id: &TaskId,
    candidate: &sigil_kernel::ExecutablePlanCandidateV1,
) -> TaskAdmissionProbeContext {
    let (route_available, credential_available) = route_and_credential_probe(root_config);
    let workspace_id = stable_workspace_id(workspace_root).ok();
    let task_owner_prefix = format!("task:{}:", task_id.as_str());
    let external_writer_active = workspace_id.is_some_and(|workspace_id| {
        sigil_kernel::WriteIsolationProjection::from_entries(session.entries())
            .active_lease_for_workspace(&workspace_id)
            .and_then(|state| state.acquired.as_ref())
            .is_some_and(|lease| !lease.owner_agent_id.starts_with(&task_owner_prefix))
    });
    let requires_write = candidate
        .required_capabilities
        .contains(&sigil_kernel::TaskCapabilityV2::WorkspaceWrite);
    // The verification runner is a host mechanism (RFC-0003 materializer), not a tool
    // capability: it is available when the auto-run policy allows checks and the workspace
    // identity the runner needs can be resolved.
    let verification_runner_available = !matches!(
        root_config.verification.auto_run,
        sigil_kernel::VerificationAutoRunPolicy::Never
    ) && stable_workspace_id(workspace_root).is_ok();
    TaskAdmissionProbeContext {
        tool_contracts,
        provider_route_available: route_available,
        credential_available,
        permission_profile_ok: !requires_write
            || !matches!(
                root_config.permission.mode,
                sigil_kernel::PermissionMode::ReadOnly
            ),
        disk_space_bytes: fs2::available_space(workspace_root).ok(),
        verification_runner_available,
        external_writer_active,
    }
}

/// Resolves route shape and the exact credential the provider build would use.
///
/// Route availability only proves the connection configuration is valid; credential
/// availability separately resolves the configured source (environment variable or stored
/// record) so a missing API key is discovered at admission instead of at provider startup.
fn route_and_credential_probe(root_config: &RootConfig) -> (bool, bool) {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    if loaded.mode != crate::provider_connections::ConfigMode::V2 {
        return (false, false);
    }
    let Some(model_ref) = loaded.default_model.as_ref() else {
        return (false, false);
    };
    let route_available =
        crate::provider_connections::resolve_model_route(root_config, model_ref).is_ok();
    if !route_available {
        return (false, false);
    }
    let Some(connection) = loaded.connections.get(&model_ref.connection_id) else {
        return (false, false);
    };
    let credential_available = match &connection.credential {
        crate::provider_connections::LoadedCredentialRef::Config(
            crate::provider_connections::CredentialRefConfig::Environment { name },
        ) => {
            let environment = crate::provider_connections::ProcessCredentialEnvironment;
            crate::provider_connections::read_configured_environment_credential(
                &connection.config,
                name,
                &environment,
            )
            .is_some()
        }
        crate::provider_connections::LoadedCredentialRef::Config(
            crate::provider_connections::CredentialRefConfig::None,
        ) => true,
        crate::provider_connections::LoadedCredentialRef::Config(
            crate::provider_connections::CredentialRefConfig::Stored { id },
        ) => {
            let store =
                crate::provider_connections::ConfiguredProviderCredentialStore::from_root_config(
                    root_config,
                );
            futures::executor::block_on(
                <crate::provider_connections::ConfiguredProviderCredentialStore as crate::provider_connections::ProviderCredentialStore>::load(
                    &store,
                    id,
                ),
            )
            .is_ok_and(|record| record.is_some())
        }
    };
    (true, credential_available)
}

/// Runs one monotonic admission attempt for an adopted Task (RFC-0067 10.2, 14.2).
///
/// Admission observes the current environment and appends a durable
/// `TaskAdmissionAttemptedV1` with a typed `Ready | Blocked | Paused` outcome. It never executes
/// tools, never modifies the workspace and never generates a Plan.
///
/// # Errors
///
/// Returns an error when the admission record cannot be appended.
pub fn admit_adopted_task(
    session: &mut Session,
    root_config: &RootConfig,
    workspace_root: &Path,
    task_id: &TaskId,
    candidate: &sigil_kernel::ExecutablePlanCandidateV1,
    probes: &TaskAdmissionProbeContext,
    now_ms: u64,
) -> Result<sigil_kernel::TaskAdmissionOutcomeV1> {
    let base_snapshot = candidate.compile_binding.base_workspace_snapshot_id.clone();
    let current_snapshot = plan_handoff_workspace_snapshot_id(root_config, workspace_root)
        .ok()
        .flatten();
    let workspace_state = match (base_snapshot.as_deref(), current_snapshot.as_deref()) {
        (Some(base), Some(current)) if base == current => {
            sigil_kernel::WorkspaceAdmissionStateV1::ExactMatch
        }
        (Some(_), Some(_)) => sigil_kernel::WorkspaceAdmissionStateV1::ExternalDrift,
        _ => sigil_kernel::WorkspaceAdmissionStateV1::SnapshotUnavailable,
    };
    let missing_capabilities = probes
        .tool_contracts
        .as_ref()
        .map(|contracts| {
            let available = contracts
                .iter()
                .flat_map(|tool| tool.capabilities.iter().copied())
                .collect::<BTreeSet<_>>();
            candidate
                .required_capabilities
                .iter()
                .copied()
                .filter(|capability| !available.contains(&capability.tool_capability()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let requires_verification = candidate
        .step_contracts
        .iter()
        .any(|binding| !binding.contract.check_spec_refs.is_empty());
    let observation = sigil_kernel::TaskAdmissionObservationV1 {
        base_workspace_snapshot_id: base_snapshot.clone(),
        current_workspace_snapshot_id: current_snapshot.clone(),
        workspace_state,
        missing_capabilities: missing_capabilities.clone(),
        provider_route_available: probes.provider_route_available,
        credential_available: probes.credential_available,
        permission_profile_ok: probes.permission_profile_ok,
        disk_space_bytes: probes.disk_space_bytes,
        external_writer_active: probes.external_writer_active,
        verification_runner_available: probes.verification_runner_available,
        observed_at_ms: now_ms,
    };
    let outcome = if session
        .task_state_projection()
        .tasks
        .get(task_id)
        .is_some_and(|task| task.status == TaskRunStatus::Paused)
    {
        // CreatePaused start mode: the task waits for an explicit resume without probing.
        sigil_kernel::TaskAdmissionOutcomeV1::Paused(sigil_kernel::TaskPauseReasonV1::CreatePaused)
    } else {
        let blocker = |reason_code: sigil_kernel::TaskBlockerReasonCodeV1,
                       summary: String,
                       affected_step: Option<TaskStepId>,
                       affected_capability: Option<sigil_kernel::TaskCapabilityV2>,
                       actions: &[sigil_kernel::TaskBlockerActionV1]| {
            sigil_kernel::TaskBlockerV1 {
                reason_code,
                summary,
                affected_step,
                affected_capability,
                retryable: true,
                available_actions: actions.to_vec(),
                evidence_digest: sigil_kernel::stable_event_hash(
                    serde_json::to_string(&observation)
                        .unwrap_or_default()
                        .as_bytes(),
                ),
                created_at_ms: now_ms,
                resolved_at_ms: None,
            }
        };
        match workspace_state {
            sigil_kernel::WorkspaceAdmissionStateV1::ExternalDrift => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::WorkspaceChanged,
                    "the workspace changed since the plan was created; re-admit after reviewing the drift"
                        .to_owned(),
                    None,
                    None,
                    &[
                        sigil_kernel::TaskBlockerActionV1::RetryAdmission,
                        sigil_kernel::TaskBlockerActionV1::Replan,
                        sigil_kernel::TaskBlockerActionV1::Cancel,
                    ],
                ))
            }
            sigil_kernel::WorkspaceAdmissionStateV1::SnapshotUnavailable => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::WorkspaceSnapshotUnavailable,
                    "the current workspace snapshot is unavailable; the task is held until the workspace can be verified"
                        .to_owned(),
                    None,
                    None,
                    &[sigil_kernel::TaskBlockerActionV1::RetryAdmission],
                ))
            }
            _ if !missing_capabilities.is_empty() => {
                let capability = missing_capabilities.first().copied();
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::MissingRequiredCapability,
                    format!(
                        "the tool registry is missing required capabilities: {}",
                        missing_capabilities
                            .iter()
                            .map(|capability| capability.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None,
                    capability,
                    &[sigil_kernel::TaskBlockerActionV1::RetryAdmission],
                ))
            }
            _ if !probes.provider_route_available => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::ProviderUnavailable,
                    "the configured provider route is unavailable; rebind the route and retry"
                        .to_owned(),
                    None,
                    None,
                    &[
                        sigil_kernel::TaskBlockerActionV1::RebindRoute,
                        sigil_kernel::TaskBlockerActionV1::RetryAdmission,
                    ],
                ))
            }
            _ if !probes.credential_available => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::CredentialUnavailable,
                    "the provider credential is unavailable; configure credentials and retry"
                        .to_owned(),
                    None,
                    None,
                    &[sigil_kernel::TaskBlockerActionV1::RetryAdmission],
                ))
            }
            _ if !probes.permission_profile_ok => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::PermissionRequired,
                    "the current permission profile cannot cover the task; grant permission and retry"
                        .to_owned(),
                    None,
                    None,
                    &[
                        sigil_kernel::TaskBlockerActionV1::GrantPermission,
                        sigil_kernel::TaskBlockerActionV1::RetryAdmission,
                    ],
                ))
            }
            _ if probes.external_writer_active => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::ExternalWriterActive,
                    "another writer holds the workspace; retry after it releases the workspace"
                        .to_owned(),
                    None,
                    None,
                    &[sigil_kernel::TaskBlockerActionV1::RetryAdmission],
                ))
            }
            _ if probes
                .disk_space_bytes
                .is_some_and(|bytes| bytes < TASK_ADMISSION_MIN_DISK_SPACE_BYTES) =>
            {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::DiskSpaceExhausted,
                    "free disk space is below the task admission threshold".to_owned(),
                    None,
                    None,
                    &[sigil_kernel::TaskBlockerActionV1::RetryAdmission],
                ))
            }
            _ if requires_verification && !probes.verification_runner_available => {
                sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker(
                    sigil_kernel::TaskBlockerReasonCodeV1::VerificationRunnerUnavailable,
                    "the plan requires verification checks but the verification runner is unavailable"
                        .to_owned(),
                    None,
                    None,
                    &[sigil_kernel::TaskBlockerActionV1::RetryAdmission],
                ))
            }
            _ => sigil_kernel::TaskAdmissionOutcomeV1::Ready(
                sigil_kernel::TaskRuntimeLeaseBindingV1 {
                    lease_id: sigil_kernel::stable_event_uuid(
                        "sigil-task-admission-lease-v1",
                        &format!(
                            "{}:{}",
                            task_id.as_str(),
                            session.task_state_projection().next_admission_ordinal(task_id)
                        ),
                    ),
                    granted_at_ms: now_ms,
                },
            ),
        }
    };
    let ordinal = session
        .task_state_projection()
        .next_admission_ordinal(task_id);
    session.append_control(ControlEntry::TaskAdmissionAttemptedV1(
        sigil_kernel::TaskAdmissionAttemptV1 {
            task_id: task_id.clone(),
            plan_version: candidate.task_plan.plan_version,
            ordinal,
            candidate_hash: candidate.candidate_hash.clone(),
            observed_environment: observation,
            outcome: outcome.clone(),
        },
    ))?;
    Ok(outcome)
}

/// Builds a bounded, user-safe message for one typed plan run rejection.
pub fn plan_run_rejection_message(rejection: &sigil_kernel::PlanRunRejectionV1) -> String {
    match rejection {
        sigil_kernel::PlanRunRejectionV1::PlanMissing => {
            "the plan is not present in this session".to_owned()
        }
        sigil_kernel::PlanRunRejectionV1::PlanHashStale { expected, current } => {
            format!("the plan changed since it was shown (expected {expected}, current {current})")
        }
        sigil_kernel::PlanRunRejectionV1::PlanNotReady { plan_state } => {
            format!("the plan is not ready to run: {}", plan_state.as_str())
        }
        sigil_kernel::PlanRunRejectionV1::PlanRejected => "the plan was rejected".to_owned(),
        sigil_kernel::PlanRunRejectionV1::CandidateMissing => {
            "the plan has no executable candidate".to_owned()
        }
        sigil_kernel::PlanRunRejectionV1::CandidateHashMismatch { expected, current } => {
            format!(
                "the plan candidate changed since it was shown (expected {expected}, current {current})"
            )
        }
        sigil_kernel::PlanRunRejectionV1::FrontierStale { expected, current } => {
            format!(
                "the session changed while running (expected {expected}, current {current}); retry the same command"
            )
        }
        sigil_kernel::PlanRunRejectionV1::CommandIdentityConflict => {
            "the run command conflicts with an earlier command".to_owned()
        }
        sigil_kernel::PlanRunRejectionV1::PermissionChoiceUnavailable { reason } => {
            format!("the requested permission is unavailable: {reason}")
        }
        sigil_kernel::PlanRunRejectionV1::SessionWriterUnavailable { reason } => {
            format!("the session writer is unavailable: {reason}")
        }
    }
}

pub fn application_plan_decision(
    root_config: &RootConfig,
    workspace_root: &Path,
    session_log_path: &Path,
    expected_scope: &str,
    command: &ApplicationPlanDecisionCommand,
) -> Result<ApplicationPlanDecisionReceipt> {
    let effective_config = root_config.with_effective_composition()?;
    let root_config = &effective_config;
    anyhow::ensure!(
        root_config.task.enabled
            && root_config
                .composition
                .allows(sigil_kernel::OptionalCapability::TaskOrchestration),
        "task orchestration is not selected for this session composition"
    );
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        root_config,
        &fallback_route,
        store.clone(),
    )?;
    if inspected.session.session_scope_id() != expected_scope {
        bail!("plan decision session scope mismatch");
    }
    crate::validate_session_composition(&inspected.session, root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan decision session scope mismatch");
    }
    let plan_id = PlanId::new(command.plan_id.clone())
        .map_err(|error| anyhow!("invalid plan id for decision: {error}"))?;
    let draft = session
        .plan_artifact_projection()
        .plans
        .get(&plan_id)
        .cloned()
        .ok_or_else(|| anyhow!("plan {} is not present in this session", plan_id.as_str()));
    if !matches!(
        command.action,
        ApplicationPlanAction::AdoptCandidate | ApplicationPlanAction::RetryReview
    ) {
        let draft = draft.as_ref().map_err(|error| anyhow!("{error}"))?;
        if draft.plan_hash != command.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                command.expected_plan_hash,
                draft.plan_hash
            );
        }
    }
    let draft = draft.ok();
    let parent_session_ref = session_ref_for_log_path(session_log_path)?;
    let receipt = match command.action {
        ApplicationPlanAction::AdoptCandidate => {
            let expected_candidate_hash = command
                .expected_candidate_hash
                .as_deref()
                .filter(|hash| !hash.trim().is_empty())
                .context("plan candidate adoption requires an exact candidate hash")?;
            let mut handler = sigil_kernel::NoopEventHandler;
            let adopted = PlanReviewCoordinator::adopt_plan_review_candidate(
                &mut session,
                &plan_id,
                expected_candidate_hash,
                &mut handler,
                now_ms(),
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: adopted.plan_hash.clone(),
                action: ApplicationPlanAction::AdoptCandidate,
                task_id: None,
                task_title: None,
                candidate_hash: Some(expected_candidate_hash.to_owned()),
                task_phase: None,
                task_blocker: None,
                user_input_request: None,
                revision_request: None,
            }
        }
        ApplicationPlanAction::RetryReview => {
            let expected_candidate_hash = command
                .expected_candidate_hash
                .as_deref()
                .filter(|hash| !hash.trim().is_empty());
            let retry = PlanReviewCoordinator::retry_plan_review_for_plan(
                &mut session,
                &plan_id,
                expected_candidate_hash,
                now_ms(),
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                // A terminal review may not have a draft hash.  Preserve the exact candidate
                // binding in the dedicated receipt field and leave the absent draft hash empty.
                plan_hash: String::new(),
                action: ApplicationPlanAction::RetryReview,
                task_id: None,
                task_title: None,
                candidate_hash: expected_candidate_hash.map(str::to_owned),
                task_phase: None,
                task_blocker: None,
                user_input_request: None,
                revision_request: Some(retry.request),
            }
        }
        ApplicationPlanAction::Run => {
            let plan_id = PlanId::new(command.plan_id.clone())
                .map_err(|error| anyhow!("invalid plan id for decision: {error}"))?;
            let run_command = sigil_kernel::PlanRunCommandV1 {
                command_id: sigil_kernel::stable_event_uuid(
                    "sigil-plan-run-command-v1",
                    &format!(
                        "{}:{}:{}:run:{}",
                        session.session_scope_id(),
                        command.plan_id,
                        command.expected_plan_hash,
                        match command.permission_grant {
                            Some(PlanApprovalPermission::WorkspaceEdits) => "scoped_edits",
                            Some(PlanApprovalPermission::Ask) | None => "current_policy",
                        }
                    ),
                ),
                session_id: session.session_scope_id().to_owned(),
                plan_id: plan_id.clone(),
                expected_plan_hash: command.expected_plan_hash.clone(),
                expected_candidate_hash: String::new(),
                expected_durable_frontier: session.durable_frontier_sequence(),
                start_mode: PlanTaskStartMode::CreateAndRun,
                permission: match command.permission_grant {
                    Some(PlanApprovalPermission::WorkspaceEdits) => {
                        sigil_kernel::PlanRunPermissionChoiceV1::GrantScopedEditsOnce
                    }
                    Some(PlanApprovalPermission::Ask) | None => {
                        sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy
                    }
                },
                source: sigil_kernel::PlanRunCommandSource::Http,
            };
            let approved = PlanExecutionService::approve(
                &mut session,
                parent_session_ref,
                &run_command,
                now_ms(),
            )
            .map_err(|rejection| {
                anyhow!(
                    "plan run was rejected: {}",
                    plan_run_rejection_message(&rejection)
                )
            })?;
            let task_phase =
                match PlanExecutionService::direct_execution_outcome(&approved, now_ms()) {
                    sigil_kernel::TaskAdmissionOutcomeV1::Ready(_) => {
                        Some(sigil_kernel::TaskExecutionPhaseV1::Ready)
                    }
                    sigil_kernel::TaskAdmissionOutcomeV1::Paused(_) => {
                        Some(sigil_kernel::TaskExecutionPhaseV1::Paused)
                    }
                    sigil_kernel::TaskAdmissionOutcomeV1::Blocked(_) => unreachable!(
                        "first-class direct Plan execution has no materialization blocker"
                    ),
                };
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.as_ref().expect("validated draft").plan_hash.clone(),
                action: ApplicationPlanAction::Run,
                task_id: Some(approved.task_id.as_str().to_owned()),
                task_title: Some(approved.task_title),
                candidate_hash: None,
                task_phase,
                task_blocker: None,
                user_input_request: None,
                revision_request: None,
            }
        }
        ApplicationPlanAction::Save => {
            PlanReviewCoordinator::record_plan_decision(
                &mut session,
                &PlanDecisionCommand {
                    plan_id: command.plan_id.clone(),
                    expected_plan_hash: command.expected_plan_hash.clone(),
                    decision: PlanDecision::SavedOnly,
                },
                now_ms(),
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.as_ref().expect("validated draft").plan_hash.clone(),
                action: ApplicationPlanAction::Save,
                task_id: None,
                task_title: None,
                candidate_hash: None,
                task_phase: None,
                task_blocker: None,
                user_input_request: None,
                revision_request: None,
            }
        }
        ApplicationPlanAction::Revise => {
            let workspace_snapshot_id =
                plan_handoff_workspace_snapshot_id(root_config, workspace_root)
                    .ok()
                    .flatten();
            let revision_request = PlanReviewCoordinator::retry_plan_revision(
                &mut session,
                &plan_id,
                &command.expected_plan_hash,
                workspace_snapshot_id,
                now_ms(),
            )?;
            let user_input_request = if revision_request.is_none() {
                let requested = PlanReviewCoordinator::request_plan_revision_guidance(
                    &mut session,
                    &plan_id,
                    &command.expected_plan_hash,
                    now_ms(),
                )?;
                Some(
                    session
                        .user_input_projection()?
                        .request(&requested.request.identity)
                        .map(sigil_kernel::UserInputRequestStateV1::public_view)
                        .context("revision guidance request was not projected")?,
                )
            } else {
                None
            };
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.as_ref().expect("validated draft").plan_hash.clone(),
                action: ApplicationPlanAction::Revise,
                task_id: None,
                task_title: None,
                candidate_hash: None,
                task_phase: None,
                task_blocker: None,
                user_input_request,
                revision_request,
            }
        }
        ApplicationPlanAction::Reject => {
            PlanReviewCoordinator::reject_plan(
                &mut session,
                &RejectPlanRequest {
                    plan_id: command.plan_id.clone(),
                    expected_plan_hash: command.expected_plan_hash.clone(),
                },
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.as_ref().expect("validated draft").plan_hash.clone(),
                action: ApplicationPlanAction::Reject,
                task_id: None,
                task_title: None,
                candidate_hash: None,
                task_phase: None,
                task_blocker: None,
                user_input_request: None,
                revision_request: None,
            }
        }
    };
    Ok(receipt)
}

/// Accepts one exact host-owned plan-revision guidance decision and prepares the supervised
/// revision attempt. The answer, terminal user-input resolution, and `RevisionRequested` fact
/// are written in one durable control batch before the request is returned.
pub fn application_plan_revision_guidance_decision(
    root_config: &RootConfig,
    workspace_root: &Path,
    session_log_path: &Path,
    expected_scope: &str,
    command: sigil_kernel::UserInputDecisionCommandV1,
) -> Result<(
    sigil_kernel::UserInputDecisionReceiptV1,
    Option<PlanReviewRunRequest>,
)> {
    let effective_config = root_config.with_effective_composition()?;
    let root_config = &effective_config;
    anyhow::ensure!(
        root_config.task.enabled
            && root_config
                .composition
                .allows(sigil_kernel::OptionalCapability::TaskOrchestration),
        "task orchestration is not selected for this session composition"
    );
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        root_config,
        &fallback_route,
        store.clone(),
    )?;
    crate::validate_session_composition(&inspected.session, root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan revision guidance session scope mismatch");
    }
    PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        command,
        plan_handoff_workspace_snapshot_id(root_config, workspace_root)
            .ok()
            .flatten(),
        now_ms(),
    )
}

/// Accepts one exact child-owned plan-research question through its bound parent session.
pub fn application_plan_review_research_input_decision(
    root_config: &RootConfig,
    session_log_path: &Path,
    expected_scope: &str,
    command: sigil_kernel::UserInputDecisionCommandV1,
    child_resource_provisioner: &dyn PlanReviewChildResourceProvisionerV1,
) -> Result<(
    sigil_kernel::UserInputDecisionReceiptV1,
    Option<PlanReviewRunRequest>,
    Option<PublicEventOutboxEntryV1>,
)> {
    let effective_config = root_config.with_effective_composition()?;
    let root_config = &effective_config;
    anyhow::ensure!(
        root_config.task.enabled
            && root_config
                .composition
                .allows(sigil_kernel::OptionalCapability::TaskOrchestration),
        "task orchestration is not selected for this session composition"
    );
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        root_config,
        &fallback_route,
        store.clone(),
    )?;
    crate::validate_session_composition(&inspected.session, root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan-review research input parent scope mismatch");
    }
    PlanReviewCoordinator::accept_plan_review_research_input_with_resources(
        &mut session,
        command,
        now_ms(),
        child_resource_provisioner,
    )
}

fn session_ref_for_log_path(path: &Path) -> Result<SessionRef> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("session.jsonl");
    // Managed session leaves are physically `<key>/records.jsonl`, but review references use the
    // logical direct `<key>.jsonl` identity. Never turn a managed path into `records.jsonl`, which
    // would make lifecycle/artifact resolution fall back to the configured legacy directory.
    let logical_file_name = if file_name == "records.jsonl"
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            == Some("session-log")
    {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .map(|key| format!("{key}.jsonl"))
            .unwrap_or_else(|| file_name.to_owned())
    } else {
        file_name.to_owned()
    };
    SessionRef::new_relative(&logical_file_name)
        .map_err(|error| anyhow!("failed to build parent session ref: {error}"))
}
