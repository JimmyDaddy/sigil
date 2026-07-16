use std::cell::Ref;

use sigil_kernel::{
    AgentResultContinuationProjection, AgentThreadStateProjection, SessionLogEntry,
    TaskStateProjection,
};

use super::formatting::truncate_session_view_text;
use super::session_flow::short_session_token;
use super::{AppState, RunPhase, SessionViewCache, TimelineRole, agent_flow, session_review};

impl AppState {
    pub(crate) fn run_phase(&self) -> RunPhase {
        self.runtime.run_phase.clone()
    }

    pub(crate) fn run_phase_label(&self) -> String {
        match &self.runtime.run_phase {
            RunPhase::Idle => "ready".to_owned(),
            RunPhase::Thinking => "thinking".to_owned(),
            RunPhase::Agent(profile_id) => format!("agent @{profile_id}"),
            RunPhase::Tool(name) => format!("tool {name}"),
            RunPhase::Streaming => "streaming".to_owned(),
        }
    }

    pub(crate) fn session_display_title(&self) -> String {
        self.timeline
            .iter()
            .find(|entry| entry.role == TimelineRole::User)
            .and_then(|entry| {
                entry
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| truncate_session_view_text(line, 56))
            })
            .unwrap_or_else(|| {
                format!(
                    "{} · {}",
                    self.workspace_root
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("session"),
                    self.runtime.model_name
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn latest_user_prompt_preview(&self) -> Option<String> {
        let entry = self
            .timeline
            .iter()
            .rev()
            .find(|entry| entry.role == TimelineRole::User)?;
        let mut visible_lines = entry
            .text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let first_line = visible_lines.next()?;
        let extra_lines = visible_lines.count();
        Some(if extra_lines == 0 {
            first_line.to_owned()
        } else {
            format!("{first_line}  +{extra_lines} more")
        })
    }

    pub(crate) fn session_sidebar_lines(&self) -> Vec<String> {
        vec![
            format!("provider: {}", self.runtime.provider_name),
            format!("model: {}", self.runtime.model_name),
            format!("effort: {}", self.runtime.reasoning_effort.as_str()),
            format!("phase: {}", self.run_phase_label()),
            format!("session: {}", short_session_token(&self.session_id)),
        ]
    }

    pub(crate) fn task_memory_sidebar_lines(&self) -> Vec<String> {
        vec!["task memory: no active V2 checkpoint".to_owned()]
    }

    pub(crate) fn session_review_sidebar_lines(&self) -> Vec<String> {
        self.session_view_cache().session_review_lines.clone()
    }

    pub(crate) fn task_sidebar_lines(&self) -> Vec<String> {
        self.session_view_cache().task_sidebar_lines.clone()
    }

    pub(crate) fn task_strip_view(&self) -> Option<super::task_sidebar::TaskStripView> {
        let mut view = self.session_view_cache().task_strip_view.clone()?;
        if let Some(verification) = view.verification.as_mut()
            && self
                .review
                .latest_checkpoint_restore_sequence
                .is_some_and(|restore| {
                    restore
                        > self
                            .review
                            .readiness_sequences_by_scope
                            .get(&verification.scope)
                            .copied()
                            .unwrap_or(0)
                })
        {
            verification.status = "stale after checkpoint restore".to_owned();
            verification.why = Some("workspace changed; refresh verification evidence".to_owned());
            verification.action = None;
            verification.inspect_lines.push(
                "checkpoint restore is newer than the latest readiness evaluation".to_owned(),
            );
        }
        Some(view)
    }

    pub(in crate::app) fn session_view_cache(&self) -> Ref<'_, SessionViewCache> {
        self.ensure_session_view_cache();
        self.session_browser.view_cache.borrow()
    }

    fn ensure_session_view_cache(&self) {
        let needs_refresh = {
            let cache = self.session_browser.view_cache.borrow();
            cache.entries_len != self.session_browser.current_entries.len()
                || cache.entries_revision != self.session_browser.current_entries_revision
        };
        if needs_refresh {
            let cache = self.build_session_view_cache();
            *self.session_browser.view_cache.borrow_mut() = cache;
        }
    }

    pub(in crate::app) fn mark_current_session_entries_changed(&mut self) {
        self.session_browser.current_entries_revision = self
            .session_browser
            .current_entries_revision
            .saturating_add(1);
        self.refresh_session_view_cache();
    }

    pub(in crate::app) fn refresh_session_view_cache(&mut self) {
        let cache = self.build_session_view_cache();
        *self.session_browser.view_cache.borrow_mut() = cache;
    }

    fn build_session_view_cache(&self) -> SessionViewCache {
        let entries = &self.session_browser.current_entries;
        let task_projection = TaskStateProjection::from_entries(entries);
        let agent_projection = AgentThreadStateProjection::from_entries(entries);
        let continuation_projection = AgentResultContinuationProjection::from_entries(entries);
        let agent_child_items = agent_flow::agent_sidebar_child_items_from_projections(
            &task_projection,
            &agent_projection,
            &continuation_projection,
        );
        let agent_graph_summary_line =
            sigil_runtime::agent_graph_product_summary_from_entries(entries)
                .map(|summary| summary.display_line());
        SessionViewCache {
            entries_len: entries.len(),
            entries_revision: self.session_browser.current_entries_revision,
            task_projection,
            agent_projection,
            task_sidebar_lines: super::task_sidebar::task_sidebar_lines(entries),
            task_strip_view: super::task_sidebar::task_strip_view(entries),
            agent_child_items,
            agent_graph_summary_line,
            compaction_preview_line: self.compaction_preview_sidebar_line(entries),
            session_review_lines: session_review::session_review_sidebar_lines(
                &self.session_log_path,
                entries,
            ),
        }
    }

    fn compaction_preview_sidebar_line(&self, entries: &[SessionLogEntry]) -> Option<String> {
        if entries.is_empty() || !self.compaction_config.enabled {
            return None;
        }
        Some("compact: inspect V2 plan with /compact".to_owned())
    }
}
