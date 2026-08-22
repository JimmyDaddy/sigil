use std::{
    any::Any,
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::{
    mutation::{ExecutionMutationProfile, MutationEventRecorder, WorkspaceMutationScan},
    permission::{ApprovalMode, NetworkPolicy, ToolOperation, infer_tool_operation},
    permission_plan::{
        ExecutionContainmentRequest, ToolAnalysisStatus, ToolPermissionEffect,
        ToolPermissionPlanDraft, ToolPermissionPlanV2, ToolPermissionSummary,
    },
    provider::ModelMessage,
    session::{
        ControlEntry, ToolArtifactAvailability, ToolArtifactCaptureSink, ToolArtifactDescriptorV1,
        ToolArtifactEncoding, ToolArtifactReadBudgetV1, ToolArtifactRefV1, ToolArtifactSensitivity,
        ToolArtifactStore, ToolOutputArchivedArtifactBindingV1,
    },
    verification::{DEFAULT_TASK_VERIFICATION_SCOPE_HASH, ToolEffect, VerificationScope},
};

const MODEL_TOOL_CONTENT_MAX_BYTES: usize = 32 * 1024;
const MODEL_TOOL_CONTENT_HEAD_BYTES: usize = 24 * 1024;
const MODEL_TOOL_CONTENT_TAIL_BYTES: usize = 8 * 1024;

/// JSON-schema-backed tool contract exposed to model providers and UI approvals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub category: ToolCategory,
    pub access: ToolAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_effect: Option<NetworkEffect>,
    pub preview: ToolPreviewCapability,
}

/// Role-specific tool visibility and execution scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolRegistryScope {
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub names: BTreeSet<String>,
    #[serde(default)]
    pub prefixes: Vec<String>,
}

impl ToolRegistryScope {
    pub fn from_names_and_prefixes(
        names: impl IntoIterator<Item = impl Into<String>>,
        prefixes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            allow_all: false,
            names: names.into_iter().map(Into::into).collect(),
            prefixes: prefixes.into_iter().map(Into::into).collect(),
        }
    }

    pub fn allows(&self, name: &str) -> bool {
        self.allow_all
            || self.names.contains(name)
            || self.prefixes.iter().any(|prefix| name.starts_with(prefix))
    }

    pub fn is_empty(&self) -> bool {
        !self.allow_all && self.names.is_empty() && self.prefixes.is_empty()
    }

    pub fn intersection(&self, other: &Self) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::default();
        }
        if self.allow_all {
            return other.clone();
        }
        if other.allow_all {
            return self.clone();
        }

        let mut names = self
            .names
            .iter()
            .filter(|name| other.allows(name))
            .cloned()
            .collect::<BTreeSet<_>>();
        names.extend(other.names.iter().filter(|name| self.allows(name)).cloned());

        let mut prefixes = Vec::new();
        for left in &self.prefixes {
            for right in &other.prefixes {
                if left.starts_with(right) {
                    push_unique_prefix(&mut prefixes, left.clone());
                } else if right.starts_with(left) {
                    push_unique_prefix(&mut prefixes, right.clone());
                }
            }
        }

        Self {
            allow_all: false,
            names,
            prefixes,
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        if self.allow_all || other.allow_all {
            return Self {
                allow_all: true,
                ..Self::default()
            };
        }

        let mut names = self.names.clone();
        names.extend(other.names.iter().cloned());

        let mut prefixes = self.prefixes.clone();
        for prefix in &other.prefixes {
            push_unique_prefix(&mut prefixes, prefix.clone());
        }

        Self {
            allow_all: false,
            names,
            prefixes,
        }
    }
}

fn push_unique_prefix(prefixes: &mut Vec<String>, prefix: String) {
    if !prefixes.iter().any(|existing| existing == &prefix) {
        prefixes.push(prefix);
    }
}

/// Coarse product category for one provider-neutral tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    File,
    Search,
    Shell,
    Mcp,
    Agent,
    Custom,
}

impl ToolCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Search => "search",
            Self::Shell => "shell",
            Self::Mcp => "mcp",
            Self::Agent => "agent",
            Self::Custom => "custom",
        }
    }
}

/// Provider-neutral access class used by permission policy and UI risk labels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccess {
    Read,
    Write,
    Execute,
}

impl ToolAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
        }
    }
}

/// Independent network effect declared by one tool or concrete tool call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkEffect {
    Read,
    Mutate,
    Unknown,
}

impl NetworkEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutate => "mutate",
            Self::Unknown => "unknown",
        }
    }
}

/// Declares whether a tool can or must provide an approval preview.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPreviewCapability {
    None,
    Optional,
    Required,
}

/// Mutation evidence strategy owned by one tool implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMutationTracking {
    None,
    /// Every workspace write uses the RFC-0002 coordinator and its exact per-file evidence.
    Controlled,
    /// Effects are not fully mediated, so the registry must scan the workspace around execution.
    Unknown,
}

/// Runtime-only scheduling contract for one registered tool generation.
///
/// This is intentionally excluded from [`ToolSpec`], so changing local execution scheduling does
/// not perturb the provider-visible tool schema or its prompt-cache prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolConcurrencyClass {
    /// The call is a barrier and may not overlap another tool body.
    Exclusive,
    /// The call may share a bounded lane after exact permission analysis confirms read-only
    /// effects and no workspace mutation tracking.
    ParallelReadOnly,
}

/// Runtime-only replay authority for one exact tool generation.
///
/// This does not appear in [`ToolSpec`]: replay safety is a host-side effect contract and must
/// never perturb the provider-visible schema or prompt-cache prefix. Unknown shell, MCP, hosted
/// and generic network effects therefore remain non-replayable unless their implementation opts
/// into an observation-backed contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolReplayClassV1 {
    /// Re-running an incomplete read cannot change world state. The current V1 runtime still
    /// records the original execution boundary before admitting a new read.
    PureRead,
    /// The effect owns an exact idempotency key and may create a successor only after its
    /// implementation-specific admission check succeeds.
    Idempotent,
    /// The effect must first observe the exact outside state; it never receives implicit replay
    /// authority from the task or permission mode.
    Reconciliable,
    /// An incomplete effect is blocked for explicit reconciliation; the host never replays it.
    NonReplayable,
}

/// Versioned local contract used when recovering an interrupted tool effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolReplayContractV1 {
    pub schema_version: u16,
    pub class: ToolReplayClassV1,
    /// Opaque implementation-owned idempotency receipt kind. Required only for `Idempotent`.
    pub idempotency_key_kind: Option<String>,
    /// Opaque read-only observation probe kind. Required for `Reconciliable` effects.
    pub reconciliation_probe_kind: Option<String>,
    /// Opaque implementation-owned version/fingerprint. It is included in local invocation
    /// grants but excluded from provider request material.
    pub runtime_fingerprint: String,
}

impl ToolReplayContractV1 {
    #[must_use]
    pub fn non_replayable() -> Self {
        Self {
            schema_version: 1,
            class: ToolReplayClassV1::NonReplayable,
            idempotency_key_kind: None,
            reconciliation_probe_kind: None,
            runtime_fingerprint: "tool-replay-v1:non-replayable".to_owned(),
        }
    }

    #[must_use]
    pub fn pure_read() -> Self {
        Self {
            schema_version: 1,
            class: ToolReplayClassV1::PureRead,
            idempotency_key_kind: None,
            reconciliation_probe_kind: None,
            runtime_fingerprint: "tool-replay-v1:pure-read".to_owned(),
        }
    }

    /// Declares a prepared local effect that can only be observed, never implicitly replayed.
    #[must_use]
    pub fn reconciliable(probe_kind: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            class: ToolReplayClassV1::Reconciliable,
            idempotency_key_kind: None,
            reconciliation_probe_kind: Some(probe_kind.into()),
            runtime_fingerprint: "tool-replay-v1:reconciliable".to_owned(),
        }
    }

    /// Rejects malformed or unversioned runtime-only recovery contracts at admission.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.runtime_fingerprint.trim().is_empty()
            || self.runtime_fingerprint.len() > 256
        {
            bail!("tool replay contract is malformed");
        }
        let valid_kind = |value: &Option<String>| {
            value
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty() && value.len() <= 128)
        };
        if !valid_kind(&self.idempotency_key_kind) || !valid_kind(&self.reconciliation_probe_kind) {
            bail!("tool replay contract contains an invalid runtime-only kind");
        }
        match self.class {
            ToolReplayClassV1::Idempotent if self.idempotency_key_kind.is_none() => {
                bail!("idempotent tool replay contract lacks an idempotency key kind");
            }
            ToolReplayClassV1::Reconciliable if self.reconciliation_probe_kind.is_none() => {
                bail!("reconciliable tool replay contract lacks an observation probe kind");
            }
            _ => {}
        }
        Ok(())
    }
}

/// Provider-neutral semantic ability contributed by one concrete tool generation.
///
/// Unlike [`ToolCategory`], capabilities are used for task admission. They are part of the local
/// runtime contract fingerprint but never enter the provider-visible tool JSON schema.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    WorkspaceRead,
    WorkspaceWrite,
    VcsRead,
    ProcessExecute,
    NetworkRead,
    ArtifactRead,
    VerificationRun,
}

impl ToolCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspace_read",
            Self::WorkspaceWrite => "workspace_write",
            Self::VcsRead => "vcs_read",
            Self::ProcessExecute => "process_execute",
            Self::NetworkRead => "network_read",
            Self::ArtifactRead => "artifact_read",
            Self::VerificationRun => "verification_run",
        }
    }
}

/// Complete runtime contract for one visible registered tool.
#[derive(Debug, Clone)]
pub struct ToolRuntimeContract {
    pub spec: ToolSpec,
    pub mutation_tracking: ToolMutationTracking,
    pub concurrency_class: ToolConcurrencyClass,
    pub capabilities: BTreeSet<ToolCapability>,
    pub replay_contract: ToolReplayContractV1,
}

/// One resource or capability subject touched by a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolSubject {
    pub kind: ToolSubjectKind,
    pub original: String,
    pub normalized: String,
    #[serde(default)]
    pub canonical_path: Option<PathBuf>,
    pub scope: ToolSubjectScope,
    /// Access required for this individual subject. Shell commands may contain both read and
    /// write paths, so policy must not infer every subject's access from the enclosing tool.
    #[serde(default = "default_tool_subject_access")]
    pub access: ToolAccess,
}

fn default_tool_subject_access() -> ToolAccess {
    ToolAccess::Read
}

impl ToolSubject {
    pub fn path(original: impl Into<String>, normalized: impl Into<String>) -> Self {
        Self::path_with_scope(original, normalized, None, ToolSubjectScope::Workspace)
    }

    pub fn path_with_scope(
        original: impl Into<String>,
        normalized: impl Into<String>,
        canonical_path: Option<PathBuf>,
        scope: ToolSubjectScope,
    ) -> Self {
        Self {
            kind: ToolSubjectKind::Path,
            original: original.into(),
            normalized: normalized.into(),
            canonical_path,
            scope,
            access: ToolAccess::Read,
        }
    }

    #[must_use]
    pub fn with_access(mut self, access: ToolAccess) -> Self {
        self.access = access;
        self
    }

    pub fn command(command: impl Into<String>, normalized: impl Into<String>) -> Self {
        Self {
            kind: ToolSubjectKind::Command,
            original: command.into(),
            normalized: normalized.into(),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
            access: ToolAccess::Execute,
        }
    }

    pub fn mcp_tool(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            kind: ToolSubjectKind::McpTool,
            original: name.clone(),
            normalized: name,
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
            access: ToolAccess::Execute,
        }
    }

    pub fn mcp_trust_class(server_name: impl Into<String>, trust_class: impl Into<String>) -> Self {
        let server_name = server_name.into();
        let trust_class = trust_class.into();
        Self {
            kind: ToolSubjectKind::McpTrustClass,
            original: format!("{server_name}:{trust_class}"),
            normalized: format!("mcp_trust_class:{trust_class}"),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
            access: ToolAccess::Execute,
        }
    }

    /// Creates an MCP trust subject whose durable identity binds one concrete process environment.
    ///
    /// The stable normalized value remains suitable for permission-rule matching, while the
    /// original value retains the static and live fingerprints for approval/audit consumers.
    #[must_use]
    pub fn mcp_trust_class_with_process_binding(
        server_name: impl Into<String>,
        trust_class: impl Into<String>,
        static_fingerprint: impl AsRef<str>,
        live_fingerprint: impl AsRef<str>,
    ) -> Self {
        let server_name = server_name.into();
        let trust_class = trust_class.into();
        Self {
            kind: ToolSubjectKind::McpTrustClass,
            original: format!(
                "{server_name}:{trust_class}:{}:{}",
                static_fingerprint.as_ref(),
                live_fingerprint.as_ref()
            ),
            normalized: format!("mcp_trust_class:{trust_class}"),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
            access: ToolAccess::Execute,
        }
    }

    pub fn agent(profile_id: impl Into<String>) -> Self {
        let profile_id = profile_id.into();
        Self {
            kind: ToolSubjectKind::Agent,
            original: profile_id.clone(),
            normalized: format!("agent:{profile_id}"),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
            access: ToolAccess::Execute,
        }
    }
}

/// Safe summary of data a tool is about to send outside the local agent boundary.
///
/// The payload must be pre-redacted and bounded by the tool implementation; it is persisted
/// in the control plane for audit and must not contain raw file contents or secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ToolEgressAudit {
    pub destination: String,
    pub operation: String,
    pub payload: Value,
    pub redacted: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSubjectKind {
    Path,
    Command,
    NetworkEndpoint,
    McpTool,
    McpTrustClass,
    Agent,
    Other,
}

impl ToolSubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Command => "command",
            Self::NetworkEndpoint => "network_endpoint",
            Self::McpTool => "mcp_tool",
            Self::McpTrustClass => "mcp_trust_class",
            Self::Agent => "agent",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSubjectScope {
    Workspace,
    /// A path proven by a built-in analyzer to remain inside the runtime-owned, session-scoped
    /// scratch capability. It is outside the workspace physically but is not an arbitrary
    /// external-directory request.
    RuntimeScratch,
    External,
    Unknown,
}

