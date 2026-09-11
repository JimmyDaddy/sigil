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

#[test]
fn http_command_frontier_replay_preserves_fingerprint_but_not_changed_payload()
-> Result<(), ApplicationError> {
    let retained = HttpCommandFrontiers::default();
    let mut first = request(1)?;
    retained.retain_original(&mut first)?;
    let mut retry = request(9)?;
    let current_admission = retry.admission.clone();
    retained.retain_original(&mut retry)?;
    assert_eq!(
        retry.envelope.expected_frontier,
        first.envelope.expected_frontier
    );
    assert_eq!(retry.admission, current_admission);
    assert_eq!(
        sigil_application::command_fingerprint(&first)?,
        sigil_application::command_fingerprint(&retry)?
    );
    retry.envelope.command = ApplicationCommand::Mcp(McpCommand::Refresh {
        binding: "changed".to_owned(),
    });
    retained.retain_original(&mut retry)?;
    assert_ne!(
        sigil_application::command_fingerprint(&first)?,
        sigil_application::command_fingerprint(&retry)?
    );
    Ok(())
}

#[test]
fn http_command_frontier_retention_uses_the_full_reservation_key() -> Result<(), ApplicationError> {
    let retained = HttpCommandFrontiers::default();
    retained.retain_original(&mut request(1)?)?;
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
        retained.retain_original(&mut foreign)?;
        assert_eq!(foreign.envelope.expected_frontier.through_sequence, 9);
    }
    let mut mismatched = request(11)?;
    mismatched.envelope.expected_frontier.scope.session = Some(SessionScopeId::new("foreign")?);
    assert_eq!(
        retained.retain_original(&mut mismatched),
        Err(ApplicationError::ScopeMismatch)
    );
    Ok(())
}

#[test]
fn http_command_frontier_concurrent_first_admission_freezes_one_original() -> anyhow::Result<()> {
    let retained = Arc::new(HttpCommandFrontiers::default());
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let workers = (1..=8)
        .map(|sequence| {
            let retained = Arc::clone(&retained);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || -> Result<ExpectedFrontier, ApplicationError> {
                let mut request = request(sequence)?;
                barrier.wait();
                retained.retain_original(&mut request)?;
                Ok(request.envelope.expected_frontier)
            })
        })
        .collect::<Vec<_>>();
    let frontiers = workers
        .into_iter()
        .map(|worker| worker.join().expect("frontier worker"))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(frontiers.iter().all(|frontier| frontier == &frontiers[0]));
    Ok(())
}

#[test]
fn http_command_frontier_capacity_preserves_existing_keys_without_eviction()
-> Result<(), ApplicationError> {
    let retained = HttpCommandFrontiers::default();
    for index in 0..MAX_HTTP_COMMAND_FRONTIERS {
        let mut request = request(1)?;
        request.envelope.command_id = ApplicationCommandId::new(format!("command-{index}"))?;
        retained.retain_original(&mut request)?;
    }
    let mut additional = request(9)?;
    assert_eq!(
        retained.retain_original(&mut additional),
        Err(ApplicationError::Unavailable)
    );
    additional.envelope.command_id = ApplicationCommandId::new("command-0")?;
    retained.retain_original(&mut additional)?;
    assert_eq!(additional.envelope.expected_frontier.through_sequence, 1);
    Ok(())
}
