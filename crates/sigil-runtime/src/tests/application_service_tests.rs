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
    ApplicationDomainCommitRef, ApplicationDomainReceipt, ApplicationFrontier,
    ApplicationInFlightReceipt, ApplicationInstanceId, ApplicationScope, AuthenticatedSubject,
    CommandAdmissionContext, CommandConflict, CommandEffectBinding, CommandLifecyclePhase,
    CommandNoEffectProof, CommandRecoveryBinding, CommandReservationKey, ConversationCommand,
    ExpectedFrontier, HostConnectionInstanceId, OpenProjectionRequest, ProjectionDeliveryAck,
    ProjectionPage, ProjectionPageRequest, ProjectionSnapshot, RunCommand, SafeText,
    SafetyStopDisposition, SessionScopeId, UncertainCommandReceipt, WorkspaceScopeId,
    command_fingerprint,
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
    fn bind_effect(
        &self,
        request: ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>> {
        let binding = CommandEffectBinding {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            reservation_fingerprint: fingerprint,
            recovery: CommandRecoveryBinding {
                key,
                phase: CommandLifecyclePhase::EffectStarted,
            },
            owner_effect_id: "test-effect".to_owned(),
        };
        Box::pin(async move { binding.validate().map(|()| binding) })
    }

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
            domain_commit: ApplicationDomainCommitRef {
                source_session_scope_id: None,
                source_event_id: "test-domain-event".to_owned(),
                source_sequence: 1,
                source_digest: "a".repeat(64),
            },
            outcome: None,
        };
        Box::pin(async move { Ok(RuntimeApplicationDispatch::Settled(receipt)) })
    }
}

struct UncertainExecutor;

