use super::*;

pub(super) fn render_section(app: &AppState, lines: &mut Vec<String>, config_state: &ConfigState) {
    let paths = &app.sigil_paths;
    lines.push("[roots]".to_owned());
    lines.push(render_config_readonly_row(
        "State root",
        &paths.state_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Cache root",
        &paths.cache_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Workspace state",
        &paths.workspace_state_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Workspace cache",
        &paths.workspace_cache_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Project assets",
        &paths.project_assets_root.display().to_string(),
    ));
    lines.push(String::new());
    lines.push("[files]".to_owned());
    lines.push(render_config_readonly_row(
        "Session logs",
        &paths.session_log_dir.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Input history",
        &paths.input_history_file.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Artifacts",
        &paths.artifacts_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Changesets",
        &paths.changesets_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Terminal tasks",
        &paths.terminal_tasks_root.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Scratch",
        &paths.scratch_root.display().to_string(),
    ));
    lines.push(String::new());
    lines.push("[artifact retention]".to_owned());
    lines.extend(render_mutation_artifact_retention_summary(
        config_state,
        &app.runtime.mutation_artifact_retention_preview,
    ));
    lines.push(String::new());
    lines.push("[details]".to_owned());
    lines.push(render_config_hint_row(
        "read-only; set [storage] roots or SIGIL_STATE_HOME/SIGIL_CACHE_HOME to override",
    ));
    lines.push(render_config_hint_row(
        "footer clean records lifecycle events; artifact details are audit/debug",
    ));
}
