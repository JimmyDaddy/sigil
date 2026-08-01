use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use sigil_kernel::{
    EnvironmentContainment, ExecutionContainmentRequest, FilesystemContainment, NetworkContainment,
    PermissionDecisionReason, PermissionDecisionSource, ProcessContainment, SyntaxThemeId,
    ToolAnalysisReasonCode, ToolAnalysisStatus, ToolPermissionEffect, ToolSubject,
};

use crate::app::{
    AppState, ApprovalAction, ApprovalChangeSetSummary, ApprovalDiagnosticSummary,
    ApprovalDiffLine, ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
    session_grant_unavailable_reason_label,
};

use super::{
    diff::{
        DiffLineKind, diff_line_number_text, diff_line_number_width, diff_line_style_for_palette,
        number_unified_diff_lines,
    },
    geometry::{halo_rect, shadow_rect},
    layout_snapshot::{
        approval_diff_view_control_label, approval_metadata_control_label, approval_modal_area,
    },
    markdown::{MarkdownRenderOptions, render_inline_markdown_spans_with_palette},
    theme::{self, ThemePalette},
};

pub(super) fn render_approval_modal(frame: &mut Frame, app: &AppState) {
    let Some(view) = app.approval_modal_view() else {
        return;
    };
    let current_theme = theme::resolve_for_app(app);
    let palette = &current_theme.palette;
    let area = approval_modal_area(frame.area(), &view);
    let backdrop = halo_rect(area, frame.area(), 5, 2);
    if backdrop.width > 0 && backdrop.height > 0 {
        frame.render_widget(Clear, backdrop);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(palette.approval_border))
                .style(Style::default().bg(palette.approval_backdrop_bg)),
            backdrop,
        );
    }
    let shadow = shadow_rect(area, frame.area());
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(
            Block::default().style(Style::default().bg(palette.approval_shadow)),
            shadow,
        );
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(approval_block_title(app))
        .title_style(
            Style::default()
                .fg(palette.text_inverse)
                .bg(palette.approval_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.approval_border))
        .style(Style::default().bg(palette.approval_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let header_lines = approval_header_lines_with_palette(
        &view,
        area.width.saturating_sub(2) as usize,
        current_theme.syntax_theme,
        palette,
    );
    let footer_lines = approval_footer_lines_with_palette(&view, palette);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((header_lines.len() as u16).saturating_add(2)),
            Constraint::Min(8),
            Constraint::Length((footer_lines.len() as u16).saturating_add(2)),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Text::from(header_lines))
            .block(
                Block::default()
                    .title("Summary")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.accent_info)),
            )
            .wrap(Wrap { trim: false }),
        layout[0],
    );

    let body_chunks = if !view.file_rows.is_empty() {
        let file_width = if layout[1].width >= 92 { 28 } else { 22 }
            .min(layout[1].width.saturating_sub(18))
            .max(16)
            .min(layout[1].width);
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(file_width), Constraint::Min(12)])
            .split(layout[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1)])
            .split(layout[1])
    };

    if !view.file_rows.is_empty() {
        let file_lines = view
            .file_rows
            .iter()
            .enumerate()
            .map(|(index, row)| render_approval_file_row_with_palette(index, row, palette))
            .collect::<Vec<_>>();
        let selected_file_index = view
            .file_rows
            .iter()
            .position(|row| row.selected)
            .map(|index| index + 1)
            .unwrap_or(0);
        frame.render_widget(
            Paragraph::new(Text::from(file_lines))
                .block(
                    Block::default()
                        .title(format!(
                            "Files {selected_file_index}/{}",
                            view.file_rows.len()
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(palette.border_subtle)),
                )
                .wrap(Wrap { trim: false }),
            body_chunks[0],
        );
    }

    let diff_area = *body_chunks.last().unwrap_or(&layout[1]);
    let diff_block = Block::default()
        .title("Diff")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border_subtle));
    let diff_inner = diff_block.inner(diff_area);
    frame.render_widget(diff_block, diff_area);
    if diff_inner.width > 0 && diff_inner.height > 0 {
        let diff_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(diff_inner);
        frame.render_widget(
            Paragraph::new(approval_diff_status_line_with_palette(&view, palette)),
            diff_sections[0],
        );
        let numbered =
            number_unified_diff_lines(view.diff_lines.iter().map(|line| line.text.as_str()));
        let line_number_width = diff_line_number_width(&numbered);
        let diff_lines = view
            .diff_lines
            .iter()
            .cloned()
            .zip(numbered)
            .map(|(line, numbered)| {
                render_approval_diff_line_with_palette(
                    line,
                    numbered.old_line,
                    numbered.new_line,
                    line_number_width,
                    palette,
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(Text::from(diff_lines))
                .scroll((app.approval.scroll_back as u16, 0))
                .wrap(Wrap { trim: false }),
            diff_sections[1],
        );
    }

    frame.render_widget(
        Paragraph::new(Text::from(footer_lines))
            .block(
                Block::default()
                    .title("Actions")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border_subtle)),
            )
            .wrap(Wrap { trim: false }),
        layout[2],
    );
}

