use anyhow::Result;
use sigil_kernel::{ConnectionId, ModelRef};
use sigil_runtime::{
    normalize_provider_model_alias,
    provider_connections::{ConnectionReadiness, resolve_default_model_route, resolve_model_route},
};

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

        let Some(root_config) = self.config_snapshot.clone() else {
            return Ok(None);
        };
        let (_, default_route) =
            resolve_default_model_route(&root_config).map_err(anyhow::Error::new)?;
        let current_connection = self
            .runtime
            .model_route
            .as_ref()
            .map(|route| route.model_ref.connection_id.clone())
            .unwrap_or_else(|| default_route.model_ref.connection_id.clone());
        let trimmed = argument.trim();
        if trimmed.is_empty() {
            self.last_notice = Some("usage: /model <model-id|connection-id/model-id>".to_owned());
            self.push_timeline(
                TimelineRole::Notice,
                "usage: /model <model-id|connection-id/model-id>",
            );
            return Ok(None);
        }
        let model_ref = if let Some((connection_id, model_id)) = trimmed.split_once('/') {
            ModelRef::new(
                ConnectionId::new(connection_id.to_owned())?,
                model_id.to_owned(),
            )?
        } else {
            let provider_name = self.runtime.provider_name.as_str();
            let model_id = normalize_provider_model_alias(provider_name, trimmed)
                .unwrap_or_else(|| trimmed.to_owned());
            ModelRef::new(current_connection, model_id)?
        };
        let (provider_name, route) =
            resolve_model_route(&root_config, &model_ref).map_err(anyhow::Error::new)?;
        let ready = self
            .runtime
            .connection_inventory
            .as_ref()
            .is_some_and(|inventory| {
                inventory
                    .entries
                    .iter()
                    .find(|entry| entry.id == model_ref.connection_id)
                    .is_some_and(|entry| {
                        matches!(
                            entry.readiness,
                            ConnectionReadiness::Ready | ConnectionReadiness::Unverified
                        )
                    })
            });
        if !ready {
            let notice = format!(
                "connection {} is not ready; open /config to repair authentication",
                model_ref.connection_id
            );
            self.last_notice = Some(notice.clone());
            self.push_timeline(TimelineRole::Notice, notice);
            return Ok(None);
        }

        if self.runtime.model_route.as_ref() == Some(&route) {
            self.last_notice = Some(format!(
                "model already active = {}/{}",
                model_ref.connection_id, model_ref.model_id
            ));
            self.push_timeline(
                TimelineRole::Notice,
                format!(
                    "model already active -> {}/{}",
                    model_ref.connection_id, model_ref.model_id
                ),
            );
            return Ok(None);
        }

        self.select_current_session_route_with_trust(
            &root_config,
            provider_name.clone(),
            route.clone(),
        )?;
        self.runtime.provider_name = provider_name;
        self.runtime.model_name = model_ref.model_id.clone();
        self.runtime.model_route = Some(route.clone());
        let notice = format!(
            "route -> {}/{}; continuing current session; saved default unchanged",
            model_ref.connection_id, model_ref.model_id
        );
        self.last_notice = Some(notice.clone());
        self.push_timeline(TimelineRole::Notice, notice);
        self.push_event(
            "model",
            format!("{}/{}", model_ref.connection_id, model_ref.model_id),
        );

        self.schedule_balance_refresh();

        Ok(Some(AppAction::SessionRuntimeRouteUpdated { route }))
    }
}
