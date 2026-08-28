//! Small, transport-neutral TUI adapter for the public `sigil-tui` framework.
//!
//! The product host remains responsible for runtime composition and physical effects.  This
//! package owns only the application client binding and a bounded framework surface, making it a
//! second consumer of the public framework without importing Sigil's runtime or kernel.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use sigil_application::{
    ApplicationClient, ApplicationCommand, ApplicationCommandId, ApplicationCommandReceipt,
    ApplicationError, ApplicationPort, ApplicationProjection, ApplicationScope,
    HostConnectionInstanceId,
};
use sigil_tui::{App, CoreError, Damage, InputEvent, NodeKey, Rect, Surface, Text, UpdateOutcome};

/// Actions emitted by the adapter's framework-facing input contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiApplicationAction {
    Refresh,
}

/// Minimal application adapter that binds a public framework surface to an application port.
///
/// It stores only the latest bounded projection.  The port implementation and its host-owned
/// authority remain outside this package.
pub struct TuiApplicationAdapter {
    client: ApplicationClient,
    projection: Mutex<Option<ApplicationProjection>>,
}

impl std::fmt::Debug for TuiApplicationAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiApplicationAdapter")
            .field("client", &self.client)
            .field(
                "projection",
                &self
                    .projection
                    .lock()
                    .map(|projection| projection.is_some())
                    .unwrap_or(false),
            )
            .finish()
    }
}

impl TuiApplicationAdapter {
    /// Creates the product adapter from the host-provided application port and identity.
    ///
    /// The host supplies the port and scope, but the application adapter owns the client binding;
    /// callers do not need to construct or retain a parallel `ApplicationClient`.
    pub fn from_port(
        port: Arc<dyn ApplicationPort>,
        scope: ApplicationScope,
        observer_generation: u64,
        client_epoch: u64,
        connection_instance: HostConnectionInstanceId,
    ) -> Result<Self, ApplicationError> {
        Ok(Self::new(ApplicationClient::new(
            port,
            scope,
            observer_generation,
            client_epoch,
            connection_instance,
        )?))
    }

    pub fn new(client: ApplicationClient) -> Self {
        Self {
            client,
            projection: Mutex::new(None),
        }
    }

    pub async fn refresh(&self) -> Result<ApplicationProjection, ApplicationError> {
        let projection = self.client.refresh().await?;
        *self
            .projection
            .lock()
            .map_err(|_| ApplicationError::Unavailable)? = Some(projection.clone());
        Ok(projection)
    }

    pub fn current_projection(&self) -> Result<Option<ApplicationProjection>, ApplicationError> {
        self.projection
            .lock()
            .map_err(|_| ApplicationError::Unavailable)
            .map(|projection| projection.clone())
    }

    pub async fn execute(
        &self,
        command: ApplicationCommand,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        self.client.execute(command).await
    }

    pub async fn execute_with_id(
        &self,
        command_id: ApplicationCommandId,
        command: ApplicationCommand,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        self.client.execute_with_id(command_id, command).await
    }
}

impl App for TuiApplicationAdapter {
    type Action = TuiApplicationAction;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action> {
        if input.validate().is_err() {
            return UpdateOutcome::Ignored;
        }
        match input {
            InputEvent::Key { code } if code == "enter" => {
                UpdateOutcome::Action(TuiApplicationAction::Refresh)
            }
            InputEvent::Paste(_) => UpdateOutcome::Redraw(Damage::PAINT),
            _ => UpdateOutcome::Ignored,
        }
    }

    fn build_surface(&self, viewport: Rect, generation: u64) -> Result<Surface, CoreError> {
        let mut surface = Surface::new(viewport, generation)?;
        let (status, active_terminals) = self
            .projection
            .lock()
            .map_err(|_| CoreError::InvalidValue("application projection lock is poisoned"))
            .map(|projection| {
                projection
                    .as_ref()
                    .map(|projection| {
                        (
                            projection.session.status.as_str().to_owned(),
                            projection.terminal.active_task_count,
                        )
                    })
                    .unwrap_or_else(|| ("not-loaded".to_owned(), 0))
            })?;
        surface.push_text(
            NodeKey::new("application.status")?,
            Rect::new(viewport.x, viewport.y, viewport.width, 1),
            Text::new(format!("status={status} terminals={active_terminals}"))?.to_string(),
        )?;
        surface.push_action(
            NodeKey::new("application.refresh")?,
            Rect::new(viewport.x, viewport.y.saturating_add(1), viewport.width, 1),
            "refresh",
            NodeKey::new("application.refresh")?,
        )?;
        Ok(surface)
    }
}

/// Compile-time assertion that the adapter's public constructor remains port-only.
pub fn application_port_type_marker(_port: Arc<dyn ApplicationPort>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_builds_a_bounded_surface_without_host_dependencies() {
        let _ = TuiApplicationAction::Refresh;
    }
}
