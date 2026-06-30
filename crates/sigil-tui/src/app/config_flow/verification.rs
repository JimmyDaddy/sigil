use super::*;

pub(super) fn render_scope_summary(config_state: &ConfigState) -> Vec<String> {
    let verification = &config_state.draft.base_root_config.verification;
    let scope = verification.scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    vec![
        render_config_readonly_row(
            "Profile",
            &format!(
                "{} ({})",
                verification.scope_profile.as_str(),
                verification.scope_profile.summary()
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
                verification.extra_scope_excludes.len(),
                verification.generated_roots.len()
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