fn approval_block_title(_app: &AppState) -> &'static str {
    " Review Tool Call "
}

#[cfg(test)]
fn approval_header_lines(view: &ApprovalModalView, max_content_width: usize) -> Vec<Line<'static>> {
    let palette = theme::default_palette();
    approval_header_lines_with_palette(view, max_content_width, SyntaxThemeId::default(), &palette)
}

fn approval_header_lines_with_palette(
    view: &ApprovalModalView,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            approval_badge_with_palette(
                &view.access_label,
                permission_risk_color(view.risk, palette),
                palette,
            ),
            Span::raw(" "),
            Span::styled(
                view.tool_name.clone(),
                Style::default()
                    .fg(palette.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("call", Style::default().fg(palette.text_muted)),
            Span::raw(" "),
            Span::styled(
                view.call_id.clone(),
                Style::default().fg(palette.accent_info),
            ),
        ]),
        Line::from(vec![Span::styled(
            view.preview_title.clone(),
            Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            approval_badge_with_palette(
                &format!("risk {}", approval_risk_label(view.risk)),
                permission_risk_color(view.risk, palette),
                palette,
            ),
            Span::raw(" "),
            Span::styled("policy", Style::default().fg(palette.text_muted)),
            Span::raw(" "),
            Span::styled(
                view.policy_label.clone(),
                Style::default().fg(palette.text_secondary),
            ),
        ]),
    ];

    if let Some(source_agent) = &view.source_agent {
        lines.push(Line::from(vec![
            approval_badge_with_palette("agent", palette.accent_info, palette),
            Span::raw(" "),
            Span::styled(
                source_agent.clone(),
                Style::default().fg(palette.text_primary),
            ),
        ]));
    }

    if let Some(change_set) = &view.change_set {
        lines.push(approval_change_set_line_with_palette(change_set, palette));
        lines.push(approval_format_hint_line_with_palette(change_set, palette));
    }

    if view.metadata_collapsed {
        lines.push(Line::from(vec![
            approval_badge_with_palette("meta hidden", palette.text_muted, palette),
            Span::raw(" "),
            Span::styled("press M to expand", Style::default().fg(palette.text_muted)),
        ]));
    } else if view.preview_summary.trim().is_empty() {
        lines.push(Line::styled(
            "No preview summary provided.",
            Style::default().fg(palette.text_muted),
        ));
    } else {
        let markdown_options =
            MarkdownRenderOptions::modal(max_content_width).with_syntax_theme(syntax_theme);
        lines.extend(view.preview_summary.lines().take(4).map(|line| {
            Line::from(render_inline_markdown_spans_with_palette(
                line,
                Style::default().fg(palette.text_secondary),
                markdown_options,
                palette,
            ))
        }));
    }

    if !view.metadata_collapsed {
        lines.extend(approval_permission_metadata_lines(
            view,
            max_content_width,
            palette,
        ));
    }

    let change_count = view.changed_files.len().max(view.file_rows.len());
    lines.push(Line::from(vec![
        Span::styled("files", Style::default().fg(palette.text_muted)),
        Span::raw(" "),
        Span::styled(
            change_count.to_string(),
            Style::default().fg(palette.text_primary),
        ),
        Span::raw("  "),
        Span::styled("hunks", Style::default().fg(palette.text_muted)),
        Span::raw(" "),
        Span::styled(
            view.hunk_total.to_string(),
            Style::default().fg(palette.text_primary),
        ),
        Span::raw("  "),
        Span::styled("mode", Style::default().fg(palette.text_muted)),
        Span::raw(" "),
        Span::styled(
            view.diff_mode_label,
            Style::default().fg(palette.accent_info),
        ),
    ]));
    lines
}

