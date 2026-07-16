use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    str,
};

use crate::{
    agent_display::{AgentDisplayNameInput, resolve_agent_display_name},
    slash::{ResolvedSlashCommand, SlashSelectorEntry},
    timeline::{
        SidebarAgentRow, TimelineEntry, TimelineRole, agent_status_symbol, compact_agent_detail,
    },
};
use anyhow::Context;
use sigil_kernel::{
    AgentResultContinuationProjection, AgentThreadDisplayNameEntry, AgentThreadId,
    AgentThreadProjection, AgentThreadStateProjection, AgentThreadStatus, ControlEntry,
    JsonlSessionStore, SessionLogEntry, TaskRunProjection, TaskStateProjection,
    normalize_task_agent_display_name,
};

use super::{
    ActiveAgentChildTranscript, AgentSidebarItem, AgentView, AppAction, AppState,
    ChildTranscriptFileSignature,
};

const CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT: usize = 80;
const CHILD_AGENT_TRANSCRIPT_RAW_LINE_LIMIT: usize = CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT * 16;
const CHILD_AGENT_TRANSCRIPT_TAIL_CHUNK_SIZE: usize = 32 * 1024;

impl AppState {
    pub(crate) fn active_agent_label(&self) -> String {
        match &self.agent_panel.active_view {
            AgentView::Main => "main".to_owned(),
            AgentView::Child { child_task_id, .. } => self
                .agent_sidebar_items()
                .into_iter()
                .find(|item| {
                    item.target
                        .as_ref()
                        .is_some_and(|target| target == &self.agent_panel.active_view)
                })
                .map(|item| agent_display_label(&item).to_owned())
                .unwrap_or_else(|| child_task_id.clone()),
        }
    }

    pub(crate) fn agent_sidebar_rows(&self) -> Vec<SidebarAgentRow> {
        self.agent_sidebar_items()
            .into_iter()
            .enumerate()
            .map(|(index, item)| SidebarAgentRow {
                active: item
                    .target
                    .as_ref()
                    .is_some_and(|target| target == &self.agent_panel.active_view),
                label: item.label,
                detail: item.detail,
                selected: index == self.agent_panel.selected,
                muted: item.muted,
            })
            .collect()
    }

    pub(crate) fn agent_graph_summary_line(&self) -> Option<String> {
        self.session_view_cache().agent_graph_summary_line.clone()
    }

    pub(crate) fn composer_agent_rows(&self) -> Vec<SidebarAgentRow> {
        bounded_composer_agent_rows(
            self.agent_sidebar_rows()
                .into_iter()
                .filter(|row| !row.muted)
                .collect(),
        )
    }

    pub(crate) fn composer_agent_panel_rows(&self) -> u16 {
        let rows = self.composer_agent_rows();
        if rows.len() <= 1 {
            return 0;
        }
        1 + rows.len().min(COMPOSER_AGENT_VISIBLE_ROWS) as u16
            + u16::from(self.composer.agent_panel_focused)
    }

    pub(crate) fn is_composer_agent_panel_focused(&self) -> bool {
        self.composer.agent_panel_focused
    }

    #[allow(dead_code)] // Kept for the composer agent panel path while queue focus is being revised.
    pub(super) fn focus_composer_agent_panel(&mut self) -> bool {
        if !self.composer_agent_panel_available() {
            self.composer.agent_panel_focused = false;
            return false;
        }
        self.select_active_agent_sidebar_item();
        self.composer.agent_panel_focused = true;
        self.last_notice = Some("agent list focused".to_owned());
        true
    }

    pub(super) fn blur_composer_agent_panel(&mut self) {
        self.composer.agent_panel_focused = false;
    }

    pub(super) fn move_composer_agent_selection(&mut self, next: bool) -> bool {
        let items = self.agent_sidebar_items();
        let selectable = selectable_agent_indexes(&items);
        if selectable.is_empty() {
            self.composer.agent_panel_focused = false;
            return false;
        }
        let current = selectable
            .iter()
            .position(|index| *index == self.agent_panel.selected)
            .or_else(|| {
                selectable.iter().position(|index| {
                    items[*index]
                        .target
                        .as_ref()
                        .is_some_and(|target| target == &self.agent_panel.active_view)
                })
            })
            .unwrap_or(0);
        let next_position = if next {
            (current + 1) % selectable.len()
        } else {
            current.saturating_sub(1)
        };
        if next_position == current {
            return false;
        }
        self.agent_panel.selected = selectable[next_position];
        true
    }

