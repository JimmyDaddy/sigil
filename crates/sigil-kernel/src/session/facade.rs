use super::*;
use crate::{ResolvedModelRoute, TaskGuidancePromotedEntry};

/// In-memory session state backed by an optional append-only JSONL store.
#[derive(Debug)]
pub struct Session {
    pub(super) session_scope_id: String,
    pub(super) provider_name: String,
    pub(super) model_name: String,
    pub(super) resolved_model_route: Option<ResolvedModelRoute>,
    pub(super) entries: Vec<SessionLogEntry>,
    pub(super) store: Option<JsonlSessionStore>,
    pub(super) stats: SessionStats,
    pub(super) runtime_attachments: SessionRuntimeAttachments,
    durable_session_entry_count: Option<u64>,
    reconstruction_records: Option<Arc<[SessionStreamRecord]>>,
}

#[derive(Clone, Default)]
pub(super) struct SessionRuntimeAttachments {
    user_url_capability_registrar: Option<Arc<dyn crate::UserUrlCapabilityRegistrar>>,
    image_attachment_resolver: Option<Arc<dyn crate::ImageAttachmentResolver>>,
}

/// Immutable live-session material proven to correspond to one durable projection frontier.
#[derive(Clone)]
pub struct StableCompactionSnapshot {
    frontier: ActiveProjectionFrontier,
    store: JsonlSessionStore,
    session_scope_id: String,
    provider_name: String,
    model_name: String,
    resolved_model_route: Option<ResolvedModelRoute>,
    entries: Vec<SessionLogEntry>,
    stats: SessionStats,
    runtime_attachments: SessionRuntimeAttachments,
    durable_session_entry_count: u64,
}

impl std::fmt::Debug for StableCompactionSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StableCompactionSnapshot")
            .field("frontier", &self.frontier)
            .field("session_scope_id", &self.session_scope_id)
            .field("provider_name", &self.provider_name)
            .field("model_name", &self.model_name)
            .field("resolved_model_route", &self.resolved_model_route)
            .field("entry_count", &self.entries.len())
            .field("stats", &self.stats)
            .field("runtime_attachments", &self.runtime_attachments)
            .finish()
    }
}

impl StableCompactionSnapshot {
    /// Returns the exact durable frontier proven by this snapshot.
    #[must_use]
    pub fn frontier(&self) -> &ActiveProjectionFrontier {
        &self.frontier
    }

    /// Returns the last durable stream sequence at the current frontier (0 for an empty stream).
    #[must_use]
    pub fn durable_frontier_sequence(&self) -> u64 {
        self.frontier
            .cursor()
            .map_or(0, |cursor| cursor.last_applied_stream_sequence)
    }

    /// Returns the immutable live entry projection at the frontier.
    #[must_use]
    pub fn entries(&self) -> &[SessionLogEntry] {
        &self.entries
    }

    /// Returns the immutable usage snapshot captured with the entries.
    #[must_use]
    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    /// Materializes a store-backed compaction session only while the durable frontier is unchanged.
    ///
    /// The returned session is bound to the same shared coordinator and may append compaction
    /// lifecycle, provider-attempt and usage records without reconstructing the store from a path.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing projection cannot be read safely.
    pub fn materialize_compaction_session(&self) -> Result<Option<Session>> {
        let active = self.store.active_projection_snapshot()?;
        if active.frontier() != &self.frontier
            || active.durable_session_entry_count() != self.durable_session_entry_count
        {
            return Ok(None);
        }
        Ok(Some(Session {
            session_scope_id: self.session_scope_id.clone(),
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            resolved_model_route: self.resolved_model_route.clone(),
            entries: self.entries.clone(),
            store: Some(self.store.clone()),
            stats: self.stats.clone(),
            runtime_attachments: self.runtime_attachments.clone(),
            durable_session_entry_count: Some(self.durable_session_entry_count),
            reconstruction_records: None,
        }))
    }

    /// Returns the captured process-local URL capability registrar.
    #[must_use]
    pub fn user_url_capability_registrar(
        &self,
    ) -> Option<Arc<dyn crate::UserUrlCapabilityRegistrar>> {
        self.runtime_attachments
            .user_url_capability_registrar
            .clone()
    }

    /// Returns the captured process-local image attachment resolver.
    #[must_use]
    pub fn image_attachment_resolver(&self) -> Option<Arc<dyn crate::ImageAttachmentResolver>> {
        self.runtime_attachments.image_attachment_resolver.clone()
    }
}

impl std::fmt::Debug for SessionRuntimeAttachments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRuntimeAttachments")
            .field(
                "user_url_capability_registrar",
                &self
                    .user_url_capability_registrar
                    .as_ref()
                    .map(|_| "configured"),
            )
            .field(
                "image_attachment_resolver",
                &self
                    .image_attachment_resolver
                    .as_ref()
                    .map(|_| "configured"),
            )
            .finish()
    }
}

struct AssembledRequest {
    request: CompletionRequest,
    prefix_snapshot: PrefixSnapshot,
}

pub(super) struct RuntimeContextResolution {
    pub(super) staged_snapshot: Option<RuntimeContextSnapshotV2>,
    pub(super) effective_summary: Option<PrefixRuntimeContextSummary>,
}

