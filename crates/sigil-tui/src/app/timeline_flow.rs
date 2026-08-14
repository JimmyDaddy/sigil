use std::{collections::BTreeSet, ops::Range};

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    AgentView, AppAction, AppState, ApprovalPresentationState, EventEntry, LiveActivitySummary,
    PaneFocus, RunPhase, ThinkingBlockMode, TimelineEntry, TimelineRole, TimelineTextSelection,
    agent_flow::agent_thread_sidebar_detail,
    formatting::{
        line_has_visible_content, plain_line_text, sidebar_width_for_terminal,
        truncate_session_view_text,
    },
    task_sidebar::task_child_session_status_label,
};

const EVENT_DETAIL_MAX_CHARS: usize = 240;

#[derive(Debug, Clone)]
pub(super) enum TimelineHistoryAnchor {
    Main {
        entry_index: usize,
        line_offset: usize,
        entry_content_offset: usize,
        absolute_start: usize,
    },
    Child {
        session_ref: String,
        entry_identity: Option<(TimelineRole, String)>,
        previous_entry_identity: Option<(TimelineRole, String)>,
        next_entry_identity: Option<(TimelineRole, String)>,
        retained_entry_index: Option<usize>,
        entry_line_offset: usize,
        entry_content_offset: usize,
        body_line_offset: usize,
        body_content_offset: usize,
    },
}

impl TimelineHistoryAnchor {
    fn after_removing_timeline_entries(mut self, removed_indices: &[usize]) -> Self {
        if let Self::Main {
            entry_index,
            line_offset,
            entry_content_offset,
            ..
        } = &mut self
        {
            let anchored_entry_was_removed = removed_indices.contains(entry_index);
            *entry_index = entry_index.saturating_sub(
                removed_indices
                    .iter()
                    .filter(|removed| **removed < *entry_index)
                    .count(),
            );
            if anchored_entry_was_removed {
                *line_offset = 0;
                *entry_content_offset = 0;
            }
        }
        self
    }
}

impl AppState {
    pub(super) fn timeline_viewport_rows(&self) -> usize {
        crate::ui::live_transcript_rows_for_app(
            Rect::new(0, 0, self.terminal_width, self.terminal_height),
            self,
        )
    }

    pub(super) fn max_timeline_scroll_back(&self) -> usize {
        let total = self.effective_timeline_render_len();
        let viewport = self.timeline_viewport_rows().max(1);
        total.saturating_sub(viewport)
    }

    pub(super) fn effective_timeline_render_len(&self) -> usize {
        if matches!(self.agent_panel.active_view, AgentView::Child { .. }) {
            return self
                .render_child_agent_transcript_lines()
                .iter()
                .rposition(line_has_visible_content)
                .map(|index| index + 1)
                .unwrap_or(0);
        }
        self.main_timeline_render_len()
    }

    fn main_timeline_render_len(&self) -> usize {
        let snapshot = self.timeline_state.render_store.snapshot();
        snapshot
            .lines_range(0..snapshot.total_lines())
            .iter()
            .rposition(line_has_visible_content)
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    pub(crate) fn main_timeline_active(&self) -> bool {
        matches!(self.agent_panel.active_view, AgentView::Main)
    }

    pub(super) fn transcript_page_step(&self) -> usize {
        (self.timeline_viewport_rows() / 2).max(1)
    }

    pub(super) fn push_timeline(&mut self, role: TimelineRole, text: impl Into<String>) {
        self.flush_deferred_timeline_renders();
        self.clear_timeline_text_selection_state();
        let is_tool = role == TimelineRole::Tool;
        let previous_selected_tool = self.timeline_state.selected_tool_activity_key.clone();
        let entry_index = self.timeline.len();
        let assistant_before_tool = is_tool
            .then(|| self.latest_assistant_entry_index_before(entry_index))
            .flatten();
        self.timeline.push(TimelineEntry {
            role,
            text: text.into(),
        });
        if let Some(index) = assistant_before_tool
            && self.assistant_entry_is_intermediate_info(index)
        {
            self.rerender_timeline_entry(index);
        }
        if is_tool
            && let Some(entry) = self.timeline.last()
            && let Some(activity) = self.tool_activity_cache_entry(entry_index, entry)
        {
            self.timeline_state.selected_tool_activity_key = Some(activity.key.clone());
            self.timeline_state.tool_activity_cache.push(activity);
        }
        if is_tool
            && previous_selected_tool != self.timeline_state.selected_tool_activity_key
            && let Some(previous_index) = previous_selected_tool
                .as_deref()
                .and_then(|key| self.timeline_entry_index_for_activity_key(key))
            && previous_index < self.timeline.len().saturating_sub(1)
        {
            self.rerender_timeline_entry(previous_index);
        }
        // Default-open file diffs can be large, so new output should not force
        // every historical activity through JSON parsing and diff rendering.
        self.append_timeline_render_store_entry(self.timeline.len().saturating_sub(1));
    }

    pub(super) fn push_event(&mut self, label: impl Into<String>, detail: impl Into<String>) {
        self.events.push(EventEntry {
            label: label.into(),
            detail: bounded_event_detail(detail.into()),
        });
        if self.events.len() > 400 {
            self.events.remove(0);
        }
    }

    pub(super) fn append_assistant_delta(&mut self, delta: &str) {
        if delta.is_empty()
            || (self.timeline_state.streaming_assistant_index.is_none() && delta.trim().is_empty())
        {
            return;
        }
        self.finish_streaming_reasoning_entry();
        if let Some(index) = self.timeline_state.streaming_assistant_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry_deferred(index);
            return;
        }

        self.push_timeline(TimelineRole::Assistant, delta);
        self.timeline_state.streaming_assistant_index = self.timeline.len().checked_sub(1);
    }

    pub(super) fn push_assistant_message_once(&mut self, content: String) {
        if content.is_empty() || self.assistant_message_seen_since_last_user(&content) {
            return;
        }
        self.push_timeline(TimelineRole::Assistant, content);
    }

    pub(super) fn push_final_assistant_message_once(&mut self, content: String) {
        self.finish_streaming_reasoning_entry();
        self.push_assistant_message_once(content);
    }

    fn assistant_message_seen_since_last_user(&self, content: &str) -> bool {
        self.timeline
            .iter()
            .rev()
            .take_while(|entry| entry.role != TimelineRole::User)
            .any(|entry| entry.role == TimelineRole::Assistant && entry.text == content)
    }

    pub(super) fn append_reasoning_delta(&mut self, delta: &str) {
        if delta.is_empty()
            || (self.timeline_state.streaming_reasoning_index.is_none() && delta.trim().is_empty())
        {
            return;
        }
        self.finish_streaming_assistant_entry();
        if let Some(index) = self.timeline_state.streaming_reasoning_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry_deferred(index);
            return;
        }

