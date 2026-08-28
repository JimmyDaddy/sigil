use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::SystemTime,
};

#[cfg(not(test))]
use std::sync::Mutex;

mod agent_flow;
mod approval_flow;
mod checkpoint_flow;
mod command_dispatch;
mod compaction_flow;
pub(crate) mod config_flow;
mod conversation_queue_flow;
mod diagnostics_flow;
mod egress_disclosure_flow;
mod feedback_flow;
mod file_type;
mod formatting;
mod image_attachment_flow;
mod input_flow;
mod input_history;
mod intent_stack_flow;
mod key_router;
mod mcp_oauth_flow;
mod modal_flow;
mod mouse_flow;
mod pending_plan_flow;
mod runtime_command_flow;
mod runtime_status;
mod runtime_view_flow;
mod session_flow;
pub(crate) mod session_lifecycle_flow;
mod session_review;
mod session_view_cache_flow;
mod setup_flow;
mod sidebar_flow;
mod slash_flow;
mod state;
mod submit_flow;
pub(crate) mod task_sidebar;
mod task_strip_flow;
mod timeline_flow;
mod timeline_render_store;
mod tool_card_interaction;
mod update_flow;
mod usage_sidebar_flow;
mod user_input_flow;
mod verification_flow;
mod worker_bridge;
mod workspace_trust_flow;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use sigil_kernel::{
    AgentThreadId, AgentThreadStateProjection, CompactionConfig, CompactionThresholdStatus,
    ControlledCheckpointRestoreRequest, ConversationInputKind, ConversationInputQueueId,
    ConversationInputTarget, ImageAttachment, InteractionMode, MemoryConfig,
    MutationArtifactCleanupTarget, MutationArtifactInventoryItem, MutationArtifactRetentionReport,
    PermissionMode, PlanApprovalPermission, PlanTaskStartMode, ReasoningEffort, RootConfig,
    SecretRedactor, SecretString, SessionConfig, SessionStats, StorageConfig, TaskStateProjection,
    ToolCall, ToolPreviewSnapshot, resolve_workspace_root,
};
use sigil_runtime::{
    BalanceSnapshot, SessionDeletePreview, SessionRetentionPreview, SigilPaths, build_run_options,
    configured_model_context_window_tokens, effective_compaction_config_with_override,
    resolve_sigil_paths, support::SupportBuildInfo,
};
use uuid::Uuid;

pub(crate) use crate::approval::{
    ApprovalAction, ApprovalChangeSetSummary, ApprovalDiagnosticSummary, ApprovalDiffLine,
    ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
    session_grant_unavailable_reason_label,
};
pub use crate::approval::{ApprovalDiffMode, ApprovalPresentationState, PendingApproval};
use crate::commands::{UiCommand, command_for_key_event};
pub(crate) use crate::config_panel::ConfigState;
pub use crate::input::PaneFocus;
use crate::runner::{EgressDisclosureReceiptTx, QueueMoveDirection, TerminalTaskControlIdentity};
pub use crate::sessions::{SessionHistoryEntry, SessionViewMode};
pub(crate) use crate::setup::{SetupField, SetupState};
use crate::slash::ResolvedSlashCommand;
pub use crate::timeline::{EventEntry, TimelineEntry, TimelineRole};
pub(crate) use crate::timeline::{
    LiveActivitySummary, RunPhase, SessionHistoryRow, SidebarCard, ThinkingBlockMode,
    ToolActivityCacheEntry,
};
use crate::workspace_git::{WorkspaceGitStatus, inspect_workspace_git_status};
pub(crate) use crate::workspace_trust::WorkspaceTrustGateState;

pub(crate) use self::checkpoint_flow::{CheckpointRestoreModalPhase, CheckpointRestoreModalView};
pub(crate) use self::egress_disclosure_flow::{EGRESS_DISCLOSURE_HEIGHT, EgressDisclosureCard};
use self::formatting::*;
#[cfg(test)]
pub(crate) use self::intent_stack_flow::IntentStackRowView;
pub(crate) use self::intent_stack_flow::{IntentStackModalPhase, IntentStackModalView};
use self::modal_flow::{ModalState, ModelPickerRefresh};
use self::runtime_status::McpProgressState;
pub(crate) use self::runtime_status::{
    McpServerRuntimeStatus, TimelineTextSelection, code_intelligence_config_status,
    diagnostic_summary_label, initial_mcp_server_status, initial_mcp_server_statuses,
};
use self::state::{
    ActiveTaskRuntimeStatus, AgentPanelState, ApprovalState, ComposerState, EgressDisclosureState,
    ReviewState, RuntimeStatusState, SessionBrowserState, TimelineState,
};
use self::timeline_render_store::TimelineRenderStore;
#[cfg(test)]
pub(crate) use self::usage_sidebar_flow::context_window_source_label;

const SESSION_HISTORY_TITLE_SCAN_LIMIT: usize = 256;
pub(crate) const SCRATCH_DIR_LABEL: &str = "cache/tmp";