impl ToolSubjectScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::RuntimeScratch => "runtime_scratch",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }
}

/// Execution context shared with tools at runtime.
#[derive(Debug, Clone)]
struct ToolArtifactSourceAuthorization {
    source_event_id: String,
    artifact_sha256: String,
    persisted_bytes: u64,
    call_id: String,
    tool_name: String,
    availability: ToolArtifactAvailability,
}

#[derive(Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub timeout_secs: u64,
    pub mutation_recorder: Option<MutationEventRecorder>,
    egress_audit_recorder: Option<crate::EgressAuditRecorder>,
    user_url_capability_registrar: Option<Arc<dyn crate::UserUrlCapabilityRegistrar>>,
    session_scope_id: Option<String>,
    logical_run_id: Option<String>,
    tool_artifact_store: Option<ToolArtifactStore>,
    tool_artifact_read_budget: Option<ToolArtifactReadBudgetV1>,
    tool_artifact_source_authorizations:
        Arc<BTreeMap<ToolArtifactRefV1, ToolArtifactSourceAuthorization>>,
    active_context_epoch_id: Option<String>,
    web_task_tree_budget: Option<Arc<crate::WebTaskTreeBudget>>,
    network_policy: NetworkPolicy,
    explicit_network_approval: bool,
    approved_subjects: Vec<ToolSubject>,
    prepared_permission_plan: Option<Arc<ToolPermissionPlanV2>>,
    progress_sink: Option<Arc<dyn ToolProgressSink>>,
    execution_mutation_profile_recorded_call_ids: BTreeSet<String>,
    cancellation: Option<crate::RunCancellationHandle>,
    agent_invocation_grant: Option<crate::AgentInvocationGrant>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace_root", &self.workspace_root)
            .field("timeout_secs", &self.timeout_secs)
            .field("mutation_recorder", &self.mutation_recorder.is_some())
            .field(
                "egress_audit_recorder",
                &self.egress_audit_recorder.is_some(),
            )
            .field(
                "user_url_capability_registrar",
                &self.user_url_capability_registrar.is_some(),
            )
            .field("session_scope_id", &self.session_scope_id)
            .field("logical_run_id", &self.logical_run_id)
            .field("tool_artifact_store", &self.tool_artifact_store.is_some())
            .field(
                "tool_artifact_read_budget",
                &self.tool_artifact_read_budget.is_some(),
            )
            .field(
                "tool_artifact_source_authorizations",
                &self.tool_artifact_source_authorizations.len(),
            )
            .field("active_context_epoch_id", &self.active_context_epoch_id)
            .field("web_task_tree_budget", &self.web_task_tree_budget.is_some())
            .field("network_policy", &self.network_policy)
            .field("explicit_network_approval", &self.explicit_network_approval)
            .field("approved_subjects", &self.approved_subjects.len())
            .field(
                "prepared_permission_plan",
                &self.prepared_permission_plan.is_some(),
            )
            .field("progress_sink", &self.progress_sink.is_some())
            .field("cancellation", &self.cancellation.is_some())
            .field(
                "agent_invocation_grant",
                &self
                    .agent_invocation_grant
                    .as_ref()
                    .map(crate::AgentInvocationGrant::fingerprint),
            )
            .field(
                "execution_mutation_profile_recorded_call_ids",
                &self.execution_mutation_profile_recorded_call_ids.len(),
            )
            .finish()
    }
}

impl ToolContext {
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>, timeout_secs: u64) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            timeout_secs,
            mutation_recorder: None,
            egress_audit_recorder: None,
            user_url_capability_registrar: None,
            session_scope_id: None,
            logical_run_id: None,
            tool_artifact_store: None,
            tool_artifact_read_budget: None,
            tool_artifact_source_authorizations: Arc::new(BTreeMap::new()),
            active_context_epoch_id: None,
            web_task_tree_budget: None,
            network_policy: NetworkPolicy::Allow,
            explicit_network_approval: false,
            approved_subjects: Vec::new(),
            prepared_permission_plan: None,
            progress_sink: None,
            execution_mutation_profile_recorded_call_ids: BTreeSet::new(),
            cancellation: None,
            agent_invocation_grant: None,
        }
    }

    /// Creates the fail-closed execution context for a configured eager network startup.
    ///
    /// This authority is narrower than interactive tool approval: it only represents an explicit
    /// root configuration whose effective network policy is already `allow`, and it still requires
    /// a durable recorder plus the root-owned budget before any adapter can reach egress.
    pub fn for_eager_network_startup(
        workspace_root: impl Into<PathBuf>,
        timeout_secs: u64,
        network_policy: NetworkPolicy,
        recorder: crate::EgressAuditRecorder,
        budget: Arc<crate::WebTaskTreeBudget>,
    ) -> Result<Self> {
        if network_policy != NetworkPolicy::Allow {
            bail!("eager network startup requires web.network_mode = allow");
        }
        Ok(Self::new(workspace_root, timeout_secs)
            .with_egress_audit_recorder(recorder)
            .with_network_authorization(NetworkPolicy::Allow, false)
            .with_web_task_tree_budget(budget))
    }

    #[must_use]
    pub fn with_mutation_recorder(mut self, recorder: MutationEventRecorder) -> Self {
        self.mutation_recorder = Some(recorder);
        self
    }

    #[must_use]
    pub(crate) fn with_egress_audit_recorder(
        mut self,
        recorder: crate::EgressAuditRecorder,
    ) -> Self {
        self.egress_audit_recorder = Some(recorder);
        self
    }

    /// Returns the durable recorder inherited from the active session.
    #[must_use]
    pub fn egress_audit_recorder(&self) -> Option<crate::EgressAuditRecorder> {
        self.egress_audit_recorder.clone()
    }

    /// Installs the process-local URL capability attachment inherited from the active session.
    #[must_use]
    pub(crate) fn with_user_url_capability_registrar(
        mut self,
        registrar: Arc<dyn crate::UserUrlCapabilityRegistrar>,
    ) -> Self {
        self.user_url_capability_registrar = Some(registrar);
        self
    }

    /// Returns the process-local URL capability attachment for exact source-id lookup.
    #[must_use]
    pub fn user_url_capability_registrar(
        &self,
    ) -> Option<Arc<dyn crate::UserUrlCapabilityRegistrar>> {
        self.user_url_capability_registrar.clone()
    }

    /// Installs the exact logical session scope for capability lookup.
    ///
    /// This is the per-run authority for session-scoped resources such as `$SIGIL_SCRATCH_DIR`;
    /// tools derive their session namespace from it, so the value must be stable across resume
    /// and identical between permission planning and execution.
    #[must_use]
    pub fn with_session_scope_id(mut self, session_scope_id: impl Into<String>) -> Self {
        self.session_scope_id = Some(session_scope_id.into());
        self
    }

    /// Returns the active logical session scope, when execution came through the agent loop.
    #[must_use]
    pub fn session_scope_id(&self) -> Option<&str> {
        self.session_scope_id.as_deref()
    }

    /// Installs the exact host-owned logical run identity for run-scoped tool routes.
    #[must_use]
    pub(crate) fn with_logical_run_id(mut self, logical_run_id: impl Into<String>) -> Self {
        self.logical_run_id = Some(logical_run_id.into());
        self
    }

    /// Returns the exact host-owned logical run identity, when execution came through the agent
    /// loop.
    #[must_use]
    pub fn logical_run_id(&self) -> Option<&str> {
        self.logical_run_id.as_deref()
    }

    /// Installs the exact permission plan revalidated immediately before execution.
    ///
    /// Host adapters that execute a user-authorized tool outside the agent loop must persist the
    /// same plan before calling this method. Carrying a plan here does not grant permission; it
    /// only binds the execution boundary and tool receipt to the authority already established by
    /// the host.
    #[must_use]
    pub fn with_prepared_permission_plan(mut self, plan: ToolPermissionPlanV2) -> Self {
        self.prepared_permission_plan = Some(Arc::new(plan));
        self
    }

    /// Returns the immutable plan whose hash was authorized for this execution.
    #[must_use]
    pub fn prepared_permission_plan(&self) -> Option<&ToolPermissionPlanV2> {
        self.prepared_permission_plan.as_deref()
    }

    #[must_use]
    pub fn with_tool_artifact_reader(
        mut self,
        store: ToolArtifactStore,
        budget: ToolArtifactReadBudgetV1,
        active_context_epoch_id: impl Into<String>,
    ) -> Self {
        self.tool_artifact_store = Some(store);
        self.tool_artifact_read_budget = Some(budget);
        self.active_context_epoch_id = Some(active_context_epoch_id.into());
        self
    }

    /// Installs one host-proven descriptor binding for typed retrieval.
    ///
    /// This is intended for constrained adapters and tests. Production agent runs install the
    /// complete active durable projection through `with_tool_artifact_source_bindings`.
    #[must_use]
    pub fn with_tool_artifact_source_binding(
        mut self,
        descriptor: &ToolArtifactDescriptorV1,
        source_event_id: impl Into<String>,
    ) -> Self {
        let source_event_id = source_event_id.into();
        if descriptor.validate().is_err()
            || source_event_id.trim().is_empty()
            || source_event_id.len() > 256
        {
            return self;
        }
        let mut bindings = self.tool_artifact_source_authorizations.as_ref().clone();
        bindings.insert(
            descriptor.artifact_ref.clone(),
            ToolArtifactSourceAuthorization {
                source_event_id,
                artifact_sha256: descriptor.content_sha256.clone(),
                persisted_bytes: descriptor.persisted_bytes,
                call_id: descriptor.tool_call_id.clone(),
                tool_name: descriptor.tool_name.clone(),
                availability: ToolArtifactAvailability::Available,
            },
        );
        self.tool_artifact_source_authorizations = Arc::new(bindings);
        self
    }

    #[must_use]
    pub(crate) fn with_tool_artifact_source_bindings(
        mut self,
        bindings: impl IntoIterator<Item = ToolOutputArchivedArtifactBindingV1>,
    ) -> Self {
        self.tool_artifact_source_authorizations = Arc::new(
            bindings
                .into_iter()
                .map(|binding| {
                    (
                        binding.artifact_ref,
                        ToolArtifactSourceAuthorization {
                            source_event_id: binding.source_event_id,
                            artifact_sha256: binding.artifact_sha256,
                            persisted_bytes: binding.persisted_bytes,
                            call_id: binding.call_id,
                            tool_name: binding.tool_name,
                            availability: binding.artifact_availability,
                        },
                    )
                })
                .collect(),
        );
        self
    }

    #[must_use]
    pub fn authorized_tool_artifact_source_event(
        &self,
        descriptor: &ToolArtifactDescriptorV1,
    ) -> Option<&str> {
        self.tool_artifact_source_authorizations
            .get(&descriptor.artifact_ref)
            .filter(|binding| {
                binding.artifact_sha256 == descriptor.content_sha256
                    && binding.persisted_bytes == descriptor.persisted_bytes
                    && binding.call_id == descriptor.tool_call_id
                    && binding.tool_name == descriptor.tool_name
                    && binding.availability == ToolArtifactAvailability::Available
            })
            .map(|binding| binding.source_event_id.as_str())
    }

    #[must_use]
    pub fn tool_artifact_store(&self) -> Option<&ToolArtifactStore> {
        self.tool_artifact_store.as_ref()
    }

    /// Starts a session-owned capture for bytes that already passed persistence policy.
    ///
    /// Tools must not pass raw credentials, unprojected URLs, or other sensitive source bytes to
    /// this boundary. The returned sink is absent when execution is not attached to a session.
    #[must_use]
    pub fn create_policy_safe_tool_output_sink(
        &self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        media_type: impl Into<String>,
        encoding: ToolArtifactEncoding,
        sensitivity: ToolArtifactSensitivity,
    ) -> Option<ToolArtifactCaptureSink> {
        self.tool_artifact_store.as_ref().map(|store| {
            store.begin_policy_safe_capture(
                tool_call_id,
                tool_name,
                media_type,
                encoding,
                sensitivity,
            )
        })
    }

    #[must_use]
    pub fn tool_artifact_read_budget(&self) -> Option<&ToolArtifactReadBudgetV1> {
        self.tool_artifact_read_budget.as_ref()
    }

    #[must_use]
    pub fn active_context_epoch_id(&self) -> Option<&str> {
        self.active_context_epoch_id.as_deref()
    }

    /// Installs the root-owned Web budget inherited from the active run.
    #[must_use]
    pub(crate) fn with_web_task_tree_budget(
        mut self,
        budget: Arc<crate::WebTaskTreeBudget>,
    ) -> Self {
        self.web_task_tree_budget = Some(budget);
        self
    }

    /// Returns the shared Web budget for this run and its nested Web effects.
    #[must_use]
    pub fn web_task_tree_budget(&self) -> Option<Arc<crate::WebTaskTreeBudget>> {
        self.web_task_tree_budget.clone()
    }

    #[must_use]
    pub fn with_cancellation(mut self, cancellation: crate::RunCancellationHandle) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Installs the exact child invocation capability for last-responsible-moment revalidation.
    #[must_use]
    pub(crate) fn with_agent_invocation_grant(
        mut self,
        grant: crate::AgentInvocationGrant,
    ) -> Self {
        self.agent_invocation_grant = Some(grant);
        self
    }

    /// Installs the effective network policy and the explicit approval fact for execution.
    ///
    /// This is crate-private so adapters can observe authorization but cannot manufacture it.
    /// An explicit approval is retained only for an `ask` policy, which keeps accidental callers
    /// fail-closed.
    #[must_use]
    pub(crate) fn with_network_authorization(
        mut self,
        network_policy: NetworkPolicy,
        explicit_network_approval: bool,
    ) -> Self {
        self.network_policy = network_policy;
        self.explicit_network_approval =
            explicit_network_approval && network_policy == NetworkPolicy::Ask;
        self
    }

    /// Returns the effective network policy for this execution.
    #[must_use]
    pub fn network_policy(&self) -> NetworkPolicy {
        self.network_policy
    }

    /// Returns whether the agent observed an explicit approval for this `ask`-policy execution.
    #[must_use]
    pub fn explicit_network_approval(&self) -> bool {
        self.explicit_network_approval
    }

    /// Admits one nested forward effect at the last responsible execution boundary.
    pub fn begin_forward_effect(
        &self,
        kind: crate::RunEffectKind,
    ) -> Result<Option<crate::RunEffectGuard>> {
        self.cancellation
            .as_ref()
            .map(|handle| handle.begin_effect(crate::RunEffectClass::Forward, kind))
            .transpose()
            .map_err(Into::into)
    }

    #[must_use]
    pub fn cancellation_handle(&self) -> Option<crate::RunCancellationHandle> {
        self.cancellation.clone()
    }

    /// Carries the exact subjects authorized by the agent into the execution boundary.
    ///
    /// This does not grant permission by itself. Tools may use it only to fail closed when a
    /// dynamic subject changes after approval and before an external effect starts.
    #[must_use]
    pub fn with_approved_subjects(mut self, subjects: Vec<ToolSubject>) -> Self {
        self.approved_subjects = subjects;
        self
    }

    /// Returns the exact subjects authorized for this execution, if it was dispatched by an agent.
    #[must_use]
    pub fn approved_subjects(&self) -> &[ToolSubject] {
        &self.approved_subjects
    }

    #[must_use]
    pub fn with_progress_sink(mut self, sink: Arc<dyn ToolProgressSink>) -> Self {
        self.progress_sink = Some(sink);
        self
    }

    /// Emits a transient tool progress update to the runtime event stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the downstream progress channel has been closed.
    pub fn emit_progress(&self, event: ToolProgressEvent) -> Result<()> {
        if let Some(sink) = &self.progress_sink {
            sink.emit(event)?;
        }
        Ok(())
    }

    #[must_use]
    pub(crate) fn with_execution_mutation_profile_recorded(
        mut self,
        call_id: impl Into<String>,
    ) -> Self {
        self.execution_mutation_profile_recorded_call_ids
            .insert(call_id.into());
        self
    }

    #[must_use]
    fn execution_mutation_profile_recorded_for(&self, call_id: &str) -> bool {
        self.execution_mutation_profile_recorded_call_ids
            .contains(call_id)
    }
}

