use super::*;

pub(super) fn render_scope_summary(config_state: &ConfigState) -> Vec<String> {
    let verification = &config_state.draft.base_root_config.verification;
    let scope = verification.scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    vec![
        render_config_readonly_row(
            "Profile",
            &format!(
                "{} ({})",
                verification.scope.profile.as_str(),
                verification.scope.profile.summary()
            ),
        ),
        render_config_readonly_row("Key excludes", &summarize_scope_excludes(&scope.exclude)),
        render_config_readonly_row(
            "Generated roots",
            &summarize_generated_roots(&scope.generated_roots),
        ),
        render_config_readonly_row(
            "Advanced overrides",
            &format!(
                "{} excludes, {} generated roots",
                verification.scope.extra_excludes.len(),
                verification.scope.generated_roots.len()
            ),
        ),
    ]
}

fn summarize_scope_excludes(excludes: &[String]) -> String {
    let key_patterns = [
        "target/**",
        "node_modules/**",
        "dist/**",
        "coverage/**",
        ".pytest_cache/**",
    ];
    let mut visible = key_patterns
        .iter()
        .filter(|pattern| excludes.iter().any(|exclude| exclude == **pattern))
        .map(|pattern| (*pattern).to_owned())
        .collect::<Vec<_>>();
    if visible.is_empty() {
        visible = excludes.iter().take(5).cloned().collect();
    }
    let hidden_count = excludes.len().saturating_sub(visible.len());
    if hidden_count == 0 {
        visible.join(", ")
    } else {
        format!("{} +{} more", visible.join(", "), hidden_count)
    }
}

fn summarize_generated_roots(roots: &[PathBuf]) -> String {
    if roots.is_empty() {
        return "none".to_owned();
    }
    let visible = roots
        .iter()
        .take(4)
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>();
    let hidden_count = roots.len().saturating_sub(visible.len());
    if hidden_count == 0 {
        visible.join(", ")
    } else {
        format!("{} +{} more", visible.join(", "), hidden_count)
    }
}

pub(super) fn render_trust_summary(app: &AppState, config_state: &ConfigState) -> Vec<String> {
    let (workspace_id, trust, _) = match app.verification_trust_context() {
        Ok(context) => context,
        Err(error) => {
            return vec![
                render_config_readonly_row("Workspace trust", "unknown"),
                render_config_hint_row(&format!(
                    "Verification discovery unavailable: {}",
                    truncate_config_detail(&format!("{error:#}"), 72)
                )),
            ];
        }
    };
    let user_check_count = config_state
        .draft
        .base_root_config
        .verification
        .checks
        .len();
    let mut lines = vec![
        render_config_readonly_row("Workspace", &truncate_config_detail(&workspace_id, 48)),
        render_config_readonly_row("Workspace trust", workspace_trust_label(trust)),
        render_config_readonly_row("User checks", &format!("{user_check_count} configured")),
        render_config_readonly_row(
            "Repo instructions",
            &repo_instruction_trust_summary(
                workspace_instruction_files(&app.workspace_root).len(),
                trust,
            ),
        ),
    ];
    match app.repo_verification_candidates(config_state) {
        Ok(repo_candidates) => {
            lines.push(render_config_readonly_row(
                "Repo checks",
                &repo_verification_candidate_summary(repo_candidates.len(), trust),
            ));
            lines.push(render_config_hint_row(
                "Task status owns run/retry actions; config only sets the long-term policy",
            ));
        }
        Err(error) => {
            lines.push(render_config_readonly_row("Repo checks", "unavailable"));
            lines.push(render_config_hint_row(&format!(
                "Verification discovery failed: {}",
                truncate_config_detail(&format!("{error:#}"), 72)
            )));
        }
    }
    lines
}
