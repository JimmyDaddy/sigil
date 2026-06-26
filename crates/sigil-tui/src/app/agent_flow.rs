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
    AgentThreadDisplayNameEntry, AgentThreadId, AgentThreadProjection, AgentThreadStatus,
    ControlEntry, JsonlSessionStore, SessionLogEntry, TaskChildSessionDisplayNameEntry,
    TaskChildSessionEntry, TaskRunProjection, normalize_task_agent_display_name,
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
        match &self.active_agent_view {
            AgentView::Main => "main".to_owned(),
            AgentView::Child { child_task_id, .. } => self
                .agent_sidebar_items()
                .into_iter()
                .find(|item| {
                    item.target
                        .as_ref()
                        .is_some_and(|target| target == &self.active_agent_view)
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
                    .is_some_and(|target| target == &self.active_agent_view),
                label: item.label,
                detail: item.detail,
                selected: index == self.sidebar_agent_selected,
                muted: item.muted,
            })
            .collect()
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
            + u16::from(self.composer_agent_panel_focused)
    }

    pub(crate) fn is_composer_agent_panel_focused(&self) -> bool {
        self.composer_agent_panel_focused
    }

    pub(super) fn focus_composer_agent_panel(&mut self) -> bool {
        if !self.composer_agent_panel_available() {
            self.composer_agent_panel_focused = false;
            return false;
        }
        self.select_active_agent_sidebar_item();
        self.composer_agent_panel_focused = true;
        self.last_notice = Some("agent list focused".to_owned());
        true
    }

    pub(super) fn blur_composer_agent_panel(&mut self) {
        self.composer_agent_panel_focused = false;
    }

    pub(super) fn selected_composer_agent_is_first(&self) -> bool {
        let items = self.agent_sidebar_items();
        selectable_agent_indexes(&items)
            .first()
            .is_some_and(|index| *index == self.sidebar_agent_selected)
    }

    pub(super) fn move_composer_agent_selection(&mut self, next: bool) -> bool {
        let items = self.agent_sidebar_items();
        let selectable = selectable_agent_indexes(&items);
        if selectable.is_empty() {
            self.composer_agent_panel_focused = false;
            return false;
        }
        let current = selectable
            .iter()
            .position(|index| *index == self.sidebar_agent_selected)
            .or_else(|| {
                selectable.iter().position(|index| {
                    items[*index]
                        .target
                        .as_ref()
                        .is_some_and(|target| target == &self.active_agent_view)
                })
            })
            .unwrap_or(0);
        let next_position = if next {
            (current + 1) % selectable.len()
        } else {
            current.saturating_sub(1)
        };
        self.sidebar_agent_selected = selectable[next_position];
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
                .is_some_and(|target| target == &self.active_agent_view);
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
                self.cancel_agent_from_command(target)?;
                return Ok(None);
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
            .agent_thread_id_for_view(&self.active_agent_view)
            .as_ref()
            .is_some_and(|active_thread_id| active_thread_id == &thread_id);
        self.sync_current_session_state(entries);
        if closing_active {
            self.active_agent_view = AgentView::Main;
            self.active_agent_child_transcript = None;
        }
        self.last_notice = Some(format!("agent closed: {}", thread_id.as_str()));
        self.push_event("agent:close", thread_id.as_str());
    }

    fn cancel_agent_from_command(&mut self, target: &str) -> anyhow::Result<()> {
        let Some(view) = self.agent_view_for_action_target(target) else {
            self.last_notice = Some(format!("agent not found: {target}"));
            return Ok(());
        };
        let Some(thread_id) = self.agent_thread_id_for_view(&view) else {
            self.last_notice = Some(format!("agent cancel unavailable: {target}"));
            return Ok(());
        };
        self.last_notice = Some(format!(
            "agent cancel unavailable until runtime support: {}",
            thread_id.as_str()
        ));
        self.push_event("agent:cancel-unavailable", thread_id.as_str());
        Ok(())
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
                    .is_some_and(|target| target == &self.active_agent_view)
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
        if !self.activate_agent_view_at_index(self.sidebar_agent_selected) {
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
        if self.sidebar_agent_selected >= items.len() {
            self.sidebar_agent_selected = items.len().saturating_sub(1);
        }
        if !self.composer_agent_panel_available() {
            self.composer_agent_panel_focused = false;
        }
        if self.active_agent_view == AgentView::Main {
            self.active_agent_child_transcript = None;
            return;
        }
        let still_available = items
            .iter()
            .filter_map(|item| item.target.as_ref())
            .any(|target| target == &self.active_agent_view);
        if still_available {
            if self.active_agent_child_transcript.is_none() || self.active_agent_view_is_terminal()
            {
                self.reload_active_agent_child_transcript();
            }
        } else {
            self.active_agent_view = AgentView::Main;
            self.active_agent_child_transcript = None;
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
        } = &self.active_agent_view
        else {
            return None;
        };
        sigil_kernel::TaskStateProjection::from_entries(&self.current_session_entries)
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
        self.sidebar_agent_selected = index;
        self.active_agent_view = target;
        if self.active_pane == super::PaneFocus::Composer && self.composer_agent_panel_available() {
            self.composer_agent_panel_focused = true;
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
            detail: if self.is_busy {
                "running in current session".to_owned()
            } else {
                "idle in current session".to_owned()
            },
            target: Some(AgentView::Main),
            thread_id: None,
            muted: false,
        }];
        let task_projection =
            sigil_kernel::TaskStateProjection::from_entries(&self.current_session_entries);
        let agent_projection =
            sigil_kernel::AgentThreadStateProjection::from_entries(&self.current_session_entries);
        let latest_task = task_projection.latest_task();
        let mut seen = std::collections::BTreeSet::new();
        let mut child_ordinal = 0usize;
        for thread_id in &agent_projection.thread_replay_order {
            if !seen.insert(thread_id.clone()) {
                continue;
            }
            if let Some(thread) = agent_projection.threads.get(thread_id) {
                if thread.closed || thread.status == AgentThreadStatus::Closed {
                    continue;
                }
                child_ordinal += 1;
                items.push(agent_sidebar_item_from_thread(
                    thread,
                    latest_task,
                    child_ordinal,
                ));
            }
        }
        if items.len() == 1 {
            items.push(AgentSidebarItem {
                label: "agents".to_owned(),
                detail: "no child agents recorded".to_owned(),
                target: None,
                thread_id: None,
                muted: true,
            });
        }
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
        if let Some(child) = self.child_session_for_agent_view(&target) {
            let entry = TaskChildSessionDisplayNameEntry {
                task_id: child.task_id.clone(),
                plan_version: child.plan_version,
                step_id: child.step_id.clone(),
                child_task_id: child.child_task_id.clone(),
                display_name: display_name.clone(),
            };
            self.append_control_to_current_session(ControlEntry::TaskChildSessionDisplayName(
                entry,
            ))?;
            self.last_notice = Some(format!(
                "agent renamed: {} -> {display_name}",
                child.child_task_id.as_str()
            ));
            self.push_event(
                "agent:rename",
                format!("{} -> {display_name}", child.child_task_id.as_str()),
            );
            return Ok(());
        }

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
            return match &self.active_agent_view {
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

    fn child_session_for_agent_view(&self, view: &AgentView) -> Option<TaskChildSessionEntry> {
        let AgentView::Child {
            child_task_id,
            child_session_ref,
        } = view
        else {
            return None;
        };
        sigil_kernel::TaskStateProjection::from_entries(&self.current_session_entries)
            .latest_task()
            .and_then(|task| {
                task.child_sessions.values().find(|child| {
                    child.child_task_id.as_str() == child_task_id
                        && child.child_session_ref == *child_session_ref
                })
            })
            .cloned()
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
        let projection =
            sigil_kernel::AgentThreadStateProjection::from_entries(&self.current_session_entries);
        projection.threads.get(thread_id).cloned()
    }

    pub(crate) fn active_agent_thread_projection(&self) -> Option<AgentThreadProjection> {
        self.agent_thread_projection_for_view(&self.active_agent_view)
    }

    fn agent_thread_projection_for_view(&self, view: &AgentView) -> Option<AgentThreadProjection> {
        let thread_id = self.agent_thread_id_for_view(view)?;
        self.agent_thread_projection_for_id(&thread_id)
    }

    fn composer_agent_panel_available(&self) -> bool {
        self.composer_agent_rows().len() > 1
    }

    fn select_active_agent_sidebar_item(&mut self) {
        if let Some(index) = self.agent_sidebar_items().iter().position(|item| {
            item.target
                .as_ref()
                .is_some_and(|target| target == &self.active_agent_view)
        }) {
            self.sidebar_agent_selected = index;
        }
    }

    fn selected_agent_command_value(&self) -> Option<String> {
        self.agent_sidebar_items()
            .get(self.sidebar_agent_selected)
            .and_then(agent_command_value)
    }

    pub(super) fn reload_active_agent_child_transcript(&mut self) -> bool {
        let AgentView::Child {
            child_session_ref, ..
        } = &self.active_agent_view
        else {
            let changed = self.active_agent_child_transcript.is_some();
            self.active_agent_child_transcript = None;
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
                let changed =
                    !self
                        .active_agent_child_transcript
                        .as_ref()
                        .is_some_and(|transcript| {
                            transcript.path == path
                                && transcript.file_signature
                                    == ChildTranscriptFileSignature::empty()
                                && transcript.load_error.as_deref() == Some(error.as_str())
                                && transcript.timeline_entries.is_empty()
                                && transcript.rendered_body_lines.is_empty()
                        });
                self.active_agent_child_transcript = Some(ActiveAgentChildTranscript {
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
            .active_agent_child_transcript
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
        self.active_agent_child_transcript = Some(match load_result {
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
            .active_agent_child_transcript
            .as_ref()
            .map(|transcript| transcript.timeline_entries.clone())
        else {
            return;
        };
        let rendered_body_lines = self.render_child_timeline_body_lines(&timeline_entries);
        if let Some(transcript) = self.active_agent_child_transcript.as_mut() {
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

    let mut selected_indexes = std::collections::BTreeSet::new();
    selected_indexes.insert(0usize);
    for (index, row) in rows.iter().enumerate() {
        if row.active || row.selected {
            selected_indexes.insert(index);
        }
    }
    for index in (0..rows.len()).rev() {
        if selected_indexes.len() >= COMPOSER_AGENT_VISIBLE_ROWS {
            break;
        }
        selected_indexes.insert(index);
    }

    rows.into_iter()
        .enumerate()
        .filter_map(|(index, row)| selected_indexes.contains(&index).then_some(row))
        .collect()
}

fn agent_sidebar_item_from_thread(
    thread: &AgentThreadProjection,
    latest_task: Option<&TaskRunProjection>,
    ordinal: usize,
) -> AgentSidebarItem {
    let legacy_child = latest_task.and_then(|task| legacy_child_for_thread(task, thread));
    let label = if let (Some(task), Some(child)) = (latest_task, legacy_child.as_ref()) {
        task_child_agent_display_name(task, child, ordinal)
    } else {
        agent_thread_display_name(thread, ordinal)
    };
    let detail = if let Some(child) = legacy_child.as_ref() {
        format!(
            "{} · {} · v{}:{}",
            agent_thread_status_label(thread.status),
            child.role.as_str(),
            child.plan_version,
            child.step_id.as_str()
        )
    } else {
        format!(
            "{} · {} · {}",
            agent_thread_status_label(thread.status),
            agent_thread_profile_label(thread),
            agent_thread_source_label(thread)
        )
    };
    let session_ref = thread.thread_session_ref.clone();
    let command_value = legacy_child
        .as_ref()
        .map(|child| child.child_task_id.as_str().to_owned())
        .unwrap_or_else(|| thread.thread_id.as_str().to_owned());
    AgentSidebarItem {
        label: format!("agent {label}"),
        detail,
        target: session_ref.map(|child_session_ref| AgentView::Child {
            child_task_id: command_value,
            child_session_ref,
        }),
        thread_id: Some(thread.thread_id.clone()),
        muted: thread.thread_session_ref.is_none(),
    }
}

fn legacy_child_for_thread<'a>(
    task: &'a TaskRunProjection,
    thread: &AgentThreadProjection,
) -> Option<&'a TaskChildSessionEntry> {
    if !thread.legacy_task {
        return None;
    }
    let session_ref = thread.thread_session_ref.as_ref()?;
    task.child_sessions
        .values()
        .find(|child| &child.child_session_ref == session_ref)
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
    item.label.strip_prefix("agent ").unwrap_or(&item.label)
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

fn task_child_agent_display_name(
    task: &TaskRunProjection,
    child: &TaskChildSessionEntry,
    ordinal: usize,
) -> String {
    let explicit = task.display_name_for_child_session(child);
    if explicit.is_some() {
        return resolve_agent_display_name(AgentDisplayNameInput {
            display_name: explicit,
            role: Some(child.role),
            ordinal: Some(ordinal),
            ..AgentDisplayNameInput::default()
        })
        .label;
    }
    if let Some(display_name) = task.plans.get(&child.plan_version).and_then(|plan| {
        plan.steps
            .iter()
            .find(|step| step.step_id == child.step_id)
            .and_then(|step| step.display_name.as_deref())
    }) && let Ok(display_name) = normalize_task_agent_display_name(display_name)
    {
        return display_name;
    }
    resolve_agent_display_name(AgentDisplayNameInput {
        display_name: explicit,
        role: Some(child.role),
        ordinal: Some(ordinal),
        ..AgentDisplayNameInput::default()
    })
    .label
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/agent_flow_detail_tests.rs"]
mod detail_tests;

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::Path,
    };

    use super::*;
    use sigil_kernel::{
        AgentConfig, CompactionConfig, MemoryConfig, ModelMessage, PermissionConfig, RootConfig,
        SessionConfig, SkillConfig, WorkspaceConfig,
    };
    use tempfile::tempdir;

    fn test_root_config() -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            storage: Default::default(),
            session: SessionConfig {
                log_dir: Some(".sigil/sessions".to_owned()),
            },
            agent: AgentConfig {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: None,
                tool_timeout_secs: 30,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: true },
            skills: SkillConfig {
                user_skills: false,
                user_agents: false,
                compatibility_sources: Vec::new(),
                ..Default::default()
            },
            compaction: CompactionConfig::default(),
            code_intelligence: Default::default(),
            terminal: Default::default(),
            verification: Default::default(),
            appearance: Default::default(),
            task: Default::default(),
            providers: BTreeMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    fn test_thread(
        thread_id: &str,
        objective: &str,
        profile_id: Option<&str>,
    ) -> anyhow::Result<AgentThreadProjection> {
        Ok(AgentThreadProjection {
            thread_id: AgentThreadId::new(thread_id)?,
            parent_thread_id: None,
            parent_session_ref: None,
            thread_session_ref: Some(sigil_kernel::SessionRef::new_relative(format!(
                "children/{thread_id}.jsonl"
            ))?),
            profile_id: profile_id
                .map(sigil_kernel::AgentProfileId::new)
                .transpose()?,
            profile_snapshot_id: None,
            run_context: None,
            objective: objective.to_owned(),
            prompt_hash: "sha256:prompt".to_owned(),
            invocation_mode: None,
            invocation_source: None,
            display_name: None,
            status: AgentThreadStatus::Started,
            reason: None,
            result: None,
            attempts: BTreeMap::new(),
            merge_safe_points: Vec::new(),
            duplicate_terminal_entries: 0,
            legacy_task: false,
            closed: false,
            unresolved: false,
            profile_snapshot_missing: false,
            profile_snapshot_mismatch: false,
        })
    }

    #[test]
    fn agent_thread_sidebar_item_uses_objective_profile_and_ordinal_fallbacks() -> anyhow::Result<()>
    {
        let objective = test_thread("thread_objective", "Review kernel", Some("reader"))?;
        let from_objective = agent_sidebar_item_from_thread(&objective, None, 1);
        assert_eq!(from_objective.label, "agent Review kernel");
        assert_eq!(from_objective.detail, "started · reader · unknown");

        let profile = test_thread("thread_profile", "   ", Some("reader-agent"))?;
        let from_profile = agent_sidebar_item_from_thread(&profile, None, 2);
        assert_eq!(from_profile.label, "agent reader agent");
        assert_eq!(from_profile.detail, "started · reader-agent · unknown");

        let ordinal = test_thread("thread_ordinal", "   ", None)?;
        let from_ordinal = agent_sidebar_item_from_thread(&ordinal, None, 3);
        assert_eq!(from_ordinal.label, "agent agent 3");
        assert_eq!(from_ordinal.detail, "started · agent · unknown");
        Ok(())
    }

    #[test]
    fn legacy_child_for_thread_ignores_non_legacy_threads() -> anyhow::Result<()> {
        let thread = test_thread("thread_non_legacy", "Review", Some("reader"))?;
        let task = TaskRunProjection {
            task_id: sigil_kernel::TaskId::new("task_1")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "task".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
            latest_plan_version: None,
            plans: BTreeMap::new(),
            steps: BTreeMap::new(),
            current_step: None,
            child_sessions: BTreeMap::new(),
            child_display_names: BTreeMap::new(),
            approval_routes: BTreeMap::new(),
            elicitation_routes: BTreeMap::new(),
            duplicate_terminal_entries: 0,
            superseded_plan_versions: BTreeSet::new(),
            route_unverified: false,
            child_unavailable: false,
        };

        assert!(legacy_child_for_thread(&task, &thread).is_none());
        Ok(())
    }

    #[test]
    fn active_agent_view_terminal_uses_child_session_status() -> anyhow::Result<()> {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_root_config());
        let task_id = sigil_kernel::TaskId::new("task_1")?;
        let step_id = sigil_kernel::TaskStepId::new("step_1")?;
        let child_ref = sigil_kernel::SessionRef::new_relative("children/child.jsonl")?;
        app.sync_current_session_state(vec![
            SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                objective: "review".to_owned(),
                status: sigil_kernel::TaskRunStatus::Running,
                reason: None,
            })),
            SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                status: sigil_kernel::TaskPlanStatus::Accepted,
                steps: vec![sigil_kernel::TaskStepSpec {
                    step_id: step_id.clone(),
                    title: "inspect".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::SubagentRead,
                }],
                reason: None,
            })),
            SessionLogEntry::Control(ControlEntry::TaskChildSession(
                sigil_kernel::TaskChildSessionEntry {
                    task_id,
                    plan_version: 1,
                    step_id,
                    child_task_id: sigil_kernel::TaskId::new("child_1")?,
                    child_session_ref: child_ref.clone(),
                    role: sigil_kernel::AgentRole::SubagentRead,
                    status: sigil_kernel::TaskChildSessionStatus::Failed,
                    summary_hash: None,
                },
            )),
        ]);
        app.active_agent_view = AgentView::Child {
            child_task_id: "child_1".to_owned(),
            child_session_ref: child_ref,
        };

        assert!(app.active_agent_view_is_terminal());
        Ok(())
    }

    #[test]
    fn recent_child_session_entries_cover_empty_valid_and_invalid_files() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let missing = temp.path().join("missing.jsonl");
        let missing_signature = child_transcript_file_signature(&missing)?;
        let missing_recent = read_recent_session_entries(&missing, 16, missing_signature)?;
        assert!(missing_recent.entries.is_empty());
        assert!(!missing_recent.truncated);

        let valid = temp.path().join("valid.jsonl");
        let lines = (0..4)
            .map(|index| {
                serde_json::to_string(&SessionLogEntry::User(ModelMessage::user(format!(
                    "child prompt {index}"
                ))))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        fs::write(&valid, format!("{lines}\n"))?;
        let valid_signature = child_transcript_file_signature(&valid)?;
        let recent = read_recent_session_entries(&valid, 2, valid_signature)?;
        assert_eq!(recent.entries.len(), 2);
        assert!(recent.truncated);

        let invalid_utf8 = temp.path().join("invalid-utf8.jsonl");
        fs::write(&invalid_utf8, [0xff, b'\n'])?;
        let error = read_recent_session_entries(
            &invalid_utf8,
            2,
            child_transcript_file_signature(&invalid_utf8)?,
        )
        .expect_err("invalid utf8 should fail");
        assert!(error.to_string().contains("decode recent entry"));

        let invalid_json = temp.path().join("invalid-json.jsonl");
        fs::write(&invalid_json, "not-json\n")?;
        let error = read_recent_session_entries(
            &invalid_json,
            2,
            child_transcript_file_signature(&invalid_json)?,
        )
        .expect_err("invalid json should fail");
        assert!(error.to_string().contains("parse recent session entry"));
        assert!(recent_session_entry_parse_error(&invalid_json).contains("invalid-json.jsonl"));
        Ok(())
    }

    #[test]
    fn agent_thread_labels_cover_status_and_source_variants() -> anyhow::Result<()> {
        let mut thread = test_thread("thread_labels", "Review", Some("reader"))?;

        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Chat);
        assert_eq!(agent_thread_source_label(&thread), "chat");
        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Mention);
        assert_eq!(agent_thread_source_label(&thread), "mention");
        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Skill);
        assert_eq!(agent_thread_source_label(&thread), "skill");
        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Task);
        assert_eq!(agent_thread_source_label(&thread), "task");
        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Plugin);
        assert_eq!(agent_thread_source_label(&thread), "plugin");
        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::System);
        assert_eq!(agent_thread_source_label(&thread), "system");
        thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Unknown);
        assert_eq!(agent_thread_source_label(&thread), "unknown");
        thread.invocation_source = None;
        assert_eq!(agent_thread_source_label(&thread), "unknown");

        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Started),
            "started"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Running),
            "running"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Blocked),
            "blocked"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Completed),
            "completed"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Failed),
            "failed"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Cancelled),
            "cancelled"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Interrupted),
            "interrupted"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Closed),
            "closed"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Unavailable),
            "unavailable"
        );
        assert_eq!(
            agent_thread_status_label(AgentThreadStatus::Unknown),
            "unknown"
        );
        Ok(())
    }
}
