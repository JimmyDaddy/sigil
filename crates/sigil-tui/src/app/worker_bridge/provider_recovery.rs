use anyhow::Result;
use sigil_kernel::{PublicProviderTurnRecoveryActionV1, PublicProviderTurnRecoveryViewV1};

use super::super::AppState;

impl AppState {
    fn actionable_authentication_recovery(&self) -> Option<&PublicProviderTurnRecoveryViewV1> {
        self.runtime.provider_turn_recovery.as_ref().filter(|view| {
            view.reason_code.as_deref() == Some("provider_authentication_required")
                && view.user_attention_required
                && view
                    .available_actions
                    .contains(&PublicProviderTurnRecoveryActionV1::UpdateConnection)
        })
    }

    pub(in crate::app) fn open_recovery_connection_settings(&mut self) -> Result<()> {
        let Some(view) = self.actionable_authentication_recovery() else {
            self.open_config_panel();
            return Ok(());
        };
        let failed_model_ref = view.failed_model_ref.clone();
        if let Some(model_ref) = failed_model_ref {
            self.focus_connection_credential(&model_ref.connection_id)?;
        } else {
            self.focus_connection_list();
        }
        Ok(())
    }
}