impl Session {
    /// Creates a new in-memory session with the given provider and model identity.
    pub fn new(provider_name: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            session_scope_id: uuid::Uuid::new_v4().to_string(),
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            resolved_model_route: None,
            entries: Vec::new(),
            store: None,
            stats: SessionStats::default(),
            runtime_attachments: SessionRuntimeAttachments::default(),
            durable_session_entry_count: None,
            reconstruction_records: None,
        }
    }

    /// Creates a new in-memory session bound to one exact secret-free provider route.
    pub fn new_with_route(provider_name: impl Into<String>, route: ResolvedModelRoute) -> Self {
        Self {
            session_scope_id: uuid::Uuid::new_v4().to_string(),
            provider_name: provider_name.into(),
            model_name: route.model_ref.model_id.clone(),
            resolved_model_route: Some(route),
            entries: Vec::new(),
            store: None,
            stats: SessionStats::default(),
            runtime_attachments: SessionRuntimeAttachments::default(),
            durable_session_entry_count: None,
            reconstruction_records: None,
        }
    }

    /// Attaches a durable JSONL store to the session.
    pub fn with_store(mut self, store: JsonlSessionStore) -> Self {
        self.session_scope_id = session_id_for_path(store.path());
        let store_is_empty = std::fs::metadata(store.path())
            .map(|metadata| metadata.len() == 0)
            .unwrap_or_else(|_| !store.path().exists());
        self.durable_session_entry_count = (self.entries.is_empty() && store_is_empty).then_some(0);
        self.store = Some(store);
        self
    }

    /// Returns the scheduler-facing durable projection for a store-backed session.
    ///
    /// In-memory sessions return `Ok(None)` because they have no durable frontier to prove.
    ///
    /// # Errors
    ///
    /// Returns an error when the attached durable stream cannot be recovered or projected.
    pub fn active_projection_snapshot(&self) -> Result<Option<ActiveSessionProjectionSnapshot>> {
        self.store
            .as_ref()
            .map(JsonlSessionStore::active_projection_snapshot)
            .transpose()
    }

    /// Returns the append-only context epoch currently governing V2 tool-result model views.
    pub fn active_tool_output_context_epoch_id(&self) -> Result<String> {
        Ok(self
            .active_projection_snapshot()?
            .map(|snapshot| snapshot.tool_output_pressure().active_epoch_id)
            .unwrap_or_else(|| "context-epoch:root".to_owned()))
    }

    /// Publishes one frontier-bound tool-output aging activation through the attached authority.
    ///
    /// In-memory sessions cannot prove a durable frontier and therefore return `Ok(None)`.
    /// Store-backed sessions compare the supplied frontier against the shared incremental
    /// projection and append atomically without replaying the JSONL stream.
    pub fn append_tool_output_aging_activation(
        &self,
        expected_frontier: &ActiveProjectionFrontier,
        activation: ToolOutputAgingActivatedV1,
    ) -> Result<Option<StoredEvent>> {
        self.store
            .as_ref()
            .map(|store| store.append_tool_output_aging_activation(expected_frontier, activation))
            .transpose()
            .map(Option::flatten)
    }

    /// Registers a durable projection observer without exposing the underlying store or writer.
    ///
    /// In-memory sessions return `Ok(None)`. Dropping the returned subscription unregisters the
    /// observer from all store handles sharing this canonical session.
    pub fn register_active_projection_observer(
        &self,
        observer: Arc<dyn ActiveProjectionObserver>,
    ) -> Result<Option<ActiveProjectionSubscription>> {
        Ok(self
            .store
            .as_ref()
            .map(|store| store.register_active_projection_observer(observer)))
    }

    /// Captures immutable compaction material only when the live entry projection is proven to
    /// have consumed every durable session-entry event through `expected_frontier`.
    ///
    /// Returns `Ok(None)` for in-memory sessions, a stale expected frontier, or a live session
    /// whose entry frontier was invalidated by an externally persisted adoption.
    ///
    /// # Errors
    ///
    /// Returns an error when the active durable projection cannot be read safely.
    pub fn stable_compaction_snapshot(
        &self,
        expected_frontier: &ActiveProjectionFrontier,
    ) -> Result<Option<StableCompactionSnapshot>> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        let active = store.active_projection_snapshot()?;
        let Some(live_entry_count) = self.durable_session_entry_count else {
            return Ok(None);
        };
        if active.frontier() != expected_frontier
            || active.durable_session_entry_count() != live_entry_count
        {
            return Ok(None);
        }
        Ok(Some(StableCompactionSnapshot {
            frontier: expected_frontier.clone(),
            store: store.clone(),
            session_scope_id: self.session_scope_id.clone(),
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            resolved_model_route: self.resolved_model_route.clone(),
            entries: self.entries.clone(),
            stats: self.stats.clone(),
            runtime_attachments: self.runtime_attachments.clone(),
            durable_session_entry_count: live_entry_count,
        }))
    }

    /// Rehydrates a session from a preloaded list of entries.
    pub fn from_entries(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        entries: Vec<SessionLogEntry>,
    ) -> Self {
        let fallback_provider_name = provider_name.into();
        let fallback_model_name = model_name.into();
        let session_scope_id = uuid::Uuid::new_v4().to_string();
        let (mut entries, audit_needed) = validated_recovered_entries(&session_scope_id, entries);
        if audit_needed {
            entries.push(unsafe_external_recovery_audit_entry());
        }
        let stats = session_stats_from_entries(&entries);
        let (provider_name, model_name) = session_identity_from_entries(&entries)
            .unwrap_or((fallback_provider_name, fallback_model_name));
        Self {
            session_scope_id,
            provider_name,
            model_name,
            resolved_model_route: session_resolved_route_from_entries(&entries),
            entries,
            store: None,
            stats,
            runtime_attachments: SessionRuntimeAttachments::default(),
            durable_session_entry_count: None,
            reconstruction_records: None,
        }
    }

    /// Loads a session from the durable store and requires its current persisted identity.
    pub fn load_from_store(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        store: JsonlSessionStore,
    ) -> Result<Self> {
        Self::load_from_store_with_route(provider_name, model_name, None, store)
    }

    /// Reconstructs the privacy-safe session source at one exact provider-request frontier.
    ///
    /// This is a read-only replay surface: records after the frontier are ignored, no recovery or
    /// reconciliation events are appended, and no durable store is attached to the returned
    /// session. Callers must still supply the same stable runtime inputs (tools, memory policy,
    /// transient system messages, route options) to the normal request builder and verify the
    /// result with [`crate::ProviderRequestEnvelopeV1::verify_reconstructed_request_at_frontier`].
    pub fn reconstruct_from_provider_frontier(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        session_path: impl AsRef<Path>,
        frontier: &crate::ProviderRequestSourceFrontierV1,
    ) -> Result<Self> {
        let fallback_provider_name = provider_name.into();
        let fallback_model_name = model_name.into();
        let records = JsonlSessionStore::read_event_records_through_provider_frontier(
            session_path,
            frontier,
        )?;
        let entries = session_entries_from_records(&records)?;
        let (entries, audit_needed) = validated_recovered_entries(&frontier.session_id, entries);
        if audit_needed {
            bail!("provider request frontier requires unsafe external recovery");
        }
        let stats = session_stats_from_entries(&entries);
        let (provider_name, model_name) = session_identity_from_entries(&entries)
            .unwrap_or((fallback_provider_name, fallback_model_name));
        Ok(Self {
            session_scope_id: frontier.session_id.clone(),
            provider_name,
            model_name,
            resolved_model_route: session_resolved_route_from_entries(&entries),
            entries,
            store: None,
            stats,
            runtime_attachments: SessionRuntimeAttachments::default(),
            durable_session_entry_count: None,
            reconstruction_records: Some(records.into()),
        })
    }

    /// Loads a durable session while supplying the exact route for a newly initialized stream.
    pub fn load_from_store_with_route(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        initial_route: Option<ResolvedModelRoute>,
        store: JsonlSessionStore,
    ) -> Result<Self> {
        Self::load_from_store_with_route_and_trust(
            provider_name,
            model_name,
            initial_route,
            None,
            store,
        )
    }

    /// Loads a durable session and atomically initializes a new route epoch with its trust proof.
    pub fn load_from_store_with_route_and_trust(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        initial_route: Option<ResolvedModelRoute>,
        initial_route_trust: Option<crate::RouteEgressTrustBinding>,
        store: JsonlSessionStore,
    ) -> Result<Self> {
        let initial_provider_name = provider_name.into();
        let initial_model_name = model_name.into();
        // Establish the V2 session envelope (including tail repair and identity) before the
        // continuation coordinator reads the stream. Otherwise coordinator recovery can expose
        // a repaired-but-not-yet-initialized file to concurrent readers during startup.
        let (entries, records, provider_name, model_name) = store.load_entries_writer_reconciled(
            initial_provider_name,
            initial_model_name,
            initial_route,
            initial_route_trust,
        )?;
        ProviderContinuationPayloadCoordinator::for_store(store.clone())?
            .recover_from_records(&records)
            .context("failed to recover provider continuation payload lifecycle")?;
        let durable_session_entry_count = Some(
            store
                .active_projection_snapshot()?
                .durable_session_entry_count(),
        );
        let session_scope_id = session_id_for_path(store.path());
        let (entries, audit_needed) = validated_recovered_entries(&session_scope_id, entries);
        let stats = session_stats_from_entries(&entries);
        let mut session = Self {
            session_scope_id,
            provider_name,
            model_name,
            resolved_model_route: session_resolved_route_from_entries(&entries),
            entries,
            store: Some(store),
            stats,
            runtime_attachments: SessionRuntimeAttachments::default(),
            durable_session_entry_count,
            reconstruction_records: None,
        };
        if audit_needed {
            session.append_control(unsafe_external_recovery_audit_control())?;
        }
        let recovered_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        crate::reconcile_unfinished_run_cancellations(&mut session, recovered_at_ms)?;
        crate::reconcile_plan_review_attempts(&mut session, recovered_at_ms)?;
        Ok(session)
    }

    /// Appends a single entry to the in-memory log and durable store when present.
    pub fn append(&mut self, entry: SessionLogEntry) -> Result<()> {
        match &entry {
            SessionLogEntry::ToolResultV3(result) => result.validate()?,
            SessionLogEntry::RuntimeContextSnapshotV2(snapshot) => snapshot.validate()?,
            SessionLogEntry::Control(control) => {
                control.validate_durable_contract()?;
                self.validate_user_input_controls(std::iter::once(control))?;
                self.validate_plan_controls(std::iter::once(control))?;
                if let ControlEntry::ToolArtifactRead(receipt) = control {
                    receipt.validate()?;
                }
            }
            _ => {}
        }
        let event = self
            .store
            .as_ref()
            .map(|store| store.append_session_entry_event(&entry))
            .transpose()?;
        self.entries.push(entry);
        if let Some(event) = event {
            self.advance_durable_session_entry_count(&[event]);
        }
        Ok(())
    }

    fn advance_durable_session_entry_count(&mut self, events: &[StoredEvent]) {
        let Some(previous) = self.durable_session_entry_count else {
            return;
        };
        let appended_entry_count = events
            .iter()
            .map(|event| {
                SessionStreamRecord::Stored(event.clone())
                    .session_log_entry()
                    .map(|entry| u64::from(entry.is_some()))
            })
            .collect::<Result<Vec<_>>>();
        let Ok(appended_entry_count) = appended_entry_count else {
            self.durable_session_entry_count = None;
            return;
        };
        let Some(store) = self.store.as_ref() else {
            self.durable_session_entry_count = None;
            return;
        };
        let Ok(snapshot) = store.active_projection_snapshot() else {
            self.durable_session_entry_count = None;
            return;
        };
        let expected = appended_entry_count
            .into_iter()
            .try_fold(previous, u64::checked_add);
        // A detached control path may append a session entry while an active run owns this
        // `Session`. Preserve the exact number of entries consumed by this live projection while
        // it is behind the durable projection; `record_durably_appended_controls` can then close
        // that known gap at the run boundary. A stable snapshot still requires equality.
        self.durable_session_entry_count =
            expected.filter(|expected| snapshot.durable_session_entry_count() >= *expected);
    }

    fn advance_durable_only_events(&mut self, events: &[StoredEvent]) {
        let all_durable_only = events.iter().all(|event| {
            SessionStreamRecord::Stored(event.clone())
                .session_log_entry()
                .is_ok_and(|entry| entry.is_none())
        });
        if all_durable_only {
            self.advance_durable_session_entry_count(events);
        } else if self.store.is_some() {
            self.durable_session_entry_count = None;
        }
    }

    pub fn append_user_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::User(message))
    }

    pub fn append_assistant_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::Assistant(message))
    }

    fn append_runtime_context_snapshot_v2(
        &mut self,
        snapshot: RuntimeContextSnapshotV2,
    ) -> Result<()> {
        snapshot.validate()?;
        self.append(SessionLogEntry::RuntimeContextSnapshotV2(snapshot))
    }

    pub(crate) fn ensure_frozen_request_runtime_context_v2(
        &mut self,
        frozen_request: &crate::FrozenProviderRequestMaterial,
    ) -> Result<()> {
        if frozen_request.session_scope_id() != self.session_scope_id() {
            bail!("frozen request runtime context belongs to a different session");
        }
        let durable = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::RuntimeContextSnapshotV2(snapshot) => {
                    Some((snapshot.message.id.as_str(), snapshot))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let mut missing = Vec::new();
        for (index, message) in frozen_request.request().messages.iter().enumerate() {
            if !is_runtime_context_v2_message(message) {
                continue;
            }
            if let Some(snapshot) = durable.get(message.id.as_str()) {
                if serde_json::to_value(&snapshot.message)? != serde_json::to_value(message)? {
                    bail!(
                        "frozen request runtime context conflicts with durable snapshot {}",
                        message.id
                    );
                }
                continue;
            }
            let source_tail_message_id = index
                .checked_sub(1)
                .and_then(|tail_index| frozen_request.request().messages.get(tail_index))
                .map(|tail| tail.id.clone());
            missing.push(RuntimeContextSnapshotV2::from_provider_message(
                message.clone(),
                source_tail_message_id,
            )?);
        }
        let [snapshot] = missing.as_slice() else {
            if missing.is_empty() {
                return Ok(());
            }
            bail!("frozen request contains multiple non-durable runtime context snapshots v2");
        };
        self.append_runtime_context_snapshot_v2(snapshot.clone())
    }

    /// Crate-local helper for tests that need a current durable tool-result record.
    #[cfg(test)]
    pub(crate) fn append_test_tool_result(&mut self, result: ToolResult) -> Result<()> {
        let artifact_store = self.tool_artifact_store();
        let (recorded, _) = ToolResultRecordedV3::capture(
            &result,
            artifact_store.as_ref(),
            ToolArtifactSensitivity::Ordinary,
        )?;
        self.append(SessionLogEntry::ToolResultV3(recorded))
    }

    #[must_use]
    pub fn tool_artifact_store(&self) -> Option<ToolArtifactStore> {
        self.store
            .as_ref()
            .map(ToolArtifactStore::for_session_store)
    }

    /// Persists one tool result and its body-free receipts/provenance as one crash-safe bundle.
    ///
    /// The tool result is deliberately first. Even on media that persists only a prefix before a
    /// crash, recovery can never expose a receipt without the provider-visible result that closes
    /// the corresponding tool call. The writer's append-bundle intent then recovers the multi-
    /// record range as all-or-none.
    pub(crate) fn append_tool_result_bundle(
        &mut self,
        result: ToolResultRecordedV3,
        controls: Vec<ControlEntry>,
    ) -> Result<()> {
        result.validate()?;
        let artifact_ref = result
            .artifact
            .descriptor()
            .map(|descriptor| descriptor.artifact_ref.clone());
        let message = result.model_message()?;
        for control in &controls {
            match control {
                ControlEntry::WebUrlCapabilityDescriptor(descriptor) => {
                    if descriptor.session_scope_id != self.session_scope_id {
                        bail!("web URL capability descriptor belongs to a different session scope");
                    }
                    if descriptor.durable_entry_id != message.id {
                        bail!("web URL capability descriptor belongs to a different tool result");
                    }
                    descriptor.validate()?;
                }
                ControlEntry::ExternalProvenance(provenance) => {
                    if provenance.session_scope_id != self.session_scope_id {
                        bail!("external provenance belongs to a different session scope");
                    }
                    provenance.validate_against_message(&message)?;
                }
                ControlEntry::ToolArtifactRead(receipt) => {
                    receipt.validate()?;
                    if result.tool_name != "read_tool_artifact" || receipt.call_id != result.call_id
                    {
                        bail!("tool artifact read receipt belongs to a different tool result");
                    }
                }
                _ => bail!("tool result bundle contains an unsupported control entry"),
            }
        }

        let mut entries = Vec::with_capacity(controls.len() + 1);
        entries.push(SessionLogEntry::ToolResultV3(result));
        entries.extend(controls.into_iter().map(SessionLogEntry::Control));
        let events = self
            .store
            .as_ref()
            .map(|store| store.append_session_entry_events(&entries))
            .transpose()?;
        self.entries.extend(entries);
        if let Some(events) = events {
            if let (Some(artifact_ref), Some(event), Some(store)) =
                (artifact_ref.as_ref(), events.first(), self.store.as_ref())
                && let Err(error) = ToolArtifactStore::for_session_store(store)
                    .bind_source_event(artifact_ref, &event.event_id)
            {
                // The `.event` binding is a rebuildable fork/GC cache. Retrieval authorization
                // comes only from the active durable pressure projection, so failure here cannot
                // broaden read authority or invalidate the committed descriptor.
                tracing::warn!(
                    artifact_ref = %artifact_ref.artifact_id,
                    error = %error,
                    "failed to bind tool artifact to its durable descriptor event"
                );
            }
            self.advance_durable_session_entry_count(&events);
        }
        Ok(())
    }

    /// Atomically closes a suspended `request_user_input` tool call and starts its logical
    /// continuation. Ordinary tool-result bundles cannot carry lifecycle controls; this narrow
    /// writer exists so a crash cannot expose an answer result without the matching continuation
    /// claim/start, or vice versa.
    pub(crate) fn append_user_input_tool_settlement_bundle(
        &mut self,
        result: ToolResultRecordedV3,
        lifecycle: Vec<crate::UserInputLifecycleEntryV1>,
        execution: ToolExecutionEntry,
    ) -> Result<()> {
        if lifecycle.is_empty() {
            bail!("user input continuation bundle requires lifecycle entries");
        }
        result.validate()?;
        execution.validate()?;
        if result.tool_name != crate::REQUEST_USER_INPUT_TOOL_NAME
            || execution.tool_name != crate::REQUEST_USER_INPUT_TOOL_NAME
            || execution.call_id != result.call_id
            || execution.status != ToolExecutionStatus::Completed
        {
            bail!("user input continuation bundle does not close the exact internal tool call");
        }
        let mut projection = self.user_input_projection()?;
        for entry in &lifecycle {
            if entry.identity().session_scope_id.as_str() != self.session_scope_id() {
                bail!("user input lifecycle entry belongs to a different session scope");
            }
            projection.apply(entry.clone())?;
        }
        let mut entries = lifecycle
            .into_iter()
            .map(crate::UserInputLifecycleEntryV1::into_control)
            .map(SessionLogEntry::Control)
            .collect::<Vec<_>>();
        entries.push(SessionLogEntry::ToolResultV3(result));
        entries.push(SessionLogEntry::Control(ControlEntry::ToolExecution(
            Box::new(execution),
        )));
        let events = self
            .store
            .as_ref()
            .map(|store| store.append_session_entry_events(&entries))
            .transpose()?;
        self.entries.extend(entries);
        if let Some(events) = events {
            self.advance_durable_session_entry_count(&events);
        }
        Ok(())
    }

    /// RFC-0062 9.4: appends one generation-guarded availability transition. The expected
    /// generation is the caller's responsibility; helper scans are available on the caller side.
    pub fn append_artifact_availability_transition(
        &mut self,
        artifact_ref: &ToolArtifactRefV1,
        expected_generation: u64,
        previous: ToolArtifactAvailabilityStateV1,
        next: ToolArtifactAvailabilityStateV1,
        reason: ToolArtifactAvailabilityReasonV1,
        changed_at_ms: u64,
    ) -> Result<()> {
        let change = ToolArtifactAvailabilityChangedV1 {
            schema_version: TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
            artifact_ref: artifact_ref.clone(),
            expected_generation,
            generation: expected_generation.saturating_add(1),
            previous,
            next,
            reason,
            changed_at_ms,
        };
        change.validate()?;
        self.append_control(ControlEntry::ToolArtifactAvailabilityChanged(change))
    }

    /// RFC-0062 9.4: resolves the current availability generation for one artifact from the
    /// durable availability transitions already in this session (0 when none exist).
    pub fn artifact_availability_generation(&self, artifact_ref: &ToolArtifactRefV1) -> u64 {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| {
                if let SessionLogEntry::Control(ControlEntry::ToolArtifactAvailabilityChanged(
                    change,
                )) = entry
                    && change.artifact_ref == *artifact_ref
                {
                    Some(change.generation)
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    /// RFC-0062 9.4: current availability state for one artifact from the durable transitions
    /// (Available when no event exists yet).
    pub fn artifact_availability_state(
        &self,
        artifact_ref: &ToolArtifactRefV1,
    ) -> ToolArtifactAvailabilityStateV1 {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| {
                if let SessionLogEntry::Control(ControlEntry::ToolArtifactAvailabilityChanged(
                    change,
                )) = entry
                    && change.artifact_ref == *artifact_ref
                {
                    Some(change.next)
                } else {
                    None
                }
            })
            .unwrap_or(ToolArtifactAvailabilityStateV1::Available)
    }

    pub fn append_control(&mut self, control: ControlEntry) -> Result<()> {
        self.append(SessionLogEntry::Control(control))
    }

    /// Rebuilds the authoritative durable user-input lifecycle projection.
    pub fn user_input_projection(&self) -> Result<crate::UserInputProjectionV1> {
        crate::UserInputProjectionV1::from_session_entries(&self.entries)
    }

    /// Atomically validates and appends one ordered durable user-input lifecycle batch.
    ///
    /// Shape validation alone is insufficient for these records: request generation, exact
    /// binding, accepted decision, continuation claim, and terminal resolution are reducer
    /// invariants. This API applies the complete candidate batch before writing any entry.
    pub fn append_user_input_lifecycle(
        &mut self,
        entries: Vec<crate::UserInputLifecycleEntryV1>,
    ) -> Result<()> {
        if entries.is_empty() {
            bail!("user input lifecycle append batch must not be empty");
        }
        let mut projection = self.user_input_projection()?;
        for entry in &entries {
            projection.apply(entry.clone())?;
        }
        self.append_controls(
            entries
                .into_iter()
                .map(crate::UserInputLifecycleEntryV1::into_control)
                .collect(),
        )
    }

    /// Appends one ordered control-state transition as a single writer batch.
    ///
    /// The in-memory projection is updated only after the complete durable batch succeeds. This
    /// keeps multi-entry state transitions from becoming partially visible to the active process.
    ///
    /// # Errors
    ///
    /// Returns an error when `controls` is empty or the durable writer cannot append the complete
    /// batch.
    /// RFC-0062 9.4: atomically appends a batch of availability transitions. The writer appends
    /// the whole batch under one durable intent, so a partial failure cannot leave half the
    /// artifacts disabled.
    pub fn append_availability_transitions(
        &mut self,
        transitions: Vec<(
            ToolArtifactRefV1,
            ToolArtifactAvailabilityStateV1,
            ToolArtifactAvailabilityStateV1,
            ToolArtifactAvailabilityReasonV1,
        )>,
        changed_at_ms: u64,
    ) -> Result<()> {
        // RFC-0062 9.4: the batch API protects the ledger invariants itself — every transition
        // must start from the artifact's CURRENT state, and a ref may appear at most once per
        // batch so the durable stream can never be made reducer-invalid by a caller.
        let mut seen = std::collections::BTreeSet::new();
        for (artifact_ref, previous, _, _) in &transitions {
            if !seen.insert(artifact_ref.clone()) {
                bail!("tool artifact availability batch repeats an opaque ref");
            }
            let current = self.artifact_availability_state(artifact_ref);
            if *previous != current {
                bail!(
                    "tool artifact availability transition is not state-contiguous: expected {:?}, got {:?}",
                    current,
                    previous
                );
            }
        }
        let controls = transitions
            .into_iter()
            .map(|(artifact_ref, previous, next, reason)| {
                let generation = self.artifact_availability_generation(&artifact_ref);
                ControlEntry::ToolArtifactAvailabilityChanged(ToolArtifactAvailabilityChangedV1 {
                    schema_version: TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
                    artifact_ref,
                    expected_generation: generation,
                    generation: generation.saturating_add(1),
                    previous,
                    next,
                    reason,
                    changed_at_ms,
                })
            })
            .collect::<Vec<_>>();
        self.append_controls(controls)
    }

    /// RFC-0062 16.2: atomically appends availability transitions together with the durable
    /// tombstone plans for every ref being disabled in this batch or already disabled.
    ///
    /// The whole batch lands under one durable intent, so a crash after the append can always
    /// reconcile `DisabledPendingDelete -> Expired` from the plans even when the physical body
    /// move completed before the terminal append.
    ///
    /// # Errors
    ///
    /// Returns an error when both inputs are empty, a transition is not state-contiguous, a ref
    /// repeats across either input, or a plan is not bound to the exact generation the
    /// `DisabledPendingDelete` state has (current generation + 1 when the ref is disabled in this
    /// batch, current generation when it is already disabled).
    pub fn append_availability_transitions_with_tombstone_plans(
        &mut self,
        transitions: Vec<(
            ToolArtifactRefV1,
            ToolArtifactAvailabilityStateV1,
            ToolArtifactAvailabilityStateV1,
            ToolArtifactAvailabilityReasonV1,
        )>,
        plans: Vec<ToolArtifactTombstonePlannedV1>,
        changed_at_ms: u64,
    ) -> Result<()> {
        if transitions.is_empty() && plans.is_empty() {
            bail!("tool artifact GC plan batch must not be empty");
        }
        let mut seen_transitions = std::collections::BTreeSet::new();
        for (artifact_ref, previous, _, _) in &transitions {
            if !seen_transitions.insert(artifact_ref.clone()) {
                bail!("tool artifact availability batch repeats an opaque ref");
            }
            let current = self.artifact_availability_state(artifact_ref);
            if *previous != current {
                bail!(
                    "tool artifact availability transition is not state-contiguous: expected {:?}, got {:?}",
                    current,
                    previous
                );
            }
        }
        let mut seen_plans = std::collections::BTreeSet::new();
        let mut controls = transitions
            .into_iter()
            .map(|(artifact_ref, previous, next, reason)| {
                let generation = self.artifact_availability_generation(&artifact_ref);
                ControlEntry::ToolArtifactAvailabilityChanged(ToolArtifactAvailabilityChangedV1 {
                    schema_version: TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
                    artifact_ref,
                    expected_generation: generation,
                    generation: generation.saturating_add(1),
                    previous,
                    next,
                    reason,
                    changed_at_ms,
                })
            })
            .collect::<Vec<_>>();
        for plan in plans {
            plan.validate()?;
            if !seen_plans.insert(plan.artifact_ref.clone()) {
                bail!("tool artifact GC plan batch repeats an opaque ref");
            }
            let generation = self.artifact_availability_generation(&plan.artifact_ref);
            let disabling_now = controls.iter().any(|control| {
                matches!(
                    control,
                    ControlEntry::ToolArtifactAvailabilityChanged(change)
                        if change.artifact_ref == plan.artifact_ref
                )
            });
            let expected = if disabling_now {
                generation.saturating_add(1)
            } else {
                if self.artifact_availability_state(&plan.artifact_ref)
                    != ToolArtifactAvailabilityStateV1::DisabledPendingDelete
                {
                    bail!(
                        "tool artifact tombstone plan requires a disabled artifact: {}",
                        plan.artifact_ref.artifact_id
                    );
                }
                generation
            };
            if plan.expected_generation != expected {
                bail!(
                    "tool artifact tombstone plan generation mismatch for {}: plan {}, ledger {}",
                    plan.artifact_ref.artifact_id,
                    plan.expected_generation,
                    expected
                );
            }
            controls.push(ControlEntry::ToolArtifactTombstonePlan(plan));
        }
        self.append_controls(controls)
    }

    /// RFC-0062 16.2: returns every durable tombstone plan recorded in this session, in append
    /// order. Recovery completes `DisabledPendingDelete -> Expired` from these plans.
    pub fn tombstone_plans(&self) -> Vec<&ToolArtifactTombstonePlannedV1> {
        self.entries
            .iter()
            .filter_map(|entry| {
                if let SessionLogEntry::Control(ControlEntry::ToolArtifactTombstonePlan(plan)) =
                    entry
                {
                    Some(plan)
                } else {
                    None
                }
            })
            .collect()
    }

    /// RFC-0062 16.2: true when a durable tombstone plan already exists for this artifact.
    #[must_use]
    pub fn has_tombstone_plan(&self, artifact_ref: &ToolArtifactRefV1) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolArtifactTombstonePlan(plan))
                    if &plan.artifact_ref == artifact_ref
            )
        })
    }

    /// RFC-0062 16.2: returns the durable tombstone plan bound to this artifact, when one exists.
    pub fn tombstone_plan_for(
        &self,
        artifact_ref: &ToolArtifactRefV1,
    ) -> Option<&ToolArtifactTombstonePlannedV1> {
        self.entries.iter().rev().find_map(|entry| {
            if let SessionLogEntry::Control(ControlEntry::ToolArtifactTombstonePlan(plan)) = entry
                && &plan.artifact_ref == artifact_ref
            {
                Some(plan)
            } else {
                None
            }
        })
    }

    pub fn append_controls(&mut self, controls: Vec<ControlEntry>) -> Result<()> {
        if controls.is_empty() {
            bail!("control append batch must not be empty");
        }
        controls
            .iter()
            .try_for_each(ControlEntry::validate_durable_contract)?;
        self.validate_user_input_controls(controls.iter())?;
        self.validate_plan_controls(controls.iter())?;
        let entries = controls
            .into_iter()
            .map(SessionLogEntry::Control)
            .collect::<Vec<_>>();
        let events = self
            .store
            .as_ref()
            .map(|store| store.append_session_entry_events(&entries))
            .transpose()?;
        self.entries.extend(entries);
        if let Some(events) = events {
            self.advance_durable_session_entry_count(&events);
        }
        Ok(())
    }

    /// Appends a control entry and returns its durable envelope when this session is store-backed.
    pub(crate) fn append_control_with_event(
        &mut self,
        control: ControlEntry,
    ) -> Result<Option<StoredEvent>> {
        control.validate_durable_contract()?;
        self.validate_user_input_controls(std::iter::once(&control))?;
        self.validate_plan_controls(std::iter::once(&control))?;
        let entry = SessionLogEntry::Control(control);
        let event = self
            .store
            .as_ref()
            .map(|store| store.append_session_entry_event(&entry))
            .transpose()?;
        self.entries.push(entry);
        if let Some(event) = event.as_ref() {
            self.advance_durable_session_entry_count(std::slice::from_ref(event));
        }
        Ok(event)
    }

    fn validate_user_input_controls<'a>(
        &self,
        controls: impl IntoIterator<Item = &'a ControlEntry>,
    ) -> Result<()> {
        let lifecycle_entries = controls
            .into_iter()
            .filter_map(crate::UserInputLifecycleEntryV1::from_control)
            .collect::<Vec<_>>();
        if lifecycle_entries.is_empty() {
            return Ok(());
        }
        if lifecycle_entries
            .iter()
            .any(|entry| entry.identity().session_scope_id.as_str() != self.session_scope_id())
        {
            bail!("user input lifecycle entry belongs to a different session scope");
        }
        let mut projection = self.user_input_projection()?;
        for entry in lifecycle_entries {
            projection.apply(entry)?;
        }
        Ok(())
    }

    fn validate_plan_controls<'a>(
        &self,
        controls: impl IntoIterator<Item = &'a ControlEntry>,
    ) -> Result<()> {
        let mut entries = self.entries.clone();
        let mut artifacts = crate::PlanArtifactProjection::from_entries(&entries);
        let mut reviews = crate::PlanReviewProjection::from_entries(&entries);
        for control in controls {
            match control {
                ControlEntry::PlanReviewAttempt(attempt) => {
                    reviews.validate_append(attempt)?;
                }
                ControlEntry::PlanDraftCreated(draft) => {
                    if let Some(existing) = artifacts.plans.get(&draft.plan_id)
                        && existing != draft
                    {
                        bail!(
                            "plan {} already has conflicting durable draft facts",
                            draft.plan_id.as_str()
                        );
                    }
                }
                ControlEntry::PlanDecisionRecorded(decision) => {
                    let draft = artifacts.plans.get(&decision.plan_id).ok_or_else(|| {
                        anyhow::anyhow!(
                            "plan decision references unknown plan {}",
                            decision.plan_id.as_str()
                        )
                    })?;
                    if draft.plan_hash != decision.plan_hash {
                        bail!("plan decision does not bind the durable plan hash");
                    }
                    if matches!(
                        decision.decision,
                        crate::PlanDecision::RevisionFailed
                            | crate::PlanDecision::RevisionSucceeded
                            | crate::PlanDecision::TaskCreationFailed
                    ) != (decision.decided_by == crate::PlanDecisionActor::System)
                    {
                        bail!("plan host settlement has an invalid decision actor");
                    }
                    if let Some(previous) = artifacts.latest_decision(&decision.plan_id) {
                        let idempotent = previous == decision;
                        let legal = matches!(
                            (previous.decision, decision.decision),
                            (
                                crate::PlanDecision::SavedOnly,
                                crate::PlanDecision::Accepted
                            ) | (
                                crate::PlanDecision::SavedOnly,
                                crate::PlanDecision::Rejected
                            ) | (
                                crate::PlanDecision::SavedOnly,
                                crate::PlanDecision::RevisionRequested
                            ) | (
                                crate::PlanDecision::RevisionRequested,
                                crate::PlanDecision::RevisionFailed
                            ) | (
                                crate::PlanDecision::RevisionRequested,
                                crate::PlanDecision::RevisionSucceeded
                            ) | (
                                crate::PlanDecision::RevisionFailed,
                                crate::PlanDecision::SavedOnly
                            ) | (
                                crate::PlanDecision::RevisionFailed,
                                crate::PlanDecision::Accepted
                            ) | (
                                crate::PlanDecision::RevisionFailed,
                                crate::PlanDecision::Rejected
                            ) | (
                                crate::PlanDecision::RevisionFailed,
                                crate::PlanDecision::RevisionRequested
                            ) | (
                                crate::PlanDecision::RevisionFailed,
                                crate::PlanDecision::TaskCreationFailed
                            ) | (
                                crate::PlanDecision::SavedOnly,
                                crate::PlanDecision::TaskCreationFailed
                            ) | (
                                crate::PlanDecision::TaskCreationFailed,
                                crate::PlanDecision::SavedOnly
                            ) | (
                                crate::PlanDecision::TaskCreationFailed,
                                crate::PlanDecision::Accepted
                            ) | (
                                crate::PlanDecision::TaskCreationFailed,
                                crate::PlanDecision::Rejected
                            ) | (
                                crate::PlanDecision::TaskCreationFailed,
                                crate::PlanDecision::RevisionRequested
                            ) | (
                                crate::PlanDecision::TaskCreationFailed,
                                crate::PlanDecision::TaskCreationFailed
                            )
                        );
                        if !idempotent && !legal {
                            bail!(
                                "plan {} has illegal decision transition {} -> {}",
                                decision.plan_id.as_str(),
                                previous.decision.as_str(),
                                decision.decision.as_str()
                            );
                        }
                    } else if !matches!(
                        decision.decision,
                        crate::PlanDecision::Accepted
                            | crate::PlanDecision::Rejected
                            | crate::PlanDecision::RevisionRequested
                            | crate::PlanDecision::TaskCreationFailed
                            | crate::PlanDecision::SavedOnly
                    ) {
                        bail!("plan revision settlement is missing its requested prefix");
                    }
                }
                ControlEntry::TaskCreatedFromPlan(created) => {
                    let draft = artifacts.plans.get(&created.plan_id).ok_or_else(|| {
                        anyhow::anyhow!(
                            "task promotion references unknown plan {}",
                            created.plan_id.as_str()
                        )
                    })?;
                    if draft.plan_hash != created.plan_hash {
                        bail!("task promotion does not bind the durable plan hash");
                    }
                }
                _ => continue,
            }
            entries.push(SessionLogEntry::Control(control.clone()));
            artifacts = crate::PlanArtifactProjection::from_entries(&entries);
            reviews = crate::PlanReviewProjection::from_entries(&entries);
            if reviews.has_conflicts() {
                bail!("plan review append would create a conflicted projection");
            }
        }
        Ok(())
    }

    /// Returns a clone of the durable store for a blocking-I/O bridge.
    pub(crate) fn durable_store(&self) -> Option<JsonlSessionStore> {
        self.store.clone()
    }

    /// Updates the in-memory projection after a caller has synchronously persisted a control.
    ///
    /// The blocking append must have completed successfully before this method is called.
    pub(crate) fn record_durably_appended_control(&mut self, control: ControlEntry) {
        if let (Some(previous), Some(store)) =
            (self.durable_session_entry_count, self.store.as_ref())
            && let Ok(active) = store.active_projection_snapshot()
            && active.durable_session_entry_count() == previous.saturating_add(1)
        {
            let promoted_user = match &control {
                ControlEntry::ConversationInputPromoted(promotion) => {
                    Some(promotion.durable_user_message.clone())
                }
                _ => None,
            };
            self.entries.push(SessionLogEntry::Control(control));
            if let Some(message) = promoted_user {
                self.entries.push(SessionLogEntry::User(message));
            }
            self.durable_session_entry_count = Some(active.durable_session_entry_count());
            return;
        }
        let serialized = serde_json::to_value(&control).ok();
        if serialized.as_ref().is_some_and(|expected| {
            self.entries.iter().any(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(existing)
                        if serde_json::to_value(existing)
                            .is_ok_and(|value| &value == expected)
                )
            })
        }) {
            return;
        }
        self.record_durably_appended_controls([control]);
    }

    /// Merges controls that an adapter has already appended to this session's durable store.
    ///
    /// This method only updates the in-memory projection and never writes the controls again. It is
    /// intended for a run boundary where the live [`Session`] was temporarily detached while the
    /// adapter accepted append-only controls through the same durable session stream.
    pub fn record_durably_appended_controls(
        &mut self,
        controls: impl IntoIterator<Item = ControlEntry>,
    ) {
        let controls = controls.into_iter().collect::<Vec<_>>();
        if controls.is_empty() {
            return;
        }
        if self.reconcile_durably_appended_controls(&controls) {
            return;
        }

        // Preserve the caller-visible controls if durable reconciliation is unavailable, but fail
        // closed for stable compaction. Count-only adoption cannot prove order when a detached
        // writer interleaves with appends owned by the live Session.
        self.entries
            .extend(controls.into_iter().map(SessionLogEntry::Control));
        self.durable_session_entry_count = None;
    }

    fn reconcile_durably_appended_controls(&mut self, controls: &[ControlEntry]) -> bool {
        let Some(store) = self.store.as_ref() else {
            return false;
        };
        let Ok(records) = store.read_event_records_writer() else {
            return false;
        };
        let Ok(canonical_entries) = session_entries_from_records(&records) else {
            return false;
        };

        // The supplied controls must occur in the claimed order. This rejects reversed or repeated
        // adoption while still allowing unrelated live-session appends to have interleaved in the
        // durable stream.
        let mut search_from = 0;
        for control in controls {
            let Ok(expected) = serde_json::to_value(control) else {
                return false;
            };
            let Some(offset) = canonical_entries[search_from..].iter().position(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(existing)
                        if serde_json::to_value(existing).is_ok_and(|value| value == expected)
                )
            }) else {
                return false;
            };
            search_from = search_from.saturating_add(offset).saturating_add(1);
        }

        let Ok(active) = store.active_projection_snapshot() else {
            return false;
        };
        if active.durable_session_entry_count() != canonical_entries.len() as u64 {
            return false;
        }

        let mut live_entries = Vec::with_capacity(canonical_entries.len());
        for entry in canonical_entries {
            let promoted_user = match &entry {
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion)) => {
                    Some(promotion.durable_user_message.clone())
                }
                _ => None,
            };
            live_entries.push(entry);
            if let Some(message) = promoted_user {
                live_entries.push(SessionLogEntry::User(message));
            }
        }
        self.stats = session_stats_from_entries(&live_entries);
        self.entries = live_entries;
        self.durable_session_entry_count = Some(active.durable_session_entry_count());
        true
    }

    /// Adopts one already-durable queue promotion into the active process projection.
    ///
    /// The promotion control remains the only durable user event. Its safe user message is added
    /// only to this live session so tool-follow-up turns owned by the same process retain their
    /// user context before the queue reaches `Delivered`.
    ///
    /// # Errors
    ///
    /// Returns an error when the promotion belongs to another session or its queue/message
    /// identity is already present in the live projection.
    pub fn record_durably_appended_conversation_input_promotion(
        &mut self,
        promotion: ConversationInputPromotedEntry,
    ) -> Result<()> {
        promotion.validate_for_session(&self.session_scope_id)?;
        if self.entries.iter().any(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(existing)) => {
                existing.queue_id == promotion.queue_id
                    || existing.durable_user_message.id == promotion.durable_user_message.id
            }
            SessionLogEntry::User(message) => message.id == promotion.durable_user_message.id,
            _ => false,
        }) {
            bail!("conversation input promotion is already present in the live session");
        }
        self.record_durably_appended_control(ControlEntry::ConversationInputPromoted(promotion));
        Ok(())
    }

    /// Adopts one already-durable task-guidance promotion without replaying the session when it is
    /// the sole new session entry at the active projection frontier.
    pub fn record_durably_appended_task_guidance_promotion(
        &mut self,
        promotion: TaskGuidancePromotedEntry,
    ) -> Result<()> {
        promotion.validate_for_session(&self.session_scope_id)?;
        self.record_durably_appended_control(ControlEntry::TaskGuidancePromoted(promotion));
        Ok(())
    }

    /// Returns the live session scope used to bind URL capabilities and external provenance.
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Attaches a non-serializable runtime URL capability registrar to this logical session.
    ///
    /// The attachment moves with the in-memory session across turns but never enters the JSONL
    /// log, snapshots, or provider-visible request material.
    pub fn try_attach_user_url_capability_registrar(
        &mut self,
        registrar: Arc<dyn crate::UserUrlCapabilityRegistrar>,
    ) -> Result<()> {
        if self
            .runtime_attachments
            .user_url_capability_registrar
            .is_some()
        {
            bail!("session already has a URL capability registrar attachment");
        }
        self.runtime_attachments.user_url_capability_registrar = Some(registrar);
        Ok(())
    }

    /// Returns a clone of the process-local registrar attachment for run-boundary fallback.
    pub fn user_url_capability_registrar(
        &self,
    ) -> Option<Arc<dyn crate::UserUrlCapabilityRegistrar>> {
        self.runtime_attachments
            .user_url_capability_registrar
            .clone()
    }

    /// Attaches the process-local controlled-cache resolver used before ordinary request freeze.
    pub fn try_attach_image_attachment_resolver(
        &mut self,
        resolver: Arc<dyn crate::ImageAttachmentResolver>,
    ) -> Result<()> {
        if self.runtime_attachments.image_attachment_resolver.is_some() {
            bail!("session already has an image attachment resolver");
        }
        self.runtime_attachments.image_attachment_resolver = Some(resolver);
        Ok(())
    }

    #[must_use]
    pub fn image_attachment_resolver(&self) -> Option<Arc<dyn crate::ImageAttachmentResolver>> {
        self.runtime_attachments.image_attachment_resolver.clone()
    }

    /// Appends a validated external provenance sidecar for an already-persisted safe message.
    pub fn append_external_provenance(
        &mut self,
        provenance: ExternalProvenanceEntry,
    ) -> Result<()> {
        if provenance.session_scope_id != self.session_scope_id {
            bail!("external provenance belongs to a different session scope");
        }
        let message = self
            .entries
            .iter()
            .find_map(|entry| match entry {
                SessionLogEntry::User(message) | SessionLogEntry::Assistant(message)
                    if message.id == provenance.message_id =>
                {
                    Some(message.clone())
                }
                SessionLogEntry::ToolResultV3(result)
                    if result.message_id == provenance.message_id =>
                {
                    result.model_message().ok()
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("external provenance message does not exist"))?;
        provenance.validate_against_message(&message)?;
        self.append_control(ControlEntry::ExternalProvenance(provenance))
    }

    /// Returns durable external provenance sidecars in append order.
    pub fn external_provenance_entries(&self) -> Vec<ExternalProvenanceEntry> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::ExternalProvenance(provenance)) => {
                    Some(provenance.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Appends a durable domain event that does not project into provider-visible chat history.
    ///
    /// In-memory sessions without a backing store cannot persist durable-only events, so they return
    /// `Ok(None)` instead of fabricating an in-memory fact that would disappear on resume. This
    /// compatibility API does not return a durable receipt and must never authorize network or
    /// extension effects; use [`Session::durable_audit_writer`] for that boundary.
    pub fn append_durable_event(
        &mut self,
        event_type: DurableEventType,
        event_class: EventClass,
        payload: serde_json::Value,
    ) -> Result<Option<StoredEvent>> {
        let event = self
            .store
            .as_ref()
            .map(|store| store.append_event(event_type, event_class, payload))
            .transpose()?;
        if let Some(event) = event.as_ref() {
            self.advance_durable_session_entry_count(std::slice::from_ref(event));
        }
        Ok(event)
    }

    pub(crate) fn append_durable_events_with_controls(
        &mut self,
        durable_events: Vec<(DurableEventType, EventClass, serde_json::Value)>,
        controls: Vec<ControlEntry>,
    ) -> Result<()> {
        let entries = controls
            .iter()
            .cloned()
            .map(SessionLogEntry::Control)
            .collect::<Vec<_>>();
        let events = self
            .store
            .as_ref()
            .map(|store| store.append_events_and_session_entries(durable_events, &entries))
            .transpose()?;
        self.entries.extend(entries);
        if let Some(events) = events {
            self.advance_durable_session_entry_count(&events);
        }
        Ok(())
    }

    /// Returns the strict store-backed writer used by pre-effect durable audit ordering.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::MissingDurableStore`] for an in-memory-only session.
    pub fn durable_audit_writer(
        &self,
    ) -> std::result::Result<std::sync::Arc<dyn DurableAuditWriter>, DurableAuditError> {
        let store = self
            .store
            .as_ref()
            .ok_or(DurableAuditError::MissingDurableStore)?;
        Ok(std::sync::Arc::new(store.clone()))
    }

    /// Returns the session-backed recorder used by a root cancellation owner.
    pub fn run_cancellation_recorder(
        &self,
    ) -> std::result::Result<crate::RunCancellationRecorder, DurableAuditError> {
        let store = self
            .store
            .as_ref()
            .ok_or(DurableAuditError::MissingDurableStore)?;
        Ok(crate::RunCancellationRecorder::new(store.clone()))
    }

    /// Returns the store-backed recorder for adapter-owned foreground run lifecycle boundaries.
    pub fn conversation_run_lifecycle_recorder(
        &self,
    ) -> std::result::Result<crate::ConversationRunLifecycleRecorder, DurableAuditError> {
        let store = self
            .store
            .as_ref()
            .ok_or(DurableAuditError::MissingDurableStore)?;
        Ok(crate::ConversationRunLifecycleRecorder::new(store.clone()))
    }

    /// Returns the store-backed recorder used by pre-egress barriers and lifecycle recovery.
    pub fn egress_audit_recorder(
        &self,
    ) -> std::result::Result<crate::EgressAuditRecorder, DurableAuditError> {
        let store = self
            .store
            .as_ref()
            .ok_or(DurableAuditError::MissingDurableStore)?;
        Ok(crate::EgressAuditRecorder::new(store.clone()))
    }

    /// Closes every durable egress authorization/query start left without a terminal outcome.
    ///
    /// This MUST run at a run-lifecycle boundary where no hosted turn can be in flight (agent run
    /// start), never on session load: a load during an active hosted turn would falsely mark an
    /// in-flight request `Interrupted`, and the real terminal outcome would then be rejected as a
    /// conflicting terminal. Idempotent; returns the number of terminal records appended.
    pub fn reconcile_egress_lifecycle(&self) -> Result<usize> {
        let Some(store) = self.store.as_ref() else {
            return Ok(0);
        };
        Ok(crate::EgressAuditRecorder::new(store.clone()).reconcile_interrupted()?)
    }

    /// Returns a store-backed mutation recorder for tool contexts when this session is durable.
    pub fn mutation_event_recorder(&self) -> Option<MutationEventRecorder> {
        self.store.as_ref().cloned().map(MutationEventRecorder::new)
    }

    /// Reconciles prepared controlled mutations that were left without terminal commit events.
    ///
    /// This requires a workspace root and is therefore run by the agent before a new turn, rather
    /// than during store-only session loading.
    pub fn reconcile_prepared_mutations(
        &mut self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Vec<StoredEvent>> {
        let Some(recorder) = self.mutation_event_recorder() else {
            return Ok(Vec::new());
        };
        let events = recorder.reconcile_prepared_mutations(workspace_root)?;
        if !events.is_empty() {
            self.advance_durable_only_events(&events);
        }
        Ok(events)
    }

    /// Reconciles interrupted write-capable tool executions with persisted mutation profiles.
    ///
    /// `Session::load_from_store` can mark unfinished tool executions as interrupted without a
    /// workspace root. This method runs at the next agent turn, when the workspace root is known,
    /// and records workspace mutation evidence without replaying the tool.
    pub fn reconcile_unfinished_write_tool_executions(
        &mut self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Vec<StoredEvent>> {
        let Some(recorder) = self.mutation_event_recorder() else {
            return Ok(Vec::new());
        };
        let workspace_root = workspace_root.as_ref();
        let mut events = Vec::new();
        for execution in interrupted_tool_execution_profiles(&self.entries) {
            if let Some(event) =
                recorder.reconcile_execution_mutation_profile(workspace_root, &execution)?
            {
                events.push(event);
            }
        }
        if !events.is_empty() {
            self.advance_durable_only_events(&events);
        }
        Ok(events)
    }

    pub fn entries(&self) -> &[SessionLogEntry] {
        &self.entries
    }

    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    pub fn resolved_model_route(&self) -> Option<&ResolvedModelRoute> {
        self.resolved_model_route.as_ref()
    }

    /// Returns the proven egress trust binding for the latest route epoch, when present.
    #[must_use]
    pub fn route_egress_trust_binding(&self) -> Option<crate::RouteEgressTrustBinding> {
        crate::session::session_route_trust_binding_from_entries(&self.entries)
    }

    /// Proves that the latest durable route boundary is exactly one requested rebind.
    #[must_use]
    pub fn route_rebind_matches(
        &self,
        source_route: &ResolvedModelRoute,
        target_route: &ResolvedModelRoute,
        egress_trust_binding: &crate::RouteEgressTrustBinding,
    ) -> bool {
        let Some(boundary_index) = self.entries.iter().rposition(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(
                    ControlEntry::SessionIdentity { .. }
                        | ControlEntry::SessionModelSelected { .. }
                        | ControlEntry::SessionRouteRebound { .. }
                )
            )
        }) else {
            return false;
        };
        let SessionLogEntry::Control(ControlEntry::SessionRouteRebound {
            resolved_model_route,
            ..
        }) = &self.entries[boundary_index]
        else {
            return false;
        };
        resolved_model_route == target_route
            && session_resolved_route_from_entries(&self.entries[..boundary_index]).as_ref()
                == Some(source_route)
            && self.route_egress_trust_binding().as_ref() == Some(egress_trust_binding)
    }

    /// Selects the exact provider route used by subsequent runs without forking the session.
    ///
    /// The selection is append-only. Provider-native continuation material written before this
    /// boundary is intentionally excluded from future requests because it may be model-specific.
    pub fn select_model_route(
        &mut self,
        provider_name: impl Into<String>,
        resolved_model_route: ResolvedModelRoute,
    ) -> Result<()> {
        let provider_name = provider_name.into();
        if provider_name.trim().is_empty() {
            bail!("session provider selection must not be empty");
        }
        let model_name = resolved_model_route.model_ref.model_id.clone();
        if provider_name == self.provider_name
            && self.resolved_model_route.as_ref() == Some(&resolved_model_route)
        {
            return Ok(());
        }
        self.append_control(ControlEntry::SessionModelSelected {
            provider_name: provider_name.clone(),
            model_name: model_name.clone(),
            resolved_model_route: resolved_model_route.clone(),
        })?;
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.resolved_model_route = Some(resolved_model_route);
        Ok(())
    }

    /// Selects an exact route and commits its opaque trust proof in one ordered writer batch.
    pub fn select_model_route_with_trust(
        &mut self,
        provider_name: impl Into<String>,
        resolved_model_route: ResolvedModelRoute,
        egress_trust_binding: crate::RouteEgressTrustBinding,
    ) -> Result<()> {
        let provider_name = provider_name.into();
        if provider_name.trim().is_empty() {
            bail!("session provider selection must not be empty");
        }
        let model_name = resolved_model_route.model_ref.model_id.clone();
        if provider_name == self.provider_name
            && self.resolved_model_route.as_ref() == Some(&resolved_model_route)
        {
            if self.route_egress_trust_binding().as_ref() == Some(&egress_trust_binding) {
                return Ok(());
            }
            self.append_control(ControlEntry::SessionRouteTrustBound {
                route_semantic_fingerprint: resolved_model_route.semantic_fingerprint.clone(),
                egress_trust_binding,
            })?;
            return Ok(());
        }
        self.append_controls(vec![
            ControlEntry::SessionModelSelected {
                provider_name: provider_name.clone(),
                model_name: model_name.clone(),
                resolved_model_route: resolved_model_route.clone(),
            },
            ControlEntry::SessionRouteTrustBound {
                route_semantic_fingerprint: resolved_model_route.semantic_fingerprint.clone(),
                egress_trust_binding,
            },
        ])?;
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.resolved_model_route = Some(resolved_model_route);
        Ok(())
    }

    /// Commits one automatic portable route rebind after runtime quiescence validation.
    ///
    /// Callers must hold the session-scoped route mutation authority. This kernel operation
    /// verifies the source route and makes the boundary plus trust proof durable before updating
    /// the in-memory route projection.
    pub fn commit_route_rebind(
        &mut self,
        provider_name: impl Into<String>,
        source_route: &ResolvedModelRoute,
        target_route: ResolvedModelRoute,
        egress_trust_binding: crate::RouteEgressTrustBinding,
    ) -> Result<()> {
        anyhow::ensure!(
            self.resolved_model_route.as_ref() == Some(source_route),
            "session route source is stale"
        );
        let provider_name = provider_name.into();
        anyhow::ensure!(
            !provider_name.trim().is_empty(),
            "session provider rebind must not be empty"
        );
        let model_name = target_route.model_ref.model_id.clone();
        self.append_controls(vec![
            ControlEntry::SessionRouteRebound {
                provider_name: provider_name.clone(),
                model_name: model_name.clone(),
                resolved_model_route: target_route.clone(),
            },
            ControlEntry::SessionRouteTrustBound {
                route_semantic_fingerprint: target_route.semantic_fingerprint.clone(),
                egress_trust_binding,
            },
        ])?;
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.resolved_model_route = Some(target_route);
        Ok(())
    }

    /// Returns the in-memory provider-visible message projection.
    ///
    /// Store-backed V2 context projection is fallible and therefore intentionally exposed through
    /// [`Session::try_context_projection_from_durable`]. Request construction uses that durable
    /// projection, while this in-memory query remains read-only and infallible.
    pub fn messages(&self) -> Vec<ModelMessage> {
        self.context_projection().model_messages()
    }

    /// Builds the in-memory chat context projection without reading or mutating the durable stream.
    #[must_use]
    pub fn context_projection(&self) -> SessionContextProjection {
        SessionContextProjection::from_entries(&self.entries)
    }

    /// Rebuilds the chat context projection from the V2 durable stream when a store is attached.
    ///
    /// # Errors
    ///
    /// Returns an error when the V2 lifecycle or sidecar stream is malformed. This query never
    /// performs recovery writes.
    pub fn try_context_projection_from_durable(&self) -> Result<Option<SessionContextProjection>> {
        if let Some(records) = &self.reconstruction_records {
            return SessionContextProjection::from_durable_records(&self.entries, records, None)
                .map(Some);
        }
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        SessionContextProjection::from_durable_records(&self.entries, &records, None).map(Some)
    }

    /// Reads the current validated durable stream through this session's shared coordinator.
    ///
    /// Expensive projections such as manual compaction may use this at an explicit safe point.
    /// Active-session callers should prefer the bounded active projection for scheduler queries.
    pub fn read_durable_event_records(&self) -> Result<Vec<SessionStreamRecord>> {
        let store = self
            .store
            .as_ref()
            .context("durable event records require a durable session store")?;
        store.read_event_records_writer()
    }

    /// Rebuilds a V3 whole-turn/token-tail preview without mutating the durable session.
    ///
    /// # Errors
    ///
    /// Returns an error when no durable store is attached or the adaptive plan cannot be proven
    /// against the current stream and exact-fit tail budget.
    pub fn adaptive_compaction_preview(
        &self,
        policy: AdaptiveTailPolicyV3,
        exact_fit_limit_tokens: u64,
    ) -> Result<Option<V2CompactionPreview>> {
        let store = self
            .store
            .as_ref()
            .context("adaptive compaction preview requires a durable session store")?;
        store.adaptive_compaction_preview(policy, exact_fit_limit_tokens, None)
    }

    /// Rebuilds provider physical-attempt evidence from the durable stream without recovery
    /// writes. Queue recovery uses this to distinguish a confirmed no-send from an uncertain
    /// provider outcome after a process restart.
    pub fn provider_physical_attempt_projection(
        &self,
    ) -> Result<crate::ProviderPhysicalAttemptProjection> {
        let store = self
            .store
            .as_ref()
            .context("provider physical-attempt projection requires a durable session store")?;
        let records = store.read_event_records_writer()?;
        crate::ProviderPhysicalAttemptProjection::from_records(&records)
    }

    /// Reads native-continuation observations and inactive candidates from the durable stream.
    ///
    /// This query never activates a candidate, creates a provider request, or performs cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error when this session has no durable store or its V2 stream is invalid.
    pub fn provider_continuation_projection(&self) -> Result<ProviderContinuationProjection> {
        let store = self
            .store
            .as_ref()
            .context("provider continuation projection requires a durable session store")?;
        store.provider_continuation_projection()
    }

    pub fn continuation_states(&self, provider_name: &str) -> Vec<ProviderContinuationState> {
        // A provider request is cache- and reconstruction-sensitive to vector order. Keep the
        // latest state per provider key, but emit keys in a deterministic order across processes.
        let mut latest_by_key =
            std::collections::BTreeMap::<(String, Option<String>), ProviderContinuationState>::new(
            );
        for entry in self.entries_after_latest_route_boundary() {
            if let SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) = entry
                && state.provider_name == provider_name
            {
                latest_by_key.insert(
                    (state.state_kind.clone(), state.message_id.clone()),
                    state.clone(),
                );
            }
        }
        latest_by_key.into_values().collect()
    }

    pub fn latest_response_handle(&self, provider_name: &str) -> Option<ResponseHandle> {
        self.entries_after_latest_route_boundary()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::ResponseHandleTracked(handle))
                    if handle.provider_name == provider_name =>
                {
                    Some(handle.clone())
                }
                _ => None,
            })
    }

    fn entries_after_latest_route_boundary(&self) -> &[SessionLogEntry] {
        let boundary = self.entries.iter().rposition(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(
                    ControlEntry::SessionIdentity { .. }
                        | ControlEntry::SessionModelSelected { .. }
                        | ControlEntry::SessionRouteRebound { .. }
                )
            )
        });
        boundary.map_or(&self.entries, |index| &self.entries[index + 1..])
    }

    pub fn latest_prefix_snapshot(&self) -> Option<PrefixSnapshot> {
        self.entries_after_latest_route_boundary()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
                    Some(snapshot.clone())
                }
                _ => None,
            })
    }

    pub fn latest_memory_snapshot(&self) -> Option<MemorySnapshot> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(snapshot)) => {
                Some(snapshot.clone())
            }
            _ => None,
        })
    }

    /// Returns durable plan artifact state reconstructed from append-only control entries.
    pub fn plan_artifact_projection(&self) -> PlanArtifactProjection {
        PlanArtifactProjection::from_entries(&self.entries)
    }

    /// Returns the last durable stream sequence at the current frontier (0 for an empty stream).
    #[must_use]
    pub fn durable_frontier_sequence(&self) -> u64 {
        self.store
            .as_ref()
            .and_then(|store| {
                store
                    .active_projection_snapshot()
                    .ok()
                    .and_then(|snapshot| {
                        snapshot
                            .frontier()
                            .cursor()
                            .map(|cursor| cursor.last_applied_stream_sequence)
                    })
            })
            .unwrap_or(0)
    }

    /// Rebuilds plan artifact state directly from the durable v2 event stream.
    pub fn try_plan_artifact_projection_from_durable(
        &self,
    ) -> Result<Option<PlanArtifactProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        let mut projection = PlanArtifactProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_plan_artifact_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable task projection reconstructed from append-only control entries.
    pub fn task_state_projection(&self) -> TaskStateProjection {
        TaskStateProjection::from_entries(&self.entries)
    }

    /// Replays durable conversation-to-task admission facts without manufacturing task runs.
    pub fn task_handoff_projection(&self) -> TaskHandoffProjection {
        TaskHandoffProjection::from_entries(&self.entries)
    }

    /// Returns route-local orchestration kill switches for this session.
    pub fn orchestration_route_disablement_projection(
        &self,
    ) -> crate::OrchestrationRouteDisablementProjection {
        crate::OrchestrationRouteDisablementProjection::from_entries(&self.entries)
    }

    /// Returns durable resume job state reconstructed from append-only control entries.
    pub fn resume_job_state_projection(
        &self,
        now_ms: u64,
    ) -> crate::resume::ResumeJobStateProjection {
        crate::resume::ResumeJobStateProjection::from_entries(&self.entries, now_ms)
    }

    /// Rebuilds resume job state directly from the durable v2 event stream.
    pub fn try_resume_job_state_projection_from_durable(
        &self,
        now_ms: u64,
    ) -> Result<Option<crate::resume::ResumeJobStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = crate::resume::ResumeJobStateProjection::default();
        for record in records {
            let Some(event) = record.domain_event_record()? else {
                continue;
            };
            let Some(entry) = session_entry_from_domain_event(&event.event)? else {
                continue;
            };
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(&control, now_ms);
            }
        }
        Ok(Some(projection))
    }

    /// Rebuilds task state directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for task projection. It preserves the existing infallible
    /// `task_state_projection` API while giving callers and tests a fail-closed durable replay
    /// option.
    pub fn try_task_state_projection_from_durable(&self) -> Result<Option<TaskStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        let mut projection = TaskStateProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_task_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable agent thread projection reconstructed from append-only control entries.
    pub fn agent_thread_state_projection(&self) -> AgentThreadStateProjection {
        AgentThreadStateProjection::from_entries(&self.entries)
    }

    /// Rebuilds the session list row for this session directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for the productized session-list projection. It preserves
    /// the UI/query projection boundary without requiring callers to manually read JSONL records.
    pub fn try_session_list_projection_from_durable(
        &self,
    ) -> Result<Option<crate::projection::SessionListProjectionSnapshot>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        crate::projection::session_list_projection_from_records(&records).map(Some)
    }

    /// Rebuilds agent thread state directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for the agent graph projection. It preserves the existing
    /// infallible `agent_thread_state_projection` API while giving callers and tests a fail-closed
    /// durable replay option.
    pub fn try_agent_thread_state_projection_from_durable(
        &self,
    ) -> Result<Option<AgentThreadStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = AgentThreadStateProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_agent_thread_projection_record(&mut projection, &mut cursor, &record)?;
        }
        projection.finalize_replay();
        Ok(Some(projection))
    }

    /// Rebuilds the agent graph projection directly from the durable v2 event stream.
    ///
    /// This is a product-surface alias for `try_agent_thread_state_projection_from_durable`, which
    /// remains the lower-level domain name for the same projection state.
    pub fn try_agent_graph_projection_from_durable(
        &self,
    ) -> Result<Option<AgentThreadStateProjection>> {
        self.try_agent_thread_state_projection_from_durable()
    }

    /// Rebuilds the dispatch trace projection directly from the durable v2 event stream.
    ///
    /// Dispatch traces are a redacted materialized view over tool, agent, usage, readiness and
    /// egress events. This method keeps new state adoption on the durable replay path instead of
    /// requiring callers to project from in-memory session entries.
    pub fn try_dispatch_trace_projection_from_durable(
        &self,
    ) -> Result<Option<crate::projection::DispatchTraceProjectionSnapshot>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        crate::projection::dispatch_trace_projection_from_records(&records).map(Some)
    }

    /// Returns durable agent profile trust decisions reconstructed from append-only control entries.
    pub fn agent_profile_trust_projection(&self) -> AgentProfileTrustProjection {
        AgentProfileTrustProjection::from_entries(&self.entries)
    }

    /// Rebuilds agent profile trust decisions directly from the durable v2 event stream.
    pub fn try_agent_profile_trust_projection_from_durable(
        &self,
    ) -> Result<Option<AgentProfileTrustProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = AgentProfileTrustProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_agent_profile_trust_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns durable agent profile policy decisions reconstructed from append-only control entries.
    pub fn agent_profile_policy_projection(&self) -> AgentProfilePolicyProjection {
        AgentProfilePolicyProjection::from_entries(&self.entries)
    }

    /// Rebuilds agent profile policy decisions directly from the durable v2 event stream.
    pub fn try_agent_profile_policy_projection_from_durable(
        &self,
    ) -> Result<Option<AgentProfilePolicyProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = AgentProfilePolicyProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_agent_profile_policy_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable skill projection reconstructed from append-only control entries.
    pub fn skill_state_projection(&self) -> SkillStateProjection {
        SkillStateProjection::from_entries(&self.entries)
    }

    /// Rebuilds skill state directly from the durable v2 event stream.
    pub fn try_skill_state_projection_from_durable(&self) -> Result<Option<SkillStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = SkillStateProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_skill_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable plugin projection reconstructed from append-only control entries.
    pub fn plugin_state_projection(&self) -> PluginStateProjection {
        PluginStateProjection::from_entries(&self.entries)
    }

    /// Rebuilds plugin state directly from the durable v2 event stream.
    pub fn try_plugin_state_projection_from_durable(
        &self,
    ) -> Result<Option<PluginStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = PluginStateProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_plugin_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable change set projection reconstructed from append-only control entries.
    pub fn changeset_projection(&self) -> ChangeSetProjection {
        ChangeSetProjection::from_entries(&self.entries)
    }

    /// Rebuilds change set state directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for changeset projection. It preserves the existing
    /// infallible `changeset_projection` API while giving callers and tests a fail-closed durable
    /// replay option.
    pub fn try_changeset_projection_from_durable(&self) -> Result<Option<ChangeSetProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = ChangeSetProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_changeset_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns durable write-isolation state reconstructed from append-only control entries.
    pub fn write_isolation_projection(&self) -> WriteIsolationProjection {
        WriteIsolationProjection::from_entries(&self.entries)
    }

    /// Rebuilds write-isolation state directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for RFC-0014 write lease, isolated workspace, changeset
    /// output, and merge review facts. It does not enforce scheduling by itself.
    pub fn try_write_isolation_projection_from_durable(
        &self,
    ) -> Result<Option<WriteIsolationProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = WriteIsolationProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_write_isolation_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns durable verification evidence reconstructed from append-only control entries.
    pub fn verification_state_projection(&self) -> VerificationStateProjection {
        VerificationStateProjection::from_entries(&self.entries)
    }

    /// Rebuilds verification state directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for verification projection. It preserves the existing
    /// infallible `verification_state_projection` API while giving callers and tests a fail-closed
    /// durable replay option.
    pub fn try_verification_state_projection_from_durable(
        &self,
    ) -> Result<Option<VerificationStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = VerificationStateProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_verification_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable terminal task projection reconstructed from append-only control entries.
    pub fn terminal_task_projection(&self) -> TerminalTaskProjection {
        TerminalTaskProjection::from_entries(&self.entries)
    }

    /// Rebuilds terminal task state directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for terminal task projection. It preserves the existing
    /// infallible `terminal_task_projection` API while giving callers and tests a fail-closed
    /// durable replay option.
    pub fn try_terminal_task_projection_from_durable(
        &self,
    ) -> Result<Option<TerminalTaskProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        let mut projection = TerminalTaskProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_terminal_task_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    pub fn conversation_queue_projection(&self) -> ConversationQueueProjection {
        ConversationQueueProjection::from_entries(&self.entries)
    }

    /// Rebuilds conversation queue state directly from the durable v2 event stream.
    pub fn try_conversation_queue_projection_from_durable(
        &self,
    ) -> Result<Option<ConversationQueueProjection>> {
        Ok(self
            .try_conversation_queue_durable_projection_from_durable()?
            .map(|projection| projection.queue))
    }

    /// Rebuilds queue state together with the precise revision required by the promotion CAS.
    pub fn try_conversation_queue_durable_projection_from_durable(
        &self,
    ) -> Result<Option<ConversationQueueDurableProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        Ok(Some(ConversationQueueDurableProjection::from_records(
            &records,
        )?))
    }

    pub fn agent_result_continuation_projection(&self) -> AgentResultContinuationProjection {
        AgentResultContinuationProjection::from_entries(&self.entries)
    }

    /// Rebuilds agent result continuation state directly from the durable v2 event stream.
    pub fn try_agent_result_continuation_projection_from_durable(
        &self,
    ) -> Result<Option<AgentResultContinuationProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = store.read_event_records_writer()?;
        let mut projection = AgentResultContinuationProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_agent_result_continuation_projection_record(
                &mut projection,
                &mut cursor,
                &record,
            )?;
        }
        Ok(Some(projection))
    }

    /// Builds one provider request from stable system memory, projected session history, and tools.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    pub fn build_request(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
    ) -> Result<CompletionRequest> {
        self.build_request_with_transient_messages(
            workspace_root,
            memory_config,
            tools,
            reasoning_effort,
            previous_response_handle,
            traffic_partition_key,
            &[],
        )
    }

    /// Rebuilds the current provider-visible durable surface without creating new snapshots.
    ///
    /// The session must come from [`Self::reconstruct_from_provider_frontier`]. The caller then
    /// supplies the exact stable runtime inputs owned by configuration and the tool registry.
    /// Dynamic context is consumed from the already durable Context V2 entry and is never
    /// re-resolved or implicitly cleared.
    #[allow(clippy::too_many_arguments)]
    pub fn reconstruct_provider_request(
        &self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        max_output_tokens: Option<u32>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
    ) -> Result<CompletionRequest> {
        let memory = self.memory_snapshot_for_pure_request(workspace_root, memory_config)?;
        let projected_messages = self.request_context_projection()?.model_messages();
        let mut request = self
            .assemble_request_from_components(
                &memory,
                projected_messages,
                None,
                None,
                tools,
                max_output_tokens,
                reasoning_effort,
                previous_response_handle,
                traffic_partition_key,
                transient_messages,
                &[],
            )?
            .request;
        crate::resolve_request_image_attachments(
            &mut request,
            self.runtime_attachments
                .image_attachment_resolver
                .as_deref(),
        )?;
        Ok(request)
    }

    /// Builds one provider request with extra transient messages that are not appended as
    /// provider-visible session history.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    #[allow(clippy::too_many_arguments)]
    pub fn build_request_with_transient_messages(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
    ) -> Result<CompletionRequest> {
        self.build_request_with_transient_messages_and_context(
            workspace_root,
            memory_config,
            tools,
            reasoning_effort,
            previous_response_handle,
            traffic_partition_key,
            transient_messages,
            RuntimeContextCandidates::default(),
        )
    }

    /// Builds one provider request with extra transient messages and runtime-selected Context V2
    /// candidates.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    /// Context assembly failures are recorded as `ContextAssemblySkipped` and degrade to a request
    /// without Context V2.
    #[allow(clippy::too_many_arguments)]
    pub fn build_request_with_transient_messages_and_context(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
    ) -> Result<CompletionRequest> {
        self.build_request_with_transient_messages_context_and_overlays(
            workspace_root,
            memory_config,
            tools,
            reasoning_effort,
            previous_response_handle,
            traffic_partition_key,
            transient_messages,
            runtime_context,
            &[],
        )
    }

    /// Builds one request while applying non-serializable exact-message overlays only after the
    /// safe PrefixSnapshot and Context V2 materialization have been durably recorded.
    #[allow(clippy::too_many_arguments)]
    pub fn build_request_with_transient_messages_context_and_overlays(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
        overlays: &[crate::TransientMessageOverlay],
    ) -> Result<CompletionRequest> {
        self.build_request_with_transient_messages_context_overlays_and_max_tokens(
            workspace_root,
            memory_config,
            tools,
            None,
            reasoning_effort,
            previous_response_handle,
            traffic_partition_key,
            transient_messages,
            runtime_context,
            overlays,
        )
    }

    /// Builds one ordinary provider request with an optional per-run output-token ceiling.
    #[allow(clippy::too_many_arguments)]
    pub fn build_request_with_transient_messages_context_overlays_and_max_tokens(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        max_output_tokens: Option<u32>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
        overlays: &[crate::TransientMessageOverlay],
    ) -> Result<CompletionRequest> {
        let memory = self.memory_snapshot_for_request(workspace_root, memory_config)?;
        let session_projection = self.request_context_projection()?;
        let projected_messages = session_projection.model_messages();
        let context_resolution = self.runtime_context_v2_resolution_or_skip(
            &session_projection,
            &projected_messages,
            runtime_context,
        )?;
        if let Some(snapshot) = context_resolution.staged_snapshot {
            self.append_runtime_context_snapshot_v2(snapshot)?;
        }
        let projected_messages = self.request_context_projection()?.model_messages();
        let mut assembled = self.assemble_request_from_components(
            &memory,
            projected_messages,
            context_resolution.effective_summary,
            None,
            tools,
            max_output_tokens,
            reasoning_effort,
            previous_response_handle,
            traffic_partition_key,
            transient_messages,
            overlays,
        )?;
        crate::resolve_request_image_attachments(
            &mut assembled.request,
            self.runtime_attachments
                .image_attachment_resolver
                .as_deref(),
        )?;
        let prefix_changed = self.latest_prefix_snapshot().is_none_or(|latest| {
            latest.sha256 != assembled.prefix_snapshot.sha256
                || latest.provider_name != assembled.prefix_snapshot.provider_name
                || latest.model_name != assembled.prefix_snapshot.model_name
                || latest.memory_fingerprint != assembled.prefix_snapshot.memory_fingerprint
                || latest.tool_schema_fingerprint
                    != assembled.prefix_snapshot.tool_schema_fingerprint
                || latest.skill_index_fingerprint
                    != assembled.prefix_snapshot.skill_index_fingerprint
        });
        if prefix_changed {
            self.append_control(ControlEntry::PrefixSnapshotCaptured(
                assembled.prefix_snapshot,
            ))?;
        }
        Ok(assembled.request)
    }

    /// Builds a complete candidate provider request without appending request-side controls.
    ///
    /// This is the pre-turn admission surface: it accepts process-local transient message
    /// material, applies the same safe/exact overlay assembly as an ordinary request, and leaves
    /// the session stream and in-memory history unchanged. Callers must separately establish a
    /// promotion and pre-send barrier before using this material for provider I/O. Non-system
    /// transient messages participate in context selection through their safe persistence
    /// projection so a pre-promotion queued user turn selects the same context as its later
    /// durable promotion without exposing exact process-local material.
    #[allow(clippy::too_many_arguments)]
    pub fn build_pre_turn_candidate_request(
        &self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        target_max_tokens: Option<u32>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
        overlays: &[crate::TransientMessageOverlay],
    ) -> Result<CompletionRequest> {
        let session_projection = self.request_context_projection()?;
        let memory = self.memory_snapshot_for_pure_request(workspace_root, memory_config)?;
        let projected_messages = session_projection.model_messages();
        let mut context_query_messages = projected_messages.clone();
        for transient in transient_messages
            .iter()
            .filter(|message| message.role != crate::MessageRole::System)
        {
            let (safe_transient, _) = crate::project_message_for_persistence(transient.clone())?;
            context_query_messages.push(safe_transient);
        }
        let context_resolution = self.build_runtime_context_v2_resolution(
            &session_projection,
            &context_query_messages,
            runtime_context,
        )?;
        let mut request = self
            .assemble_request_from_components(
                &memory,
                projected_messages,
                context_resolution.effective_summary,
                context_resolution.staged_snapshot,
                tools,
                target_max_tokens,
                reasoning_effort,
                previous_response_handle,
                traffic_partition_key,
                transient_messages,
                overlays,
            )?
            .request;
        crate::resolve_request_image_attachments(
            &mut request,
            self.runtime_attachments
                .image_attachment_resolver
                .as_deref(),
        )?;
        Ok(request)
    }

    /// Materializes the exact request that would follow a portable compaction activation without
    /// appending a `MemorySnapshot`, `ContextAssemblySkipped`, or `PrefixSnapshot` control entry.
    ///
    /// The caller must freeze and prove the returned request before it records the compaction
    /// `Started` barrier. Context V2 assembly errors are returned rather than downgraded because
    /// a downgraded request would no longer be the reviewed candidate target.
    #[allow(clippy::too_many_arguments)]
    pub fn build_portable_compaction_candidate_request(
        &self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        checkpoint: &ContinuationCheckpointV1,
        task_memory: &TaskMemoryV1,
        candidate_messages: Vec<ModelMessage>,
        tools: Vec<ToolSpec>,
        target_max_tokens: Option<u32>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
        overlays: &[crate::TransientMessageOverlay],
    ) -> Result<CompletionRequest> {
        let durable_projection = self.request_context_projection()?;
        let candidate_projection = durable_projection.with_portable_candidate(
            checkpoint,
            task_memory,
            candidate_messages,
        )?;
        let memory = self.memory_snapshot_for_pure_request(workspace_root, memory_config)?;
        let projected_messages = candidate_projection.model_messages();
        let mut context_query_messages = projected_messages.clone();
        for transient in transient_messages
            .iter()
            .filter(|message| message.role != crate::MessageRole::System)
        {
            let (safe_transient, _) = crate::project_message_for_persistence(transient.clone())?;
            context_query_messages.push(safe_transient);
        }
        let context_resolution = self.build_runtime_context_v2_resolution(
            &candidate_projection,
            &context_query_messages,
            runtime_context,
        )?;
        let mut request = self
            .assemble_request_from_components(
                &memory,
                projected_messages,
                context_resolution.effective_summary,
                context_resolution.staged_snapshot,
                tools,
                target_max_tokens,
                reasoning_effort,
                previous_response_handle,
                traffic_partition_key,
                transient_messages,
                overlays,
            )?
            .request;
        crate::strip_request_image_attachments_for_compaction(&mut request);
        Ok(request)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble_request_from_components(
        &self,
        memory: &MemorySnapshot,
        projected_messages: Vec<ModelMessage>,
        runtime_context_summary: Option<PrefixRuntimeContextSummary>,
        staged_runtime_context_snapshot: Option<RuntimeContextSnapshotV2>,
        tools: Vec<ToolSpec>,
        target_max_tokens: Option<u32>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        overlays: &[crate::TransientMessageOverlay],
    ) -> Result<AssembledRequest> {
        let mut safe_request_messages = memory.messages.clone();
        let mut exact_overlays = overlays.to_vec();
        let (system_transients, non_system_transients): (Vec<_>, Vec<_>) = transient_messages
            .iter()
            .partition(|message| message.role == crate::MessageRole::System);
        for transient in system_transients {
            let (safe_transient, exact_overlay) =
                crate::project_message_for_persistence(transient.clone())?;
            safe_request_messages.push(safe_transient);
            exact_overlays.push(exact_overlay);
        }
        safe_request_messages.extend(projected_messages);
        for transient in non_system_transients {
            let (safe_transient, exact_overlay) =
                crate::project_message_for_persistence(transient.clone())?;
            safe_request_messages.push(safe_transient);
            exact_overlays.push(exact_overlay);
        }
        if let Some(snapshot) = staged_runtime_context_snapshot {
            snapshot.validate()?;
            safe_request_messages.push(snapshot.message);
        }

        let materialized_messages = serde_json::to_string(&safe_request_messages)
            .context("failed to serialize messages")?;
        let materialized_tools =
            serde_json::to_string(&tools).context("failed to serialize tool specs")?;
        let materialized_bytes = materialized_messages
            .len()
            .saturating_add(1)
            .saturating_add(materialized_tools.len());
        let mut digest = Sha256::new();
        digest.update(materialized_messages.as_bytes());
        digest.update(b"\n");
        digest.update(materialized_tools.as_bytes());
        let mut prefix_snapshot = PrefixSnapshot {
            materialization: PrefixSnapshotMaterialization::new(
                materialized_bytes,
                safe_request_messages.len(),
                tools.len(),
                runtime_context_summary,
            ),
            sha256: format!("{:x}", digest.finalize()),
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            memory_fingerprint: "none".to_owned(),
            tool_schema_fingerprint: format!("{:x}", Sha256::digest(materialized_tools.as_bytes())),
            skill_index_fingerprint: "none".to_owned(),
        };
        apply_memory_report(&mut prefix_snapshot, &memory.report);
        let request_messages =
            crate::apply_exact_message_overlays(&safe_request_messages, &exact_overlays)?;
        let deterministic_materialization = serde_json::to_value(&request_messages)?
            == serde_json::to_value(&safe_request_messages)?;
        Ok(AssembledRequest {
            request: CompletionRequest {
                provider_name: self.provider_name.clone(),
                model_name: self.model_name.clone(),
                messages: request_messages,
                tools,
                temperature: None,
                max_tokens: target_max_tokens,
                reasoning_effort,
                previous_response_handle,
                continuation_states: self.continuation_states(&self.provider_name),
                traffic_partition_key,
                background: false,
                store: false,
                deterministic_materialization,
                hosted_tools: Vec::new(),
            },
            prefix_snapshot,
        })
    }

    fn runtime_context_v2_resolution_or_skip(
        &mut self,
        session_projection: &SessionContextProjection,
        projected_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
    ) -> Result<RuntimeContextResolution> {
        let runtime_candidate_count = runtime_context.items.len();
        let runtime_item_ids = runtime_context
            .items
            .iter()
            .take(12)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        match self.build_runtime_context_v2_resolution(
            session_projection,
            projected_messages,
            runtime_context,
        ) {
            Ok(message) => Ok(message),
            Err(error) => {
                self.append_control(ControlEntry::ContextAssemblySkipped(
                    ContextAssemblySkippedEntry {
                        reason: format!("{error:#}"),
                        candidate_count: runtime_candidate_count,
                        item_ids: runtime_item_ids,
                    },
                ))?;
                Ok(RuntimeContextResolution {
                    staged_snapshot: None,
                    effective_summary: None,
                })
            }
        }
    }

    pub(super) fn build_runtime_context_v2_resolution(
        &self,
        session_projection: &SessionContextProjection,
        projected_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
    ) -> Result<RuntimeContextResolution> {
        let mut snippets = BTreeMap::new();
        let mut items = Vec::new();

        if let Some((latest_user_index, query)) =
            latest_user_context_query_from_projection(session_projection, projected_messages)
        {
            let mut external_message_ids = self
                .external_provenance_entries()
                .into_iter()
                .map(|provenance| provenance.message_id)
                .collect::<std::collections::BTreeSet<_>>();
            external_message_ids.extend(
                session_projection
                    .trust_projection
                    .external_untrusted_message_ids
                    .iter()
                    .cloned(),
            );
            let archive = session_archive_from_projected_messages_with_external(
                session_projection,
                projected_messages,
                latest_user_index,
                &external_message_ids,
            );
            let hits = archive.search_bm25(&query, REQUEST_CONTEXT_V0_SESSION_ARCHIVE_LIMIT);
            for hit in hits {
                snippets.insert(hit.item.id.clone(), hit.snippet);
                items.push(hit.item);
            }
        }

        if let Some(task_memory) = session_projection.task_memory.as_ref()
            && let Ok(task_items) = task_memory_context_items(task_memory)
        {
            insert_task_memory_context_snippets(task_memory, &mut snippets);
            items.extend(task_items);
        }

        snippets.extend(runtime_context.snippets);
        items.extend(runtime_context.items);

        let external_sources = self
            .external_provenance_entries()
            .into_iter()
            .rev()
            .flat_map(|provenance| provenance.sources)
            .take(REQUEST_CONTEXT_V0_EXTERNAL_SOURCE_LIMIT);
        for source in external_sources {
            let snippet = source.title.as_deref().map_or_else(
                || source.safe_display_url.clone(),
                |title| format!("{title} — {}", source.safe_display_url),
            );
            let item_id = format!("external-source:{}", source.source_id);
            let item = ContextItem {
                id: item_id.clone(),
                source: ContextSource::ExternalSource,
                source_event_id: None,
                trust_level: ContextTrustLevel::ExternalUntrusted,
                sensitivity: ContextSensitivity::External,
                egress_decision: Some("external_safe_persistence".to_owned()),
                repo_revision: None,
                token_cost: estimate_context_token_cost(&snippet),
                score: None,
                score_breakdown: Vec::new(),
                inclusion_reason: ContextInclusionReason::RequiredEvidence,
                body_ref: ContextBodyRef::inline(&snippet),
            };
            snippets.insert(item_id, snippet);
            items.push(item);
        }

        let latest_visible = projected_messages
            .iter()
            .rev()
            .find(|message| is_runtime_context_v2_message(message));
        let latest_durable = self.entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::RuntimeContextSnapshotV2(snapshot) => Some(snapshot),
            _ => None,
        });
        let source_tail_message_id = projected_messages.last().map(|message| message.id.clone());

        if items.is_empty() {
            if latest_visible.is_none() && latest_durable.is_none() {
                return Ok(RuntimeContextResolution {
                    staged_snapshot: None,
                    effective_summary: None,
                });
            }
            let content = render_runtime_context_v2_clear_content()?;
            let summary = empty_runtime_context_v2_summary();
            if latest_visible.and_then(|message| message.content.as_deref())
                == Some(content.as_str())
            {
                return Ok(RuntimeContextResolution {
                    staged_snapshot: None,
                    effective_summary: Some(summary),
                });
            }
            return Ok(RuntimeContextResolution {
                staged_snapshot: Some(RuntimeContextSnapshotV2::new(
                    RuntimeContextSnapshotStateV2::Cleared,
                    content,
                    source_tail_message_id,
                )),
                effective_summary: Some(summary),
            });
        }

        let packed = pack_context_items(
            items,
            ContextPackOptions::new(REQUEST_CONTEXT_V0_MAX_TOKENS),
        )?;
        let summary = summarize_runtime_context(&packed);
        let content = render_runtime_context_v2_content(&packed, &snippets)?;
        if latest_visible.and_then(|message| message.content.as_deref()) == Some(content.as_str()) {
            return Ok(RuntimeContextResolution {
                staged_snapshot: None,
                effective_summary: Some(summary),
            });
        }
        Ok(RuntimeContextResolution {
            staged_snapshot: Some(RuntimeContextSnapshotV2::new(
                RuntimeContextSnapshotStateV2::Active,
                content,
                source_tail_message_id,
            )),
            effective_summary: Some(summary),
        })
    }

    fn memory_snapshot_for_request(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
    ) -> Result<MemorySnapshot> {
        let memory = materialize_memory(workspace_root, memory_config)?;
        if let Some(snapshot) = self.latest_memory_snapshot()
            && snapshot.report.fingerprint == memory.report.fingerprint
        {
            return Ok(snapshot);
        }

        let snapshot = MemorySnapshot {
            messages: memory.messages,
            report: memory.report,
        };
        self.append_control(ControlEntry::MemorySnapshotCaptured(snapshot.clone()))?;
        Ok(snapshot)
    }

    fn memory_snapshot_for_pure_request(
        &self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
    ) -> Result<MemorySnapshot> {
        let memory = materialize_memory(workspace_root, memory_config)?;
        if let Some(snapshot) = self.latest_memory_snapshot()
            && snapshot.report.fingerprint == memory.report.fingerprint
        {
            return Ok(snapshot);
        }
        Ok(MemorySnapshot {
            messages: memory.messages,
            report: memory.report,
        })
    }

    pub fn store_path(&self) -> Option<&Path> {
        self.store.as_ref().map(JsonlSessionStore::path)
    }

    /// Returns the next session-stream sequence for synthetic evidence tied to this session.
    ///
    /// Durable-only domain events do not appear in `Session::entries`, so callers that need
    /// stream ordering must use the durable writer state when a store is present.
    pub fn next_stream_sequence_hint(&self) -> Result<u64> {
        let Some(store) = &self.store else {
            return Ok((self.entries.len() as u64).saturating_add(1));
        };
        store.next_stream_sequence()
    }

    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    pub fn stats_mut(&mut self) -> &mut SessionStats {
        &mut self.stats
    }

    /// Rebuilds usage and cost statistics directly from the durable v2 event stream.
    ///
    /// This is the RFC-0001 replay path for token/cost projection. It preserves the existing
    /// infallible `stats` API while giving callers and tests a fail-closed durable replay option.
    pub fn try_usage_stats_from_durable(&self) -> Result<Option<SessionStats>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut stats = SessionStats::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_usage_projection_record(&mut stats, &mut cursor, &record)?;
        }
        Ok(Some(stats))
    }

    pub fn ensure_identity_entry(&mut self) -> Result<()> {
        if self.entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        }) {
            return Ok(());
        }

        self.append_control(ControlEntry::SessionIdentity {
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            resolved_model_route: self.resolved_model_route.clone(),
        })
    }

    fn request_context_projection(&self) -> Result<SessionContextProjection> {
        Ok(self
            .try_context_projection_from_durable()?
            .unwrap_or_else(|| self.context_projection()))
    }
}

