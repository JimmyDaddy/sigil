use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, ConversationInputKind, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputStatus, ConversationInputTarget, ConversationQueueItemProjection,
    ConversationQueueProjection, SessionLogEntry,
};

use crate::{
    runner::QueueMoveDirection,
    slash::{ResolvedSlashCommand, SlashSelectorEntry},
    timeline::{ComposerQueueRow, TimelineRole},
    ui::StatusKind,
};

use super::{AgentView, AppAction, AppState, ComposerQueueAction, PaneFocus};

const COMPOSER_QUEUE_VISIBLE_ROWS: usize = 4;
const OPTIMISTIC_QUEUE_ID_PREFIX: &str = "ui_pending_";

impl AppState {
    pub(crate) fn conversation_queue_projection(&self) -> ConversationQueueProjection {
        let Some(visible_target) = self.active_conversation_queue_target() else {
            return ConversationQueueProjection::default();
        };

        let mut entries = self.session_browser.current_entries.clone();
        for (index, queued) in self.composer.optimistic_queue_items.iter().enumerate() {
            if optimistic_queue_item_confirmed_by_durable_projection(
                queued,
                index,
                &self.composer.optimistic_queue_items,
                &self.session_browser.current_entries,
            ) {
                continue;
            }
            entries.push(SessionLogEntry::Control(
                ControlEntry::ConversationInputQueued(queued.clone()),
            ));
        }
        let mut projection = ConversationQueueProjection::from_entries(&entries);
        projection
            .items
            .retain(|item| item.queued.target == visible_target);
        if !projection.items.iter().any(|item| {
            projection
                .next_dispatchable
                .as_ref()
                .is_some_and(|queue_id| item.queued.queue_id == *queue_id)
        }) {
            projection.next_dispatchable = None;
        }
        projection
    }

    pub(super) fn active_conversation_queue_target(&self) -> Option<ConversationInputTarget> {
        match &self.agent_panel.active_view {
            AgentView::Main => Some(ConversationInputTarget::MainThread),
            AgentView::Child { .. } => self.active_agent_thread_projection().map(|thread| {
                ConversationInputTarget::AgentThread {
                    thread_id: thread.thread_id,
                }
            }),
        }
    }

    pub(super) fn active_conversation_queue_submission(
        &self,
    ) -> (ConversationInputKind, ConversationInputTarget) {
        let target = self
            .active_conversation_queue_target()
            .unwrap_or(ConversationInputTarget::MainThread);
        let kind = match &target {
            ConversationInputTarget::MainThread => ConversationInputKind::Chat,
            ConversationInputTarget::AgentThread { .. } => ConversationInputKind::AgentMessage,
        };
        (kind, target)
    }

    pub(super) fn push_optimistic_conversation_queue_item(
        &mut self,
        prompt: String,
        kind: ConversationInputKind,
        target: ConversationInputTarget,
    ) {
        let queue_id = self.next_optimistic_queue_id();
        let prompt = sigil_kernel::safe_persistence_text(&prompt);
        self.composer
            .optimistic_queue_items
            .push(ConversationInputQueuedEntry {
                queue_id,
                target,
                kind,
                prompt_hash: conversation_prompt_hash(&prompt),
                prompt,
                reasoning_effort: Some(self.runtime.reasoning_effort.clone()),
                created_at_ms: None,
            });
        self.refresh_conversation_queue_selection();
    }

    pub(super) fn reconcile_optimistic_conversation_queue_items(&mut self) {
        if self.composer.optimistic_queue_items.is_empty() {
            return;
        }

        let optimistic_items = std::mem::take(&mut self.composer.optimistic_queue_items);
        let mut retained = Vec::new();
        for queued in optimistic_items {
            let confirmed_count =
                active_durable_queue_match_count(&queued, &self.session_browser.current_entries);
            let retained_match_count = retained
                .iter()
                .filter(|retained| queued_inputs_match(retained, &queued))
                .count();
            if retained_match_count >= confirmed_count {
                retained.push(queued);
            }
        }
        self.composer.optimistic_queue_items = retained;
    }

    pub(crate) fn composer_queue_rows(&self) -> Vec<ComposerQueueRow> {
        let projection = self.conversation_queue_projection();
        projection
            .items
            .into_iter()
            .enumerate()
            .take(COMPOSER_QUEUE_VISIBLE_ROWS)
            .map(|(index, item)| ComposerQueueRow {
                label: queue_prompt_label(&item),
                detail: queue_item_detail(&item, projection.paused),
                status: queue_status_kind(item.status, projection.paused),
                selected: index == self.composer.queue_selected,
            })
            .collect()
    }