fn approval_permission_metadata_lines(
    view: &ApprovalModalView,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let summary = if view.safe_summary.detail.trim().is_empty() {
        view.safe_summary.title.trim().to_owned()
    } else if view.safe_summary.title.trim().is_empty() {
        view.safe_summary.detail.trim().to_owned()
    } else {
        format!(
            "{} — {}",
            view.safe_summary.title.trim(),
            view.safe_summary.detail.trim()
        )
    };
    let effects = if view.effects.is_empty() {
        "none".to_owned()
    } else {
        view.effects
            .iter()
            .map(|effect| permission_effect_label(*effect))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let subjects = bounded_subject_summary(&view.subjects);
    let analysis = analysis_status_label(&view.analysis);
    let containment = containment_label(&view.containment);
    let decisions = bounded_decision_reason_summary(&view.decision_reasons);
    let session_grant = if view.session_grant_available {
        "available for equivalent policy-bounded requests".to_owned()
    } else {
        format!(
            "unavailable because {}",
            session_grant_unavailable_reason_label(view.session_grant_unavailable_reason)
        )
    };

    let mut lines = [
        ("summary", summary),
        ("effects", format!("{effects}  subjects {subjects}")),
        ("analysis", format!("{analysis}  containment {containment}")),
        ("decisions", decisions),
        ("session grant", session_grant),
    ]
    .into_iter()
    .map(|(label, value)| {
        Line::from(vec![
            Span::styled(format!("{label} "), Style::default().fg(palette.text_muted)),
            Span::styled(
                bounded_metadata_text(&value, max_content_width.saturating_sub(label.len() + 1)),
                Style::default().fg(palette.text_secondary),
            ),
        ])
    })
    .collect::<Vec<_>>();
    if let Some(hint) = analysis_recovery_hint(&view.analysis) {
        lines.push(Line::from(vec![
            Span::styled("recovery ", Style::default().fg(palette.text_muted)),
            Span::styled(hint, Style::default().fg(palette.accent_info)),
        ]));
    }
    lines
}

fn analysis_recovery_hint(analysis: &ToolAnalysisStatus) -> Option<&'static str> {
    let unsupported_shell_syntax = match analysis {
        ToolAnalysisStatus::Unsupported { reason } | ToolAnalysisStatus::Invalid { reason } => {
            matches!(
                reason.code,
                ToolAnalysisReasonCode::UnsupportedSyntax | ToolAnalysisReasonCode::InvalidSyntax
            )
        }
        ToolAnalysisStatus::Conservative { reasons } => reasons.iter().any(|reason| {
            matches!(
                reason.code,
                ToolAnalysisReasonCode::UnsupportedSyntax | ToolAnalysisReasonCode::InvalidSyntax
            )
        }),
        ToolAnalysisStatus::Complete => false,
    };
    unsupported_shell_syntax.then_some(
        "unsupported shell dialect; run /doctor, then review terminal settings in /config",
    )
}

fn bounded_metadata_text(value: &str, max_chars: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() && max_chars > 1 {
        format!(
            "{}…",
            bounded.chars().take(max_chars - 1).collect::<String>()
        )
    } else {
        bounded
    }
}

