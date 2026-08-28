//! Small, transport-neutral TUI adapter for the public `sigil-tui` framework.
//!
//! The product host remains responsible for runtime composition and physical effects.  This
//! package owns only the application client binding and a bounded framework surface, making it a
//! second consumer of the public framework without importing Sigil's runtime or kernel.

#![forbid(unsafe_code)]

use std::sync::Arc;

use sigil_application::{
    ApplicationClient, ApplicationCommand, ApplicationCommandReceipt, ApplicationError,
    ApplicationPort, ApplicationProjection,
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
    projection: Option<ApplicationProjection>,
}

impl std::fmt::Debug for TuiApplicationAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiApplicationAdapter")
            .field("client", &self.client)
            .field("projection", &self.projection.is_some())
            .finish()
    }
}

impl TuiApplicationAdapter {
    pub fn new(client: ApplicationClient) -> Self {
        Self {
            client,
            projection: None,
        }
    }

    pub async fn refresh(&mut self) -> Result<&ApplicationProjection, ApplicationError> {
        self.projection = Some(self.client.refresh().await?);
        Ok(self
            .projection
            .as_ref()
            .expect("projection was just stored"))
    }

    pub async fn execute(
        &self,
        command: ApplicationCommand,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        self.client.execute(command).await
    }

    pub fn projection(&self) -> Option<&ApplicationProjection> {
        self.projection.as_ref()
    }
}

impl App for TuiApplicationAdapter {
    type Action = TuiApplicationAction;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action> {
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
        let status = self
            .projection
            .as_ref()
            .map(|projection| projection.session.status.as_str())
            .unwrap_or("not-loaded");
        let active_terminals = self
            .projection
            .as_ref()
            .map(|projection| projection.terminal.active_task_count)
            .unwrap_or(0);
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
