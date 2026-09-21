use super::*;
use crate::application_reservation_store::ManagedApplicationReservationStore;
use crate::application_service::{
    RuntimeApplicationCommandExecutor, RuntimeApplicationDeliveryAcker,
    RuntimeApplicationProjectionSource, RuntimeApplicationReservationAdmission,
    RuntimeApplicationReservationStore, RuntimeApplicationService,
};
use crate::managed_storage_writer::StorageWriterChannelV1;
use crate::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, grant_for_channel_with_context,
};
use futures::future::{BoxFuture, ready};
use sigil_application::{
    ApplicationCommandReceipt, ApplicationDomainReceipt, ApplicationError, ApplicationPort,
    CommandNoEffectProof, OpenProjectionRequest, ProjectionDeliveryAck, ProjectionPage,
    ProjectionPageRequest, ProjectionSnapshot, UncertainCommandReceipt,
};

fn production_writer(root: &std::path::Path) -> Arc<ManagedStorageWriterAdapterV1> {
    use sigil_kernel::{
        capability_issuer::KernelCapabilityBrokerV1,
        resource::{AuthorityGeneration, CanonicalHash},
    };
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([31; 32]),
    };
    let manifest = CanonicalHash::from_bytes([32; 32]);
    let mut grants = AuthorityStorageGrantTableV1::new();
    for (index, channel) in [
        StorageWriterChannelV1::ApplicationControlLog,
        StorageWriterChannelV1::ApplicationCommandIndex,
        StorageWriterChannelV1::ApplicationControlRecovery,
    ]
    .into_iter()
    .enumerate()
    {
        grants
            .register(grant_for_channel_with_context(
                channel,
                50 + index as u8,
                generation,
                manifest,
            ))
            .expect("resume fixture storage channel grant should register");
    }
    let service = Arc::new(
        AuthorityManagedStorageServiceV1::new_with_state_root(grants, generation, root)
            .expect("resume fixture managed storage service should initialize"),
    );
    Arc::new(ManagedStorageWriterAdapterV1::with_storage_issuer(
        service,
        root.to_owned(),
        manifest,
        Arc::new(KernelCapabilityBrokerV1::new()),
    ))
}

struct UnusedProjection;
impl RuntimeApplicationProjectionSource for UnusedProjection {
    fn open_projection(
        &self,
        _: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        Box::pin(ready(Err(ApplicationError::Unavailable)))
    }
    fn page(
        &self,
        _: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        Box::pin(ready(Err(ApplicationError::Unavailable)))
    }
}
struct Delivery;
impl RuntimeApplicationDeliveryAcker for Delivery {
    fn acknowledge(
        &self,
        _: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(Ok(())))
    }
}