    pub(super) fn agent_slash_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        if let Some(query) = agent_rename_selector_query(arg) {
            return self.agent_rename_slash_entries(query);
        }
        let query = arg.trim().to_ascii_lowercase();
        let mut entries = Vec::new();
        for (index, item) in self.agent_sidebar_items().into_iter().enumerate() {
            let Some(value) = agent_command_value(&item) else {
                continue;
            };
            let label = agent_display_label(&item).to_owned();
            let search = format!(
                "{} {} {} {} {}",
                index + 1,
                label.to_ascii_lowercase(),
                value.to_ascii_lowercase(),
                item.label.to_ascii_lowercase(),
                item.detail.to_ascii_lowercase()
            );
            if !query.is_empty() && !search.contains(&query) {
                continue;
            }
            let active = item
                .target
                .as_ref()
                .is_some_and(|target| target == &self.agent_panel.active_view);
            let symbol = if active {
                "◉"
            } else {
                agent_status_symbol(&item.detail)
            };
            entries.push(SlashSelectorEntry {
                fill: format!("/agent {value}"),
                label,
                description: format!("{symbol} {}", compact_agent_detail(&item.detail)),
                resolved: ResolvedSlashCommand {
                    canonical: "/agent".to_owned(),
                    arg: value,
                },
            });
        }

