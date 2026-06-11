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
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunPhase {
    Idle,
    Thinking,
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

#[cfg(test)]
#[path = "tests/timeline_tests.rs"]
mod tests;
