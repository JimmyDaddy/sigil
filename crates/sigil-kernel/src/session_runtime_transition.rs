//! Durable facts emitted by the owner of a session runtime transition.
//!
//! These records bind a route operation to its original command and source revision. They do
//! not issue route authority or restore live readiness after a process restart.

use serde::{Deserialize, Serialize};

use crate::{ResolvedModelRoute, RouteEgressTrustBinding};

/// Secret-free command identity frozen before a runtime transition can stop its old worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeCommandCauseV1 {
    pub application_instance: String,
    pub workspace_scope: Option<String>,
    pub session_scope: String,
    pub principal: String,
    pub client_epoch: u64,
    pub command_id: String,
    pub reservation_fingerprint: String,
}

/// Exact, host-owned runtime identity echoed only by a worker which has finished startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeReadyV1 {
    pub operation_id: String,
    pub route_revision: u64,
    pub worker_generation: u64,
    pub boot_identity: String,
}

/// Original route/trust/CAS binding; retries reuse this record instead of selecting a new source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeTransitionIntentV1 {
    pub cause: SessionRuntimeCommandCauseV1,
    pub source_frontier_sequence: u64,
    pub source_route_revision: u64,
    pub source_route: ResolvedModelRoute,
    pub source_trust: Option<RouteEgressTrustBinding>,
    pub target_provider: String,
    pub target_route: ResolvedModelRoute,
    pub target_trust: RouteEgressTrustBinding,
    pub configuration_fingerprint: String,
    pub reset_private_context: bool,
}

/// Append-only milestones. Configured is not Activated; a historical activation is not live Ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum SessionRuntimeTransitionPhaseV1 {
    Intent {
        binding: Box<SessionRuntimeTransitionIntentV1>,
    },
    Configured {
        intent_digest: String,
        route_revision: u64,
    },
    Activated {
        intent_digest: String,
        configured_sequence: u64,
        ready: SessionRuntimeReadyV1,
    },
}

/// One bounded fact written to the existing session control stream by its runtime controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeTransitionV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub transition: SessionRuntimeTransitionPhaseV1,
}

impl SessionRuntimeTransitionV1 {
    /// Checks shape only; source authority, phase ordering and live worker ownership belong to
    /// the runtime controller consuming the durable session stream.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.schema_version == 1,
            "unsupported runtime transition schema"
        );
        validate_identity(&self.operation_id)?;
        match &self.transition {
            SessionRuntimeTransitionPhaseV1::Intent { binding } => {
                for value in [
                    &binding.cause.application_instance,
                    &binding.cause.session_scope,
                    &binding.cause.principal,
                    &binding.cause.command_id,
                    &binding.target_provider,
                ] {
                    validate_identity(value)?;
                }
                if let Some(workspace) = &binding.cause.workspace_scope {
                    validate_identity(workspace)?;
                }
                anyhow::ensure!(
                    binding.cause.client_epoch != 0,
                    "runtime transition client epoch is missing"
                );
                validate_digest(&binding.cause.reservation_fingerprint)?;
                validate_digest(&binding.configuration_fingerprint)?;
                anyhow::ensure!(
                    binding.source_frontier_sequence >= binding.source_route_revision
                        && binding.source_route_revision != 0,
                    "runtime source revision is missing"
                );
                anyhow::ensure!(
                    binding.reset_private_context == (binding.source_route != binding.target_route),
                    "runtime private context boundary mismatch"
                );
            }
            SessionRuntimeTransitionPhaseV1::Configured {
                intent_digest,
                route_revision,
            } => {
                validate_record_checksum(intent_digest)?;
                anyhow::ensure!(*route_revision != 0, "runtime route revision is missing");
            }
            SessionRuntimeTransitionPhaseV1::Activated {
                intent_digest,
                configured_sequence,
                ready,
            } => {
                validate_record_checksum(intent_digest)?;
                validate_identity(&ready.boot_identity)?;
                anyhow::ensure!(
                    ready.operation_id == self.operation_id
                        && ready.worker_generation != 0
                        && ready.route_revision != 0
                        && *configured_sequence != 0,
                    "runtime activation binding is incomplete"
                );
            }
        }
        Ok(())
    }
}

fn validate_identity(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control),
        "runtime transition identity is invalid"
    );
    Ok(())
}

fn validate_digest(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "runtime transition digest is invalid"
    );
    Ok(())
}

fn validate_record_checksum(value: &str) -> anyhow::Result<()> {
    validate_digest(
        value
            .strip_prefix(crate::event::RECORD_CHECKSUM_PREFIX)
            .ok_or_else(|| {
                anyhow::anyhow!("runtime transition source checksum profile is invalid")
            })?,
    )
}