        entries
    }

    pub(super) fn agent_selector_allows_popup(&self, arg: &str) -> bool {
        !(agent_rename_is_entering_display_name(arg)
            || agent_close_prefix(arg)
            || agent_cancel_prefix(arg)
            || agent_message_prefix(arg))
    }

    fn agent_rename_slash_entries(&self, query: &str) -> Vec<SlashSelectorEntry> {
        let query = query.trim().to_ascii_lowercase();
        let mut entries = Vec::new();
        for (index, item) in self.agent_sidebar_items().into_iter().enumerate() {
            let Some(value) = agent_command_value(&item) else {
                continue;
            };
            if value == "main" {
                continue;
            }
            let label = agent_display_label(&item).to_owned();
            let search = format!(
                "{} {} {} {}",
                index + 1,
                label.to_ascii_lowercase(),
                value.to_ascii_lowercase(),
                item.detail.to_ascii_lowercase()
            );
            if !query.is_empty() && !search.contains(&query) {
                continue;
            }
            entries.push(SlashSelectorEntry {
                fill: format!("/agent rename {value} "),
                label,
                description: format!("rename {}", compact_agent_detail(&item.detail)),
                resolved: ResolvedSlashCommand {
                    canonical: "/agent".to_owned(),
                    arg: format!("rename {value}"),
                },
            });
        }
        entries
    }

    pub(super) fn activate_agent_from_command(
        &mut self,
        arg: &str,
    ) -> anyhow::Result<Option<AppAction>> {
        let value = arg.trim();
        if value.is_empty() {
            self.last_notice =
                Some("usage: /agent <main|next|prev|child-id|rename target name>".to_owned());
            return Ok(None);
        }
        if agent_rename_prefix(value) {
            if let Some(rename_args) = agent_rename_args(value) {
                self.rename_agent_from_command(rename_args)?;
                return Ok(None);
            }
            self.last_notice = Some("usage: /agent rename <child-id|current> <name>".to_owned());
            return Ok(None);
        }
        if agent_close_prefix(value) {
            if let Some(target) = agent_action_target(value, "close") {
                return self.close_agent_from_command(target);
            }
            self.last_notice = Some("usage: /agent close <agent|current>".to_owned());
            return Ok(None);
        }
        if agent_cancel_prefix(value) {
            if let Some(target) = agent_action_target(value, "cancel") {
                return self.cancel_agent_from_command(target);
            }
            self.last_notice = Some("usage: /agent cancel <agent|current>".to_owned());
            return Ok(None);
        }
        if agent_message_prefix(value) {
            if let Some(args) = agent_message_args(value) {
                return self.message_agent_from_command(args);
            }
            self.last_notice = Some("usage: /agent message <agent|current> <prompt>".to_owned());
            return Ok(None);
        }
        match value.to_ascii_lowercase().as_str() {
            "next" | "n" => {
                self.cycle_agent_view(false);
            }
            "prev" | "previous" | "p" => {
                self.cycle_agent_view(true);
            }
            _ => {
                if !self.activate_agent_view_by_value(value) {
                    self.last_notice = Some(format!("agent not found: {value}"));
                }
            }
        }
        Ok(None)
    }

    fn close_agent_from_command(&mut self, target: &str) -> anyhow::Result<Option<AppAction>> {
        let Some(view) = self.agent_view_for_action_target(target) else {
            self.last_notice = Some(format!("agent not found: {target}"));
            return Ok(None);
        };
        let Some(thread) = self.agent_thread_projection_for_view(&view) else {
            self.last_notice = Some(format!("agent close unavailable: {target}"));
            return Ok(None);
        };
        let thread_id = thread.thread_id.clone();
        if !thread.status.is_terminal() {
            self.last_notice = Some(format!(
                "agent close unavailable until terminal: {}",
                thread_id.as_str()
            ));
            self.push_event("agent:close-unavailable", thread_id.as_str());
            return Ok(None);
        }
        self.last_notice = Some(format!("agent close requested: {}", thread_id.as_str()));
        self.push_event("agent:close-requested", thread_id.as_str());
        Ok(Some(AppAction::CloseAgent {
            thread_id,
            reason: Some("closed from TUI /agent".to_owned()),
        }))
    }

    fn message_agent_from_command(
        &mut self,
        args: AgentMessageArgs<'_>,
    ) -> anyhow::Result<Option<AppAction>> {
        let Some(view) = self.agent_view_for_action_target(args.target) else {
            self.last_notice = Some(format!("agent not found: {}", args.target));
            return Ok(None);
        };
        let Some(thread_id) = self.agent_thread_id_for_view(&view) else {
            self.last_notice = Some(format!("agent message unavailable: {}", args.target));
            return Ok(None);
        };
        self.last_notice = Some(format!("agent message requested: {}", thread_id.as_str()));
        self.push_event("agent:message-requested", thread_id.as_str());
        Ok(Some(AppAction::MessageAgent {
            thread_id,
            prompt: args.prompt.to_owned(),
        }))
    }

    pub(super) fn apply_agent_thread_closed(
        &mut self,
        thread_id: AgentThreadId,
        entries: Vec<SessionLogEntry>,
    ) {
        let closing_active = self
            .agent_thread_id_for_view(&self.agent_panel.active_view)
            .as_ref()
            .is_some_and(|active_thread_id| active_thread_id == &thread_id);
        self.sync_current_session_state(entries);
        if closing_active {
            self.agent_panel.active_view = AgentView::Main;
            self.agent_panel.active_child_transcript = None;
        }
        self.last_notice = Some(format!("agent closed: {}", thread_id.as_str()));
        self.push_event("agent:close", thread_id.as_str());
    }

    pub(super) fn apply_agent_thread_cancelled(
        &mut self,
        thread_id: AgentThreadId,
        entries: Vec<SessionLogEntry>,
    ) {
        self.sync_current_session_state(entries);
        self.last_notice = Some(format!("agent cancelled: {}", thread_id.as_str()));
        self.push_event("agent:cancel", thread_id.as_str());
    }

    fn cancel_agent_from_command(&mut self, target: &str) -> anyhow::Result<Option<AppAction>> {
        let Some(view) = self.agent_view_for_action_target(target) else {
            self.last_notice = Some(format!("agent not found: {target}"));
            return Ok(None);
        };
        let Some(thread) = self.agent_thread_projection_for_view(&view) else {
            self.last_notice = Some(format!("agent cancel unavailable: {target}"));
            return Ok(None);
        };
        let thread_id = thread.thread_id.clone();
        if thread.status.is_terminal() {
            self.last_notice = Some(format!(
                "agent cancel unavailable after terminal: {}",
                thread_id.as_str()
            ));
            self.push_event("agent:cancel-unavailable", thread_id.as_str());
            return Ok(None);
        }
        self.last_notice = Some(format!("agent cancel requested: {}", thread_id.as_str()));
        self.push_event("agent:cancel-requested", thread_id.as_str());
        Ok(Some(AppAction::CancelAgent {
            thread_id,
            reason: Some("cancelled from TUI /agent".to_owned()),
        }))
    }

    pub(super) fn cycle_agent_view(&mut self, reverse: bool) -> bool {
        let items = self.agent_sidebar_items();
        let selectable = selectable_agent_indexes(&items);
        if selectable.len() <= 1 {
            self.last_notice = Some("no child agents to switch".to_owned());
            return false;
        }

        let current = selectable
            .iter()
            .position(|index| {
                items[*index]
                    .target
                    .as_ref()
                    .is_some_and(|target| target == &self.agent_panel.active_view)
            })
            .unwrap_or(0);
        let next_position = if reverse {
            current
                .checked_sub(1)
                .unwrap_or_else(|| selectable.len().saturating_sub(1))
        } else {
            (current + 1) % selectable.len()
        };
        self.activate_agent_view_at_index(selectable[next_position])
    }

    pub(super) fn activate_selected_agent_view(&mut self) {
        if !self.activate_agent_view_at_index(self.agent_panel.selected) {
            self.push_timeline(
                TimelineRole::Notice,
                self.last_notice
                    .clone()
                    .unwrap_or_else(|| "no agent selected".to_owned()),
            );
        }
    }

    pub(super) fn close_selected_agent_from_panel(&mut self) -> anyhow::Result<Option<AppAction>> {
        let Some(target) = self.selected_agent_command_value() else {
            self.last_notice = Some("no agent selected".to_owned());
            return Ok(None);
        };
        if target == "main" {
            self.last_notice = Some("agent close unavailable for main".to_owned());
            return Ok(None);
        }
        self.close_agent_from_command(&target)
    }

    pub(super) fn begin_message_selected_agent_from_panel(&mut self) -> bool {
        let Some(target) = self.selected_agent_command_value() else {
            self.last_notice = Some("no agent selected".to_owned());
            return false;
        };
        if target == "main" {
            self.last_notice = Some("agent message unavailable for main".to_owned());
            return false;
        }
        self.set_input_and_cursor(format!("/agent message {target} "));
        self.blur_composer_agent_panel();
        self.blur_composer_queue_panel();
        self.reset_slash_selector();
        self.reset_input_history_navigation();
        self.last_notice = Some(format!("compose agent message: {target}"));
        true
    }

    pub(super) fn refresh_active_agent_view_after_parent_sync(&mut self) {
        let items = self.agent_sidebar_items();
        if self.agent_panel.selected >= items.len() {
            self.agent_panel.selected = items.len().saturating_sub(1);
        }
        if !self.composer_agent_panel_available() {
            self.composer.agent_panel_focused = false;
        }
        if self.agent_panel.active_view == AgentView::Main {
            self.agent_panel.active_child_transcript = None;
            return;
        }
        let still_available = items
            .iter()
            .filter_map(|item| item.target.as_ref())
            .any(|target| target == &self.agent_panel.active_view);
        if still_available {
            if self.agent_panel.active_child_transcript.is_none()
                || self.active_agent_view_is_terminal()
            {
                self.reload_active_agent_child_transcript();
            }
        } else {
            self.agent_panel.active_view = AgentView::Main;
            self.agent_panel.active_child_transcript = None;
        }
    }

    fn active_agent_view_is_terminal(&self) -> bool {
        if let Some(thread) = self.active_agent_thread_projection() {
            return thread.status.is_terminal();
        }
        self.active_agent_child_entry().is_some_and(|child| {
            matches!(
                child.status,
                sigil_kernel::TaskChildSessionStatus::Completed
                    | sigil_kernel::TaskChildSessionStatus::Failed
                    | sigil_kernel::TaskChildSessionStatus::Cancelled
                    | sigil_kernel::TaskChildSessionStatus::Interrupted
                    | sigil_kernel::TaskChildSessionStatus::Unavailable
            )
        })
    }

    pub(super) fn active_agent_child_entry(&self) -> Option<sigil_kernel::TaskChildSessionEntry> {
        let AgentView::Child {
            child_task_id,
            child_session_ref,
        } = &self.agent_panel.active_view
        else {
            return None;
        };
        self.session_view_cache()
            .task_projection
            .latest_task()
            .and_then(|task| {
                task.child_sessions.values().find(|child| {
                    child.child_task_id.as_str() == child_task_id
                        && child.child_session_ref == *child_session_ref
                })
            })
            .cloned()
    }

    fn activate_agent_view_by_value(&mut self, value: &str) -> bool {
        let normalized = value.trim().to_ascii_lowercase();
        let items = self.agent_sidebar_items();
        let target_index = items.iter().enumerate().find_map(|(index, item)| {
            let command_value = agent_command_value(item)?;
            let ordinal = (index + 1).to_string();
            let label = agent_display_label(item).to_ascii_lowercase();
            (normalized == command_value.to_ascii_lowercase()
                || normalized == label
                || normalized == item.label.to_ascii_lowercase()
                || normalized == ordinal)
                .then_some(index)
        });
        target_index.is_some_and(|index| self.activate_agent_view_at_index(index))
    }

    pub(super) fn activate_agent_view_at_index(&mut self, index: usize) -> bool {
        let Some(item) = self.agent_sidebar_items().get(index).cloned() else {
            self.last_notice = Some("no agent selected".to_owned());
            return false;
        };
        let Some(target) = item.target else {
            self.last_notice = Some(format!("agent focus unavailable: {}", item.detail));
            return false;
        };
        self.agent_panel.selected = index;
        self.agent_panel.active_view = target;
        self.blur_composer_queue_panel();
        self.refresh_conversation_queue_selection();
        if self.active_pane == super::PaneFocus::Composer && self.composer_agent_panel_available() {
            self.composer.agent_panel_focused = true;
        }
        self.reload_active_agent_child_transcript();
        self.timeline_scroll_back = 0;
        self.last_notice = Some(format!("agent focus: {} · {}", item.label, item.detail));
        self.push_event("agent:focus", format!("{} · {}", item.label, item.detail));
        true
    }

    fn agent_sidebar_items(&self) -> Vec<AgentSidebarItem> {
        let mut items = vec![AgentSidebarItem {
            label: "main".to_owned(),
            detail: if self.runtime.is_busy {
                "running in current session".to_owned()
            } else {
                "idle in current session".to_owned()
            },
            target: Some(AgentView::Main),
            thread_id: None,
            muted: false,
        }];
        items.extend(self.session_view_cache().agent_child_items.clone());
        items
    }

    fn rename_agent_from_command(&mut self, args: AgentRenameArgs<'_>) -> anyhow::Result<()> {
        let display_name = match normalize_task_agent_display_name(args.display_name) {
            Ok(display_name) => display_name,
            Err(error) => {
                self.last_notice = Some(format!("agent rename failed: {error}"));
                return Ok(());
            }
        };
        let Some(target) = self.agent_view_for_action_target(args.target) else {
            self.last_notice = Some(format!("agent not found: {}", args.target));
            return Ok(());
        };
        let Some(thread_id) = self.agent_thread_id_for_view(&target) else {
            self.last_notice = Some(format!("agent rename unavailable: {}", args.target));
            return Ok(());
        };
        self.append_control_to_current_session(ControlEntry::AgentThreadDisplayName(
            AgentThreadDisplayNameEntry {
                thread_id: thread_id.clone(),
                display_name: display_name.clone(),
            },
        ))?;
        self.last_notice = Some(format!(
            "agent renamed: {} -> {display_name}",
            thread_id.as_str()
        ));
        self.push_event(
            "agent:rename",
            format!("{} -> {display_name}", thread_id.as_str()),
        );
        Ok(())
    }

    fn agent_view_for_action_target(&self, target: &str) -> Option<AgentView> {
        if matches!(target.to_ascii_lowercase().as_str(), "current" | ".") {
            return match &self.agent_panel.active_view {
                AgentView::Main => None,
                view => Some(view.clone()),
            };
        }
        let index = self.agent_sidebar_item_index_by_value(target)?;
        let items = self.agent_sidebar_items();
        match items.get(index)?.target.as_ref()? {
            AgentView::Main => None,
            view => Some(view.clone()),
        }
    }

    fn agent_sidebar_item_index_by_value(&self, value: &str) -> Option<usize> {
        let normalized = value.trim().to_ascii_lowercase();
        self.agent_sidebar_items()
            .iter()
            .enumerate()
            .find_map(|(index, item)| {
                let command_value = agent_command_value(item)?;
                let ordinal = (index + 1).to_string();
                let label = agent_display_label(item).to_ascii_lowercase();
                (normalized == command_value.to_ascii_lowercase()
                    || normalized == label
                    || normalized == item.label.to_ascii_lowercase()
                    || normalized == ordinal)
                    .then_some(index)
            })
    }

    fn agent_thread_id_for_view(&self, view: &AgentView) -> Option<AgentThreadId> {
        let items = self.agent_sidebar_items();
        items
            .iter()
            .find(|item| item.target.as_ref().is_some_and(|target| target == view))
            .and_then(|item| item.thread_id.clone())
    }

    fn agent_thread_projection_for_id(
        &self,
        thread_id: &AgentThreadId,
    ) -> Option<AgentThreadProjection> {
        self.session_view_cache()
            .agent_projection
            .threads
            .get(thread_id)
            .cloned()
    }

    pub(crate) fn active_agent_thread_projection(&self) -> Option<AgentThreadProjection> {
        self.agent_thread_projection_for_view(&self.agent_panel.active_view)
    }

    fn agent_thread_projection_for_view(&self, view: &AgentView) -> Option<AgentThreadProjection> {
        let thread_id = self.agent_thread_id_for_view(view)?;
        self.agent_thread_projection_for_id(&thread_id)
    }

    fn composer_agent_panel_available(&self) -> bool {
        self.composer_agent_rows().len() > 1
    }

    #[allow(dead_code)] // Used by focus_composer_agent_panel when that focus path is enabled.
    fn select_active_agent_sidebar_item(&mut self) {
        if let Some(index) = self.agent_sidebar_items().iter().position(|item| {
            item.target
                .as_ref()
                .is_some_and(|target| target == &self.agent_panel.active_view)
        }) {
            self.agent_panel.selected = index;
        }
    }

    fn selected_agent_command_value(&self) -> Option<String> {
        self.agent_sidebar_items()
            .get(self.agent_panel.selected)
            .and_then(agent_command_value)
    }

    pub(super) fn reload_active_agent_child_transcript(&mut self) -> bool {
        let AgentView::Child {
            child_session_ref, ..
        } = &self.agent_panel.active_view
        else {
            let changed = self.agent_panel.active_child_transcript.is_some();
            self.agent_panel.active_child_transcript = None;
            return changed;
        };
        let parent_dir = self
            .session_log_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let path = child_session_ref.resolve(parent_dir);
        let file_signature = match child_transcript_file_signature(&path) {
            Ok(file_signature) => file_signature,
            Err(error) => {
                let error = error.to_string();
                let changed = !self
                    .agent_panel
                    .active_child_transcript
                    .as_ref()
                    .is_some_and(|transcript| {
                        transcript.path == path
                            && transcript.file_signature == ChildTranscriptFileSignature::empty()
                            && transcript.load_error.as_deref() == Some(error.as_str())
                            && transcript.timeline_entries.is_empty()
                            && transcript.rendered_body_lines.is_empty()
                    });
                self.agent_panel.active_child_transcript = Some(ActiveAgentChildTranscript {
                    path: path.clone(),
                    file_signature: ChildTranscriptFileSignature::empty(),
                    timeline_entries: Vec::new(),
                    rendered_body_lines: Vec::new(),
                    total_timeline_entries: 0,
                    transcript_truncated: false,
                    load_error: Some(error),
                });
                return changed;
            }
        };
        if self
            .agent_panel
            .active_child_transcript
            .as_ref()
            .is_some_and(|transcript| {
                transcript.path == path && transcript.file_signature == file_signature
            })
        {
            return false;
        }
        let load_result = read_recent_session_entries(
            &path,
            CHILD_AGENT_TRANSCRIPT_RAW_LINE_LIMIT,
            file_signature.clone(),
        );
        self.agent_panel.active_child_transcript = Some(match load_result {
            Ok(recent) => {
                let (timeline_entries, total_timeline_entries) =
                    self.bounded_child_timeline_entries(&recent.entries);
                let rendered_body_lines = self.render_child_timeline_body_lines(&timeline_entries);
                ActiveAgentChildTranscript {
                    path,
                    file_signature: recent.file_signature,
                    timeline_entries,
                    rendered_body_lines,
                    total_timeline_entries,
                    transcript_truncated: recent.truncated,
                    load_error: None,
                }
            }
            Err(error) => ActiveAgentChildTranscript {
                path,
                file_signature,
                timeline_entries: Vec::new(),
                rendered_body_lines: Vec::new(),
                total_timeline_entries: 0,
                transcript_truncated: false,
                load_error: Some(error.to_string()),
            },
        });
        true
    }

    pub(super) fn rerender_active_agent_child_transcript(&mut self) {
        let Some(timeline_entries) = self
            .agent_panel
            .active_child_transcript
            .as_ref()
            .map(|transcript| transcript.timeline_entries.clone())
        else {
            return;
        };
        let rendered_body_lines = self.render_child_timeline_body_lines(&timeline_entries);
        if let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() {
            transcript.rendered_body_lines = rendered_body_lines;
        }
    }

    fn bounded_child_timeline_entries(
        &self,
        entries: &[SessionLogEntry],
    ) -> (Vec<TimelineEntry>, usize) {
        let timeline_entries = self.restored_timeline_entries_from_session_entries(entries);
        let total_timeline_entries = timeline_entries.len();
        if total_timeline_entries <= CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT {
            return (timeline_entries, total_timeline_entries);
        }
        (
            timeline_entries
                .into_iter()
                .skip(total_timeline_entries - CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT)
                .collect(),
            total_timeline_entries,
        )
    }
}

