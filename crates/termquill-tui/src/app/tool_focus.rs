use super::*;

impl AppState {
    pub(crate) fn has_tool_cards(&self) -> bool {
        self.tool_timeline_entry_indices().is_some()
    }

    pub(super) fn focus_latest_tool_card(&mut self) -> bool {
        let Some(indices) = self.tool_timeline_entry_indices() else {
            self.last_notice = Some("no tool cards yet".to_owned());
            return false;
        };
        let selected = *indices.last().expect("tool entry indices are non-empty");
        self.selected_tool_timeline_entry = Some(selected);
        self.rebuild_timeline_render_cache();
        self.reveal_timeline_entry(selected);
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:focus", "latest");
        self.last_notice = Some(self.tool_card_status_line());
        true
    }

    pub(super) fn select_adjacent_tool_card(&mut self, forward: bool) -> bool {
        let Some(indices) = self.tool_timeline_entry_indices() else {
            self.last_notice = Some("no tool cards yet".to_owned());
            return false;
        };
        let selected = self.next_tool_entry(&indices, forward);
        self.selected_tool_timeline_entry = Some(selected);
        self.rebuild_timeline_render_cache();
        self.reveal_timeline_entry(selected);
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:focus", if forward { "next" } else { "previous" });
        self.last_notice = Some(self.tool_card_status_line());
        true
    }

    pub(super) fn toggle_selected_tool_card(&mut self) -> bool {
        let Some(indices) = self.tool_timeline_entry_indices() else {
            self.last_notice = Some("no tool cards yet".to_owned());
            return false;
        };
        let selected = self.ensure_selected_tool_entry(&indices);
        if self.tool_entry_is_open(selected) {
            self.expanded_tool_timeline_entries.remove(&selected);
            if self.tool_entry_defaults_to_expanded(selected) {
                self.collapsed_tool_timeline_entries.insert(selected);
            }
        } else if self.tool_entry_defaults_to_expanded(selected) {
            self.collapsed_tool_timeline_entries.remove(&selected);
        } else if !self.expanded_tool_timeline_entries.insert(selected) {
            self.expanded_tool_timeline_entries.remove(&selected);
        } else {
            self.collapsed_tool_timeline_entries.remove(&selected);
        }
        self.rebuild_timeline_render_cache();
        self.reveal_timeline_entry(selected);
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:view", "toggle");
        self.last_notice = Some(self.tool_card_status_line());
        true
    }

    pub(super) fn clear_tool_card_focus(&mut self) -> bool {
        if self.selected_tool_timeline_entry.take().is_none() {
            return false;
        }
        self.rebuild_timeline_render_cache();
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:focus", "clear");
        self.last_notice = Some("tool focus cleared".to_owned());
        true
    }

    pub(super) fn tool_timeline_entry_indices(&self) -> Option<Vec<usize>> {
        let indices = self
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| (entry.role == TimelineRole::Tool).then_some(index))
            .collect::<Vec<_>>();
        (!indices.is_empty()).then_some(indices)
    }

    fn ensure_selected_tool_entry(&mut self, indices: &[usize]) -> usize {
        if let Some(selected) = self
            .selected_tool_timeline_entry
            .filter(|index| indices.contains(index))
        {
            return selected;
        }
        let latest = *indices.last().expect("tool entry indices are non-empty");
        self.selected_tool_timeline_entry = Some(latest);
        latest
    }

    fn next_tool_entry(&mut self, indices: &[usize], forward: bool) -> usize {
        let current = self.ensure_selected_tool_entry(indices);
        let position = indices
            .iter()
            .position(|index| *index == current)
            .unwrap_or(0);
        let next_position = if forward {
            (position + 1) % indices.len()
        } else if position == 0 {
            indices.len() - 1
        } else {
            position - 1
        };
        indices[next_position]
    }

    fn reveal_timeline_entry(&mut self, entry_index: usize) {
        let Some(range) = self.timeline_render_ranges.get(entry_index) else {
            return;
        };
        let effective_len = self.effective_timeline_render_len();
        if effective_len == 0 {
            return;
        }
        let entry_end = range.end.min(effective_len).max(1);
        self.timeline_scroll_back = effective_len
            .saturating_sub(entry_end)
            .min(self.max_timeline_scroll_back());
    }

    pub(super) fn tool_card_status_line(&self) -> String {
        let Some(indices) = self.tool_timeline_entry_indices() else {
            return "tools: none".to_owned();
        };
        let selected = self
            .selected_tool_timeline_entry
            .and_then(|entry| indices.iter().position(|index| *index == entry))
            .map(|position| position + 1)
            .unwrap_or(indices.len());
        let selected_entry = self
            .selected_tool_timeline_entry
            .unwrap_or(*indices.last().unwrap_or(&0));
        let open = self.tool_entry_is_open(selected_entry);
        format!(
            "tool card {selected}/{} {}",
            indices.len(),
            if open { "open" } else { "brief" }
        )
    }

    fn tool_entry_is_open(&self, entry_index: usize) -> bool {
        self.expanded_tool_timeline_entries.contains(&entry_index)
            || (self.tool_entry_defaults_to_expanded(entry_index)
                && !self.collapsed_tool_timeline_entries.contains(&entry_index))
    }

    fn tool_entry_defaults_to_expanded(&self, entry_index: usize) -> bool {
        let Some(entry) = self.timeline.get(entry_index) else {
            return false;
        };
        if entry.role != TimelineRole::Tool {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(&entry.text)
            .ok()
            .and_then(|value| value.get("diff").cloned())
            .is_some()
    }
}
