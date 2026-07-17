use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{McpServerConfig, SecretString};

use super::{AppAction, AppState, ModalState};
use crate::runner::McpOAuthUserAction;

#[derive(Debug)]
pub(super) struct McpOAuthModalState {
    pub(super) server_name: String,
    pub(super) status: Option<sigil_runtime::McpOAuthAuthStatus>,
    pub(super) manual_callback: Option<String>,
    pub(super) revocation: Option<sigil_runtime::McpOAuthRevocationOutcome>,
}

impl AppState {
    pub(super) fn open_mcp_oauth_modal(&mut self, server: &McpServerConfig) -> Option<AppAction> {
        let server_name = server.name.clone();
        self.modal_state = Some(ModalState::McpOAuth(McpOAuthModalState {
            server_name: server_name.clone(),
            status: None,
            manual_callback: None,
            revocation: None,
        }));
        self.last_notice = Some(format!("checking MCP {server_name} authentication"));
        Some(AppAction::McpOAuth {
            server_name,
            action: McpOAuthUserAction::Inspect,
        })
    }

    pub(super) fn mcp_oauth_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::McpOAuth(_)))
    }

    pub(super) fn apply_mcp_oauth_status(
        &mut self,
        status: sigil_runtime::McpOAuthAuthStatus,
        revocation: Option<sigil_runtime::McpOAuthRevocationOutcome>,
    ) {
        let server_name = status.server_name.clone();
        let phase = status.phase;
        if let Some(ModalState::McpOAuth(modal)) = self.modal_state.as_mut()
            && modal.server_name == server_name
        {
            modal.status = Some(status.clone());
            modal.revocation = revocation;
            if phase != sigil_runtime::McpOAuthAuthPhase::AwaitingCallback {
                modal.manual_callback = None;
            }
        }
        let runtime_status = match phase {
            sigil_runtime::McpOAuthAuthPhase::AuthenticationRequired
            | sigil_runtime::McpOAuthAuthPhase::Cancelled
            | sigil_runtime::McpOAuthAuthPhase::RevokedLocallyRetained => {
                super::McpServerRuntimeStatus::AuthenticationRequired
            }
            sigil_runtime::McpOAuthAuthPhase::Discovering
            | sigil_runtime::McpOAuthAuthPhase::AwaitingCallback
            | sigil_runtime::McpOAuthAuthPhase::ExchangingCode => {
                super::McpServerRuntimeStatus::Activating
            }
            sigil_runtime::McpOAuthAuthPhase::SignedIn
            | sigil_runtime::McpOAuthAuthPhase::Refreshing
            | sigil_runtime::McpOAuthAuthPhase::Revoking => {
                super::McpServerRuntimeStatus::Refreshing
            }
            sigil_runtime::McpOAuthAuthPhase::Failed => super::McpServerRuntimeStatus::Failed {
                message: status
                    .error
                    .map(|error| format!("OAuth {error:?}"))
                    .unwrap_or_else(|| "OAuth failed".to_owned()),
            },
            sigil_runtime::McpOAuthAuthPhase::NotConfigured => {
                super::McpServerRuntimeStatus::Deferred
            }
        };
        self.runtime
            .mcp_server_statuses
            .insert(server_name.clone(), runtime_status);
        self.push_event("mcp:oauth", format!("server={server_name} phase={phase:?}"));
    }

    pub(super) fn handle_mcp_oauth_modal_key_event(&mut self, key: KeyEvent) -> Option<AppAction> {
        let Some(ModalState::McpOAuth(modal)) = self.modal_state.as_mut() else {
            return None;
        };
        if let Some(buffer) = modal.manual_callback.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    modal.manual_callback = None;
                    self.last_notice = Some("manual callback entry cancelled".to_owned());
                    return None;
                }
                KeyCode::Enter if !buffer.trim().is_empty() => {
                    let callback = SecretString::new(std::mem::take(buffer));
                    modal.manual_callback = None;
                    if let Some(status) = modal.status.take() {
                        modal.status = Some(
                            status.with_phase(sigil_runtime::McpOAuthAuthPhase::ExchangingCode),
                        );
                    }
                    return Some(AppAction::McpOAuth {
                        server_name: modal.server_name.clone(),
                        action: McpOAuthUserAction::ManualCallback(callback),
                    });
                }
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    buffer.pop();
                    return None;
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    if buffer.len() < 8 * 1024 && !character.is_control() {
                        buffer.push(character);
                    }
                    return None;
                }
                _ => return None,
            }
        }

        let phase = modal.status.as_ref().map(|status| status.phase);
        match key.code {
            KeyCode::Esc => {
                if matches!(
                    phase,
                    Some(
                        sigil_runtime::McpOAuthAuthPhase::Discovering
                            | sigil_runtime::McpOAuthAuthPhase::AwaitingCallback
                            | sigil_runtime::McpOAuthAuthPhase::ExchangingCode
                    )
                ) {
                    if let Some(status) = modal.status.take() {
                        modal.status = Some(status.cancelled());
                    }
                    return Some(AppAction::McpOAuth {
                        server_name: modal.server_name.clone(),
                        action: McpOAuthUserAction::Cancel,
                    });
                }
                self.modal_state = None;
                None
            }
            KeyCode::Enter => match phase {
                Some(
                    sigil_runtime::McpOAuthAuthPhase::AuthenticationRequired
                    | sigil_runtime::McpOAuthAuthPhase::Failed
                    | sigil_runtime::McpOAuthAuthPhase::Cancelled,
                ) => Some(AppAction::McpOAuth {
                    server_name: modal.server_name.clone(),
                    action: McpOAuthUserAction::SignIn,
                }),
                Some(sigil_runtime::McpOAuthAuthPhase::SignedIn) => Some(AppAction::McpOAuth {
                    server_name: modal.server_name.clone(),
                    action: McpOAuthUserAction::Refresh,
                }),
                Some(sigil_runtime::McpOAuthAuthPhase::RevokedLocallyRetained) => {
                    Some(AppAction::McpOAuth {
                        server_name: modal.server_name.clone(),
                        action: McpOAuthUserAction::ClearLocal,
                    })
                }
                _ => None,
            },
            KeyCode::Char('o' | 'O')
                if phase == Some(sigil_runtime::McpOAuthAuthPhase::AwaitingCallback) =>
            {
                modal
                    .status
                    .as_ref()
                    .and_then(sigil_runtime::McpOAuthAuthStatus::authorization_url)
                    .map(|url| AppAction::OpenSecretExternalUrl { url })
            }
            KeyCode::Char('c' | 'C')
                if phase == Some(sigil_runtime::McpOAuthAuthPhase::AwaitingCallback) =>
            {
                modal
                    .status
                    .as_ref()
                    .and_then(sigil_runtime::McpOAuthAuthStatus::authorization_url)
                    .map(|text| AppAction::CopySecretToClipboard { text })
            }
            KeyCode::Char('m' | 'M')
                if phase == Some(sigil_runtime::McpOAuthAuthPhase::AwaitingCallback) =>
            {
                modal.manual_callback = Some(String::new());
                self.last_notice = Some("paste the complete callback URL, then Enter".to_owned());
                None
            }
            KeyCode::Char('r' | 'R')
                if phase == Some(sigil_runtime::McpOAuthAuthPhase::SignedIn) =>
            {
                Some(AppAction::McpOAuth {
                    server_name: modal.server_name.clone(),
                    action: McpOAuthUserAction::Refresh,
                })
            }
            KeyCode::Char('s' | 'S')
                if phase == Some(sigil_runtime::McpOAuthAuthPhase::SignedIn) =>
            {
                Some(AppAction::McpOAuth {
                    server_name: modal.server_name.clone(),
                    action: McpOAuthUserAction::Revoke,
                })
            }
            KeyCode::Char('l' | 'L')
                if phase == Some(sigil_runtime::McpOAuthAuthPhase::RevokedLocallyRetained) =>
            {
                Some(AppAction::McpOAuth {
                    server_name: modal.server_name.clone(),
                    action: McpOAuthUserAction::ClearLocal,
                })
            }
            _ => None,
        }
    }
}