    pub(crate) fn queue_strip_rows(&self) -> u16 {
        let item_count = self.conversation_queue_projection().items.len();
        if item_count == 0 {
            return 0;
        }
        2 + item_count.min(COMPOSER_QUEUE_VISIBLE_ROWS) as u16
    }

    pub(crate) fn is_composer_queue_panel_focused(&self) -> bool {
        self.composer.queue_panel_focused
    }

    pub(crate) fn composer_queue_paused(&self) -> bool {
        self.conversation_queue_projection().paused
    }

    pub(crate) fn composer_queue_summary(&self) -> Option<String> {
        let projection = self.conversation_queue_projection();
        let count = projection.items.len();
        let next = projection.items.first()?;
        let noun = queue_summary_noun(next);
        let plural = if count == 1 { "" } else { "s" };
        Some(format!(
            "{count} {noun}{plural} pending · next {}: {}",
            queue_target_label(&next.queued.target),
            queue_prompt_preview(next)
        ))
    }

    pub(crate) fn selected_composer_queue_action(&self) -> ComposerQueueAction {
        self.composer.queue_action_selected
    }

    pub(super) fn focus_composer_queue_panel(&mut self) -> bool {
        if !self.composer_queue_panel_available() {
            self.composer.queue_panel_focused = false;
            return false;
        }
        self.refresh_conversation_queue_selection();
        self.reset_composer_queue_action();
        self.composer.queue_panel_focused = true;
        self.blur_composer_agent_panel();
        self.last_notice = Some("follow-ups focused".to_owned());
        true
    }

    pub(super) fn blur_composer_queue_panel(&mut self) {
        self.composer.queue_panel_focused = false;
    }

    pub(super) fn blur_composer_aux_panels(&mut self) {
        self.blur_composer_queue_panel();
        self.blur_composer_agent_panel();
    }

    pub(super) fn move_composer_queue_selection(&mut self, next: bool) -> bool {
        let count = self.conversation_queue_projection().items.len();
        if count == 0 {
            self.composer.queue_panel_focused = false;
            return false;
        }
        let max_index = count.min(COMPOSER_QUEUE_VISIBLE_ROWS).saturating_sub(1);
        self.composer.queue_selected = if next {
            self.composer
                .queue_selected
                .saturating_add(1)
                .min(max_index)
        } else {
            self.composer.queue_selected.saturating_sub(1)
        };
        self.reset_composer_queue_action();
        true
    }

    pub(super) fn cycle_composer_queue_action(&mut self, forward: bool) {
        self.composer.queue_action_selected = self.composer.queue_action_selected.next(forward);
    }

    pub(super) fn execute_selected_queue_action(&mut self) -> Option<AppAction> {
        match self.composer.queue_action_selected {
            ComposerQueueAction::SendNow => self.send_selected_queue_item_now(),
            ComposerQueueAction::KeepNext => self.promote_selected_queue_item(),
            ComposerQueueAction::Edit => {
                self.begin_edit_selected_queue_item();
                None
            }
            ComposerQueueAction::Delete => self.cancel_selected_queue_item(),
        }
    }

    pub(super) fn promote_selected_queue_item(&mut self) -> Option<AppAction> {
        let queue_id = self.selected_confirmed_queue_id()?;
        self.last_notice = Some("follow-up will run next".to_owned());
        Some(AppAction::PromoteQueuedConversationInput { queue_id })
    }

    pub(super) fn send_selected_queue_item_now(&mut self) -> Option<AppAction> {
        let queue_id = self.selected_confirmed_queue_id()?;
        self.last_notice = Some("interrupting current turn for follow-up".to_owned());
        Some(AppAction::SendQueuedConversationInputNow { queue_id })
    }

    pub(super) fn cancel_selected_queue_item(&mut self) -> Option<AppAction> {
        let queue_id = self.selected_confirmed_queue_id()?;
        self.last_notice = Some("follow-up removed".to_owned());
        Some(AppAction::CancelQueuedConversationInput { queue_id })
    }

    pub(super) fn move_selected_queue_item(
        &mut self,
        direction: QueueMoveDirection,
    ) -> Option<AppAction> {
        let queue_id = self.selected_confirmed_queue_id()?;
        Some(AppAction::MoveQueuedConversationInput {
            queue_id,
            direction,
        })
    }

