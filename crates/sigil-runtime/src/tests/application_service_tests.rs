use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::future::BoxFuture;
use sigil_application::{
    APPLICATION_CONTRACT_SCHEMA_VERSION, ApplicationCommand, ApplicationCommandEnvelope,
    ApplicationCommandId, ApplicationCommandReceipt, ApplicationCommandRequest,
    ApplicationDomainReceipt, ApplicationFrontier, ApplicationInFlightReceipt,
    ApplicationInstanceId, ApplicationScope, AuthenticatedSubject, CommandAdmissionContext,
    CommandConflict, CommandReservationKey, ConversationCommand, ExpectedFrontier,
    HostConnectionInstanceId, OpenProjectionRequest, ProjectionDeliveryAck, ProjectionPage,
    ProjectionPageRequest, ProjectionSnapshot, SafeText, SessionScopeId, UncertainCommandReceipt,
    WorkspaceScopeId,
};

use super::*;

struct UnavailableProjection;

impl RuntimeApplicationProjectionSource for UnavailableProjection {
    fn open_projection(
        &self,
        _request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }

    fn page(
        &self,
        _request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
}

struct SettlingExecutor {
    calls: Arc<AtomicUsize>,
}

impl RuntimeApplicationCommandExecutor for SettlingExecutor {
    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let receipt = ApplicationDomainReceipt {
            command_id: request.envelope.command_id,
            command_kind: request.envelope.command.kind().to_owned(),
            frontier: ApplicationFrontier {
                schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
                scope: request.admission.scope.clone(),
                writer_generation: 1,
                stream_generation: 1,
                through_sequence: 1,
                durable_cursor: "cursor-1".to_owned(),
            },
            settlement: request.envelope.command.policy().settlement,
            summary: "settled in test executor".to_owned(),
            outcome: None,
        };
        Box::pin(async move { Ok(RuntimeApplicationDispatch::Settled(receipt)) })
    }
}

struct UncertainExecutor;

impl RuntimeApplicationCommandExecutor for UncertainExecutor {
    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>> {
        Box::pin(async move {
            Ok(RuntimeApplicationDispatch::Uncertain(
                UncertainCommandReceipt {
                    command_id: request.envelope.command_id,
                    command_kind: request.envelope.command.kind().to_owned(),
                    reservation_fingerprint: "fingerprint".to_owned(),
                    recovery_binding: "test-reconcile".to_owned(),
                },
            ))
        })
    }
}

struct Acker;

impl RuntimeApplicationDeliveryAcker for Acker {
    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async move { acknowledgement.validate() })
    }
}

#[derive(Default)]
struct TestReservationStore {
    entries: Mutex<BTreeMap<CommandReservationKey, (String, TestReservationState)>>,
    fail_mark: bool,
}

enum TestReservationState {
    Reserved,
    DispatchStarted,
    Terminal(Box<ApplicationCommandReceipt>),
}

impl RuntimeApplicationReservationStore for TestReservationStore {
    fn reserve(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>> {
        let result = (|| {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let Some((original, receipt)) = entries.get(&key) else {
                entries.insert(key, (fingerprint, TestReservationState::Reserved));
                return Ok(RuntimeApplicationReservationAdmission::Reserved);
            };
            if original != &fingerprint {
                return Ok(RuntimeApplicationReservationAdmission::Conflict(
                    CommandConflict {
                        command_id: request.envelope.command_id,
                        original_fingerprint: original.clone(),
                        received_fingerprint: fingerprint,
                    },
                ));
            }
            Ok(match receipt {
                TestReservationState::Terminal(receipt) => {
                    RuntimeApplicationReservationAdmission::Existing(receipt.as_ref().clone())
                }
                TestReservationState::Reserved | TestReservationState::DispatchStarted => {
                    RuntimeApplicationReservationAdmission::InFlight(ApplicationInFlightReceipt {
                        command_id: request.envelope.command_id,
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                    })
                }
            })
        })();
        Box::pin(async move { result })
    }

    fn mark_dispatch_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        if self.fail_mark {
            return Box::pin(async { Err(ApplicationError::Unavailable) });
        }
        let result = (|| {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let Some((original, state)) = entries.get_mut(&key) else {
                return Err(ApplicationError::Unavailable);
            };
            if original != &fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match state {
                TestReservationState::Reserved => {
                    *state = TestReservationState::DispatchStarted;
                    Ok(())
                }
                TestReservationState::DispatchStarted => Ok(()),
                TestReservationState::Terminal(_) => Err(ApplicationError::InvalidRequest(
                    "terminal reservation cannot be dispatched".to_owned(),
                )),
            }
        })();
        Box::pin(async move { result })
    }