pub(super) fn modal_lines(state: &McpOAuthModalState) -> Vec<String> {
    let Some(status) = state.status.as_ref() else {
        return vec![
            format!("Server: {}", state.server_name),
            "Credential: checking system store".to_owned(),
            "Esc close".to_owned(),
        ];
    };
    let issuer = status.issuer.as_deref().unwrap_or("not discovered");
    let scopes = super::compact_mcp_oauth_scopes(&status.scopes);
    let mut lines = vec![
        format!("Server: {}", status.server_name),
        format!("Resource: {}", status.resource),
        format!("Issuer: {issuer}"),
        format!("Scopes: {scopes}"),
        format!("Credential: {:?}", status.credential),
        format!("State: {:?}", status.phase),
    ];
    if let Some(error) = status.error {
        lines.push(format!("Last error: {error:?}"));
    }
    if let Some(revocation) = state.revocation {
        lines.push(format!("Remote revoke: {revocation:?}"));
    }
    lines.push(String::new());
    if let Some(buffer) = state.manual_callback.as_ref() {
        lines.push("Paste complete callback URL; value stays transient".to_owned());
        lines.push(format!(
            "Callback URL: {}|",
            "•".repeat(buffer.chars().count())
        ));
        lines.push("Enter submit · Esc cancel entry".to_owned());
    } else {
        lines.push(match status.phase {
            sigil_runtime::McpOAuthAuthPhase::AuthenticationRequired
            | sigil_runtime::McpOAuthAuthPhase::Failed
            | sigil_runtime::McpOAuthAuthPhase::Cancelled => "Enter sign in · Esc close".to_owned(),
            sigil_runtime::McpOAuthAuthPhase::AwaitingCallback => {
                "O open browser · C copy URL · M manual callback · Esc cancel".to_owned()
            }
            sigil_runtime::McpOAuthAuthPhase::SignedIn => {
                "R/Enter refresh · S sign out (remote revoke first) · Esc close".to_owned()
            }
            sigil_runtime::McpOAuthAuthPhase::RevokedLocallyRetained => {
                "L/Enter clear local credential · Esc keep local credential".to_owned()
            }
            sigil_runtime::McpOAuthAuthPhase::NotConfigured => "Esc close".to_owned(),
            _ => "Working · Esc cancel".to_owned(),
        });
    }
    lines
}