fn permission_effect_label(effect: ToolPermissionEffect) -> &'static str {
    match effect {
        ToolPermissionEffect::FileRead => "file_read",
        ToolPermissionEffect::FileWrite => "file_write",
        ToolPermissionEffect::FileDelete => "file_delete",
        ToolPermissionEffect::ExecuteTrustedBinary => "execute_trusted_binary",
        ToolPermissionEffect::ExecuteWorkspaceCode => "execute_workspace_code",
        ToolPermissionEffect::ExecuteDynamicCode => "execute_dynamic_code",
        ToolPermissionEffect::NetworkRead => "network_read",
        ToolPermissionEffect::NetworkMutate => "network_mutate",
        ToolPermissionEffect::NetworkUnknown => "network_unknown",
        ToolPermissionEffect::AgentLifecycle => "agent_lifecycle",
        ToolPermissionEffect::ProcessControl => "process_control",
        ToolPermissionEffect::PrivilegeEscalation => "privilege_escalation",
        ToolPermissionEffect::PersistenceChange => "persistence_change",
        ToolPermissionEffect::RemoteMutation => "remote_mutation",
        ToolPermissionEffect::CredentialAccess => "credential_access",
        ToolPermissionEffect::ExternalApplicationControl => "external_application_control",
        ToolPermissionEffect::Unknown => "unknown",
    }
}

fn bounded_subject_summary(subjects: &[ToolSubject]) -> String {
    if subjects.is_empty() {
        return "none".to_owned();
    }
    let mut groups = std::collections::BTreeMap::<String, usize>::new();
    for subject in subjects {
        *groups
            .entry(format!(
                "{}({})",
                subject.kind.as_str(),
                subject.scope.as_str()
            ))
            .or_default() += 1;
    }
    groups
        .into_iter()
        .take(4)
        .map(|(label, count)| {
            if count == 1 {
                label
            } else {
                format!("{label}×{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn analysis_status_label(status: &ToolAnalysisStatus) -> String {
    match status {
        ToolAnalysisStatus::Complete => "complete".to_owned(),
        ToolAnalysisStatus::Conservative { reasons } => format!(
            "conservative ({})",
            reasons
                .iter()
                .take(3)
                .map(|reason| analysis_reason_code_label(reason.code))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ToolAnalysisStatus::Unsupported { reason } => {
            format!("unsupported ({})", analysis_reason_code_label(reason.code))
        }
        ToolAnalysisStatus::Invalid { reason } => {
            format!("invalid ({})", analysis_reason_code_label(reason.code))
        }
    }
}

fn analysis_reason_code_label(code: ToolAnalysisReasonCode) -> &'static str {
    match code {
        ToolAnalysisReasonCode::UnknownProgram => "unknown_program",
        ToolAnalysisReasonCode::DynamicCommand => "dynamic_command",
        ToolAnalysisReasonCode::UnsupportedSyntax => "unsupported_syntax",
        ToolAnalysisReasonCode::InvalidSyntax => "invalid_syntax",
        ToolAnalysisReasonCode::AnalysisLimitExceeded => "analysis_limit_exceeded",
        ToolAnalysisReasonCode::UnresolvedPath => "unresolved_path",
        ToolAnalysisReasonCode::UnresolvedExecutable => "unresolved_executable",
        ToolAnalysisReasonCode::UnprovenContainment => "unproven_containment",
    }
}

fn containment_label(containment: &ExecutionContainmentRequest) -> String {
    format!(
        "fs {} · network {} · process {} · env {} · persistent {}",
        filesystem_containment_label(containment.filesystem),
        network_containment_label(containment.network),
        process_containment_label(containment.process),
        environment_containment_label(containment.environment),
        if containment.persistent_process {
            "yes"
        } else {
            "no"
        }
    )
}

fn filesystem_containment_label(value: FilesystemContainment) -> &'static str {
    match value {
        FilesystemContainment::Unspecified => "unspecified",
        FilesystemContainment::WorkspaceReadOnly => "workspace_read_only",
        FilesystemContainment::WorkspaceWrite => "workspace_write",
        FilesystemContainment::WorkspaceAndScratch => "workspace_and_scratch",
    }
}

fn network_containment_label(value: NetworkContainment) -> &'static str {
    match value {
        NetworkContainment::Unspecified => "unspecified",
        NetworkContainment::Deny => "deny",
        NetworkContainment::ReadOnly => "read_only",
        NetworkContainment::Allow => "allow",
    }
}

fn process_containment_label(value: ProcessContainment) -> &'static str {
    match value {
        ProcessContainment::Unspecified => "unspecified",
        ProcessContainment::OwnedTree => "owned_tree",
        ProcessContainment::Isolated => "isolated",
    }
}

fn environment_containment_label(value: EnvironmentContainment) -> &'static str {
    match value {
        EnvironmentContainment::Unspecified => "unspecified",
        EnvironmentContainment::Restricted => "restricted",
        EnvironmentContainment::UserInherited => "user_inherited",
    }
}