struct Executor {
    owner: Arc<RuntimeSessionController>,
    route: ResolvedModelRoute,
}
impl RuntimeApplicationCommandExecutor for Executor {
    fn session_runtime_resume_binding(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>> {
        Box::pin(ready(
            self.owner
                .resume_effect_binding(&request)
                .map_err(|_| ApplicationError::Unavailable),
        ))
    }
    fn bind_effect(
        &self,
        request: ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>> {
        Box::pin(ready(
            self.owner
                .bind_effect(&request, &self.route, key, fingerprint)
                .map_err(|_| ApplicationError::Unavailable),
        ))
    }
    fn reconcile(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<Option<RuntimeApplicationDispatch>, ApplicationError>> {
        Box::pin(ready(
            self.owner
                .reconcile(&request)
                .map_err(|_| ApplicationError::Unavailable),
        ))
    }
    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>> {
        Box::pin(ready(
            self.owner
                .dispatch(&request)
                .map_err(|_| ApplicationError::Unavailable),
        ))
    }
}

/// Only the exact gate append boundary fails; every other operation uses the real RA writer.
struct FailEffectGate {
    inner: Arc<ManagedApplicationReservationStore>,
}
macro_rules! delegate_store {
    ($name:ident ($($argument:ident: $ty:ty),*) -> $output:ty) => {
        fn $name(&self, $($argument: $ty),*) -> BoxFuture<'static, Result<$output, ApplicationError>> {
            self.inner.$name($($argument),*)
        }
    };
}
impl RuntimeApplicationReservationStore for FailEffectGate {
    fn forward_guard(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<
        Option<Box<dyn sigil_kernel::managed_storage::ManagedStorageForwardGuardV1>>,
        ApplicationError,
    > {
        self.inner.forward_guard(request)
    }
    delegate_store!(reserve(key: CommandReservationKey, fingerprint: String, request: ApplicationCommandRequest) -> RuntimeApplicationReservationAdmission);
    delegate_store!(mark_dispatch_started(key: CommandReservationKey, fingerprint: String) -> ());
    delegate_store!(resume_session_runtime_effect(key: CommandReservationKey, fingerprint: String, binding: CommandEffectBinding) -> ());
    delegate_store!(mark_domain_committed(key: CommandReservationKey, fingerprint: String, receipt: ApplicationDomainReceipt) -> ());
    delegate_store!(mark_confirmed_no_effect(key: CommandReservationKey, fingerprint: String, proof: CommandNoEffectProof) -> ());
    delegate_store!(mark_uncertain(key: CommandReservationKey, fingerprint: String, receipt: UncertainCommandReceipt) -> ());
    delegate_store!(settle(key: CommandReservationKey, fingerprint: String, receipt: ApplicationCommandReceipt) -> ());
    fn mark_effect_started(
        &self,
        _: CommandReservationKey,
        _: String,
        _: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(Err(ApplicationError::Unavailable)))
    }
}

fn service(
    fixture: &Fixture,
    reservations: Arc<dyn RuntimeApplicationReservationStore>,
) -> Result<RuntimeApplicationService> {
    Ok(RuntimeApplicationService::new(
        Arc::new(UnusedProjection),
        Arc::new(Executor {
            owner: Arc::new(fixture.controller()?),
            route: fixture.route.clone(),
        }),
        reservations,
        Arc::new(Delivery),
    ))
}

fn bounded(test: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = sender.send(test());
    });
    let result = match receiver.recv_timeout(std::time::Duration::from_secs(15)) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => anyhow::bail!(
            "runtime resume exceeded bounded deadline; possible namespace lock recursion"
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            handle.join().expect("resume fixture thread panicked");
            anyhow::bail!("resume fixture result channel disconnected");
        }
    };
    handle.join().expect("resume fixture thread");
    result
}

#[test]
fn failed_effect_gate_reopens_and_resumes_same_intent_through_real_ra_without_lock_recursion()
-> Result<()> {
    bounded(|| {
        let mut fixture = Fixture::new(false)?;
        let authority = tempfile::tempdir()?;
        let writer = production_writer(authority.path());
        let store = Arc::new(ManagedApplicationReservationStore::open(
            Arc::clone(&writer),
            "resume",
        )?);
        fixture.request.admission.command_journal = store.command_journal_binding()?;
        let original_request = fixture.request.clone();
        let original_fingerprint = command_fingerprint(&original_request)?;
        let failed = service(
            &fixture,
            Arc::new(FailEffectGate {
                inner: Arc::clone(&store),
            }),
        )?;
        assert!(futures::executor::block_on(failed.execute(original_request.clone())).is_err());
        let before = fixture.replay()?;
        assert!(before.intent.is_some());
        assert!(before.configured.is_none());
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 0);
        drop(failed);
        drop(store);
        let store = Arc::new(ManagedApplicationReservationStore::open(
            Arc::clone(&writer),
            "resume",
        )?);
        let resumed = service(&fixture, store)?;
        let mut changed = original_request.clone();
        changed.envelope.expected_frontier.through_sequence += 1;
        assert!(
            futures::executor::block_on(resumed.resume_session_runtime_transition(changed))
                .is_err()
        );
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 0);
        assert!(matches!(
            futures::executor::block_on(
                resumed.resume_session_runtime_transition(original_request.clone())
            )?,
            ApplicationCommandReceipt::Settled(_)
        ));
        let after = fixture.replay()?;
        assert_eq!(before.intent, after.intent);
        assert!(after.configured.is_some());
        assert!(after.activated.is_some());
        assert_eq!(
            command_fingerprint(&original_request)?,
            original_fingerprint
        );
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            futures::executor::block_on(resumed.execute(original_request))?,
            ApplicationCommandReceipt::Replayed(_)
        ));
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 1);
        Ok(())
    })
}