/// Transient progress sink installed by the agent loop while a tool is executing.
///
/// Progress events are for live UI surfaces and must not be treated as provider-visible final
/// tool results. Durable audit state should continue to use started/completed tool execution
/// entries and final [`ToolResult`] metadata.
pub trait ToolProgressSink: Send + Sync {
    /// Emits one progress event.
    ///
    /// # Errors
    ///
    /// Returns an error when the receiver cannot accept the event.
    fn emit(&self, event: ToolProgressEvent) -> Result<()>;
}

/// Stable identifier for one logical tool execution lifecycle.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ToolExecutionId(String);

impl ToolExecutionId {
    /// Creates an identifier safe to use in progress coalescing keys and durable execution records.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_tool_execution_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ToolExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolExecutionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn validate_tool_execution_id(value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("tool execution id cannot be empty");
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        bail!("tool execution id must not contain path separators or traversal");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("tool execution id contains unsupported characters");
    }
    Ok(())
}

/// Provider-neutral live progress update for a running tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ToolProgressEvent {
    pub execution_id: ToolExecutionId,
    pub call_id: String,
    pub tool_name: String,
    pub sequence: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_log_ref: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    #[serde(default)]
    pub details: Value,
}

/// Normalized tool execution result returned to the agent loop and UI.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub content: String,
    pub status: ToolResultStatus,
    pub metadata: ToolResultMeta,
    #[serde(skip)]
    pub transient_context: Vec<ModelMessage>,
    #[serde(skip)]
    pub control_entries: Vec<ControlEntry>,
    #[serde(skip)]
    pub url_capability_registrations: Box<Vec<crate::UserUrlCapabilityRegistration>>,
    #[serde(skip)]
    pub external_sources: Box<Vec<crate::ExternalSourceRecord>>,
    #[serde(skip)]
    pre_captured_artifact: Option<Box<PreCapturedToolArtifact>>,
    /// RFC-0062 8: harness-owned capture evidence returned with the execution receipt; settled
    /// by the tool layer into the durable V3 projection.
    #[serde(skip)]
    capture_outcome: Option<crate::ExecutionCaptureOutcome>,
    /// RFC-0062 9.7: already-materialized durable V3 projection (process capture path). When
    /// present, the agent loop appends it directly instead of re-capturing the bounded content.
    #[serde(skip)]
    durable_v3_projection: Option<Box<crate::ToolResultRecordedV3>>,
}

#[derive(Debug, Clone)]
pub(crate) enum PreCapturedToolArtifact {
    Published(Box<ToolArtifactDescriptorV1>),
    Unavailable { observed_bytes: u64 },
}

impl Clone for ToolResult {
    fn clone(&self) -> Self {
        Self {
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            content: self.content.clone(),
            status: self.status.clone(),
            metadata: self.metadata.clone(),
            transient_context: self.transient_context.clone(),
            control_entries: self.control_entries.clone(),
            url_capability_registrations: self.url_capability_registrations.clone(),
            external_sources: self.external_sources.clone(),
            pre_captured_artifact: self.pre_captured_artifact.clone(),
            // RFC-0062 8: capture ownership is single; clones never carry the harness capture.
            capture_outcome: None,
            durable_v3_projection: self.durable_v3_projection.clone(),
        }
    }
}

impl ToolResult {
    pub fn ok(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
        metadata: ToolResultMeta,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            content: content.into(),
            status: ToolResultStatus::Ok,
            metadata,
            transient_context: Vec::new(),
            control_entries: Vec::new(),
            url_capability_registrations: Box::default(),
            external_sources: Box::default(),
            pre_captured_artifact: None,
            capture_outcome: None,
            durable_v3_projection: None,
        }
    }

    pub fn error(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        kind: ToolErrorKind,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            content: message.clone(),
            capture_outcome: None,
            durable_v3_projection: None,
            status: ToolResultStatus::Error(ToolError {
                kind,
                message,
                retryable: false,
                details: Value::Null,
            }),
            metadata: ToolResultMeta::default(),
            transient_context: Vec::new(),
            control_entries: Vec::new(),
            url_capability_registrations: Box::default(),
            external_sources: Box::default(),
            pre_captured_artifact: None,
        }
    }

    pub fn with_error_details(mut self, retryable: bool, details: Value) -> Self {
        if let ToolResultStatus::Error(error) = &mut self.status {
            error.retryable = retryable;
            error.details = details;
        }
        self
    }

    pub fn with_transient_context(mut self, context: Vec<ModelMessage>) -> Self {
        self.transient_context = context;
        self
    }

    pub fn with_control_entry(mut self, entry: ControlEntry) -> Self {
        self.control_entries.push(entry);
        self
    }

    /// Carries exact URL registrations to the agent's pre-persistence tool-result boundary.
    ///
    /// The registrations are consumed before the result is emitted as a `RunEvent`; raw URL
    /// material therefore remains process-local and never enters the durable tool envelope.
    pub fn with_url_capability_registrations(
        mut self,
        registrations: Vec<crate::UserUrlCapabilityRegistration>,
    ) -> Self {
        self.url_capability_registrations = Box::new(registrations);
        self
    }

    /// Carries normalized external sources for a durable tool-result provenance sidecar.
    pub fn with_external_sources(mut self, sources: Vec<crate::ExternalSourceRecord>) -> Self {
        self.external_sources = Box::new(sources);
        self
    }

    /// Attaches a session-scoped artifact already published by the tool's streaming output path.
    ///
    /// The agent boundary revalidates session scope, content hash, and result identity before
    /// using this descriptor. A failed validation is fail-closed and never falls back to
    /// recapturing the tool's inline preview.
    #[must_use]
    pub fn with_captured_artifact(mut self, artifact: ToolArtifactDescriptorV1) -> Self {
        self.pre_captured_artifact = Some(Box::new(PreCapturedToolArtifact::Published(Box::new(
            artifact,
        ))));
        self
    }

    /// Marks a streaming capture attempt as unavailable without recapturing its inline preview.
    #[must_use]
    pub fn with_unavailable_artifact_capture(mut self, observed_bytes: u64) -> Self {
        self.pre_captured_artifact = Some(Box::new(PreCapturedToolArtifact::Unavailable {
            observed_bytes,
        }));
        self
    }

    pub(crate) fn pre_captured_artifact(&self) -> Option<&PreCapturedToolArtifact> {
        self.pre_captured_artifact.as_deref()
    }

    pub fn is_error(&self) -> bool {
        matches!(self.status, ToolResultStatus::Error(_))
    }

    pub fn to_model_content(&self) -> String {
        let mut envelope = Map::new();
        let model_content = model_visible_tool_content(&self.content);
        match &self.status {
            ToolResultStatus::Ok => {
                envelope.insert("status".to_owned(), Value::String("ok".to_owned()));
                envelope.insert(
                    "content".to_owned(),
                    Value::String(model_content.content.into_owned()),
                );
            }
            ToolResultStatus::Error(error) => {
                envelope.insert("status".to_owned(), Value::String("error".to_owned()));
                envelope.insert(
                    "content".to_owned(),
                    Value::String(model_content.content.into_owned()),
                );
                envelope.insert("error".to_owned(), error.to_model_value());
            }
        }
        if let Some(truncation) = model_content.truncation {
            envelope.insert("content_truncation".to_owned(), truncation.to_model_value());
        }
        if let Some(meta) = self.metadata.to_model_value() {
            envelope.insert("meta".to_owned(), meta);
        }
        serde_json::to_string(&Value::Object(envelope)).unwrap_or_else(|error| {
            format!(
                r#"{{"status":"error","error":{{"kind":"internal","message":"failed to serialize tool result: {error}","retryable":false}}}}"#
            )
        })
    }

    pub fn to_model_message(&self) -> crate::provider::ModelMessage {
        crate::provider::ModelMessage::tool(self.call_id.clone(), self.to_model_content())
    }

    /// RFC-0062 8: takes the harness-owned capture outcome for terminal settlement.
    #[must_use]
    pub fn take_capture_outcome(&mut self) -> Option<crate::ExecutionCaptureOutcome> {
        self.capture_outcome.take()
    }

    /// RFC-0062 8: attaches harness-owned capture evidence returned by the execution backend.
    pub fn with_capture_outcome(mut self, outcome: crate::ExecutionCaptureOutcome) -> Self {
        self.capture_outcome = Some(outcome);
        self
    }

    /// RFC-0062 9.7: attaches the already-materialized durable V3 projection (process capture).
    pub fn with_durable_v3_projection(
        mut self,
        recorded: crate::ToolResultRecordedV3,
        _display: crate::session::ToolDisplayViewV1,
    ) -> Self {
        self.durable_v3_projection = Some(Box::new(recorded));
        self
    }

    #[must_use]
    pub fn durable_v3_projection(&self) -> Option<&crate::ToolResultRecordedV3> {
        self.durable_v3_projection.as_deref()
    }

    pub fn set_durable_v3_projection(
        &mut self,
        recorded: crate::ToolResultRecordedV3,
        display: crate::session::ToolDisplayViewV1,
    ) {
        self.durable_v3_projection = Some(Box::new(recorded));
        let _ = display;
    }

    /// RFC-0062 8: installs the outcome and lets the caller finalize the artifact from it.
    pub fn attach_capture_outcome(&mut self, outcome: crate::ExecutionCaptureOutcome) {
        self.capture_outcome = Some(outcome);
    }

    pub fn summary(&self) -> ToolResultSummary {
        let (error_kind, error_message) = match &self.status {
            ToolResultStatus::Ok => (None, None),
            ToolResultStatus::Error(error) => (Some(error.kind), Some(error.message.clone())),
        };
        ToolResultSummary {
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            is_error: self.is_error(),
            status_label: if self.is_error() {
                "error".to_owned()
            } else {
                "ok".to_owned()
            },
            content_preview: self.content.clone(),
            changed_files: self.metadata.changed_files.clone(),
            exit_code: self.metadata.exit_code,
            truncated: self.metadata.truncated,
            bytes: self.metadata.bytes.or(self.metadata.returned_bytes),
            error_kind,
            error_message,
        }
    }
}

struct ModelVisibleToolContent<'a> {
    content: Cow<'a, str>,
    truncation: Option<ModelToolContentTruncation>,
}

struct ModelToolContentTruncation {
    original_bytes: usize,
    omitted_bytes: usize,
    head_bytes: usize,
    tail_bytes: usize,
}

impl ModelToolContentTruncation {
    fn to_model_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("truncated".to_owned(), Value::Bool(true));
        object.insert(
            "reason".to_owned(),
            Value::String("model_context_limit".to_owned()),
        );
        object.insert(
            "original_bytes".to_owned(),
            Value::Number((self.original_bytes as u64).into()),
        );
        object.insert(
            "omitted_bytes".to_owned(),
            Value::Number((self.omitted_bytes as u64).into()),
        );
        object.insert(
            "head_bytes".to_owned(),
            Value::Number((self.head_bytes as u64).into()),
        );
        object.insert(
            "tail_bytes".to_owned(),
            Value::Number((self.tail_bytes as u64).into()),
        );
        Value::Object(object)
    }
}

fn model_visible_tool_content(content: &str) -> ModelVisibleToolContent<'_> {
    if content.len() <= MODEL_TOOL_CONTENT_MAX_BYTES {
        return ModelVisibleToolContent {
            content: Cow::Borrowed(content),
            truncation: None,
        };
    }

    let head_end = previous_char_boundary(content, MODEL_TOOL_CONTENT_HEAD_BYTES);
    let tail_start = next_char_boundary(
        content,
        content.len().saturating_sub(MODEL_TOOL_CONTENT_TAIL_BYTES),
    )
    .max(head_end);
    let tail_bytes = content.len().saturating_sub(tail_start);
    let omitted_bytes = tail_start.saturating_sub(head_end);
    let marker = format!(
        "\n[model content truncated: original_bytes={} omitted_bytes={} head_bytes={} tail_bytes={}]\n",
        content.len(),
        omitted_bytes,
        head_end,
        tail_bytes
    );
    let mut visible = String::with_capacity(head_end + marker.len() + tail_bytes);
    visible.push_str(&content[..head_end]);
    visible.push_str(&marker);
    visible.push_str(&content[tail_start..]);

    ModelVisibleToolContent {
        content: Cow::Owned(visible),
        truncation: Some(ModelToolContentTruncation {
            original_bytes: content.len(),
            omitted_bytes,
            head_bytes: head_end,
            tail_bytes,
        }),
    }
}

