use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::*;
use crate::{EventId, MessageRole, ProjectionCursor, decode_stored_event};

/// Schema version for the durable-cursor safe-fold planning shape.
pub const COMPACTION_FOLD_PLAN_SCHEMA_VERSION: u16 = 1;
/// Schema version for the adaptive whole-turn tail selection evidence.
pub const ADAPTIVE_TAIL_SELECTION_SCHEMA_VERSION: u16 = 1;
/// Default number of complete turns that remain raw after a V3 rotation.
pub const DEFAULT_TAIL_MIN_COMPLETE_TURNS: usize = 2;
/// Default lower bound for the adaptive raw-tail target.
pub const DEFAULT_TAIL_TARGET_MIN_TOKENS: u64 = 8 * 1024;
/// Default ordinary upper bound for the adaptive raw-tail target.
pub const DEFAULT_TAIL_TARGET_MAX_TOKENS: u64 = 64 * 1024;
/// Default `2.0` multiplier, represented in parts per million for deterministic persistence.
pub const DEFAULT_TAIL_RECENT_TURN_P95_MULTIPLIER_PPM: u32 = 2_000_000;
/// Default `0.25` usable-context cap, represented in parts per million.
pub const DEFAULT_TAIL_MAX_USABLE_CONTEXT_RATIO_PPM: u32 = 250_000;
/// Number of recent complete turns sampled for the deterministic p95.
pub const DEFAULT_TAIL_RECENT_TURN_SAMPLE_LIMIT: usize = 20;
const TURN_MESSAGE_TOKEN_OVERHEAD: u64 = 16;

/// Stable reference to one raw durable event without copying its payload into a plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionEventRef {
    pub stream_sequence: u64,
    pub event_id: EventId,
}

/// Provider-neutral V3 policy for selecting a raw tail by complete turn and token target.
///
/// Floating-point policy values are persisted as parts per million so replay and stale-plan
/// validation produce byte-identical decisions on every supported platform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AdaptiveTailPolicyV3 {
    pub tail_min_complete_turns: usize,
    pub tail_target_min_tokens: u64,
    pub tail_target_max_tokens: u64,
    pub tail_recent_turn_p95_multiplier_ppm: u32,
    pub tail_max_usable_context_ratio_ppm: u32,
    pub recent_turn_sample_limit: usize,
}

impl Default for AdaptiveTailPolicyV3 {
    fn default() -> Self {
        Self {
            tail_min_complete_turns: DEFAULT_TAIL_MIN_COMPLETE_TURNS,
            tail_target_min_tokens: DEFAULT_TAIL_TARGET_MIN_TOKENS,
            tail_target_max_tokens: DEFAULT_TAIL_TARGET_MAX_TOKENS,
            tail_recent_turn_p95_multiplier_ppm: DEFAULT_TAIL_RECENT_TURN_P95_MULTIPLIER_PPM,
            tail_max_usable_context_ratio_ppm: DEFAULT_TAIL_MAX_USABLE_CONTEXT_RATIO_PPM,
            recent_turn_sample_limit: DEFAULT_TAIL_RECENT_TURN_SAMPLE_LIMIT,
        }
    }
}

impl AdaptiveTailPolicyV3 {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.tail_min_complete_turns < DEFAULT_TAIL_MIN_COMPLETE_TURNS
            || self.tail_target_min_tokens == 0
            || self.tail_target_max_tokens < self.tail_target_min_tokens
            || self.tail_recent_turn_p95_multiplier_ppm == 0
            || self.tail_max_usable_context_ratio_ppm == 0
            || self.tail_max_usable_context_ratio_ppm > 1_000_000
            || self.recent_turn_sample_limit == 0
        {
            bail!("adaptive tail policy is invalid");
        }
        Ok(())
    }
}

/// Completion state of one provider-visible turn group.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TailTurnStateV3 {
    Complete,
    Active,
}

/// Stable evidence for one complete or active turn retained as an atomic raw group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RetainedTurnGroupV3 {
    pub first_event: CompactionEventRef,
    pub last_event: CompactionEventRef,
    pub event_ids: Vec<EventId>,
    /// Deterministic UTF-8/wire-structure upper bound used only for tail planning.
    pub token_upper_bound: u64,
    pub state: TailTurnStateV3,
}

