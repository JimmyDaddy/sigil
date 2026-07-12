use std::collections::BTreeMap;

use super::summarize_error;
use crate::{
    app::{AppState, ApprovalDiagnosticSummary, McpProgressState, McpServerRuntimeStatus},
    runner::{McpActivationStatus, WorkerCommand},
};
use sigil_kernel::ToolResult;
use sigil_runtime::{
    BalanceSnapshot, McpListChangedNotification, McpProgressNotification,
    provider_balance_status_config,
};

impl AppState {
    pub(in crate::app) fn schedule_balance_refresh(&mut self) {
        if self.runtime.active_balance_refresh_id.is_some() || self.is_setup_mode() {
            return;
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.runtime.balance_snapshot.status = "n/a".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        let provider_config = match provider_balance_status_config(root_config) {
            Ok(Some(provider_config)) => provider_config,
            Ok(None) => {
                self.runtime.balance_snapshot.available = false;
                self.runtime.balance_snapshot.status = "n/a".to_owned();
                self.refresh_usage_sidebar_cache();
                return;
            }
            Err(_) => {
                self.runtime.balance_snapshot.status = "balance unavailable".to_owned();
                self.refresh_usage_sidebar_cache();
                return;
            }
        };
        if provider_config.api_key.is_none() {
            self.runtime.balance_snapshot.available = false;
            self.runtime.balance_snapshot.status = "missing auth".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        }

        self.runtime.balance_snapshot.status = "loading".to_owned();
        self.refresh_usage_sidebar_cache();
        let request_id = self.next_background_request_id();
        self.runtime.active_balance_refresh_id = Some(request_id);
        self.enqueue_worker_command(WorkerCommand::RefreshProviderBalance {
            request_id,
            provider_config,
        });
    }

    pub(in crate::app) fn apply_provider_balance_refresh(
        &mut self,
        request_id: u64,
        snapshot: BalanceSnapshot,
    ) -> bool {
        if self.runtime.active_balance_refresh_id != Some(request_id) {
            return false;
        }
        self.runtime.active_balance_refresh_id = None;
        self.runtime.balance_snapshot = snapshot.clone();
        self.push_event("balance", snapshot.status);
        self.refresh_usage_sidebar_cache();
        true
    }

    pub(in crate::app) fn apply_mcp_activation_status(
        &mut self,
        server_name: Option<String>,
        status: McpActivationStatus,
    ) {
        let finishes_disclosure_operation = matches!(
            &status,
            McpActivationStatus::Ready { .. } | McpActivationStatus::Failed { .. }
        );
        let Some(server_name) = server_name else {
            self.push_event("mcp", mcp_activation_event_detail(None, &status));
            if finishes_disclosure_operation {
                self.clear_recent_egress_disclosure();
            }
            return;
        };
        let runtime_status = match &status {
            McpActivationStatus::Activating => McpServerRuntimeStatus::Activating,
            McpActivationStatus::Refreshing => McpServerRuntimeStatus::Refreshing,
            McpActivationStatus::Deferred => McpServerRuntimeStatus::Deferred,
            McpActivationStatus::Stale { capability } => McpServerRuntimeStatus::Stale {
                capability: capability.clone(),
            },
            McpActivationStatus::Ready {
                added_tools,
                process_coverage,
            } => McpServerRuntimeStatus::Ready {
                tool_count: Some(*added_tools),
                process_coverage: process_coverage.clone(),
            },
            McpActivationStatus::Failed { error } => McpServerRuntimeStatus::Failed {
                message: error.clone(),
            },
        };
        self.runtime
            .mcp_server_statuses
            .insert(server_name.clone(), runtime_status);
        self.push_event(
            "mcp",
            mcp_activation_event_detail(Some(&server_name), &status),
        );
        if finishes_disclosure_operation {
            self.clear_recent_egress_disclosure();
        }
    }

    pub(in crate::app) fn apply_mcp_progress(&mut self, notification: McpProgressNotification) {
        self.runtime.mcp_progress = Some(McpProgressState {
            server_name: notification.server_name.clone(),
            detail: mcp_progress_detail(&notification),
        });
    }

    pub(in crate::app) fn apply_mcp_list_changed(
        &mut self,
        notification: McpListChangedNotification,
    ) {
        let server_name = notification.server_name.clone();
        let capability = notification.kind.as_str().to_owned();
        self.apply_mcp_activation_status(
            Some(server_name.clone()),
            McpActivationStatus::Stale {
                capability: capability.clone(),
            },
        );
        self.last_notice = Some(format!(
            "MCP {server_name} {capability} changed; refresh queued"
        ));
    }

    pub(in crate::app) fn apply_code_intelligence_tool_status(&mut self, result: &ToolResult) {
        if !result.tool_name.starts_with("code_") {
            return;
        }
        let updated_server_lines = if let Some(lines) = code_intelligence_server_lines(result) {
            for (key, line) in lines {
                self.runtime
                    .code_intelligence_server_lines
                    .insert(key, line);
            }
            true
        } else {
            false
        };
        if let Some(status_line) = code_diagnostics_status_line(result) {
            self.runtime.code_intelligence_status = status_line;
            self.runtime.code_intelligence_diagnostics_line = Some(code_diagnostics_sidebar_line(
                &self.runtime.code_intelligence_status,
            ));
            if let Some(summaries) = code_diagnostics_by_path(result) {
                self.runtime.code_intelligence_diagnostics_by_path = summaries;
            }
        } else if let Some(status_line) = result
            .metadata
            .details
            .get("code_intelligence")
            .and_then(|details| details.get("status_line"))
            .and_then(serde_json::Value::as_str)
        {
            self.runtime.code_intelligence_status = status_line.to_owned();
            if result.is_error() && !updated_server_lines {
                self.runtime.code_intelligence_server_lines.insert(
                    "status".to_owned(),
                    format!("status: {}", self.runtime.code_intelligence_status),
                );
            }
        } else if result.is_error() {
            self.runtime.code_intelligence_status = "degraded tool error".to_owned();
            if !updated_server_lines {
                self.runtime.code_intelligence_server_lines.insert(
                    "status".to_owned(),
                    format!("status: {}", self.runtime.code_intelligence_status),
                );
            }
        } else {
            self.runtime.code_intelligence_status = "ready".to_owned();
        }
        self.push_event(
            "code_intelligence",
            self.runtime.code_intelligence_status.clone(),
        );
    }

    pub(in crate::app) fn apply_mcp_activation_tool_status(&mut self, result: &ToolResult) {
        if result.tool_name != "mcp_activate_server" || result.is_error() {
            return;
        }
        let Ok(content) = serde_json::from_str::<serde_json::Value>(&result.content) else {
            return;
        };
        let Some(server_name) = content
            .get("server_name")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let status = content
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ready");
        if status != "ready" && status != "already_ready" {
            return;
        }
        let added_tools = content
            .get("added_tools")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let process_coverage = content
            .get("process_coverage")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        self.apply_mcp_activation_status(
            Some(server_name.to_owned()),
            McpActivationStatus::Ready {
                added_tools,
                process_coverage,
            },
        );
    }
}

pub(super) fn mcp_activation_event_detail(
    server_name: Option<&str>,
    status: &McpActivationStatus,
) -> String {
    let scope = server_name
        .map(|name| format!("server={name} "))
        .unwrap_or_default();
    let status = match status {
        McpActivationStatus::Activating => "activating".to_owned(),
        McpActivationStatus::Refreshing => "refreshing".to_owned(),
        McpActivationStatus::Deferred => "deferred".to_owned(),
        McpActivationStatus::Stale { capability } => format!("stale {capability}"),
        McpActivationStatus::Ready {
            added_tools,
            process_coverage,
        } => process_coverage
            .as_deref()
            .map(|coverage| format!("ready tools={added_tools} coverage={coverage}"))
            .unwrap_or_else(|| format!("ready tools={added_tools}")),
        McpActivationStatus::Failed { error } => format!("failed {}", summarize_error(error)),
    };
    format!("{scope}{status}")
}

fn mcp_progress_detail(notification: &McpProgressNotification) -> String {
    let message = notification
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("working");
    match (notification.progress, notification.total) {
        (Some(progress), Some(total)) if total > 0.0 => format!(
            "{}: {} {:.0}%",
            notification.server_name,
            message,
            (progress / total * 100.0).clamp(0.0, 100.0)
        ),
        (Some(progress), _) => format!("{}: {} {:.0}", notification.server_name, message, progress),
        _ => format!("{}: {}", notification.server_name, message),
    }
}

pub(super) fn code_intelligence_server_lines(result: &ToolResult) -> Option<Vec<(String, String)>> {
    let servers = result
        .metadata
        .details
        .get("code_intelligence")
        .and_then(|details| details.get("servers"))
        .and_then(serde_json::Value::as_array)?;
    let mut lines = Vec::new();
    for server in servers {
        let server_name = server
            .get("server")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("server");
        let status = server
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ready");
        let languages = server
            .get("languages")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|language| !language.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let label = if languages.is_empty() {
            server_name.to_owned()
        } else {
            languages.join("/")
        };
        let line = match status {
            "ready" => format!("{label}: ready {server_name}"),
            "fallback" => format!("{label}: fallback {server_name}"),
            "installed" => format!("{label}: installed {server_name}"),
            "missing" => format!("{label}: missing {server_name}"),
            "configured" => format!("{label}: configured {server_name}"),
            "disabled" => format!("{label}: disabled {server_name}"),
            other => format!("{label}: {other}"),
        };
        lines.push((server_name.to_owned(), line));
    }
    Some(lines)
}

pub(super) fn code_diagnostics_status_line(result: &ToolResult) -> Option<String> {
    if result.tool_name != "code_diagnostics" || result.is_error() {
        return None;
    }
    let content = serde_json::from_str::<serde_json::Value>(&result.content).ok()?;
    let diagnostics = content
        .get("diagnostics")
        .or_else(|| content.get("results"))?
        .as_array()?;
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic
                .get("severity")
                .and_then(serde_json::Value::as_str)
                == Some("error")
        })
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic
                .get("severity")
                .and_then(serde_json::Value::as_str)
                == Some("warning")
        })
        .count();
    if errors == 0 && warnings == 0 {
        Some("diagnostics clean".to_owned())
    } else {
        Some(format!("diagnostics {errors} errors {warnings} warnings"))
    }
}

