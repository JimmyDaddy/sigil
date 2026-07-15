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
    lines.push(render_config_readonly_row(
        "Workspace skills",
        &paths.workspace_skills_dir.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Workspace commands",
        &paths.workspace_commands_dir.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Workspace agents",
        &paths.workspace_agents_dir.display().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Workspace plugins",
        &paths.workspace_plugins_dir.display().to_string(),
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
    lines.push("[session retention]".to_owned());
    lines.extend(render_session_retention_summary(app, config_state));
    lines.push(String::new());
    lines.push("[details]".to_owned());
    lines.push(render_config_hint_row(
        "read-only; state/cache roots can be overridden, project assets are fixed under workspace .sigil",
    ));
    lines.push(render_config_hint_row(
        "footer clean records lifecycle events; artifact details are audit/debug",
    ));
    lines.push(render_config_hint_row(
        "footer sessions requires explicit review; ordinary runs never apply session retention",
    ));
}

fn render_session_retention_summary(app: &AppState, config_state: &ConfigState) -> Vec<String> {
    let retention = &config_state.draft.base_root_config.session.retention;
    let mut lines = vec![
        render_config_readonly_row(
            "Max sessions",
            &optional_count_summary(retention.max_sessions),
        ),
        render_config_readonly_row("Max bytes", &optional_bytes_summary(retention.max_bytes)),
        render_config_readonly_row(
            "Expire older than",
            &optional_duration_ms_summary(retention.expire_older_than_ms),
        ),
    ];
    match &app.runtime.session_retention_preview {
        SessionRetentionMaintenancePreview::Pending { .. } => {
            lines.push(render_config_readonly_row("Preview", "loading"));
        }
        SessionRetentionMaintenancePreview::Ready { preview } => {
            lines.push(render_config_readonly_row(
                "Current sessions",
                &format!(
                    "{} ({})",
                    preview.total_ready_sessions,
                    optional_bytes_summary(Some(preview.total_ready_bytes))
                ),
            ));
            lines.push(render_config_readonly_row(
                "Cleanup preview",
                &format!(
                    "delete {}, release {}",
                    preview.candidates.len(),
                    optional_bytes_summary(Some(preview.selected_bytes))
                ),
            ));
            lines.push(render_config_readonly_row(
                "Protected",
                &format!(
                    "{} protected, {} pinned, {} ineligible",
                    preview.protected_sessions,
                    preview.pinned_sessions,
                    preview.ineligible_sessions
                ),
            ));
            lines.push(render_config_readonly_row(
                "Constraints",
                if preview.constraints_satisfied {
                    "satisfied"
                } else {
                    "not fully satisfiable"
                },
            ));
        }
        SessionRetentionMaintenancePreview::Unavailable { error } => {
            lines.push(render_config_readonly_row("Preview", "unavailable"));
            lines.push(render_config_hint_row(&truncate_config_detail(error, 72)));
        }
    }
    lines
}
