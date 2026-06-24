use sigil_kernel::{
    ConversationInputQueueId, ConversationInputStatus, ConversationQueueItemProjection,
    ConversationQueueProjection,
};

use crate::{
    runner::QueueMoveDirection,
    slash::{ResolvedSlashCommand, SlashSelectorEntry},
    timeline::{ComposerQueueRow, TimelineRole},
    ui::StatusKind,
};

use super::{AppAction, AppState, ComposerQueueAction, PaneFocus};

const COMPOSER_QUEUE_VISIBLE_ROWS: usize = 4;

impl AppState {
    pub(crate) fn conversation_queue_projection(&self) -> ConversationQueueProjection {
        ConversationQueueProjection::from_entries(&self.current_session_entries)
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
                selected: index == self.composer_queue_selected,
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
        self.composer_queue_panel_focused
    }

    pub(crate) fn composer_queue_paused(&self) -> bool {
        self.conversation_queue_projection().paused
    }

    pub(crate) fn selected_composer_queue_action(&self) -> ComposerQueueAction {
        self.composer_queue_action_selected
    }

    pub(super) fn focus_composer_queue_panel(&mut self) -> bool {
        if !self.composer_queue_panel_available() {
            self.composer_queue_panel_focused = false;
            return false;
        }
        self.refresh_conversation_queue_selection();
        self.reset_composer_queue_action();
        self.composer_queue_panel_focused = true;
        self.blur_composer_agent_panel();
        self.last_notice = Some("queue focused".to_owned());
        true
    }

    pub(super) fn blur_composer_queue_panel(&mut self) {
        self.composer_queue_panel_focused = false;
    }

    pub(super) fn blur_composer_aux_panels(&mut self) {
        self.blur_composer_queue_panel();
        self.blur_composer_agent_panel();
    }

    pub(super) fn selected_composer_queue_is_first(&self) -> bool {
        self.composer_queue_selected == 0
    }

    pub(super) fn selected_composer_queue_is_last(&self) -> bool {
        let count = self.conversation_queue_projection().items.len();
        count == 0 || self.composer_queue_selected + 1 >= count.min(COMPOSER_QUEUE_VISIBLE_ROWS)
    }

    pub(super) fn move_composer_queue_selection(&mut self, next: bool) -> bool {
        let count = self.conversation_queue_projection().items.len();
        if count == 0 {
            self.composer_queue_panel_focused = false;
            return false;
        }
        let max_index = count.min(COMPOSER_QUEUE_VISIBLE_ROWS).saturating_sub(1);
        self.composer_queue_selected = if next {
            self.composer_queue_selected
                .saturating_add(1)
                .min(max_index)
        } else {
            self.composer_queue_selected.saturating_sub(1)
        };
        self.reset_composer_queue_action();
        true
    }

    pub(super) fn cycle_composer_queue_action(&mut self, forward: bool) {
        self.composer_queue_action_selected = self.composer_queue_action_selected.next(forward);
    }