/// Validated phase records for one operation, read from its original durable stream.
#[derive(Debug, Clone, Default)]
pub struct SessionRuntimeTransitionReplayV1 {
    pub intent: Option<(SessionRuntimeTransitionIntentV1, crate::StoredEvent)>,
    pub configured: Option<(u64, crate::StoredEvent)>,
    pub activated: Option<(SessionRuntimeReadyV1, crate::StoredEvent)>,
}

impl SessionRuntimeTransitionReplayV1 {
    /// Replays only typed records and verifies every phase's original causal binding.
    pub fn from_records(
        records: &[crate::SessionStreamRecord],
        operation_id: &str,
    ) -> anyhow::Result<Self> {
        let mut replay = Self::default();
        for record in records {
            let Some(crate::SessionLogEntry::Control(
                crate::ControlEntry::SessionRuntimeTransitionV1(entry),
            )) = record.session_log_entry()?
            else {
                continue;
            };
            if entry.operation_id != operation_id {
                continue;
            }
            entry.validate()?;
            match entry.transition {
                SessionRuntimeTransitionPhaseV1::Intent { binding } => {
                    anyhow::ensure!(
                        replay.intent.is_none(),
                        "runtime operation has duplicate intent"
                    );
                    anyhow::ensure!(
                        binding.cause.session_scope == record.session_id(),
                        "runtime intent session mismatch"
                    );
                    replay.intent = Some((*binding, record.stored_event().clone()));
                }
                SessionRuntimeTransitionPhaseV1::Configured {
                    intent_digest,
                    route_revision,
                } => {
                    let (_, intent) = replay
                        .intent
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("runtime configuration has no intent"))?;
                    anyhow::ensure!(
                        intent.record_checksum == intent_digest && replay.configured.is_none(),
                        "runtime configuration cause mismatch"
                    );
                    replay.configured = Some((route_revision, record.stored_event().clone()));
                }
                SessionRuntimeTransitionPhaseV1::Activated {
                    intent_digest,
                    configured_sequence,
                    ready,
                } => {
                    let (_, intent) = replay
                        .intent
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("runtime activation has no intent"))?;
                    let (revision, configured) = replay.configured.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("runtime activation has no configuration")
                    })?;
                    anyhow::ensure!(
                        intent.record_checksum == intent_digest
                            && configured.stream_sequence == configured_sequence
                            && ready.route_revision == *revision
                            && replay.activated.is_none(),
                        "runtime activation cause mismatch"
                    );
                    replay.activated = Some((ready, record.stored_event().clone()));
                }
            }
        }
        Ok(replay)
    }
}

/// Returns the existing canonical route/trust revision, which is its last durable source sequence.
pub fn session_runtime_route_revision(
    records: &[crate::SessionStreamRecord],
) -> anyhow::Result<u64> {
    for record in records.iter().rev() {
        if matches!(
            record.session_log_entry()?,
            Some(crate::SessionLogEntry::Control(
                crate::ControlEntry::SessionIdentity { .. }
                    | crate::ControlEntry::SessionModelSelected { .. }
                    | crate::ControlEntry::SessionRouteRebound { .. }
                    | crate::ControlEntry::SessionRouteTrustBound { .. }
            ))
        ) {
            return Ok(record.stream_sequence());
        }
    }
    anyhow::bail!("runtime transition requires an existing route revision")
}

