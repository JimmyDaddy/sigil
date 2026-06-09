use ratatui::text::Line;

use super::*;

struct InspectionGroupProjection {
    end: usize,
    tool_indices: Vec<usize>,
}

impl AppState {
    pub(super) fn live_panel_height(&self) -> u16 {
        self.terminal_height
            .saturating_sub(self.footer_strip_height())
            .saturating_sub(1)
            .max(1)
    }

    pub(super) fn timeline_viewport_rows(&self) -> usize {
        self.live_panel_height()
            .saturating_sub(u16::from(self.live_activity_summary().is_some()))
            .max(1) as usize
    }

    pub(super) fn max_timeline_scroll_back(&self) -> usize {
        let total = self.effective_timeline_render_len();
        let viewport = self.timeline_viewport_rows().max(1);
        total.saturating_sub(viewport)
    }

    pub(super) fn effective_timeline_render_len(&self) -> usize {
        self.timeline_render_cache
            .iter()
            .rposition(line_has_visible_content)
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn scrollback_cutoff_line(&self) -> usize {
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
        let is_tool = role == TimelineRole::Tool;
        let previous_selected_tool = self.selected_tool_activity_key.clone();
        self.timeline.push(TimelineEntry {
            role,
            text: text.into(),
        });
        let mut trimmed_front = false;
        if self.timeline.len() > 400 {
            self.timeline.remove(0);
            self.reindex_timeline_state_after_front_trim();
            trimmed_front = true;
        }
        if is_tool {
            self.selected_tool_activity_key = self
                .timeline
                .last()
                .and_then(|entry| {
                    let index = self.timeline.len().saturating_sub(1);
                    self.tool_activity_key_at(index, entry)
                })
                .map(|(_, key)| key);
        }
        if trimmed_front {
            self.rebuild_timeline_render_cache();
            return;
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

    fn reindex_timeline_state_after_front_trim(&mut self) {
        self.streaming_assistant_index = self
            .streaming_assistant_index
            .and_then(|index| index.checked_sub(1));
        self.streaming_reasoning_index = self
            .streaming_reasoning_index
            .and_then(|index| index.checked_sub(1));
        self.retain_existing_tool_activity_state();
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
        self.streaming_reasoning_index = None;
        if let Some(index) = self.streaming_assistant_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry(index);
            return;
        }

        self.push_timeline(TimelineRole::Assistant, delta);
        self.streaming_assistant_index = self.timeline.len().checked_sub(1);
    }

    pub(super) fn append_reasoning_delta(&mut self, delta: &str) {
        self.streaming_assistant_index = None;
        if let Some(index) = self.streaming_reasoning_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry(index);
            return;
        }

        self.push_timeline(TimelineRole::Thinking, delta);
        self.streaming_reasoning_index = self.timeline.len().checked_sub(1);
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
        self.rebuild_timeline_render_cache();
        self.last_notice = Some(format!("thinking {}", self.thinking_block_mode.as_str()));
        self.push_event("thinking:view", self.thinking_block_mode.as_str());
    }

    pub(super) fn rebuild_timeline_render_cache(&mut self) {
        let options = self.timeline_render_options();
        self.timeline_render_cache.clear();
        self.timeline_plain_cache.clear();
        self.timeline_prefix_hashes.clear();
        let (rendered, ranges) =
            self.render_timeline_projection_range(0..self.timeline.len(), &options);
        self.timeline_render_ranges = ranges;
        self.extend_timeline_render_buffers(rendered);
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    pub(super) fn rerender_timeline_entry(&mut self, index: usize) {
        if let Some(source_range) = self.timeline_inspection_projection_bounds(index) {
            self.rerender_timeline_projection_range(source_range);
            return;
        }
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

    fn append_timeline_render_cache_entry(&mut self, index: usize) {
        if let Some(source_range) = self.timeline_inspection_projection_bounds(index)
            && source_range.start < index
        {
            self.rerender_timeline_projection_range(source_range);
            return;
        }
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

    fn render_timeline_projection_range(
        &self,
        source_range: Range<usize>,
        options: &crate::ui::TimelineRenderOptions,
    ) -> (Vec<Line<'static>>, Vec<Range<usize>>) {
        let mut lines = Vec::new();
        let mut ranges = Vec::new();
        let mut index = source_range.start;
        while index < source_range.end {
            let start = lines.len();
            if let Some(group) = self.inspection_group_projection_before(index, source_range.end) {
                let entries = group
                    .tool_indices
                    .iter()
                    .filter_map(|entry_index| {
                        self.timeline
                            .get(*entry_index)
                            .map(|entry| (*entry_index, entry))
                    })
                    .collect::<Vec<_>>();
                lines.extend(crate::ui::render_inspected_group_lines(&entries, options));
                let end = lines.len();
                for _ in index..group.end {
                    ranges.push(start..end);
                }
                index = group.end;
                continue;
            }
            if let Some(entry) = self.timeline.get(index) {
                lines.extend(crate::ui::render_timeline_entry_lines_with_options(
                    entry, options, index,
                ));
            }
            let end = lines.len();
            ranges.push(start..end);
            index += 1;
        }
        (lines, ranges)
    }

    fn rerender_timeline_projection_range(&mut self, source_range: Range<usize>) {
        if source_range.is_empty() {
            return;
        }
        let rendered_source_end = source_range.end.min(self.timeline_render_ranges.len());
        let existing_start = self
            .timeline_render_ranges
            .get(source_range.start)
            .map(|range| range.start)
            .unwrap_or(self.timeline_render_cache.len());
        let existing_end = if rendered_source_end > source_range.start {
            self.timeline_render_ranges[rendered_source_end - 1].end
        } else {
            existing_start
        };
        let options = self.timeline_render_options();
        let (new_lines, relative_ranges) =
            self.render_timeline_projection_range(source_range.clone(), &options);
        let new_plain = new_lines.iter().map(plain_line_text).collect::<Vec<_>>();
        let old_len = existing_end.saturating_sub(existing_start);
        let new_len = new_lines.len();
        self.timeline_render_cache
            .splice(existing_start..existing_end, new_lines);
        self.timeline_plain_cache
            .splice(existing_start..existing_end, new_plain);
        let absolute_ranges = relative_ranges
            .into_iter()
            .map(|range| existing_start + range.start..existing_start + range.end)
            .collect::<Vec<_>>();
        self.timeline_render_ranges
            .splice(source_range.start..rendered_source_end, absolute_ranges);
        if new_len != old_len {
            let delta = new_len as isize - old_len as isize;
            for range in self
                .timeline_render_ranges
                .iter_mut()
                .skip(source_range.end)
            {
                range.start = range.start.saturating_add_signed(delta);
                range.end = range.end.saturating_add_signed(delta);
            }
        }
        self.rebuild_timeline_prefix_hashes_from(existing_start);
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    fn timeline_inspection_projection_bounds(&self, index: usize) -> Option<Range<usize>> {
        if !self.timeline_entry_can_participate_in_inspection_projection(index) {
            return None;
        }
        let mut start = index;
        while start > 0 && self.timeline_entry_can_participate_in_inspection_projection(start - 1) {
            start -= 1;
        }
        let mut end = index + 1;
        while self.timeline_entry_can_participate_in_inspection_projection(end) {
            end += 1;
        }
        Some(start..end)
    }

    fn inspection_group_projection_before(
        &self,
        start: usize,
        limit: usize,
    ) -> Option<InspectionGroupProjection> {
        let mut index = start;
        let mut end = start;
        let mut tool_indices = Vec::new();
        while index < limit {
            if self.timeline_entry_is_inspection_tool(index) {
                tool_indices.push(index);
                end = index + 1;
                index += 1;
                continue;
            }
            if self.timeline_entry_is_inspection_bridge_notice(index) {
                index += 1;
                continue;
            }
            break;
        }
        (tool_indices.len() > 1).then_some(InspectionGroupProjection { end, tool_indices })
    }

    fn timeline_entry_can_participate_in_inspection_projection(&self, index: usize) -> bool {
        self.timeline_entry_is_inspection_tool(index)
            || self.timeline_entry_is_inspection_bridge_notice(index)
    }

    fn timeline_entry_is_inspection_bridge_notice(&self, index: usize) -> bool {
        self.timeline
            .get(index)
            .is_some_and(is_inspection_bridge_notice)
    }

    fn timeline_entry_is_inspection_tool(&self, index: usize) -> bool {
        self.timeline
            .get(index)
            .filter(|entry| entry.role == TimelineRole::Tool)
            .and_then(|entry| crate::ui::tool_activity_view(entry, index))
            .is_some_and(|activity| activity.is_inspection)
    }

    fn extend_last_render_block_range_by_one_line(&mut self) {
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

    fn rebuild_timeline_prefix_hashes_from(&mut self, start_line: usize) {
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

    fn trim_trailing_timeline_blanks(&mut self) {
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
        let delta = 3;
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
        let cutoff_line = self.scrollback_cutoff_line();
        let start = from_index.min(cutoff_line);
        let mut lines = self.timeline_render_cache
            [start..cutoff_line.min(self.timeline_render_cache.len())]
            .to_vec();
        while lines
            .last()
            .map(|line| !line_has_visible_content(line))
            .unwrap_or(false)
        {
            let _ = lines.pop();
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

    pub(crate) fn transcript_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        let effective_len = self.effective_timeline_render_len();
        if effective_len == 0 {
            return vec![
                Line::from("no messages yet"),
                Line::from("send a prompt to start"),
            ];
        }
        let viewport = max_lines.max(1);
        let scroll_back = self
            .timeline_scroll_back
            .min(effective_len.saturating_sub(viewport));
        let end = effective_len.saturating_sub(scroll_back);
        let start = end.saturating_sub(viewport);
        self.timeline_render_cache[start..end].to_vec()
    }

    pub fn timeline_revision(&self) -> u64 {
        self.timeline_revision
    }

    fn timeline_render_options(&self) -> crate::ui::TimelineRenderOptions {
        crate::ui::TimelineRenderOptions {
            expand_tool_previews: false,
            expand_thinking_blocks: matches!(self.thinking_block_mode, ThinkingBlockMode::Expanded),
            selected_tool_activity_key: self.selected_tool_activity_key.clone(),
            expanded_tool_activity_keys: self.expanded_tool_activity_keys.clone(),
            collapsed_tool_activity_keys: self.collapsed_tool_activity_keys.clone(),
            max_content_width: self.timeline_content_width(),
        }
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

fn is_inspection_bridge_notice(entry: &TimelineEntry) -> bool {
    entry.role == TimelineRole::Notice
        && entry.text.starts_with("permission ")
        && entry.text.contains(" mode=allow")
}