fn bounded_decision_reason_summary(reasons: &[PermissionDecisionReason]) -> String {
    if reasons.is_empty() {
        return "none".to_owned();
    }
    reasons
        .iter()
        .take(3)
        .map(|reason| {
            format!(
                "{}/{}",
                decision_source_label(reason.source),
                reason.code.trim()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn decision_source_label(source: PermissionDecisionSource) -> &'static str {
    match source {
        PermissionDecisionSource::HardSafety => "hard_safety",
        PermissionDecisionSource::ManagedRule => "managed_rule",
        PermissionDecisionSource::DelegatedRule => "delegated_rule",
        PermissionDecisionSource::UserRule => "user_rule",
        PermissionDecisionSource::SessionGrant => "session_grant",
        PermissionDecisionSource::SandboxSubstitution => "sandbox_substitution",
        PermissionDecisionSource::PermissionModeDefault => "permission_mode_default",
        PermissionDecisionSource::ToolDefault => "tool_default",
    }
}

fn approval_risk_label(risk: sigil_kernel::PermissionRisk) -> &'static str {
    match risk {
        sigil_kernel::PermissionRisk::Low => "low",
        sigil_kernel::PermissionRisk::Medium => "medium",
        sigil_kernel::PermissionRisk::High => "high",
        sigil_kernel::PermissionRisk::Destructive => "destructive",
        sigil_kernel::PermissionRisk::Protected => "protected",
    }
}

fn permission_risk_color(risk: sigil_kernel::PermissionRisk, palette: &ThemePalette) -> Color {
    match risk {
        sigil_kernel::PermissionRisk::Low => palette.risk_low,
        sigil_kernel::PermissionRisk::Medium => palette.risk_medium,
        sigil_kernel::PermissionRisk::High
        | sigil_kernel::PermissionRisk::Destructive
        | sigil_kernel::PermissionRisk::Protected => palette.risk_high,
    }
}

#[allow(dead_code)]
fn approval_change_set_line(change_set: &ApprovalChangeSetSummary) -> Line<'static> {
    let palette = theme::default_palette();
    approval_change_set_line_with_palette(change_set, &palette)
}

fn approval_change_set_line_with_palette(
    change_set: &ApprovalChangeSetSummary,
    palette: &ThemePalette,
) -> Line<'static> {
    Line::from(vec![
        approval_badge_with_palette("change set", palette.accent_info, palette),
        Span::raw(" "),
        Span::styled(
            change_set.id.clone(),
            Style::default().fg(palette.text_primary),
        ),
        Span::raw("  "),
        approval_badge_with_palette(
            &format!("risk {}", change_set.risk),
            approval_risk_color_with_palette(&change_set.risk, palette),
            palette,
        ),
    ])
}

#[allow(dead_code)]
fn approval_format_hint_line(change_set: &ApprovalChangeSetSummary) -> Line<'static> {
    let palette = theme::default_palette();
    approval_format_hint_line_with_palette(change_set, &palette)
}