        self.push_timeline(TimelineRole::Thinking, delta);
        self.timeline_state.streaming_reasoning_index = self.timeline.len().checked_sub(1);
    }

    pub(super) fn finish_streaming_reasoning_entry(&mut self) {
        if let Some(index) = self.timeline_state.streaming_reasoning_index.take() {
            self.rerender_timeline_entry_deferred(index);
        }
    }

    pub(super) fn discard_streaming_reasoning_entry(&mut self) {
        let Some(index) = self.timeline_state.streaming_reasoning_index.take() else {
            return;
        };
        if self
            .timeline
            .get(index)
            .is_none_or(|entry| entry.role != TimelineRole::Thinking)
        {
            return;
        }
        let history_anchor = self.capture_timeline_history_anchor();
        self.timeline.remove(index);
        self.rebuild_timeline_projection_after_entry_removal(history_anchor, &[index]);
    }

    pub(super) fn rebuild_timeline_projection_after_entry_removal(
        &mut self,
        history_anchor: Option<TimelineHistoryAnchor>,
        removed_indices: &[usize],
    ) {
        let history_anchor =
            history_anchor.map(|anchor| anchor.after_removing_timeline_entries(removed_indices));
        self.remap_tool_activity_state_after_entry_removal(removed_indices);
        self.timeline_state.streaming_assistant_index = None;
        self.timeline_state.streaming_reasoning_index = None;
        self.timeline_state.expanded_thinking_entry_indices.clear();
        self.timeline_state.collapsed_thinking_entry_indices.clear();
        self.timeline_state.expanded_diagram_entry_indices.clear();
        self.timeline_state.deferred_render_indexes.clear();
        self.rebuild_tool_activity_cache();
        self.clear_timeline_text_selection_state();
        let options = self.timeline_render_options();
        self.timeline_state
            .render_store
            .rebuild(&self.timeline, &options);
        self.timeline_state.revision = self.timeline_state.revision.saturating_add(1);
        self.restore_timeline_history_anchor(history_anchor);
    }

    fn remap_tool_activity_state_after_entry_removal(&mut self, removed_indices: &[usize]) {
        self.tool_progress_entry_indices.retain(|_, entry_index| {
            if removed_indices.contains(entry_index) {
                return false;
            }
            let shift = removed_indices
                .iter()
                .filter(|removed_index| **removed_index < *entry_index)
                .count();
            *entry_index = (*entry_index).saturating_sub(shift);
            true
        });
        self.timeline_state.selected_tool_activity_key = self
            .timeline_state
            .selected_tool_activity_key
            .take()
            .and_then(|key| remap_tool_activity_key_after_entry_removal(key, removed_indices));
        self.timeline_state.expanded_tool_activity_keys =
            std::mem::take(&mut self.timeline_state.expanded_tool_activity_keys)
                .into_iter()
                .filter_map(|key| remap_tool_activity_key_after_entry_removal(key, removed_indices))
                .collect();
        self.timeline_state.collapsed_tool_activity_keys =
            std::mem::take(&mut self.timeline_state.collapsed_tool_activity_keys)
                .into_iter()
                .filter_map(|key| remap_tool_activity_key_after_entry_removal(key, removed_indices))
                .collect();
        self.timeline_state.tool_activity_visible_rows =
            std::mem::take(&mut self.timeline_state.tool_activity_visible_rows)
                .into_iter()
                .filter_map(|(key, rows)| {
                    remap_tool_activity_key_after_entry_removal(key, removed_indices)
                        .map(|key| (key, rows))
                })
                .collect();
    }

    pub(super) fn push_phase_marker(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.runtime.last_phase_marker.as_deref() == Some(text.as_str()) {
            return;
        }
        self.runtime.last_phase_marker = Some(text.clone());
        self.push_event("phase", text);
    }

    pub(super) fn toggle_thinking_block_mode(&mut self) {
        self.thinking_block_mode = match self.thinking_block_mode {
            ThinkingBlockMode::Collapsed => ThinkingBlockMode::Expanded,
            ThinkingBlockMode::Expanded => ThinkingBlockMode::Collapsed,
        };
        self.timeline_state.expanded_thinking_entry_indices.clear();
        self.timeline_state.collapsed_thinking_entry_indices.clear();
        self.rebuild_timeline_render_surfaces();
        self.last_notice = Some(format!("thinking {}", self.thinking_block_mode.as_str()));
        self.push_event("thinking:view", self.thinking_block_mode.as_str());
    }

    pub(crate) fn toggle_thinking_entry(&mut self, entry_index: usize) -> bool {
        let Some(entry) = self.timeline.get(entry_index) else {
            return false;
        };
        if entry.role != TimelineRole::Thinking
            || !crate::ui::thinking_has_collapsed_content(&entry.text)
        {
            return false;
        }

        let expanded = self.thinking_entry_is_expanded(entry_index);
        if expanded {
            self.timeline_state
                .expanded_thinking_entry_indices
                .remove(&entry_index);
            self.timeline_state
                .collapsed_thinking_entry_indices
                .insert(entry_index);
        } else {
            self.timeline_state
                .collapsed_thinking_entry_indices
                .remove(&entry_index);
            self.timeline_state
                .expanded_thinking_entry_indices
                .insert(entry_index);
        }
        self.rerender_timeline_entry(entry_index);
        let state = if expanded { "collapsed" } else { "expanded" };
        self.last_notice = Some(format!("thinking {state}"));
        self.push_event("thinking:entry", format!("{entry_index} {state}"));
        true
    }

    pub(crate) fn toggle_latest_diagram_entry(&mut self) -> bool {
        let Some(entry_index) =
            self.timeline
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, entry)| {
                    (entry.role == TimelineRole::Assistant
                        && crate::ui::markdown_contains_mermaid_diagram(&entry.text))
                    .then_some(index)
                })
        else {
            self.last_notice = Some("no diagram source yet".to_owned());
            return false;
        };
        let expanded = !self
            .timeline_state
            .expanded_diagram_entry_indices
            .remove(&entry_index);
        if expanded {
            self.timeline_state
                .expanded_diagram_entry_indices
                .insert(entry_index);
        }
        self.rerender_timeline_entry(entry_index);
        let state = if expanded { "expanded" } else { "collapsed" };
        self.last_notice = Some(format!("diagram source {state} · Ctrl-L copies response"));
        self.push_event("diagram:view", state);
        true
    }

    pub(super) fn has_collapsible_thinking_blocks(&self) -> bool {
        self.timeline
            .iter()
            .enumerate()
            .any(|(index, entry)| self.thinking_entry_is_collapsible(index, entry))
    }

    pub(crate) fn collapsible_thinking_entry_indices(&self) -> Vec<usize> {
        self.timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                self.thinking_entry_is_collapsible(index, entry)
                    .then_some(index)
            })
            .collect()
    }

    fn thinking_entry_is_collapsible(&self, entry_index: usize, entry: &TimelineEntry) -> bool {
        entry.role == TimelineRole::Thinking
            && self.timeline_state.streaming_reasoning_index != Some(entry_index)
            && crate::ui::thinking_has_collapsed_content(&entry.text)
    }

    fn thinking_entry_is_expanded(&self, entry_index: usize) -> bool {
        self.timeline_state.streaming_reasoning_index == Some(entry_index)
            || self
                .timeline_state
                .expanded_thinking_entry_indices
                .contains(&entry_index)
            || (matches!(self.thinking_block_mode, ThinkingBlockMode::Expanded)
                && !self
                    .timeline_state
                    .collapsed_thinking_entry_indices
                    .contains(&entry_index))
    }

    pub(super) fn rebuild_timeline_render_store(&mut self) {
        let history_anchor = self.capture_timeline_history_anchor();
        self.clear_timeline_text_selection_state();
        self.timeline_state.deferred_render_indexes.clear();
        let options = self.timeline_render_options();
        self.timeline_state
            .render_store
            .rebuild(&self.timeline, &options);
        self.timeline_state.revision = self.timeline_state.revision.saturating_add(1);
        self.restore_timeline_history_anchor(history_anchor);
    }

    pub(super) fn rebuild_timeline_render_surfaces(&mut self) {
        self.rebuild_timeline_render_store();
        self.rerender_active_agent_child_transcript();
    }

    pub(super) fn rerender_timeline_entry(&mut self, index: usize) {
        let history_anchor = self.capture_timeline_history_anchor();
        self.clear_timeline_text_selection_state();
        self.timeline_state.deferred_render_indexes.remove(&index);
        let options = self.timeline_render_options();
        self.timeline_state
            .render_store
            .rerender_entry(&self.timeline, index, &options);
        self.timeline_state.revision = self.timeline_state.revision.saturating_add(1);
        self.restore_timeline_history_anchor(history_anchor);
    }

    fn rerender_timeline_entry_deferred(&mut self, index: usize) {
        if self.timeline_state.defer_renders {
            self.timeline_state.deferred_render_indexes.insert(index);
            return;
        }
        self.rerender_timeline_entry(index);
    }

    pub fn begin_timeline_render_batch(&mut self) {
        self.timeline_state.defer_renders = true;
    }

    pub fn flush_timeline_render_batch(&mut self) -> bool {
        self.timeline_state.defer_renders = false;
        self.flush_deferred_timeline_renders()
    }

    pub(super) fn finish_streaming_assistant_entry(&mut self) {
        let Some(index) = self.timeline_state.streaming_assistant_index.take() else {
            return;
        };
        self.timeline_state.deferred_render_indexes.remove(&index);
        let Some(entry) = self.timeline.get(index) else {
            return;
        };
        if entry.text.trim().is_empty() {
            let history_anchor = self.capture_timeline_history_anchor();
            self.timeline.remove(index);
            self.rebuild_timeline_projection_after_entry_removal(history_anchor, &[index]);
            return;
        }
        self.rerender_timeline_entry(index);
    }

    pub(super) fn discard_streaming_assistant_entry(&mut self) {
        let Some(index) = self.timeline_state.streaming_assistant_index.take() else {
            return;
        };
        self.timeline_state.deferred_render_indexes.remove(&index);
        if self
            .timeline
            .get(index)
            .is_none_or(|entry| entry.role != TimelineRole::Assistant)
        {
            return;
        }
        let history_anchor = self.capture_timeline_history_anchor();
        self.timeline.remove(index);
        self.rebuild_timeline_projection_after_entry_removal(history_anchor, &[index]);
    }

    pub(super) fn downgrade_streaming_assistant_entry_to_thinking(&mut self) {
        let Some(index) = self.timeline_state.streaming_assistant_index else {
            return;
        };
        let Some(entry) = self.timeline.get(index) else {
            return;
        };
        if entry.role != TimelineRole::Assistant {
            return;
        }
        if entry.text.trim().is_empty() {
            let history_anchor = self.capture_timeline_history_anchor();
            self.timeline_state.streaming_assistant_index = None;
            self.timeline.remove(index);
            self.rebuild_timeline_projection_after_entry_removal(history_anchor, &[index]);
            return;
        }
        if let Some(entry) = self.timeline.get_mut(index) {
            entry.role = TimelineRole::Thinking;
        }
    }

    pub(super) fn flush_deferred_timeline_renders(&mut self) -> bool {
        if self.timeline_state.deferred_render_indexes.is_empty() {
            return false;
        }
        let indexes = std::mem::take(&mut self.timeline_state.deferred_render_indexes);
        for index in indexes {
            if index < self.timeline.len() {
                self.rerender_timeline_entry(index);
            }
        }
        true
    }

    pub(super) fn append_timeline_render_store_entry(&mut self, index: usize) {
        let history_anchor = self.capture_timeline_history_anchor();
        self.clear_timeline_text_selection_state();
        let options = self.timeline_render_options();
        self.timeline_state
            .render_store
            .append_entry(&self.timeline, index, &options);
        self.timeline_state.revision = self.timeline_state.revision.saturating_add(1);
        self.restore_timeline_history_anchor(history_anchor);
    }

    pub(super) fn reset_scroll(&mut self) {
        self.return_timeline_to_live_tail();
        self.approval.scroll_back = 0;
        self.activity_scroll_back = 0;
    }

    pub(super) fn scroll_timeline(&mut self, delta: usize) {
        if delta == 0 {
            return;
        }
        self.timeline_scroll_back = self
            .timeline_scroll_back
            .saturating_add(delta)
            .min(self.max_timeline_scroll_back());
    }

    pub(super) fn unscroll_timeline(&mut self, delta: usize) {
        self.timeline_scroll_back = self.timeline_scroll_back.saturating_sub(delta);
    }

    pub(super) fn scroll_timeline_to_top(&mut self) {
        self.timeline_scroll_back = self.max_timeline_scroll_back();
    }

    pub(super) fn return_timeline_to_live_tail(&mut self) {
        self.timeline_scroll_back = 0;
    }

    pub(super) fn timeline_at_live_tail(&self) -> bool {
        self.timeline_scroll_back == 0
    }

    pub(super) fn capture_timeline_history_anchor(&self) -> Option<TimelineHistoryAnchor> {
        if self.timeline_at_live_tail() {
            return None;
        }
        match &self.agent_panel.active_view {
            AgentView::Main => {
                let snapshot = self.timeline_state.render_store.snapshot();
                let start = self
                    .visible_timeline_render_range(self.timeline_viewport_rows())
                    .start;
                let entry_index = snapshot.entry_at_line(start)?;
                let entry_range = snapshot.range_for_entry(entry_index)?;
                let line_offset = start.saturating_sub(entry_range.start);
                let entry_content_offset = snapshot
                    .plain_lines_range(entry_range.clone())
                    .iter()
                    .take(line_offset)
                    .map(|line| timeline_history_line_weight(line))
                    .sum();
                Some(TimelineHistoryAnchor::Main {
                    entry_index,
                    line_offset,
                    entry_content_offset,
                    absolute_start: start,
                })
            }
            AgentView::Child {
                child_session_ref, ..
            } => {
                let (header, body) = self.render_child_agent_transcript_sections();
                let max_lines = self.timeline_viewport_rows();
                if max_lines <= header.len() {
                    return None;
                }
                let effective_len = child_transcript_effective_len(&body);
                let viewport = max_lines.saturating_sub(header.len()).max(1);
                let scroll_back = self
                    .timeline_scroll_back
                    .min(effective_len.saturating_sub(viewport));
                let end = effective_len.saturating_sub(scroll_back);
                let start = end.saturating_sub(viewport);
                let body_content_offset = body
                    .iter()
                    .take(start)
                    .map(|line| timeline_history_line_weight(&plain_line_text(line)))
                    .sum();
                let (
                    entry_identity,
                    previous_entry_identity,
                    next_entry_identity,
                    retained_entry_index,
                    entry_line_offset,
                    entry_content_offset,
                ) = self
                    .agent_panel
                    .active_child_transcript
                    .as_ref()
                    .and_then(|transcript| {
                        let (projected_lines, entry_ranges) = self
                            .render_child_timeline_body_projection(&transcript.timeline_entries);
                        let (local_index, range) = entry_ranges
                            .iter()
                            .enumerate()
                            .find(|(_, range)| !range.is_empty() && range.end > start)?;
                        let anchored_line = start.max(range.start).min(range.end);
                        let entry_line_offset = anchored_line.saturating_sub(range.start);
                        let entry_content_offset = projected_lines
                            .get(range.start..anchored_line)
                            .unwrap_or_default()
                            .iter()
                            .map(|line| timeline_history_line_weight(&plain_line_text(line)))
                            .sum();
                        Some((
                            transcript
                                .timeline_entries
                                .get(local_index)
                                .map(|entry| (entry.role, entry.text.clone())),
                            local_index.checked_sub(1).and_then(|index| {
                                transcript
                                    .timeline_entries
                                    .get(index)
                                    .map(|entry| (entry.role, entry.text.clone()))
                            }),
                            transcript
                                .timeline_entries
                                .get(local_index.saturating_add(1))
                                .map(|entry| (entry.role, entry.text.clone())),
                            Some(local_index),
                            entry_line_offset,
                            entry_content_offset,
                        ))
                    })
                    .unwrap_or((None, None, None, None, 0, 0));
                Some(TimelineHistoryAnchor::Child {
                    session_ref: child_session_ref.as_path().display().to_string(),
                    entry_identity,
                    previous_entry_identity,
                    next_entry_identity,
                    retained_entry_index,
                    entry_line_offset,
                    entry_content_offset,
                    body_line_offset: start,
                    body_content_offset,
                })
            }
        }
    }

    pub(super) fn restore_timeline_history_anchor(
        &mut self,
        anchor: Option<TimelineHistoryAnchor>,
    ) {
        let Some(anchor) = anchor else {
            return;
        };
        match anchor {
            TimelineHistoryAnchor::Main {
                entry_index,
                line_offset,
                entry_content_offset,
                absolute_start,
            } => {
                if !matches!(self.agent_panel.active_view, AgentView::Main) {
                    return;
                }
                let snapshot = self.timeline_state.render_store.snapshot();
                let effective_len = self.main_timeline_render_len().min(snapshot.total_lines());
                let desired_start = snapshot
                    .range_for_entry(entry_index)
                    .map(|range| {
                        let available = range.end.saturating_sub(range.start);
                        let lines = snapshot.plain_lines_range(range.clone());
                        let mut remaining = entry_content_offset;
                        let logical_line_offset = lines
                            .iter()
                            .enumerate()
                            .find_map(|(index, line)| {
                                let weight = timeline_history_line_weight(line);
                                if remaining < weight {
                                    Some(index)
                                } else {
                                    remaining = remaining.saturating_sub(weight);
                                    None
                                }
                            })
                            .unwrap_or(line_offset);
                        range
                            .start
                            .saturating_add(logical_line_offset.min(available.saturating_sub(1)))
                    })
                    .unwrap_or(absolute_start)
                    .min(effective_len);
                let viewport = self.timeline_viewport_rows().max(1);
                let desired_end = desired_start.saturating_add(viewport).min(effective_len);
                self.timeline_scroll_back = effective_len.saturating_sub(desired_end);
            }
            TimelineHistoryAnchor::Child {
                session_ref,
                entry_identity,
                previous_entry_identity,
                next_entry_identity,
                retained_entry_index,
                entry_line_offset,
                entry_content_offset,
                body_line_offset,
                body_content_offset,
            } => {
                let AgentView::Child {
                    child_session_ref, ..
                } = &self.agent_panel.active_view
                else {
                    return;
                };
                if child_session_ref.as_path().display().to_string() != session_ref {
                    return;
                }
                let (header, body) = self.render_child_agent_transcript_sections();
                let max_lines = self.timeline_viewport_rows();
                if max_lines <= header.len() {
                    self.timeline_scroll_back = 0;
                    return;
                }
                let effective_len = child_transcript_effective_len(&body);
                let entry_start =
                    self.agent_panel
                        .active_child_transcript
                        .as_ref()
                        .and_then(|transcript| {
                            let local_index = entry_identity
                                .as_ref()
                                .and_then(|identity| {
                                    transcript
                                        .timeline_entries
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, entry)| {
                                            child_timeline_entry_matches(entry, identity)
                                        })
                                        .min_by_key(|(index, _)| {
                                            let context_score = child_anchor_context_score(
                                                &transcript.timeline_entries,
                                                *index,
                                                previous_entry_identity.as_ref(),
                                                next_entry_identity.as_ref(),
                                            );
                                            (
                                                2_usize.saturating_sub(context_score),
                                                retained_entry_index.map_or(0, |retained| {
                                                    retained.abs_diff(*index)
                                                }),
                                            )
                                        })
                                        .map(|(index, _)| index)
                                })
                                .or(retained_entry_index)?
                                .min(transcript.timeline_entries.len().saturating_sub(1));
                            let (projected_lines, entry_ranges) = self
                                .render_child_timeline_body_projection(
                                    &transcript.timeline_entries,
                                );
                            let range = entry_ranges.get(local_index)?.clone();
                            if range.is_empty() {
                                return Some(range.start);
                            }
                            let mut remaining = entry_content_offset;
                            let logical_line_offset = projected_lines
                                .get(range.clone())?
                                .iter()
                                .enumerate()
                                .find_map(|(index, line)| {
                                    let weight =
                                        timeline_history_line_weight(&plain_line_text(line));
                                    if remaining < weight {
                                        Some(index)
                                    } else {
                                        remaining = remaining.saturating_sub(weight);
                                        None
                                    }
                                })
                                .unwrap_or(entry_line_offset);
                            Some(
                                range
                                    .start
                                    .saturating_add(
                                        logical_line_offset.min(range.len().saturating_sub(1)),
                                    )
                                    .min(effective_len),
                            )
                        });
                let desired_start = entry_start.unwrap_or_else(|| {
                    let mut remaining = body_content_offset;
                    body.iter()
                        .take(effective_len)
                        .enumerate()
                        .find_map(|(index, line)| {
                            let weight = timeline_history_line_weight(&plain_line_text(line));
                            if remaining < weight {
                                Some(index)
                            } else {
                                remaining = remaining.saturating_sub(weight);
                                None
                            }
                        })
                        .unwrap_or(body_line_offset)
                        .min(effective_len)
                });
                let viewport = max_lines.saturating_sub(header.len()).max(1);
                let desired_end = desired_start.saturating_add(viewport).min(effective_len);
                self.timeline_scroll_back = effective_len.saturating_sub(desired_end);
            }
        }
    }

    pub fn handle_mouse_scroll(&mut self, upward: bool) {
        let delta = self.terminal_scroll_sensitivity();
        if self.approval.has_actionable_pending() {
            if upward {
                self.approval.scroll_back = self.approval.scroll_back.saturating_sub(delta);
            } else {
                self.approval.scroll_back = self.approval.scroll_back.saturating_add(delta);
            }
            return;
        }

        if upward {
            self.scroll_timeline(delta);
        } else {
            self.unscroll_timeline(delta);
        }
    }

    pub(super) fn scroll_active_pane(&mut self, delta: usize) {
        match self.active_pane {
            PaneFocus::Composer => self.scroll_timeline(delta),
            PaneFocus::Activity => {
                if self.approval.has_actionable_pending() {
                    self.approval.scroll_back = self.approval.scroll_back.saturating_sub(delta);
                } else {
                    self.activity_scroll_back = self.activity_scroll_back.saturating_add(delta);
                }
            }
        }
    }

    pub(super) fn unscroll_active_pane(&mut self, delta: usize) {
        match self.active_pane {
            PaneFocus::Composer => self.unscroll_timeline(delta),
            PaneFocus::Activity => {
                if self.approval.has_actionable_pending() {
                    self.approval.scroll_back = self.approval.scroll_back.saturating_add(delta);
                } else {
                    self.activity_scroll_back = self.activity_scroll_back.saturating_sub(delta);
                }
            }
        }
    }

    pub(crate) fn visible_timeline_render_range(&self, max_lines: usize) -> Range<usize> {
        let effective_len = self
            .effective_timeline_render_len()
            .min(self.timeline_state.render_store.snapshot().total_lines());
        if effective_len == 0 {
            return 0..0;
        }
        let viewport = max_lines.max(1);
        let scroll_back = self
            .timeline_scroll_back
            .min(effective_len.saturating_sub(viewport));
        let end = effective_len.saturating_sub(scroll_back);
        let start = end.saturating_sub(viewport);
        start..end
    }

    pub(crate) fn timeline_entry_render_range(&self, entry_index: usize) -> Option<Range<usize>> {
        self.timeline_state
            .render_store
            .snapshot()
            .range_for_entry(entry_index)
    }

    pub(crate) fn timeline_plain_line(&self, line_index: usize) -> Option<&str> {
        self.timeline_state
            .render_store
            .snapshot()
            .plain_line(line_index)
    }

    #[cfg(test)]
    pub(crate) fn timeline_render_line_count(&self) -> usize {
        self.timeline_state.render_store.snapshot().total_lines()
    }

    #[cfg(test)]
    pub(crate) fn timeline_render_lines(&self) -> Vec<Line<'static>> {
        let snapshot = self.timeline_state.render_store.snapshot();
        snapshot.lines_range(0..snapshot.total_lines())
    }

    #[cfg(test)]
    pub(crate) fn timeline_plain_lines(&self) -> Vec<String> {
        let snapshot = self.timeline_state.render_store.snapshot();
        snapshot.plain_lines_range(0..snapshot.total_lines())
    }

    pub(crate) fn transcript_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        if max_lines == 0 {
            return Vec::new();
        }

        if matches!(self.agent_panel.active_view, AgentView::Child { .. }) {
            return self.child_agent_transcript_lines(max_lines);
        }

        let visible_range = self.visible_timeline_render_range(max_lines);
        if visible_range.is_empty() {
            if !self.timeline.is_empty() {
                return Vec::new();
            }
            return vec![
                Line::from("no messages yet"),
                Line::from("send a prompt to start"),
            ];
        }
        let selection = self.selected_timeline_line_range();
        let selection_style = {
            let options = self.timeline_render_options();
            timeline_selection_style(&options.theme.palette)
        };
        self.timeline_state
            .render_store
            .snapshot()
            .lines_range(visible_range.clone())
            .iter()
            .enumerate()
            .map(|(offset, line)| {
                let line_index = visible_range.start.saturating_add(offset);
                if let Some(columns) = self.selected_timeline_column_range(line_index) {
                    selected_timeline_line_columns_with_style(
                        line.clone(),
                        columns,
                        selection_style,
                    )
                } else if selection
                    .as_ref()
                    .is_some_and(|range| range.contains(&line_index))
                {
                    selected_timeline_line(line.clone(), selection_style)
                } else {
                    line.clone()
                }
            })
            .collect()
    }

    fn child_agent_transcript_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        let (header, body) = self.render_child_agent_transcript_sections();
        if max_lines <= header.len() {
            return header.into_iter().take(max_lines).collect();
        }
        let body_budget = max_lines.saturating_sub(header.len());
        let effective_len = child_transcript_effective_len(&body);
        if effective_len == 0 {
            return header;
        }
        let viewport = body_budget.max(1);
        let scroll_back = self
            .timeline_scroll_back
            .min(effective_len.saturating_sub(viewport));
        let end = effective_len.saturating_sub(scroll_back);
        let start = end.saturating_sub(viewport);
        header
            .into_iter()
            .chain(body[start..end].iter().cloned())
            .collect()
    }

    fn render_child_agent_transcript_lines(&self) -> Vec<Line<'static>> {
        let (header, body) = self.render_child_agent_transcript_sections();
        header.into_iter().chain(body).collect()
    }

    fn render_child_agent_transcript_sections(&self) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let AgentView::Child {
            child_session_ref, ..
        } = &self.agent_panel.active_view
        else {
            return (Vec::new(), Vec::new());
        };
        let child = self.active_agent_child_entry();
        let agent_thread = self.active_agent_thread_projection();
        let continuation_projection = sigil_kernel::AgentResultContinuationProjection::from_entries(
            &self.session_browser.current_entries,
        );
        let active_label = self.active_agent_label();
        let theme = self.timeline_render_options().theme;
        let mut header = vec![Line::from(vec![
            Span::styled("agent view", Style::default().fg(theme.palette.accent_info)),
            Span::raw(format!(": {active_label}")),
            Span::raw(" · child session"),
        ])];
        if let Some(thread) = agent_thread.as_ref() {
            let session_view_cache = self.session_view_cache();
            let latest_task = session_view_cache.task_projection.latest_task();
            let continuation_unresolved = continuation_projection
                .statuses
                .get(&thread.thread_id)
                .is_some_and(|status| status.is_unresolved());
            header.push(Line::from(format!(
                "status: {}",
                agent_thread_sidebar_detail(thread, latest_task, continuation_unresolved)
            )));
        } else if let Some(child) = child.as_ref() {
            let result_label = if task_child_session_is_terminal(child.status) {
                if child.summary_hash.is_some() {
                    "result ready"
                } else {
                    "result missing"
                }
            } else {
                "result pending"
            };
            header.push(Line::from(format!(
                "status: {} · {} · v{}:{} · {}",
                task_child_session_status_label(child.status),
                child.role.as_str(),
                child.plan_version,
                child.step_id.as_str(),
                result_label
            )));
        }
        header.push(Line::from(format!(
            "session: {}",
            truncate_session_view_text(&child_session_ref.as_path().display().to_string(), 96)
        )));
        let mut body = Vec::new();
        let Some(transcript) = self.agent_panel.active_child_transcript.as_ref() else {
            body.push(Line::from("child session not loaded"));
            self.append_child_queue_overlay(&mut body);
            return self.wrap_child_agent_transcript_sections(header, body);
        };
        if let Some(error) = transcript.load_error.as_ref() {
            body.push(Line::from(format!(
                "load error: {}",
                truncate_session_view_text(error, 120)
            )));
            body.push(Line::from(format!(
                "path: {}",
                truncate_session_view_text(&transcript.path.display().to_string(), 120)
            )));
            self.append_child_queue_overlay(&mut body);
            return self.wrap_child_agent_transcript_sections(header, body);
        }
        if transcript.rendered_body_lines.is_empty() {
            body.push(Line::from("child session has no transcript messages yet"));
            self.append_child_queue_overlay(&mut body);
            return self.wrap_child_agent_transcript_sections(header, body);
        }
        if transcript.transcript_truncated {
            header.push(Line::from(format!(
                "showing latest {} child transcript entries",
                transcript.timeline_entries.len()
            )));
        } else if transcript.total_timeline_entries > transcript.timeline_entries.len() {
            header.push(Line::from(format!(
                "showing latest {} of {} child transcript entries",
                transcript.timeline_entries.len(),
                transcript.total_timeline_entries
            )));
        }
        body = transcript.rendered_body_lines.clone();
        self.append_child_queue_overlay(&mut body);
        self.wrap_child_agent_transcript_sections(header, body)
    }

    fn wrap_child_agent_transcript_sections(
        &self,
        header: Vec<Line<'static>>,
        body: Vec<Line<'static>>,
    ) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let width = self.timeline_render_width();
        (
            crate::ui::wrap_terminal_lines(header, width),
            crate::ui::wrap_terminal_lines(body, width),
        )
    }

    fn append_child_queue_overlay(&self, body: &mut Vec<Line<'static>>) {
        let queued_entries = self
            .conversation_queue_projection()
            .items
            .into_iter()
            .map(|item| TimelineEntry {
                role: TimelineRole::User,
                text: item.queued.prompt,
            })
            .collect::<Vec<_>>();
        if queued_entries.is_empty() {
            return;
        }
        let overlay = self.render_child_timeline_body_lines(&queued_entries);
        if !body.is_empty() && !overlay.is_empty() {
            body.push(Line::raw(String::new()));
        }
        body.extend(overlay);
    }

    pub(super) fn render_child_timeline_body_lines(
        &self,
        timeline_entries: &[TimelineEntry],
    ) -> Vec<Line<'static>> {
        self.render_child_timeline_body_projection(timeline_entries)
            .0
    }

    fn render_child_timeline_body_projection(
        &self,
        timeline_entries: &[TimelineEntry],
    ) -> (Vec<Line<'static>>, Vec<Range<usize>>) {
        let mut body = Vec::new();
        let mut entry_ranges = Vec::with_capacity(timeline_entries.len());
        let mut options = self.timeline_render_options();
        options.selected_tool_activity_key = None;
        options.hovered_tool_activity_key = None;
        options.streaming_assistant_index = None;
        options.streaming_reasoning_index = active_child_transcript_reasoning_index(
            timeline_entries,
            self.active_child_is_running(),
        );
        for (index, entry) in timeline_entries.iter().enumerate() {
            let rendered =
                crate::ui::render_timeline_entry_lines_with_options(entry, &options, index);
            let rendered = crate::ui::wrap_terminal_lines(rendered, options.max_render_width);
            if !rendered.is_empty() && !body.is_empty() {
                body.push(Line::raw(String::new()));
            }
            let start = body.len();
            body.extend(rendered);
            entry_ranges.push(start..body.len());
        }
        while body
            .last()
            .is_some_and(|line| !line_has_visible_content(line))
        {
            let _ = body.pop();
        }
        for range in &mut entry_ranges {
            range.end = range.end.min(body.len());
            range.start = range.start.min(range.end);
        }
        (body, entry_ranges)
    }

    fn active_child_is_running(&self) -> bool {
        matches!(self.agent_panel.active_view, AgentView::Child { .. })
            && self
                .active_agent_thread_projection()
                .is_some_and(|thread| !thread.status.is_terminal())
    }

    pub(crate) fn selected_timeline_line_range(&self) -> Option<Range<usize>> {
        if !self.main_timeline_active() {
            return None;
        }
        let range = self.timeline_state.text_selection?.normalized_range();
        let end = range
            .end
            .min(self.timeline_state.render_store.snapshot().total_lines());
        (range.start < end).then_some(range.start..end)
    }

    pub(crate) fn selected_timeline_text(&self) -> Option<String> {
        let range = self.selected_timeline_line_range()?;
        if self
            .timeline_state
            .text_selection
            .and_then(TimelineTextSelection::normalized_column_bounds)
            .is_some()
        {
            return Some(
                range
                    .filter_map(|line_index| {
                        let line = self
                            .timeline_state
                            .render_store
                            .snapshot()
                            .plain_line(line_index)?;
                        let columns = self.selected_timeline_column_range(line_index)?;
                        Some(text_by_display_columns(line, columns.start, columns.end))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .filter(|text| !text.is_empty());
        }
        Some(
            self.timeline_state
                .render_store
                .snapshot()
                .plain_lines_range(range)
                .join("\n"),
        )
        .filter(|text| !text.is_empty())
    }

    pub(crate) fn request_clipboard_copy(&mut self, text: String) -> AppAction {
        let status = clipboard_copy_status(&text);
        self.last_notice = Some(format!("copy pending {status}"));
        self.push_event("selection:copy", format!("pending {status}"));
        AppAction::CopyToClipboard { text }
    }

    pub(crate) fn request_copy_selection_or_latest_response(&mut self) -> Option<AppAction> {
        let text = self
            .selected_timeline_text()
            .or_else(|| self.latest_assistant_response_text());
        let Some(text) = text else {
            self.last_notice = Some("nothing to copy".to_owned());
            return None;
        };
        Some(self.request_clipboard_copy(text))
    }

    fn latest_assistant_response_text(&self) -> Option<String> {
        if matches!(self.agent_panel.active_view, AgentView::Child { .. }) {
            return self
                .agent_panel
                .active_child_transcript
                .as_ref()?
                .timeline_entries
                .iter()
                .rev()
                .find(|entry| {
                    entry.role == TimelineRole::Assistant && !entry.text.trim().is_empty()
                })
                .map(|entry| entry.text.clone());
        }

        self.timeline
            .iter()
            .rev()
            .find(|entry| entry.role == TimelineRole::Assistant && !entry.text.trim().is_empty())
            .map(|entry| entry.text.clone())
    }

    pub(crate) fn begin_timeline_text_selection_at(
        &mut self,
        line_index: usize,
        column: usize,
    ) -> bool {
        if !self.main_timeline_active() {
            return self.clear_timeline_text_selection();
        }
        if line_index >= self.timeline_state.render_store.snapshot().total_lines() {
            return self.clear_timeline_text_selection();
        }
        self.timeline_state.text_selection_anchor = Some(line_index);
        self.timeline_state.text_selection_anchor_column = Some(column);
        self.timeline_state.text_selection.take().is_some()
    }

    pub(crate) fn update_timeline_text_selection(&mut self, line_index: usize) -> bool {
        if !self.main_timeline_active() {
            return self.clear_timeline_text_selection();
        }
        let Some(anchor) = self.timeline_state.text_selection_anchor else {
            return false;
        };
        let len = self.timeline_state.render_store.snapshot().total_lines();
        if len == 0 {
            return false;
        }
        let cursor = line_index.min(len.saturating_sub(1));
        let next = Some(TimelineTextSelection::line(anchor, cursor));
        let changed = self.timeline_state.text_selection != next;
        self.timeline_state.text_selection = next;
        changed
    }

    pub(crate) fn update_timeline_text_selection_at(
        &mut self,
        line_index: usize,
        column: usize,
    ) -> bool {
        if !self.main_timeline_active() {
            return self.clear_timeline_text_selection();
        }
        let Some(anchor) = self.timeline_state.text_selection_anchor else {
            return false;
        };
        let Some(anchor_column) = self.timeline_state.text_selection_anchor_column else {
            return self.update_timeline_text_selection(line_index);
        };
        let len = self.timeline_state.render_store.snapshot().total_lines();
        if len == 0 {
            return false;
        }
        let cursor = line_index.min(len.saturating_sub(1));
        let next = Some(TimelineTextSelection::column(
            anchor,
            anchor_column,
            cursor,
            column,
        ));
        let changed = self.timeline_state.text_selection != next;
        self.timeline_state.text_selection = next;
        changed
    }

    pub(crate) fn finish_timeline_text_selection(&mut self) -> bool {
        self.timeline_state.text_selection_anchor.take().is_some()
    }

    pub(crate) fn clear_timeline_text_selection(&mut self) -> bool {
        self.clear_timeline_text_selection_state()
    }

    fn clear_timeline_text_selection_state(&mut self) -> bool {
        let changed = self.timeline_state.text_selection.is_some()
            || self.timeline_state.text_selection_anchor.is_some()
            || self.timeline_state.text_selection_anchor_column.is_some();
        self.timeline_state.text_selection = None;
        self.timeline_state.text_selection_anchor = None;
        self.timeline_state.text_selection_anchor_column = None;
        changed
    }

    fn selected_timeline_column_range(&self, line_index: usize) -> Option<Range<usize>> {
        let selection = self.timeline_state.text_selection?;
        let (start_line, start_column, end_line, end_column) =
            selection.normalized_column_bounds()?;
        if line_index < start_line || line_index > end_line {
            return None;
        }
        let line = self
            .timeline_state
            .render_store
            .snapshot()
            .plain_line(line_index)?;
        let line_width = crate::ui::terminal_cell_width(line);
        let start = if line_index == start_line {
            start_column.min(line_width)
        } else {
            0
        };
        let end = if line_index == end_line {
            end_column.min(line_width)
        } else {
            line_width
        };
        (start < end).then_some(start..end)
    }

    pub fn record_clipboard_copy_success(&mut self, text: &str) {
        self.clear_timeline_text_selection_state();
        self.last_notice = Some(format!("copied {}", clipboard_copy_status(text)));
        self.push_event("selection:copy", clipboard_copy_status(text));
    }

    pub fn record_clipboard_copy_unavailable(&mut self, reason: &str) {
        self.last_notice = Some(format!("clipboard unavailable: {reason}"));
        self.push_event("selection:copy", format!("unavailable {reason}"));
    }

    pub fn timeline_revision(&self) -> u64 {
        self.timeline_state
            .revision
            .max(self.timeline_state.render_store.snapshot().revision())
    }

    fn timeline_render_options(&self) -> crate::ui::TimelineRenderOptions {
        crate::ui::TimelineRenderOptions {
            expand_tool_previews: false,
            expand_thinking_blocks: matches!(self.thinking_block_mode, ThinkingBlockMode::Expanded),
            selected_tool_activity_key: self.timeline_state.selected_tool_activity_key.clone(),
            hovered_tool_activity_key: self.hovered_tool_activity_key(),
            expanded_tool_activity_keys: self.timeline_state.expanded_tool_activity_keys.clone(),
            collapsed_tool_activity_keys: self.timeline_state.collapsed_tool_activity_keys.clone(),
            tool_activity_visible_rows: self.timeline_state.tool_activity_visible_rows.clone(),
            max_content_width: self.timeline_content_width(),
            max_render_width: self.timeline_render_width(),
            streaming_assistant_index: self.timeline_state.streaming_assistant_index,
            streaming_reasoning_index: self.timeline_state.streaming_reasoning_index,
            intermediate_assistant_indices: self.intermediate_assistant_indices(),
            expanded_thinking_entry_indices: self
                .timeline_state
                .expanded_thinking_entry_indices
                .clone(),
            collapsed_thinking_entry_indices: self
                .timeline_state
                .collapsed_thinking_entry_indices
                .clone(),
            expanded_diagram_entry_indices: self
                .timeline_state
                .expanded_diagram_entry_indices
                .clone(),
            hovered_thinking_entry_index: self.hovered_thinking_entry_index(),
            theme: crate::ui::theme::resolve_for_app(self),
        }
    }

    fn hovered_thinking_entry_index(&self) -> Option<usize> {
        match self.mouse_hover_target? {
            crate::mouse::HitTarget::ThinkingBlock { entry_index } => Some(entry_index),
            _ => None,
        }
    }

    fn intermediate_assistant_indices(&self) -> BTreeSet<usize> {
        self.timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (entry.role == TimelineRole::Assistant
                    && !entry.text.trim().is_empty()
                    && self.assistant_entry_is_intermediate_info(index))
                .then_some(index)
            })
            .collect()
    }

    fn assistant_entry_is_intermediate_info(&self, index: usize) -> bool {
        self.timeline
            .iter()
            .skip(index.saturating_add(1))
            .find_map(|entry| match entry.role {
                TimelineRole::Tool => Some(true),
                TimelineRole::Notice | TimelineRole::Phase | TimelineRole::System => None,
                TimelineRole::User | TimelineRole::Assistant | TimelineRole::Thinking => {
                    Some(false)
                }
            })
            .unwrap_or(false)
    }

    fn latest_assistant_entry_index_before(&self, index: usize) -> Option<usize> {
        self.timeline
            .iter()
            .take(index)
            .enumerate()
            .rev()
            .find_map(|(entry_index, entry)| {
                (entry.role == TimelineRole::Assistant && !entry.text.trim().is_empty())
                    .then_some(entry_index)
            })
    }

    fn timeline_content_width(&self) -> usize {
        self.timeline_render_width().saturating_sub(4).max(1)
    }

    fn timeline_render_width(&self) -> usize {
        let total_width = self.terminal_width as usize;
        let sidebar_width = if self.info_rail_visible() {
            sidebar_width_for_terminal(total_width)
        } else {
            0
        };
        total_width
            .saturating_sub(sidebar_width)
            .saturating_sub(2)
            .max(1)
    }

    pub(crate) fn live_activity_summary(&self) -> Option<LiveActivitySummary> {
        if let Some(pending) = &self.approval.pending {
            let detail = match &pending.presentation_state {
                ApprovalPresentationState::Pending => {
                    format!("waiting for decision on {}", pending.call.name)
                }
                ApprovalPresentationState::DecisionAccepted { .. } => {
                    format!("decision accepted for {}; resuming run", pending.call.name)
                }
                ApprovalPresentationState::DeliveryUncertain { .. } => format!(
                    "approval state uncertain for {}; awaiting authority",
                    pending.call.name
                ),
            };
            return Some(LiveActivitySummary {
                label: "approval".to_owned(),
                detail,
            });
        }
        if let Some(summary) = self.active_child_agent_activity_summary() {
            return Some(summary);
        }
        if !self.runtime.is_busy {
            return None;
        }
        if let Some(progress) = &self.runtime.mcp_progress {
            return Some(LiveActivitySummary {
                label: "mcp".to_owned(),
                detail: progress.detail.clone(),
            });
        }
        let (label, detail) = match &self.runtime.run_phase {
            RunPhase::Idle => ("working", "waiting for next event".to_owned()),
            RunPhase::Thinking => (
                "thinking",
                format!("reasoning with {}", self.runtime.model_name),
            ),
            RunPhase::Agent(profile_id) => ("agent", format!("waiting for @{profile_id} result")),
            RunPhase::Tool(name) => ("tool", format!("running {name}")),
            RunPhase::Streaming => ("streaming", "receiving response".to_owned()),
        };
        Some(LiveActivitySummary {
            label: label.to_owned(),
            detail,
        })
    }

    pub(crate) fn live_panel_phase(&self) -> RunPhase {
        if matches!(self.agent_panel.active_view, AgentView::Child { .. }) {
            if let Some(thread) = self.active_agent_thread_projection() {
                return RunPhase::Agent(
                    thread
                        .profile_id
                        .map(|profile_id| profile_id.as_str().to_owned())
                        .unwrap_or_else(|| "agent".to_owned()),
                );
            }
            if self.active_agent_child_entry().is_some() {
                return RunPhase::Agent("agent".to_owned());
            }
        }
        self.run_phase()
    }

    fn active_child_agent_activity_summary(&self) -> Option<LiveActivitySummary> {
        if !matches!(self.agent_panel.active_view, AgentView::Child { .. }) {
            return None;
        }
        let label = "agent".to_owned();
        let active_label = self.active_agent_label();
        if let Some(thread) = self.active_agent_thread_projection() {
            if thread.status.is_terminal() {
                return None;
            }
            let profile = thread
                .profile_id
                .as_ref()
                .map(sigil_kernel::AgentProfileId::as_str)
                .unwrap_or("agent");
            return Some(LiveActivitySummary {
                label,
                detail: format!(
                    "{} · {} · {}",
                    active_label,
                    agent_thread_status_label(thread.status),
                    profile
                ),
            });
        }
        let child = self.active_agent_child_entry()?;
        if task_child_session_is_terminal(child.status) {
            return None;
        }
        Some(LiveActivitySummary {
            label,
            detail: format!(
                "{} · {} · {}",
                active_label,
                task_child_session_status_label(child.status),
                child.role.as_str()
            ),
        })
    }
}