impl RuntimeApplicationCommandExecutor for UncertainExecutor {
    fn bind_effect(
        &self,
        request: ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>> {
        let binding = CommandEffectBinding {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            reservation_fingerprint: fingerprint,
            recovery: CommandRecoveryBinding {
                key,
                phase: CommandLifecyclePhase::EffectStarted,
            },
            owner_effect_id: "test-uncertain-effect".to_owned(),
        };
        Box::pin(async move { binding.validate().map(|()| binding) })
    }

    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>> {
        Box::pin(async move {
            Ok(RuntimeApplicationDispatch::Uncertain(
                UncertainCommandReceipt {
                    command_id: request.envelope.command_id.clone(),
                    command_kind: request.envelope.command.kind().to_owned(),
                    reservation_fingerprint: "a".repeat(64),
                    recovery: CommandRecoveryBinding {
                        key: request
                            .admission
                            .reservation_key(&request.envelope.command_id),
                        phase: CommandLifecyclePhase::EffectStarted,
                    },
                    owner_recovery_binding: None,
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
    fail_reserve: bool,
    fail_settle: bool,
    injected_failure: Option<(TestJournalFailureStage, ApplicationError)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TestJournalFailureStage {
    Reserve,
    Dispatch,
    Effect,
    Guard,
}

impl TestReservationStore {
    fn failure(&self, stage: TestJournalFailureStage) -> Option<ApplicationError> {
        self.injected_failure
            .as_ref()
            .filter(|(at, _)| *at == stage)
            .map(|(_, error)| error.clone())
    }
}

enum TestReservationState {
    Reserved,
    DispatchStarted,
    EffectStarted,
    DomainCommitted(Box<ApplicationDomainReceipt>),
    Uncertain(Box<UncertainCommandReceipt>),
    Settled(Box<ApplicationCommandReceipt>),
}

impl RuntimeApplicationReservationStore for TestReservationStore {
    fn forward_guard(
        &self,
        _request: &ApplicationCommandRequest,
    ) -> Result<
        Option<Box<dyn sigil_kernel::managed_storage::ManagedStorageForwardGuardV1>>,
        ApplicationError,
    > {
        match self.failure(TestJournalFailureStage::Guard) {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    fn reserve(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>> {
        if let Some(error) = self.failure(TestJournalFailureStage::Reserve) {
            return Box::pin(async move { Err(error) });
        }
        if self.fail_reserve {
            return Box::pin(async { Err(ApplicationError::Unavailable) });
        }
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
                TestReservationState::Settled(receipt) => {
                    RuntimeApplicationReservationAdmission::Existing(Box::new(
                        receipt.as_ref().clone(),
                    ))
                }
                TestReservationState::Uncertain(receipt) => {
                    RuntimeApplicationReservationAdmission::Existing(Box::new(
                        ApplicationCommandReceipt::Uncertain(receipt.as_ref().clone()),
                    ))
                }
                TestReservationState::DomainCommitted(receipt) => {
                    RuntimeApplicationReservationAdmission::Existing(Box::new(
                        ApplicationCommandReceipt::Settled(receipt.as_ref().clone()),
                    ))
                }
                TestReservationState::Reserved | TestReservationState::DispatchStarted => {
                    RuntimeApplicationReservationAdmission::Existing(Box::new(
                        ApplicationCommandReceipt::Uncertain(UncertainCommandReceipt {
                            command_id: request.envelope.command_id,
                            command_kind: request.envelope.command.kind().to_owned(),
                            reservation_fingerprint: fingerprint,
                            recovery: CommandRecoveryBinding {
                                key,
                                phase: match receipt {
                                    TestReservationState::Reserved => {
                                        CommandLifecyclePhase::Reserved
                                    }
                                    TestReservationState::DispatchStarted => {
                                        CommandLifecyclePhase::DispatchStarted
                                    }
                                    _ => unreachable!("pre-effect states are matched above"),
                                },
                            },
                            owner_recovery_binding: None,
                        }),
                    ))
                }
                TestReservationState::EffectStarted => {
                    RuntimeApplicationReservationAdmission::InFlight(ApplicationInFlightReceipt {
                        command_id: request.envelope.command_id,
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                        phase: CommandLifecyclePhase::EffectStarted,
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
        if let Some(error) = self.failure(TestJournalFailureStage::Dispatch) {
            return Box::pin(async move { Err(error) });
        }
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
                _ => Err(ApplicationError::InvalidRequest(
                    "terminal reservation cannot be dispatched".to_owned(),
                )),
            }
        })();
        Box::pin(async move { result })
    }

    fn mark_effect_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        _binding: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        if let Some(error) = self.failure(TestJournalFailureStage::Effect) {
            return Box::pin(async move { Err(error) });
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
                TestReservationState::DispatchStarted => {
                    *state = TestReservationState::EffectStarted;
                    Ok(())
                }
                TestReservationState::EffectStarted => Ok(()),
                _ => Err(ApplicationError::InvalidRequest(
                    "effect marker is not monotonic".to_owned(),
                )),
            }
        })();
        Box::pin(async move { result })
    }

    fn mark_domain_committed(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationDomainReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
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
                TestReservationState::EffectStarted => {
                    *state = TestReservationState::DomainCommitted(Box::new(receipt));
                    Ok(())
                }
                TestReservationState::DomainCommitted(previous) if **previous == receipt => Ok(()),
                _ => Err(ApplicationError::InvalidRequest(
                    "domain commit marker is not monotonic".to_owned(),
                )),
            }
        })();
        Box::pin(async move { result })
    }

    fn mark_confirmed_no_effect(
        &self,
        _key: CommandReservationKey,
        _fingerprint: String,
        _proof: CommandNoEffectProof,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }

    fn mark_uncertain(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: UncertainCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
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
                TestReservationState::EffectStarted => {
                    *state = TestReservationState::Uncertain(Box::new(receipt));
                    Ok(())
                }
                TestReservationState::Uncertain(previous) if **previous == receipt => Ok(()),
                _ => Err(ApplicationError::InvalidRequest(
                    "uncertain marker is not monotonic".to_owned(),
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
        if self.fail_settle {
            return Box::pin(async { Err(ApplicationError::Unavailable) });
        }
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
            *stored = TestReservationState::Settled(Box::new(receipt));
            Ok(())
        })();
        Box::pin(async move { result })
    }
}

struct SafetyStopExecutor {
    dispatch_calls: Arc<AtomicUsize>,
    safety_stop_calls: Arc<AtomicUsize>,
    disposition: SafetyStopDisposition,
}

impl RuntimeApplicationCommandExecutor for SafetyStopExecutor {
    fn bind_effect(
        &self,
        request: ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>> {
        SettlingExecutor {
            calls: Arc::clone(&self.dispatch_calls),
        }
        .bind_effect(request, key, fingerprint)
    }

    fn dispatch(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>> {
        self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }

    fn request_safety_stop(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<SafetyStopDisposition, ApplicationError>> {
        self.safety_stop_calls.fetch_add(1, Ordering::SeqCst);
        let disposition = self.disposition;
        Box::pin(async move { Ok(disposition) })
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

fn safety_stop_request() -> ApplicationCommandRequest {
    let mut request = request("stop", 1);
    request.envelope.command = ApplicationCommand::Run(RunCommand::CancelTerminalTask {
        identity: sigil_application::ApplicationTerminalTaskIdentity {
            session_scope_id: SafeText::new("session").expect("session"),
            run_id: SafeText::new("run").expect("run"),
            task_id: SafeText::new("task").expect("task"),
            expected_generation: 1,
        },
    });
    request
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
    let error = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect_err("dispatch marker failure must remain an error");
    assert!(matches!(error, ApplicationError::Unavailable));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let state = reservations.entries.lock().expect("reservation state");
    assert!(
        state
            .values()
            .all(|(_, state)| matches!(state, TestReservationState::Reserved))
    );
    drop(state);
    let repair = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect("same-key retry must require repair");
    assert!(matches!(
        repair,
        ApplicationCommandReceipt::ReplayedUncertain(receipt)
            if receipt.recovery.phase == CommandLifecyclePhase::Reserved
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_service_does_not_redispatch_a_replayed_dispatch_marker() {
    let request = request("hello", 1);
    let key = request
        .admission
        .reservation_key(&request.envelope.command_id);
    let fingerprint = command_fingerprint(&request).expect("fingerprint");
    let reservations = Arc::new(TestReservationStore::default());
    reservations
        .entries
        .lock()
        .expect("reservation state")
        .insert(key, (fingerprint, TestReservationState::DispatchStarted));
    let calls = Arc::new(AtomicUsize::new(0));
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(SettlingExecutor {
            calls: Arc::clone(&calls),
        }),
        Arc::clone(&reservations) as Arc<dyn RuntimeApplicationReservationStore>,
        Arc::new(Acker),
    );

    let receipt = futures::executor::block_on(service.execute(request))
        .expect("replayed dispatch marker must require repair");
    assert!(matches!(
        receipt,
        ApplicationCommandReceipt::ReplayedUncertain(receipt)
            if receipt.recovery.phase == CommandLifecyclePhase::DispatchStarted
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_service_replays_domain_commit_after_fault_injected_settlement_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reservations = Arc::new(TestReservationStore {
        fail_settle: true,
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

    let first = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect("domain commit must remain terminal when settlement indexing fails");
    assert!(matches!(first, ApplicationCommandReceipt::Settled(_)));
    let replay = futures::executor::block_on(service.execute(request("hello", 1)))
        .expect("same key must replay the verified domain commit");
    assert!(matches!(replay, ApplicationCommandReceipt::Replayed(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        reservations
            .entries
            .lock()
            .expect("reservation state")
            .values()
            .all(|(_, state)| matches!(state, TestReservationState::DomainCommitted(_)))
    );
}

#[test]
fn runtime_service_only_reports_unrecorded_stop_when_owner_closed_the_forward_gate() {
    let dispatch_calls = Arc::new(AtomicUsize::new(0));
    let safety_stop_calls = Arc::new(AtomicUsize::new(0));
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(SafetyStopExecutor {
            dispatch_calls: Arc::clone(&dispatch_calls),
            safety_stop_calls: Arc::clone(&safety_stop_calls),
            disposition: SafetyStopDisposition::ForwardGateClosed,
        }),
        Arc::new(TestReservationStore {
            fail_reserve: true,
            ..TestReservationStore::default()
        }),
        Arc::new(Acker),
    );

    let receipt = futures::executor::block_on(service.execute(safety_stop_request()))
        .expect("owner-confirmed stop");
    assert!(matches!(
        receipt,
        ApplicationCommandReceipt::SafetyStopRequestedButUnrecorded(_)
    ));
    assert_eq!(safety_stop_calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispatch_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_service_rejects_unconfirmed_safety_stop_without_normal_dispatch() {
    let dispatch_calls = Arc::new(AtomicUsize::new(0));
    let safety_stop_calls = Arc::new(AtomicUsize::new(0));
    let service = RuntimeApplicationService::new(
        Arc::new(UnavailableProjection),
        Arc::new(SafetyStopExecutor {
            dispatch_calls: Arc::clone(&dispatch_calls),
            safety_stop_calls: Arc::clone(&safety_stop_calls),
            disposition: SafetyStopDisposition::Uncertain,
        }),
        Arc::new(TestReservationStore {
            fail_reserve: true,
            ..TestReservationStore::default()
        }),
        Arc::new(Acker),
    );

    let error = futures::executor::block_on(service.execute(safety_stop_request()))
        .expect_err("unconfirmed stop must not be reported as requested");
    assert!(matches!(error, ApplicationError::Unavailable));
    assert_eq!(safety_stop_calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispatch_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn exact_stop_survives_each_pre_dispatch_journal_failure_without_bypassing_scope_or_conflict() {
    for stage in [
        TestJournalFailureStage::Reserve,
        TestJournalFailureStage::Dispatch,
        TestJournalFailureStage::Effect,
        TestJournalFailureStage::Guard,
    ] {
        for error in [
            ApplicationError::Unavailable,
            ApplicationError::ScopeMismatch,
        ] {
            for exact_stop in [true, false] {
                let dispatch_calls = Arc::new(AtomicUsize::new(0));
                let safety_stop_calls = Arc::new(AtomicUsize::new(0));
                let reservations = Arc::new(TestReservationStore {
                    injected_failure: Some((stage, error.clone())),
                    ..TestReservationStore::default()
                });
                let service = RuntimeApplicationService::new(
                    Arc::new(UnavailableProjection),
                    Arc::new(SafetyStopExecutor {
                        dispatch_calls: Arc::clone(&dispatch_calls),
                        safety_stop_calls: Arc::clone(&safety_stop_calls),
                        disposition: SafetyStopDisposition::ForwardGateClosed,
                    }),
                    reservations.clone(),
                    Arc::new(Acker),
                );
                let mut request = request("forward", 1);
                if exact_stop {
                    request.envelope.command = ApplicationCommand::Run(RunCommand::Cancel {
                        binding: "run".to_owned(),
                        reason: None,
                    });
                }
                let result = futures::executor::block_on(service.execute(request.clone()));
                let safe = exact_stop && error == ApplicationError::Unavailable;
                if safe {
                    assert!(
                        matches!(
                            result,
                            Ok(ApplicationCommandReceipt::SafetyStopRequestedButUnrecorded(
                                _
                            ))
                        ),
                        "{stage:?}: {result:?}"
                    );
                } else {
                    assert_eq!(result, Err(error.clone()), "{stage:?}");
                }
                assert_eq!(safety_stop_calls.load(Ordering::SeqCst), usize::from(safe));
                assert_eq!(dispatch_calls.load(Ordering::SeqCst), 0);
                if stage != TestJournalFailureStage::Reserve {
                    request.envelope.command = ApplicationCommand::Run(RunCommand::Cancel {
                        binding: "different-run".to_owned(),
                        reason: None,
                    });
                    assert!(matches!(
                        futures::executor::block_on(service.execute(request)),
                        Ok(ApplicationCommandReceipt::PayloadConflict(_))
                    ));
                    assert_eq!(safety_stop_calls.load(Ordering::SeqCst), usize::from(safe));
                }
            }
        }
    }
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

#[tokio::test]
async fn runtime_page_cancellation_drops_the_active_source_future() -> anyhow::Result<()> {
    struct PendingSource {
        started: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicUsize>,
    }
    struct SourceDrop(Arc<AtomicUsize>);
    impl Drop for SourceDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl RuntimeApplicationProjectionSource for PendingSource {
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
            let started = self.started.clone();
            let dropped = self.dropped.clone();
            Box::pin(async move {
                let _drop = SourceDrop(dropped);
                started.notify_one();
                futures::future::pending().await
            })
        }
    }
    let started = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(RuntimeApplicationService::new(
        Arc::new(PendingSource {
            started: started.clone(),
            dropped: dropped.clone(),
        }),
        Arc::new(SettlingExecutor {
            calls: Arc::default(),
        }),
        Arc::new(TestReservationStore::default()),
        Arc::new(Acker),
    ));
    let scope = request("unused", 1).admission.scope;
    let request = ProjectionPageRequest {
        request_id: PageRequestId::new("cancel-active-page")?,
        scope: scope.clone(),
        source_generation: 1,
        at_frontier: ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope,
            writer_generation: 1,
            stream_generation: 1,
            through_sequence: 1,
            durable_cursor: "session-stream:1".into(),
        },
        query: sigil_application::PageQueryFingerprint::new("transcript")?,
        anchor: sigil_application::PageAnchor {
            item_id: None,
            intra_item_row: 0,
            cursor: None,
        },
        direction: sigil_application::PageDirection::Older,
        limit: std::num::NonZeroUsize::new(1).expect("positive limit"),
        width_bucket: 80,
    };
    let source = service.clone();
    let page = request.clone();
    let loading = tokio::spawn(async move { source.page(page).await });
    started.notified().await;
    assert_eq!(
        service.cancel_page(request.request_id.clone()).await,
        PageCancellationReceipt::CancelledBeforeLoad
    );
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), loading).await??,
        Err(ApplicationError::ResetRequired)
    ));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(matches!(
        service.page(request).await,
        Err(ApplicationError::ResetRequired)
    ));
    Ok(())
}

#[test]
fn ordinary_run_binding_recovers_only_exact_admission_and_preserves_outcome() -> anyhow::Result<()>
{
    let fixture = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(fixture.path().join("admission.jsonl"))?;
    let mut session = sigil_kernel::Session::new("test", "model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let mut request = request("original input", 1);
    let session_scope = SessionScopeId::new(session.session_scope_id())?;
    request.admission.scope.session = Some(session_scope.clone());
    request.envelope.expected_frontier.scope.session = Some(session_scope);
    let binding = crate::application_operation_owner::application_operation_binding(&request)?
        .expect("ordinary run binding");
    let owner = session.application_operation_owner()?;
    owner.prepare(&binding)?;
    session.bind_application_operation(binding.clone())?;
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
            "accepted-run",
            1,
        )?)?;
    let frontier = |sequence| ApplicationFrontier {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: request.admission.scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        through_sequence: sequence,
        durable_cursor: "actual-session-cut".to_owned(),
    };
    assert!(
        crate::application_operation_owner::reconcile_application_operation_receipt(
            &request,
            &store.read_handle(),
            &frontier(u64::MAX)
        )?
        .is_none()
    );
    session.record_bound_conversation_run_admission("accepted-run")?;
    let proof =
        sigil_kernel::session::reconcile_application_operation(&owner.read_handle(), &binding)?
            .expect("causal admission");
    let receipt = crate::application_operation_owner::reconcile_application_operation_receipt(
        &request,
        &store.read_handle(),
        &frontier(proof.stream_sequence()),
    )?
    .expect("receipt");
    assert!(
        matches!(receipt, RuntimeApplicationDispatch::Settled(receipt) if matches!(receipt.outcome.as_deref(), Some(sigil_application::ApplicationCommandOutcome::ConversationRunAccepted { run_id }) if run_id.as_str() == "accepted-run"))
    );
    assert!(
        crate::application_operation_owner::application_operation_receipt_from_proof(
            &request,
            &binding,
            &proof,
            &frontier(proof.stream_sequence() - 1)
        )
        .is_err(),
        "an old observed cut cannot advertise this admission"
    );
    let mut changed = request.clone();
    changed.envelope.command =
        ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
            prompt: Some(SafeText::new("other input")?),
            options: None,
        });
    assert!(
        crate::application_operation_owner::application_operation_receipt_from_proof(
            &changed,
            &binding,
            &proof,
            &frontier(proof.stream_sequence())
        )
        .is_err()
    );
    Ok(())
}