fn previous_char_boundary(value: &str, max_index: usize) -> usize {
    let mut index = max_index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(value: &str, min_index: usize) -> usize {
    let mut index = min_index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Structured success/error status for one tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Ok,
    Error(ToolError),
}

/// Stable structured tool error returned to provider-visible history and UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub details: Value,
}

impl ToolError {
    fn to_model_value(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "kind".to_owned(),
            Value::String(self.kind.as_str().to_owned()),
        );
        object.insert("message".to_owned(), Value::String(self.message.clone()));
        object.insert("retryable".to_owned(), Value::Bool(self.retryable));
        if !value_is_empty(&self.details) {
            object.insert("details".to_owned(), model_visible_details(&self.details));
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    InvalidInput,
    PermissionDenied,
    ApprovalRequired,
    ApprovalDenied,
    PathOutsideWorkspace,
    ExternalDirectoryRequired,
    NotFound,
    Timeout,
    /// Execution exceeded a bounded runtime resource such as captured output.
    ResourceLimit,
    /// Execution could not start or continue because a local resource is exhausted.
    ResourceExhausted,
    Interrupted,
    ExitStatus,
    Io,
    Utf8,
    Network,
    Protocol,
    Unsupported,
    /// A recovery-critical effect cannot proceed without its configured durable audit sink.
    DurabilityRequired,
    /// An approval-bound immutable mutation no longer matches its call or workspace revision.
    StalePreparedMutation,
    /// A concurrent workspace write touched evidence needed by this effect. The effect must be
    /// re-read, regenerated, or reconciled; the enclosing Task remains resumable.
    WorkspaceConflict,
    /// The effect may have happened but its terminal receipt is not yet proven. Automatic replay
    /// is forbidden until a read-only reconciliation probe settles the exact effect.
    EffectReconciliationRequired,
    /// The session-scoped scratch namespace has reached its capacity quota.
    ScratchQuotaExceeded,
    Internal,
}

impl ToolErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
            Self::ApprovalRequired => "approval_required",
            Self::ApprovalDenied => "approval_denied",
            Self::PathOutsideWorkspace => "path_outside_workspace",
            Self::ExternalDirectoryRequired => "external_directory_required",
            Self::NotFound => "not_found",
            Self::Timeout => "timeout",
            Self::ResourceLimit => "resource_limit",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Interrupted => "interrupted",
            Self::ExitStatus => "exit_status",
            Self::Io => "io",
            Self::Utf8 => "utf8",
            Self::Network => "network",
            Self::Protocol => "protocol",
            Self::Unsupported => "unsupported",
            Self::DurabilityRequired => "durability_required",
            Self::StalePreparedMutation => "stale_prepared_mutation",
            Self::WorkspaceConflict => "workspace_conflict",
            Self::EffectReconciliationRequired => "effect_reconciliation_required",
            Self::ScratchQuotaExceeded => "scratch_quota_exceeded",
            Self::Internal => "internal",
        }
    }

    /// Returns true when this error is an active recovery boundary rather than an irrecoverable
    /// tool failure. A successful typed tool receipt may resolve it; final prose may not.
    #[must_use]
    pub fn is_recovery_blocker(self) -> bool {
        matches!(
            self,
            Self::ResourceExhausted
                | Self::DurabilityRequired
                | Self::StalePreparedMutation
                | Self::WorkspaceConflict
                | Self::EffectReconciliationRequired
        )
    }
}

/// Typed infrastructure failure at the boundary between an admitted tool and its effect receipt.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolExecutionGuardError {
    #[error("tool effect requires reconciliation before it can be replayed or accepted")]
    EffectReconciliationRequired,
}

pub(crate) fn tool_result_from_execution_error(
    call_id: impl Into<String>,
    tool_name: impl Into<String>,
    error: &anyhow::Error,
) -> ToolResult {
    let kind = match error.downcast_ref::<ToolExecutionGuardError>() {
        Some(ToolExecutionGuardError::EffectReconciliationRequired) => {
            ToolErrorKind::EffectReconciliationRequired
        }
        None => ToolErrorKind::Internal,
    };
    ToolResult::error(call_id, tool_name, kind, error.to_string()).with_error_details(
        kind.is_recovery_blocker(),
        serde_json::json!({
            "recovery_blocker": kind.is_recovery_blocker(),
            "recovery_action": match kind {
                ToolErrorKind::WorkspaceConflict => "reread_or_rebase",
                ToolErrorKind::EffectReconciliationRequired => "reconcile_effect",
                _ => "inspect",
            },
        }),
    )
}

/// Shared summary used by TUI, CLI, and future audit surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResultSummary {
    pub call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub status_label: String,
    pub content_preview: String,
    pub changed_files: Vec<String>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub bytes: Option<u64>,
    pub error_kind: Option<ToolErrorKind>,
    pub error_message: Option<String>,
}

/// Human-readable preview shown before a mutating tool is approved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreview {
    pub title: String,
    pub summary: String,
    pub body: String,
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub file_diffs: Vec<ToolPreviewFile>,
}

/// Per-file diff section within a tool preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewFile {
    pub path: String,
    pub diff: String,
}

/// Tool-owned immutable artifact materialized before permission approval.
///
/// The kernel keeps the artifact opaque while binding its content digest and exact subjects to
/// the permission decision. A prepared artifact is moved into execution exactly once; tools must
/// not place a second-query token or mutable cache handle in this value.
pub struct ToolPreparation {
    preview: ToolPreview,
    subjects: Vec<ToolSubject>,
    content_digest: String,
    artifact: Box<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for ToolPreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPreparation")
            .field("preview", &self.preview)
            .field("subjects", &self.subjects)
            .field("content_digest", &self.content_digest)
            .finish_non_exhaustive()
    }
}

impl ToolPreparation {
    /// Creates a tool preparation from one immutable artifact and its content-bound digest.
    pub fn new<T>(
        preview: ToolPreview,
        subjects: Vec<ToolSubject>,
        content_digest: impl Into<String>,
        artifact: T,
    ) -> Result<Self>
    where
        T: Any + Send + Sync,
    {
        let content_digest = content_digest.into();
        if !content_digest.starts_with("sha256:") {
            bail!("tool preparation content digest must use sha256");
        }
        if subjects.is_empty() {
            bail!("tool preparation must bind at least one permission subject");
        }
        Ok(Self {
            preview,
            subjects,
            content_digest,
            artifact: Box::new(artifact),
        })
    }
}

/// Permission binding attached to one prepared tool artifact.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreparationBinding {
    pub call_id: String,
    pub tool_name: String,
    pub args_digest: String,
    pub approval_identity: String,
    pub policy_fingerprint: String,
    pub subjects: Vec<ToolSubject>,
}

/// Safe durable projection linking approval, execution, and mutation batch audit records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PreparedToolAuditBinding {
    pub schema_version: u32,
    pub approval_identity: String,
    pub prepared_digest: String,
    pub content_digest: String,
    pub args_digest: String,
    pub policy_fingerprint: String,
}

/// Registry-owned draft whose exact subjects must participate in permission evaluation.
pub struct ToolPreparationDraft {
    tool: Arc<dyn Tool>,
    args: Value,
    preparation: ToolPreparation,
    call_id: String,
    tool_name: String,
    args_digest: String,
}

impl std::fmt::Debug for ToolPreparationDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPreparationDraft")
            .field("call_id", &self.call_id)
            .field("tool_name", &self.tool_name)
            .field("args_digest", &self.args_digest)
            .field("preparation", &self.preparation)
            .finish()
    }
}

impl ToolPreparationDraft {
    #[must_use]
    pub fn preview(&self) -> &ToolPreview {
        &self.preparation.preview
    }

    #[must_use]
    pub fn subjects(&self) -> &[ToolSubject] {
        &self.preparation.subjects
    }

    /// Binds this one-shot draft to the exact permission and approval authority.
    pub(crate) fn bind_with_approval_identity(
        self,
        policy_fingerprint: impl Into<String>,
        approval_identity: impl Into<String>,
    ) -> Result<PreparedToolCall> {
        let policy_fingerprint = policy_fingerprint.into();
        if !policy_fingerprint.starts_with("sha256:") {
            bail!("prepared tool policy fingerprint must use sha256");
        }
        let approval_identity = approval_identity.into();
        if approval_identity.trim().is_empty() {
            bail!("prepared tool approval identity must not be empty");
        }
        let binding = ToolPreparationBinding {
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            args_digest: self.args_digest,
            approval_identity,
            policy_fingerprint,
            subjects: self.preparation.subjects.clone(),
        };
        let digest_material = serde_json::json!({
            "schema_version": 2,
            "binding": {
                "call_id": &binding.call_id,
                "tool_name": &binding.tool_name,
                "args_digest": &binding.args_digest,
                "policy_fingerprint": &binding.policy_fingerprint,
                "subjects": &binding.subjects,
            },
            "content_digest": self.preparation.content_digest,
        });
        let encoded = serde_json::to_vec(&digest_material)
            .map_err(|error| anyhow!("failed to encode prepared tool binding: {error}"))?;
        let prepared_digest = crate::stable_event_hash(encoded);
        Ok(PreparedToolCall {
            tool: self.tool,
            args: self.args,
            artifact: self.preparation.artifact,
            preview: self.preparation.preview,
            binding,
            content_digest: self.preparation.content_digest,
            prepared_digest,
        })
    }
}

/// One approval-bound prepared tool call consumed by execution exactly once.
pub struct PreparedToolCall {
    tool: Arc<dyn Tool>,
    args: Value,
    artifact: Box<dyn Any + Send + Sync>,
    preview: ToolPreview,
    binding: ToolPreparationBinding,
    content_digest: String,
    prepared_digest: String,
}

impl std::fmt::Debug for PreparedToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedToolCall")
            .field("binding", &self.binding)
            .field("content_digest", &self.content_digest)
            .field("prepared_digest", &self.prepared_digest)
            .finish_non_exhaustive()
    }
}

impl PreparedToolCall {
    pub(crate) fn authorize(mut self, approval_identity: impl Into<String>) -> Result<Self> {
        let approval_identity = approval_identity.into();
        if approval_identity.trim().is_empty() {
            bail!("prepared tool approval identity must not be empty");
        }
        self.binding.approval_identity = approval_identity;
        Ok(self)
    }

    #[must_use]
    pub fn preview(&self) -> &ToolPreview {
        &self.preview
    }

    #[must_use]
    pub fn prepared_digest(&self) -> &str {
        &self.prepared_digest
    }

    #[must_use]
    pub fn binding(&self) -> &ToolPreparationBinding {
        &self.binding
    }

    #[must_use]
    pub fn audit_binding(&self) -> PreparedToolAuditBinding {
        PreparedToolAuditBinding {
            schema_version: 1,
            approval_identity: self.binding.approval_identity.clone(),
            prepared_digest: self.prepared_digest.clone(),
            content_digest: self.content_digest.clone(),
            args_digest: self.binding.args_digest.clone(),
            policy_fingerprint: self.binding.policy_fingerprint.clone(),
        }
    }

    fn into_execution(self) -> PreparedToolExecution {
        PreparedToolExecution {
            artifact: self.artifact,
            binding: self.binding,
            content_digest: self.content_digest,
            prepared_digest: self.prepared_digest,
        }
    }
}

/// Tool-facing approval-bound artifact passed only through the prepared execution path.
pub struct PreparedToolExecution {
    artifact: Box<dyn Any + Send + Sync>,
    binding: ToolPreparationBinding,
    content_digest: String,
    prepared_digest: String,
}

impl std::fmt::Debug for PreparedToolExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedToolExecution")
            .field("binding", &self.binding)
            .field("content_digest", &self.content_digest)
            .field("prepared_digest", &self.prepared_digest)
            .finish_non_exhaustive()
    }
}

impl PreparedToolExecution {
    #[must_use]
    pub fn prepared_digest(&self) -> &str {
        &self.prepared_digest
    }

    #[must_use]
    pub fn binding(&self) -> &ToolPreparationBinding {
        &self.binding
    }

    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }

    #[must_use]
    pub fn audit_binding(&self) -> PreparedToolAuditBinding {
        PreparedToolAuditBinding {
            schema_version: 1,
            approval_identity: self.binding.approval_identity.clone(),
            prepared_digest: self.prepared_digest.clone(),
            content_digest: self.content_digest.clone(),
            args_digest: self.binding.args_digest.clone(),
            policy_fingerprint: self.binding.policy_fingerprint.clone(),
        }
    }

    /// Consumes and downcasts the opaque tool-owned artifact.
    pub fn into_artifact<T>(self) -> Result<T>
    where
        T: Any + Send + Sync,
    {
        self.artifact
            .downcast::<T>()
            .map(|artifact| *artifact)
            .map_err(|_| anyhow!("prepared tool artifact type does not match the registered tool"))
    }
}

/// Bounded, persisted projection of one tool preview for user-facing UI replay.
///
/// This snapshot is control-plane data only. It is designed for TUI/session restore surfaces and
/// must not be injected into provider-visible tool result content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewSnapshot {
    pub call_id: String,
    pub tool_name: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub file_diffs: Vec<ToolPreviewFileSnapshot>,
    pub original_stats: ToolDiffStats,
    pub rendered_stats: ToolDiffStats,
    pub original_line_count: usize,
    pub rendered_line_count: usize,
    pub original_byte_count: usize,
    pub rendered_byte_count: usize,
    pub truncated: bool,
    #[serde(default)]
    pub original_preview_hash: Option<String>,
    pub budget: ToolDiffBudget,
}

