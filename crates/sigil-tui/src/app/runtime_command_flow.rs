use anyhow::Result;
use sigil_runtime::{normalize_provider_model_alias, set_active_provider_model};

use super::{AppAction, AppState, TimelineRole, formatting::parse_reasoning_effort};

impl AppState {
    pub(super) fn set_runtime_reasoning_effort_from_command(
        &mut self,
        argument: &str,
    ) -> Result<Option<AppAction>> {
        let Some(effort) = parse_reasoning_effort(argument) else {
            self.last_notice = Some("usage: /effort <low|medium|high|max>".to_owned());
            self.push_timeline(TimelineRole::Notice, "usage: /effort <low|medium|high|max>");
            return Ok(None);
        };

        self.runtime.reasoning_effort = effort.clone();
        self.last_notice = Some(format!("reasoning effort = {}", effort.as_str()));
        self.push_event("effort", effort.as_str());
        self.push_timeline(
            TimelineRole::Notice,
            format!("reasoning effort -> {}", effort.as_str()),
        );
        Ok(None)
    }

    pub(super) fn set_runtime_model_from_command(
        &mut self,
        argument: &str,
    ) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.last_notice = Some("busy; model locked".to_owned());
            self.push_timeline(TimelineRole::Notice, "busy; switch model after the run");
            return Ok(None);
        }

        let provider_name = self
            .config_snapshot
            .as_ref()
            .map(|config| config.agent.provider.as_str())
            .unwrap_or(self.runtime.provider_name.as_str());
        let Some(model) = normalize_provider_model_alias(provider_name, argument) else {
            self.last_notice = Some("usage: /model <flash|pro|id>".to_owned());
            self.push_timeline(TimelineRole::Notice, "usage: /model <flash|pro|id>");
            return Ok(None);
        };

        if model == self.runtime.model_name {
            self.last_notice = Some(format!("model already active = {model}"));
            self.push_timeline(
                TimelineRole::Notice,
                format!("model already active -> {model}"),
            );
            return Ok(None);
        }

        let Some(root_config) = self.config_snapshot.as_ref() else {
            return Ok(None);
        };

        let mut next_config = root_config.clone();
        set_active_provider_model(&mut next_config, &model)?;

        self.apply_runtime_config_snapshot(&next_config);
        self.reset_for_new_session(
            next_config.agent.provider.clone(),
            model.clone(),
            format!("model -> {model}; started a fresh session"),
        );
        self.schedule_balance_refresh();

        Ok(Some(AppAction::RuntimeConfigUpdated {
            root_config: Box::new(next_config),
        }))
    }
}