#[derive(Debug)]
struct RecentSessionEntries {
    entries: Vec<SessionLogEntry>,
    file_signature: ChildTranscriptFileSignature,
    truncated: bool,
}

fn child_transcript_file_signature(path: &Path) -> anyhow::Result<ChildTranscriptFileSignature> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(ChildTranscriptFileSignature {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(ChildTranscriptFileSignature::empty())
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to stat child session {}", path.display()))
        }
    }
}

fn read_recent_session_entries(
    path: &Path,
    max_lines: usize,
    file_signature: ChildTranscriptFileSignature,
) -> anyhow::Result<RecentSessionEntries> {
    if file_signature == ChildTranscriptFileSignature::empty() || max_lines == 0 {
        return Ok(RecentSessionEntries {
            entries: Vec::new(),
            file_signature,
            truncated: false,
        });
    }
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let (bytes, truncated_by_seek) =
        read_tail_jsonl_bytes(&mut file, file_signature.len, max_lines)
            .with_context(|| format!("failed to read recent entries from {}", path.display()))?;
    let mut lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        let _ = lines.pop();
    }
    let start = lines.len().saturating_sub(max_lines);
    let mut truncated = truncated_by_seek || start > 0;
    let mut entries = Vec::new();
    for raw_line in lines.into_iter().skip(start).rev() {
        if entries.len() >= CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT {
            truncated = true;
            break;
        }
        let line = str::from_utf8(raw_line)
            .with_context(|| format!("failed to decode recent entry from {}", path.display()))?
            .trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(entry) = JsonlSessionStore::session_entry_from_json_line(line)
            .with_context(|| recent_session_entry_parse_error(path))?
        {
            entries.push(entry);
        }
    }
    entries.reverse();
    Ok(RecentSessionEntries {
        entries,
        file_signature,
        truncated,
    })
}