impl ToolPreviewSnapshot {
    /// Builds a bounded snapshot from an approval preview.
    ///
    /// The resulting snapshot keeps enough unified diff context for UI rendering while recording
    /// the original stats needed to show truncation honestly.
    pub fn from_preview(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        preview: &ToolPreview,
        budget: ToolDiffBudget,
        original_preview_hash: Option<String>,
    ) -> Self {
        let mut rendered_files = Vec::new();
        let mut original_stats = ToolDiffStats::default();
        let mut rendered_stats = ToolDiffStats::default();
        let mut original_line_count = 0usize;
        let mut rendered_line_count = 0usize;
        let mut original_byte_count = 0usize;
        let mut rendered_byte_count = 0usize;
        let mut truncated = preview.file_diffs.len() > budget.max_files;

        for file in preview.file_diffs.iter().take(budget.max_files) {
            let file_original_line_count = diff_line_count(&file.diff);
            let file_original_byte_count = file.diff.len();
            let file_original_stats = ToolDiffStats::from_unified_diff(&file.diff);
            original_stats += file_original_stats;
            original_line_count += file_original_line_count;
            original_byte_count += file_original_byte_count;

            let remaining_lines = budget.max_lines_total.saturating_sub(rendered_line_count);
            let remaining_bytes = budget.max_bytes_total.saturating_sub(rendered_byte_count);
            let line_budget = budget.max_lines_per_file.min(remaining_lines);
            let byte_budget = budget.max_bytes_per_file.min(remaining_bytes);
            let bounded = bounded_diff_text(&file.diff, line_budget, byte_budget);
            let file_rendered_stats = ToolDiffStats::from_unified_diff(&bounded.diff);
            let file_rendered_line_count = diff_line_count(&bounded.diff);
            let file_rendered_byte_count = bounded.diff.len();

            truncated |= bounded.truncated;
            rendered_stats += file_rendered_stats;
            rendered_line_count += file_rendered_line_count;
            rendered_byte_count += file_rendered_byte_count;

            rendered_files.push(ToolPreviewFileSnapshot {
                path: file.path.clone(),
                diff: bounded.diff,
                original_stats: file_original_stats,
                rendered_stats: file_rendered_stats,
                original_line_count: file_original_line_count,
                rendered_line_count: file_rendered_line_count,
                original_byte_count: file_original_byte_count,
                rendered_byte_count: file_rendered_byte_count,
                truncated: bounded.truncated,
            });
        }

        for file in preview.file_diffs.iter().skip(budget.max_files) {
            original_stats += ToolDiffStats::from_unified_diff(&file.diff);
            original_line_count += diff_line_count(&file.diff);
            original_byte_count += file.diff.len();
        }

        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            title: preview.title.clone(),
            summary: preview.summary.clone(),
            changed_files: preview.changed_files.clone(),
            file_diffs: rendered_files,
            original_stats,
            rendered_stats,
            original_line_count,
            rendered_line_count,
            original_byte_count,
            rendered_byte_count,
            truncated,
            original_preview_hash,
            budget,
        }
    }
}

/// Per-file bounded diff captured for one tool preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewFileSnapshot {
    pub path: String,
    pub diff: String,
    pub original_stats: ToolDiffStats,
    pub rendered_stats: ToolDiffStats,
    pub original_line_count: usize,
    pub rendered_line_count: usize,
    pub original_byte_count: usize,
    pub rendered_byte_count: usize,
    pub truncated: bool,
}

/// Unified diff statistics used by approval and historical tool-card surfaces.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolDiffStats {
    pub added: usize,
    pub removed: usize,
    pub hunks: usize,
}

impl ToolDiffStats {
    /// Counts added, removed, and hunk header lines in unified diff text.
    pub fn from_unified_diff(diff: &str) -> Self {
        let mut stats = Self::default();
        for line in diff.lines() {
            if line.starts_with("@@") {
                stats.hunks += 1;
            } else if line.starts_with('+') && !line.starts_with("+++") {
                stats.added += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                stats.removed += 1;
            }
        }
        stats
    }
}

impl std::ops::AddAssign for ToolDiffStats {
    fn add_assign(&mut self, rhs: Self) {
        self.added += rhs.added;
        self.removed += rhs.removed;
        self.hunks += rhs.hunks;
    }
}

/// Budget used when persisting tool preview diffs into the append-only control log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolDiffBudget {
    pub max_files: usize,
    pub max_lines_total: usize,
    pub max_lines_per_file: usize,
    pub max_bytes_total: usize,
    pub max_bytes_per_file: usize,
}

impl Default for ToolDiffBudget {
    fn default() -> Self {
        Self {
            max_files: 12,
            max_lines_total: 320,
            max_lines_per_file: 160,
            max_bytes_total: 96 * 1024,
            max_bytes_per_file: 48 * 1024,
        }
    }
}

struct BoundedDiffText {
    diff: String,
    truncated: bool,
}

fn bounded_diff_text(diff: &str, max_lines: usize, max_bytes: usize) -> BoundedDiffText {
    let original_line_count = diff_line_count(diff);
    if max_lines == 0 || max_bytes == 0 {
        return BoundedDiffText {
            diff: String::new(),
            truncated: !diff.is_empty(),
        };
    }

    let mut rendered = String::new();
    let mut rendered_lines = 0usize;
    for line in diff.lines().take(max_lines) {
        let separator_bytes = usize::from(!rendered.is_empty());
        if rendered.len() + separator_bytes + line.len() > max_bytes {
            break;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(line);
        rendered_lines += 1;
    }

    BoundedDiffText {
        truncated: rendered_lines < original_line_count || rendered.len() < diff.len(),
        diff: rendered,
    }
}

fn diff_line_count(diff: &str) -> usize {
    if diff.is_empty() {
        0
    } else {
        diff.lines().count()
    }
}

/// Additional structured metadata emitted by a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResultMeta {
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub stdout_bytes: Option<u64>,
    pub stderr_bytes: Option<u64>,
    pub bytes: Option<u64>,
    pub truncated: bool,
    pub omitted_bytes: Option<u64>,
    pub limit_bytes: Option<u64>,
    pub limit_lines: Option<u64>,
    pub returned_bytes: Option<u64>,
    pub returned_lines: Option<u64>,
    pub total_bytes: Option<u64>,
    pub total_lines: Option<u64>,
    pub returned_matches: Option<u64>,
    pub total_matches: Option<u64>,
    pub returned_entries: Option<u64>,
    pub total_entries: Option<u64>,
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ToolReceiptMetadata>,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolReceiptStatus {
    Pending,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolReceiptMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_operation_ids: Vec<String>,
    pub status: ToolReceiptStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolReceiptReplayDecision {
    ReplayAllowed,
    ReplayDenied,
}

impl ToolReceiptMetadata {
    #[must_use]
    pub fn replay_decision(&self) -> ToolReceiptReplayDecision {
        if self.idempotent
            && self.idempotency_key.is_some()
            && self.status == ToolReceiptStatus::Interrupted
        {
            ToolReceiptReplayDecision::ReplayAllowed
        } else {
            ToolReceiptReplayDecision::ReplayDenied
        }
    }
}

impl Default for ToolResultMeta {
    fn default() -> Self {
        Self {
            duration_ms: None,
            exit_code: None,
            stdout_bytes: None,
            stderr_bytes: None,
            bytes: None,
            truncated: false,
            omitted_bytes: None,
            limit_bytes: None,
            limit_lines: None,
            returned_bytes: None,
            returned_lines: None,
            total_bytes: None,
            total_lines: None,
            returned_matches: None,
            total_matches: None,
            returned_entries: None,
            total_entries: None,
            changed_files: Vec::new(),
            receipt: None,
            details: Value::Null,
        }
    }
}

impl ToolResultMeta {
    fn to_model_value(&self) -> Option<Value> {
        let mut object = Map::new();
        insert_u64(&mut object, "duration_ms", self.duration_ms);
        insert_i32(&mut object, "exit_code", self.exit_code);
        insert_u64(&mut object, "stdout_bytes", self.stdout_bytes);
        insert_u64(&mut object, "stderr_bytes", self.stderr_bytes);
        insert_u64(&mut object, "bytes", self.bytes);
        if self.truncated {
            object.insert("truncated".to_owned(), Value::Bool(true));
        }
        insert_u64(&mut object, "omitted_bytes", self.omitted_bytes);
        insert_u64(&mut object, "limit_bytes", self.limit_bytes);
        insert_u64(&mut object, "limit_lines", self.limit_lines);
        insert_u64(&mut object, "returned_bytes", self.returned_bytes);
        insert_u64(&mut object, "returned_lines", self.returned_lines);
        insert_u64(&mut object, "total_bytes", self.total_bytes);
        insert_u64(&mut object, "total_lines", self.total_lines);
        insert_u64(&mut object, "returned_matches", self.returned_matches);
        insert_u64(&mut object, "total_matches", self.total_matches);
        insert_u64(&mut object, "returned_entries", self.returned_entries);
        insert_u64(&mut object, "total_entries", self.total_entries);
        if !self.changed_files.is_empty() {
            object.insert(
                "changed_files".to_owned(),
                Value::Array(
                    self.changed_files
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        if !value_is_empty(&self.details) {
            object.insert("details".to_owned(), model_visible_details(&self.details));
        }
        (!object.is_empty()).then_some(Value::Object(object))
    }
}

fn model_visible_details(value: &Value) -> Value {
    const MODEL_DETAIL_STRING_LIMIT: usize = 4096;
    const MODEL_DETAIL_STRING_PREVIEW: usize = 240;

    fn omitted_string_metadata(text: &str, reason: &str, include_preview: bool) -> Value {
        let mut object = Map::new();
        object.insert("omitted".to_owned(), Value::Bool(true));
        object.insert("reason".to_owned(), Value::String(reason.to_owned()));
        object.insert(
            "bytes".to_owned(),
            Value::Number((text.len() as u64).into()),
        );
        object.insert(
            "chars".to_owned(),
            Value::Number((text.chars().count() as u64).into()),
        );
        object.insert(
            "lines".to_owned(),
            Value::Number((text.lines().count() as u64).into()),
        );
        if include_preview {
            object.insert(
                "preview".to_owned(),
                Value::String(text.chars().take(MODEL_DETAIL_STRING_PREVIEW).collect()),
            );
        }
        Value::Object(object)
    }

    fn sanitize(key: Option<&str>, value: &Value) -> Value {
        match value {
            Value::String(text) if key == Some("output_preview") => {
                omitted_string_metadata(text, "ui_artifact_only", false)
            }
            Value::String(text) if text.len() > MODEL_DETAIL_STRING_LIMIT => {
                omitted_string_metadata(text, "model_context_limit", true)
            }
            Value::Array(values) => {
                Value::Array(values.iter().map(|value| sanitize(None, value)).collect())
            }
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), sanitize(Some(key.as_str()), value)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }

    sanitize(None, value)
}

fn insert_i32(object: &mut Map<String, Value>, key: &str, value: Option<i32>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::Number(value.into()));
    }
}

fn insert_u64(object: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::Number(value.into()));
    }
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

/// Exact provider-neutral identity for resources owned by one registered tool lifecycle.
///
/// The namespace identifies the owning subsystem, `scope` retains its exact resource identity, and
/// `generation` distinguishes concurrent or replacement lifecycles inside that scope.
/// Provider-visible tool names must not be used as lifecycle identities because they may be
/// sanitized, truncated, or hashed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolLifecycleOwner {
    namespace: String,
    scope: String,
    generation: String,
}

impl ToolLifecycleOwner {
    #[must_use]
    pub fn new(
        namespace: impl Into<String>,
        scope: impl Into<String>,
        generation: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            scope: scope.into(),
            generation: generation.into(),
        }
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    #[must_use]
    pub fn belongs_to(&self, namespace: &str, scope: &str) -> bool {
        self.namespace == namespace && self.scope == scope
    }
}

fn declared_permission_effects(
    access: ToolAccess,
    operation: ToolOperation,
    network_effect: Option<NetworkEffect>,
) -> BTreeSet<ToolPermissionEffect> {
    let mut effects = BTreeSet::new();
    match access {
        ToolAccess::Read => {
            effects.insert(ToolPermissionEffect::FileRead);
        }
        ToolAccess::Write => {
            effects.insert(ToolPermissionEffect::FileWrite);
        }
        ToolAccess::Execute => {
            effects.insert(match operation {
                ToolOperation::ExecuteReadOnlyCommand => ToolPermissionEffect::ExecuteTrustedBinary,
                ToolOperation::ExecuteWorkspaceCheckCommand => {
                    ToolPermissionEffect::ExecuteWorkspaceCode
                }
                ToolOperation::SendTerminalInput
                | ToolOperation::ResizeTerminalTask
                | ToolOperation::CancelTerminalTask => ToolPermissionEffect::ProcessControl,
                _ => ToolPermissionEffect::Unknown,
            });
        }
    }
    if matches!(
        operation,
        ToolOperation::DeleteFile
            | ToolOperation::DeleteDirectory
            | ToolOperation::RecursiveDelete
            | ToolOperation::ExecuteDestructiveCommand
    ) {
        effects.insert(ToolPermissionEffect::FileDelete);
    }
    match network_effect {
        Some(NetworkEffect::Read) => {
            effects.insert(ToolPermissionEffect::NetworkRead);
        }
        Some(NetworkEffect::Mutate) => {
            effects.insert(ToolPermissionEffect::NetworkMutate);
        }
        Some(NetworkEffect::Unknown) => {
            effects.insert(ToolPermissionEffect::NetworkUnknown);
        }
        None => {}
    }
    effects
}

/// Complete deterministic facts used by typed tools with a single declared permission plan.
#[derive(Debug, Clone)]
pub struct DeclaredToolPermissionFacts {
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub network_effect: Option<NetworkEffect>,
    pub subjects: Vec<ToolSubject>,
    pub tool_default_mode: Option<ApprovalMode>,
}

/// Builds one V2 draft from a typed tool's already-decoded facts.
///
/// Dynamic tools should parse their arguments once, construct this value, and avoid exposing
/// parallel permission methods that can drift or repeat argument analysis.
pub fn declared_tool_permission_plan(
    spec: &ToolSpec,
    args: &Value,
    facts: DeclaredToolPermissionFacts,
) -> Result<ToolPermissionPlanDraft> {
    let mut semantic_scope =
        crate::ToolSemanticScope::new(format!("{}:{}", spec.name, facts.operation.as_str()), 1);
    semantic_scope.qualifiers.insert(
        "args_sha256".to_owned(),
        crate::event::canonical_json_content_hash(args)?,
    );
    Ok(ToolPermissionPlanDraft {
        access: facts.access,
        operation: facts.operation,
        effects: declared_permission_effects(facts.access, facts.operation, facts.network_effect),
        subjects: facts.subjects,
        analysis: ToolAnalysisStatus::Complete,
        containment: ExecutionContainmentRequest::default(),
        semantic_scope: Some(semantic_scope),
        tool_default_mode: facts.tool_default_mode,
        analysis_bindings: BTreeMap::from([("planner".to_owned(), "tool_declared_v2".to_owned())]),
        safe_summary: ToolPermissionSummary {
            title: spec.name.clone(),
            detail: format!("{} operation", facts.operation.as_str()),
            step_count: 1,
            workspace_code_steps: u32::from(matches!(
                facts.operation,
                ToolOperation::ExecuteWorkspaceCheckCommand
            )),
        },
    })
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's stable contract and JSON Schema surface.
    fn spec(&self) -> ToolSpec;

    /// Declares how this tool records workspace mutation evidence.
    fn mutation_tracking(&self) -> ToolMutationTracking {
        default_tool_mutation_tracking(&self.spec())
    }

    /// Declares whether an exactly analyzed read-only call may enter the bounded parallel lane.
    ///
    /// The default is fail-closed. The scheduler still checks the concrete permission plan and
    /// mutation contract, so this opt-in can never widen an analyzed call's effects.
    fn concurrency_class(&self) -> ToolConcurrencyClass {
        ToolConcurrencyClass::Exclusive
    }

    /// Declares the runtime-only reconciliation/replay contract for interrupted effects.
    ///
    /// The default is fail-closed. A tool may not become replayable merely because it is run
    /// under `danger-full-access` or because the enclosing task was otherwise read-only.
    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::non_replayable()
    }

    /// Declares semantic abilities used by task admission.
    ///
    /// The default is deliberately conservative and derives only unambiguous abilities from the
    /// provider-neutral tool spec. Specialized tools such as VCS inspection or artifact readers
    /// must opt in explicitly.
    fn capabilities(&self) -> BTreeSet<ToolCapability> {
        let spec = self.spec();
        let mut capabilities = BTreeSet::new();
        match (spec.category, spec.access) {
            (ToolCategory::File | ToolCategory::Search, ToolAccess::Read) => {
                capabilities.insert(ToolCapability::WorkspaceRead);
            }
            (ToolCategory::File, ToolAccess::Write) => {
                capabilities.insert(ToolCapability::WorkspaceRead);
                capabilities.insert(ToolCapability::WorkspaceWrite);
            }
            (ToolCategory::Shell, ToolAccess::Execute) => {
                capabilities.insert(ToolCapability::ProcessExecute);
            }
            _ => {}
        }
        if spec.network_effect == Some(NetworkEffect::Read) {
            capabilities.insert(ToolCapability::NetworkRead);
        }
        capabilities
    }

    /// Shuts down lifecycle resources owned by this registered tool generation.
    ///
    /// Stateless tools use the default no-op. Long-lived process or transport tools override this
    /// hook so registry replacement can prove the retired generation has stopped before reporting
    /// success.
    ///
    /// # Errors
    ///
    /// Returns an error when owned lifecycle resources cannot be shut down completely.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    /// Returns the exact lifecycle owner for long-lived resources held by this tool.
    ///
    /// Stateless tools return `None`. All tools backed by the same process or transport generation
    /// return the same lossless owner so registry replacement can retire them atomically.
    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        None
    }

    /// Produces all deterministic permission facts for one concrete call in one invocation.
    ///
    /// The default binds the tool's explicit access, operation, subjects, network effect and
    /// default policy into one deterministic V2 plan. Shell-like or dynamically scoped tools must
    /// override this method when those declarations do not completely describe a concrete call.
    ///
    /// # Errors
    ///
    /// Returns an error when arguments are invalid or stable permission facts cannot be derived.
    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let spec = self.spec();
        let access = spec.access;
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access,
                operation: infer_tool_operation(&spec.name, access),
                network_effect: spec.network_effect,
                subjects: Vec::new(),
                tool_default_mode: None,
            },
        )
    }

    /// Returns a safe, bounded audit summary for one outbound tool call.
    ///
    /// This hook is evaluated after permission approval and before execution. The returned
    /// payload is written to durable control state, so implementations must not include raw
    /// secrets, large user content, or unbounded remote payloads.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable egress summary can be
    /// derived.
    fn egress_audit(&self, _ctx: &ToolContext, _args: &Value) -> Result<Option<ToolEgressAudit>> {
        Ok(None)
    }

    /// Produces an optional approval preview for the given tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when preview materialization fails and the caller should surface
    /// that failure instead of silently fabricating a preview.
    async fn preview(&self, _ctx: ToolContext, _args: Value) -> Result<Option<ToolPreview>> {
        Ok(None)
    }

    /// Materializes an immutable, one-shot artifact before permission evaluation.
    ///
    /// Tools whose exact mutation subjects are known only after an external planner response use
    /// this hook. The returned subjects replace the coarse pre-plan permission subjects. The
    /// default keeps existing tools on the ordinary preview/execute path.
    async fn prepare(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: Value,
    ) -> Result<Option<ToolPreparation>> {
        Ok(None)
    }

    /// Executes one approval-bound artifact without replanning or re-querying its source.
    ///
    /// # Errors
    ///
    /// The default fails closed because a tool that returns [`ToolPreparation`] must explicitly
    /// implement the matching one-shot execution path.
    async fn execute_prepared(
        &self,
        _ctx: ToolContext,
        _args: Value,
        _prepared: PreparedToolExecution,
    ) -> Result<ToolResult> {
        bail!("tool does not implement prepared execution")
    }

    /// Executes the tool call within the provided workspace context.
    ///
    /// # Errors
    ///
    /// Returns an error when arguments are invalid or the underlying tool action fails before
    /// it can be expressed as a structured [`ToolResult`].
    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult>;
}

