use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::{Component, Path},
};

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

const TOOL_CARD_VISIBLE_ROWS_STEP: usize = 64;
const TERMINAL_TASK_LOG_LABEL_ROOT: &str = "state/artifacts/tasks";
const TERMINAL_TASK_LOG_PREVIEW_MAX_BYTES: usize = 512 * 1024;

impl AppState {
    pub(crate) fn has_tool_cards(&self) -> bool {
        !self.timeline_state.tool_activity_cache.is_empty()
    }

    pub(crate) fn tool_activity_entry_indices(&self) -> Vec<usize> {
        self.timeline_state
            .tool_activity_cache
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
        self.timeline_state
            .tool_activity_cache
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
        self.timeline_state.selected_tool_activity_key = Some(selected_key.clone());
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
            if let Some(visible_rows) = self
                .timeline_state
                .tool_activity_visible_rows
                .get(&selected_key)
                .copied()
            {
                if self.tool_entry_has_expandable_terminal_log(selected_index)
                    || !self.tool_entry_visible_rows_are_complete(selected_index, visible_rows)
                {
                    let visible_rows = visible_rows.saturating_add(TOOL_CARD_VISIBLE_ROWS_STEP);
                    self.timeline_state
                        .tool_activity_visible_rows
                        .insert(selected_key.clone(), visible_rows);
                    self.expand_terminal_task_preview_to_rows(selected_index, visible_rows);
                } else {
                    self.close_tool_entry_by_key(selected_index, &selected_key);
                }
            } else {
                self.close_tool_entry_by_key(selected_index, &selected_key);
            }
        } else if self.tool_entry_defaults_to_expanded(selected_index) {
            self.timeline_state
                .collapsed_tool_activity_keys
                .remove(&selected_key);
            self.timeline_state
                .tool_activity_visible_rows
                .remove(&selected_key);
        } else if !self
            .timeline_state
            .expanded_tool_activity_keys
            .insert(selected_key.clone())
        {
            self.timeline_state
                .expanded_tool_activity_keys
                .remove(&selected_key);
            self.timeline_state
                .tool_activity_visible_rows
                .remove(&selected_key);
        } else {
            self.timeline_state
                .collapsed_tool_activity_keys
                .remove(&selected_key);
            self.timeline_state
                .tool_activity_visible_rows
                .insert(selected_key.clone(), TOOL_CARD_VISIBLE_ROWS_STEP);
            self.expand_terminal_task_preview_to_rows(selected_index, TOOL_CARD_VISIBLE_ROWS_STEP);
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
            .timeline_state
            .selected_tool_activity_key
            .as_deref()
            .and_then(|key| self.timeline_entry_index_for_activity_key(key));
        if self
            .timeline_state
            .selected_tool_activity_key
            .take()
            .is_none()
        {
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
            .timeline_state
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
        self.timeline_state
            .tool_activity_cache
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

    pub(super) fn rebuild_tool_activity_cache(&mut self) {
        let selected_key = self.timeline_state.selected_tool_activity_key.clone();
        self.timeline_state.tool_activity_cache = self
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| self.tool_activity_cache_entry(index, entry))
            .collect();
        if let Some(selected_key) = selected_key.as_deref()
            && self
                .timeline_state
                .tool_activity_cache
                .iter()
                .any(|activity| activity.key == selected_key)
        {
            self.timeline_state.selected_tool_activity_key = Some(selected_key.to_owned());
            return;
        }
        self.timeline_state.selected_tool_activity_key = self
            .timeline_state
            .tool_activity_cache
            .last()
            .map(|activity| activity.key.clone());
    }

    pub(super) fn ensure_selected_tool_entry(
        &mut self,
        entries: &[(usize, String)],
    ) -> (usize, String) {
        if let Some(selected_key) = self.timeline_state.selected_tool_activity_key.as_deref()
            && let Some((index, key)) = entries.iter().find(|(_, key)| key == selected_key)
        {
            return (*index, key.clone());
        }
        let latest = entries
            .last()
            .cloned()
            .expect("tool activity entries are non-empty");
        self.timeline_state.selected_tool_activity_key = Some(latest.1.clone());
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
        let selected_key = self.timeline_state.selected_tool_activity_key.as_deref()?;
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
        let Some(range) = self.timeline_entry_render_range(entry_index) else {
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
            .timeline_state
            .selected_tool_activity_key
            .as_deref()
            .and_then(|selected_key| entries.iter().position(|(_, key)| key == selected_key))
            .map(|position| position + 1)
            .unwrap_or(entries.len());
        let (selected_entry, selected_key) = self
            .timeline_state
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
        self.timeline_state
            .expanded_tool_activity_keys
            .contains(key)
            || (self.tool_entry_defaults_to_expanded(entry_index)
                && !self
                    .timeline_state
                    .collapsed_tool_activity_keys
                    .contains(key))
    }

    fn close_tool_entry_by_key(&mut self, entry_index: usize, key: &str) {
        self.timeline_state.expanded_tool_activity_keys.remove(key);
        self.timeline_state.tool_activity_visible_rows.remove(key);
        if self.tool_entry_defaults_to_expanded(entry_index) {
            self.timeline_state
                .collapsed_tool_activity_keys
                .insert(key.to_owned());
        }
    }

    fn tool_entry_visible_rows_are_complete(
        &self,
        entry_index: usize,
        visible_rows: usize,
    ) -> bool {
        let Some(available_rows) = self.tool_entry_available_preview_rows(entry_index) else {
            return false;
        };
        visible_rows >= available_rows
    }

    fn tool_entry_available_preview_rows(&self, entry_index: usize) -> Option<usize> {
        let entry = self.timeline.get(entry_index)?;
        let value = serde_json::from_str::<serde_json::Value>(&entry.text).ok()?;
        let object = value.as_object()?;
        let preview_rows = object
            .get("preview_lines")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let hidden_rows = object
            .get("hidden_lines")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize;
        let diff_rows = object
            .get("diff")
            .and_then(|diff| diff.get("files"))
            .and_then(serde_json::Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(|file| file.get("lines"))
                    .filter_map(serde_json::Value::as_array)
                    .map(Vec::len)
                    .sum::<usize>()
            })
            .unwrap_or(0);
        Some(preview_rows.saturating_add(hidden_rows).max(diff_rows))
    }

    fn tool_entry_has_expandable_terminal_log(&self, entry_index: usize) -> bool {
        let Some(value) = self.tool_entry_json(entry_index) else {
            return false;
        };
        if !tool_entry_json_is_terminal_task(&value) {
            return false;
        }
        let hidden_rows = value
            .get("hidden_lines")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        hidden_rows > 0 && terminal_task_log_label_path(&value).is_some()
    }

    fn expand_terminal_task_preview_to_rows(&mut self, entry_index: usize, visible_rows: usize) {
        let Some(mut value) = self.tool_entry_json(entry_index) else {
            return;
        };
        if !tool_entry_json_is_terminal_task(&value) {
            return;
        }
        let Some(log_path) = terminal_task_log_label_path(&value) else {
            return;
        };
        let Some((lines, has_more)) = self.read_terminal_task_log_preview(&log_path, visible_rows)
        else {
            return;
        };
        let Some(object) = value.as_object_mut() else {
            return;
        };
        object.insert(
            "preview_lines".to_owned(),
            serde_json::Value::Array(lines.into_iter().map(serde_json::Value::String).collect()),
        );
        object.insert(
            "hidden_lines".to_owned(),
            serde_json::Value::Number(u64::from(has_more).into()),
        );
        if let Some(entry) = self.timeline.get_mut(entry_index)
            && let Ok(text) = serde_json::to_string(&value)
        {
            entry.text = text;
        }
    }

    fn read_terminal_task_log_preview(
        &self,
        log_path: &Path,
        requested_rows: usize,
    ) -> Option<(Vec<String>, bool)> {
        let log_path = self.resolve_terminal_task_log_path(log_path)?;
        let file = File::open(log_path).ok()?;
        let mut reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut bytes = 0usize;
        let mut has_more = false;
        let mut line = String::new();
        let requested_rows = requested_rows.max(1);
        loop {
            line.clear();
            let read = reader.read_line(&mut line).ok()?;
            if read == 0 {
                break;
            }
            bytes = bytes.saturating_add(read);
            if lines.len() >= requested_rows || bytes > TERMINAL_TASK_LOG_PREVIEW_MAX_BYTES {
                has_more = true;
                break;
            }
            let trimmed = line.trim_end_matches(['\r', '\n']).to_owned();
            lines.push(trimmed);
        }
        Some((lines, has_more))
    }

    fn resolve_terminal_task_log_path(&self, log_path: &Path) -> Option<std::path::PathBuf> {
        if log_path.is_absolute()
            || log_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return None;
        }
        let suffix = log_path
            .strip_prefix(Path::new(TERMINAL_TASK_LOG_LABEL_ROOT))
            .ok()?;
        Some(self.sigil_paths.terminal_tasks_root.join(suffix))
    }

    fn tool_entry_json(&self, entry_index: usize) -> Option<serde_json::Value> {
        let entry = self.timeline.get(entry_index)?;
        serde_json::from_str::<serde_json::Value>(&entry.text).ok()
    }

    fn tool_entry_defaults_to_expanded(&self, entry_index: usize) -> bool {
        self.timeline_state
            .tool_activity_cache
            .iter()
            .find(|activity| activity.index == entry_index)
            .map(|activity| activity.defaults_expanded)
            .unwrap_or(false)
    }
}

fn tool_entry_json_is_terminal_task(value: &serde_json::Value) -> bool {
    value
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|tool_name| tool_name == "terminal_task")
}

fn terminal_task_log_label_path(value: &serde_json::Value) -> Option<std::path::PathBuf> {
    value
        .get("metadata")?
        .get("details")?
        .get("terminal_task")?
        .get("log_path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(std::path::PathBuf::from)
}
