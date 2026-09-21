use super::{AppState, TimelineEntry, TimelineRole};

fn has_running_elapsed(entry: &TimelineEntry) -> bool {
    if entry.role != TimelineRole::Tool {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&entry.text) else {
        return false;
    };
    let Some(details) = value.pointer("/metadata/details") else {
        return false;
    };
    let execution = details.get("terminal_task").unwrap_or(details);
    let started = details
        .get("started_at_ms")
        .or_else(|| execution.get("started_at_ms"));
    let status = execution
        .get("status")
        .or_else(|| value.get("status"))
        .and_then(serde_json::Value::as_str);
    started.and_then(serde_json::Value::as_u64).is_some()
        && matches!(status, Some("starting" | "running"))
}

impl AppState {
    pub(super) fn track_command_elapsed(&mut self, index: usize) {
        if self.timeline.get(index).is_some_and(has_running_elapsed) {
            self.timeline_state.running_command_indices.insert(index);
        } else {
            self.timeline_state.running_command_indices.remove(&index);
        }
    }

    pub(super) fn rebuild_command_elapsed_tracking(&mut self) {
        self.timeline_state.running_command_indices = self
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| has_running_elapsed(entry).then_some(index))
            .collect();
    }

    pub(crate) fn has_running_command_elapsed(&self) -> bool {
        !self.timeline_state.running_command_indices.is_empty()
    }

    pub(crate) fn refresh_command_elapsed(&mut self, now_ms: u64) -> bool {
        // Keep a user-owned text selection stable while only the elapsed label changes.
        if self.timeline_state.text_selection.is_some() {
            return false;
        }
        let second = now_ms / 1_000;
        if self.timeline_state.command_elapsed_second == Some(second) {
            return false;
        }
        self.timeline_state.command_elapsed_second = Some(second);
        let indices = self
            .timeline_state
            .running_command_indices
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let changed = !indices.is_empty();
        for index in indices {
            self.rerender_timeline_entry(index);
        }
        changed
    }
}

#[cfg(test)]
#[path = "tests/command_elapsed_tests.rs"]
mod tests;
