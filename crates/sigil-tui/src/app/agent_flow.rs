use std::path::Path;

use crate::{
    slash::{ResolvedSlashCommand, SlashSelectorEntry},
    timeline::{SidebarAgentRow, TimelineRole, agent_status_symbol, compact_agent_detail},
};
use sigil_kernel::{
    AgentRole, AgentThreadClosedEntry, AgentThreadDisplayNameEntry, AgentThreadId,
    AgentThreadProjection, AgentThreadStatus, ControlEntry, JsonlSessionStore,
    TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskRunProjection,
    normalize_task_agent_display_name,
};

use super::{ActiveAgentChildTranscript, AgentSidebarItem, AgentView, AppState};

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
        self.agent_sidebar_rows()
            .into_iter()
            .filter(|row| !row.muted)
            .collect()
    }

    pub(crate) fn composer_agent_panel_rows(&self) -> u16 {
        let rows = self.composer_agent_rows();
        if rows.len() <= 1 {
            return 0;
        }
        1 + rows.len().min(COMPOSER_AGENT_VISIBLE_ROWS) as u16
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
        !agent_rename_is_entering_display_name(arg)
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

    pub(super) fn activate_agent_from_command(&mut self, arg: &str) -> anyhow::Result<()> {
        let value = arg.trim();
        if value.is_empty() {
            self.last_notice =
                Some("usage: /agent <main|next|prev|child-id|rename target name>".to_owned());
            return Ok(());
        }
        if agent_rename_prefix(value) {
            if let Some(rename_args) = agent_rename_args(value) {
                return self.rename_agent_from_command(rename_args);
            }
            self.last_notice = Some("usage: /agent rename <child-id|current> <name>".to_owned());
            return Ok(());
        }
        if agent_close_prefix(value) {
            if let Some(target) = agent_action_target(value, "close") {
                return self.close_agent_from_command(target);
            }
            self.last_notice = Some("usage: /agent close <agent|current>".to_owned());
            return Ok(());
        }
        if agent_cancel_prefix(value) {
            if let Some(target) = agent_action_target(value, "cancel") {
                return self.cancel_agent_from_command(target);
            }
            self.last_notice = Some("usage: /agent cancel <agent|current>".to_owned());
            return Ok(());
        }
        if agent_message_prefix(value) {
            self.last_notice = Some(
                "agent messaging will be enabled with message_agent in the next phase".to_owned(),
            );
            return Ok(());
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
        Ok(())
    }

    fn close_agent_from_command(&mut self, target: &str) -> anyhow::Result<()> {
        let Some(view) = self.agent_view_for_action_target(target) else {
            self.last_notice = Some(format!("agent not found: {target}"));
            return Ok(());
        };
        let Some(thread) = self.agent_thread_projection_for_view(&view) else {
            self.last_notice = Some(format!("agent close unavailable: {target}"));
            return Ok(());
        };
        let thread_id = thread.thread_id.clone();
        if !thread.status.is_terminal() {
            self.last_notice = Some(format!(
                "agent close unavailable until terminal: {}",
                thread_id.as_str()
            ));
            self.push_event("agent:close-unavailable", thread_id.as_str());
            return Ok(());
        }
        self.append_control_to_current_session(ControlEntry::AgentThreadClosed(
            AgentThreadClosedEntry {
                thread_id: thread_id.clone(),
                reason: Some("closed from TUI /agent".to_owned()),
            },
        ))?;
        if self.active_agent_view == view {
            self.active_agent_view = AgentView::Main;
            self.active_agent_child_transcript = None;
        }
        self.last_notice = Some(format!("agent closed: {}", thread_id.as_str()));
        self.push_event("agent:close", thread_id.as_str());
        Ok(())
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
            self.reload_active_agent_child_transcript();
        } else {
            self.active_agent_view = AgentView::Main;
            self.active_agent_child_transcript = None;
        }
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

    fn reload_active_agent_child_transcript(&mut self) {
        let AgentView::Child {
            child_session_ref, ..
        } = &self.active_agent_view
        else {
            self.active_agent_child_transcript = None;
            return;
        };
        let parent_dir = self
            .session_log_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let path = child_session_ref.resolve(parent_dir);
        let load_result = JsonlSessionStore::read_entries(&path);
        self.active_agent_child_transcript = Some(match load_result {
            Ok(entries) => ActiveAgentChildTranscript {
                path,
                entries,
                load_error: None,
            },
            Err(error) => ActiveAgentChildTranscript {
                path,
                entries: Vec::new(),
                load_error: Some(error.to_string()),
            },
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentRenameArgs<'a> {
    target: &'a str,
    display_name: &'a str,
}

fn selectable_agent_indexes(items: &[AgentSidebarItem]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.target.is_some().then_some(index))
        .collect()
}

fn agent_sidebar_item_from_thread(
    thread: &AgentThreadProjection,
    latest_task: Option<&TaskRunProjection>,
    ordinal: usize,
) -> AgentSidebarItem {
    let legacy_child = latest_task.and_then(|task| legacy_child_for_thread(task, thread));
    let label = thread.display_name.clone().unwrap_or_else(|| {
        if let (Some(task), Some(child)) = (latest_task, legacy_child.as_ref()) {
            task_child_agent_display_name(task, child, ordinal)
        } else {
            fallback_agent_thread_display_name(thread, ordinal)
        }
    });
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

fn fallback_agent_thread_display_name(thread: &AgentThreadProjection, ordinal: usize) -> String {
    if !thread.objective.trim().is_empty()
        && let Ok(display_name) = normalize_task_agent_display_name(&thread.objective)
    {
        return display_name;
    }
    thread
        .profile_id
        .as_ref()
        .map(|profile_id| profile_id.as_str().replace(['_', '-'], " "))
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| format!("agent {ordinal}"))
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
    if let Some(display_name) = task.display_name_for_child_session(child) {
        return display_name.to_owned();
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
    fallback_agent_display_name(child.role, ordinal)
}

fn fallback_agent_display_name(role: AgentRole, ordinal: usize) -> String {
    match role {
        AgentRole::SubagentRead => format!("read {ordinal}"),
        AgentRole::SubagentWrite => format!("write {ordinal}"),
        AgentRole::Planner => format!("plan {ordinal}"),
        AgentRole::Executor => format!("agent {ordinal}"),
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

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
