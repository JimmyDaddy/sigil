use std::{collections::BTreeMap, ops::Range};

use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, McpServerConfig, McpServerStartup, RootConfig,
};

use crate::approval::ApprovalDiagnosticSummary;

use super::formatting::summarize_error;

const USD_TO_CNY: f64 = 7.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineTextSelection {
    pub anchor: usize,
    pub cursor: usize,
    pub anchor_column: Option<usize>,
    pub cursor_column: Option<usize>,
}

impl TimelineTextSelection {
    pub(crate) fn line(anchor: usize, cursor: usize) -> Self {
        Self {
            anchor,
            cursor,
            anchor_column: None,
            cursor_column: None,
        }
    }

    pub(crate) fn column(
        anchor: usize,
        anchor_column: usize,
        cursor: usize,
        cursor_column: usize,
    ) -> Self {
        Self {
            anchor,
            cursor,
            anchor_column: Some(anchor_column),
            cursor_column: Some(cursor_column),
        }
    }

    pub(crate) fn normalized_range(self) -> Range<usize> {
        let start = self.anchor.min(self.cursor);
        let end = self.anchor.max(self.cursor).saturating_add(1);
        start..end
    }

    pub(crate) fn normalized_column_bounds(self) -> Option<(usize, usize, usize, usize)> {
        let anchor_column = self.anchor_column?;
        let cursor_column = self.cursor_column?;
        if (self.anchor, anchor_column) <= (self.cursor, cursor_column) {
            Some((self.anchor, anchor_column, self.cursor, cursor_column))
        } else {
            Some((self.cursor, cursor_column, self.anchor, anchor_column))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UsageCostCurrency {
    Usd,
    Cny,
}

impl UsageCostCurrency {
    pub(super) fn from_code(code: Option<&str>) -> Self {
        match code {
            Some(code) if code.eq_ignore_ascii_case("CNY") => Self::Cny,
            _ => Self::Usd,
        }
    }

    pub(super) fn format_cost(self, usd_value: f64) -> String {
        match self {
            Self::Usd => format!("USD {usd_value:.4}"),
            Self::Cny => format!("CNY {:.4}", usd_value * USD_TO_CNY),
        }
    }
}

pub(crate) fn code_intelligence_config_status(config: &CodeIntelligenceConfig) -> String {
    if !config.enabled || config.startup == CodeIntelStartup::Off {
        "off".to_owned()
    } else {
        config.startup.as_str().to_owned()
    }
}

pub(crate) fn diagnostic_summary_label(summary: ApprovalDiagnosticSummary) -> String {
    if summary.is_clean() {
        return "clean".to_owned();
    }
    let mut parts = Vec::new();
    if summary.errors > 0 {
        parts.push(count_label(summary.errors, "error", "errors"));
    }
    if summary.warnings > 0 {
        parts.push(count_label(summary.warnings, "warning", "warnings"));
    }
    parts.join(" ")
}

pub(crate) fn count_label(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpServerRuntimeStatus {
    Deferred,
    Activating,
    Refreshing,
    Stale { capability: String },
    Ready { tool_count: Option<usize> },
    Failed { message: String },
}

impl McpServerRuntimeStatus {
    pub(super) fn label(&self) -> String {
        match self {
            Self::Deferred => "deferred".to_owned(),
            Self::Activating => "activating".to_owned(),
            Self::Refreshing => "refreshing".to_owned(),
            Self::Stale { capability } => format!("stale {capability}"),
            Self::Ready { tool_count: None } => "ready".to_owned(),
            Self::Ready {
                tool_count: Some(count),
            } => format!("ready {}", count_label(*count, "tool", "tools")),
            Self::Failed { message } => format!("failed: {}", summarize_error(message)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpProgressState {
    pub(super) server_name: String,
    pub(super) detail: String,
}

pub(crate) fn initial_mcp_server_statuses(
    root_config: &RootConfig,
) -> BTreeMap<String, McpServerRuntimeStatus> {
    root_config
        .mcp_servers
        .iter()
        .map(|server| (server.name.clone(), initial_mcp_server_status(server)))
        .collect()
}

pub(crate) fn initial_mcp_server_status(server: &McpServerConfig) -> McpServerRuntimeStatus {
    match server.startup {
        McpServerStartup::Eager => McpServerRuntimeStatus::Activating,
        McpServerStartup::Lazy => McpServerRuntimeStatus::Deferred,
    }
}