    pub(super) fn begin_edit_selected_queue_item(&mut self) -> bool {
        let Some(item) = self.selected_queue_item() else {
            return false;
        };
        if is_optimistic_queue_id(&item.queued.queue_id) {
            self.last_notice = Some("follow-up is being saved".to_owned());
            return false;
        }
        self.composer.queue_edit_target = Some(item.queued.queue_id.clone());
        self.set_input_and_cursor(item.queued.prompt.clone());
        self.active_pane = PaneFocus::Composer;
        self.blur_composer_queue_panel();
        self.blur_composer_agent_panel();
        self.reset_slash_selector();
        self.reset_input_history_navigation();
        self.last_notice = Some("editing follow-up".to_owned());
        self.push_event("follow-up:edit", item.queued.queue_id.as_str());
        true
    }

    pub(super) fn refresh_conversation_queue_selection(&mut self) {
        let projection = self.conversation_queue_projection();
        let visible_count = projection.items.len().min(COMPOSER_QUEUE_VISIBLE_ROWS);
        if visible_count == 0 {
            self.composer.queue_selected = 0;
            self.composer.queue_panel_focused = false;
            self.reset_composer_queue_action();
            self.composer.queue_edit_target = None;
            return;
        }
        self.composer.queue_selected = self.composer.queue_selected.min(visible_count - 1);
        if let Some(target) = &self.composer.queue_edit_target
            && !projection
                .items
                .iter()
                .any(|item| item.queued.queue_id == *target)
        {
            self.composer.queue_edit_target = None;
        }
    }

    pub(super) fn finish_queue_edit_submission(&mut self, prompt: String) -> Option<AppAction> {
        let queue_id = self.composer.queue_edit_target.take()?;
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.reset_slash_selector();
        self.reset_input_history_navigation();
        self.push_timeline(TimelineRole::Notice, "follow-up edited");
        self.push_event("follow-up:edit-submit", queue_id.as_str());
        Some(AppAction::EditQueuedConversationInput { queue_id, prompt })
    }