/// Self-contained proof of one V3 whole-turn raw-tail decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AdaptiveTailSelectionV3 {
    pub schema_version: u16,
    pub policy: AdaptiveTailPolicyV3,
    /// Tail budget after caller-owned static/provider/output/safety reservations.
    pub exact_fit_limit_tokens: u64,
    pub recent_complete_turn_p95_tokens: u64,
    pub ordinary_target_tokens: u64,
    /// Actual whole-turn target after an oversized active turn extension.
    pub effective_target_tokens: u64,
    pub folded_token_upper_bound: u64,
    pub retained_token_upper_bound: u64,
    pub protected_tail_token_upper_bound: u64,
    pub folded_complete_turns: usize,
    pub retained_complete_turns: usize,
    pub active_turn_extended: bool,
    pub exact_fit_saturated: bool,
    pub retained_turns: Vec<RetainedTurnGroupV3>,
    /// Controls or unsafe message groups at/after the raw-tail frontier that remain replayable
    /// outside the fold rather than being silently split.
    pub protected_tail_events: Vec<ProtectedCompactionEventRef>,
}

/// Why a durable event cannot be folded from the provider-visible raw history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CompactionFoldProtectionReason {
    /// This message is already represented by the active V2 checkpoint boundary and must not be
    /// folded into a later checkpoint a second time.
    ExistingCompactionBoundary,
    /// This is an append-only control entry whose state must remain independently replayable.
    ControlState,
    /// This direct or unknown non-critical event is not a provider-visible chat message.
    NonMessageDurableEvent,
    /// The message has malformed role/tool fields and cannot safely participate in a fold.
    MalformedMessage,
    /// A tool-call assistant message is missing, duplicates, or ambiguously binds a result.
    UnsafeToolPair,
    /// A tool result has no uniquely matching prior assistant tool call.
    UnpairedToolResult,
    /// A message belongs to a turn containing an unsafe or cross-turn tool pair.
    WholeTurnAtomicity,
    /// A message belongs to a turn with a requested approval or running tool execution.
    ActiveToolOrApproval,
    /// A provider-visible message appeared before any user turn boundary.
    OrphanTurnMessage,
}

/// One protected raw event and the reason it remains outside the fold range.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProtectedCompactionEventRef {
    pub event: CompactionEventRef,
    pub reason: CompactionFoldProtectionReason,
}

/// A replay-safe plan for folding only durable chat history.
///
/// This is intentionally a planning contract, not an apply operation: it never removes JSONL
/// records, changes the active V2 boundary, materializes a summary, or sends a provider request.
/// `base_stream_cursor`, the exact folded ids, and the exact retained ids make later apply-time
/// stale-plan checks possible without putting raw message contents in the plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionFoldPlan {
    pub schema_version: u16,
    /// Session scope shared by every event reference in this plan.
    pub session_id: crate::SessionId,
    /// Exact durable tail observed when the plan was built.
    pub base_stream_cursor: ProjectionCursor,
    /// The latest previously activated fold boundary, when this is a repeated compaction plan.
    ///
    /// All messages at or before this durable cursor are already represented by the previous
    /// checkpoint and therefore remain outside the next fold input.
    pub prior_folded_through: Option<CompactionCursor>,
    /// The greatest folded message cursor, when at least one message is safe to fold.
    pub folded_through: Option<CompactionCursor>,
    /// Provider-visible message event ids that a later checkpoint may cover.
    pub folded_event_ids: Vec<EventId>,
    /// Provider-visible message event ids that must remain raw in the next projection.
    pub retained_event_ids: Vec<EventId>,
    /// Controls, non-message events, and unsafe message pairs that are never candidates here.
    pub protected_events: Vec<ProtectedCompactionEventRef>,
    /// Exact whole-turn tail-selection proof.
    pub adaptive_tail: AdaptiveTailSelectionV3,
}

/// Read-only V2 compaction planning result for a durable session stream.
///
/// This contains only the safe fold decision and the currently activated V2 boundary. It does
/// not create an attempt, checkpoint, task memory, token estimate, or provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2CompactionPreview {
    /// Exact fold/retain/protection decision reconstructed from the durable stream.
    pub plan: CompactionFoldPlan,
    /// The active compaction that supplied the prior boundary, when one exists.
    pub active_compaction_id: Option<CompactionId>,
}