fn remap_tool_activity_key_after_entry_removal(
    key: String,
    removed_indices: &[usize],
) -> Option<String> {
    let Some((prefix, index)) = key.rsplit_once(":entry:") else {
        return Some(key);
    };
    let Ok(index) = index.parse::<usize>() else {
        return Some(key);
    };
    if removed_indices.contains(&index) {
        return None;
    }
    let shift = removed_indices
        .iter()
        .filter(|removed_index| **removed_index < index)
        .count();
    Some(format!("{prefix}:entry:{}", index.saturating_sub(shift)))
}

fn timeline_history_line_weight(line: &str) -> usize {
    line.chars()
        .filter(|character| !character.is_whitespace())
        .count()
        .max(1)
}

fn child_transcript_effective_len(body: &[Line<'_>]) -> usize {
    body.iter()
        .rposition(line_has_visible_content)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn child_timeline_entry_matches(entry: &TimelineEntry, identity: &(TimelineRole, String)) -> bool {
    entry.role == identity.0 && entry.text == identity.1
}

fn child_anchor_context_score(
    entries: &[TimelineEntry],
    index: usize,
    previous: Option<&(TimelineRole, String)>,
    next: Option<&(TimelineRole, String)>,
) -> usize {
    let previous_matches = match previous {
        Some(identity) => index
            .checked_sub(1)
            .and_then(|previous_index| entries.get(previous_index))
            .is_some_and(|entry| child_timeline_entry_matches(entry, identity)),
        None => index == 0,
    };
    let next_matches = match next {
        Some(identity) => entries
            .get(index.saturating_add(1))
            .is_some_and(|entry| child_timeline_entry_matches(entry, identity)),
        None => index.saturating_add(1) >= entries.len(),
    };
    usize::from(previous_matches) + usize::from(next_matches)
}

fn bounded_event_detail(detail: String) -> String {
    truncate_session_view_text(&detail, EVENT_DETAIL_MAX_CHARS)
}

fn task_child_session_is_terminal(status: sigil_kernel::TaskChildSessionStatus) -> bool {
    matches!(
        status,
        sigil_kernel::TaskChildSessionStatus::Completed
            | sigil_kernel::TaskChildSessionStatus::Failed
            | sigil_kernel::TaskChildSessionStatus::Cancelled
            | sigil_kernel::TaskChildSessionStatus::Interrupted
            | sigil_kernel::TaskChildSessionStatus::Unavailable
    )
}

fn agent_thread_status_label(status: sigil_kernel::AgentThreadStatus) -> &'static str {
    match status {
        sigil_kernel::AgentThreadStatus::Started => "started",
        sigil_kernel::AgentThreadStatus::Running => "running",
        sigil_kernel::AgentThreadStatus::Blocked => "blocked",
        sigil_kernel::AgentThreadStatus::Completed => "completed",
        sigil_kernel::AgentThreadStatus::Failed => "failed",
        sigil_kernel::AgentThreadStatus::Cancelled => "cancelled",
        sigil_kernel::AgentThreadStatus::Interrupted => "interrupted",
        sigil_kernel::AgentThreadStatus::Closed => "closed",
        sigil_kernel::AgentThreadStatus::Unavailable => "unavailable",
        sigil_kernel::AgentThreadStatus::Unknown => "unknown",
    }
}

fn selected_timeline_line(line: Line<'static>, selection_style: Style) -> Line<'static> {
    line.patch_style(selection_style)
}

#[allow(dead_code)]
pub(super) fn selected_timeline_line_columns(
    line: Line<'static>,
    columns: Range<usize>,
) -> Line<'static> {
    let theme = crate::ui::theme::Theme::default();
    selected_timeline_line_columns_with_style(
        line,
        columns,
        timeline_selection_style(&theme.palette),
    )
}

fn selected_timeline_line_columns_with_style(
    line: Line<'static>,
    columns: Range<usize>,
    selection_style: Style,
) -> Line<'static> {
    if columns.start >= columns.end {
        return line;
    }
    let mut display_column = 0usize;
    let mut selected_line = line;
    let spans = std::mem::take(&mut selected_line.spans);
    selected_line.spans = spans
        .into_iter()
        .flat_map(|span| {
            split_span_for_column_selection(span, &mut display_column, &columns, selection_style)
        })
        .collect();
    selected_line
}