fn validated_recovered_entries(
    session_scope_id: &str,
    entries: Vec<SessionLogEntry>,
) -> (Vec<SessionLogEntry>, bool) {
    let audit_already_present = entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ContextAssemblySkipped(audit))
                if audit.reason == UNSAFE_EXTERNAL_RECOVERY_AUDIT_REASON
        )
    });
    let mut safe_entries = Vec::with_capacity(entries.len());
    let mut skipped = false;
    for entry in entries {
        let accepted = match &entry {
            SessionLogEntry::Control(ControlEntry::ExternalProvenance(provenance)) => {
                let message = safe_entries.iter().find_map(|candidate| match candidate {
                    SessionLogEntry::User(message) | SessionLogEntry::Assistant(message)
                        if message.id == provenance.message_id =>
                    {
                        Some(message.clone())
                    }
                    SessionLogEntry::ToolResultV3(result)
                        if result.message_id == provenance.message_id =>
                    {
                        result.model_message().ok()
                    }
                    _ => None,
                });
                provenance.session_scope_id == session_scope_id
                    && message.is_some_and(|message| {
                        provenance.validate_against_message(&message).is_ok()
                    })
            }
            SessionLogEntry::Control(ControlEntry::WebUrlCapabilityDescriptor(descriptor)) => {
                descriptor.session_scope_id == session_scope_id
                    && descriptor.validate().is_ok()
                    && safe_entries.iter().any(|candidate| {
                        match (descriptor.provenance, candidate) {
                            (
                                crate::WebUrlProvenanceKind::UserMessage,
                                SessionLogEntry::User(message),
                            ) => message.id == descriptor.durable_entry_id,
                            (
                                crate::WebUrlProvenanceKind::WebSearchResult
                                | crate::WebUrlProvenanceKind::PriorWebFetch
                                | crate::WebUrlProvenanceKind::RedirectTarget,
                                SessionLogEntry::Assistant(message),
                            ) => message.id == descriptor.durable_entry_id,
                            (
                                crate::WebUrlProvenanceKind::WebSearchResult
                                | crate::WebUrlProvenanceKind::PriorWebFetch
                                | crate::WebUrlProvenanceKind::RedirectTarget,
                                SessionLogEntry::ToolResultV3(result),
                            ) => result.message_id == descriptor.durable_entry_id,
                            _ => false,
                        }
                    })
            }
            _ => true,
        };
        if accepted {
            safe_entries.push(entry);
        } else {
            skipped = true;
        }
    }
    (safe_entries, skipped && !audit_already_present)
}

fn unsafe_external_recovery_audit_control() -> ControlEntry {
    ControlEntry::ContextAssemblySkipped(ContextAssemblySkippedEntry {
        reason: UNSAFE_EXTERNAL_RECOVERY_AUDIT_REASON.to_owned(),
        candidate_count: 0,
        item_ids: Vec::new(),
    })
}

fn unsafe_external_recovery_audit_entry() -> SessionLogEntry {
    SessionLogEntry::Control(unsafe_external_recovery_audit_control())
}