fn approval_format_hint_line_with_palette(
    change_set: &ApprovalChangeSetSummary,
    palette: &ThemePalette,
) -> Line<'static> {
    Line::from(vec![
        Span::styled("format", Style::default().fg(palette.text_muted)),
        Span::raw(" "),
        Span::styled(
            change_set.format_hint.clone(),
            Style::default().fg(palette.text_secondary),
        ),
    ])
}

#[cfg(test)]
fn approval_footer_lines(view: &ApprovalModalView) -> Vec<Line<'static>> {
    let palette = theme::default_palette();
    approval_footer_lines_with_palette(view, &palette)
}

fn approval_footer_lines_with_palette(
    view: &ApprovalModalView,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let file_hint = if view.file_rows.len() > 1 {
        "  ,/. file"
    } else {
        ""
    };
    let mut action_line = approval_action_badges(view, palette);
    if action_line.is_empty() {
        action_line.push(Span::styled(
            "No available actions",
            Style::default().fg(palette.text_muted),
        ));
    }
    let action_hint =
        "Tab/Left/Right switch  Enter select  Y allow once  N deny  M details  V view";
    vec![
        Line::from(action_line),
        Line::styled(
            format!("{action_hint}  [,] hunk{file_hint}  Up/Down scroll"),
            Style::default().fg(palette.text_muted),
        ),
    ]
}

fn approval_action_badges(view: &ApprovalModalView, palette: &ThemePalette) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, action) in ApprovalAction::order(view.session_grant_available)
        .iter()
        .enumerate()
    {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let color = match action {
            ApprovalAction::AllowOnce | ApprovalAction::AllowSession => palette.approval_allow_bg,
            ApprovalAction::Deny => palette.approval_deny_bg,
        };
        spans.push(approval_action_badge_with_palette(
            action.label(),
            color,
            view.selected_action == *action,
            palette,
        ));
    }
    spans
}

#[cfg(test)]
fn approval_action_badge(label: &str, color: Color, selected: bool) -> Span<'static> {
    let palette = theme::default_palette();
    approval_action_badge_with_palette(label, color, selected, &palette)
}

fn approval_action_badge_with_palette(
    label: &str,
    color: Color,
    selected: bool,
    palette: &ThemePalette,
) -> Span<'static> {
    if selected {
        return Span::styled(
            format!("▶ {label} "),
            Style::default()
                .fg(palette.text_inverse)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        );
    }
    approval_badge_with_palette(label, color, palette)
}

#[cfg(test)]
fn render_approval_file_row(index: usize, row: &ApprovalFileRow) -> Line<'static> {
    let palette = theme::default_palette();
    render_approval_file_row_with_palette(index, row, &palette)
}

fn render_approval_file_row_with_palette(
    index: usize,
    row: &ApprovalFileRow,
    palette: &ThemePalette,
) -> Line<'static> {
    let marker = if row.selected { "> " } else { "  " };
    let style = if row.selected {
        Style::default()
            .fg(palette.selection_fg)
            .bg(palette.selection_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text_primary)
    };
    let mut spans = vec![
        Span::styled(format!("{marker}{} ", index + 1), style),
        Span::styled(row.path.clone(), style),
    ];
    if let Some(action) = &row.action {
        spans.extend([
            Span::raw("  "),
            Span::styled(
                action.clone(),
                approval_file_meta_style_with_palette(action, row.selected, palette),
            ),
        ]);
    }
    if let Some(risk) = &row.risk {
        spans.extend([
            Span::raw(" "),
            Span::styled(
                format!("risk {risk}"),
                approval_file_meta_style_with_palette(risk, row.selected, palette),
            ),
        ]);
    }
    if let Some(diagnostics) = row.diagnostics {
        spans.extend([
            Span::raw("  "),
            Span::styled(
                approval_diagnostics_label(diagnostics),
                approval_diagnostics_style_with_palette(diagnostics, palette),
            ),
        ]);
    }
    Line::from(spans)
}

#[cfg(test)]
fn approval_file_meta_style(label: &str, selected: bool) -> Style {
    let palette = theme::default_palette();
    approval_file_meta_style_with_palette(label, selected, &palette)
}

