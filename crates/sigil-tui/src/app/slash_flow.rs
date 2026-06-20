use super::session_flow::session_history_display_label;
use super::{
    AppAction, AppState,
    formatting::{human_file_size, relative_age_label},
};
use crate::slash::{
    EFFORT_SELECTOR_OPTIONS, MODEL_SELECTOR_OPTIONS, ResolvedSlashCommand, SLASH_COMMANDS,
    SlashArgumentOption, SlashCommandSpec, SlashSelectorEntry,
};
use anyhow::{Result, anyhow};
use sigil_kernel::{
    AgentProfileKind, AgentProfileSource, AgentTrustState, SkillDescriptor, SkillRunMode,
    SkillTrustState, default_user_config_dir,
};
use sigil_runtime::{AgentProfileRegistry, ResolvedAgentProfile};

impl AppState {
    fn slash_query(prompt: &str) -> Option<(&str, String)> {
        let trimmed = prompt.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }

        Some(Self::command_token_and_arg(trimmed))
    }

    fn agent_mention_query(prompt: &str) -> Option<&str> {
        let trimmed = prompt.trim_start();
        let query = trimmed.strip_prefix('@')?;
        if query.chars().any(char::is_whitespace) {
            return None;
        }
        Some(query)
    }

    fn command_token_and_arg(prompt: &str) -> (&str, String) {
        if let Some((token, arg)) = prompt.split_once(char::is_whitespace) {
            return (token, arg.trim().to_owned());
        }

        (prompt, String::new())
    }

    fn slash_has_argument_boundary(prompt: &str) -> bool {
        let trimmed = prompt.trim_start();
        trimmed.chars().any(char::is_whitespace)
    }

    fn slash_command_matches(token: &str) -> Vec<&'static SlashCommandSpec> {
        if token == "/" || token.is_empty() {
            return SLASH_COMMANDS.iter().collect();
        }

        SLASH_COMMANDS
            .iter()
            .filter(|spec| {
                spec.canonical.starts_with(token)
                    || spec.aliases.iter().any(|alias| alias.starts_with(token))
            })
            .collect()
    }

    fn exact_slash_command(token: &str) -> Option<&'static SlashCommandSpec> {
        SLASH_COMMANDS
            .iter()
            .find(|spec| spec.canonical == token || spec.aliases.contains(&token))
    }

    fn executable_slash_command(token: &str) -> Option<&'static SlashCommandSpec> {
        Self::exact_slash_command(token)
    }

    fn slash_option_matches(option: &SlashArgumentOption, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }

        option
            .keywords
            .iter()
            .any(|keyword| keyword.starts_with(query))
    }

    fn slash_command_entries(token: &str, arg: &str) -> Vec<SlashSelectorEntry> {
        Self::slash_command_matches(token)
            .into_iter()
            .map(|spec| {
                let aliases = if spec.aliases.is_empty() {
                    String::new()
                } else {
                    format!("  alias: {}", spec.aliases.join(", "))
                };
                let fill = if arg.is_empty() {
                    let suffix = if spec.completes_with_space { " " } else { "" };
                    format!("{}{}", spec.canonical, suffix)
                } else {
                    format!("{} {arg}", spec.canonical)
                };
                SlashSelectorEntry {
                    fill,
                    label: spec.canonical.to_owned(),
                    description: format!("{}{}", spec.description, aliases),
                    resolved: ResolvedSlashCommand {
                        canonical: spec.canonical.to_owned(),
                        arg: arg.to_owned(),
                    },
                }
            })
            .collect()
    }

    fn slash_argument_entries(
        &self,
        spec: &SlashCommandSpec,
        arg: &str,
    ) -> Option<Vec<SlashSelectorEntry>> {
        match spec.canonical {
            "/agent" => Some(self.agent_slash_entries(arg)),
            "/effort" => Some(self.effort_selector_entries(arg)),
            "/model" => Some(self.model_selector_entries(arg)),
            "/resume" => Some(self.resume_selector_entries(arg)),
            _ => None,
        }
    }

    fn effort_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let query = arg.trim().to_ascii_lowercase();
        let current = self.reasoning_effort.as_str();
        let mut options = EFFORT_SELECTOR_OPTIONS
            .iter()
            .copied()
            .filter(|option| Self::slash_option_matches(option, &query))
            .collect::<Vec<_>>();
        options.sort_by_key(|option| option.value != current);

        options
            .into_iter()
            .map(|option| SlashSelectorEntry {
                fill: format!("/effort {}", option.value),
                label: option.label.to_owned(),
                description: format!("{}  {}", option.value, option.description),
                resolved: ResolvedSlashCommand {
                    canonical: "/effort".to_owned(),
                    arg: option.value.to_owned(),
                },
            })
            .collect()
    }

    fn model_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let trimmed = arg.trim();
        let query = trimmed.to_ascii_lowercase();
        let current = self.model_name.as_str();
        let current_is_known = MODEL_SELECTOR_OPTIONS
            .iter()
            .any(|option| option.value == current);
        let mut entries = Vec::new();

        if !current_is_known
            && (query.is_empty() || current.to_ascii_lowercase().starts_with(&query))
        {
            entries.push(SlashSelectorEntry {
                fill: format!("/model {current}"),
                label: "current".to_owned(),
                description: format!("{current}  current custom model"),
                resolved: ResolvedSlashCommand {
                    canonical: "/model".to_owned(),
                    arg: current.to_owned(),
                },
            });
        }

        let mut options = MODEL_SELECTOR_OPTIONS
            .iter()
            .copied()
            .filter(|option| Self::slash_option_matches(option, &query))
            .collect::<Vec<_>>();
        options.sort_by_key(|option| option.value != current);

        entries.extend(options.into_iter().map(|option| SlashSelectorEntry {
            fill: format!("/model {}", option.value),
            label: option.label.to_owned(),
            description: format!("{}  {}", option.value, option.description),
            resolved: ResolvedSlashCommand {
                canonical: "/model".to_owned(),
                arg: option.value.to_owned(),
            },
        }));

        if entries.is_empty() && !trimmed.is_empty() {
            let custom = trimmed.to_owned();
            entries.push(SlashSelectorEntry {
                fill: format!("/model {custom}"),
                label: "custom".to_owned(),
                description: format!("{custom}  use typed model id"),
                resolved: ResolvedSlashCommand {
                    canonical: "/model".to_owned(),
                    arg: custom,
                },
            });
        }

        entries
    }

    fn resume_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let query = arg.trim().to_ascii_lowercase();
        self.resume_candidate_indices()
            .into_iter()
            .enumerate()
            .filter_map(|(candidate_index, entry_index)| {
                let entry = self.session_history.get(entry_index)?;
                let ordinal = candidate_index + 1;
                let label = session_history_display_label(entry);
                let current = entry.path == self.session_log_path;
                let search_text = format!(
                    "{} {} {} {}",
                    ordinal,
                    entry.label.to_ascii_lowercase(),
                    entry.title.clone().unwrap_or_default().to_ascii_lowercase(),
                    entry.path.display().to_string().to_ascii_lowercase()
                );
                let latest_match = query == "latest" && candidate_index == 0;
                let include = query.is_empty() || latest_match || search_text.contains(&query);
                include.then(|| SlashSelectorEntry {
                    fill: format!("/resume {ordinal}"),
                    label: ordinal.to_string(),
                    description: format!(
                        "{}{}  {} · {}",
                        label,
                        if current { "  current" } else { "" },
                        human_file_size(entry.bytes),
                        relative_age_label(entry.modified_epoch_secs)
                    ),
                    resolved: ResolvedSlashCommand {
                        canonical: "/resume".to_owned(),
                        arg: entry.path.display().to_string(),
                    },
                })
            })
            .collect()
    }

    fn user_invocable_skill_descriptors(&self) -> Vec<SkillDescriptor> {
        let Some(root_config) = self.root_config_snapshot() else {
            return Vec::new();
        };
        let user_config_dir = default_user_config_dir().ok();
        sigil_runtime::discover_skill_index_with_user_dir(
            &self.workspace_root,
            user_config_dir.as_deref(),
            &root_config.skills,
        )
        .map(|report| report.snapshot.descriptors)
        .unwrap_or_default()
    }

    fn user_invocable_agent_profiles(&self) -> Vec<ResolvedAgentProfile> {
        let Some(root_config) = self.root_config_snapshot() else {
            return Vec::new();
        };
        AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &self.workspace_root,
            &self.current_session_entries,
        )
        .map(|registry| {
            registry
                .profiles()
                .iter()
                .filter(|profile| profile.effective_enabled())
                .filter(|profile| profile.effective_user_invocation_allowed())
                .filter(|profile| profile.trust_state == AgentTrustState::Trusted)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
    }

    pub(super) fn exact_skill_descriptor(&self, skill_id: &str) -> Option<SkillDescriptor> {
        self.user_invocable_skill_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.id == skill_id)
    }

    pub(super) fn resolve_agent_mention_invocation(
        &self,
        prompt: &str,
    ) -> Result<(String, String)> {
        let trimmed = prompt.trim_start();
        let Some(stripped) = trimmed.strip_prefix('@') else {
            return Err(anyhow!("not an agent mention"));
        };
        if stripped.trim().is_empty() {
            return Err(anyhow!("usage: @agent <prompt>"));
        }
        let (token, arg) = Self::command_token_and_arg(trimmed);
        let profile_id = token
            .strip_prefix('@')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("usage: @agent <prompt>"))?;
        if arg.trim().is_empty() {
            return Err(anyhow!("usage: @{profile_id} <prompt>"));
        }
        let Some(profile) = self
            .user_invocable_agent_profiles()
            .into_iter()
            .find(|profile| profile.profile.id.as_str() == profile_id)
        else {
            return Err(anyhow!("unknown agent {profile_id}"));
        };
        Ok((profile.profile.id.as_str().to_owned(), arg))
    }

    pub(super) fn slash_skill_entries(&self, token: &str, arg: &str) -> Vec<SlashSelectorEntry> {
        let Some(query) = token.strip_prefix('/') else {
            return Vec::new();
        };
        if query.is_empty() && token != "/" {
            return Vec::new();
        }
        self.user_invocable_skill_descriptors()
            .into_iter()
            .filter(slash_skill_is_visible)
            .filter(|descriptor| query.is_empty() || descriptor.id.starts_with(query))
            .map(|descriptor| {
                let fill = if arg.is_empty() {
                    format!("/{}", descriptor.id)
                } else {
                    format!("/{} {arg}", descriptor.id)
                };
                let item_kind = slash_skill_display_kind(&descriptor);
                let description = if descriptor.description.trim().is_empty() {
                    format!("{item_kind} · {}", descriptor.run_as.as_str())
                } else {
                    format!(
                        "{item_kind} · {} · {}",
                        descriptor.run_as.as_str(),
                        descriptor.description.trim()
                    )
                };
                SlashSelectorEntry {
                    fill,
                    label: format!("/{}", descriptor.id),
                    description,
                    resolved: ResolvedSlashCommand {
                        canonical: format!("/{}", descriptor.id),
                        arg: arg.to_owned(),
                    },
                }
            })
            .collect()
    }

    fn agent_mention_entries(&self, query: &str) -> Vec<SlashSelectorEntry> {
        let query = query.to_ascii_lowercase();
        self.user_invocable_agent_profiles()
            .into_iter()
            .filter(|profile| agent_profile_matches_query(profile, &query))
            .map(|profile| {
                let profile_id = profile.profile.id.as_str();
                let description = agent_mention_description(&profile);
                SlashSelectorEntry {
                    fill: format!("@{profile_id} "),
                    label: format!("@{profile_id}"),
                    description,
                    resolved: ResolvedSlashCommand {
                        canonical: "@agent".to_owned(),
                        arg: profile_id.to_owned(),
                    },
                }
            })
            .collect()
    }

    fn slash_selector_entries(&self) -> Vec<SlashSelectorEntry> {
        if !self.slash_selector_context_allows_popup() {
            return Vec::new();
        }
        if let Some(query) = Self::agent_mention_query(&self.input) {
            return self.agent_mention_entries(query);
        }
        let Some((token, arg)) = Self::slash_query(&self.input) else {
            return Vec::new();
        };

        let mut entries = if let Some(spec) = Self::exact_slash_command(token)
            && let Some(entries) = self.slash_argument_entries(spec, &arg)
        {
            entries
        } else {
            let mut entries = Self::slash_command_entries(token, &arg);
            if Self::exact_slash_command(token).is_none() {
                entries.extend(self.slash_skill_entries(token, &arg));
            }
            entries
        };

        self.decorate_pending_mouse_confirmation(&mut entries);
        entries
    }

    fn slash_selector_context_allows_popup(&self) -> bool {
        if Self::agent_mention_query(&self.input).is_some() {
            return true;
        }
        let Some((token, arg)) = Self::slash_query(&self.input) else {
            return false;
        };
        if !Self::slash_has_argument_boundary(&self.input) {
            return true;
        }

        if let Some(spec) = Self::exact_slash_command(token) {
            if spec.canonical == "/agent" && !self.agent_selector_allows_popup(&arg) {
                return false;
            }
            return self.slash_argument_entries(spec, &arg).is_some();
        }

        true
    }

    fn decorate_pending_mouse_confirmation(&self, entries: &mut [SlashSelectorEntry]) {
        let Some(pending) = &self.pending_mouse_slash_confirmation else {
            return;
        };
        for entry in entries {
            if &entry.resolved == pending {
                entry.description = format!(
                    "click again to confirm {}  {}",
                    entry.fill.trim_end(),
                    entry.description
                );
            }
        }
    }

    pub(super) fn selected_slash_entry(&self) -> Option<SlashSelectorEntry> {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            return None;
        }

        let index = self.slash_selector_index.min(rows.len().saturating_sub(1));
        rows.get(index).cloned()
    }

    pub(super) fn resolve_slash_command(&self, prompt: &str) -> Option<ResolvedSlashCommand> {
        let (token, arg) = Self::slash_query(prompt)?;
        if let Some(entry) = self.selected_slash_entry() {
            return Some(entry.resolved);
        }

        if let Some(spec) = Self::executable_slash_command(token) {
            return Some(ResolvedSlashCommand {
                canonical: spec.canonical.to_owned(),
                arg,
            });
        }
        let skill_id = token.strip_prefix('/')?;
        self.exact_skill_descriptor(skill_id)
            .filter(slash_skill_is_resolvable)
            .map(|descriptor| ResolvedSlashCommand {
                canonical: format!("/{}", descriptor.id),
                arg,
            })
    }

    pub(super) fn reset_slash_selector(&mut self) {
        self.slash_selector_index = 0;
        self.pending_mouse_slash_confirmation = None;
        self.refresh_slash_selector_context();
    }

    fn refresh_slash_selector_context(&mut self) {
        let Some((token, _)) = Self::slash_query(&self.input) else {
            return;
        };
        if token == "/resume" {
            self.refresh_session_history();
        }
    }

    pub(super) fn move_slash_selector(&mut self, forward: bool) {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            return;
        }
        self.pending_mouse_slash_confirmation = None;

        if forward {
            self.slash_selector_index = (self.slash_selector_index + 1) % rows.len();
        } else if self.slash_selector_index == 0 {
            self.slash_selector_index = rows.len() - 1;
        } else {
            self.slash_selector_index -= 1;
        }

        if let Some(entry) = rows.get(self.slash_selector_index) {
            self.last_notice = Some(format!("slash selected {}", entry.label));
        }
    }

    pub(super) fn handle_mouse_slash_candidate(
        &mut self,
        index: usize,
    ) -> Result<Option<AppAction>> {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            return Ok(None);
        }

        let selected = index.min(rows.len().saturating_sub(1));
        self.slash_selector_index = selected;
        let Some(entry) = rows.get(selected).cloned() else {
            return Ok(None);
        };

        if entry.fill.ends_with(' ') {
            self.complete_slash_entry(&entry);
            return Ok(None);
        }

        if Self::slash_command_requires_mouse_confirmation(&entry.resolved) {
            if self
                .pending_mouse_slash_confirmation
                .as_ref()
                .is_some_and(|pending| pending == &entry.resolved)
            {
                let prompt = entry.fill.trim_end().to_owned();
                self.record_input_history(prompt.clone());
                self.reset_input_history_navigation();
                return self.execute_slash_command(entry.resolved, prompt);
            }

            self.complete_slash_entry(&entry);
            self.pending_mouse_slash_confirmation = Some(entry.resolved.clone());
            self.last_notice = Some(format!("click again to confirm {}", entry.fill.trim_end()));
            return Ok(None);
        }

        self.pending_mouse_slash_confirmation = None;
        let prompt = entry.fill.trim_end().to_owned();
        self.record_input_history(prompt.clone());
        self.reset_input_history_navigation();
        self.execute_slash_command(entry.resolved, prompt)
    }

    pub(super) fn accept_slash_selector(&mut self) {
        let Some(entry) = self.selected_slash_entry() else {
            return;
        };
        self.complete_slash_entry(&entry);
    }

    fn complete_slash_entry(&mut self, entry: &SlashSelectorEntry) {
        let trimmed = self.input.trim_start();
        let leading_len = self.input.len().saturating_sub(trimmed.len());
        let leading = self.input[..leading_len].to_owned();
        let completed = format!("{leading}{}", entry.fill);
        self.set_input_and_cursor(completed);
        self.last_notice = Some(format!("slash completed to {}", entry.fill.trim_end()));
        self.reset_input_history_navigation();
        self.reset_slash_selector();
    }

    fn slash_command_requires_mouse_confirmation(command: &ResolvedSlashCommand) -> bool {
        matches!(
            command.canonical.as_str(),
            "/compact" | "/model" | "/new" | "/quit" | "/resume"
        )
    }

    pub(super) fn should_accept_slash_selector_on_enter(&self) -> bool {
        let Some(entry) = self.selected_slash_entry() else {
            return false;
        };

        entry.label.starts_with('/') && self.input.trim_start() != entry.fill.trim_end()
            || entry.label.starts_with('@') && self.input.trim_start() != entry.fill.trim_end()
    }

    pub fn has_slash_selector(&self) -> bool {
        self.slash_selector_context_allows_popup()
    }

    pub fn has_agent_mention_selector(&self) -> bool {
        Self::agent_mention_query(&self.input).is_some()
    }

    pub fn slash_selector_selected_index(&self) -> Option<usize> {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            None
        } else {
            Some(self.slash_selector_index.min(rows.len().saturating_sub(1)))
        }
    }

    pub fn slash_selector_rows(&self) -> Vec<(String, String)> {
        self.slash_selector_entries()
            .into_iter()
            .map(|entry| (entry.label, entry.description))
            .collect()
    }

    pub fn slash_selector_empty_message(&self) -> Option<&'static str> {
        if !self.slash_selector_context_allows_popup() {
            return None;
        }
        if Self::agent_mention_query(&self.input).is_some() {
            return self
                .slash_selector_entries()
                .is_empty()
                .then_some("no matching agent");
        }
        let (token, _) = Self::slash_query(&self.input)?;
        if !self.slash_selector_entries().is_empty() {
            return None;
        }

        match Self::exact_slash_command(token).map(|spec| spec.canonical) {
            Some("/agent") => Some("no matching agent"),
            Some("/effort") => Some("pick effort: low | medium | high | max"),
            Some("/resume") if self.session_history.is_empty() => Some("no saved sessions"),
            Some("/resume") => Some("no matching session"),
            _ => Some("no slash match"),
        }
    }

    pub(crate) fn slash_selector_visible_rows(&self) -> u16 {
        if self.has_slash_selector() {
            let title_rows = u16::from(self.slash_selector_title().is_some());
            title_rows.saturating_add(self.slash_selector_rows().len().clamp(1, 8) as u16)
        } else {
            0
        }
    }

    pub fn slash_command_hints(&self) -> Vec<String> {
        let mut hints = self
            .slash_selector_rows()
            .into_iter()
            .map(|(command, description)| format!("{command} - {description}"))
            .collect::<Vec<_>>();
        if hints.is_empty()
            && let Some(message) = self.slash_selector_empty_message()
        {
            hints.push(message.to_owned());
        }
        hints
    }

    pub(crate) fn slash_selector_title(&self) -> Option<&'static str> {
        if Self::agent_mention_query(&self.input).is_some() {
            return Some("Agent");
        }
        let (token, _) = Self::slash_query(&self.input)?;
        match Self::exact_slash_command(token).map(|spec| spec.canonical) {
            Some("/agent") => Some("Agent"),
            Some("/resume") => Some("Resume session"),
            _ => None,
        }
    }
}