    pub(super) fn queue_slash_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let query = arg.trim().to_ascii_lowercase();
        queue_slash_options()
            .into_iter()
            .filter(|(action, _, _)| query.is_empty() || action.starts_with(&query))
            .map(|(action, label, description)| SlashSelectorEntry {
                fill: format!("/queue {action}"),
                label: label.to_owned(),
                description: description.to_owned(),
                resolved: ResolvedSlashCommand {
                    canonical: "/queue".to_owned(),
                    arg: action.to_owned(),
                },
            })
            .collect()
    }

    pub(super) fn execute_queue_slash_command(
        &mut self,
        arg: &str,
    ) -> anyhow::Result<Option<AppAction>> {
        let value = arg.trim();
        if value.is_empty()
            || value.eq_ignore_ascii_case("show")
            || value.eq_ignore_ascii_case("focus")
        {
            if self.focus_composer_queue_panel() {
                return Ok(None);
            }
            self.last_notice = Some("no follow-ups pending".to_owned());
            return Ok(None);
        }
        let mut parts = value.split_whitespace();
        let action = parts.next().unwrap_or_default().to_ascii_lowercase();
        let target = parts.next().unwrap_or_default();
        match action.as_str() {
            "pause" => Ok(self.toggle_queue_pause_to(true)),
            "resume" => Ok(self.toggle_queue_pause_to(false)),
            "next" | "send" => Ok(self.queue_action_for_target(target, |queue_id| {
                AppAction::PromoteQueuedConversationInput { queue_id }
            })),
            "now" | "send-now" | "interrupt" => Ok(self
                .queue_action_for_target(target, |queue_id| {
                    AppAction::SendQueuedConversationInputNow { queue_id }
                })),
            "delete" | "cancel" | "remove" => Ok(self
                .queue_action_for_target(target, |queue_id| {
                    AppAction::CancelQueuedConversationInput { queue_id }
                })),
            "edit" => {
                if !target.is_empty()
                    && let Some(index) = self.queue_index_for_target(target)
                {
                    self.composer.queue_selected = index;
                }
                if self.begin_edit_selected_queue_item() {
                    Ok(None)
                } else {
                    self.last_notice = Some("queue item not found".to_owned());
                    Ok(None)
                }
            }
            "up" => Ok(self.queue_action_for_target(target, |queue_id| {
                AppAction::MoveQueuedConversationInput {
                    queue_id,
                    direction: QueueMoveDirection::Up,
                }
            })),
            "down" => Ok(self.queue_action_for_target(target, |queue_id| {
                AppAction::MoveQueuedConversationInput {
                    queue_id,
                    direction: QueueMoveDirection::Down,
                }
            })),
            _ => {
                self.last_notice =
                    Some("usage: /queue <show|next|interrupt|edit|delete>".to_owned());
                Ok(None)
            }
        }
    }

    pub(super) fn cancel_queue_edit(&mut self) -> bool {
        if self.composer.queue_edit_target.is_none() {
            return false;
        }
        self.composer.queue_edit_target = None;
        self.last_notice = Some("follow-up edit cancelled".to_owned());
        true
    }

    fn composer_queue_panel_available(&self) -> bool {
        !self.conversation_queue_projection().items.is_empty()
    }

    fn reset_composer_queue_action(&mut self) {
        self.composer.queue_action_selected = ComposerQueueAction::KeepNext;
    }

    fn selected_queue_id(&self) -> Option<ConversationInputQueueId> {
        self.selected_queue_item()
            .map(|item| item.queued.queue_id.clone())
    }

    fn selected_confirmed_queue_id(&mut self) -> Option<ConversationInputQueueId> {
        let queue_id = self.selected_queue_id()?;
        if is_optimistic_queue_id(&queue_id) {
            self.last_notice = Some("follow-up is being saved".to_owned());
            return None;
        }
        Some(queue_id)
    }

    fn selected_queue_item(&self) -> Option<ConversationQueueItemProjection> {
        self.conversation_queue_projection()
            .items
            .into_iter()
            .take(COMPOSER_QUEUE_VISIBLE_ROWS)
            .nth(self.composer.queue_selected)
    }

    fn toggle_queue_pause_to(&mut self, paused: bool) -> Option<AppAction> {
        self.last_notice = Some(if paused {
            "queue paused".to_owned()
        } else {
            "queue resumed".to_owned()
        });
        Some(AppAction::SetConversationQueuePaused { paused })
    }

    fn queue_action_for_target(
        &mut self,
        target: &str,
        build: impl FnOnce(ConversationInputQueueId) -> AppAction,
    ) -> Option<AppAction> {
        let queue_id = if target.is_empty() {
            self.selected_queue_id()
        } else {
            self.queue_id_for_target(target)
        };
        match queue_id {
            Some(queue_id) if is_optimistic_queue_id(&queue_id) => {
                self.last_notice = Some("follow-up is being saved".to_owned());
                None
            }
            Some(queue_id) => Some(build(queue_id)),
            None => {
                self.last_notice = Some("queue item not found".to_owned());
                None
            }
        }
    }

    fn queue_id_for_target(&self, target: &str) -> Option<ConversationInputQueueId> {
        self.queue_index_for_target(target).and_then(|index| {
            self.conversation_queue_projection()
                .items
                .get(index)
                .map(|item| item.queued.queue_id.clone())
        })
    }

    fn queue_index_for_target(&self, target: &str) -> Option<usize> {
        let normalized = target.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Some(self.composer.queue_selected);
        }
        let projection = self.conversation_queue_projection();
        if let Ok(ordinal) = normalized.parse::<usize>()
            && ordinal > 0
            && ordinal <= projection.items.len()
        {
            return Some(ordinal - 1);
        }
        projection.items.iter().position(|item| {
            item.queued
                .queue_id
                .as_str()
                .eq_ignore_ascii_case(&normalized)
                || queue_prompt_label(item)
                    .to_ascii_lowercase()
                    .contains(&normalized)
        })
    }

    fn next_optimistic_queue_id(&mut self) -> ConversationInputQueueId {
        loop {
            let next = self.composer.next_optimistic_queue_id;
            self.composer.next_optimistic_queue_id =
                self.composer.next_optimistic_queue_id.saturating_add(1);
            let candidate = format!("{OPTIMISTIC_QUEUE_ID_PREFIX}{next}");
            if let Ok(queue_id) = ConversationInputQueueId::new(&candidate) {
                return queue_id;
            }
        }
    }
}

fn optimistic_queue_item_confirmed_by_durable_projection(
    queued: &ConversationInputQueuedEntry,
    index: usize,
    optimistic_items: &[ConversationInputQueuedEntry],
    durable_entries: &[SessionLogEntry],
) -> bool {
    let confirmed_count = active_durable_queue_match_count(queued, durable_entries);
    let previous_optimistic_match_count = optimistic_items
        .iter()
        .take(index)
        .filter(|candidate| queued_inputs_match(candidate, queued))
        .count();
    previous_optimistic_match_count < confirmed_count
}

