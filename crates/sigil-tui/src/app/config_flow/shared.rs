use super::*;

#[cfg(test)]
pub(crate) fn cycle_approval_mode(mode: ApprovalMode) -> ApprovalMode {
    match mode {
        ApprovalMode::Allow => ApprovalMode::Ask,
        ApprovalMode::Ask => ApprovalMode::Deny,
        ApprovalMode::Deny => ApprovalMode::Allow,
    }
}

pub(super) fn cycle_code_intel_startup(startup: CodeIntelStartup) -> CodeIntelStartup {
    match startup {
        CodeIntelStartup::Off => CodeIntelStartup::Lazy,
        CodeIntelStartup::Lazy => CodeIntelStartup::Eager,
        CodeIntelStartup::Eager => CodeIntelStartup::Off,
    }
}

pub(super) fn render_effective_context_window(config_state: &ConfigState) -> String {
    let fallback_tokens = config_state
        .draft
        .compaction_context_window_tokens
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|tokens| *tokens > 0);
    let resolved = resolve_context_window_tokens(
        &config_state.draft.provider_name,
        config_state.draft.provider_model.trim(),
        fallback_tokens,
    );

    match resolved.tokens {
        Some(tokens) if tokens > 0 => format!(
            "{} tokens  source={}",
            format_token_count(tokens as u64),
            config_context_window_source_label(resolved.source)
        ),
        _ => "unknown  source=none".to_owned(),
    }
}

pub(super) fn config_context_window_source_label(source: ContextWindowSource) -> &'static str {
    match source {
        ContextWindowSource::Provider => "provider",
        ContextWindowSource::Config => "fallback",
        ContextWindowSource::None => "none",
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(super) enum AgentPolicyToggle {
    Enabled,
    UserInvocable,
    ModelInvocable,
}

#[cfg(test)]
pub(super) fn policy_override(target: bool, source: bool) -> Option<bool> {
    (target != source).then_some(target)
}

pub(super) fn focus_first_config_footer_action(
    config_state: &mut ConfigState,
) -> ConfigFooterAction {
    let action = ConfigFooterAction::actions_for_section(config_state.selected_section)
        .first()
        .copied()
        .unwrap_or(ConfigFooterAction::Close);
    config_state.focus_footer(action);
    action
}

pub(super) fn config_field_display_label(
    config_state: &ConfigState,
    field: ConfigField,
) -> &'static str {
    if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
        && config_state.selected_agent().is_some()
    {
        return "Agent";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
    {
        return skill_display_title(skill);
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "Agent";
    }
    field.display_label()
}

pub(super) fn config_field_key_label(
    config_state: &ConfigState,
    field: ConfigField,
) -> &'static str {
    if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
        && config_state.selected_agent().is_some()
    {
        return "agent";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
    {
        return skill_display_noun(skill);
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "agent";
    }
    field.label()
}

pub(super) fn config_field_help_text(
    config_state: &ConfigState,
    field: ConfigField,
) -> &'static str {
    if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
        && config_state.selected_agent().is_some()
    {
        return "Selected agent profile. Up/Down moves through agents; footer actions trust or disable it.";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
        && skill_is_agent(skill)
    {
        return "Selected child-session agent. Up/Down moves through agents; footer action uses it.";
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "Selected child-session agent. Up/Down moves through agents; footer action uses it.";
    }
    field.help_text()
}

pub(super) fn pluralize(noun: &'static str, count: usize) -> &'static str {
    match (noun, count) {
        ("skill", 1) => "skill",
        ("agent", 1) => "agent",
        ("skill", _) => "skills",
        ("agent", _) => "agents",
        _ => noun,
    }
}

pub(super) fn push_wrapped_readonly_rows(lines: &mut Vec<String>, label: &str, value: &str) {
    let value = if value.trim().is_empty() {
        "none"
    } else {
        value
    };
    for (index, segment) in chunk_for_review_display(value).into_iter().enumerate() {
        let row_label = if index == 0 {
            label.to_owned()
        } else {
            format!("{label} part {}", index + 1)
        };
        lines.push(render_config_readonly_row(&row_label, &segment));
    }
}

pub(super) fn chunk_for_review_display(value: &str) -> Vec<String> {
    const CHUNK_SIZE: usize = 48;
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= CHUNK_SIZE {
        return vec![value.to_owned()];
    }
    chars
        .chunks(CHUNK_SIZE)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

pub(super) fn short_hash(hash: &str) -> String {
    if hash.trim().is_empty() {
        return "none".to_owned();
    }
    let prefix = hash.chars().take(12).collect::<String>();
    if hash.chars().count() > 12 {
        format!("{prefix}...")
    } else {
        prefix
    }
}

pub(super) fn list_summary(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(",")
    }
}

pub(super) fn path_pattern_summary(patterns: &[String]) -> String {
    if patterns.is_empty() {
        "none".to_owned()
    } else {
        patterns.join(",")
    }
}

pub(super) fn truncate_config_detail(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let prefix = value.chars().take(max_chars - 3).collect::<String>();
    format!("{prefix}...")
}

pub(super) fn bool_summary(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(super) fn render_config_hint_row(text: &str) -> String {
    format!("i {text}")
}

pub(super) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
