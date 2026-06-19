use std::path::Path;

use crate::{
    slash::{ResolvedSlashCommand, SlashSelectorEntry},
    timeline::{SidebarAgentRow, TimelineRole, agent_status_symbol, compact_agent_detail},
};
use sigil_kernel::{
    AgentRole, ControlEntry, JsonlSessionStore, TaskChildSessionDisplayNameEntry,
    TaskChildSessionEntry, TaskRunProjection, normalize_task_agent_display_name,
};

use super::{ActiveAgentChildTranscript, AgentSidebarItem, AgentView, AppState, task_sidebar};

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
            muted: false,
        }];
        let projection =
            sigil_kernel::TaskStateProjection::from_entries(&self.current_session_entries);
        let Some(task) = projection.latest_task() else {
            items.push(AgentSidebarItem {
                label: "agents".to_owned(),
                detail: "available via /plan".to_owned(),
                target: None,
                muted: true,
            });
            return items;
        };
        if task.child_sessions.is_empty() {
            items.push(AgentSidebarItem {
                label: "agents".to_owned(),
                detail: "no child sessions recorded".to_owned(),
                target: None,
                muted: true,
            });
            return items;
        }
        items.extend(
            task.child_sessions
                .values()
                .enumerate()
                .map(|(index, child)| {
                    let nickname = task_child_agent_display_name(task, child, index + 1);
                    AgentSidebarItem {
                        label: format!("agent {nickname}"),
                        detail: format!(
                            "{} · {} · v{}:{}",
                            task_sidebar::task_child_session_status_label(child.status),
                            child.role.as_str(),
                            child.plan_version,
                            child.step_id.as_str()
                        ),
                        target: Some(AgentView::Child {
                            child_task_id: child.child_task_id.as_str().to_owned(),
                            child_session_ref: child.child_session_ref.clone(),
                        }),
                        muted: false,
                    }
                }),
        );
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
        let Some(child) = self.child_session_for_agent_rename_target(args.target) else {
            self.last_notice = Some(format!("agent not found: {}", args.target));
            return Ok(());
        };
        let entry = TaskChildSessionDisplayNameEntry {
            task_id: child.task_id.clone(),
            plan_version: child.plan_version,
            step_id: child.step_id.clone(),
            child_task_id: child.child_task_id.clone(),
            display_name: display_name.clone(),
        };
        self.append_control_to_current_session(ControlEntry::TaskChildSessionDisplayName(entry))?;
        self.last_notice = Some(format!(
            "agent renamed: {} -> {display_name}",
            child.child_task_id.as_str()
        ));
        self.push_event(
            "agent:rename",
            format!("{} -> {display_name}", child.child_task_id.as_str()),
        );
        Ok(())
    }

    fn child_session_for_agent_rename_target(&self, target: &str) -> Option<TaskChildSessionEntry> {
        if matches!(target.to_ascii_lowercase().as_str(), "current" | ".") {
            return self.active_agent_child_entry();
        }
        let index = self.agent_sidebar_item_index_by_value(target)?;
        let items = self.agent_sidebar_items();
        let target = items.get(index)?.target.as_ref()?;
        match target {
            AgentView::Main => None,
            AgentView::Child { .. } => self.child_session_for_agent_view(target),
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