impl CompactionFoldPlan {
    /// Builds a V3 safe-fold plan whose raw tail consists only of complete turn groups.
    ///
    /// `exact_fit_limit_tokens` is a caller-computed tail budget after static request segments,
    /// provider state, output reservation, tool-growth reservation and safety margin. The planner
    /// uses a deterministic UTF-8/wire upper bound for relative tail selection; final activation
    /// still requires the exact frozen-request proof.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream or policy is invalid, an active/required atomic tail
    /// exceeds the supplied exact-fit limit, or the prior boundary is stale.
    pub fn from_records_after_adaptive_tail(
        records: &[SessionStreamRecord],
        policy: AdaptiveTailPolicyV3,
        exact_fit_limit_tokens: u64,
        prior_folded_through: Option<&CompactionCursor>,
    ) -> Result<Self> {
        policy.validate()?;
        if exact_fit_limit_tokens == 0 {
            bail!("adaptive tail exact-fit limit must be non-zero");
        }

        let (session_id, base_stream_cursor, messages, mut protected) =
            prepare_fold_candidates(records, prior_folded_through)?;
        let tool_groups = classify_tool_pair_groups(&messages, &mut protected);
        let pending_calls = pending_tool_or_approval_call_ids(records)?;
        let turns = classify_turn_groups(
            &messages,
            &tool_groups,
            &pending_calls,
            &mut protected,
            prior_folded_through,
        )?;
        let (retained_indexes, adaptive_tail) = select_adaptive_tail(
            &turns,
            &messages,
            &protected,
            policy,
            exact_fit_limit_tokens,
        )?;

        let mut folded_event_ids = Vec::new();
        let mut retained_event_ids = Vec::new();
        let mut folded_through = None;
        for (index, candidate) in messages.iter().enumerate() {
            if protected.contains_key(&candidate.event) {
                continue;
            }
            if retained_indexes.contains(&index) {
                retained_event_ids.push(candidate.event.event_id.clone());
            } else {
                folded_through = Some(CompactionCursor {
                    session_id: session_id.clone(),
                    through_stream_sequence: candidate.event.stream_sequence,
                    through_event_id: candidate.event.event_id.clone(),
                });
                folded_event_ids.push(candidate.event.event_id.clone());
            }
        }

        Ok(Self {
            schema_version: COMPACTION_FOLD_PLAN_SCHEMA_VERSION,
            session_id,
            base_stream_cursor,
            prior_folded_through: prior_folded_through.cloned(),
            folded_through,
            folded_event_ids,
            retained_event_ids,
            protected_events: protected
                .into_iter()
                .map(|(event, reason)| ProtectedCompactionEventRef { event, reason })
                .collect(),
            adaptive_tail,
        })
    }

    /// Rebuilds and compares the plan against the current complete durable stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the session changed or the rebuilt protection/fold decision differs.
    /// A later compaction apply must perform this check under its single-writer CAS boundary.
    pub fn validate_against(&self, records: &[SessionStreamRecord]) -> Result<()> {
        if self.schema_version != COMPACTION_FOLD_PLAN_SCHEMA_VERSION {
            bail!("unsupported compaction fold-plan schema version");
        }
        let current = Self::from_records_after_adaptive_tail(
            records,
            self.adaptive_tail.policy.clone(),
            self.adaptive_tail.exact_fit_limit_tokens,
            self.prior_folded_through.as_ref(),
        )?;
        if &current != self {
            bail!("compaction fold plan is stale against the current durable stream");
        }
        Ok(())
    }

    /// Returns whether this plan contains any newly safe foldable history.
    #[must_use]
    pub fn has_foldable_history(&self) -> bool {
        !self.folded_event_ids.is_empty()
    }
}

impl JsonlSessionStore {
    /// Rebuilds a read-only V3 whole-turn compaction preview from the durable stream.
    ///
    /// This shares the V2 lifecycle and active sidecar resolver; only the raw-tail selection is
    /// upgraded. It never appends an epoch, checkpoint, shrink sidecar or provider request.
    ///
    /// # Errors
    ///
    /// Returns an error for an unfinished attempt, invalid adaptive policy/budget, or malformed
    /// durable stream.
    pub fn adaptive_compaction_preview(
        &self,
        policy: AdaptiveTailPolicyV3,
        exact_fit_limit_tokens: u64,
        branch_id: Option<&str>,
    ) -> Result<Option<V2CompactionPreview>> {
        let records = Self::read_event_records(self.path())?;
        if records.is_empty() {
            return Ok(None);
        }
        let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
        if !lifecycle.unfinished_attempts().is_empty() {
            bail!("cannot preview adaptive compaction while another attempt is unfinished");
        }
        let sidecars = CompactionSidecarProjection::from_records(&records)?;
        let active = sidecars.latest_for_branch(branch_id);
        let prior_folded_through = active.map(|sidecar| sidecar.folded_through.clone());
        let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
            &records,
            policy,
            exact_fit_limit_tokens,
            prior_folded_through.as_ref(),
        )?;
        if !plan.has_foldable_history() {
            return Ok(None);
        }
        Ok(Some(V2CompactionPreview {
            plan,
            active_compaction_id: active.map(|sidecar| sidecar.compaction_id.clone()),
        }))
    }
}

