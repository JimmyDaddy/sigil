//! Runtime implementation boundary for the transport-neutral application contract.
//!
//! This module owns the one in-process application service used by product surfaces.  Physical
//! resource allocation remains below the injected runtime executor/source; this layer owns
//! command identity, replay/conflict handling, page request lifecycle, and the application-port
//! boundary.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use futures::future::BoxFuture;
use sigil_application::{
    ApplicationCommandReceipt, ApplicationCommandRequest, ApplicationDomainReceipt,
    ApplicationError, ApplicationInFlightReceipt, ApplicationPort, CommandConflict,
    CommandRejection, CommandReservationKey, OpenProjectionRequest, PageCancellationReceipt,
    PageRequestId, ProjectionDeliveryAck, ProjectionPage, ProjectionPageRequest,
    ProjectionSnapshot, UncertainCommandReceipt, command_fingerprint,
};

/// Runtime query source for the bounded application projection.
///
/// Implementations adapt durable session/application truth into renderer-safe application
/// snapshots and pages.  They must not expose paths, provider payloads, or physical authority
/// objects through the application contract.
pub trait RuntimeApplicationProjectionSource: Send + Sync {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>>;

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>>;
}

/// Runtime-owned command effect result after durable application admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeApplicationDispatch {
    Settled(ApplicationDomainReceipt),
    Rejected(CommandRejection),
    Uncertain(UncertainCommandReceipt),
}

/// Runtime command executor supplied by the composition root.
///
/// The service reserves the command before calling this trait.  An executor error is therefore
/// converted to an `Uncertain` receipt instead of being treated as proof that no effect happened.
pub trait RuntimeApplicationCommandExecutor: Send + Sync {
    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>>;
}

/// Runtime owner for application projection delivery acknowledgements.
pub trait RuntimeApplicationDeliveryAcker: Send + Sync {
    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;
}

/// Result of atomically reserving a command identity in the application journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeApplicationReservationAdmission {
    Reserved,
    InFlight(ApplicationInFlightReceipt),
    Existing(ApplicationCommandReceipt),
    Conflict(CommandConflict),
}

/// Durable application reservation store supplied by the runtime composition root.
pub trait RuntimeApplicationReservationStore: Send + Sync {
    fn reserve(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>>;

    fn mark_dispatch_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;

    fn settle(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageRecord {
    request: ProjectionPageRequest,
    result: Option<ProjectionPage>,
    cancelled: bool,
}

#[derive(Default)]
struct RuntimeApplicationState {
    pages: BTreeMap<PageRequestId, PageRecord>,
}

/// The single runtime implementation of [`ApplicationPort`].
pub struct RuntimeApplicationService {
    projection: Arc<dyn RuntimeApplicationProjectionSource>,
    executor: Arc<dyn RuntimeApplicationCommandExecutor>,
    reservations: Arc<dyn RuntimeApplicationReservationStore>,
    delivery: Arc<dyn RuntimeApplicationDeliveryAcker>,
    state: Arc<Mutex<RuntimeApplicationState>>,
}

impl fmt::Debug for RuntimeApplicationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeApplicationService")
            .field("projection", &"<runtime projection source>")
            .field("executor", &"<runtime command executor>")
            .field("reservations", &"<runtime reservation store>")
            .field("delivery", &"<runtime delivery acker>")
            .finish_non_exhaustive()
    }
}

impl RuntimeApplicationService {
    pub fn new(
        projection: Arc<dyn RuntimeApplicationProjectionSource>,
        executor: Arc<dyn RuntimeApplicationCommandExecutor>,
        reservations: Arc<dyn RuntimeApplicationReservationStore>,
        delivery: Arc<dyn RuntimeApplicationDeliveryAcker>,
    ) -> Self {
        Self {
            projection,
            executor,
            reservations,
            delivery,
            state: Arc::new(Mutex::new(RuntimeApplicationState::default())),
        }
    }

    fn uncertain_receipt(
        request: &ApplicationCommandRequest,
        fingerprint: String,
    ) -> ApplicationCommandReceipt {
        ApplicationCommandReceipt::Uncertain(UncertainCommandReceipt {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            reservation_fingerprint: fingerprint,
            recovery_binding: "application-command-reconcile".to_owned(),
        })
    }

    fn validate_settled_receipt(
        request: &ApplicationCommandRequest,
        receipt: ApplicationDomainReceipt,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        if receipt.command_id != request.envelope.command_id
            || receipt.command_kind != request.envelope.command.kind()
            || receipt.frontier.scope != request.admission.scope
        {
            return Err(ApplicationError::CorruptProjection(
                "runtime command receipt does not match its admitted request".to_owned(),
            ));
        }
        Ok(ApplicationCommandReceipt::Settled(receipt))
    }
}

impl ApplicationPort for RuntimeApplicationService {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        self.projection.open_projection(request)
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        let projection = Arc::clone(&self.projection);
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            if request.limit.get() > sigil_application::MAX_PAGE_ITEMS {
                return Err(ApplicationError::InvalidRequest(
                    "page limit exceeds application bound".to_owned(),
                ));
            }
            {
                let mut state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
                if let Some(record) = state.pages.get(&request.request_id) {
                    if record.request != request {
                        return Err(ApplicationError::ScopeMismatch);
                    }
                    if record.cancelled {
                        return Err(ApplicationError::ResetRequired);
                    }
                    if let Some(result) = &record.result {
                        return Ok(result.clone());
                    }
                    return Err(ApplicationError::Unavailable);
                }
                state.pages.insert(
                    request.request_id.clone(),
                    PageRecord {
                        request: request.clone(),
                        result: None,
                        cancelled: false,
                    },
                );
            }