/// Runtime registry for built-in and remote tools.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<BTreeMap<String, RegisteredTool>>>,
    run_input_preparer: Arc<RwLock<Option<RunInputPreparerBinding>>>,
    scope: Option<Arc<ToolRegistryScope>>,
    deny_scope: Option<Arc<ToolRegistryScope>>,
}

#[derive(Clone)]
struct RegisteredTool {
    tool: Arc<dyn Tool>,
    invocation_gate: Arc<ToolInvocationGate>,
}

impl RegisteredTool {
    fn new(tool: Arc<dyn Tool>) -> Self {
        Self {
            tool,
            invocation_gate: Arc::new(ToolInvocationGate::default()),
        }
    }

    fn acquire(&self) -> Result<Arc<ToolInvocationLease>> {
        self.invocation_gate.acquire()
    }
}

#[derive(Default)]
struct ToolInvocationGate {
    state: Mutex<ToolInvocationGateState>,
    notify: Notify,
}

#[derive(Default)]
struct ToolInvocationGateState {
    active: usize,
    retired: bool,
}

impl ToolInvocationGate {
    fn acquire(self: &Arc<Self>) -> Result<Arc<ToolInvocationLease>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            bail!("registered tool generation is retiring");
        }
        state.active = state.active.saturating_add(1);
        Ok(Arc::new(ToolInvocationLease {
            gate: Arc::clone(self),
        }))
    }

    fn retire(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired = true;
    }

    async fn wait_quiescent(&self) {
        loop {
            let notified = self.notify.notified();
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                == 0
            {
                return;
            }
            notified.await;
        }
    }
}

struct ToolInvocationLease {
    gate: Arc<ToolInvocationGate>,
}

impl Drop for ToolInvocationLease {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
        let quiescent = state.active == 0;
        drop(state);
        if quiescent {
            self.gate.notify.notify_waiters();
        }
    }
}

/// Exact removed tool generation that can be shut down only after active invocations drain.
pub struct ToolLifecycleRetirement {
    registrations: Vec<RegisteredTool>,
}

impl ToolLifecycleRetirement {
    #[must_use]
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.registrations
            .iter()
            .map(|registration| Arc::clone(&registration.tool))
            .collect()
    }

    /// Waits for every exact pinned invocation and then shuts down each resource owner once.
    pub async fn dispose_and_quiesce(&self) -> Result<()> {
        for registration in &self.registrations {
            registration.invocation_gate.wait_quiescent().await;
        }
        let mut attempted_owners = BTreeSet::new();
        let mut failures = Vec::new();
        for registration in &self.registrations {
            if let Some(owner) = registration.tool.lifecycle_owner()
                && !attempted_owners.insert(owner)
            {
                continue;
            }
            if let Err(error) = registration.tool.shutdown().await {
                failures.push(format!(
                    "failed to shut down registered tool {}: {error:#}",
                    registration.tool.spec().name
                ));
            }
        }
        if !failures.is_empty() {
            bail!(failures.join("; "));
        }
        Ok(())
    }
}

/// Exact registered tool generation pinned for one authorization/execution lifecycle.
#[derive(Clone)]
pub(crate) struct ResolvedToolInvocation {
    tool: Arc<dyn Tool>,
    pub(crate) contract: ToolRuntimeContract,
    _invocation_lease: Arc<ToolInvocationLease>,
}

impl std::fmt::Debug for ResolvedToolInvocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedToolInvocation")
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

impl ResolvedToolInvocation {
    pub(crate) async fn prepare(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreparationDraft>> {
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let args_digest = prepared_args_digest(&args)?;
        let Some(preparation) = self
            .tool
            .prepare(ctx, call.id.clone(), args.clone())
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(ToolPreparationDraft {
            tool: Arc::clone(&self.tool),
            args,
            preparation,
            call_id: call.id,
            tool_name: call.name,
            args_digest,
        }))
    }

    pub(crate) fn permission_plan(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
        prepared_subjects: Option<&[ToolSubject]>,
    ) -> Result<ToolPermissionPlanV2> {
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let mut draft = self.tool.permission_plan(ctx, &args)?;
        if let Some(subjects) = prepared_subjects {
            draft.subjects = subjects.to_vec();
        }
        ToolPermissionPlanV2::bind(&call.name, &args, &ctx.workspace_root, draft)
    }

    pub(crate) fn egress_audit(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ToolEgressAudit>> {
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        self.tool.egress_audit(ctx, &args)
    }

    pub(crate) async fn preview(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreview>> {
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        self.tool.preview(ctx, args).await
    }

    pub(crate) fn execution_mutation_profile(
        &self,
        ctx: &ToolContext,
        call_id: &str,
    ) -> Result<Option<ExecutionMutationProfile>> {
        execution_mutation_profile_for_tool(
            ctx,
            &self.contract.spec,
            self.contract.mutation_tracking,
            call_id,
        )
    }

    pub(crate) async fn execute_after_started_audit(
        &self,
        registry: &ToolRegistry,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        let ctx = ctx.with_execution_mutation_profile_recorded(call.id.clone());
        let workspace_frontier = registry.validate_agent_invocation_grant(&ctx)?;
        ensure_execution_mutation_profile_recorded(
            &ctx,
            &self.contract.spec,
            self.contract.mutation_tracking,
            &call.id,
        )?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let mutation_scan = begin_unknown_mutation_scan(&ctx, self.contract.mutation_tracking)
            .map_err(|error| {
                anyhow!(
                    "failed to start workspace mutation detection for {}: {error:#}",
                    self.contract.spec.name
                )
            })?;
        let call_id = call.id;
        let result = self.tool.execute(ctx.clone(), call_id.clone(), args).await;
        let audited_workspace_scan =
            finish_unknown_mutation_scan(&ctx, &self.contract.spec, &call_id, mutation_scan)
                .map_err(|error| unknown_mutation_scan_finish_error(&self.contract.spec, error))?;
        finish_agent_invocation_workspace_effect(
            &ctx,
            &self.contract.spec,
            &call_id,
            self.contract.mutation_tracking,
            workspace_frontier,
            audited_workspace_scan,
        )?;
        result
    }
}

#[derive(Clone)]
struct RunInputPreparerBinding {
    preparer: Arc<dyn crate::AgentRunInputPreparer>,
    required_tools: Option<ToolRegistryScope>,
}

/// Non-owning handle used by tools that need to mutate their containing registry.
///
/// Keeping this handle inside a registered tool does not create a registry-to-tool ownership
/// cycle. It upgrades only while some external [`ToolRegistry`] owner remains alive.
#[derive(Clone)]
pub struct WeakToolRegistry {
    tools: Weak<RwLock<BTreeMap<String, RegisteredTool>>>,
    run_input_preparer: Weak<RwLock<Option<RunInputPreparerBinding>>>,
    scope: Option<Arc<ToolRegistryScope>>,
    deny_scope: Option<Arc<ToolRegistryScope>>,
}

/// Strong role-specific view over a shared tool registry.
#[derive(Clone)]
pub struct ScopedToolRegistry {
    inner: ToolRegistry,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: Arc::new(RwLock::new(BTreeMap::new())),
            run_input_preparer: Arc::new(RwLock::new(None)),
            scope: None,
            deny_scope: None,
        }
    }
}

impl ToolRegistry {
    /// Creates an empty tool registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a non-owning handle that can recover this registry while an external owner lives.
    pub fn downgrade(&self) -> WeakToolRegistry {
        WeakToolRegistry {
            tools: Arc::downgrade(&self.tools),
            run_input_preparer: Arc::downgrade(&self.run_input_preparer),
            scope: self.scope.clone(),
            deny_scope: self.deny_scope.clone(),
        }
    }