fn recent_session_entry_parse_error(path: &Path) -> String {
    let path = path.display();
    format!("failed to parse recent session entry from {path}")
}

fn read_tail_jsonl_bytes(
    file: &mut File,
    file_len: u64,
    max_lines: usize,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut position = file_len;
    let mut newline_count = 0usize;
    let mut chunks = Vec::new();
    while position > 0 && newline_count <= max_lines {
        let read_size = position.min(CHILD_AGENT_TRANSCRIPT_TAIL_CHUNK_SIZE as u64) as usize;
        position = position.saturating_sub(read_size as u64);
        file.seek(SeekFrom::Start(position))?;
        let mut chunk = vec![0; read_size];
        file.read_exact(&mut chunk)?;
        newline_count =
            newline_count.saturating_add(chunk.iter().filter(|byte| **byte == b'\n').count());
        chunks.push(chunk);
    }
    let truncated = position > 0;
    let mut bytes = Vec::new();
    for chunk in chunks.into_iter().rev() {
        bytes.extend(chunk);
    }
    if truncated {
        if let Some(first_newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=first_newline);
        } else {
            bytes.clear();
        }
    }
    Ok((bytes, truncated))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentRenameArgs<'a> {
    target: &'a str,
    display_name: &'a str,
}

struct AgentMessageArgs<'a> {
    target: &'a str,
    prompt: &'a str,
}