            let result = projection.page(request.clone()).await;
            let mut state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
            let record = state
                .pages
                .get_mut(&request.request_id)
                .ok_or(ApplicationError::Unavailable)?;
            if record.cancelled {
                return Err(ApplicationError::ResetRequired);
            }
            let page = result?;
            if page.request_id != request.request_id
                || page.scope != request.scope
                || page.source_generation != request.source_generation
                || page.at_frontier != request.at_frontier
                || page.query != request.query
            {
                return Err(ApplicationError::CorruptProjection(
                    "runtime page response does not match its request".to_owned(),
                ));
            }
            record.result = Some(page.clone());
            Ok(page)
        })
    }

    fn cancel_page(&self, request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let Ok(mut state) = state.lock() else {
                return PageCancellationReceipt::UnknownRequest;
            };
            let Some(record) = state.pages.get_mut(&request) else {
                return PageCancellationReceipt::UnknownRequest;
            };
            if record.result.is_some() {
                return PageCancellationReceipt::Completed;
            }
            record.cancelled = true;
            PageCancellationReceipt::CancelledBeforeLoad
        })
    }

    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        if let Err(error) = acknowledgement.validate() {
            return Box::pin(async move { Err(error) });
        }
        self.delivery.acknowledge(acknowledgement)
    }

    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        let executor = Arc::clone(&self.executor);
        let reservations = Arc::clone(&self.reservations);
        Box::pin(async move {
            request.validate()?;
            let fingerprint = command_fingerprint(&request)?;
            let key = request
                .admission
                .reservation_key(&request.envelope.command_id);
            let admission = reservations
                .reserve(key.clone(), fingerprint.clone(), request.clone())
                .await?;
            match admission {
                RuntimeApplicationReservationAdmission::Reserved => {}
                RuntimeApplicationReservationAdmission::InFlight(receipt) => {
                    return Ok(ApplicationCommandReceipt::InFlight(receipt));
                }
                RuntimeApplicationReservationAdmission::Existing(receipt) => {
                    return Ok(match receipt {
                        ApplicationCommandReceipt::Settled(domain) => {
                            ApplicationCommandReceipt::Replayed(domain)
                        }
                        ApplicationCommandReceipt::Uncertain(receipt) => {
                            ApplicationCommandReceipt::ReplayedUncertain(receipt)
                        }
                        receipt => receipt,
                    });
                }
                RuntimeApplicationReservationAdmission::Conflict(conflict) => {
                    return Ok(ApplicationCommandReceipt::PayloadConflict(conflict));
                }
            }

            if let Err(error) = reservations
                .mark_dispatch_started(key.clone(), fingerprint.clone())
                .await
            {
                // The reservation is already durable, but dispatch has not been admitted. If
                // the marker write raced a storage fault, retain an explicit uncertain terminal
                // instead of leaving an unrepairable Reserved row that every retry reports as
                // InFlight forever. A failed settlement keeps the original error and the
                // durable reservation remains fail-closed for operator reconciliation.
                let uncertain = Self::uncertain_receipt(&request, fingerprint.clone());
                if reservations
                    .settle(key, fingerprint, uncertain.clone())
                    .await
                    .is_ok()
                {
                    return Ok(uncertain);
                }
                return Err(error);
            }

            let outcome = match executor.dispatch(request.clone()).await {
                Ok(RuntimeApplicationDispatch::Settled(receipt)) => {
                    match Self::validate_settled_receipt(&request, receipt) {
                        Ok(receipt) => receipt,
                        Err(_) => Self::uncertain_receipt(&request, fingerprint.clone()),
                    }
                }
                Ok(RuntimeApplicationDispatch::Rejected(rejection)) => {
                    ApplicationCommandReceipt::Rejected(rejection)
                }
                Ok(RuntimeApplicationDispatch::Uncertain(receipt)) => {
                    ApplicationCommandReceipt::Uncertain(receipt)
                }
                Err(_) => Self::uncertain_receipt(&request, fingerprint.clone()),
            };

            if reservations
                .settle(key, fingerprint, outcome.clone())
                .await
                .is_err()
            {
                return Ok(Self::uncertain_receipt(
                    &request,
                    command_fingerprint(&request)?,
                ));
            }
            Ok(outcome)
        })
    }
}

#[cfg(test)]
mod tests {
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
        ProjectionPageRequest, ProjectionSnapshot, SafeText, SessionScopeId,
        UncertainCommandReceipt, WorkspaceScopeId,
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
        ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>>
        {
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
                        RuntimeApplicationReservationAdmission::InFlight(
                            ApplicationInFlightReceipt {
                                command_id: request.envelope.command_id,
                                command_kind: request.envelope.command.kind().to_owned(),
                                reservation_fingerprint: fingerprint,
                            },
                        )
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
                    prompt: SafeText::new(prompt).expect("prompt"),
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
        let first = futures::executor::block_on(service.execute(request("hello", 1)))
            .expect("first command");
        assert!(matches!(first, ApplicationCommandReceipt::Settled(_)));
        let replay = futures::executor::block_on(service.execute(request("hello", 1)))
            .expect("replay command");
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
}