fn validate_prior_folded_through(
    records: &[SessionStreamRecord],
    session_id: &str,
    prior_folded_through: Option<&CompactionCursor>,
) -> Result<()> {
    let Some(prior) = prior_folded_through else {
        return Ok(());
    };
    if prior.session_id != session_id || prior.through_stream_sequence == 0 {
        bail!("compaction fold-plan prior boundary has an invalid session or sequence");
    }
    let prior_index = usize::try_from(prior.through_stream_sequence - 1)
        .context("compaction fold-plan prior boundary sequence overflows usize")?;
    let record = records
        .get(prior_index)
        .context("compaction fold-plan prior boundary is outside the durable stream")?;
    if record.event_id() != prior.through_event_id {
        bail!("compaction fold-plan prior boundary event id does not match the durable stream");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct FoldMessage {
    event: CompactionEventRef,
    message: crate::ModelMessage,
}

type PreparedFoldCandidates = (
    crate::SessionId,
    ProjectionCursor,
    Vec<FoldMessage>,
    BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
);

#[derive(Debug, Clone)]
struct TurnGroup {
    indexes: Vec<usize>,
    token_upper_bound: u64,
    state: TailTurnStateV3,
    protected: bool,
}

fn prepare_fold_candidates(
    records: &[SessionStreamRecord],
    prior_folded_through: Option<&CompactionCursor>,
) -> Result<PreparedFoldCandidates> {
    let session_id = validate_complete_stream(records)?;
    validate_prior_folded_through(records, &session_id, prior_folded_through)?;
    let visible_promotions = super::conversation_promotion_projection::
        provider_visible_conversation_promotion_event_ids(records)?;
    let promoted_message_ids = records
        .iter()
        .filter(|record| visible_promotions.contains(record.event_id()))
        .filter_map(|record| session_entry_from_stored_event(record.stored_event()).transpose())
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion)) => {
                Some(promotion.durable_user_message.id)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut messages = Vec::new();
    let mut protected = BTreeMap::new();
    for record in records {
        let event = record.stored_event();
        decode_stored_event(event.clone())?;
        let reference = event_ref(event);
        match session_entry_from_stored_event(event)? {
            Some(SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion)))
                if visible_promotions.contains(&event.event_id) =>
            {
                messages.push(FoldMessage {
                    event: reference,
                    message: promotion.durable_user_message,
                });
            }
            Some(SessionLogEntry::User(message)) if promoted_message_ids.contains(&message.id) => {
                protected.insert(reference, CompactionFoldProtectionReason::ControlState);
            }
            Some(SessionLogEntry::User(message)) | Some(SessionLogEntry::Assistant(message)) => {
                messages.push(FoldMessage {
                    event: reference,
                    message,
                });
            }
            Some(SessionLogEntry::ToolResultV3(result)) => {
                messages.push(FoldMessage {
                    event: reference,
                    message: result.model_message()?,
                });
            }
            Some(SessionLogEntry::Control(_)) => {
                protected.insert(reference, CompactionFoldProtectionReason::ControlState);
            }
            None => {
                protected.insert(
                    reference,
                    CompactionFoldProtectionReason::NonMessageDurableEvent,
                );
            }
        }
    }
    if let Some(prior) = prior_folded_through {
        for candidate in &messages {
            if candidate.event.stream_sequence <= prior.through_stream_sequence {
                protected.insert(
                    candidate.event.clone(),
                    CompactionFoldProtectionReason::ExistingCompactionBoundary,
                );
            }
        }
    }
    let base_stream_cursor = records
        .last()
        .expect("validated complete stream is non-empty")
        .projection_cursor(COMPACTION_FOLD_PLAN_SCHEMA_VERSION);
    Ok((session_id, base_stream_cursor, messages, protected))
}