pub(super) fn code_diagnostics_by_path(
    result: &ToolResult,
) -> Option<BTreeMap<String, ApprovalDiagnosticSummary>> {
    if result.tool_name != "code_diagnostics" || result.is_error() {
        return None;
    }
    let content = serde_json::from_str::<serde_json::Value>(&result.content).ok()?;
    let mut summaries = BTreeMap::<String, ApprovalDiagnosticSummary>::new();
    if let Some(paths) = content
        .get("query")
        .and_then(|query| query.get("paths"))
        .and_then(serde_json::Value::as_array)
    {
        for path in paths.iter().filter_map(serde_json::Value::as_str) {
            summaries
                .entry(normalize_diagnostic_path(path))
                .or_default();
        }
    }

    let diagnostics = content
        .get("diagnostics")
        .or_else(|| content.get("results"))?
        .as_array()?;
    for diagnostic in diagnostics {
        let Some(path) = diagnostic
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(normalize_diagnostic_path)
        else {
            continue;
        };
        let summary = summaries.entry(path).or_default();
        match diagnostic
            .get("severity")
            .and_then(serde_json::Value::as_str)
        {
            Some("error") => summary.errors += 1,
            Some("warning") => summary.warnings += 1,
            _ => {}
        }
    }
    Some(summaries)
}

pub(super) fn normalize_diagnostic_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

pub(super) fn code_diagnostics_sidebar_line(status_line: &str) -> String {
    if status_line == "diagnostics clean" {
        return "diagnostics: clean".to_owned();
    }
    status_line
        .strip_prefix("diagnostics ")
        .map(|summary| format!("diagnostics: {summary}"))
        .unwrap_or_else(|| format!("diagnostics: {status_line}"))
}
