use std::path::Path;

use sigil_kernel::{PublicRouteRecoveryAction as Action, PublicRouteRecoveryCode as Code};

use super::WorkerMessage;

pub(crate) fn worker_session_route_recovery_message(
    error: &anyhow::Error,
    session_log_path: &Path,
) -> Option<WorkerMessage> {
    if let Some(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::Busy {
            observed_generation,
        },
    ) = error.downcast_ref::<
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError,
    >() {
        return Some(WorkerMessage::SessionRouteRecoveryRequired {
            code: Code::SessionAlreadyActive,
            actions: vec![
                Action::RetrySessionAttach,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            recovery_binding: sigil_runtime::interactive_session_attachment::session_attachment_path_recovery_binding(
                session_log_path,
                observed_generation,
            ),
            retryable: true,
            target_session: None,
        });
    }
    if let Some(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::StaleRecoveryBinding {
            recovery_binding,
        },
    ) = error.downcast_ref::<
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError,
    >() {
        return Some(WorkerMessage::SessionRouteRecoveryRequired {
            code: Code::SessionAlreadyActive,
            actions: vec![
                Action::RetrySessionAttach,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            recovery_binding: recovery_binding.clone(),
            retryable: true,
            target_session: None,
        });
    }

    if matches!(
        error.downcast_ref::<sigil_runtime::provider_connections::ResolvedRouteError>(),
        Some(sigil_runtime::provider_connections::ResolvedRouteError::NotConfigured)
    ) {
        return Some(WorkerMessage::SessionRouteRecoveryRequired {
            code: Code::ModelRouteNotConfigured,
            actions: vec![
                Action::RepairConnection,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            recovery_binding: String::new(),
            retryable: false,
            target_session: None,
        });
    }
    if error
        .downcast_ref::<sigil_runtime::provider_connections::ResolvedRouteError>()
        .is_some()
    {
        return Some(WorkerMessage::SessionRouteRecoveryRequired {
            code: Code::ConnectionConfigInvalid,
            actions: vec![
                Action::RepairConnection,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            recovery_binding: String::new(),
            retryable: false,
            target_session: None,
        });
    }

    let route =
        error.downcast_ref::<sigil_runtime::provider_connections::SessionRouteLoadError>()?;
    let (code, actions, retryable) = match route {
        sigil_runtime::provider_connections::SessionRouteLoadError::ConfirmationRequired {
            ..
        } => (
            Code::SessionRouteConfirmationRequired,
            vec![
                Action::ConfirmCurrentRoute,
                Action::RepairConnection,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            true,
        ),
        sigil_runtime::provider_connections::SessionRouteLoadError::SelectionRequired {
            reason: sigil_runtime::provider_connections::SessionRouteUnavailableReason::ConnectionNotFound,
            ..
        } => (
            Code::SessionRouteSelectionRequired,
            vec![
                Action::RepairConnection,
                Action::SelectReplacement,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            true,
        ),
        sigil_runtime::provider_connections::SessionRouteLoadError::SelectionRequired {
            reason: sigil_runtime::provider_connections::SessionRouteUnavailableReason::ConnectionConfigInvalid,
            ..
        }
        | sigil_runtime::provider_connections::SessionRouteLoadError::SetupRequired {
            reason: sigil_runtime::provider_connections::ModelRouteSetupReason::ConfigurationInvalid,
            ..
        } => (
            Code::ConnectionConfigInvalid,
            vec![
                Action::RepairConnection,
                Action::SelectReplacement,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            false,
        ),
        sigil_runtime::provider_connections::SessionRouteLoadError::SetupRequired {
            reason: sigil_runtime::provider_connections::ModelRouteSetupReason::RouteNotConfigured,
            ..
        } => (
            Code::ModelRouteNotConfigured,
            vec![
                Action::RepairConnection,
                Action::StartNewSession,
                Action::BackToSessionLibrary,
            ],
            false,
        ),
        sigil_runtime::provider_connections::SessionRouteLoadError::WriterBusy { .. } => (
            Code::SessionWriterBusy,
            vec![Action::StartNewSession, Action::BackToSessionLibrary],
            true,
        ),
        sigil_runtime::provider_connections::SessionRouteLoadError::Unavailable(source) => {
            if source
                .downcast_ref::<sigil_runtime::provider_connections::ResolvedRouteError>()
                .is_some()
            {
                (
                    Code::ConnectionConfigInvalid,
                    vec![
                        Action::RepairConnection,
                        Action::StartNewSession,
                        Action::BackToSessionLibrary,
                    ],
                    false,
                )
            } else {
                (
                    Code::SessionStreamInvalid,
                    vec![Action::StartNewSession, Action::BackToSessionLibrary],
                    false,
                )
            }
        }
    };
    Some(WorkerMessage::SessionRouteRecoveryRequired {
        code,
        actions,
        recovery_binding: route.recovery_binding().unwrap_or_default().to_owned(),
        retryable,
        target_session: None,
    })
}
