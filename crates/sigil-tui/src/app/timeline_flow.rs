use std::{collections::BTreeSet, ops::Range};

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    AgentView, AppState, EventEntry, LiveActivitySummary, PaneFocus, RunPhase, ThinkingBlockMode,
    TimelineEntry, TimelineRole, TimelineTextSelection,
    formatting::{
        hash_timeline_line, line_has_visible_content, plain_line_text, sidebar_width_for_terminal,
        truncate_session_view_text,
    },
    task_sidebar::task_child_session_status_label,
};

impl AppState {
    pub(super) fn live_panel_height(&self) -> u16 {
        self.terminal_height
            .saturating_sub(self.footer_strip_height())
            .saturating_sub(1)
            .max(1)
    }

    pub(super) fn timeline_viewport_rows(&self) -> usize {
        self.live_panel_height()
            .saturating_sub(self.live_status_band_rows())
            .max(1) as usize
    }

    fn live_status_band_rows(&self) -> u16 {
        let progress_rows: u16 = if self.live_activity_summary().is_some() {
            2
        } else {
            0
        };
        let task_rows = self
            .task_strip_view()
            .map(|view| {
                if view.rows.is_empty() {
                    0
                } else {
                    1 + view.rows.len().min(4) as u16
                }
            })
            .unwrap_or(0);
        let content_rows = progress_rows.saturating_add(task_rows);
        if content_rows == 0 {
            0
        } else {
            content_rows.saturating_add(1)
        }
    }

    pub(super) fn max_timeline_scroll_back(&self) -> usize {
        let total = self.effective_timeline_render_len();
        let viewport = self.timeline_viewport_rows().max(1);
        total.saturating_sub(viewport)
    }

    pub(super) fn effective_timeline_render_len(&self) -> usize {
        if matches!(self.active_agent_view, AgentView::Child { .. }) {
            return self
                .render_child_agent_transcript_lines()
                .iter()
                .rposition(line_has_visible_content)
                .map(|index| index + 1)
                .unwrap_or(0);
        }
        self.timeline_render_cache
            .iter()
            .rposition(line_has_visible_content)
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    pub(super) fn scrollback_cutoff_line(&self) -> usize {
        let durable_cutoff_entry = match self.streaming_assistant_index {
            Some(index) if index + 1 == self.timeline.len() && self.is_busy => index,
            _ => self.timeline.len(),
        };
        let durable_cutoff_line = if durable_cutoff_entry == 0 {
            0
        } else {
            self.timeline_render_ranges
                .get(durable_cutoff_entry - 1)
                .map(|range| range.end)
                .unwrap_or(self.timeline_render_cache.len())
        };
        let live_tail_start = self
            .effective_timeline_render_len()
            .saturating_sub(self.timeline_viewport_rows().max(1));
        durable_cutoff_line.min(live_tail_start)
    }

    pub(super) fn transcript_page_step(&self) -> usize {
        (self.timeline_viewport_rows() / 2).max(1)
    }

    pub(super) fn push_timeline(&mut self, role: TimelineRole, text: impl Into<String>) {
        self.flush_deferred_timeline_renders();
        self.clear_timeline_text_selection_state();
        let is_tool = role == TimelineRole::Tool;
        let previous_selected_tool = self.selected_tool_activity_key.clone();
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
            self.selected_tool_activity_key = Some(activity.key.clone());
            self.tool_activity_cache.push(activity);
        }
        if is_tool
            && previous_selected_tool != self.selected_tool_activity_key
            && let Some(previous_index) = previous_selected_tool
                .as_deref()
                .and_then(|key| self.timeline_entry_index_for_activity_key(key))
            && previous_index < self.timeline.len().saturating_sub(1)
        {
            self.rerender_timeline_entry(previous_index);
        }
        // Default-open file diffs can be large, so new output should not force
        // every historical activity through JSON parsing and diff rendering.
        self.append_timeline_render_cache_entry(self.timeline.len().saturating_sub(1));
    }

