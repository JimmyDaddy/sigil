use super::*;

mod agent;
mod code_intel;
mod diff;
mod file;
mod generic;
mod json;
mod search;
mod shell;
mod terminal;

pub(super) use agent::*;
pub(super) use code_intel::*;
pub(super) use diff::*;
pub(super) use file::*;
pub(super) use generic::*;
pub(super) use json::*;
pub(super) use search::*;
pub(super) use shell::*;
pub(super) use terminal::*;

pub(super) fn tool_has_preview(summary: &ToolCardRender) -> bool {
    terminal_task_tool(summary)
        || !summary.preview_lines.is_empty()
        || summary.preview_value.is_some()
        || summary.diff.is_some()
}

#[cfg(test)]
pub(super) fn render_tool_collapsed_preview_body(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_tool_collapsed_preview_body_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

pub(super) fn render_tool_collapsed_preview_body_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let body = render_tool_preview_body_with_palette(
        summary,
        accent,
        max_content_width,
        syntax_theme,
        palette,
    );
    if body.len() <= COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS {
        return body;
    }

    let visible_rows = COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS;
    let hidden_rows = collapsed_tool_hidden_rows(summary, body.len(), visible_rows);
    let mut lines = body.into_iter().take(visible_rows).collect::<Vec<_>>();
    lines.extend(render_tool_hidden_tail(accent, hidden_rows, palette));
    lines
}

pub(super) fn collapsed_tool_hidden_rows(
    summary: &ToolCardRender,
    body_rows: usize,
    visible_rows: usize,
) -> usize {
    let omitted_rows = body_rows.saturating_sub(visible_rows);
    if summary.hidden_lines == 0 {
        return omitted_rows;
    }
    summary
        .hidden_lines
        .saturating_add(omitted_rows.saturating_sub(1))
}

#[cfg(test)]
pub(super) fn render_tool_preview_body(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_tool_preview_body_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

pub(super) fn render_tool_preview_body_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if (tool_name_matches(&summary.tool_name, "ls")
        || tool_name_matches(&summary.tool_name, "glob"))
        && let Some(lines) = render_path_list_preview_with_palette(summary, accent, palette)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "grep")
        && let Some(lines) = render_grep_preview_with_palette(summary, accent, palette)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        return render_bash_preview_with_palette(summary, accent, palette);
    }
    if terminal_task_tool(summary) {
        return render_terminal_task_preview_with_palette(summary, accent, palette);
    }
    if agent_tool(summary) {
        return render_agent_tool_preview(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    if file_change_tool(summary)
        && let Some(lines) = render_file_change_preview_with_palette(summary, accent, palette)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return render_read_file_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    if code_intelligence_tool(summary) {
        return render_code_intelligence_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    render_generic_tool_preview_with_palette(
        summary,
        accent,
        max_content_width,
        syntax_theme,
        palette,
    )
}
