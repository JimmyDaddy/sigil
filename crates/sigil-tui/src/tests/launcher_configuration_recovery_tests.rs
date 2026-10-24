use super::*;
use crate::app::{ConfigurationSaveFollowUp, ConfigurationSaveRequest};
use sigil_kernel::{
    PublicProviderTurnRecoveryActionV1, PublicProviderTurnRecoveryPhaseV1,
    PublicProviderTurnRecoveryViewV1,
};
use std::sync::{Arc, Mutex};

fn app_with_authentication_recovery(workspace: &Path) -> AppState {
    let mut app = application_admission_test_app(workspace);
    app.runtime.provider_turn_recovery = Some(PublicProviderTurnRecoveryViewV1 {
        phase: PublicProviderTurnRecoveryPhaseV1::Blocked,
        active_retry_count: 0,
        active_max_retries: 0,
        retry_count: 0,
        max_transport_retries: 0,
        next_retry_unix_ms: None,
        reason_code: Some("provider_authentication_required".to_owned()),
        failed_model_ref: app
            .runtime
            .model_route
            .as_ref()
            .map(|route| route.model_ref.clone()),
        available_actions: vec![PublicProviderTurnRecoveryActionV1::UpdateConnection],
        user_attention_required: true,
    });
    app
}

fn publication(app: &AppState, root_only: bool) -> Arc<ConfigurationSaveRequest> {
    let config = app
        .persisted_config_snapshot()
        .expect("fixture configuration")
        .clone();
    Arc::new(ConfigurationSaveRequest {
        expected: config.clone(),
        next_base: config.clone(),
        config_path: app.config_path.clone(),
        follow_up: ConfigurationSaveFollowUp::RebootRuntime,
        root_only,
        draft_binding: None,
        draft: Mutex::new(None),
        published_root_config: Mutex::new(Some(config)),
        close_after_save: false,
    })
}

#[test]
fn r9_feedback_root_only_publication_keeps_authentication_repair_target() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut app = app_with_authentication_recovery(fixture.path());
    let previous = app.runtime.provider_turn_recovery.clone();
    let request = publication(&app, true);
    super::super::apply_configuration_publication(&mut app, &request)?;
    assert_eq!(app.runtime.provider_turn_recovery, previous);
    Ok(())
}

#[test]
fn r9_feedback_connection_publication_clears_the_previous_authentication_hint() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut app = app_with_authentication_recovery(fixture.path());
    let request = publication(&app, false);
    super::super::apply_configuration_publication(&mut app, &request)?;
    assert!(app.runtime.provider_turn_recovery.is_none());
    Ok(())
}
