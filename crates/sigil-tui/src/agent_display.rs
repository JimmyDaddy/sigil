use sigil_kernel::{AgentRole, normalize_task_agent_display_name};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentDisplayName {
    pub(crate) label: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AgentDisplayNameInput<'a> {
    pub(crate) display_name: Option<&'a str>,
    pub(crate) objective: Option<&'a str>,
    pub(crate) profile_id: Option<&'a str>,
    pub(crate) thread_id: Option<&'a str>,
    pub(crate) role: Option<AgentRole>,
    pub(crate) ordinal: Option<usize>,
}

pub(crate) fn resolve_agent_display_name(input: AgentDisplayNameInput<'_>) -> AgentDisplayName {
    if let Some(label) = normalized_explicit_label(input.display_name) {
        return AgentDisplayName { label };
    }
    if let Some(objective) = input.objective
        && !objective.trim().is_empty()
        && let Ok(label) = normalize_task_agent_display_name(objective)
    {
        return AgentDisplayName { label };
    }
    if let Some(profile_id) = input.profile_id {
        let label = profile_label(profile_id);
        if !label.trim().is_empty() {
            return AgentDisplayName { label };
        }
    }
    if let Some(role) = input.role {
        return AgentDisplayName {
            label: fallback_role_label(role, input.ordinal),
        };
    }
    if let Some(thread_id) = input.thread_id {
        let label = thread_id.trim();
        if !label.is_empty() {
            return AgentDisplayName {
                label: label.to_owned(),
            };
        }
    }
    AgentDisplayName {
        label: input
            .ordinal
            .map(|ordinal| format!("agent {ordinal}"))
            .unwrap_or_else(|| "agent".to_owned()),
    }
}

fn normalized_explicit_label(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    (!value.is_empty()).then(|| value.to_owned())
}

fn profile_label(profile_id: &str) -> String {
    profile_id.trim().replace(['_', '-'], " ")
}

fn fallback_role_label(role: AgentRole, ordinal: Option<usize>) -> String {
    let base = match role {
        AgentRole::Planner => "planner",
        AgentRole::Executor => "executor",
        AgentRole::SubagentRead => "read",
        AgentRole::SubagentWrite => "write",
    };
    match ordinal {
        Some(ordinal) if base == "agent" => format!("agent {ordinal}"),
        Some(ordinal) => format!("{base} {ordinal}"),
        None => base.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_display_name_is_preserved() {
        let name = resolve_agent_display_name(AgentDisplayNameInput {
            display_name: Some("agent explore"),
            ..AgentDisplayNameInput::default()
        });

        assert_eq!(name.label, "agent explore");
    }

    #[test]
    fn profile_is_preferred_before_thread_id() {
        let name = resolve_agent_display_name(AgentDisplayNameInput {
            profile_id: Some("kernel-explorer"),
            thread_id: Some("agent_chat_123"),
            ..AgentDisplayNameInput::default()
        });

        assert_eq!(name.label, "kernel explorer");
    }

    #[test]
    fn role_thread_and_ordinal_fallbacks_are_human_readable() {
        let blank_display = resolve_agent_display_name(AgentDisplayNameInput {
            display_name: Some("   "),
            role: Some(AgentRole::Planner),
            ordinal: Some(2),
            ..AgentDisplayNameInput::default()
        });
        assert_eq!(blank_display.label, "planner 2");

        let executor = resolve_agent_display_name(AgentDisplayNameInput {
            role: Some(AgentRole::Executor),
            ordinal: Some(3),
            ..AgentDisplayNameInput::default()
        });
        assert_eq!(executor.label, "executor 3");

        let thread = resolve_agent_display_name(AgentDisplayNameInput {
            thread_id: Some(" agent_chat_123 "),
            ..AgentDisplayNameInput::default()
        });
        assert_eq!(thread.label, "agent_chat_123");

        let ordinal = resolve_agent_display_name(AgentDisplayNameInput {
            ordinal: Some(4),
            ..AgentDisplayNameInput::default()
        });
        assert_eq!(ordinal.label, "agent 4");
    }
}
