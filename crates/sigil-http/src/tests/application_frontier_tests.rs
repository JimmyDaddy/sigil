use super::*;
use sigil_application::{
    ApplicationCommandEnvelope, ApplicationInstanceId, CommandAdmissionContext, ExpectedFrontier,
    McpCommand, WorkspaceScopeId,
};

fn request(sequence: u64) -> Result<ApplicationCommandRequest, ApplicationError> {
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("application")?,
        authenticated_subject: AuthenticatedSubject::new("principal")?,
        workspace: None,
        session: Some(SessionScopeId::new("session")?),
    };
    Ok(ApplicationCommandRequest {
        admission: CommandAdmissionContext::host_bound(
            scope.authenticated_subject.clone(),
            1,
            HostConnectionInstanceId::new(format!("connection-{sequence}"))?,
            scope.clone(),
        )?,
        envelope: ApplicationCommandEnvelope {
            schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
            command_id: ApplicationCommandId::new("same-command")?,
            correlation_id: None,
            expected_frontier: ExpectedFrontier {
                scope,
                writer_generation: 1,
                through_sequence: sequence,
            },
            command: ApplicationCommand::Mcp(McpCommand::Refresh {
                binding: "server".to_owned(),
            }),
        },
    })
}

/// The transport fixture only exposes an already-durable owner context. Persistence, cold
/// reopen and more than 4096 identities are exercised by the managed runtime store tests.
struct OriginalContextPort {
    key: sigil_application::CommandReservationKey,
    context: sigil_application::OriginalCommandContext,
}
impl ApplicationPort for OriginalContextPort {
    fn original_command_context(
        &self,
        key: sigil_application::CommandReservationKey,
    ) -> BoxFuture<
        'static,
        Result<Option<sigil_application::OriginalCommandContext>, ApplicationError>,
    > {
        let result = (key == self.key).then(|| self.context.clone());
        Box::pin(async move { Ok(result) })
    }
    fn open_projection(
        &self,
        _: sigil_application::OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<sigil_application::ProjectionSnapshot, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
    fn page(
        &self,
        _: sigil_application::ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<sigil_application::ProjectionPage, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
    fn cancel_page(
        &self,
        _: PageRequestId,
    ) -> BoxFuture<'static, sigil_application::PageCancellationReceipt> {
        Box::pin(async { sigil_application::PageCancellationReceipt::CancelledBeforeLoad })
    }
    fn acknowledge(
        &self,
        _: sigil_application::ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
    fn execute(
        &self,
        _: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
}

fn client_for(
    original: &ApplicationCommandRequest,
    scope: ApplicationScope,
) -> Result<ApplicationClient, ApplicationError> {
    ApplicationClient::new(
        Arc::new(OriginalContextPort {
            key: original
                .admission
                .reservation_key(&original.envelope.command_id),
            context: sigil_application::OriginalCommandContext {
                expected_frontier: original.envelope.expected_frontier.clone(),
                command_journal: Some(sigil_application::CommandJournalBinding {
                    logical_journal_id: "journal".to_owned(),
                    command_generation: 3,
                }),
            },
        }),
        scope,
        1,
        1,
        HostConnectionInstanceId::new("replacement-connection")?,
    )
}

#[test]
fn rebuilt_http_client_restores_durable_frontier_and_generation_without_changing_payload()
-> Result<(), ApplicationError> {
    let original = request(1)?;
    let mut retry = request(9)?;
    let client = client_for(&original, original.admission.scope.clone())?;
    futures::executor::block_on(client.restore_original_command_context(&mut retry))?;
    assert_eq!(
        retry.envelope.expected_frontier,
        original.envelope.expected_frontier
    );
    assert_eq!(
        retry
            .admission
            .command_journal
            .as_ref()
            .expect("journal")
            .command_generation,
        3
    );
    assert_eq!(
        sigil_application::command_fingerprint(&retry)?,
        sigil_application::command_fingerprint(&original)?
    );
    retry.envelope.command = ApplicationCommand::Mcp(McpCommand::Refresh {
        binding: "changed".to_owned(),
    });
    futures::executor::block_on(client.restore_original_command_context(&mut retry))?;
    assert_ne!(
        sigil_application::command_fingerprint(&retry)?,
        sigil_application::command_fingerprint(&original)?
    );
    Ok(())
}

#[test]
fn durable_command_context_lookup_uses_every_reservation_key_component()
-> Result<(), ApplicationError> {
    let original = request(1)?;
    for component in 0..6 {
        let mut foreign = request(9)?;
        match component {
            0 => {
                foreign.admission.scope.application_instance =
                    ApplicationInstanceId::new("other-application")?
            }
            1 => {
                foreign.admission.principal = AuthenticatedSubject::new("other-principal")?;
                foreign.admission.scope.authenticated_subject = foreign.admission.principal.clone();
            }
            2 => foreign.admission.scope.session = Some(SessionScopeId::new("other-session")?),
            3 => {
                foreign.admission.scope.workspace = Some(WorkspaceScopeId::new("other-workspace")?)
            }
            4 => foreign.admission.client_epoch = 2,
            _ => foreign.envelope.command_id = ApplicationCommandId::new("other-command")?,
        }
        foreign.envelope.expected_frontier.scope = foreign.admission.scope.clone();
        let client = client_for(&original, foreign.admission.scope.clone())?;
        futures::executor::block_on(client.restore_original_command_context(&mut foreign))?;
        assert_eq!(foreign.envelope.expected_frontier.through_sequence, 9);
    }
    let client = client_for(&original, original.admission.scope.clone())?;
    let mut mismatched = request(11)?;
    mismatched.envelope.expected_frontier.scope.session = Some(SessionScopeId::new("foreign")?);
    assert_eq!(
        futures::executor::block_on(client.restore_original_command_context(&mut mismatched)),
        Err(ApplicationError::ScopeMismatch)
    );
    Ok(())
}

#[test]
fn user_input_dispatch_failure_observation_is_exact_and_clears_after_the_invocation()
-> Result<(), ApplicationError> {
    let original = request(1)?;
    let key = original
        .admission
        .reservation_key(&original.envelope.command_id);
    let fingerprint = sigil_application::command_fingerprint(&original)?;
    let slot = Arc::new(std::sync::Mutex::new(Some(UserInputDispatchObservation {
        key,
        fingerprint,
        error: None,
    })));
    let failure = || crate::HttpRegistryError::DriverRejected {
        operation: "User input decision",
        run_id: "session".to_owned(),
        message: "the child run is already active".to_owned(),
    };
    {
        let _guard = DispatchObservationGuard(Arc::clone(&slot));
        for component in 0..8 {
            let mut foreign = original.clone();
            match component {
                0 => {
                    foreign.admission.scope.application_instance =
                        ApplicationInstanceId::new("another-application")?;
                }
                1 => foreign.admission.principal = AuthenticatedSubject::new("another-principal")?,
                2 => {
                    foreign.admission.scope.session = Some(SessionScopeId::new("another-session")?)
                }
                3 => {
                    foreign.admission.scope.workspace =
                        Some(WorkspaceScopeId::new("another-workspace")?);
                }
                4 => foreign.admission.client_epoch = 2,
                5 => foreign.envelope.command_id = ApplicationCommandId::new("another-command")?,
                6 => foreign.envelope.expected_frontier.through_sequence = 99,
                _ => {
                    foreign.envelope.command = ApplicationCommand::Mcp(McpCommand::Refresh {
                        binding: "another-server".to_owned(),
                    });
                }
            }
            let mut current = slot.lock().expect("observation lock");
            let observation = current.as_mut().expect("active invocation");
            observation.record_failure(&foreign, failure())?;
            assert!(observation.error.is_none(), "foreign component {component}");
        }
        slot.lock()
            .expect("observation lock")
            .as_mut()
            .expect("active invocation")
            .record_failure(&original, failure())?;
        let mut current = slot.lock().expect("observation lock");
        assert_eq!(
            current.take().expect("active invocation").error,
            Some(failure())
        );
        assert!(current.take().is_none(), "a diagnostic is consumed once");
        // A call that exits before consuming the diagnostic still cannot leak it to a later
        // command or an older client invocation.
        *current = Some(UserInputDispatchObservation {
            key: original
                .admission
                .reservation_key(&original.envelope.command_id),
            fingerprint: sigil_application::command_fingerprint(&original)?,
            error: Some(failure()),
        });
    }
    assert!(slot.lock().expect("observation lock").is_none());
    Ok(())
}