    fn settle(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = (|| {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let Some((original, stored)) = entries.get_mut(&key) else {
                return Err(ApplicationError::Unavailable);
            };
            if original != &fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            *stored = TestReservationState::Terminal(Box::new(receipt));
            Ok(())
        })();
        Box::pin(async move { result })
    }
}

fn request(prompt: &str, client_epoch: u64) -> ApplicationCommandRequest {
    let subject = AuthenticatedSubject::new("subject").expect("subject");
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("app").expect("app"),
        authenticated_subject: subject.clone(),
        workspace: Some(WorkspaceScopeId::new("workspace").expect("workspace")),
        session: Some(SessionScopeId::new("session").expect("session")),
    };
    ApplicationCommandRequest {
        envelope: ApplicationCommandEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            command_id: ApplicationCommandId::new("command").expect("command"),
            correlation_id: None,
            expected_frontier: ExpectedFrontier {
                scope: scope.clone(),
                writer_generation: 1,
                through_sequence: 0,
            },
            command: ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: Some(SafeText::new(prompt).expect("prompt")),
                options: None,
            }),
        },
        admission: CommandAdmissionContext::host_bound(
            subject,
            client_epoch,
            HostConnectionInstanceId::new("connection").expect("connection"),
            scope,
        )
        .expect("admission"),
    }
}

#[test]
fn runtime_service_replays_and_conflicts_by_admission_key() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(SettlingExecutor {
            calls: Arc::clone(&calls),
        }),
        Arc::new(TestReservationStore::default()),
        Arc::new(Acker),
    );
    let first =
        futures::executor::block_on(service.execute(request("hello", 1))).expect("first command");
    assert!(matches!(first, ApplicationCommandReceipt::Settled(_)));
    let replay =
        futures::executor::block_on(service.execute(request("hello", 1))).expect("replay command");
    assert!(matches!(replay, ApplicationCommandReceipt::Replayed(_)));
    let conflict = futures::executor::block_on(service.execute(request("different", 1)))
        .expect("conflicting command");
    assert!(matches!(
        conflict,
        ApplicationCommandReceipt::PayloadConflict(_)
    ));
    let new_epoch = futures::executor::block_on(service.execute(request("hello", 2)))
        .expect("new epoch command");
    assert!(matches!(new_epoch, ApplicationCommandReceipt::Settled(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn runtime_service_settles_uncertain_when_dispatch_marker_fails() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reservations = Arc::new(TestReservationStore {
        fail_mark: true,
        ..TestReservationStore::default()
    });
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(SettlingExecutor {
            calls: Arc::clone(&calls),
        }),
        Arc::clone(&reservations) as Arc<dyn RuntimeApplicationReservationStore>,
        Arc::new(Acker),
    );
    let receipt = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect("uncertain terminal");
    assert!(matches!(receipt, ApplicationCommandReceipt::Uncertain(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let state = reservations.entries.lock().expect("reservation state");
    assert!(state.values().all(|(_, state)| matches!(
        state,
        TestReservationState::Terminal(receipt)
            if matches!(receipt.as_ref(), ApplicationCommandReceipt::Uncertain(_))
    )));
}

#[test]
fn runtime_service_replays_an_uncertain_terminal_without_redispatching() {
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(UncertainExecutor),
        Arc::new(TestReservationStore::default()),
        Arc::new(Acker),
    );
    let first = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect("uncertain command");
    assert!(matches!(first, ApplicationCommandReceipt::Uncertain(_)));
    let replay = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect("uncertain replay");
    assert!(matches!(
        replay,
        ApplicationCommandReceipt::ReplayedUncertain(_)
    ));
}

#[test]
fn runtime_service_rejects_invalid_delivery_ack_before_delegation() {
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(SettlingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(TestReservationStore::default()),
        Arc::new(Acker),
    );
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("app").expect("app"),
        authenticated_subject: AuthenticatedSubject::new("subject").expect("subject"),
        workspace: None,
        session: Some(SessionScopeId::new("session").expect("session")),
    };
    let error = futures::executor::block_on(service.acknowledge(ProjectionDeliveryAck {
        scope: scope.clone(),
        observer_generation: 1,
        event_id: String::new(),
        frontier: ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope,
            writer_generation: 1,
            stream_generation: 1,
            through_sequence: 0,
            durable_cursor: "cursor".to_owned(),
        },
    }))
    .expect_err("empty event id must fail");
    assert!(matches!(error, ApplicationError::InvalidRequest(_)));
}