fn active_durable_queue_match_count(
    queued: &ConversationInputQueuedEntry,
    entries: &[SessionLogEntry],
) -> usize {
    ConversationQueueProjection::from_entries(entries)
        .items
        .iter()
        .filter(|item| queued_inputs_match(&item.queued, queued))
        .count()
}

fn queued_inputs_match(
    left: &ConversationInputQueuedEntry,
    right: &ConversationInputQueuedEntry,
) -> bool {
    left.target == right.target
        && left.kind == right.kind
        && left.prompt == right.prompt
        && left.reasoning_effort == right.reasoning_effort
}

fn is_optimistic_queue_id(queue_id: &ConversationInputQueueId) -> bool {
    queue_id.as_str().starts_with(OPTIMISTIC_QUEUE_ID_PREFIX)
}

fn conversation_prompt_hash(prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn queue_slash_options() -> [(&'static str, &'static str, &'static str); 5] {
    [
        ("show", "show", "focus follow-up panel"),
        ("next", "next", "run selected after current turn"),
        (
            "interrupt",
            "interrupt",
            "stop current turn and run selected",
        ),
        ("edit", "edit", "edit selected follow-up"),
        ("delete", "delete", "cancel selected follow-up"),
    ]
}

fn queue_prompt_label(item: &ConversationQueueItemProjection) -> String {
    item.queued
        .prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("(empty)")
        .to_owned()
}

fn queue_item_detail(item: &ConversationQueueItemProjection, paused: bool) -> String {
    let status = if paused && item.status == ConversationInputStatus::Queued {
        "paused"
    } else {
        queue_status_label(item.status)
    };
    format!(
        "{} · {} · {}",
        status,
        queue_target_label(&item.queued.target),
        queue_kind_label(item.queued.kind),
    )
}

fn queue_target_label(target: &sigil_kernel::ConversationInputTarget) -> String {
    match target {
        sigil_kernel::ConversationInputTarget::MainThread => "main".to_owned(),
        sigil_kernel::ConversationInputTarget::AgentThread { thread_id } => {
            format!("agent {}", thread_id.as_str())
        }
    }
}

fn queue_kind_label(kind: sigil_kernel::ConversationInputKind) -> &'static str {
    match kind {
        sigil_kernel::ConversationInputKind::Chat => "follow-up",
        sigil_kernel::ConversationInputKind::PlanPrompt => "plan",
        sigil_kernel::ConversationInputKind::AgentMention => "agent",
        sigil_kernel::ConversationInputKind::AgentMessage => "message",
        sigil_kernel::ConversationInputKind::Unknown => "unknown",
    }
}

fn queue_prompt_preview(item: &ConversationQueueItemProjection) -> String {
    const MAX_CHARS: usize = 48;
    let label = queue_prompt_label(item);
    let mut preview = label.chars().take(MAX_CHARS).collect::<String>();
    if label.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
}

fn queue_status_kind(status: ConversationInputStatus, paused: bool) -> StatusKind {
    if paused && status == ConversationInputStatus::Queued {
        return StatusKind::Warning;
    }
    match status {
        ConversationInputStatus::Queued => StatusKind::Pending,
        ConversationInputStatus::Dispatching => StatusKind::Running,
        ConversationInputStatus::Delivered => StatusKind::Success,
        ConversationInputStatus::Rejected
        | ConversationInputStatus::Cancelled
        | ConversationInputStatus::Stale => StatusKind::Error,
        ConversationInputStatus::Unknown => StatusKind::Unknown,
    }
}

fn queue_status_label(status: ConversationInputStatus) -> &'static str {
    match status {
        ConversationInputStatus::Queued => "pending",
        ConversationInputStatus::Dispatching => "dispatching",
        ConversationInputStatus::Delivered => "delivered",
        ConversationInputStatus::Rejected => "rejected",
        ConversationInputStatus::Cancelled => "cancelled",
        ConversationInputStatus::Stale => "stale",
        ConversationInputStatus::Unknown => "unknown",
    }
}

fn queue_summary_noun(item: &ConversationQueueItemProjection) -> &'static str {
    match &item.queued.target {
        sigil_kernel::ConversationInputTarget::MainThread => "follow-up",
        sigil_kernel::ConversationInputTarget::AgentThread { .. } => "agent message",
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/conversation_queue_flow_detail_tests.rs"]
mod tests;