fn pending_tool_or_approval_call_ids(records: &[SessionStreamRecord]) -> Result<BTreeSet<String>> {
    let mut pending = BTreeSet::new();
    let mut session_entries = Vec::new();
    for record in records {
        let entry = session_entry_from_stored_event(record.stored_event())?;
        if let Some(entry) = entry.as_ref() {
            session_entries.push(entry.clone());
        }
        match entry {
            Some(SessionLogEntry::Control(ControlEntry::ToolApproval(approval))) => {
                match approval.action {
                    ToolApprovalAuditAction::Requested => {
                        pending.insert(approval.call_id);
                    }
                    ToolApprovalAuditAction::Resolved | ToolApprovalAuditAction::PreviewFailed => {
                        pending.remove(&approval.call_id);
                    }
                    ToolApprovalAuditAction::DecisionAccepted => {}
                }
            }
            Some(SessionLogEntry::Control(ControlEntry::ToolExecution(execution))) => {
                match execution.status {
                    ToolExecutionStatus::Started => {
                        pending.insert(execution.call_id);
                    }
                    ToolExecutionStatus::Completed
                    | ToolExecutionStatus::Failed
                    | ToolExecutionStatus::Cancelled
                    | ToolExecutionStatus::Interrupted => {
                        pending.remove(&execution.call_id);
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    let user_inputs = crate::UserInputProjectionV1::from_session_entries(&session_entries)?;
    for state in user_inputs.pending() {
        if let Some(binding) = state.requested.request.continuation.as_ref() {
            pending.insert(binding.tool_call_id.clone());
        }
    }
    Ok(pending)
}

fn classify_turn_groups(
    messages: &[FoldMessage],
    tool_groups: &[BTreeSet<usize>],
    pending_calls: &BTreeSet<String>,
    protected: &mut BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
    prior_folded_through: Option<&CompactionCursor>,
) -> Result<Vec<TurnGroup>> {
    let first_current_sequence = prior_folded_through
        .map(|cursor| cursor.through_stream_sequence.saturating_add(1))
        .unwrap_or(1);
    let mut raw_groups = Vec::<Vec<usize>>::new();
    let mut current = Vec::new();
    for (index, candidate) in messages.iter().enumerate() {
        if candidate.event.stream_sequence < first_current_sequence {
            continue;
        }
        if matches!(candidate.message.role, MessageRole::User) {
            if !current.is_empty() {
                raw_groups.push(std::mem::take(&mut current));
            }
            current.push(index);
        } else if current.is_empty() {
            protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::OrphanTurnMessage,
            );
        } else {
            current.push(index);
        }
    }
    if !current.is_empty() {
        raw_groups.push(current);
    }

    let mut turn_by_message = BTreeMap::<usize, usize>::new();
    for (turn_index, indexes) in raw_groups.iter().enumerate() {
        for index in indexes {
            turn_by_message.insert(*index, turn_index);
        }
    }
    for tool_group in tool_groups {
        let owner_turns = tool_group
            .iter()
            .filter_map(|index| turn_by_message.get(index).copied())
            .collect::<BTreeSet<_>>();
        if owner_turns.len() > 1 {
            for turn_index in owner_turns {
                for index in &raw_groups[turn_index] {
                    protect(
                        protected,
                        &messages[*index],
                        CompactionFoldProtectionReason::WholeTurnAtomicity,
                    );
                }
            }
        }
    }

    let last_turn_index = raw_groups.len().saturating_sub(1);
    let mut turns = Vec::with_capacity(raw_groups.len());
    for (turn_index, indexes) in raw_groups.into_iter().enumerate() {
        let terminally_complete = indexes
            .last()
            .is_some_and(|index| is_terminal_assistant_message(&messages[*index].message));
        let state = if terminally_complete {
            TailTurnStateV3::Complete
        } else {
            TailTurnStateV3::Active
        };
        let has_pending_call = indexes.iter().any(|index| {
            messages[*index]
                .message
                .tool_calls
                .iter()
                .any(|call| pending_calls.contains(&call.id))
                || messages[*index]
                    .message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|call_id| pending_calls.contains(call_id))
        });
        if has_pending_call {
            for index in &indexes {
                // Active ownership is the strongest reason: unlike an ordinary unmatched tool
                // pair, approval and user-input suspension must remain a visible raw frontier.
                protected.insert(
                    messages[*index].event.clone(),
                    CompactionFoldProtectionReason::ActiveToolOrApproval,
                );
            }
        }
        if state == TailTurnStateV3::Active && turn_index != last_turn_index {
            for index in &indexes {
                protect(
                    protected,
                    &messages[*index],
                    CompactionFoldProtectionReason::WholeTurnAtomicity,
                );
            }
        }
        if indexes
            .iter()
            .any(|index| protected.contains_key(&messages[*index].event))
        {
            for index in &indexes {
                protect(
                    protected,
                    &messages[*index],
                    CompactionFoldProtectionReason::WholeTurnAtomicity,
                );
            }
        }
        let token_upper_bound = indexes.iter().try_fold(0_u64, |total, index| {
            total
                .checked_add(message_token_upper_bound(&messages[*index].message)?)
                .context("adaptive tail turn token upper bound overflowed")
        })?;
        let is_protected = indexes
            .iter()
            .any(|index| protected.contains_key(&messages[*index].event));
        turns.push(TurnGroup {
            indexes,
            token_upper_bound,
            state,
            protected: is_protected,
        });
    }
    Ok(turns)
}

fn is_terminal_assistant_message(message: &crate::ModelMessage) -> bool {
    matches!(message.role, MessageRole::Assistant)
        && message.tool_calls.is_empty()
        && !matches!(
            message.assistant_kind,
            Some(
                crate::AssistantMessageKind::ToolPreamble
                    | crate::AssistantMessageKind::Progress
                    | crate::AssistantMessageKind::ReasoningTrace
            )
        )
}

fn message_token_upper_bound(message: &crate::ModelMessage) -> Result<u64> {
    let mut tokens = TURN_MESSAGE_TOKEN_OVERHEAD
        .checked_add(message.id.len() as u64)
        .and_then(|value| value.checked_add(message.content.as_deref().map_or(0, str::len) as u64))
        .and_then(|value| {
            value.checked_add(message.tool_call_id.as_deref().map_or(0, str::len) as u64)
        })
        .context("adaptive tail message token upper bound overflowed")?;
    for call in &message.tool_calls {
        tokens = tokens
            .checked_add(TURN_MESSAGE_TOKEN_OVERHEAD)
            .and_then(|value| value.checked_add(call.id.len() as u64))
            .and_then(|value| value.checked_add(call.name.len() as u64))
            .and_then(|value| value.checked_add(call.args_json.len() as u64))
            .context("adaptive tail tool-call token upper bound overflowed")?;
    }
    for attachment in &message.image_attachments {
        tokens = tokens
            .checked_add(TURN_MESSAGE_TOKEN_OVERHEAD)
            .and_then(|value| value.checked_add(attachment.estimated_visual_tokens))
            .context("adaptive tail image token upper bound overflowed")?;
    }
    Ok(tokens.max(1))
}

fn select_adaptive_tail(
    turns: &[TurnGroup],
    messages: &[FoldMessage],
    protected: &BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
    policy: AdaptiveTailPolicyV3,
    exact_fit_limit_tokens: u64,
) -> Result<(BTreeSet<usize>, AdaptiveTailSelectionV3)> {
    let mut complete_samples = turns
        .iter()
        .rev()
        .filter(|turn| turn.state == TailTurnStateV3::Complete && !turn.protected)
        .take(policy.recent_turn_sample_limit)
        .map(|turn| turn.token_upper_bound)
        .collect::<Vec<_>>();
    complete_samples.sort_unstable();
    let recent_complete_turn_p95_tokens = nearest_rank_percentile(&complete_samples, 95);
    let multiplied_p95 = (u128::from(recent_complete_turn_p95_tokens)
        * u128::from(policy.tail_recent_turn_p95_multiplier_ppm))
    .div_ceil(1_000_000) as u64;
    let ratio_cap = (u128::from(exact_fit_limit_tokens)
        * u128::from(policy.tail_max_usable_context_ratio_ppm)
        / 1_000_000) as u64;
    let ordinary_ceiling = policy.tail_target_max_tokens.min(ratio_cap).max(1);
    let ordinary_floor = policy.tail_target_min_tokens.min(ordinary_ceiling);
    let ordinary_target_tokens = multiplied_p95.max(ordinary_floor).min(ordinary_ceiling);

    let mut retained_indexes = BTreeSet::new();
    let mut protected_tail_token_upper_bound = 0_u64;
    for turn in turns.iter().filter(|turn| turn.protected) {
        protected_tail_token_upper_bound = protected_tail_token_upper_bound
            .checked_add(turn.token_upper_bound)
            .context("adaptive protected-tail token upper bound overflowed")?;
    }
    if protected_tail_token_upper_bound > exact_fit_limit_tokens {
        bail!("protected atomic tail exceeds the adaptive exact-fit limit");
    }

    let active_turn = turns
        .last()
        .filter(|turn| turn.state == TailTurnStateV3::Active);
    let active_turn_tokens = active_turn.map_or(0, |turn| turn.token_upper_bound);
    let mut retained_token_upper_bound = 0_u64;
    if let Some(active) = active_turn.filter(|turn| !turn.protected) {
        for index in &active.indexes {
            retained_indexes.insert(*index);
        }
        retained_token_upper_bound = active.token_upper_bound;
    }
    let mut required_raw_tokens = protected_tail_token_upper_bound
        .checked_add(retained_token_upper_bound)
        .context("adaptive required-tail token upper bound overflowed")?;
    if required_raw_tokens > exact_fit_limit_tokens {
        bail!("active turn exceeds the adaptive exact-fit limit");
    }

    let mut retained_complete_turns = 0;
    let mut exact_fit_saturated = false;
    for turn in turns
        .iter()
        .rev()
        .filter(|turn| turn.state == TailTurnStateV3::Complete && !turn.protected)
    {
        if retained_complete_turns >= policy.tail_min_complete_turns
            && required_raw_tokens >= ordinary_target_tokens
        {
            break;
        }
        let next_required = required_raw_tokens
            .checked_add(turn.token_upper_bound)
            .context("adaptive retained-tail token upper bound overflowed")?;
        if next_required > exact_fit_limit_tokens {
            if retained_complete_turns < policy.tail_min_complete_turns {
                bail!("minimum complete-turn tail exceeds the adaptive exact-fit limit");
            }
            exact_fit_saturated = true;
            break;
        }
        for index in &turn.indexes {
            retained_indexes.insert(*index);
        }
        retained_token_upper_bound = retained_token_upper_bound
            .checked_add(turn.token_upper_bound)
            .context("adaptive retained-tail token upper bound overflowed")?;
        required_raw_tokens = next_required;
        retained_complete_turns += 1;
    }

    let active_turn_extended = active_turn_tokens > ordinary_target_tokens;
    let effective_target_tokens = ordinary_target_tokens
        .max(required_raw_tokens)
        .min(exact_fit_limit_tokens);
    let mut folded_token_upper_bound = 0_u64;
    let mut folded_complete_turns = 0_usize;
    for turn in turns
        .iter()
        .filter(|turn| turn.state == TailTurnStateV3::Complete && !turn.protected)
        .filter(|turn| {
            !turn
                .indexes
                .iter()
                .any(|index| retained_indexes.contains(index))
        })
    {
        folded_token_upper_bound = folded_token_upper_bound
            .checked_add(turn.token_upper_bound)
            .context("adaptive folded-tail token upper bound overflowed")?;
        folded_complete_turns += 1;
    }
    let retained_turns = turns
        .iter()
        .filter(|turn| {
            !turn.protected
                && turn
                    .indexes
                    .iter()
                    .any(|index| retained_indexes.contains(index))
        })
        .map(|turn| {
            let first = turn.indexes.first().expect("classified turn is non-empty");
            let last = turn.indexes.last().expect("classified turn is non-empty");
            RetainedTurnGroupV3 {
                first_event: messages[*first].event.clone(),
                last_event: messages[*last].event.clone(),
                event_ids: turn
                    .indexes
                    .iter()
                    .map(|index| messages[*index].event.event_id.clone())
                    .collect(),
                token_upper_bound: turn.token_upper_bound,
                state: turn.state,
            }
        })
        .collect::<Vec<_>>();
    let raw_frontier = turns
        .iter()
        .filter(|turn| {
            turn.protected
                || turn
                    .indexes
                    .iter()
                    .any(|index| retained_indexes.contains(index))
        })
        .filter_map(|turn| turn.indexes.first())
        .map(|index| messages[*index].event.stream_sequence)
        .min()
        .unwrap_or(u64::MAX);
    let protected_tail_events = protected
        .iter()
        .filter(|(event, reason)| {
            event.stream_sequence >= raw_frontier
                && **reason != CompactionFoldProtectionReason::ExistingCompactionBoundary
        })
        .map(|(event, reason)| ProtectedCompactionEventRef {
            event: event.clone(),
            reason: reason.clone(),
        })
        .collect();

    Ok((
        retained_indexes,
        AdaptiveTailSelectionV3 {
            schema_version: ADAPTIVE_TAIL_SELECTION_SCHEMA_VERSION,
            policy,
            exact_fit_limit_tokens,
            recent_complete_turn_p95_tokens,
            ordinary_target_tokens,
            effective_target_tokens,
            folded_token_upper_bound,
            retained_token_upper_bound,
            protected_tail_token_upper_bound,
            folded_complete_turns,
            retained_complete_turns,
            active_turn_extended,
            exact_fit_saturated,
            retained_turns,
            protected_tail_events,
        },
    ))
}

fn nearest_rank_percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percentile).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn validate_complete_stream(records: &[SessionStreamRecord]) -> Result<crate::SessionId> {
    let Some(first) = records.first() else {
        bail!("cannot build a compaction fold plan for an empty durable stream");
    };
    let session_id = first.session_id().to_owned();
    if session_id.trim().is_empty() {
        bail!("compaction fold-plan stream session id is empty");
    }
    for (offset, record) in records.iter().enumerate() {
        let event = record.stored_event();
        event
            .verify_record_checksum()
            .context("compaction fold-plan record checksum is invalid")?;
        if event.session_id != session_id {
            bail!("compaction fold-plan stream spans multiple sessions");
        }
        let expected_sequence = offset as u64 + 1;
        if event.stream_sequence != expected_sequence {
            bail!("compaction fold-plan stream sequence is not contiguous");
        }
        if event.event_id.trim().is_empty() {
            bail!("compaction fold-plan event id is empty");
        }
    }
    Ok(session_id)
}

fn event_ref(event: &crate::StoredEvent) -> CompactionEventRef {
    CompactionEventRef {
        stream_sequence: event.stream_sequence,
        event_id: event.event_id.clone(),
    }
}

fn classify_tool_pair_groups(
    messages: &[FoldMessage],
    protected: &mut BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
) -> Vec<BTreeSet<usize>> {
    let mut owners = BTreeMap::<String, usize>::new();
    let mut calls_by_assistant = BTreeMap::<usize, Vec<String>>::new();

    for (index, candidate) in messages.iter().enumerate() {
        let message = &candidate.message;
        match message.role {
            MessageRole::User => {
                if !message.tool_calls.is_empty() || message.tool_call_id.is_some() {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::MalformedMessage,
                    );
                }
            }
            MessageRole::Assistant => {
                if message.tool_call_id.is_some() {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::MalformedMessage,
                    );
                    continue;
                }
                if message.tool_calls.is_empty() {
                    continue;
                }
                let mut call_ids = BTreeSet::new();
                let mut malformed = false;
                for call in &message.tool_calls {
                    if call.id.trim().is_empty() || !call_ids.insert(call.id.clone()) {
                        malformed = true;
                        continue;
                    }
                    if let Some(previous) = owners.insert(call.id.clone(), index) {
                        malformed = true;
                        protect(
                            protected,
                            &messages[previous],
                            CompactionFoldProtectionReason::UnsafeToolPair,
                        );
                    }
                }
                if malformed {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::UnsafeToolPair,
                    );
                } else {
                    calls_by_assistant.insert(index, call_ids.into_iter().collect());
                }
            }
            MessageRole::Tool => {
                if !message.tool_calls.is_empty()
                    || message
                        .tool_call_id
                        .as_deref()
                        .is_none_or(|tool_call_id| tool_call_id.trim().is_empty())
                {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::MalformedMessage,
                    );
                }
            }
            MessageRole::System => protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::MalformedMessage,
            ),
        }
    }

    let mut results_by_call = BTreeMap::<String, Vec<usize>>::new();
    for (index, candidate) in messages.iter().enumerate() {
        let message = &candidate.message;
        if !matches!(message.role, MessageRole::Tool) || protected.contains_key(&candidate.event) {
            continue;
        }
        let Some(call_id) = message.tool_call_id.as_deref() else {
            continue;
        };
        results_by_call
            .entry(call_id.to_owned())
            .or_default()
            .push(index);
    }

    let mut groups = Vec::new();
    for (assistant_index, call_ids) in &calls_by_assistant {
        let assistant = &messages[*assistant_index];
        let mut group = BTreeSet::from([*assistant_index]);
        let mut safe = !protected.contains_key(&assistant.event);
        for call_id in call_ids {
            let result_indexes = results_by_call.get(call_id).cloned().unwrap_or_default();
            if result_indexes.len() != 1 || result_indexes[0] <= *assistant_index {
                safe = false;
                for result_index in result_indexes {
                    protect(
                        protected,
                        &messages[result_index],
                        CompactionFoldProtectionReason::UnsafeToolPair,
                    );
                }
                continue;
            }
            group.insert(result_indexes[0]);
        }
        if safe {
            groups.push(group);
        } else {
            protect(
                protected,
                assistant,
                CompactionFoldProtectionReason::UnsafeToolPair,
            );
        }
    }

    for (index, candidate) in messages.iter().enumerate() {
        if !matches!(candidate.message.role, MessageRole::Tool)
            || protected.contains_key(&candidate.event)
        {
            continue;
        }
        let Some(call_id) = candidate.message.tool_call_id.as_deref() else {
            continue;
        };
        let Some(owner) = owners.get(call_id) else {
            protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::UnpairedToolResult,
            );
            continue;
        };
        if protected.contains_key(&messages[*owner].event)
            || !calls_by_assistant.contains_key(owner)
            || index <= *owner
        {
            protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::UnsafeToolPair,
            );
        }
    }
    groups
}

fn protect(
    protected: &mut BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
    candidate: &FoldMessage,
    reason: CompactionFoldProtectionReason,
) {
    protected.entry(candidate.event.clone()).or_insert(reason);
}

#[cfg(test)]
#[path = "tests/compaction_plan_tests.rs"]
mod tests;