/// Commits route, trust and Configured through the session writer's existing crash-safe bundle.
/// The runtime caller must hold the attachment's route-mutation permit. Writer CAS revalidates
/// the original route revision, so a stale controller cannot overwrite another selection.
pub fn commit_session_runtime_configuration(
    store: &crate::JsonlSessionStore,
    operation_id: &str,
) -> anyhow::Result<crate::StoredEvent> {
    for _ in 0..8 {
        let records = store.read_event_records_writer()?;
        let replay = SessionRuntimeTransitionReplayV1::from_records(&records, operation_id)?;
        if let Some((_, configured)) = replay.configured {
            return Ok(configured);
        }
        let (binding, intent) = replay
            .intent
            .ok_or_else(|| anyhow::anyhow!("runtime transition intent is missing"))?;
        anyhow::ensure!(
            session_runtime_route_revision(&records)? == binding.source_route_revision,
            "runtime transition source route revision changed"
        );
        let current = crate::Session::load_from_store_for_control(store.clone())?;
        anyhow::ensure!(
            current.session_scope_id() == binding.cause.session_scope
                && current.resolved_model_route() == Some(&binding.source_route)
                && current.route_egress_trust_binding() == binding.source_trust,
            "runtime transition source route or trust changed"
        );
        let sequence = records
            .last()
            .map_or(0, crate::SessionStreamRecord::stream_sequence);
        let mut controls = Vec::new();
        if current.provider_name() != binding.target_provider
            || binding.source_route != binding.target_route
        {
            controls.push(crate::ControlEntry::SessionModelSelected {
                provider_name: binding.target_provider.clone(),
                model_name: binding.target_route.model_ref.model_id.clone(),
                resolved_model_route: binding.target_route.clone(),
            });
        }
        if !controls.is_empty() || binding.source_trust.as_ref() != Some(&binding.target_trust) {
            controls.push(crate::ControlEntry::SessionRouteTrustBound {
                route_semantic_fingerprint: binding.target_route.semantic_fingerprint.clone(),
                egress_trust_binding: binding.target_trust.clone(),
            });
        }
        let route_revision = if controls.is_empty() {
            binding.source_route_revision
        } else {
            sequence
                .checked_add(u64::try_from(controls.len())?)
                .ok_or_else(|| anyhow::anyhow!("runtime route revision exhausted"))?
        };
        let mut events = controls.into_iter().map(|control| (
            uuid::Uuid::new_v4().to_string(), crate::DurableEventType::SessionEntryRecorded,
            crate::EventClass::NonCritical, serde_json::json!({"session_log_entry": crate::SessionLogEntry::Control(control)}),
        )).collect::<Vec<_>>();
        let configured = SessionRuntimeTransitionV1 {
            schema_version: 1,
            operation_id: operation_id.to_owned(),
            transition: SessionRuntimeTransitionPhaseV1::Configured {
                intent_digest: intent.record_checksum.clone(),
                route_revision,
            },
        };
        configured.validate()?;
        events.push((uuid::Uuid::new_v4().to_string(), crate::DurableEventType::SessionRuntimeTransitionV1,
            crate::EventClass::Critical, serde_json::json!({"session_log_entry": crate::SessionLogEntry::Control(crate::ControlEntry::SessionRuntimeTransitionV1(configured))})));
        if let Some(mut written) = store.append_crash_safe_events_if(events, |current| {
            Ok(current
                .last()
                .map_or(0, crate::SessionStreamRecord::stream_sequence)
                == sequence
                && session_runtime_route_revision(current)? == binding.source_route_revision)
        })? {
            return written
                .pop()
                .ok_or_else(|| anyhow::anyhow!("runtime configuration bundle returned no marker"));
        }
    }
    anyhow::bail!("runtime transition could not acquire the current writer frontier")
}

/// Binds Ready to the still-current Configured route under the same writer CAS. A matching
/// historical activation is reused, while another route revision cannot inherit this Ready.
pub fn commit_session_runtime_activation(
    store: &crate::JsonlSessionStore,
    operation_id: &str,
    ready: SessionRuntimeReadyV1,
) -> anyhow::Result<crate::StoredEvent> {
    for _ in 0..8 {
        let records = store.read_event_records_writer()?;
        let replay = SessionRuntimeTransitionReplayV1::from_records(&records, operation_id)?;
        if let Some((existing, event)) = replay.activated {
            anyhow::ensure!(
                existing == ready,
                "runtime operation already activated another worker"
            );
            return Ok(event);
        }
        let (intent, intent_record) = replay
            .intent
            .ok_or_else(|| anyhow::anyhow!("runtime intent missing"))?;
        let (revision, configured) = replay
            .configured
            .ok_or_else(|| anyhow::anyhow!("runtime configuration missing"))?;
        anyhow::ensure!(
            ready.operation_id == operation_id
                && ready.route_revision == revision
                && session_runtime_route_revision(&records)? == revision,
            "runtime Ready route revision is stale"
        );
        let current = crate::Session::load_from_store_for_control(store.clone())?;
        anyhow::ensure!(
            current.resolved_model_route() == Some(&intent.target_route)
                && current.route_egress_trust_binding().as_ref() == Some(&intent.target_trust),
            "runtime Ready route or trust no longer configured"
        );
        let sequence = records
            .last()
            .map_or(0, crate::SessionStreamRecord::stream_sequence);
        let activated = SessionRuntimeTransitionV1 {
            schema_version: 1,
            operation_id: operation_id.to_owned(),
            transition: SessionRuntimeTransitionPhaseV1::Activated {
                intent_digest: intent_record.record_checksum,
                configured_sequence: configured.stream_sequence,
                ready: ready.clone(),
            },
        };
        activated.validate()?;
        let events = vec![(
            uuid::Uuid::new_v4().to_string(),
            crate::DurableEventType::SessionRuntimeTransitionV1,
            crate::EventClass::Critical,
            serde_json::json!({"session_log_entry": crate::SessionLogEntry::Control(
                crate::ControlEntry::SessionRuntimeTransitionV1(activated))}),
        )];
        if let Some(mut events) = store.append_crash_safe_events_if(events, |current| {
            Ok(current
                .last()
                .map_or(0, crate::SessionStreamRecord::stream_sequence)
                == sequence
                && session_runtime_route_revision(current)? == revision)
        })? {
            return events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("activation batch returned no event"));
        }
    }
    anyhow::bail!("runtime activation could not acquire current writer frontier")
}