fn selectable_agent_indexes(items: &[AgentSidebarItem]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.target.is_some().then_some(index))
        .collect()
}

fn bounded_composer_agent_rows(rows: Vec<SidebarAgentRow>) -> Vec<SidebarAgentRow> {
    if rows.len() <= COMPOSER_AGENT_VISIBLE_ROWS {
        return rows;
    }

    let anchor = rows
        .iter()
        .position(|row| row.selected)
        .or_else(|| rows.iter().position(|row| row.active))
        .unwrap_or(0);
    let start = anchor
        .saturating_add(1)
        .saturating_sub(COMPOSER_AGENT_VISIBLE_ROWS)
        .min(rows.len() - COMPOSER_AGENT_VISIBLE_ROWS);

    rows.into_iter()
        .skip(start)
        .take(COMPOSER_AGENT_VISIBLE_ROWS)
        .collect()
}

pub(super) fn agent_sidebar_child_items_from_projections(
    task_projection: &TaskStateProjection,
    agent_projection: &AgentThreadStateProjection,
    continuation_projection: &AgentResultContinuationProjection,
) -> Vec<AgentSidebarItem> {
    let latest_task = task_projection.latest_task();
    let mut seen = std::collections::BTreeSet::new();
    let mut child_ordinal = 0usize;
    let mut items = Vec::new();
    for thread_id in &agent_projection.thread_replay_order {
        if !seen.insert(thread_id.clone()) {
            continue;
        }
        if let Some(thread) = agent_projection.threads.get(thread_id) {
            if thread.closed || thread.status == AgentThreadStatus::Closed {
                continue;
            }
            child_ordinal += 1;
            let continuation_unresolved = continuation_projection
                .statuses
                .get(thread_id)
                .is_some_and(|status| status.is_unresolved());
            items.push(agent_sidebar_item_from_thread(
                thread,
                latest_task,
                child_ordinal,
                continuation_unresolved,
            ));
        }
    }
    items
}