fn split_span_for_column_selection(
    span: Span<'static>,
    display_column: &mut usize,
    columns: &Range<usize>,
    selection_style: Style,
) -> Vec<Span<'static>> {
    let mut pieces = Vec::new();
    let mut current_text = String::new();
    let mut current_selected: Option<bool> = None;
    for grapheme in span.content.as_ref().graphemes(true) {
        let Some(width) = crate::ui::terminal_grapheme_width(grapheme) else {
            continue;
        };
        let next_column = display_column.saturating_add(width);
        let selected = if width == 0 {
            *display_column >= columns.start && *display_column < columns.end
        } else {
            next_column > columns.start && *display_column < columns.end
        };
        if current_selected != Some(selected) && !current_text.is_empty() {
            pieces.push(selection_span_piece(
                &span,
                &current_text,
                current_selected == Some(true),
                selection_style,
            ));
            current_text.clear();
        }
        current_selected = Some(selected);
        current_text.push_str(grapheme);
        *display_column = next_column;
    }
    if !current_text.is_empty() {
        pieces.push(selection_span_piece(
            &span,
            &current_text,
            current_selected == Some(true),
            selection_style,
        ));
    }
    pieces
}

fn selection_span_piece(
    source: &Span<'static>,
    text: &str,
    selected: bool,
    selection_style: Style,
) -> Span<'static> {
    let style = if selected {
        source.style.patch(selection_style)
    } else {
        source.style
    };
    Span::styled(text.to_owned(), style)
}

