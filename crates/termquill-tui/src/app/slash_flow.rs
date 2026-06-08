use super::session_flow::session_history_display_label;
use super::*;
use crate::slash::{
    EFFORT_SELECTOR_OPTIONS, MODEL_SELECTOR_OPTIONS, ResolvedSlashCommand, SLASH_COMMANDS,
    SlashArgumentOption, SlashCommandSpec, SlashSelectorEntry,
};

impl AppState {
    fn slash_query(prompt: &str) -> Option<(&str, String)> {
        let trimmed = prompt.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }

        Some(Self::command_token_and_arg(trimmed))
    }

    fn command_token_and_arg(prompt: &str) -> (&str, String) {
        if let Some((token, arg)) = prompt.split_once(char::is_whitespace) {
            return (token, arg.trim().to_owned());
        }

        (prompt, String::new())
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
                        canonical: spec.canonical,
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
                    canonical: "/effort",
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
                    canonical: "/model",
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
                canonical: "/model",
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
                    canonical: "/model",
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
                        canonical: "/resume",
                        arg: entry.path.display().to_string(),
                    },
                })
            })
            .collect()
    }

    fn slash_selector_entries(&self) -> Vec<SlashSelectorEntry> {
        let Some((token, arg)) = Self::slash_query(&self.input) else {
            return Vec::new();
        };

        if let Some(spec) = Self::exact_slash_command(token)
            && let Some(entries) = self.slash_argument_entries(spec, &arg)
        {
            return entries;
        }

        Self::slash_command_entries(token, &arg)
    }

    fn selected_slash_entry(&self) -> Option<SlashSelectorEntry> {
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

        Self::executable_slash_command(token).map(|spec| ResolvedSlashCommand {
            canonical: spec.canonical,
            arg,
        })
    }

    pub(super) fn reset_slash_selector(&mut self) {
        self.slash_selector_index = 0;
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

    pub(super) fn accept_slash_selector(&mut self) {
        let Some(entry) = self.selected_slash_entry() else {
            return;
        };
        let trimmed = self.input.trim_start();
        let leading_len = self.input.len().saturating_sub(trimmed.len());
        let leading = self.input[..leading_len].to_owned();
        let completed = format!("{leading}{}", entry.fill);
        self.set_input_and_cursor(completed);
        self.last_notice = Some(format!("slash completed to {}", entry.fill.trim_end()));
        self.reset_input_history_navigation();
        self.reset_slash_selector();
    }

    pub(super) fn should_accept_slash_selector_on_enter(&self) -> bool {
        let Some(entry) = self.selected_slash_entry() else {
            return false;
        };

        entry.label.starts_with('/') && self.input.trim_start() != entry.fill.trim_end()
    }

    pub fn has_slash_selector(&self) -> bool {
        Self::slash_query(&self.input).is_some()
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
        let (token, _) = Self::slash_query(&self.input)?;
        if !self.slash_selector_entries().is_empty() {
            return None;
        }

        match Self::exact_slash_command(token).map(|spec| spec.canonical) {
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
        let (token, _) = Self::slash_query(&self.input)?;
        match Self::exact_slash_command(token).map(|spec| spec.canonical) {
            Some("/resume") => Some("Resume session"),
            _ => None,
        }
    }
}