fn agent_sidebar_item_from_thread(
    thread: &AgentThreadProjection,
    latest_task: Option<&TaskRunProjection>,
    ordinal: usize,
    continuation_unresolved: bool,
) -> AgentSidebarItem {
    let label = agent_thread_display_name(thread, ordinal);
    let detail = agent_thread_sidebar_detail(thread, latest_task, continuation_unresolved);
    let session_ref = thread.thread_session_ref.clone();
    let command_value = thread.thread_id.as_str().to_owned();
    AgentSidebarItem {
        label,
        detail,
        target: session_ref.map(|child_session_ref| AgentView::Child {
            child_task_id: command_value,
            child_session_ref,
        }),
        thread_id: Some(thread.thread_id.clone()),
        muted: thread.thread_session_ref.is_none(),
    }
}

pub(super) fn agent_thread_sidebar_detail(
    thread: &AgentThreadProjection,
    latest_task: Option<&TaskRunProjection>,
    continuation_unresolved: bool,
) -> String {
    let _ = latest_task;
    let status = agent_thread_effective_status(thread, continuation_unresolved);
    agent_thread_detail(thread, status, continuation_unresolved)
}

fn agent_thread_effective_status(
    thread: &AgentThreadProjection,
    continuation_unresolved: bool,
) -> AgentThreadStatus {
    if continuation_unresolved && thread.status == AgentThreadStatus::Failed {
        AgentThreadStatus::Running
    } else {
        thread.status
    }
}

fn agent_thread_display_name(thread: &AgentThreadProjection, ordinal: usize) -> String {
    resolve_agent_display_name(AgentDisplayNameInput {
        display_name: thread.display_name.as_deref(),
        objective: Some(&thread.objective),
        profile_id: thread
            .profile_id
            .as_ref()
            .map(|profile_id| profile_id.as_str()),
        ordinal: Some(ordinal),
        ..AgentDisplayNameInput::default()
    })
    .label
}

fn agent_thread_profile_label(thread: &AgentThreadProjection) -> String {
    thread
        .profile_id
        .as_ref()
        .map(|profile_id| profile_id.as_str().to_owned())
        .unwrap_or_else(|| "agent".to_owned())
}

fn agent_thread_source_label(thread: &AgentThreadProjection) -> &'static str {
    match thread.invocation_source {
        Some(sigil_kernel::AgentInvocationSource::Chat) => "chat",
        Some(sigil_kernel::AgentInvocationSource::Mention) => "mention",
        Some(sigil_kernel::AgentInvocationSource::Skill) => "skill",
        Some(sigil_kernel::AgentInvocationSource::Task) => "task",
        Some(sigil_kernel::AgentInvocationSource::Plugin) => "plugin",
        Some(sigil_kernel::AgentInvocationSource::System) => "system",
        Some(sigil_kernel::AgentInvocationSource::Unknown) | None => "unknown",
    }
}

fn agent_thread_detail(
    thread: &AgentThreadProjection,
    status: AgentThreadStatus,
    continuation_unresolved: bool,
) -> String {
    let mut parts = vec![
        agent_thread_status_label(status).to_owned(),
        agent_thread_profile_label(thread),
        agent_thread_mode_source_label(thread),
    ];
    if let Some(context) = &thread.run_context {
        if !context.model.trim().is_empty() {
            parts.push(context.model.clone());
        }
        if !context.effective_tool_scope_hash.trim().is_empty() {
            parts.push("tools scoped".to_owned());
        }
        if !context.workspace_root.as_str().trim().is_empty() {
            parts.push("workspace inherited".to_owned());
        }
    }
    if let Some(heartbeat) = agent_thread_heartbeat_label(thread) {
        parts.push(heartbeat.to_owned());
    }
    parts.push(agent_thread_result_label(thread, status, continuation_unresolved).to_owned());
    parts.join(" · ")
}