pub(super) fn compact_mcp_oauth_scopes(scopes: &[String]) -> String {
    if scopes.is_empty() {
        return "default".to_owned();
    }
    let mut shown = scopes
        .iter()
        .take(3)
        .map(|scope| {
            let mut characters = scope.chars();
            let prefix = characters.by_ref().take(48).collect::<String>();
            if characters.next().is_some() {
                format!("{prefix}…")
            } else {
                prefix
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if scopes.len() > 3 {
        shown.push_str(&format!(" +{}", scopes.len() - 3));
    }
    shown
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentView {
    Main,
    Child {
        child_task_id: String,
        child_session_ref: sigil_kernel::SessionRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerQueueAction {
    KeepNext,
    Edit,
    Delete,
}

impl ComposerQueueAction {
    pub(crate) const ORDER: [Self; 3] = [Self::KeepNext, Self::Edit, Self::Delete];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::KeepNext => "Run next",
            Self::Edit => "Edit",
            Self::Delete => "Delete",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::KeepNext => "run after the current turn",
            Self::Edit => "edit follow-up",
            Self::Delete => "remove follow-up",
        }
    }

    pub(crate) fn is_destructive(self) -> bool {
        matches!(self, Self::Delete)
    }

    fn next(self, forward: bool) -> Self {
        let current = Self::ORDER
            .iter()
            .position(|action| *action == self)
            .unwrap_or(0);
        let len = Self::ORDER.len();
        let next = if forward {
            (current + 1) % len
        } else {
            current.checked_sub(1).unwrap_or(len - 1)
        };
        Self::ORDER[next]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Build,
    Plan,
}

impl ComposerMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Plan => "Plan",
        }
    }

    fn notice(self) -> &'static str {
        match self {
            Self::Build => "thinking",
            Self::Plan => "planning",
        }
    }

    fn phase_marker(self) -> &'static str {
        match self {
            Self::Build => "thinking",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveAgentChildTranscript {
    path: PathBuf,
    file_signature: ChildTranscriptFileSignature,
    timeline_entries: Vec<TimelineEntry>,
    rendered_body_lines: Vec<Line<'static>>,
    total_timeline_entries: usize,
    transcript_truncated: bool,
    load_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildTranscriptFileSignature {
    len: u64,
    modified: Option<SystemTime>,
}

impl ChildTranscriptFileSignature {
    fn empty() -> Self {
        Self {
            len: 0,
            modified: None,
        }
    }
}

#[derive(Debug, Clone)]
struct AgentSidebarItem {
    label: String,
    detail: String,
    target: Option<AgentView>,
    thread_id: Option<sigil_kernel::AgentThreadId>,
    muted: bool,
}

#[derive(Debug, Clone, Default)]
struct SessionViewCache {
    entries_len: usize,
    entries_revision: u64,
    #[cfg(test)]
    rebuild_count: u64,
    task_projection: TaskStateProjection,
    agent_projection: AgentThreadStateProjection,
    task_sidebar_lines: Vec<String>,
    task_strip_view: Option<task_sidebar::TaskStripView>,
    agent_child_items: Vec<AgentSidebarItem>,
    agent_graph_summary_line: Option<String>,
    compaction_preview_line: Option<String>,
    session_review_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPlanApproval {
    pub(crate) plan_id: Option<String>,
    pub(crate) plan_hash: String,
    pub(crate) summary: String,
    pub(crate) steps: Vec<String>,
    pub(crate) target_path_count: usize,
    pub(crate) suggested_check_count: usize,
    pub(crate) workspace_snapshot_id: Option<String>,
    pub(crate) stale: bool,
    pub(crate) stale_reason: Option<String>,
    pub(crate) last_run_failure: Option<String>,
    /// This workbench is resuming the existing approved Task shell, not approving a new Plan.
    pub(crate) retrying_materialization: bool,
    pub(crate) allowed_actions: Vec<sigil_kernel::PublicPlanAction>,
    pub(crate) revision: Option<sigil_kernel::PublicPlanRevisionSummaryV1>,
    pub(crate) detail: sigil_kernel::PlanReviewDetailV1,
    pub(crate) workbench_open: bool,
    pub(crate) workbench_scroll: usize,
    pub(crate) workbench_scroll_extent: ViewportScrollExtent,
    pub(crate) selected_action: PlanWorkbenchAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanWorkbenchAction {
    Run,
    Save,
    Revise,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingUserInputForm {
    pub(crate) view: UserInputFormViewModel,
    pub(crate) request: Option<sigil_kernel::PublicUserInputRequestV1>,
    pub(crate) source: UserInputFormSource,
    pub(crate) recovery_command: Option<sigil_kernel::UserInputDecisionCommandV1>,
    pub(crate) queue_position: usize,
    pub(crate) queue_length: usize,
    pub(crate) open: bool,
    pub(crate) focused_question: usize,
    pub(crate) focus_actions: bool,
    pub(crate) selected_action: UserInputFormAction,
    pub(crate) drafts: Vec<UserInputDraftValue>,
    pub(crate) scroll: usize,
    pub(crate) scroll_extent: ViewportScrollExtent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserInputFormViewModel {
    pub(crate) prompt: String,
    pub(crate) questions: Vec<sigil_kernel::UserInputQuestionV1>,
    pub(crate) allowed_actions: Vec<sigil_kernel::UserInputActionV1>,
}

impl From<&sigil_kernel::PublicUserInputRequestV1> for UserInputFormViewModel {
    fn from(request: &sigil_kernel::PublicUserInputRequestV1) -> Self {
        Self {
            prompt: request.prompt.clone(),
            questions: request.questions.clone(),
            allowed_actions: request.allowed_actions.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserInputFormSource {
    DurableAgent,
    Mcp { server_name: String },
}

#[derive(Debug)]
struct PendingMcpElicitation {
    response_tx: Option<crate::runner::McpElicitationResponseTx>,
}

impl PendingMcpElicitation {
    fn send(&mut self, response: sigil_runtime::McpElicitationResponse) {
        if let Some(response_tx) = self.response_tx.take() {
            let _ = response_tx.send(response);
        }
    }
}

impl Drop for PendingMcpElicitation {
    fn drop(&mut self) {
        self.send(sigil_runtime::McpElicitationResponse::cancel());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserInputFormAction {
    Resume,
    Submit,
    Decline,
    CancelRun,
}

impl UserInputFormAction {
    pub(crate) const ORDER: [Self; 4] =
        [Self::Resume, Self::Submit, Self::Decline, Self::CancelRun];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Resume => "Resume",
            Self::Submit => "Submit",
            Self::Decline => "Decline",
            Self::CancelRun => "Cancel run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserInputDraftValue {
    Text(String),
    Number(String),
    Integer(String),
    Boolean(Option<bool>),
    SingleSelect {
        selected: Option<usize>,
        other: String,
    },
    MultiSelect {
        cursor: usize,
        selected: Vec<String>,
    },
}

impl PlanWorkbenchAction {
    pub(crate) const ORDER: [Self; 4] = [Self::Run, Self::Save, Self::Revise, Self::Reject];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::Save => "Save",
            Self::Revise => "Revise",
            Self::Reject => "Reject",
        }
    }

    pub(crate) fn shortcut(self) -> &'static str {
        match self {
            Self::Run => "R",
            Self::Save => "S",
            Self::Revise => "V",
            Self::Reject => "X",
        }
    }
}

#[cfg(test)]
impl PendingPlanApproval {
    pub(crate) fn test_fixture(
        plan_id: &str,
        plan_text: &str,
        plan_hash: &str,
        summary: &str,
        steps: Vec<String>,
        target_paths: Vec<String>,
        suggested_checks: Vec<String>,
    ) -> Self {
        let detail_steps = steps
            .iter()
            .enumerate()
            .map(|(index, title)| sigil_kernel::PlanReviewStepDetailV1 {
                step_id: format!("step-{}", index + 1),
                title: title.clone(),
                display_name: None,
                detail: None,
                role: None,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
                target_paths: target_paths.clone(),
                suggested_checks: Vec::new(),
                deliverables: Vec::new(),
                acceptance_criteria: Vec::new(),
                required_capabilities: Vec::new(),
                risk: None,
                notes: Vec::new(),
            })
            .collect();
        let target_path_count = target_paths.len();
        let suggested_check_count = suggested_checks.len();
        Self {
            plan_id: Some(plan_id.to_owned()),
            plan_hash: plan_hash.to_owned(),
            summary: summary.to_owned(),
            steps,
            target_path_count,
            suggested_check_count,
            workspace_snapshot_id: None,
            stale: false,
            stale_reason: None,
            last_run_failure: None,
            retrying_materialization: false,
            allowed_actions: vec![
                sigil_kernel::PublicPlanAction::Run,
                sigil_kernel::PublicPlanAction::Save,
                sigil_kernel::PublicPlanAction::Revise,
                sigil_kernel::PublicPlanAction::Reject,
            ],
            revision: None,
            detail: sigil_kernel::PlanReviewDetailV1 {
                plan_id: sigil_kernel::PlanId::new(plan_id).expect("test plan id"),
                plan_hash: plan_hash.to_owned(),
                workspace_snapshot_id: None,
                source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
                summary: summary.to_owned(),
                steps: detail_steps,
                target_paths,
                suggested_checks: Vec::new(),
                risk: None,
                notes: Vec::new(),
                lineage: sigil_kernel::PlanLineageV1 {
                    source: sigil_kernel::PlanSourceRef::default(),
                    plan_review_id: None,
                    attempt_id: None,
                    created_at_ms: 0,
                },
                legacy_markdown: Some(plan_text.to_owned()),
                compile: sigil_kernel::PlanCompileDetailV1 {
                    state: sigil_kernel::PlanReadyStateV1::Ready,
                    candidate_hash: None,
                    compiler_version: None,
                    failure: None,
                },
            },
            workbench_open: false,
            workbench_scroll: 0,
            workbench_scroll_extent: Default::default(),
            selected_action: PlanWorkbenchAction::Run,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ViewportScrollExtent(Rc<Cell<usize>>);

impl ViewportScrollExtent {
    pub(crate) fn get(&self) -> usize {
        self.0.get()
    }

    pub(crate) fn set(&self, value: usize) {
        self.0.set(value);
    }
}

impl PartialEq for ViewportScrollExtent {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for ViewportScrollExtent {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerPasteSpan {
    start: usize,
    end: usize,
    char_count: usize,
    line_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum MutationArtifactRetentionPreview {
    Pending,
    Ready {
        report: MutationArtifactRetentionReport,
        artifacts: Vec<MutationArtifactInventoryItem>,
    },
    Unavailable(String),
}

#[derive(Debug)]
pub struct AppState {
    pub config_path: PathBuf,
    pub workspace_root: PathBuf,
    workspace_git_status: Option<WorkspaceGitStatus>,
    pub sigil_paths: SigilPaths,
    /// RFC-0071 R71.6: single managed input-history writer (authority-declared leaf); None
    /// keeps the legacy path (tests / legacy boot).
    managed_history_writer: Option<
        std::sync::Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
    /// RFC-0071 R71.6: the boot authority composition shared with the worker.
    authority_composition:
        Option<std::sync::Arc<sigil_runtime::application_host::RuntimeAuthorityCompositionV1>>,
    /// Published boot epoch shared with the worker's guarded session opens.
    boot_cutover: Option<std::sync::Arc<sigil_runtime::application_host::RuntimeGlobalCutoverV1>>,
    pub session_log_dir: PathBuf,
    pub session_log_path: PathBuf,
    pub session_id: String,
    pending_worker_session_attachment: std::cell::RefCell<
        Option<(
            PathBuf,
            std::sync::Arc<
                sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
            >,
        )>,
    >,
    support_build_info: SupportBuildInfo,
    update_state: update_flow::UpdateUiState,
    pub(crate) runtime: RuntimeStatusState,
    pub(crate) composer: ComposerState,
    pub(crate) approval: ApprovalState,
    pub(crate) session_browser: SessionBrowserState,
    pub timeline: Vec<TimelineEntry>,
    pub events: Vec<EventEntry>,
    pub should_quit: bool,
    pub active_pane: PaneFocus,
    pub timeline_scroll_back: usize,
    pub activity_scroll_back: usize,
    info_rail_visible: bool,
    info_rail_detail: bool,
    review: ReviewState,
    /// Persisted configuration used for authority/config CAS and product configuration UI.
    config_snapshot: Option<RootConfig>,
    /// Session-effective worker configuration. This may contain a narrow route overlay (for
    /// example `/model`) but must never be used as an authority composition input.
    session_runtime_config: Option<RootConfig>,
    recent_model_refs: Vec<sigil_kernel::ModelRef>,
    terminal_keyboard_enhancement_enabled: bool,
    secret_redactor: SecretRedactor,
    setup_state: Option<SetupState>,
    workspace_trust_gate_state: Option<WorkspaceTrustGateState>,
    config_state: Option<ConfigState>,
    modal_state: Option<ModalState>,
    pending_mcp_elicitation: Option<PendingMcpElicitation>,
    tool_preview_snapshots: HashMap<String, ToolPreviewSnapshot>,
    // Populated only from kernel safe-persistence projections, never exact provider arguments.
    safe_tool_calls: HashMap<String, ToolCall>,
    // Bridges one live progress card to its terminal result without treating call ids as global.
    tool_progress_execution_ids: HashMap<String, String>,
    // Tracks the exact live card occurrence for an active execution.
    tool_progress_entry_indices: HashMap<String, usize>,
    compaction_config: CompactionConfig,
    memory_config: MemoryConfig,
    thinking_block_mode: ThinkingBlockMode,
    timeline_state: TimelineState,
    pending_terminal_cancel_confirmation: Option<String>,
    application_terminal_projection: Option<sigil_application::TerminalSurfaceProjection>,
    terminal_task_control_identities: HashMap<String, TerminalTaskControlIdentity>,
    pending_mouse_slash_confirmation: Option<ResolvedSlashCommand>,
    mouse_hover_target: Option<crate::mouse::HitTarget>,
    pending_mouse_left_down: bool,
    pending_tool_card_body_click_entry: Option<usize>,
    last_notice: Option<String>,
    usage_sidebar_cache: Vec<String>,
    sidebar_selected_card: SidebarCard,
    agent_panel: AgentPanelState,
    egress_disclosure: EgressDisclosureState,
    terminal_width: u16,
    terminal_height: u16,
    slash_selector_index: usize,
}

#[derive(Debug, Clone)]
pub enum AppAction {
    SubmitPrompt(String),
    SubmitPromptWithAttachments {
        prompt: String,
        attachments: Vec<ImageAttachment>,
    },
    QueueConversationInput {
        prompt: String,
        kind: ConversationInputKind,
        target: ConversationInputTarget,
    },
    CancelQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    EditQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        prompt: String,
    },
    MoveQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        direction: QueueMoveDirection,
    },
    PromoteQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    SendQueuedConversationInputNow {
        queue_id: ConversationInputQueueId,
    },
    SetConversationQueuePaused {
        paused: bool,
    },
    SubmitPlanPrompt(String),
    CreateTaskFromPlan {
        plan_id: String,
        expected_plan_hash: String,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    },
    RejectPlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    SavePlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    RevisePlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    SubmitUserInputDecision {
        command_id: Option<String>,
        request_id: String,
        generation: u32,
        expected_request_hash: String,
        decision: sigil_kernel::UserInputDecisionV1,
    },
    SubmitTask(String),
    InvokeInlineSkill {
        skill_id: String,
        arguments: String,
    },
    InvokeChildSessionSkill {
        skill_id: String,
        arguments: String,
    },
    InvokeAgentProfile {
        profile_id: String,
        prompt: String,
        parent_prompt: String,
    },
    ContinueTask {
        task_id: Option<String>,
        guidance: Option<String>,
    },
    PauseTask {
        request: sigil_kernel::TaskPauseRequest,
    },
    ApprovalDecision {
        call_id: String,
        approval_request_id: String,
        approved: bool,
    },
    ApprovalSessionDecision {
        call_id: String,
        approval_request_id: String,
    },
    ApprovalFamilyDecision {
        call_id: String,
        approval_request_id: String,
        pattern: String,
    },
    ApprovalDecisionWithArgs {
        call_id: String,
        approval_request_id: String,
        args_json: String,
    },
    BackgroundActiveAgent,
    CancelRun,
    CancelTerminalTask {
        identity: TerminalTaskControlIdentity,
    },
    CloseAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    CancelAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    MessageAgent {
        thread_id: AgentThreadId,
        prompt: String,
    },
    CopyToClipboard {
        text: String,
    },
    CopySecretToClipboard {
        text: SecretString,
    },
    OpenExternalUrl {
        url: String,
    },
    OpenSecretExternalUrl {
        url: SecretString,
    },
    RevealFile {
        path: PathBuf,
    },
    CheckForUpdate {
        force_refresh: bool,
        channel: sigil_updater::UpdateChannel,
    },
    ApplyUpdate {
        channel: sigil_updater::UpdateChannel,
    },
    StartV2Compaction,
    PreviewV2Compaction,
    ApplyV2Compaction {
        request_id: u64,
    },
    ApplyStandaloneToolOutputShrink {
        request_id: u64,
    },
    CancelV2CompactionReview {
        request_id: u64,
    },
    CheckChangedFilesDiagnostics,
    CleanMutationArtifacts {
        target: MutationArtifactCleanupTarget,
    },
    DeleteMutationArtifact {
        artifact_id: String,
    },
    TrustWorkspace,
    ApproveVerificationCheck {
        check_spec_id: String,
    },
    SandboxVerificationCheck {
        check_spec_id: String,
    },
    RerunTaskVerification {
        request: sigil_kernel::TaskVerificationRerunRequest,
    },
    ReviewTaskIntegration {
        request: sigil_kernel::TaskIntegrationReviewRequest,
    },
    AcceptTaskIntegration {
        request: sigil_kernel::TaskIntegrationReviewRequest,
    },
    PreviewCheckpointRestore {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    ExecuteCheckpointRestore {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    ForkConversationAtCheckpoint {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    LoadIntentStack {
        request_id: u64,
    },
    PreviewIntentDrop {
        request_id: u64,
        intent_ref: sigil_kernel::IntentVersionRef,
    },
    ExecuteIntentDrop {
        request_id: u64,
        request: sigil_kernel::IntentDropRequestV1,
    },
    InspectLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    ForkLocalSession {
        request_id: u64,
        source_path: PathBuf,
        current_model_route: sigil_kernel::ResolvedModelRoute,
    },
    ExportLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    SetLocalSessionPin {
        request_id: u64,
        source_path: PathBuf,
        pinned: bool,
    },
    PreviewLocalSessionDelete {
        request_id: u64,
        source_path: PathBuf,
    },
    ApplyLocalSessionDelete {
        request_id: u64,
        preview: SessionDeletePreview,
    },
    ApplySessionRetention {
        request_id: u64,
        preview: SessionRetentionPreview,
    },
    PreviewSessionRetention {
        request_id: u64,
        policy: sigil_runtime::SessionRetentionPolicy,
    },
    ActivateLazyMcp {
        server_name: Option<String>,
    },
    RefreshMcpServer {
        server_name: String,
    },
    McpOAuth {
        server_name: String,
        action: crate::runner::McpOAuthUserAction,
    },
    StartNewSession {
        session_log_path: PathBuf,
    },
    SwitchSession {
        session_log_path: PathBuf,
    },
    RuntimeConfigUpdated {
        root_config: Box<RootConfig>,
    },
    /// Session-scoped provider route update. It changes only the worker's effective route; the
    /// persisted configuration and authority composition remain unchanged.
    SessionRuntimeRouteUpdated {
        route: sigil_kernel::ResolvedModelRoute,
    },
    /// Runtime permission-mode switch for the active run (and persisted default); does not
    /// restart the worker.
    UpdateActiveRunPermissionMode {
        mode: sigil_kernel::PermissionMode,
    },
    SetDefaultModel {
        root_config: Box<RootConfig>,
        expected_root_config: Box<RootConfig>,
    },
    SetupCompleted {
        config_path: PathBuf,
        root_config: Box<RootConfig>,
    },
    ConfigSaved {
        root_config: Box<RootConfig>,
    },
    PersistConfiguration {
        request: Arc<ConfigurationSaveRequest>,
    },
}

#[cfg_attr(test, allow(dead_code))]
pub struct ConfigurationSaveRequest {
    pub(crate) expected: RootConfig,
    pub(crate) next_base: RootConfig,
    pub(crate) config_path: PathBuf,
    pub(crate) follow_up: ConfigurationSaveFollowUp,
    pub(crate) root_only: bool,
    #[cfg(not(test))]
    pub(crate) draft: Mutex<Option<sigil_runtime::provider_connections::ConnectionSaveDraft>>,
}

#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigurationSaveFollowUp {
    RebootRuntime,
    ApplyActiveRunPermissionMode(sigil_kernel::PermissionMode),
    ApplyPersistedDefaultModel,
}

impl std::fmt::Debug for ConfigurationSaveRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigurationSaveRequest")
            .field("expected", &self.expected)
            .field("next_base", &self.next_base)
            .field("config_path", &self.config_path)
            .field("draft", &"[redacted]")
            .finish()
    }
}

fn configured_runtime_route(
    root_config: &RootConfig,
) -> (String, String, Option<sigil_kernel::ResolvedModelRoute>) {
    sigil_runtime::provider_connections::resolve_default_model_route(root_config)
        .map(|(provider_name, route)| {
            (provider_name, route.model_ref.model_id.clone(), Some(route))
        })
        .unwrap_or_else(|_| {
            (
                root_config.agent.runtime_provider.clone(),
                root_config.agent.model.clone(),
                None,
            )
        })
}

impl AppState {
    pub(crate) fn retain_worker_session_attachment(
        &self,
        session_log_path: PathBuf,
        attachment: std::sync::Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
    ) {
        *self.pending_worker_session_attachment.borrow_mut() = Some((session_log_path, attachment));
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn release_worker_session_attachment(&self) {
        *self.pending_worker_session_attachment.borrow_mut() = None;
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn worker_session_attachment(
        &self,
    ) -> Option<
        std::sync::Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
    > {
        let mut pending = self.pending_worker_session_attachment.borrow_mut();
        match pending.as_ref() {
            Some((session_log_path, _)) if session_log_path == &self.session_log_path => pending
                .as_ref()
                .map(|(_, attachment)| std::sync::Arc::clone(attachment)),
            Some(_) => {
                *pending = None;
                None
            }
            None => None,
        }
    }

    /// Attaches the boot authority composition (single call at boot).
    pub fn set_authority_composition(
        &mut self,
        composition: std::sync::Arc<sigil_runtime::application_host::RuntimeAuthorityCompositionV1>,
    ) {
        self.managed_history_writer = Some(std::sync::Arc::clone(&composition.storage_writer));
        self.authority_composition = Some(composition);
        // RFC-0071 R71.6: route the session writer and input history to the authority-declared
        // managed leaves (the worker/store read and write the same leaves; no dual write).
        self.session_log_path = self
            .managed_session_log_path()
            .unwrap_or_else(|| self.session_log_path.clone());
        self.load_input_history();
    }

    pub fn set_boot_cutover(
        &mut self,
        cutover: std::sync::Arc<sigil_runtime::application_host::RuntimeGlobalCutoverV1>,
    ) {
        self.boot_cutover = Some(cutover);
    }

    /// Applies the bounded runtime application projection to the legacy orchestration state used
    /// by the current product adapter. The projection is the source of truth for shared run
    /// status, selected route and attention; it never carries paths or provider payloads.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn apply_application_projection(
        &mut self,
        projection: &sigil_application::ApplicationProjection,
    ) -> bool {
        let mut changed = false;
        let is_busy = projection.run.status.as_str() == "running";
        if self.runtime.is_busy != is_busy {
            self.runtime.is_busy = is_busy;
            changed = true;
        }
        if let Some(route) = projection.configuration.selected_route.as_ref()
            && self.runtime.model_name != route.as_str()
        {
            self.runtime.model_name = route.as_str().to_owned();
            changed = true;
        }
        if let Some(notice) = projection.attention.last_notice.as_ref()
            && self.last_notice.as_deref() != Some(notice.as_str())
        {
            self.last_notice = Some(notice.as_str().to_owned());
            changed = true;
        }
        if self.application_terminal_projection.as_ref() != Some(&projection.terminal) {
            self.application_terminal_projection = Some(projection.terminal.clone());
            changed = true;
        }
        changed
    }

    /// Clears all authority-owned boot attachments before a replacement transaction is
    /// attempted. A failed config replacement must not leave the stopped worker paired with an
    /// authority composition built from a previous snapshot.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn clear_boot_authority(&mut self) {
        self.managed_history_writer = None;
        self.authority_composition = None;
        self.boot_cutover = None;
    }

    /// Applies the runtime boot owner's frozen workspace/path view before any session or writer
    /// state is initialized. This prevents the TUI shell from resolving authority roots a second
    /// time from its own process cwd.
    pub(crate) fn set_frozen_boot_paths(
        &mut self,
        workspace_root: PathBuf,
        sigil_paths: sigil_runtime::paths::SigilPaths,
    ) {
        self.workspace_root = workspace_root.clone();
        self.workspace_git_status = inspect_workspace_git_status(&workspace_root);
        self.sigil_paths = sigil_paths;
        self.session_log_dir = self.sigil_paths.session_log_dir.clone();
        self.session_log_path = self
            .session_log_dir
            .join(format!("session-{}.jsonl", self.session_id));
    }

    /// Authority-declared managed session-log leaf for the CURRENT session (opaque stem as the
    /// per-session key); None when no writer is attached (tests / legacy boot).
    fn managed_session_log_path(&self) -> Option<std::path::PathBuf> {
        let writer = self.managed_history_writer.as_ref()?;
        let stem = self.session_log_path.file_stem()?.to_str()?;
        writer
            .managed_named_leaf_path(
                sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
                stem,
            )
            .ok()
            .map(|path| path.join("records.jsonl"))
    }

    /// Resolves the current session's artifact roots through the authority-declared leaves.
    /// `None` means the current-schema composition could not provide a safe read route; callers
    /// must not fall back to the legacy sibling path in that case.
    pub(super) fn tool_artifact_store_for_current_session(
        &self,
    ) -> Option<sigil_kernel::ToolArtifactStore> {
        let writer = self.managed_history_writer.as_ref()?;
        let composition = self.authority_composition.as_ref()?;
        let staging =
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging;
        let store = sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore;
        if !composition.declared_channels.contains(&staging)
            || !composition.declared_channels.contains(&store)
        {
            return None;
        }
        // The app shell does not own the worker's paired artifact lease. Returning a
        // path-backed kernel store here would reopen the P1-10 bypass; the worker-provided
        // runtime attachment is the only current-schema reader route.
        let _ = (writer, composition, staging, store);
        None
    }

    /// The boot authority composition (None before boot attachment). Used by the production
    /// launcher; test builds route composition through the launcher builder so the accessor can
    /// be unused in that cfg.
    #[allow(dead_code)]
    pub(crate) fn authority_composition(
        &self,
    ) -> Option<&std::sync::Arc<sigil_runtime::application_host::RuntimeAuthorityCompositionV1>>
    {
        self.authority_composition.as_ref()
    }

    /// The boot owner's published current-schema decision shared with the worker. Production
    /// worker startup must consume this value instead of reopening a manifest by pathname.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn boot_cutover(
        &self,
    ) -> Option<&std::sync::Arc<sigil_runtime::application_host::RuntimeGlobalCutoverV1>> {
        self.boot_cutover.as_ref()
    }

    pub fn from_root_config(config_path: &Path, root_config: &RootConfig) -> Self {
        let launch_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let workspace_root =
            resolve_workspace_root(config_path, &launch_cwd, &root_config.workspace.root);
        let sigil_paths =
            resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
        let recent_model_refs = sigil_runtime::provider_connections::load_recent_model_refs(
            &sigil_paths.state_root,
            root_config,
        );
        let session_log_dir = sigil_paths.session_log_dir.clone();
        let session_id = Uuid::new_v4().to_string();
        let permission_mode = root_config.permission.mode.as_str().to_owned();
        let (configured_provider_name, configured_model_name, configured_model_route) =
            configured_runtime_route(root_config);
        let initial_model_context_window_tokens =
            configured_model_route.as_ref().and_then(|route| {
                configured_model_context_window_tokens(root_config, &route.model_ref)
            });
        let initial_compaction_status = effective_compaction_config_with_override(
            &configured_provider_name,
            &configured_model_name,
            initial_model_context_window_tokens,
            &root_config.compaction,
        )
        .threshold_status(0)
        .as_str()
        .to_owned();
        let initial_code_intelligence_status =
            code_intelligence_config_status(&root_config.code_intelligence);
        let initial_reasoning_effort = build_run_options(
            root_config,
            workspace_root.clone(),
            InteractionMode::Interactive,
            None,
        )
        .reasoning_effort
        .unwrap_or(ReasoningEffort::Max);
        let workspace_git_status = inspect_workspace_git_status(&workspace_root);

        let mut app = Self {
            config_path: config_path.to_path_buf(),
            workspace_root,
            workspace_git_status,
            sigil_paths,
            managed_history_writer: None,
            authority_composition: None,
            boot_cutover: None,
            session_log_dir,
            session_log_path: PathBuf::new(),
            session_id,
            pending_worker_session_attachment: std::cell::RefCell::new(None),
            support_build_info: SupportBuildInfo::unknown(),
            update_state: update_flow::UpdateUiState::default(),
            runtime: RuntimeStatusState {
                provider_name: configured_provider_name,
                model_name: configured_model_name,
                model_route: configured_model_route,
                permission_mode,
                memory_enabled: root_config.memory.enabled,
                memory_document_count: 0,
                memory_last_status: "pending".to_owned(),
                mutation_artifact_retention_preview: MutationArtifactRetentionPreview::Pending,
                session_retention_preview: Default::default(),
                compaction_status: initial_compaction_status,
                code_intelligence_status: initial_code_intelligence_status,
                code_intelligence_server_lines: BTreeMap::new(),
                code_intelligence_diagnostics_line: None,
                code_intelligence_diagnostics_by_path: BTreeMap::new(),
                mcp_server_statuses: initial_mcp_server_statuses(root_config),
                stats: SessionStats::default(),
                session_delta_stats: SessionStats::default(),
                is_busy: false,
                mcp_progress: None,
                reasoning_effort: initial_reasoning_effort,
                run_phase: RunPhase::Idle,
                last_phase_marker: None,
                balance_snapshot: BalanceSnapshot {
                    status: "pending".to_owned(),
                    ..BalanceSnapshot::default()
                },
                next_background_request_id: 1,
                pending_worker_commands: Vec::new(),
                pending_session_route_recovery_binding: None,
                pending_session_route_recovery_code: None,
                pending_session_route_recovery_target: None,
                pending_session_route_selection: None,
                worker_rebind_required: false,
                active_balance_refresh_id: None,
                active_model_picker_refresh: None,
                connection_model_catalog_views: BTreeMap::new(),
                setup_model_catalog_rx: None,
                connection_inventory: None,
                connection_inventory_revision: 0,
                connection_inventory_rx: None,
                #[cfg(not(test))]
                connection_inventory_worker: None,
                #[cfg(not(test))]
                pending_connection_inventory_config: None,
                active_task: None,
                task_provider_route_diagnostics:
                    sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default(),
                task_completion_progress: sigil_runtime::TaskCompletionProgressSnapshot::default(),
            },
            composer: ComposerState::default(),
            approval: ApprovalState::default(),
            session_browser: SessionBrowserState::default(),
            timeline: Vec::new(),
            events: Vec::new(),
            should_quit: false,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            activity_scroll_back: 0,
            info_rail_visible: root_config.appearance.info_rail,
            info_rail_detail: false,
            review: ReviewState::default(),
            config_snapshot: Some(root_config.clone()),
            session_runtime_config: Some(root_config.clone()),
            recent_model_refs,
            terminal_keyboard_enhancement_enabled: false,
            secret_redactor: sigil_runtime::secret_redactor_for_root_config(root_config),
            setup_state: None,
            workspace_trust_gate_state: None,
            config_state: None,
            modal_state: None,
            pending_mcp_elicitation: None,
            tool_preview_snapshots: HashMap::new(),
            safe_tool_calls: HashMap::new(),
            tool_progress_execution_ids: HashMap::new(),
            tool_progress_entry_indices: HashMap::new(),
            compaction_config: root_config.compaction.clone(),
            memory_config: root_config.memory.clone(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            timeline_state: TimelineState::default(),
            pending_terminal_cancel_confirmation: None,
            application_terminal_projection: None,
            terminal_task_control_identities: HashMap::new(),
            pending_mouse_slash_confirmation: None,
            mouse_hover_target: None,
            pending_mouse_left_down: false,
            pending_tool_card_body_click_entry: None,
            last_notice: None,
            usage_sidebar_cache: Vec::new(),
            sidebar_selected_card: SidebarCard::Permission,
            agent_panel: AgentPanelState::default(),
            egress_disclosure: EgressDisclosureState::default(),
            terminal_width: 120,
            terminal_height: 32,
            slash_selector_index: 0,
        };
        app.session_log_path = app
            .session_log_dir
            .join(format!("session-{}.jsonl", app.session_id));
        if let Some(path) = app.managed_session_log_path() {
            app.session_log_path = path;
        }
        app.load_input_history();
        app.seed_connection_inventory_offline(root_config);
        app.refresh_memory_summary();
        app.recompute_compaction_status(false);
        app.refresh_session_history();
        app.bootstrap();
        app.schedule_balance_refresh();
        app.refresh_usage_sidebar_cache();
        app
    }

    pub fn from_setup(
        config_path: PathBuf,
        workspace_root: PathBuf,
        startup_error: Option<String>,
    ) -> Self {
        let sigil_paths = resolve_sigil_paths(
            &StorageConfig::default(),
            &SessionConfig::default(),
            &workspace_root,
        );
        let session_log_dir = sigil_paths.session_log_dir.clone();
        let session_id = Uuid::new_v4().to_string();
        let workspace_git_status = inspect_workspace_git_status(&workspace_root);
        let mut app = Self {
            config_path: config_path.clone(),
            workspace_root: workspace_root.clone(),
            workspace_git_status,
            sigil_paths,
            managed_history_writer: None,
            authority_composition: None,
            boot_cutover: None,
            session_log_dir,
            session_log_path: PathBuf::new(),
            session_id,
            pending_worker_session_attachment: std::cell::RefCell::new(None),
            support_build_info: SupportBuildInfo::unknown(),
            update_state: update_flow::UpdateUiState::default(),
            runtime: RuntimeStatusState {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
                model_route: None,
                permission_mode: PermissionMode::Manual.as_str().to_owned(),
                memory_enabled: true,
                memory_document_count: 0,
                memory_last_status: "pending".to_owned(),
                mutation_artifact_retention_preview: MutationArtifactRetentionPreview::Pending,
                session_retention_preview: Default::default(),
                compaction_status: CompactionThresholdStatus::NotAvailable.as_str().to_owned(),
                code_intelligence_status: "off".to_owned(),
                code_intelligence_server_lines: BTreeMap::new(),
                code_intelligence_diagnostics_line: None,
                code_intelligence_diagnostics_by_path: BTreeMap::new(),
                mcp_server_statuses: BTreeMap::new(),
                stats: SessionStats::default(),
                session_delta_stats: SessionStats::default(),
                is_busy: false,
                mcp_progress: None,
                reasoning_effort: ReasoningEffort::Max,
                run_phase: RunPhase::Idle,
                last_phase_marker: None,
                balance_snapshot: BalanceSnapshot {
                    status: "missing auth".to_owned(),
                    ..BalanceSnapshot::default()
                },
                next_background_request_id: 1,
                pending_worker_commands: Vec::new(),
                pending_session_route_recovery_binding: None,
                pending_session_route_recovery_code: None,
                pending_session_route_recovery_target: None,
                pending_session_route_selection: None,
                worker_rebind_required: false,
                active_balance_refresh_id: None,
                active_model_picker_refresh: None,
                connection_model_catalog_views: BTreeMap::new(),
                setup_model_catalog_rx: None,
                connection_inventory: None,
                connection_inventory_revision: 0,
                connection_inventory_rx: None,
                #[cfg(not(test))]
                connection_inventory_worker: None,
                #[cfg(not(test))]
                pending_connection_inventory_config: None,
                active_task: None,
                task_provider_route_diagnostics:
                    sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default(),
                task_completion_progress: sigil_runtime::TaskCompletionProgressSnapshot::default(),
            },
            composer: ComposerState::default(),
            approval: ApprovalState::default(),
            session_browser: SessionBrowserState::default(),
            timeline: Vec::new(),
            events: Vec::new(),
            should_quit: false,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            activity_scroll_back: 0,
            info_rail_visible: true,
            info_rail_detail: false,
            review: ReviewState::default(),
            config_snapshot: None,
            session_runtime_config: None,
            recent_model_refs: Vec::new(),
            terminal_keyboard_enhancement_enabled: false,
            secret_redactor: SecretRedactor::default(),
            setup_state: Some(SetupState::new(config_path, startup_error.clone())),
            workspace_trust_gate_state: None,
            config_state: None,
            modal_state: None,
            pending_mcp_elicitation: None,
            tool_preview_snapshots: HashMap::new(),
            safe_tool_calls: HashMap::new(),
            tool_progress_execution_ids: HashMap::new(),
            tool_progress_entry_indices: HashMap::new(),
            compaction_config: CompactionConfig::default(),
            memory_config: MemoryConfig::default(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            timeline_state: TimelineState::default(),
            pending_terminal_cancel_confirmation: None,
            application_terminal_projection: None,
            terminal_task_control_identities: HashMap::new(),
            pending_mouse_slash_confirmation: None,
            mouse_hover_target: None,
            pending_mouse_left_down: false,
            pending_tool_card_body_click_entry: None,
            last_notice: startup_error,
            usage_sidebar_cache: Vec::new(),
            sidebar_selected_card: SidebarCard::Permission,
            agent_panel: AgentPanelState::default(),
            egress_disclosure: EgressDisclosureState::default(),
            terminal_width: 120,
            terminal_height: 32,
            slash_selector_index: 0,
        };
        app.session_log_path = app
            .session_log_dir
            .join(format!("session-{}.jsonl", app.session_id));
        if let Some(path) = app.managed_session_log_path() {
            app.session_log_path = path;
        }
        app.load_input_history();
        app.bootstrap_setup();
        app.refresh_usage_sidebar_cache();
        app
    }

    pub(crate) fn workspace_git_status(&self) -> Option<&WorkspaceGitStatus> {
        self.workspace_git_status.as_ref()
    }

    pub(crate) fn refresh_workspace_git_status(&mut self) {
        self.workspace_git_status = inspect_workspace_git_status(&self.workspace_root);
    }

    pub(crate) fn code_intelligence_sidebar_lines(&self) -> Vec<String> {
        if self.runtime.code_intelligence_server_lines.is_empty()
            && self.runtime.code_intelligence_diagnostics_line.is_none()
            && self
                .runtime
                .code_intelligence_diagnostics_by_path
                .is_empty()
        {
            return vec![format!("status: {}", self.runtime.code_intelligence_status)];
        }
        let mut lines = self
            .runtime
            .code_intelligence_server_lines
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(line) = &self.runtime.code_intelligence_diagnostics_line {
            lines.push(line.clone());
        }
        lines.extend(self.code_intelligence_diagnostic_file_lines());
        lines
    }

    fn code_intelligence_diagnostic_file_lines(&self) -> Vec<String> {
        if self
            .runtime
            .code_intelligence_diagnostics_by_path
            .is_empty()
        {
            return Vec::new();
        }
        const MAX_DIAGNOSTIC_FILES: usize = 4;
        let mut summaries = self
            .runtime
            .code_intelligence_diagnostics_by_path
            .iter()
            .map(|(path, summary)| (path.as_str(), *summary))
            .collect::<Vec<_>>();
        summaries.sort_by(|(left_path, left), (right_path, right)| {
            right
                .errors
                .cmp(&left.errors)
                .then_with(|| right.warnings.cmp(&left.warnings))
                .then_with(|| left_path.cmp(right_path))
        });

        let mut lines = vec![format!("latest diagnostics: {} files", summaries.len())];
        lines.extend(
            summaries
                .iter()
                .take(MAX_DIAGNOSTIC_FILES)
                .map(|(path, summary)| format!("{path}: {}", diagnostic_summary_label(*summary))),
        );
        let hidden = summaries.len().saturating_sub(MAX_DIAGNOSTIC_FILES);
        if hidden > 0 {
            lines.push(format!("+{hidden} more files"));
        }
        lines
    }

    fn bootstrap(&mut self) {
        self.timeline.clear();
        self.timeline_state.tool_activity_cache.clear();
        self.timeline_state.tool_activity_visible_rows.clear();
        self.events.clear();
        self.ensure_scratch_dir();
        self.push_timeline(TimelineRole::System, "sigil ready.");
        self.push_event("session", format!("active {}", self.session_id));
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.push_event(
            "model",
            format!("{}/{}", self.runtime.provider_name, self.runtime.model_name),
        );
        self.push_event("effort", self.runtime.reasoning_effort.as_str());
        self.push_event("permission_mode", self.runtime.permission_mode.clone());
        self.push_event(
            "memory",
            format!(
                "enabled={} docs={} status={}",
                self.runtime.memory_enabled,
                self.runtime.memory_document_count,
                self.runtime.memory_last_status
            ),
        );
        self.push_event("compaction", self.runtime.compaction_status.clone());
        self.push_event(
            "code_intelligence",
            self.runtime.code_intelligence_status.clone(),
        );
        self.push_event("session_log", self.session_log_path.display().to_string());
        self.push_event("focus", self.active_pane.label());
        self.reset_scroll();
    }

    fn bootstrap_setup(&mut self) {
        self.timeline.clear();
        self.timeline_state.tool_activity_cache.clear();
        self.timeline_state.tool_activity_visible_rows.clear();
        self.events.clear();
        self.ensure_scratch_dir();
        self.push_timeline(TimelineRole::System, "quick setup");
        self.push_timeline(TimelineRole::Notice, "launch dir = workspace");
        if let Some(error) = self
            .setup_state
            .as_ref()
            .and_then(|state| state.startup_error.clone())
        {
            self.push_timeline(TimelineRole::Notice, format!("load failed: {error}"));
        }
        self.push_event("mode", "setup");
        if let Some(state) = &self.setup_state {
            self.push_event("config_path", state.config_path.display().to_string());
        }
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.reset_scroll();
    }

    fn ensure_scratch_dir(&mut self) {
        let temp_dir = self.sigil_paths.scratch_root.clone();
        // RFC-0062 14.1: the workspace scratch base must be owner-only before any session
        // namespace is created below it.
        match std::fs::create_dir_all(&temp_dir).and_then(|()| {
            sigil_kernel::secure_private_path_permissions(&temp_dir).map_err(std::io::Error::other)
        }) {
            Ok(()) => self.push_event("scratch", SCRATCH_DIR_LABEL),
            Err(error) => self.push_event(
                "scratch",
                format!("failed to create {}: {error}", temp_dir.display()),
            ),
        }
    }

    fn new_session_log_path(&self) -> PathBuf {
        self.session_log_dir
            .join(format!("session-{}.jsonl", Uuid::new_v4()))
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        if self.is_workspace_trust_gate_mode() {
            return self.handle_workspace_trust_gate_key_event(key);
        }
        if self.session_lifecycle_modal_open() {
            return Ok(self.handle_session_lifecycle_modal_key(key));
        }
        if self.is_setup_mode() {
            return self.handle_setup_key_event(key);
        }
        if self.mcp_oauth_modal_open() {
            return Ok(self.handle_mcp_oauth_modal_key_event(key));
        }
        if self.is_config_mode() {
            return self.handle_config_key_event(key);
        }

        if command_for_key_event(key) == Some(UiCommand::CopyTranscript) {
            return Ok(self.request_copy_selection_or_latest_response());
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.checkpoint_mutation_pending() || self.intent_drop_applying() {
                self.last_notice = Some(
                    "a reviewed workspace operation is applying; wait for completion before quitting"
                        .to_owned(),
                );
                return Ok(None);
            }
            if let Some(text) = self.selected_timeline_text() {
                return Ok(Some(self.request_clipboard_copy(text)));
            }
            self.modal_state = None;
            if self.runtime.is_busy {
                self.last_notice = Some("cancellation requested".to_owned());
                self.push_timeline(TimelineRole::Notice, "cancel requested");
                return Ok(Some(AppAction::CancelRun));
            }
            self.should_quit = true;
            return Ok(None);
        }
        if self.checkpoint_restore_modal_open() {
            return Ok(self.handle_checkpoint_restore_modal_key_event(key));
        }
        if self.intent_stack_modal_open() {
            return Ok(self.handle_intent_stack_modal_key_event(key));
        }
        if self.feedback_modal_open() {
            return Ok(self.handle_feedback_modal_key_event(key));
        }
        if self.has_modal() {
            let outcome = if key.code == KeyCode::Enter {
                self.submit_modal()
            } else {
                self.handle_modal_key_event(key)
            };
            match outcome {
                modal_flow::ModalOutcome::V2CompactionConfirmed { request_id } => {
                    self.apply_modal_outcome(modal_flow::ModalOutcome::V2CompactionConfirmed {
                        request_id,
                    });
                    return Ok(Some(AppAction::ApplyV2Compaction { request_id }));
                }
                modal_flow::ModalOutcome::V2CompactionDismissed { request_id } => {
                    self.apply_modal_outcome(modal_flow::ModalOutcome::V2CompactionDismissed {
                        request_id,
                    });
                    return Ok(Some(AppAction::CancelV2CompactionReview { request_id }));
                }
                modal_flow::ModalOutcome::StandaloneToolOutputShrinkConfirmed { request_id } => {
                    self.apply_modal_outcome(
                        modal_flow::ModalOutcome::StandaloneToolOutputShrinkConfirmed {
                            request_id,
                        },
                    );
                    return Ok(Some(AppAction::ApplyStandaloneToolOutputShrink {
                        request_id,
                    }));
                }
                outcome => self.apply_modal_outcome(outcome),
            }
            return Ok(None);
        }

        if let Some(outcome) = self.handle_user_input_form_key_event(key) {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_pending_plan_approval_key_event(key) {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_key_router_event(key)? {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_pending_approval_key_event(key) {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_verification_card_key_event(key) {
            return Ok(outcome);
        }

        if self.runtime.is_busy
            && !self.approval.has_actionable_pending()
            && matches!(self.runtime.run_phase, RunPhase::Agent(_))
            && matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
            && has_control_without_alt(key)
        {
            self.last_notice = Some("agent background requested".to_owned());
            self.push_timeline(TimelineRole::Notice, "agent background requested");
            self.push_event("agent", "background requested");
            return Ok(Some(AppAction::BackgroundActiveAgent));
        }

        if self.active_pane == PaneFocus::Activity
            && !self.approval.has_actionable_pending()
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Char(character)
                    if key.modifiers.is_empty()
                        && normalize_command_prefix_character(character).is_some() =>
                {
                    self.active_pane = PaneFocus::Composer;
                    self.insert_input_character('/');
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    return Ok(None);
                }
                KeyCode::BackTab if self.sidebar_selected_card == SidebarCard::Permission => {
                    return self.toggle_runtime_permission_mode();
                }
                KeyCode::Up => {
                    self.move_sidebar_selection(false);
                    return Ok(None);
                }
                KeyCode::Down => {
                    self.move_sidebar_selection(true);
                    return Ok(None);
                }
                KeyCode::Enter if self.sidebar_selected_card == SidebarCard::Agents => {
                    self.activate_selected_agent_view();
                    return Ok(None);
                }
                KeyCode::Esc => {
                    if self.handle_ui_command(UiCommand::ClearToolCardFocus) {
                        return Ok(None);
                    }
                    self.active_pane = PaneFocus::Composer;
                    return Ok(None);
                }
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right
                    if key.modifiers.is_empty() =>
                {
                    return Ok(None);
                }
                _ => {}
            }
        }

        if matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'))
            && key.modifiers == KeyModifiers::CONTROL
            && self.resume_session_selector_active()
        {
            return Ok(self.open_selected_session_actions());
        }

        if let Some(command) = command_for_key_event(key) {
            let composer_owns_shortcut = command == UiCommand::SearchToolArtifact
                && self.active_pane == PaneFocus::Composer
                && !self.composer.input.is_empty();
            // Alt-F remains readline-compatible while the user is editing.
            // Artifact search is available when activity focus owns the
            // shortcut or the composer is empty.
            if !composer_owns_shortcut {
                if command == UiCommand::OpenCheckpointRestore {
                    return Ok(self.open_checkpoint_restore_modal());
                }
                if command == UiCommand::OpenIntentStack {
                    return Ok(self.open_intent_stack_modal());
                }
                if command == UiCommand::CheckChangedFilesDiagnostics {
                    return Ok(self.request_changed_files_diagnostics());
                }
                if command == UiCommand::PauseActiveTask {
                    return Ok(self.request_active_task_pause());
                }
                if command == UiCommand::CancelFocusedTerminalTask {
                    return Ok(self.request_focused_terminal_task_cancel());
                }
                self.handle_ui_command(command);
                return Ok(None);
            }
        }

        match key.code {
            KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                self.scroll_timeline(self.transcript_page_step());
                return Ok(None);
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                self.unscroll_timeline(self.transcript_page_step());
                return Ok(None);
            }
            KeyCode::Char('p')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                self.navigate_input_history(true);
                return Ok(None);
            }
            KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                self.navigate_input_history(false);
                return Ok(None);
            }
            KeyCode::Home
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                self.scroll_timeline_to_top();
                return Ok(None);
            }
            KeyCode::End
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                self.unscroll_timeline(usize::MAX / 2);
                return Ok(None);
            }
            KeyCode::Char('t')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.approval.has_actionable_pending() =>
            {
                if self.active_pane == PaneFocus::Activity
                    && self.timeline_state.selected_tool_activity_key.is_some()
                    && self.toggle_selected_tool_card()
                {
                    return Ok(None);
                }
                if self.toggle_task_strip_expansion() {
                    return Ok(None);
                }
                if self.active_pane != PaneFocus::Activity && self.has_collapsible_thinking_blocks()
                {
                    self.toggle_thinking_block_mode();
                    return Ok(None);
                }
                if self.timeline_state.selected_tool_activity_key.is_some()
                    && self.toggle_selected_tool_card()
                {
                    return Ok(None);
                }
                if self.has_collapsible_thinking_blocks() {
                    self.toggle_thinking_block_mode();
                }
                return Ok(None);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(None);
            }
            _ => {}
        }

        if let Some(outcome) = self.handle_composer_key_event(key)? {
            return Ok(outcome);
        }

        match key.code {
            KeyCode::BackTab if !self.approval.has_actionable_pending() => {
                return self.toggle_runtime_permission_mode();
            }
            KeyCode::PageUp => self.scroll_timeline(self.transcript_page_step()),
            KeyCode::PageDown => self.unscroll_timeline(self.transcript_page_step()),
            KeyCode::Home => self.scroll_timeline_to_top(),
            KeyCode::End => self.unscroll_timeline(usize::MAX / 2),
            KeyCode::Esc => {
                if self.composer.input.is_empty()
                    && self.handle_ui_command(UiCommand::ClearToolCardFocus)
                {
                    return Ok(None);
                }
                if self.cancel_queue_edit() {
                    self.clear_input_preserving_draft();
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    self.active_pane = PaneFocus::Composer;
                    return Ok(None);
                }
                if self.composer.input.is_empty() && self.composer.mode == ComposerMode::Plan {
                    self.composer.mode = ComposerMode::Build;
                    self.last_notice = Some("build mode".to_owned());
                    self.push_event("mode", "build");
                    return Ok(None);
                }
                self.blur_composer_aux_panels();
                self.clear_input_preserving_draft();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
                self.active_pane = PaneFocus::Composer;
            }
            _ => {}
        }
        Ok(None)
    }
}

fn is_composer_newline_key(key: KeyEvent) -> bool {
    (key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT))
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
}

fn is_composer_submit_key(key: KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
}

fn is_composer_text_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control())
}

fn has_control_without_alt(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

fn has_alt_without_control(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT) && !key.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
pub(crate) mod tests;