    pub(super) fn push_event(&mut self, label: impl Into<String>, detail: impl Into<String>) {
        self.events.push(EventEntry {
            label: label.into(),
            detail: detail.into(),
        });
        if self.events.len() > 400 {
            self.events.remove(0);
        }
    }

    pub(super) fn append_assistant_delta(&mut self, delta: &str) {
        self.finish_streaming_reasoning_entry();
        if let Some(index) = self.streaming_assistant_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry_deferred(index);
            return;
        }

        self.push_timeline(TimelineRole::Assistant, delta);
        self.streaming_assistant_index = self.timeline.len().checked_sub(1);
    }

    pub(super) fn append_reasoning_delta(&mut self, delta: &str) {
        self.finish_streaming_assistant_entry();
        if let Some(index) = self.streaming_reasoning_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry_deferred(index);
            return;
        }

        self.push_timeline(TimelineRole::Thinking, delta);
        self.streaming_reasoning_index = self.timeline.len().checked_sub(1);
    }

    pub(super) fn finish_streaming_reasoning_entry(&mut self) {
        if let Some(index) = self.streaming_reasoning_index.take() {
            self.rerender_timeline_entry_deferred(index);
        }
    }

    pub(super) fn push_phase_marker(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.last_phase_marker.as_deref() == Some(text.as_str()) {
            return;
        }
        self.last_phase_marker = Some(text.clone());
        self.push_event("phase", text);
    }

    pub(super) fn toggle_thinking_block_mode(&mut self) {
        self.thinking_block_mode = match self.thinking_block_mode {
            ThinkingBlockMode::Collapsed => ThinkingBlockMode::Expanded,
            ThinkingBlockMode::Expanded => ThinkingBlockMode::Collapsed,
        };
        self.expanded_thinking_entry_indices.clear();
        self.collapsed_thinking_entry_indices.clear();
        self.rebuild_timeline_render_cache();
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
            self.expanded_thinking_entry_indices.remove(&entry_index);
            self.collapsed_thinking_entry_indices.insert(entry_index);
        } else {
            self.collapsed_thinking_entry_indices.remove(&entry_index);
            self.expanded_thinking_entry_indices.insert(entry_index);
        }
        self.rerender_timeline_entry(entry_index);
        let state = if expanded { "collapsed" } else { "expanded" };
        self.last_notice = Some(format!("thinking {state}"));
        self.push_event("thinking:entry", format!("{entry_index} {state}"));
        true
    }

    pub(super) fn has_collapsible_thinking_blocks(&self) -> bool {
        self.timeline
            .iter()
            .any(|entry| self.thinking_entry_is_collapsible(entry))
    }

    pub(crate) fn collapsible_thinking_entry_indices(&self) -> Vec<usize> {
        self.timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| self.thinking_entry_is_collapsible(entry).then_some(index))
            .collect()
    }

    fn thinking_entry_is_collapsible(&self, entry: &TimelineEntry) -> bool {
        entry.role == TimelineRole::Thinking
            && crate::ui::thinking_has_collapsed_content(&entry.text)
    }

    fn thinking_entry_is_expanded(&self, entry_index: usize) -> bool {
        self.streaming_reasoning_index == Some(entry_index)
            || self.expanded_thinking_entry_indices.contains(&entry_index)
            || (matches!(self.thinking_block_mode, ThinkingBlockMode::Expanded)
                && !self.collapsed_thinking_entry_indices.contains(&entry_index))
    }

    pub(super) fn rebuild_timeline_render_cache(&mut self) {
        self.clear_timeline_text_selection_state();
        self.deferred_timeline_render_indexes.clear();
        let options = self.timeline_render_options();
        self.timeline_render_cache.clear();
        self.timeline_plain_cache.clear();
        self.timeline_prefix_hashes.clear();
        self.timeline_render_ranges.clear();
        for index in 0..self.timeline.len() {
            let start = self.timeline_render_cache.len();
            let rendered = {
                let entry = &self.timeline[index];
                crate::ui::render_timeline_entry_lines_with_options(entry, &options, index)
            };
            self.extend_timeline_render_buffers(rendered);
            let end = self.timeline_render_cache.len();
            self.timeline_render_ranges.push(start..end);
        }
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    pub(super) fn rerender_timeline_entry(&mut self, index: usize) {
        self.clear_timeline_text_selection_state();
        self.deferred_timeline_render_indexes.remove(&index);
        let Some(existing_range) = self.timeline_render_ranges.get(index).cloned() else {
            self.rebuild_timeline_render_cache();
            return;
        };
        let Some(entry) = self.timeline.get(index) else {
            self.rebuild_timeline_render_cache();
            return;
        };
        let options = self.timeline_render_options();
        let new_lines = crate::ui::render_timeline_entry_lines_with_options(entry, &options, index);
        let new_plain = new_lines.iter().map(plain_line_text).collect::<Vec<_>>();
        let old_len = existing_range.end.saturating_sub(existing_range.start);
        let new_len = new_lines.len();
        self.timeline_render_cache
            .splice(existing_range.clone(), new_lines);
        self.timeline_plain_cache
            .splice(existing_range.clone(), new_plain);
        self.timeline_render_ranges[index] =
            existing_range.start..existing_range.start.saturating_add(new_len);
        if new_len != old_len {
            let delta = new_len as isize - old_len as isize;
            for range in self.timeline_render_ranges.iter_mut().skip(index + 1) {
                range.start = range.start.saturating_add_signed(delta);
                range.end = range.end.saturating_add_signed(delta);
            }
        }
        self.rebuild_timeline_prefix_hashes_from(existing_range.start);
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    fn rerender_timeline_entry_deferred(&mut self, index: usize) {
        if self.defer_timeline_renders {
            self.deferred_timeline_render_indexes.insert(index);
            return;
        }
        self.rerender_timeline_entry(index);
    }

    pub fn begin_timeline_render_batch(&mut self) {
        self.defer_timeline_renders = true;
    }

    pub fn flush_timeline_render_batch(&mut self) -> bool {
        self.defer_timeline_renders = false;
        self.flush_deferred_timeline_renders()
    }

    pub(super) fn finish_streaming_assistant_entry(&mut self) {
        let Some(index) = self.streaming_assistant_index.take() else {
            return;
        };
        self.deferred_timeline_render_indexes.remove(&index);
        if index < self.timeline.len() {
            self.rerender_timeline_entry(index);
        }
    }

    pub(super) fn flush_deferred_timeline_renders(&mut self) -> bool {
        if self.deferred_timeline_render_indexes.is_empty() {
            return false;
        }
        let indexes = std::mem::take(&mut self.deferred_timeline_render_indexes);
        for index in indexes {
            if index < self.timeline.len() {
                self.rerender_timeline_entry(index);
            }
        }
        true
    }

    pub(super) fn append_timeline_render_cache_entry(&mut self, index: usize) {
        self.clear_timeline_text_selection_state();
        if index != self.timeline_render_ranges.len() {
            self.rebuild_timeline_render_cache();
            return;
        }
        let Some(entry) = self.timeline.get(index) else {
            self.rebuild_timeline_render_cache();
            return;
        };
        let options = self.timeline_render_options();
        let new_lines = crate::ui::render_timeline_entry_lines_with_options(entry, &options, index);
        if !new_lines.is_empty() && !self.timeline_render_cache.is_empty() {
            self.extend_timeline_render_buffers(vec![Line::raw(String::new())]);
            self.extend_last_render_block_range_by_one_line();
        }
        let start = self.timeline_render_cache.len();
        self.extend_timeline_render_buffers(new_lines);
        let end = self.timeline_render_cache.len();
        self.timeline_render_ranges.push(start..end);
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    pub(super) fn extend_last_render_block_range_by_one_line(&mut self) {
        let Some(old_range) = self.timeline_render_ranges.last().cloned() else {
            return;
        };
        let new_range = old_range.start..old_range.end.saturating_add(1);
        for range in &mut self.timeline_render_ranges {
            if *range == old_range {
                *range = new_range.clone();
            }
        }
    }

    fn extend_timeline_render_buffers(&mut self, lines: Vec<Line<'static>>) {
        for line in lines {
            let plain = plain_line_text(&line);
            self.timeline_render_cache.push(line);
            self.timeline_plain_cache.push(plain.clone());
            let hash = hash_timeline_line(
                self.timeline_prefix_hashes.last().copied().unwrap_or(0),
                &plain,
            );
            self.timeline_prefix_hashes.push(hash);
        }
    }

    pub(super) fn rebuild_timeline_prefix_hashes_from(&mut self, start_line: usize) {
        let truncate_to = start_line.min(self.timeline_plain_cache.len());
        self.timeline_prefix_hashes.truncate(truncate_to);
        let mut hash = if truncate_to == 0 {
            0
        } else {
            self.timeline_prefix_hashes.last().copied().unwrap_or(0)
        };
        for line in self.timeline_plain_cache.iter().skip(truncate_to) {
            hash = hash_timeline_line(hash, line);
            self.timeline_prefix_hashes.push(hash);
        }
    }

    pub(super) fn trim_trailing_timeline_blanks(&mut self) {
        while self
            .timeline_render_cache
            .last()
            .map(|line| line.spans.is_empty())
            .unwrap_or(false)
        {
            let _ = self.timeline_render_cache.pop();
            let _ = self.timeline_plain_cache.pop();
            let _ = self.timeline_prefix_hashes.pop();
            if let Some(last_index) = self.timeline_render_ranges.len().checked_sub(1) {
                let old_range = self.timeline_render_ranges[last_index].clone();
                if old_range.end > old_range.start {
                    let new_range = old_range.start..old_range.end - 1;
                    self.timeline_render_ranges[last_index] = new_range.clone();
                    for range in self.timeline_render_ranges.iter_mut().take(last_index) {
                        if *range == old_range {
                            *range = new_range.clone();
                        }
                    }
                } else {
                    let _ = self.timeline_render_ranges.pop();
                }
            }
        }
    }

    pub(super) fn reset_scroll(&mut self) {
        self.timeline_scroll_back = 0;
        self.approval_scroll_back = 0;
        self.activity_scroll_back = 0;
    }

    pub(super) fn scroll_timeline(&mut self, delta: usize) {
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

    pub fn handle_mouse_scroll(&mut self, upward: bool) {
        let delta = self.terminal_scroll_sensitivity();
        if self.pending_approval.is_some() {
            if upward {
                self.approval_scroll_back = self.approval_scroll_back.saturating_sub(delta);
            } else {
                self.approval_scroll_back = self.approval_scroll_back.saturating_add(delta);
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
                if self.pending_approval.is_some() {
                    self.approval_scroll_back = self.approval_scroll_back.saturating_sub(delta);
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
                if self.pending_approval.is_some() {
                    self.approval_scroll_back = self.approval_scroll_back.saturating_add(delta);
                } else {
                    self.activity_scroll_back = self.activity_scroll_back.saturating_sub(delta);
                }
            }
        }
    }

    pub fn scrollback_lines(&self) -> Vec<Line<'static>> {
        self.scrollback_lines_from(0)
    }

    pub fn scrollback_lines_from(&self, from_index: usize) -> Vec<Line<'static>> {
        self.scrollback_lines_range(from_index, self.scrollback_cutoff_line())
    }

    pub fn scrollback_lines_range(&self, from_index: usize, to_index: usize) -> Vec<Line<'static>> {
        let cutoff_line = self.scrollback_cutoff_line();
        let start = from_index.min(cutoff_line);
        let end = to_index.min(cutoff_line).max(start);
        let mut lines = self.timeline_render_cache[start..end].to_vec();
        if end >= cutoff_line {
            while lines
                .last()
                .map(|line| !line_has_visible_content(line))
                .unwrap_or(false)
            {
                let _ = lines.pop();
            }
        }
        lines
    }

    pub fn scrollback_line_count(&self) -> usize {
        self.scrollback_cutoff_line()
    }

    pub fn scrollback_prefix_hash(&self, line_count: usize) -> u64 {
        let count = line_count.min(self.scrollback_cutoff_line());
        if count == 0 {
            return 0;
        }
        self.timeline_prefix_hashes
            .get(count - 1)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn visible_timeline_render_range(&self, max_lines: usize) -> Range<usize> {
        let effective_len = self.effective_timeline_render_len();
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
        self.timeline_render_ranges.get(entry_index).cloned()
    }

    pub(crate) fn timeline_plain_line(&self, line_index: usize) -> Option<&str> {
        self.timeline_plain_cache
            .get(line_index)
            .map(String::as_str)
    }

    pub(crate) fn transcript_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        if max_lines == 0 {
            return Vec::new();
        }

        if matches!(self.active_agent_view, AgentView::Child { .. }) {
            return self.child_agent_transcript_lines(max_lines);
        }

        let visible_range = self.visible_timeline_render_range(max_lines);
        if visible_range.is_empty() {
            return vec![
                Line::from("no messages yet"),
                Line::from("send a prompt to start"),
            ];
        }
        let selection = self.selected_timeline_line_range();
        self.timeline_render_cache[visible_range.clone()]
            .iter()
            .enumerate()
            .map(|(offset, line)| {
                let line_index = visible_range.start.saturating_add(offset);
                if let Some(columns) = self.selected_timeline_column_range(line_index) {
                    selected_timeline_line_columns(line.clone(), columns)
                } else if selection
                    .as_ref()
                    .is_some_and(|range| range.contains(&line_index))
                {
                    selected_timeline_line(line.clone())
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
        let effective_len = body
            .iter()
            .rposition(line_has_visible_content)
            .map(|index| index + 1)
            .unwrap_or(0);
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
        } = &self.active_agent_view
        else {
            return (Vec::new(), Vec::new());
        };
        let child = self.active_agent_child_entry();
        let active_label = self.active_agent_label();
        let mut header = vec![Line::from(vec![
            Span::styled("agent view", Style::default().fg(Color::Cyan)),
            Span::raw(format!(": {active_label}")),
            Span::raw(" · child session"),
        ])];
        if let Some(child) = child.as_ref() {
            header.push(Line::from(format!(
                "status: {} · {} · v{}:{}",
                task_child_session_status_label(child.status),
                child.role.as_str(),
                child.plan_version,
                child.step_id.as_str()
            )));
        }
        header.push(Line::from(format!(
            "session: {}",
            truncate_session_view_text(&child_session_ref.as_path().display().to_string(), 96)
        )));
        let mut body = Vec::new();
        let Some(transcript) = self.active_agent_child_transcript.as_ref() else {
            body.push(Line::from("child session not loaded"));
            return (header, body);
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
            return (header, body);
        }
        let timeline_entries =
            self.restored_timeline_entries_from_session_entries(&transcript.entries);
        if timeline_entries.is_empty() {
            body.push(Line::from("child session has no transcript messages yet"));
            return (header, body);
        }
        let mut options = self.timeline_render_options();
        options.selected_tool_activity_key = None;
        options.hovered_tool_activity_key = None;
        options.streaming_assistant_index = None;
        for (index, entry) in timeline_entries.iter().enumerate() {
            let rendered =
                crate::ui::render_timeline_entry_lines_with_options(entry, &options, index);
            if !rendered.is_empty() && !body.is_empty() {
                body.push(Line::raw(String::new()));
            }
            body.extend(rendered);
        }
        while body
            .last()
            .is_some_and(|line| !line_has_visible_content(line))
        {
            let _ = body.pop();
        }
        (header, body)
    }

    pub(crate) fn selected_timeline_line_range(&self) -> Option<Range<usize>> {
        let range = self.timeline_text_selection?.normalized_range();
        let end = range.end.min(self.timeline_plain_cache.len());
        (range.start < end).then_some(range.start..end)
    }

    pub(crate) fn selected_timeline_text(&self) -> Option<String> {
        let range = self.selected_timeline_line_range()?;
        if self
            .timeline_text_selection
            .and_then(TimelineTextSelection::normalized_column_bounds)
            .is_some()
        {
            return Some(
                range
                    .filter_map(|line_index| {
                        let line = self.timeline_plain_cache.get(line_index)?;
                        let columns = self.selected_timeline_column_range(line_index)?;
                        Some(text_by_display_columns(line, columns.start, columns.end))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .filter(|text| !text.is_empty());
        }
        Some(self.timeline_plain_cache[range].join("\n")).filter(|text| !text.is_empty())
    }

    pub(crate) fn begin_timeline_text_selection_at(
        &mut self,
        line_index: usize,
        column: usize,
    ) -> bool {
        if line_index >= self.timeline_plain_cache.len() {
            return self.clear_timeline_text_selection();
        }
        self.timeline_text_selection_anchor = Some(line_index);
        self.timeline_text_selection_anchor_column = Some(column);
        self.timeline_text_selection.take().is_some()
    }

    pub(crate) fn update_timeline_text_selection(&mut self, line_index: usize) -> bool {
        let Some(anchor) = self.timeline_text_selection_anchor else {
            return false;
        };
        let len = self.timeline_plain_cache.len();
        if len == 0 {
            return false;
        }
        let cursor = line_index.min(len.saturating_sub(1));
        let next = Some(TimelineTextSelection::line(anchor, cursor));
        let changed = self.timeline_text_selection != next;
        self.timeline_text_selection = next;
        changed
    }

    pub(crate) fn update_timeline_text_selection_at(
        &mut self,
        line_index: usize,
        column: usize,
    ) -> bool {
        let Some(anchor) = self.timeline_text_selection_anchor else {
            return false;
        };
        let Some(anchor_column) = self.timeline_text_selection_anchor_column else {
            return self.update_timeline_text_selection(line_index);
        };
        let len = self.timeline_plain_cache.len();
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
        let changed = self.timeline_text_selection != next;
        self.timeline_text_selection = next;
        changed
    }

    pub(crate) fn finish_timeline_text_selection(&mut self) -> bool {
        self.timeline_text_selection_anchor.take().is_some()
    }

    pub(crate) fn clear_timeline_text_selection(&mut self) -> bool {
        self.clear_timeline_text_selection_state()
    }

    fn clear_timeline_text_selection_state(&mut self) -> bool {
        let changed = self.timeline_text_selection.is_some()
            || self.timeline_text_selection_anchor.is_some()
            || self.timeline_text_selection_anchor_column.is_some();
        self.timeline_text_selection = None;
        self.timeline_text_selection_anchor = None;
        self.timeline_text_selection_anchor_column = None;
        changed
    }

    fn selected_timeline_column_range(&self, line_index: usize) -> Option<Range<usize>> {
        let selection = self.timeline_text_selection?;
        let (start_line, start_column, end_line, end_column) =
            selection.normalized_column_bounds()?;
        if line_index < start_line || line_index > end_line {
            return None;
        }
        let line = self.timeline_plain_cache.get(line_index)?;
        let line_width = UnicodeWidthStr::width(line.as_str());
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
        self.last_notice = Some(format!("copied {}", clipboard_copy_status(text)));
        self.push_event("selection:copy", clipboard_copy_status(text));
    }

    pub fn record_clipboard_copy_unavailable(&mut self, reason: &str) {
        self.last_notice = Some(format!("clipboard unavailable: {reason}"));
        self.push_event("selection:copy", format!("unavailable {reason}"));
    }

    pub fn timeline_revision(&self) -> u64 {
        self.timeline_revision
    }

    fn timeline_render_options(&self) -> crate::ui::TimelineRenderOptions {
        crate::ui::TimelineRenderOptions {
            expand_tool_previews: false,
            expand_thinking_blocks: matches!(self.thinking_block_mode, ThinkingBlockMode::Expanded),
            selected_tool_activity_key: self.selected_tool_activity_key.clone(),
            hovered_tool_activity_key: self.hovered_tool_activity_key(),
            expanded_tool_activity_keys: self.expanded_tool_activity_keys.clone(),
            collapsed_tool_activity_keys: self.collapsed_tool_activity_keys.clone(),
            max_content_width: self.timeline_content_width(),
            streaming_assistant_index: self.streaming_assistant_index,
            streaming_reasoning_index: self.streaming_reasoning_index,
            intermediate_assistant_indices: self.intermediate_assistant_indices(),
            expanded_thinking_entry_indices: self.expanded_thinking_entry_indices.clone(),
            collapsed_thinking_entry_indices: self.collapsed_thinking_entry_indices.clone(),
            hovered_thinking_entry_index: self.hovered_thinking_entry_index(),
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
        let total_width = self.terminal_width.max(24) as usize;
        let sidebar_width = sidebar_width_for_terminal(total_width);
        let live_panel_width = total_width
            .saturating_sub(sidebar_width)
            .saturating_sub(2)
            .max(10);
        live_panel_width.saturating_sub(4).max(20)
    }

    pub(crate) fn live_activity_summary(&self) -> Option<LiveActivitySummary> {
        if let Some(pending) = &self.pending_approval {
            return Some(LiveActivitySummary {
                label: "approval".to_owned(),
                detail: format!("waiting for decision on {}", pending.call.name),
            });
        }
        if !self.is_busy {
            return None;
        }
        if let Some(progress) = &self.mcp_progress {
            return Some(LiveActivitySummary {
                label: "mcp".to_owned(),
                detail: progress.detail.clone(),
            });
        }
        let (label, detail) = match &self.run_phase {
            RunPhase::Idle => ("working", "waiting for next event".to_owned()),
            RunPhase::Thinking => ("thinking", format!("reasoning with {}", self.model_name)),
            RunPhase::Tool(name) => ("tool", format!("running {name}")),
            RunPhase::Streaming => ("streaming", "writing the reply".to_owned()),
        };
        Some(LiveActivitySummary {
            label: label.to_owned(),
            detail,
        })
    }
}

fn selected_timeline_line(line: Line<'static>) -> Line<'static> {
    line.patch_style(timeline_selection_style())
}

pub(super) fn selected_timeline_line_columns(
    line: Line<'static>,
    columns: Range<usize>,
) -> Line<'static> {
    if columns.start >= columns.end {
        return line;
    }
    let mut display_column = 0usize;
    let mut selected_line = line;
    let spans = std::mem::take(&mut selected_line.spans);
    selected_line.spans = spans
        .into_iter()
        .flat_map(|span| split_span_for_column_selection(span, &mut display_column, &columns))
        .collect();
    selected_line
}

fn split_span_for_column_selection(
    span: Span<'static>,
    display_column: &mut usize,
    columns: &Range<usize>,
) -> Vec<Span<'static>> {
    let mut pieces = Vec::new();
    let mut current_text = String::new();
    let mut current_selected: Option<bool> = None;
    for grapheme in span.content.as_ref().graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
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
        ));
    }
    pieces
}

fn selection_span_piece(source: &Span<'static>, text: &str, selected: bool) -> Span<'static> {
    let style = if selected {
        source.style.patch(timeline_selection_style())
    } else {
        source.style
    };
    Span::styled(text.to_owned(), style)
}

fn timeline_selection_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Rgb(242, 171, 122))
}

pub(super) fn text_by_display_columns(text: &str, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    let mut output = String::new();
    let mut display_column = 0usize;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
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

pub(super) fn clipboard_copy_status(text: &str) -> String {
    let lines = text.lines().count().max(1);
    let chars = text.chars().count();
    format!("{lines} line(s), {chars} char(s)")
}