    pub(super) fn execute_selected_queue_action(&mut self) -> Option<AppAction> {
        match self.composer_queue_action_selected {
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
        let queue_id = self.selected_queue_id()?;
        self.last_notice = Some("queued input moved to next turn".to_owned());
        Some(AppAction::PromoteQueuedConversationInput { queue_id })
    }

    pub(super) fn send_selected_queue_item_now(&mut self) -> Option<AppAction> {
        let queue_id = self.selected_queue_id()?;
        self.last_notice = Some("queued input sending now".to_owned());
        Some(AppAction::SendQueuedConversationInputNow { queue_id })
    }

    pub(super) fn cancel_selected_queue_item(&mut self) -> Option<AppAction> {
        let queue_id = self.selected_queue_id()?;
        self.last_notice = Some("queued input cancelled".to_owned());
        Some(AppAction::CancelQueuedConversationInput { queue_id })
    }

    pub(super) fn move_selected_queue_item(
        &mut self,
        direction: QueueMoveDirection,
    ) -> Option<AppAction> {
        let queue_id = self.selected_queue_id()?;
        Some(AppAction::MoveQueuedConversationInput {
            queue_id,
            direction,
        })
    }

    pub(super) fn begin_edit_selected_queue_item(&mut self) -> bool {
        let Some(item) = self.selected_queue_item() else {
            return false;
        };
        self.queue_edit_target = Some(item.queued.queue_id.clone());
        self.set_input_and_cursor(item.queued.prompt.clone());
        self.active_pane = PaneFocus::Composer;
        self.blur_composer_queue_panel();
        self.blur_composer_agent_panel();
        self.reset_slash_selector();
        self.reset_input_history_navigation();
        self.last_notice = Some("editing queued input".to_owned());
        self.push_event("queue:edit", item.queued.queue_id.as_str());
        true
    }

    pub(super) fn refresh_conversation_queue_selection(&mut self) {
        let projection = self.conversation_queue_projection();
        let visible_count = projection.items.len().min(COMPOSER_QUEUE_VISIBLE_ROWS);
        if visible_count == 0 {
            self.composer_queue_selected = 0;
            self.composer_queue_panel_focused = false;
            self.reset_composer_queue_action();
            self.queue_edit_target = None;
            return;
        }
        self.composer_queue_selected = self.composer_queue_selected.min(visible_count - 1);
        if let Some(target) = &self.queue_edit_target
            && !projection
                .items
                .iter()
                .any(|item| item.queued.queue_id == *target)
        {
            self.queue_edit_target = None;
        }
    }

    pub(super) fn finish_queue_edit_submission(&mut self, prompt: String) -> Option<AppAction> {
        let queue_id = self.queue_edit_target.take()?;
        self.input.clear();
        self.input_cursor = 0;
        self.input_paste_spans.clear();
        self.reset_slash_selector();
        self.reset_input_history_navigation();
        self.push_timeline(TimelineRole::Notice, "queued input edited");
        self.push_event("queue:edit-submit", queue_id.as_str());
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
            self.last_notice = Some("queue empty".to_owned());
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
            "now" | "send-now" => Ok(self.queue_action_for_target(target, |queue_id| {
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
                    self.composer_queue_selected = index;
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
                self.last_notice = Some("usage: /queue <show|next|now|edit|delete>".to_owned());
                Ok(None)
            }
        }
    }

    pub(super) fn cancel_queue_edit(&mut self) -> bool {
        if self.queue_edit_target.is_none() {
            return false;
        }
        self.queue_edit_target = None;
        self.last_notice = Some("queue edit cancelled".to_owned());
        true
    }

    fn composer_queue_panel_available(&self) -> bool {
        !self.conversation_queue_projection().items.is_empty()
    }

    fn reset_composer_queue_action(&mut self) {
        self.composer_queue_action_selected = ComposerQueueAction::SendNow;
    }

    fn selected_queue_id(&self) -> Option<ConversationInputQueueId> {
        self.selected_queue_item()
            .map(|item| item.queued.queue_id.clone())
    }

    fn selected_queue_item(&self) -> Option<ConversationQueueItemProjection> {
        self.conversation_queue_projection()
            .items
            .into_iter()
            .take(COMPOSER_QUEUE_VISIBLE_ROWS)
            .nth(self.composer_queue_selected)
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
        queue_id.map(build).or_else(|| {
            self.last_notice = Some("queue item not found".to_owned());
            None
        })
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
            return Some(self.composer_queue_selected);
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
}

fn queue_slash_options() -> [(&'static str, &'static str, &'static str); 5] {
    [
        ("show", "show", "focus queue panel"),
        ("next", "next", "run selected after current turn"),
        ("now", "now", "interrupt current turn and run selected"),
        ("edit", "edit", "edit selected queued input"),
        ("delete", "delete", "cancel selected queued input"),
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
        "{} · {}",
        status,
        match item.queued.kind {
            sigil_kernel::ConversationInputKind::Chat => "chat",
            sigil_kernel::ConversationInputKind::PlanPrompt => "plan",
            sigil_kernel::ConversationInputKind::AgentMention => "agent",
            sigil_kernel::ConversationInputKind::AgentMessage => "message",
            sigil_kernel::ConversationInputKind::Unknown => "unknown",
        }
    )
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
        ConversationInputStatus::Queued => "queued",
        ConversationInputStatus::Dispatching => "dispatching",
        ConversationInputStatus::Delivered => "delivered",
        ConversationInputStatus::Rejected => "rejected",
        ConversationInputStatus::Cancelled => "cancelled",
        ConversationInputStatus::Stale => "stale",
        ConversationInputStatus::Unknown => "unknown",
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/conversation_queue_flow_detail_tests.rs"]
mod tests;