fn slash_skill_display_kind(skill: &SkillDescriptor) -> &'static str {
    if matches!(skill.run_as, SkillRunMode::ChildSession) {
        "agent"
    } else {
        "skill"
    }
}

fn slash_skill_is_visible(skill: &SkillDescriptor) -> bool {
    slash_skill_is_resolvable(skill) && skill.trust == SkillTrustState::Trusted
}

fn slash_skill_is_resolvable(skill: &SkillDescriptor) -> bool {
    skill.enabled && skill.user_invocable && matches!(skill.run_as, SkillRunMode::Inline)
}

fn agent_profile_matches_query(profile: &ResolvedAgentProfile, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut search = format!(
        "{} {} {}",
        profile.profile.id.as_str().to_ascii_lowercase(),
        profile.profile.description.to_ascii_lowercase(),
        agent_profile_source_label(&profile.source)
    );
    for nickname in &profile.profile.nickname_candidates {
        search.push(' ');
        search.push_str(&nickname.to_ascii_lowercase());
    }
    search.contains(query)
}

fn agent_mention_description(profile: &ResolvedAgentProfile) -> String {
    let kind = agent_profile_kind_label(profile.profile.kind);
    let source = agent_profile_source_label(&profile.source);
    let description = profile.profile.description.trim();
    if description.is_empty() {
        format!("{kind} · {source}")
    } else {
        format!("{kind} · {source} · {description}")
    }
}

fn agent_profile_kind_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::Primary => "primary",
        AgentProfileKind::Subagent => "subagent",
        AgentProfileKind::System => "system",
        AgentProfileKind::Unknown => "unknown",
    }
}

fn agent_profile_source_label(source: &AgentProfileSource) -> String {
    match source {
        AgentProfileSource::Workspace => "workspace".to_owned(),
        AgentProfileSource::User => "user".to_owned(),
        AgentProfileSource::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
        AgentProfileSource::Compatibility { provider } => format!("compat:{provider}"),
        AgentProfileSource::System => "system".to_owned(),
        AgentProfileSource::LegacyTask => "legacy_task".to_owned(),
        AgentProfileSource::Unknown => "unknown".to_owned(),
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/slash_flow_detail_tests.rs"]
mod tests;
