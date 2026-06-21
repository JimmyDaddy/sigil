use crate::ui::{FocusKind, StatusKind, focus_symbol, status_kind_from_label, status_symbol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRole {
    System,
    User,
    Assistant,
    Phase,
    Thinking,
    Tool,
    Notice,
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub role: TimelineRole,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct EventEntry {
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolActivityCacheEntry {
    pub index: usize,
    pub key: String,
    pub defaults_expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveActivitySummary {
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub(crate) enum SessionHistoryRow {
    SessionHeader {
        filter: String,
        total: usize,
    },
    SessionItem {
        index: usize,
        label: String,
        current: bool,
        selected: bool,
        meta: String,
    },
    Empty {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarCard {
    Permission,
    Agents,
    Usage,
}

impl SidebarCard {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Agents => "agents",
            Self::Usage => "usage",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Permission => Self::Agents,
            Self::Agents => Self::Usage,
            Self::Usage => Self::Permission,
        }
    }

    pub(crate) fn previous(self) -> Self {
        match self {
            Self::Permission => Self::Usage,
            Self::Agents => Self::Permission,
            Self::Usage => Self::Agents,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SidebarAgentRow {
    pub label: String,
    pub detail: String,
    pub selected: bool,
    pub active: bool,
    pub muted: bool,
}

impl SidebarAgentRow {
    pub(crate) fn focus_symbol(&self, show_selection: bool) -> &'static str {
        let kind = if self.active {
            FocusKind::Current
        } else if show_selection && self.selected {
            FocusKind::Selected
        } else {
            FocusKind::None
        };
        focus_symbol(kind)
    }

    pub(crate) fn status_symbol(&self) -> &'static str {
        status_symbol(self.status_kind())
    }

    pub(crate) fn status_kind(&self) -> StatusKind {
        agent_status_kind(&self.detail)
    }

    pub(crate) fn compact_detail(&self) -> String {
        compact_agent_detail(&self.detail)
    }
}

pub(crate) fn agent_status_symbol(detail: &str) -> &'static str {
    status_symbol(agent_status_kind(detail))
}

pub(crate) fn agent_status_kind(detail: &str) -> StatusKind {
    agent_status_label(detail)
        .map(status_kind_from_label)
        .unwrap_or(StatusKind::Unknown)
}

pub(crate) fn compact_agent_detail(detail: &str) -> String {
    let trimmed = detail.trim();
    match trimmed {
        "idle in current session" | "running in current session" => {
            return "current session".to_owned();
        }
        _ => {}
    }
    trimmed
        .split_once(" · ")
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_else(|| trimmed.to_owned())
}

fn agent_status_label(detail: &str) -> Option<&str> {
    let detail = detail.trim();
    detail
        .split_once(' ')
        .map(|(status, _)| status)
        .or_else(|| detail.split_once(" · ").map(|(status, _)| status))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunPhase {
    Idle,
    Thinking,
    Agent(String),
    Tool(String),
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThinkingBlockMode {
    Collapsed,
    Expanded,
}

impl ThinkingBlockMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Collapsed => "collapsed",
            Self::Expanded => "expanded",
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/timeline_tests.rs"]
mod tests;