    /// Freezes the currently visible tool implementations into an independent registry map.
    ///
    /// The tool implementations remain shared `Arc`s, but later registration or replacement in
    /// the source registry cannot change this snapshot's resolved contracts. Child-agent
    /// admission uses this to bind its ToolSpec proof to the tools it will actually execute.
    pub fn snapshot(&self) -> Self {
        let visible_tools = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            tools
                .iter()
                .filter(|(name, _)| self.allows(name))
                .map(|(name, registration)| (name.clone(), registration.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let input_preparer = {
            let preparer = match self.run_input_preparer.read() {
                Ok(preparer) => preparer,
                Err(poisoned) => poisoned.into_inner(),
            };
            preparer.clone()
        };
        Self {
            tools: Arc::new(RwLock::new(visible_tools)),
            run_input_preparer: Arc::new(RwLock::new(input_preparer)),
            scope: None,
            deny_scope: None,
        }
    }

    /// Registers one tool by its stable spec name, replacing any prior entry with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name.clone();
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools.insert(name, RegisteredTool::new(tool));
    }

    /// Attaches one runtime-owned per-run input resolver shared by every scoped registry view.
    pub fn set_run_input_preparer(&mut self, preparer: Arc<dyn crate::AgentRunInputPreparer>) {
        let mut slot = match self.run_input_preparer.write() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = Some(RunInputPreparerBinding {
            preparer,
            required_tools: None,
        });
    }

    /// Attaches a per-run input resolver only while at least one matching capability tool is
    /// visible through the effective registry scope.
    ///
    /// Runtime capabilities that alter provider requests must not survive a role or purpose scope
    /// that removed their model-visible tool. The binding is evaluated against both the current
    /// tool generation and the effective allow/deny scopes each time a run starts.
    pub fn set_run_input_preparer_for_tools(
        &mut self,
        preparer: Arc<dyn crate::AgentRunInputPreparer>,
        required_tools: ToolRegistryScope,
    ) {
        let mut slot = match self.run_input_preparer.write() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = Some(RunInputPreparerBinding {
            preparer,
            required_tools: Some(required_tools),
        });
    }

    pub(crate) fn run_input_preparer(&self) -> Option<Arc<dyn crate::AgentRunInputPreparer>> {
        let slot = match self.run_input_preparer.read() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        let binding = slot.as_ref()?;
        if let Some(required_tools) = binding.required_tools.as_ref() {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !tools
                .keys()
                .any(|name| required_tools.allows(name) && self.allows(name))
            {
                return None;
            }
        }
        Some(Arc::clone(&binding.preparer))
    }

    /// Returns a role-scoped registry sharing the same underlying tool map.
    pub fn scoped(&self, scope: ToolRegistryScope) -> ScopedToolRegistry {
        ScopedToolRegistry {
            inner: Self {
                tools: Arc::clone(&self.tools),
                run_input_preparer: Arc::clone(&self.run_input_preparer),
                scope: Some(Arc::new(self.effective_scope(scope))),
                deny_scope: self.deny_scope.clone(),
            },
        }
    }

    /// Returns a scoped registry that also denies matching tool names across all tool paths.
    pub fn scoped_with_denies(
        &self,
        scope: ToolRegistryScope,
        deny_scope: ToolRegistryScope,
    ) -> ScopedToolRegistry {
        ScopedToolRegistry {
            inner: Self {
                tools: Arc::clone(&self.tools),
                run_input_preparer: Arc::clone(&self.run_input_preparer),
                scope: Some(Arc::new(self.effective_scope(scope))),
                deny_scope: self.effective_deny_scope(deny_scope).map(Arc::new),
            },
        }
    }

    fn effective_scope(&self, scope: ToolRegistryScope) -> ToolRegistryScope {
        match self.scope.as_deref() {
            Some(existing) => existing.intersection(&scope),
            None => scope,
        }
    }

    fn effective_deny_scope(&self, deny_scope: ToolRegistryScope) -> Option<ToolRegistryScope> {
        let effective = match self.deny_scope.as_deref() {
            Some(existing) => existing.union(&deny_scope),
            None => deny_scope,
        };
        (!effective.is_empty()).then_some(effective)
    }

    /// Removes registered tools whose names start with the provided prefix.
    ///
    /// Returns the number of removed tools.
    pub fn unregister_by_name_prefix(&mut self, prefix: &str) -> usize {
        self.drain_by_name_prefix(prefix).len()
    }

    /// Removes and returns registered tools whose names start with the provided prefix.
    pub fn drain_by_name_prefix(&mut self, prefix: &str) -> Vec<Arc<dyn Tool>> {
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        let names = tools
            .keys()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        names
            .into_iter()
            .filter_map(|name| tools.remove(&name))
            .map(|registration| {
                registration.invocation_gate.retire();
                registration.tool
            })
            .collect()
    }

    /// Removes and returns tools belonging to one exact, opaque lifecycle owner.
    pub fn drain_by_lifecycle_owner(&mut self, owner: &ToolLifecycleOwner) -> Vec<Arc<dyn Tool>> {
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        let names = tools
            .iter()
            .filter(|(_, registration)| registration.tool.lifecycle_owner().as_ref() == Some(owner))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names
            .into_iter()
            .filter_map(|name| tools.remove(&name))
            .map(|registration| {
                registration.invocation_gate.retire();
                registration.tool
            })
            .collect()
    }

    /// Removes an exact lifecycle generation and returns its quiescent disposal owner.
    pub fn retire_by_lifecycle_owner(
        &mut self,
        owner: &ToolLifecycleOwner,
    ) -> ToolLifecycleRetirement {
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        let names = tools
            .iter()
            .filter(|(_, registration)| registration.tool.lifecycle_owner().as_ref() == Some(owner))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let registrations = names
            .into_iter()
            .filter_map(|name| tools.remove(&name))
            .inspect(|registration| registration.invocation_gate.retire())
            .collect();
        ToolLifecycleRetirement { registrations }
    }

    /// Returns distinct lifecycle generations registered for one exact opaque scope.
    pub fn lifecycle_owners_by_scope(
        &self,
        namespace: &str,
        scope: &str,
    ) -> Vec<ToolLifecycleOwner> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools
            .values()
            .filter_map(|registration| registration.tool.lifecycle_owner())
            .filter(|owner| owner.belongs_to(namespace, scope))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Returns provider-visible names owned by one exact lifecycle generation.
    #[must_use]
    pub fn tool_names_by_lifecycle_owner(&self, owner: &ToolLifecycleOwner) -> Vec<String> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools
            .iter()
            .filter(|(name, registration)| {
                self.allows(name) && registration.tool.lifecycle_owner().as_ref() == Some(owner)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Returns the full list of registered tool specifications.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools
            .values()
            .filter_map(|registration| {
                let spec = registration.tool.spec();
                self.allows(&spec.name).then_some(spec)
            })
            .collect()
    }

    /// Returns the resolved contract and mutation evidence strategy for every visible tool.
    ///
    /// Admission checks use this instead of tool-name allowlists so a same-name replacement with
    /// broader effects cannot inherit read-only trust from the replaced implementation.
    pub fn contracts(&self) -> Vec<ToolRuntimeContract> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools
            .values()
            .filter_map(|registration| {
                let spec = registration.tool.spec();
                self.allows(&spec.name).then(|| ToolRuntimeContract {
                    spec,
                    mutation_tracking: registration.tool.mutation_tracking(),
                    concurrency_class: registration.tool.concurrency_class(),
                    capabilities: registration.tool.capabilities(),
                    replay_contract: registration.tool.replay_contract(),
                })
            })
            .collect()
    }

    /// Computes the exact visible tool-contract fingerprint used by invocation grants.
    ///
    /// # Errors
    ///
    /// Returns an error when a tool specification cannot be serialized.
    pub fn contract_fingerprint(&self) -> Result<String> {
        let contracts = self
            .contracts()
            .into_iter()
            .map(|contract| {
                contract.replay_contract.validate()?;
                Ok(json!({
                    "spec": contract.spec,
                    "mutation_tracking": match contract.mutation_tracking {
                        ToolMutationTracking::None => "none",
                        ToolMutationTracking::Controlled => "controlled",
                        ToolMutationTracking::Unknown => "unknown",
                    },
                    "concurrency_class": match contract.concurrency_class {
                        ToolConcurrencyClass::Exclusive => "exclusive",
                        ToolConcurrencyClass::ParallelReadOnly => "parallel_read_only",
                    },
                    "capabilities": contract
                        .capabilities
                        .iter()
                        .map(|capability| capability.as_str())
                        .collect::<Vec<_>>(),
                    "replay_contract": contract.replay_contract,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let encoded = serde_json::to_vec(&contracts)?;
        let mut hasher = Sha256::new();
        hasher.update(encoded);
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub(crate) fn resolve_invocation(&self, name: &str) -> Result<ResolvedToolInvocation> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        let registration = self.allowed_registration(&tools, name)?;
        let tool = Arc::clone(&registration.tool);
        let replay_contract = tool.replay_contract();
        replay_contract.validate()?;
        Ok(ResolvedToolInvocation {
            contract: ToolRuntimeContract {
                spec: tool.spec(),
                mutation_tracking: tool.mutation_tracking(),
                concurrency_class: tool.concurrency_class(),
                capabilities: tool.capabilities(),
                replay_contract,
            },
            tool,
            _invocation_lease: registration.acquire()?,
        })
    }

    pub(crate) fn invocation_generation_is_current(
        &self,
        invocation: &ResolvedToolInvocation,
    ) -> bool {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.allowed_tool(&tools, &invocation.contract.spec.name)
            .is_ok_and(|current| Arc::ptr_eq(&current, &invocation.tool))
    }

    /// Returns one registered spec by name.
    pub fn spec_for(&self, name: &str) -> Option<ToolSpec> {
        if !self.allows(name) {
            return None;
        }
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools.get(name).map(|registration| registration.tool.spec())
    }

    /// Executes a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool fails.
    pub async fn execute(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        let invocation = self.resolve_invocation(&call.name)?;
        let tool = Arc::clone(&invocation.tool);
        let spec = invocation.contract.spec.clone();
        let mutation_tracking = invocation.contract.mutation_tracking;
        let workspace_frontier = self.validate_agent_invocation_grant(&ctx)?;
        ensure_execution_mutation_profile_recorded(&ctx, &spec, mutation_tracking, &call.id)?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let mutation_scan =
            begin_unknown_mutation_scan(&ctx, mutation_tracking).map_err(|error| {
                anyhow!(
                    "failed to start workspace mutation detection for {}: {error:#}",
                    spec.name
                )
            })?;
        let call_id = call.id;
        let result = tool.execute(ctx.clone(), call_id.clone(), args).await;
        let audited_workspace_scan =
            finish_unknown_mutation_scan(&ctx, &spec, &call_id, mutation_scan)
                .map_err(|error| unknown_mutation_scan_finish_error(&spec, error))?;
        finish_agent_invocation_workspace_effect(
            &ctx,
            &spec,
            &call_id,
            mutation_tracking,
            workspace_frontier,
            audited_workspace_scan,
        )?;
        result
    }

    /// Executes a tool call after the caller has persisted the corresponding started audit.
    ///
    /// Callers that execute unknown-side-effect tools with a mutation recorder must first append
    /// `ToolExecutionStarted` carrying `ExecutionMutationProfile`; this wrapper marks that
    /// precondition for the low-level registry guard.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool fails.
    pub async fn execute_after_started_audit(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        let ctx = ctx.with_execution_mutation_profile_recorded(call.id.clone());
        self.execute(ctx, call).await
    }

    /// Executes an approval-bound prepared artifact after the caller persists started audit.
    ///
    /// The prepared call is consumed by value. Any registry generation, call identity, argument,
    /// subject, or approval binding mismatch fails with `stale_prepared_mutation` before mutation.
    pub(crate) async fn execute_prepared_after_started_audit(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
        prepared: PreparedToolCall,
        current_policy_fingerprint: &str,
        current_approval_identity: &str,
    ) -> Result<ToolResult> {
        let ctx = ctx.with_execution_mutation_profile_recorded(call.id.clone());
        let mismatch = if prepared.binding.policy_fingerprint != current_policy_fingerprint {
            Some("policy_changed_after_approval")
        } else if prepared.binding.approval_identity != current_approval_identity {
            Some("approval_authority_changed")
        } else {
            None
        };
        if let Some(reason) = mismatch {
            return Ok(stale_prepared_tool_result(
                &call,
                prepared.prepared_digest(),
                reason,
            ));
        }
        self.execute_prepared(ctx, call, prepared).await
    }

    /// Executes one prepared tool call by consuming its immutable approval-bound artifact.
    async fn execute_prepared(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
        prepared: PreparedToolCall,
    ) -> Result<ToolResult> {
        let (current_tool, _invocation_lease) = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            let registration = self.allowed_registration(&tools, &call.name)?;
            (Arc::clone(&registration.tool), registration.acquire()?)
        };
        let spec = current_tool.spec();
        let mutation_tracking = current_tool.mutation_tracking();
        let workspace_frontier = self.validate_agent_invocation_grant(&ctx)?;
        ensure_execution_mutation_profile_recorded(&ctx, &spec, mutation_tracking, &call.id)?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let observed_args_digest = prepared_args_digest(&args)?;
        let mismatch = if call.id != prepared.binding.call_id {
            Some("call_id_changed")
        } else if call.name != prepared.binding.tool_name {
            Some("tool_name_changed")
        } else if observed_args_digest != prepared.binding.args_digest || args != prepared.args {
            Some("args_changed_after_preview")
        } else if ctx.approved_subjects() != prepared.binding.subjects.as_slice() {
            Some("approved_subjects_changed")
        } else if !Arc::ptr_eq(&current_tool, &prepared.tool) {
            Some("registered_tool_generation_changed")
        } else {
            None
        };
        if let Some(reason) = mismatch {
            return Ok(stale_prepared_tool_result(
                &call,
                prepared.prepared_digest(),
                reason,
            ));
        }

        let mutation_scan =
            begin_unknown_mutation_scan(&ctx, mutation_tracking).map_err(|error| {
                anyhow!(
                    "failed to start workspace mutation detection for {}: {error:#}",
                    spec.name
                )
            })?;
        let call_id = call.id;
        let result = current_tool
            .execute_prepared(ctx.clone(), args, prepared.into_execution())
            .await;
        let audited_workspace_scan =
            finish_unknown_mutation_scan(&ctx, &spec, &call_id, mutation_scan)
                .map_err(|error| unknown_mutation_scan_finish_error(&spec, error))?;
        finish_agent_invocation_workspace_effect(
            &ctx,
            &spec,
            &call_id,
            mutation_tracking,
            workspace_frontier,
            audited_workspace_scan,
        )?;
        result
    }

    fn validate_agent_invocation_grant(
        &self,
        ctx: &ToolContext,
    ) -> Result<Option<crate::WorkspaceSnapshotId>> {
        let Some(grant) = ctx.agent_invocation_grant.as_ref() else {
            return Ok(None);
        };
        let cancellation = ctx.cancellation.as_ref().ok_or_else(|| {
            anyhow!("child tool execution is missing its root cancellation scope")
        })?;
        if cancellation.is_cancel_requested() {
            bail!("root run cancelled before child tool execution");
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let workspace_snapshot_id =
            crate::agent_invocation_workspace_snapshot_id(&ctx.workspace_root)?;
        grant.validate_tool_effect(
            &self.contract_fingerprint()?,
            &workspace_snapshot_id,
            cancellation.scope_id(),
            now_ms,
        )?;
        Ok(Some(workspace_snapshot_id))
    }

    /// Returns the mutation profile that must be persisted before executing this tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the workspace snapshot cannot be captured.
    pub fn execution_mutation_profile(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ExecutionMutationProfile>> {
        let (spec, mutation_tracking) = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            let tool = self.allowed_tool(&tools, &call.name)?;
            (tool.spec(), tool.mutation_tracking())
        };
        execution_mutation_profile_for_tool(ctx, &spec, mutation_tracking, &call.id)
    }

    /// Builds a preview for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or preview
    /// generation itself fails.
    pub async fn preview(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreview>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.preview(ctx, args).await
    }

    /// Materializes a one-shot tool artifact before permission evaluation.
    ///
    /// The draft's exact subjects must replace any coarse pre-plan subjects when constructing the
    /// permission decision. Returning `None` leaves the tool on the ordinary preview path.
    pub async fn prepare(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreparationDraft>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let args_digest = prepared_args_digest(&args)?;
        let Some(preparation) = tool.prepare(ctx, call.id.clone(), args.clone()).await? else {
            return Ok(None);
        };
        Ok(Some(ToolPreparationDraft {
            tool,
            args,
            preparation,
            call_id: call.id,
            tool_name: call.name,
            args_digest,
        }))
    }

    /// Builds one immutable V2 permission plan after decoding arguments exactly once.
    pub fn permission_plan(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<ToolPermissionPlanV2> {
        self.permission_plan_with_subjects(ctx, call, None)
    }

    /// Builds one immutable plan while replacing coarse subjects with a prepared artifact's exact
    /// subjects before the plan hash is calculated.
    pub fn permission_plan_with_subjects(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
        prepared_subjects: Option<&[ToolSubject]>,
    ) -> Result<ToolPermissionPlanV2> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let mut draft = tool.permission_plan(ctx, &args)?;
        if let Some(subjects) = prepared_subjects {
            draft.subjects = subjects.to_vec();
        }
        ToolPermissionPlanV2::bind(&call.name, &args, &ctx.workspace_root, draft)
    }

    /// Returns a safe outbound audit summary for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the JSON arguments are invalid.
    pub fn egress_audit(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ToolEgressAudit>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.egress_audit(ctx, &args)
    }

    fn allows(&self, name: &str) -> bool {
        self.scope.as_ref().is_none_or(|scope| scope.allows(name))
            && self
                .deny_scope
                .as_ref()
                .is_none_or(|scope| !scope.allows(name))
    }

    fn allowed_tool(
        &self,
        tools: &BTreeMap<String, RegisteredTool>,
        name: &str,
    ) -> Result<Arc<dyn Tool>> {
        Ok(Arc::clone(&self.allowed_registration(tools, name)?.tool))
    }

    fn allowed_registration<'a>(
        &self,
        tools: &'a BTreeMap<String, RegisteredTool>,
        name: &str,
    ) -> Result<&'a RegisteredTool> {
        if !self.allows(name) {
            return Err(anyhow!("tool {name} is not available in this role scope"));
        }
        tools
            .get(name)
            .ok_or_else(|| anyhow!("unknown tool {name}"))
    }
}

impl WeakToolRegistry {
    /// Recovers an owning registry handle, or returns `None` after the registry has been released.
    pub fn upgrade(&self) -> Option<ToolRegistry> {
        Some(ToolRegistry {
            tools: self.tools.upgrade()?,
            run_input_preparer: self.run_input_preparer.upgrade()?,
            scope: self.scope.clone(),
            deny_scope: self.deny_scope.clone(),
        })
    }
}

impl ScopedToolRegistry {
    /// Returns this scoped registry as the standard registry type used by the agent loop.
    pub fn into_registry(self) -> ToolRegistry {
        self.inner
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.inner.specs()
    }

    pub fn spec_for(&self, name: &str) -> Option<ToolSpec> {
        self.inner.spec_for(name)
    }

    /// Executes a scoped tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is outside the role scope, unknown, or fails.
    pub async fn execute(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        self.inner.execute(ctx, call).await
    }

    /// Builds a scoped approval preview.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is outside the role scope, unknown, or preview fails.
    pub async fn preview(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreview>> {
        self.inner.preview(ctx, call).await
    }

    pub async fn prepare(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreparationDraft>> {
        self.inner.prepare(ctx, call).await
    }

    pub fn permission_plan(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<ToolPermissionPlanV2> {
        self.inner.permission_plan(ctx, call)
    }

    pub fn egress_audit(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ToolEgressAudit>> {
        self.inner.egress_audit(ctx, call)
    }
}

fn prepared_args_digest(args: &Value) -> Result<String> {
    let encoded = serde_json::to_vec(args)
        .map_err(|error| anyhow!("failed to encode prepared tool arguments: {error}"))?;
    Ok(crate::stable_event_hash(encoded))
}

fn stale_prepared_tool_result(
    call: &crate::provider::ToolCall,
    prepared_digest: &str,
    reason: &str,
) -> ToolResult {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::StalePreparedMutation,
        format!("prepared mutation is stale: {reason}"),
    )
    .with_error_details(
        false,
        serde_json::json!({
            "reason": reason,
            "prepared_mutation_digest": prepared_digest,
        }),
    );
    result.metadata.details = serde_json::json!({
        "prepared_mutation_digest": prepared_digest,
        "stale_reason": reason,
    });
    result
}

fn begin_unknown_mutation_scan(
    ctx: &ToolContext,
    mutation_tracking: ToolMutationTracking,
) -> Result<Option<WorkspaceMutationScan>> {
    if mutation_tracking != ToolMutationTracking::Unknown {
        return Ok(None);
    }
    let Some(recorder) = &ctx.mutation_recorder else {
        return Ok(None);
    };
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    recorder
        .capture_workspace_scan(&ctx.workspace_root, &scope)
        .map(Some)
}

fn ensure_execution_mutation_profile_recorded(
    ctx: &ToolContext,
    spec: &ToolSpec,
    mutation_tracking: ToolMutationTracking,
    call_id: &str,
) -> Result<()> {
    if mutation_tracking != ToolMutationTracking::Unknown || ctx.mutation_recorder.is_none() {
        return Ok(());
    }
    if ctx.execution_mutation_profile_recorded_for(call_id) {
        return Ok(());
    }
    Err(anyhow!(
        "tool {} requires persisted ToolExecutionStarted execution mutation profile before execution",
        spec.name
    ))
}

pub(crate) fn execution_mutation_profile_for_tool(
    ctx: &ToolContext,
    spec: &ToolSpec,
    mutation_tracking: ToolMutationTracking,
    call_id: &str,
) -> Result<Option<ExecutionMutationProfile>> {
    if mutation_tracking != ToolMutationTracking::Unknown {
        return Ok(None);
    }
    let Some(recorder) = &ctx.mutation_recorder else {
        return Ok(None);
    };
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    recorder
        .execution_mutation_profile(
            &ctx.workspace_root,
            &scope,
            call_id.to_owned(),
            spec.name.clone(),
            unknown_mutation_tool_effect(spec),
        )
        .map(Some)
}

fn finish_unknown_mutation_scan(
    ctx: &ToolContext,
    spec: &ToolSpec,
    call_id: &str,
    scan: Option<WorkspaceMutationScan>,
) -> Result<Option<WorkspaceMutationScan>> {
    let Some(scan) = scan else {
        return Ok(None);
    };
    let Some(recorder) = &ctx.mutation_recorder else {
        return Ok(None);
    };
    let after = match recorder.capture_workspace_scan(&ctx.workspace_root, &scan.scope) {
        Ok(after) => after,
        Err(_) => {
            recorder.record_workspace_scan_unavailable_after(
                &scan,
                call_id.to_owned(),
                spec.name.clone(),
                unknown_mutation_tool_effect(spec),
            )?;
            return Ok(None);
        }
    };
    recorder.record_workspace_mutation_scan_result(
        &scan,
        &after,
        call_id.to_owned(),
        spec.name.clone(),
        unknown_mutation_tool_effect(spec),
    )?;
    Ok(Some(after))
}

fn finish_agent_invocation_workspace_effect(
    ctx: &ToolContext,
    spec: &ToolSpec,
    call_id: &str,
    mutation_tracking: ToolMutationTracking,
    validated_workspace_snapshot_id: Option<crate::WorkspaceSnapshotId>,
    audited_workspace_scan: Option<WorkspaceMutationScan>,
) -> Result<()> {
    let Some(grant) = ctx.agent_invocation_grant.as_ref() else {
        return Ok(());
    };
    let Some(validated_workspace_snapshot_id) = validated_workspace_snapshot_id else {
        bail!("child tool execution lost its validated workspace frontier");
    };
    if spec.access == ToolAccess::Read && mutation_tracking == ToolMutationTracking::None {
        return Ok(());
    }
    let observed_after = crate::agent_invocation_workspace_snapshot_id(&ctx.workspace_root)?;
    if mutation_tracking == ToolMutationTracking::Unknown {
        let complete_audit = audited_workspace_scan.as_ref().is_some_and(|scan| {
            scan.workspace_snapshot_id.is_some()
                && scan.manifest.is_some()
                && !scan.workspace_knowledge.is_unknown_dirty()
        });
        if !complete_audit {
            return Err(effect_reconciliation_required_error(
                ctx,
                spec,
                call_id,
                &validated_workspace_snapshot_id,
            ));
        }
    }
    grant.advance_workspace_frontier(&validated_workspace_snapshot_id, observed_after)
}

fn effect_reconciliation_required_error(
    ctx: &ToolContext,
    spec: &ToolSpec,
    call_id: &str,
    validated_workspace_snapshot_id: &crate::WorkspaceSnapshotId,
) -> anyhow::Error {
    let effect_seed = format!(
        "{}\0{}\0{}\0{}",
        ctx.logical_run_id().unwrap_or("unbound-logical-run"),
        call_id,
        spec.name,
        validated_workspace_snapshot_id
    );
    let effect_digest = crate::stable_event_hash(effect_seed.as_bytes());
    let reconciliation_id =
        crate::stable_event_uuid("sigil-effect-reconciliation-v1", &effect_digest);
    let requested_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let entry = crate::EffectReconciliationRequiredEntryV1 {
        schema_version: crate::EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id,
        effect_id: call_id.to_owned(),
        effect_digest,
        replay_contract_fingerprint: "tool-replay-v1:non_replayable_unknown_effect".to_owned(),
        reason_code: "workspace_effect_evidence_incomplete".to_owned(),
        requested_at_unix_ms,
        logical_run_id: ctx.logical_run_id().map(str::to_owned),
        task_id: None,
        step_id: None,
        participant_attempt_id: None,
        base_workspace_observation_id: Some(validated_workspace_snapshot_id.as_str().to_owned()),
        current_workspace_observation_id: None,
        known_receipt_ids: Vec::new(),
        allowed_probe_kinds: vec![crate::ReconciliationProbeKindV1::WorkspaceObservation],
        probe_budget_ms: 10_000,
    };
    let persist_error = ctx
        .mutation_recorder
        .as_ref()
        .ok_or_else(|| anyhow!("durable mutation recorder is unavailable"))
        .and_then(|recorder| {
            recorder
                .append_effect_reconciliation_required(&entry)
                .map(|_| ())
        })
        .err();
    let error = anyhow::Error::from(ToolExecutionGuardError::EffectReconciliationRequired);
    match persist_error {
        Some(persist_error) => error.context(format!(
            "failed to persist effect reconciliation requirement: {persist_error:#}"
        )),
        None => error,
    }
}

fn unknown_mutation_scan_finish_error(spec: &ToolSpec, error: anyhow::Error) -> anyhow::Error {
    let message = format!(
        "failed to finish workspace mutation detection for {}: {error:#}",
        spec.name
    );
    anyhow!(message)
}

fn default_tool_mutation_tracking(spec: &ToolSpec) -> ToolMutationTracking {
    if matches!(spec.category, ToolCategory::Mcp | ToolCategory::Custom) {
        ToolMutationTracking::Unknown
    } else if spec.access == ToolAccess::Read {
        ToolMutationTracking::None
    } else if spec.category == ToolCategory::Shell {
        ToolMutationTracking::Unknown
    } else {
        ToolMutationTracking::None
    }
}

fn unknown_mutation_tool_effect(_spec: &ToolSpec) -> ToolEffect {
    ToolEffect::Unknown
}

#[cfg(test)]
#[path = "tests/network_tool_tests.rs"]
mod network_tests;
#[cfg(test)]
#[path = "tests/tool_tests.rs"]
mod tests;