fn approval_file_meta_style_with_palette(
    label: &str,
    selected: bool,
    palette: &ThemePalette,
) -> Style {
    if selected {
        return Style::default()
            .fg(palette.text_inverse)
            .bg(approval_risk_color_with_palette(label, palette))
            .add_modifier(Modifier::BOLD);
    }
    Style::default()
        .fg(approval_risk_color_with_palette(label, palette))
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
fn approval_diff_status_line(view: &ApprovalModalView) -> Line<'static> {
    let palette = theme::default_palette();
    approval_diff_status_line_with_palette(view, &palette)
}

fn approval_diff_status_line_with_palette(
    view: &ApprovalModalView,
    palette: &ThemePalette,
) -> Line<'static> {
    let hunk_label = if view.hunk_total == 0 {
        "hunk 0/0".to_owned()
    } else {
        format!("hunk {}/{}", view.active_hunk_index, view.hunk_total)
    };
    let target_label = if view.file_rows.is_empty() && view.changed_files.is_empty() {
        "target"
    } else {
        "path"
    };
    let mut spans = vec![
        approval_badge_with_palette("Prev", palette.text_muted, palette),
        Span::raw(" "),
        approval_badge_with_palette("Next", palette.text_muted, palette),
        Span::raw(" "),
        approval_badge_with_palette(
            &approval_diff_view_control_label(view.diff_mode_label),
            palette.accent_info,
            palette,
        ),
        Span::raw(" "),
        approval_badge_with_palette(
            approval_metadata_control_label(view.metadata_collapsed),
            palette.text_muted,
            palette,
        ),
        Span::raw("  "),
        Span::styled(target_label, Style::default().fg(palette.text_muted)),
        Span::raw(" "),
        Span::styled(
            view.diff_label.clone(),
            Style::default().fg(palette.text_primary),
        ),
        Span::raw("  "),
        Span::styled("mode", Style::default().fg(palette.text_muted)),
        Span::raw(" "),
        Span::styled(
            view.diff_mode_label,
            Style::default().fg(palette.accent_info),
        ),
        Span::raw("  "),
        Span::styled(hunk_label, Style::default().fg(palette.diff_hunk_fg)),
    ];
    if let Some(diagnostics) = selected_approval_diagnostics(view) {
        spans.extend([
            Span::raw("  "),
            Span::styled("diagnostics", Style::default().fg(palette.text_muted)),
            Span::raw(" "),
            Span::styled(
                approval_diagnostics_label(diagnostics),
                approval_diagnostics_style_with_palette(diagnostics, palette),
            ),
        ]);
    }
    Line::from(spans)
}

fn selected_approval_diagnostics(view: &ApprovalModalView) -> Option<ApprovalDiagnosticSummary> {
    view.file_rows
        .iter()
        .find(|row| row.selected)
        .and_then(|row| row.diagnostics)
}