#[test]
fn historical_activation_does_not_skip_live_gate_resume_or_spawn_twice() -> Result<()> {
    bounded(|| {
        let mut fixture = Fixture::new(false)?;
        fixture.host.fail_open.store(true, Ordering::SeqCst);
        let authority = tempfile::tempdir()?;
        let writer = production_writer(authority.path());
        let store = Arc::new(ManagedApplicationReservationStore::open(
            writer,
            "gate-resume",
        )?);
        fixture.request.admission.command_journal = store.command_journal_binding()?;
        let application = service(&fixture, store)?;
        let first = futures::executor::block_on(application.execute(fixture.request.clone()))?;
        assert!(matches!(first, ApplicationCommandReceipt::Uncertain(_)));
        let activated = fixture.replay()?.activated.expect("durable Activated").1;
        assert!(!fixture.host.live.load(Ordering::Acquire));
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 1);
        // Ordinary query/replay preserves historical evidence without mutating live readiness.
        let historical = futures::executor::block_on(application.execute(fixture.request.clone()))?;
        assert!(matches!(historical, ApplicationCommandReceipt::Replayed(_)));
        assert!(!fixture.host.live.load(Ordering::Acquire));
        let resumed = futures::executor::block_on(
            application.resume_session_runtime_transition(fixture.request.clone()),
        )?;
        assert!(matches!(resumed, ApplicationCommandReceipt::Settled(_)));
        assert!(fixture.host.live.load(Ordering::Acquire));
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .replay()?
                .activated
                .expect("same Activated")
                .1
                .event_id,
            activated.event_id
        );
        Ok(())
    })
}

#[test]
fn sealed_command_generation_refuses_existing_runtime_intent_resume() -> Result<()> {
    bounded(|| {
        let mut fixture = Fixture::new(false)?;
        let authority = tempfile::tempdir()?;
        let writer = production_writer(authority.path());
        let store = Arc::new(ManagedApplicationReservationStore::open(
            Arc::clone(&writer),
            "sealed-resume",
        )?);
        fixture.request.admission.command_journal = store.command_journal_binding()?;
        let failed = service(
            &fixture,
            Arc::new(FailEffectGate {
                inner: Arc::clone(&store),
            }),
        )?;
        assert!(futures::executor::block_on(failed.execute(fixture.request.clone())).is_err());
        let before = fixture.replay()?;
        drop(failed);
        drop(store);
        let lease = writer.acquire_named(
            StorageWriterChannelV1::ApplicationControlLog,
            "sealed-resume",
        )?;
        writer.write_record(&lease, b"{broken-tail")?;
        writer.finalize(lease)?;
        let store = Arc::new(ManagedApplicationReservationStore::open(
            Arc::clone(&writer),
            "sealed-resume",
        )?);
        let preview = store.preview_control_log_recovery()?;
        let activated = store.seal_and_rotate_control_log(&preview)?;
        assert_eq!(activated.command_generation, 1);
        let resumed = service(&fixture, store)?;
        assert!(
            futures::executor::block_on(
                resumed.resume_session_runtime_transition(fixture.request.clone())
            )
            .is_err()
        );
        let mut changed_generation = fixture.request.clone();
        changed_generation.admission.command_journal = Some(activated);
        assert!(
            futures::executor::block_on(
                resumed.resume_session_runtime_transition(changed_generation)
            )
            .is_err()
        );
        assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.replay()?.intent, before.intent);
        assert!(fixture.replay()?.configured.is_none());
        assert!(fixture.replay()?.activated.is_none());
        Ok(())
    })
}
