use super::{AppState, PaneFocus, TimelineEntry, TimelineRole, ToolActivityCacheEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCardFocusSource {
    Mouse,
    Latest,
    Next,
    Previous,
}

impl ToolCardFocusSource {
    fn event_detail(self) -> &'static str {
        match self {
            Self::Mouse => "mouse",
            Self::Latest => "latest",
            Self::Next => "next",
            Self::Previous => "previous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCardRevealPolicy {
    KeepViewport,
    RevealEntry,
    PreserveTail,
}

impl AppState {
    pub(crate) fn has_tool_cards(&self) -> bool {
        !self.tool_activity_cache.is_empty()
    }

    pub(crate) fn tool_activity_entry_indices(&self) -> Vec<usize> {
        self.tool_activity_cache
            .iter()
            .map(|activity| activity.index)
            .collect()
    }

    pub(crate) fn select_tool_activity_entry(&mut self, entry_index: usize) -> bool {
        self.focus_tool_activity_entry(
            entry_index,
            ToolCardFocusSource::Mouse,
            ToolCardRevealPolicy::KeepViewport,
        )
    }

    pub(crate) fn toggle_tool_activity_entry(&mut self, entry_index: usize) -> bool {
        if !self.focus_tool_activity_entry(
            entry_index,
            ToolCardFocusSource::Mouse,
            ToolCardRevealPolicy::KeepViewport,
        ) {
            return false;
        }
        self.toggle_selected_tool_card_with_policy(ToolCardRevealPolicy::PreserveTail)
    }

    pub(crate) fn hovered_tool_activity_key(&self) -> Option<String> {
        let entry_index = match self.mouse_hover_target? {
            crate::mouse::HitTarget::ToolCardHeader { entry_index }
            | crate::mouse::HitTarget::ToolCardHiddenPreview { entry_index }
            | crate::mouse::HitTarget::ToolCard { entry_index } => entry_index,
            _ => return None,
        };
        self.tool_activity_cache
            .iter()
            .find_map(|activity| (activity.index == entry_index).then(|| activity.key.clone()))
    }

    pub(super) fn focus_latest_tool_card(&mut self) -> bool {
        let Some(entries) = self.tool_activity_entries() else {
            self.last_notice = Some("no activities yet".to_owned());
            return false;
        };
        let (selected_index, _) = entries
            .last()
            .cloned()
            .expect("tool activity entries are non-empty");
        self.focus_tool_activity_entry(
            selected_index,
            ToolCardFocusSource::Latest,
            ToolCardRevealPolicy::RevealEntry,
        )
    }

    pub(super) fn select_adjacent_tool_card(&mut self, forward: bool) -> bool {
        let Some(entries) = self.tool_activity_entries() else {
            self.last_notice = Some("no activities yet".to_owned());
            return false;
        };
        let (selected_index, _) = self.next_tool_entry(&entries, forward);
        let source = if forward {
            ToolCardFocusSource::Next
        } else {
            ToolCardFocusSource::Previous
        };
        self.focus_tool_activity_entry(selected_index, source, ToolCardRevealPolicy::RevealEntry)
    }

    pub(super) fn toggle_selected_tool_card(&mut self) -> bool {
        self.toggle_selected_tool_card_with_policy(ToolCardRevealPolicy::PreserveTail)
    }

    pub(super) fn begin_tool_card_body_click(&mut self, entry_index: usize) {
        self.pending_tool_card_body_click_entry = Some(entry_index);
    }

    pub(super) fn cancel_tool_card_body_click(&mut self) {
        self.pending_tool_card_body_click_entry = None;
    }

    pub(super) fn take_pending_tool_card_body_click(
        &mut self,
        target: crate::mouse::HitTarget,
    ) -> Option<usize> {
        let pending = self.pending_tool_card_body_click_entry.take()?;
        match target {
            crate::mouse::HitTarget::ToolCard { entry_index } if entry_index == pending => {
                Some(entry_index)
            }
            _ => None,
        }
    }

    fn focus_tool_activity_entry(
        &mut self,
        entry_index: usize,
        source: ToolCardFocusSource,
        reveal_policy: ToolCardRevealPolicy,
    ) -> bool {
        let Some(entries) = self.tool_activity_entries() else {
            return false;
        };
        let Some((selected_index, selected_key)) =
            entries.iter().find(|(index, _)| *index == entry_index)
        else {
            return false;
        };
        let previous_index = self.selected_tool_entry_index(&entries);
        self.selected_tool_activity_key = Some(selected_key.clone());
        self.active_pane = PaneFocus::Activity;
        self.rerender_tool_selection_change(previous_index, *selected_index);
        self.apply_tool_card_reveal_policy(*selected_index, reveal_policy, false);
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:focus", source.event_detail());
        self.last_notice = Some(self.tool_card_status_line());
        true
    }

    fn toggle_selected_tool_card_with_policy(
        &mut self,
        reveal_policy: ToolCardRevealPolicy,
    ) -> bool {
        let Some(entries) = self.tool_activity_entries() else {
            self.last_notice = Some("no activities yet".to_owned());
            return false;
        };
        let (selected_index, selected_key) = self.ensure_selected_tool_entry(&entries);
        let was_at_tail = self.timeline_scroll_back == 0;
        self.active_pane = PaneFocus::Activity;
        if self.tool_entry_is_open_by_key(selected_index, &selected_key) {
            self.expanded_tool_activity_keys.remove(&selected_key);
            if self.tool_entry_defaults_to_expanded(selected_index) {
                self.collapsed_tool_activity_keys
                    .insert(selected_key.clone());
            }
        } else if self.tool_entry_defaults_to_expanded(selected_index) {
            self.collapsed_tool_activity_keys.remove(&selected_key);
        } else if !self
            .expanded_tool_activity_keys
            .insert(selected_key.clone())
        {
            self.expanded_tool_activity_keys.remove(&selected_key);
        } else {
            self.collapsed_tool_activity_keys.remove(&selected_key);
        }
        self.rerender_timeline_entry(selected_index);
        self.apply_tool_card_reveal_policy(selected_index, reveal_policy, was_at_tail);
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:view", "toggle");
        self.last_notice = Some(self.tool_card_status_line());
        true
    }

    pub(super) fn clear_tool_card_focus(&mut self) -> bool {
        let previous_index = self
            .selected_tool_activity_key
            .as_deref()
            .and_then(|key| self.timeline_entry_index_for_activity_key(key));
        if self.selected_tool_activity_key.take().is_none() {
            return false;
        }
        if let Some(index) = previous_index {
            self.rerender_timeline_entry(index);
        }
        self.active_pane = PaneFocus::Composer;
        self.refresh_usage_sidebar_cache();
        self.push_event("tool:focus", "clear");
        self.last_notice = Some("activity focus cleared".to_owned());
        true
    }

    #[cfg(test)]
    pub(super) fn tool_timeline_entry_indices(&self) -> Option<Vec<usize>> {
        self.tool_activity_entries()
            .map(|entries| entries.into_iter().map(|(index, _)| index).collect())
    }

    pub(super) fn tool_activity_entries(&self) -> Option<Vec<(usize, String)>> {
        let entries = self
            .tool_activity_cache
            .iter()
            .map(|activity| (activity.index, activity.key.clone()))
            .collect::<Vec<_>>();
        (!entries.is_empty()).then_some(entries)
    }

    pub(super) fn timeline_entry_index_for_activity_key(
        &self,
        activity_key: &str,
    ) -> Option<usize> {
        self.tool_activity_cache
            .iter()
            .find_map(|activity| (activity.key == activity_key).then_some(activity.index))
    }

    pub(super) fn tool_activity_cache_entry(
        &self,
        index: usize,
        entry: &TimelineEntry,
    ) -> Option<ToolActivityCacheEntry> {
        (entry.role == TimelineRole::Tool)
            .then(|| crate::ui::tool_activity_view(entry, index))
            .flatten()
            .map(|activity| ToolActivityCacheEntry {
                index,
                key: activity.key,
                defaults_expanded: activity.defaults_expanded,
            })
    }

    pub(super) fn ensure_selected_tool_entry(
        &mut self,
        entries: &[(usize, String)],
    ) -> (usize, String) {
        if let Some(selected_key) = self.selected_tool_activity_key.as_deref()
            && let Some((index, key)) = entries.iter().find(|(_, key)| key == selected_key)
        {
            return (*index, key.clone());
        }
        let latest = entries
            .last()
            .cloned()
            .expect("tool activity entries are non-empty");
        self.selected_tool_activity_key = Some(latest.1.clone());
        latest
    }

    pub(super) fn next_tool_entry(
        &mut self,
        entries: &[(usize, String)],
        forward: bool,
    ) -> (usize, String) {
        let (_, current_key) = self.ensure_selected_tool_entry(entries);
        let position = entries
            .iter()
            .position(|(_, key)| key == &current_key)
            .unwrap_or(0);
        let next_position = if forward {
            (position + 1) % entries.len()
        } else if position == 0 {
            entries.len() - 1
        } else {
            position - 1
        };
        entries[next_position].clone()
    }

    fn selected_tool_entry_index(&self, entries: &[(usize, String)]) -> Option<usize> {
        let selected_key = self.selected_tool_activity_key.as_deref()?;
        entries
            .iter()
            .find_map(|(index, key)| (key == selected_key).then_some(*index))
    }

    pub(super) fn rerender_tool_selection_change(
        &mut self,
        previous_index: Option<usize>,
        selected_index: usize,
    ) {
        if previous_index == Some(selected_index) {
            self.rerender_timeline_entry(selected_index);
            return;
        }
        if let Some(index) = previous_index {
            self.rerender_timeline_entry(index);
        }
        self.rerender_timeline_entry(selected_index);
    }

    pub(super) fn reveal_timeline_entry(&mut self, entry_index: usize) {
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

    fn apply_tool_card_reveal_policy(
        &mut self,
        entry_index: usize,
        reveal_policy: ToolCardRevealPolicy,
        was_at_tail: bool,
    ) {
        match reveal_policy {
            ToolCardRevealPolicy::KeepViewport => {}
            ToolCardRevealPolicy::RevealEntry => self.reveal_timeline_entry(entry_index),
            ToolCardRevealPolicy::PreserveTail => {
                self.reveal_timeline_entry(entry_index);
                if was_at_tail {
                    self.timeline_scroll_back = 0;
                }
            }
        }
    }

    pub(super) fn tool_card_status_line(&self) -> String {
        let Some(entries) = self.tool_activity_entries() else {
            return "activities: none".to_owned();
        };
        let selected = self
            .selected_tool_activity_key
            .as_deref()
            .and_then(|selected_key| entries.iter().position(|(_, key)| key == selected_key))
            .map(|position| position + 1)
            .unwrap_or(entries.len());
        let (selected_entry, selected_key) = self
            .selected_tool_activity_key
            .as_deref()
            .and_then(|selected_key| {
                entries
                    .iter()
                    .find(|(_, key)| key == selected_key)
                    .map(|(index, key)| (*index, key.clone()))
            })
            .unwrap_or_else(|| entries.last().cloned().unwrap_or((0, String::new())));
        let open = self.tool_entry_is_open_by_key(selected_entry, &selected_key);
        format!(
            "activity {selected}/{} {}",
            entries.len(),
            if open { "open" } else { "brief" }
        )
    }

    fn tool_entry_is_open_by_key(&self, entry_index: usize, key: &str) -> bool {
        self.expanded_tool_activity_keys.contains(key)
            || (self.tool_entry_defaults_to_expanded(entry_index)
                && !self.collapsed_tool_activity_keys.contains(key))
    }

    fn tool_entry_defaults_to_expanded(&self, entry_index: usize) -> bool {
        self.tool_activity_cache
            .iter()
            .find(|activity| activity.index == entry_index)
            .map(|activity| activity.defaults_expanded)
            .unwrap_or(false)
    }
}