fn approval_diagnostics_label(summary: ApprovalDiagnosticSummary) -> String {
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

#[cfg(test)]
fn approval_diagnostics_style(summary: ApprovalDiagnosticSummary) -> Style {
    let palette = theme::default_palette();
    approval_diagnostics_style_with_palette(summary, &palette)
}

fn approval_diagnostics_style_with_palette(
    summary: ApprovalDiagnosticSummary,
    palette: &ThemePalette,
) -> Style {
    if summary.errors > 0 {
        Style::default().fg(palette.risk_high)
    } else if summary.warnings > 0 {
        Style::default().fg(palette.risk_medium)
    } else {
        Style::default().fg(palette.risk_low)
    }
}

#[cfg(test)]
fn approval_risk_color(label: &str) -> Color {
    let palette = theme::default_palette();
    approval_risk_color_with_palette(label, &palette)
}

fn approval_risk_color_with_palette(label: &str, palette: &ThemePalette) -> Color {
    match label {
        "high" | "delete" | "destructive" | "protected" => palette.risk_high,
        "medium" | "update" => palette.risk_medium,
        "low" | "create" => palette.risk_low,
        _ => palette.text_muted,
    }
}

fn count_label(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
fn render_approval_diff_line(
    line: ApprovalDiffLine,
    old_line: Option<usize>,
    new_line: Option<usize>,
    line_number_width: usize,
) -> Line<'static> {
    let palette = theme::default_palette();
    render_approval_diff_line_with_palette(line, old_line, new_line, line_number_width, &palette)
}

fn render_approval_diff_line_with_palette(
    line: ApprovalDiffLine,
    old_line: Option<usize>,
    new_line: Option<usize>,
    line_number_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let (accent, body_style) =
        diff_line_style_for_palette(approval_diff_line_kind(line.kind), palette);
    let marker = if line.active_hunk { ">" } else { "│" };
    let marker_style = if line.active_hunk {
        Style::default()
            .fg(palette.text_inverse)
            .bg(palette.approval_selected_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(accent)
    };
    let body_style = if line.active_hunk {
        body_style.bg(palette.diff_current_hunk_bg)
    } else {
        body_style
    };
    Line::from(vec![
        Span::styled(marker.to_owned(), marker_style),
        Span::styled(
            diff_line_number_text(old_line, line_number_width),
            approval_old_line_number_style_with_palette(old_line, line.kind, palette),
        ),
        Span::styled(" ", Style::default().fg(palette.diff_gutter_fg)),
        Span::styled(
            diff_line_number_text(new_line, line_number_width),
            approval_new_line_number_style_with_palette(new_line, line.kind, palette),
        ),
        Span::styled("│ ", Style::default().fg(palette.diff_gutter_fg)),
        Span::styled(
            if line.text.is_empty() {
                " ".to_owned()
            } else {
                line.text
            },
            body_style,
        ),
    ])
}

#[allow(dead_code)]
fn approval_old_line_number_style(old_line: Option<usize>, kind: ApprovalDiffLineKind) -> Style {
    let palette = theme::default_palette();
    approval_old_line_number_style_with_palette(old_line, kind, &palette)
}

fn approval_old_line_number_style_with_palette(
    old_line: Option<usize>,
    kind: ApprovalDiffLineKind,
    palette: &ThemePalette,
) -> Style {
    if old_line.is_none() {
        return Style::default().fg(palette.diff_gutter_fg);
    }
    let style = Style::default().fg(palette.diff_removed_fg);
    if kind == ApprovalDiffLineKind::Removed {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

#[allow(dead_code)]
fn approval_new_line_number_style(new_line: Option<usize>, kind: ApprovalDiffLineKind) -> Style {
    let palette = theme::default_palette();
    approval_new_line_number_style_with_palette(new_line, kind, &palette)
}

fn approval_new_line_number_style_with_palette(
    new_line: Option<usize>,
    kind: ApprovalDiffLineKind,
    palette: &ThemePalette,
) -> Style {
    if new_line.is_none() {
        return Style::default().fg(palette.diff_gutter_fg);
    }
    let style = Style::default().fg(palette.diff_added_fg);
    if kind == ApprovalDiffLineKind::Added {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn approval_diff_line_kind(kind: ApprovalDiffLineKind) -> DiffLineKind {
    match kind {
        ApprovalDiffLineKind::Header => DiffLineKind::Header,
        ApprovalDiffLineKind::Hunk => DiffLineKind::Hunk,
        ApprovalDiffLineKind::Added => DiffLineKind::Added,
        ApprovalDiffLineKind::Removed => DiffLineKind::Removed,
        ApprovalDiffLineKind::Context => DiffLineKind::Context,
    }
}

#[allow(dead_code)]
fn approval_badge(label: &str, color: Color) -> Span<'static> {
    let palette = theme::default_palette();
    approval_badge_with_palette(label, color, &palette)
}

fn approval_badge_with_palette(label: &str, color: Color, palette: &ThemePalette) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(palette.text_inverse)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/approval_tests.rs"]
mod tests;