fn timeline_selection_style(palette: &crate::ui::theme::ThemePalette) -> Style {
    Style::default()
        .fg(palette.selection_fg)
        .bg(palette.selection_bg)
}

pub(super) fn text_by_display_columns(text: &str, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    let mut output = String::new();
    let mut display_column = 0usize;
    for grapheme in text.graphemes(true) {
        let Some(width) = crate::ui::terminal_grapheme_width(grapheme) else {
            continue;
        };
        let next_column = display_column.saturating_add(width);
        let selected = if width == 0 {
            display_column >= start && display_column < end
        } else {
            next_column > start && display_column < end
        };
        if selected {
            output.push_str(grapheme);
        }
        display_column = next_column;
    }
    output
}

fn active_child_transcript_reasoning_index(
    timeline_entries: &[TimelineEntry],
    child_running: bool,
) -> Option<usize> {
    if !child_running {
        return None;
    }
    let last_index = timeline_entries
        .iter()
        .rposition(|entry| !entry.text.trim().is_empty())?;
    (timeline_entries[last_index].role == TimelineRole::Thinking).then_some(last_index)
}

pub(super) fn clipboard_copy_status(text: &str) -> String {
    let lines = text.lines().count().max(1);
    let chars = text.chars().count();
    format!("{lines} line(s), {chars} char(s)")
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/timeline_flow_unit_tests.rs"]
mod tests;