fn agent_thread_mode_source_label(thread: &AgentThreadProjection) -> String {
    let source = agent_thread_source_label(thread);
    match thread.invocation_mode {
        Some(sigil_kernel::AgentInvocationMode::Foreground) => format!("foreground {source}"),
        Some(sigil_kernel::AgentInvocationMode::Background) => format!("background {source}"),
        Some(sigil_kernel::AgentInvocationMode::JoinBeforeFinal) => {
            format!("join-before-final {source}")
        }
        Some(sigil_kernel::AgentInvocationMode::Unknown) | None => source.to_owned(),
    }
}

fn agent_thread_result_label(
    thread: &AgentThreadProjection,
    status: AgentThreadStatus,
    continuation_unresolved: bool,
) -> &'static str {
    if continuation_unresolved || !status.is_terminal() {
        return "result pending";
    }
    if thread.result.is_some() {
        "result ready"
    } else {
        "result missing"
    }
}

fn agent_thread_heartbeat_label(thread: &AgentThreadProjection) -> Option<&'static str> {
    if thread
        .attempts
        .values()
        .any(|attempt| attempt.last_heartbeat_ms.is_some())
    {
        Some("heartbeat seen")
    } else {
        None
    }
}

fn agent_thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Started => "started",
        AgentThreadStatus::Running => "running",
        AgentThreadStatus::Blocked => "blocked",
        AgentThreadStatus::Completed => "completed",
        AgentThreadStatus::Failed => "failed",
        AgentThreadStatus::Cancelled => "cancelled",
        AgentThreadStatus::Interrupted => "interrupted",
        AgentThreadStatus::Closed => "closed",
        AgentThreadStatus::Unavailable => "unavailable",
        AgentThreadStatus::Unknown => "unknown",
    }
}

fn agent_command_value(item: &AgentSidebarItem) -> Option<String> {
    match item.target.as_ref()? {
        AgentView::Main => Some("main".to_owned()),
        AgentView::Child { child_task_id, .. } => Some(child_task_id.clone()),
    }
}

fn agent_display_label(item: &AgentSidebarItem) -> &str {
    &item.label
}

const COMPOSER_AGENT_VISIBLE_ROWS: usize = 4;

fn agent_rename_args(value: &str) -> Option<AgentRenameArgs<'_>> {
    let value = value.trim_start();
    let rest = value
        .strip_prefix("rename")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))?
        .trim_start();
    let (target, display_name) = rest.split_once(char::is_whitespace)?;
    let display_name = display_name.trim();
    if target.is_empty() || display_name.is_empty() {
        return None;
    }
    Some(AgentRenameArgs {
        target,
        display_name,
    })
}

fn agent_message_args(value: &str) -> Option<AgentMessageArgs<'_>> {
    let value = value.trim_start();
    let rest = value
        .strip_prefix("message")
        .or_else(|| value.strip_prefix("steer"))
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))?
        .trim_start();
    let (target, prompt) = rest.split_once(char::is_whitespace)?;
    let prompt = prompt.trim();
    if target.is_empty() || prompt.is_empty() {
        return None;
    }
    Some(AgentMessageArgs { target, prompt })
}

fn agent_rename_prefix(value: &str) -> bool {
    let value = value.trim_start();
    value
        .strip_prefix("rename")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn agent_close_prefix(value: &str) -> bool {
    agent_action_prefix(value, "close")
}

fn agent_cancel_prefix(value: &str) -> bool {
    agent_action_prefix(value, "cancel")
}

fn agent_message_prefix(value: &str) -> bool {
    agent_action_prefix(value, "message") || agent_action_prefix(value, "steer")
}

fn agent_action_prefix(value: &str, command: &str) -> bool {
    let value = value.trim_start();
    value
        .strip_prefix(command)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn agent_action_target<'a>(value: &'a str, command: &str) -> Option<&'a str> {
    let value = value.trim_start();
    let rest = value
        .strip_prefix(command)
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))?
        .trim_start();
    (!rest.is_empty()).then_some(rest)
}

fn agent_rename_selector_query(value: &str) -> Option<&str> {
    let value = value.trim_start();
    let rest = value
        .strip_prefix("rename")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))?
        .trim_start();
    if rest.split_once(char::is_whitespace).is_some() {
        return None;
    }
    Some(rest)
}

fn agent_rename_is_entering_display_name(value: &str) -> bool {
    let value = value.trim_start();
    let Some(rest) = value
        .strip_prefix("rename")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    else {
        return false;
    };
    rest.trim_start().split_once(char::is_whitespace).is_some()
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/agent_flow_detail_tests.rs"]
mod detail_tests;

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/agent_flow_unit_tests.rs"]
mod tests;
