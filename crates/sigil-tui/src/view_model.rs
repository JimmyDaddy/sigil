use std::{collections::BTreeMap, env, path::Path};

use ratatui::text::Line;
use sigil_kernel::{
    AgentMailboxStatus, AgentThreadStateProjection, ContextInclusionReason, ContextItem,
    ContextSource, PackedContext, ResumeJobStateProjection, SessionLogEntry, SourcedFact,
    TaskMemoryV1,
};

use crate::{
    app::{AppState, ComposerQueueAction, PaneFocus},
    commands::{global_control_hints, tool_card_control_hints},
    timeline::{ComposerQueueRow, RunPhase, SidebarAgentRow},
    ui::StatusKind,
};

const INFO_RAIL_AGENT_ROW_LIMIT: usize = 3;
const INFO_RAIL_TASK_LINE_LIMIT: usize = 4;
const INFO_RAIL_CONTROL_LIMIT: usize = 3;

#[derive(Debug, Clone)]
pub(crate) struct UiViewModel {
    pub info_rail: InfoRailViewModel,
    pub composer: ComposerViewModel,
    pub footer: FooterViewModel,
}

impl UiViewModel {
    pub(crate) fn from_app(app: &AppState) -> Self {
        Self {
            info_rail: InfoRailViewModel::from_app(app),
            composer: ComposerViewModel::from_app(app),
            footer: FooterViewModel::from_app(app),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InfoRailViewModel {
    pub session_title: String,
    pub workspace_label: String,
    pub session_lines: Vec<String>,
    pub permission_lines: Vec<String>,
    pub agent_lines: Vec<String>,
    pub mcp_lines: Vec<String>,
    pub code_lines: Vec<String>,
    pub task_lines: Vec<String>,
    pub usage_lines: Vec<String>,
    pub controls: Vec<String>,
}

impl InfoRailViewModel {
    fn from_app(app: &AppState) -> Self {
        let detail = app.info_rail_detail_enabled();
        let controls = info_rail_controls(app, detail);

        Self {
            session_title: app.session_display_title(),
            workspace_label: display_path_label(&app.workspace_root),
            session_lines: if detail {
                detail_info_rail_session_lines(app)
            } else {
                info_rail_session_lines(app)
            },
            permission_lines: if detail {
                app.permission_card_lines()
            } else {
                info_rail_permission_lines(app)
            },
            agent_lines: app
                .agent_graph_summary_line()
                .into_iter()
                .chain(
                    displayed_info_rail_agent_rows(app, detail)
                        .into_iter()
                        .map(|row| {
                            format!(
                                "{} {}: {} {}",
                                row.focus_symbol(true),
                                row.label,
                                row.status_symbol(),
                                row.compact_detail()
                            )
                        }),
                )
                .collect(),
            mcp_lines: app.mcp_sidebar_lines(),
            code_lines: app.code_intelligence_sidebar_lines(),
            task_lines: if detail {
                app.task_sidebar_lines()
            } else {
                compact_info_rail_task_lines(app)
            },
            usage_lines: if detail {
                app.usage_sidebar_lines().to_vec()
            } else {
                compact_info_rail_usage_lines(app)
            },
            controls,
        }
    }
}

fn displayed_info_rail_agent_rows(app: &AppState, detail: bool) -> Vec<SidebarAgentRow> {
    if detail {
        app.agent_sidebar_rows()
    } else {
        info_rail_agent_rows(app)
    }
}

pub(crate) fn info_rail_agent_rows(app: &AppState) -> Vec<SidebarAgentRow> {
    info_rail_agent_row_entries(app)
        .into_iter()
        .map(|(_, row)| row)
        .collect()
}

pub(crate) fn displayed_info_rail_agent_row_entries(
    app: &AppState,
) -> Vec<(usize, SidebarAgentRow)> {
    if app.info_rail_detail_enabled() {
        app.agent_sidebar_rows().into_iter().enumerate().collect()
    } else {
        info_rail_agent_row_entries(app)
    }
}

pub(crate) fn info_rail_agent_row_entries(app: &AppState) -> Vec<(usize, SidebarAgentRow)> {
    let rows = app.agent_sidebar_rows();
    if rows.len() <= INFO_RAIL_AGENT_ROW_LIMIT {
        return rows.into_iter().enumerate().collect();
    }

    let mut selected_indexes = std::collections::BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        if row.active || row.selected {
            selected_indexes.insert(index);
        }
    }
    for (index, row) in rows.iter().enumerate() {
        if selected_indexes.len() >= INFO_RAIL_AGENT_ROW_LIMIT {
            break;
        }
        if !row.muted {
            selected_indexes.insert(index);
        }
    }
    for index in (0..rows.len()).rev() {
        if selected_indexes.len() >= INFO_RAIL_AGENT_ROW_LIMIT {
            break;
        }
        selected_indexes.insert(index);
    }

    rows.into_iter()
        .enumerate()
        .filter_map(|(index, row)| selected_indexes.contains(&index).then_some((index, row)))
        .collect()
}

pub(crate) fn info_rail_session_lines(app: &AppState) -> Vec<String> {
    let mut lines = vec![
        format!(
            "model: {} · {}",
            app.runtime.model_name,
            app.reasoning_effort_label()
        ),
        format!(
            "state: {} · {}",
            app.run_phase_label(),
            short_session_label(&app.session_id)
        ),
    ];
    if app.runtime.memory_enabled {
        lines.push(format!(
            "memory: {} docs · {}",
            app.runtime.memory_document_count, app.runtime.memory_last_status
        ));
    } else {
        lines.push("memory: off".to_owned());
    }
    let task_memory = app.task_memory_sidebar_lines();
    if task_memory
        .first()
        .is_some_and(|line| line != "task memory: none yet")
    {
        lines.extend(task_memory);
    }
    lines
}

fn detail_info_rail_session_lines(app: &AppState) -> Vec<String> {
    app.session_sidebar_lines()
        .into_iter()
        .chain(std::iter::once(if app.runtime.memory_enabled {
            format!(
                "memory: {} docs · {}",
                app.runtime.memory_document_count, app.runtime.memory_last_status
            )
        } else {
            "memory: off".to_owned()
        }))
        .chain(app.task_memory_sidebar_lines())
        .collect()
}

pub(crate) fn info_rail_permission_lines(app: &AppState) -> Vec<String> {
    let scope = if app.runtime.is_busy {
        "locked during run"
    } else {
        "saved default"
    };
    vec![format!(
        "mode: {} · {scope}",
        app.runtime.permission_default_mode
    )]
}

fn compact_info_rail_task_lines(app: &AppState) -> Vec<String> {
    let lines = app.task_sidebar_lines();
    if lines.len() <= INFO_RAIL_TASK_LINE_LIMIT {
        return lines;
    }
    let preferred = ["task:", "status:", "progress:", "current:", "last:"];
    let mut compact = Vec::new();
    for prefix in preferred {
        if let Some(line) = lines.iter().find(|line| line.starts_with(prefix)) {
            compact.push(line.clone());
        }
    }
    if compact.is_empty() {
        compact.extend(lines.into_iter().take(INFO_RAIL_TASK_LINE_LIMIT));
    }
    compact.truncate(INFO_RAIL_TASK_LINE_LIMIT);
    compact
}

fn compact_info_rail_usage_lines(app: &AppState) -> Vec<String> {
    let lines = app.usage_sidebar_lines();
    let preferred = ["ctx:", "compact:", "spent since opening:", "balance:"];
    preferred
        .iter()
        .filter_map(|prefix| lines.iter().find(|line| line.starts_with(prefix)).cloned())
        .collect()
}

fn info_rail_controls(app: &AppState, detail: bool) -> Vec<String> {
    let mut controls = global_control_hints(app.runtime.is_busy && app.approval.pending.is_none());
    if app.has_tool_cards() {
        controls.retain(|hint| !hint.starts_with("Ctrl-T: thinking"));
        controls.extend(tool_card_control_hints());
    }
    if detail {
        return controls;
    }

    let preferred: &[&str] = if app.has_tool_cards() {
        &["F2:", "Ctrl-T:", "Ctrl-G:"]
    } else {
        &["F1:", "F2:", "Ctrl-C:", "Esc:", "/ or `:"]
    };
    let mut compact = Vec::new();
    for prefix in preferred {
        if let Some(line) = controls.iter().find(|line| line.starts_with(prefix)) {
            compact.push(line.clone());
        }
        if compact.len() >= INFO_RAIL_CONTROL_LIMIT {
            break;
        }
    }
    if compact.len() < INFO_RAIL_CONTROL_LIMIT {
        for line in controls {
            if compact.iter().any(|existing| existing == &line) {
                continue;
            }
            compact.push(line);
            if compact.len() >= INFO_RAIL_CONTROL_LIMIT {
                break;
            }
        }
    }
    compact
}

fn short_session_label(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

#[derive(Debug, Clone)]
pub(crate) struct ComposerViewModel {
    pub mode_label: String,
    pub phase: RunPhase,
    pub provider_name: String,
    pub model_name: String,
    pub reasoning_effort_label: String,
    pub agent_rows: Vec<SidebarAgentRow>,
    pub agent_panel_focused: bool,
    pub input: String,
    pub input_rows: u16,
    pub cursor_position: (u16, u16),
}

impl ComposerViewModel {
    fn from_app(app: &AppState) -> Self {
        Self {
            mode_label: format!(
                "{} · agent: {}",
                app.composer_mode_label(),
                app.active_agent_label()
            ),
            phase: app.run_phase(),
            provider_name: app.runtime.provider_name.clone(),
            model_name: app.runtime.model_name.clone(),
            reasoning_effort_label: app.reasoning_effort_label().to_owned(),
            agent_rows: app.composer_agent_rows(),
            agent_panel_focused: app.is_composer_agent_panel_focused(),
            input: app.composer_display_input(),
            input_rows: app.composer_input_rows(),
            cursor_position: app.input_cursor_visual_position(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStripViewModel {
    pub title: String,
    pub detail: String,
    pub rows: Vec<TaskStripRowViewModel>,
}

impl TaskStripViewModel {
    pub(crate) fn from_task_strip_view(view: crate::app::task_sidebar::TaskStripView) -> Self {
        Self {
            title: view.title,
            detail: view.detail,
            rows: view
                .rows
                .into_iter()
                .map(|row| TaskStripRowViewModel {
                    kind: row.kind,
                    label: row.label,
                    active: row.active,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStripRowViewModel {
    pub kind: StatusKind,
    pub label: String,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FooterViewModel {
    pub phase: RunPhase,
    pub is_busy: bool,
    pub run_label: String,
    pub hints: String,
    pub context_label: String,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueActionButtonViewModel {
    pub label: String,
    pub detail: String,
    pub selected: bool,
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskMemoryInspectViewModel {
    pub summary: String,
    pub objective: String,
    pub decisions: Vec<String>,
    pub files_changed: Vec<String>,
    pub checks_run: Vec<String>,
    pub unresolved: Vec<String>,
}

impl TaskMemoryInspectViewModel {
    pub(crate) fn from_task_memory(memory: &TaskMemoryV1) -> Self {
        Self {
            summary: format!(
                "memory: {} · snapshot {}",
                compact_identifier(&memory.memory_id),
                compact_identifier(&memory.valid_for_snapshot)
            ),
            objective: format!("objective: {}", compact_memory_text(&memory.objective)),
            decisions: memory
                .decisions
                .iter()
                .take(3)
                .map(|decision| {
                    let mut line = format!(
                        "decision: {}{}",
                        compact_memory_text(&decision.decision.text),
                        fact_source_marker(&decision.decision)
                    );
                    if let Some(rationale) = &decision.rationale {
                        line.push_str(&format!(" · why {}", compact_memory_text(&rationale.text)));
                    }
                    line
                })
                .collect(),
            files_changed: memory
                .files_changed
                .iter()
                .take(5)
                .map(|file| format!("file: {}", file.path.display()))
                .collect(),
            checks_run: memory
                .verification_results
                .iter()
                .take(5)
                .map(|receipt| format!("check: {}", compact_identifier(receipt)))
                .collect(),
            unresolved: memory
                .unresolved_issues
                .iter()
                .take(3)
                .map(|fact| {
                    format!(
                        "unresolved: {}{}",
                        compact_memory_text(&fact.text),
                        fact_source_marker(fact)
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            "[memory]".to_owned(),
            self.summary.clone(),
            self.objective.clone(),
        ];
        lines.extend(non_empty_or_none(&self.decisions, "decision"));
        lines.extend(non_empty_or_none(&self.files_changed, "file"));
        lines.extend(non_empty_or_none(&self.checks_run, "check"));
        lines.extend(non_empty_or_none(&self.unresolved, "unresolved"));
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryPanelViewModel {
    pub title: String,
    pub last_known: String,
    pub risk_summary: String,
    pub recommended_action: String,
}

impl RecoveryPanelViewModel {
    pub(crate) fn from_entries(entries: &[SessionLogEntry], now_ms: u64) -> Option<Self> {
        let resume_projection = ResumeJobStateProjection::from_entries(entries, now_ms);
        let stale_jobs = resume_projection.stale_jobs();
        let agent_projection = AgentThreadStateProjection::from_entries(entries);
        let interrupted_mailbox = agent_projection
            .mailbox_messages
            .values()
            .filter(|message| message.status == AgentMailboxStatus::Interrupted)
            .count();
        let pending_mailbox = agent_projection
            .mailbox_messages
            .values()
            .filter(|message| {
                matches!(
                    message.status,
                    AgentMailboxStatus::Queued | AgentMailboxStatus::Delivered
                )
            })
            .count();
        let interrupted_attempts = agent_projection
            .threads
            .values()
            .flat_map(|thread| thread.attempts.values())
            .filter(|attempt| attempt.interrupted.is_some())
            .count();
        if stale_jobs.is_empty()
            && interrupted_mailbox == 0
            && pending_mailbox == 0
            && interrupted_attempts == 0
        {
            return None;
        }

        let last_known = stale_jobs
            .first()
            .map(|job| {
                let task = job
                    .intent
                    .task_id
                    .as_ref()
                    .map(|task| task.as_str())
                    .unwrap_or("session");
                let step = job
                    .lease
                    .as_ref()
                    .and_then(|lease| lease.step_id.as_ref())
                    .map(|step| step.as_str())
                    .unwrap_or("unknown step");
                format!("last: {task} · {step}")
            })
            .unwrap_or_else(|| {
                let mailbox = interrupted_mailbox + pending_mailbox;
                if mailbox > 0 {
                    format!("last: {mailbox} pending mailbox messages")
                } else {
                    format!("last: {interrupted_attempts} interrupted agent attempts")
                }
            });

        let risk_summary = recovery_risk_summary(
            stale_jobs.len(),
            interrupted_mailbox,
            pending_mailbox,
            interrupted_attempts,
        );
        let recommended_action = match (
            stale_jobs.len(),
            interrupted_mailbox + pending_mailbox,
            interrupted_attempts,
        ) {
            (0, 0, _) => "action: inspect attempt, then resume or mark abandoned".to_owned(),
            (0, _, _) => "action: inspect mailbox, then resume or mark abandoned".to_owned(),
            _ => "action: inspect recovery, then resume or mark abandoned".to_owned(),
        };

        Some(Self {
            title: "Recovery".to_owned(),
            last_known,
            risk_summary,
            recommended_action,
        })
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        vec![
            self.title.clone(),
            format!("  {}", self.last_known),
            format!("  risk: {}", self.risk_summary),
            format!("  {}", self.recommended_action),
        ]
    }
}

fn recovery_risk_summary(
    stale_jobs: usize,
    interrupted_mailbox: usize,
    pending_mailbox: usize,
    interrupted_attempts: usize,
) -> String {
    let mut parts = Vec::new();
    if stale_jobs > 0 {
        parts.push(format!("{stale_jobs} stale jobs"));
    }
    if interrupted_mailbox > 0 {
        parts.push(format!("{interrupted_mailbox} interrupted mailbox"));
    }
    if pending_mailbox > 0 {
        parts.push(format!("{pending_mailbox} pending mailbox"));
    }
    if interrupted_attempts > 0 {
        parts.push(format!("{interrupted_attempts} interrupted attempts"));
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn non_empty_or_none(lines: &[String], label: &str) -> Vec<String> {
    if lines.is_empty() {
        vec![format!("{label}: none")]
    } else {
        lines.to_vec()
    }
}

fn fact_source_marker(fact: &SourcedFact) -> &'static str {
    match (fact.model_generated, fact.verified) {
        (true, false) => " [model/unverified]",
        (true, true) => " [model/verified]",
        (false, true) => " [verified]",
        (false, false) => "",
    }
}

fn compact_identifier(value: &str) -> String {
    compact_string(value, 24)
}

fn compact_memory_text(value: &str) -> String {
    compact_string(value, 96)
}

fn compact_string(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let keep = max_chars.saturating_sub(3);
    let mut text = value.chars().take(keep).collect::<String>();
    text.push_str("...");
    text
}

// RFC-0006 keeps this adapter available for the provenance surface without adding another default
// info-rail section before the product flow is selected.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextProvenanceSummaryViewModel {
    pub budget_line: String,
    pub top_sources: Vec<String>,
    pub excluded_summary: Vec<String>,
    pub warning: Option<String>,
    pub recommended_action: Option<String>,
}

#[allow(dead_code)]
impl ContextProvenanceSummaryViewModel {
    pub(crate) fn from_packed_context(packed: &PackedContext, top_source_limit: usize) -> Self {
        let included = packed
            .stable_prefix
            .iter()
            .chain(packed.dynamic_suffix.iter())
            .collect::<Vec<_>>();
        let budget_line = format!(
            "context: {} / {} tokens · {} included · {} excluded",
            packed.used_tokens,
            packed.max_tokens,
            included.len(),
            packed.excluded.len()
        );
        let top_sources = context_source_summary(&included, top_source_limit);
        let excluded_summary = context_excluded_summary(&packed.excluded);
        let warning = context_warning(&packed.excluded);
        let recommended_action = context_recommended_action(&packed.excluded);

        Self {
            budget_line,
            top_sources,
            excluded_summary,
            warning,
            recommended_action,
        }
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![self.budget_line.clone()];
        lines.extend(
            self.top_sources
                .iter()
                .map(|line| format!("source: {line}")),
        );
        lines.extend(
            self.excluded_summary
                .iter()
                .map(|line| format!("excluded: {line}")),
        );
        if let Some(warning) = &self.warning {
            lines.push(format!("warning: {warning}"));
        }
        if let Some(action) = &self.recommended_action {
            lines.push(format!("action: {action}"));
        }
        lines
    }
}

#[allow(dead_code)]
fn context_source_summary(items: &[&ContextItem], limit: usize) -> Vec<String> {
    let mut groups = BTreeMap::<&'static str, (usize, usize)>::new();
    for item in items {
        let entry = groups
            .entry(context_source_label(&item.source))
            .or_default();
        entry.0 += 1;
        entry.1 += item.token_cost;
    }
    let mut rows = groups
        .into_iter()
        .map(|(source, (count, tokens))| (source, count, tokens))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(right.0)));
    rows.into_iter()
        .take(limit)
        .map(|(source, count, tokens)| format!("{source} · {count} item(s) · {tokens} tokens"))
        .collect()
}

#[allow(dead_code)]
fn context_excluded_summary(items: &[ContextItem]) -> Vec<String> {
    let mut groups = BTreeMap::<&'static str, usize>::new();
    for item in items {
        *groups
            .entry(context_exclusion_label(&item.inclusion_reason))
            .or_default() += 1;
    }
    groups
        .into_iter()
        .map(|(reason, count)| format!("{reason} · {count} item(s)"))
        .collect()
}

#[allow(dead_code)]
fn context_warning(items: &[ContextItem]) -> Option<String> {
    if items
        .iter()
        .any(|item| item.inclusion_reason == ContextInclusionReason::ExcludedSecret)
    {
        return Some("secret-like context was blocked".to_owned());
    }
    if items
        .iter()
        .any(|item| item.inclusion_reason == ContextInclusionReason::ExcludedUntrustedWorkspace)
    {
        return Some("untrusted workspace context was not promoted".to_owned());
    }
    None
}

#[allow(dead_code)]
fn context_recommended_action(items: &[ContextItem]) -> Option<String> {
    if items
        .iter()
        .any(|item| item.inclusion_reason == ContextInclusionReason::ExcludedSecret)
    {
        return Some("review egress".to_owned());
    }
    if items
        .iter()
        .any(|item| item.inclusion_reason == ContextInclusionReason::ExcludedUntrustedWorkspace)
    {
        return Some("review trust".to_owned());
    }
    if items
        .iter()
        .any(|item| item.inclusion_reason == ContextInclusionReason::ExcludedTokenBudget)
    {
        return Some("adjust context budget".to_owned());
    }
    None
}

#[allow(dead_code)]
fn context_source_label(source: &ContextSource) -> &'static str {
    match source {
        ContextSource::SystemPrompt => "system",
        ContextSource::UserMessage => "user",
        ContextSource::WorkspaceInstruction => "workspace instruction",
        ContextSource::RepositoryFile => "repo file",
        ContextSource::ToolObservation => "tool",
        ContextSource::DurableEvent => "event",
        ContextSource::EvidenceReceipt => "evidence",
        ContextSource::MutationEvidence => "mutation",
        ContextSource::VerificationEvidence => "verification",
        ContextSource::LspSymbol => "symbol",
        ContextSource::LspDiagnostic => "diagnostic",
        ContextSource::LspReference => "reference",
        ContextSource::CurrentDiff => "diff",
        ContextSource::SessionArchive => "session archive",
        ContextSource::TaskDigest => "task digest",
        ContextSource::ExtensionProvided => "extension",
    }
}

#[allow(dead_code)]
fn context_exclusion_label(reason: &ContextInclusionReason) -> &'static str {
    match reason {
        ContextInclusionReason::ExcludedUntrustedWorkspace => "untrusted workspace",
        ContextInclusionReason::ExcludedSecret => "secret",
        ContextInclusionReason::ExcludedEgressDenied => "egress denied",
        ContextInclusionReason::ExcludedTokenBudget => "token budget",
        ContextInclusionReason::ExcludedUnsupported => "unsupported",
        _ => "other",
    }
}

impl FooterViewModel {
    fn from_app(app: &AppState) -> Self {
        Self {
            phase: app.run_phase(),
            is_busy: app.runtime.is_busy && app.approval.pending.is_none(),
            run_label: footer_run_label(app),
            hints: footer_hints(app),
            context_label: app.context_usage_line(),
        }
    }
}

fn footer_run_label(app: &AppState) -> String {
    if let Some(activity) = app.live_activity_summary() {
        return format!("{} · {}", activity.label, activity.detail);
    }
    "ready".to_owned()
}

fn footer_hints(app: &AppState) -> String {
    let agent = format!("agent: {}", app.active_agent_label());
    if app.pending_plan_approval().is_some() {
        return format!("{agent} · A ask · W workspace edits · C continue · Esc discard");
    }
    if app.approval.pending.is_some() {
        return format!("{agent} · Y allow · N deny · V diff");
    }
    if app.runtime.is_busy && matches!(app.run_phase(), RunPhase::Agent(_)) {
        return format!(
            "{agent} · Enter queue next turn · Ctrl-B background · Esc interrupt · Ctrl-T details"
        );
    }
    if app.runtime.is_busy {
        return format!("{agent} · Enter queue next turn · Esc interrupt · Ctrl-T details");
    }
    if app.active_pane == PaneFocus::Composer && app.has_slash_selector() {
        if app.has_agent_mention_selector() {
            return format!("{agent} · ↑↓ choose · Tab/Enter insert · Esc close");
        }
        return format!("{agent} · ↑↓ choose · Tab accept · Enter run · Esc close");
    }
    if app.is_composer_queue_panel_focused() {
        return format!("{agent} · Queue ↑↓ item · Tab action · Enter run · Esc input");
    }
    if app.is_composer_agent_panel_focused() {
        return format!("{agent} · ↑↓ agent · Enter switch · C close · M message · Esc input");
    }
    format!("{agent} · Enter send · Shift-Enter newline · Alt-A agent · / commands")
}

#[derive(Debug, Clone)]
pub(crate) struct LivePanelViewModel {
    pub phase: RunPhase,
    pub queue_rows: Vec<ComposerQueueRow>,
    pub queue_paused: bool,
    pub queue_panel_focused: bool,
    pub queue_action_buttons: Vec<QueueActionButtonViewModel>,
    pub progress: Option<LiveProgressViewModel>,
    pub plan_approval: Option<PlanApprovalViewModel>,
    pub task_strip: Option<TaskStripViewModel>,
    pub transcript_lines: Vec<Line<'static>>,
}

impl LivePanelViewModel {
    pub(crate) fn from_app(app: &AppState, transcript_rows: usize) -> Self {
        Self {
            phase: app.live_panel_phase(),
            queue_rows: app.composer_queue_rows(),
            queue_paused: app.composer_queue_paused(),
            queue_panel_focused: app.is_composer_queue_panel_focused(),
            queue_action_buttons: queue_action_buttons(app.selected_composer_queue_action()),
            progress: app
                .live_activity_summary()
                .map(|summary| LiveProgressViewModel::from_parts(&summary.label, &summary.detail)),
            plan_approval: app
                .pending_plan_approval()
                .map(PlanApprovalViewModel::from_pending),
            task_strip: app
                .task_strip_view()
                .map(TaskStripViewModel::from_task_strip_view),
            transcript_lines: app.transcript_lines(transcript_rows),
        }
    }
}

fn queue_action_buttons(selected: ComposerQueueAction) -> Vec<QueueActionButtonViewModel> {
    ComposerQueueAction::ORDER
        .iter()
        .map(|action| QueueActionButtonViewModel {
            label: action.label().to_owned(),
            detail: action.detail().to_owned(),
            selected: *action == selected,
            destructive: action.is_destructive(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanApprovalViewModel {
    pub hash: String,
    pub scope_summary: String,
}

impl PlanApprovalViewModel {
    fn from_pending(pending: &crate::app::PendingPlanApproval) -> Self {
        Self {
            hash: short_plan_hash(&pending.plan_hash),
            scope_summary: pending.scope_summary.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveProgressViewModel {
    pub title: String,
    pub detail: String,
}

impl LiveProgressViewModel {
    fn from_parts(label: &str, detail: &str) -> Self {
        let title = match label {
            "thinking" => "Thinking".to_owned(),
            "tool" => tool_progress_title(detail),
            "mcp" => "MCP".to_owned(),
            "streaming" => "Replying".to_owned(),
            "approval" => "Approval".to_owned(),
            _ => "Working".to_owned(),
        };
        Self {
            title,
            detail: detail.to_owned(),
        }
    }
}

fn short_plan_hash(plan_hash: &str) -> String {
    plan_hash.chars().take(19).collect()
}

fn tool_progress_title(detail: &str) -> String {
    let tool_name = detail
        .strip_prefix("running ")
        .unwrap_or(detail)
        .split_whitespace()
        .next()
        .unwrap_or("tool");
    match tool_name {
        "bash" => "Bash".to_owned(),
        "read_file" => "Read".to_owned(),
        "write_file" => "Write".to_owned(),
        "edit_file" => "Edit".to_owned(),
        "delete_file" => "Delete".to_owned(),
        "grep" => "Search".to_owned(),
        "glob" | "ls" => "Inspect".to_owned(),
        other => title_case_tool_name(other),
    }
}

fn title_case_tool_name(tool_name: &str) -> String {
    let words = tool_name
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return "Tool".to_owned();
    }
    words
        .into_iter()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_path_label(path: &Path) -> String {
    let display = path.to_string_lossy().into_owned();
    if let Ok(home) = env::var("HOME")
        && let Some(suffix) = display.strip_prefix(&home)
    {
        return format!("~{suffix}");
    }
    display
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/view_model_tests.rs"]
mod tests;
