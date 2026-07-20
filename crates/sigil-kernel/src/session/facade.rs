use super::*;

/// In-memory session state backed by an optional append-only JSONL store.
#[derive(Debug)]
pub struct Session {
    pub(super) session_scope_id: String,
    pub(super) provider_name: String,
    pub(super) model_name: String,
    pub(super) entries: Vec<SessionLogEntry>,
    pub(super) store: Option<JsonlSessionStore>,
    pub(super) stats: SessionStats,
    pub(super) runtime_attachments: SessionRuntimeAttachments,
}

#[derive(Default)]
pub(super) struct SessionRuntimeAttachments {
    user_url_capability_registrar: Option<Arc<dyn crate::UserUrlCapabilityRegistrar>>,
    image_attachment_resolver: Option<Arc<dyn crate::ImageAttachmentResolver>>,
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

impl Session {
    /// Creates a new in-memory session with the given provider and model identity.
    pub fn new(provider_name: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            session_scope_id: uuid::Uuid::new_v4().to_string(),
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            entries: Vec::new(),
            store: None,
            stats: SessionStats::default(),
            runtime_attachments: SessionRuntimeAttachments::default(),
        }
    }

    /// Attaches a durable JSONL store to the session.
    pub fn with_store(mut self, store: JsonlSessionStore) -> Self {
        self.session_scope_id = session_id_for_path(store.path());
        self.store = Some(store);
        self
    }

    /// Rehydrates a session from a preloaded list of entries.
    pub fn from_entries(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        entries: Vec<SessionLogEntry>,
    ) -> Self {
        let session_scope_id = uuid::Uuid::new_v4().to_string();
        let (mut entries, audit_needed) = validated_recovered_entries(&session_scope_id, entries);
        if audit_needed {
            entries.push(unsafe_external_recovery_audit_entry());
        }
        let stats = session_stats_from_entries(&entries);
        Self {
            session_scope_id,
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            entries,
            store: None,
            stats,
            runtime_attachments: SessionRuntimeAttachments::default(),
        }
    }

    /// Loads a session from the durable store and recovers its persisted identity when possible.
    pub fn load_from_store(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        store: JsonlSessionStore,
    ) -> Result<Self> {
        let fallback_provider_name = provider_name.into();
        let fallback_model_name = model_name.into();
        // Establish the V2 session envelope (including tail repair and identity) before the
        // continuation coordinator reads the stream. Otherwise coordinator recovery can expose
        // a repaired-but-not-yet-initialized file to concurrent readers during startup.
        let (entries, provider_name, model_name) =
            store.load_entries_writer_reconciled(fallback_provider_name, fallback_model_name)?;
        ProviderContinuationPayloadCoordinator::for_store(store.clone())?
            .recover()
            .context("failed to recover provider continuation payload lifecycle")?;
        crate::EgressAuditRecorder::new(store.clone()).reconcile_interrupted()?;
        let session_scope_id = session_id_for_path(store.path());
        let (entries, audit_needed) = validated_recovered_entries(&session_scope_id, entries);
        let stats = session_stats_from_entries(&entries);
        let mut session = Self {
            session_scope_id,
            provider_name,
            model_name,
            entries,
            store: Some(store),
            stats,
            runtime_attachments: SessionRuntimeAttachments::default(),
        };
        if audit_needed {
            session.append_control(unsafe_external_recovery_audit_control())?;
        }
        let recovered_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        crate::reconcile_unfinished_run_cancellations(&mut session, recovered_at_ms)?;
        Ok(session)
    }

    /// Appends a single entry to the in-memory log and durable store when present.
    pub fn append(&mut self, entry: SessionLogEntry) -> Result<()> {
        if let Some(store) = &self.store {
            store.append(&entry)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn append_user_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::User(message))
    }

    pub fn append_assistant_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::Assistant(message))
    }

    pub fn append_tool_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::ToolResult(message))
    }

    /// Persists one tool result and its URL provenance controls as a single ordered writer batch.
    pub(crate) fn append_tool_result_bundle(
        &mut self,
        message: ModelMessage,
        controls: Vec<ControlEntry>,
    ) -> Result<()> {
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
                _ => bail!("tool result bundle contains an unsupported control entry"),
            }
        }

        let mut entries = Vec::with_capacity(controls.len() + 1);
        entries.push(SessionLogEntry::ToolResult(message));
        entries.extend(controls.into_iter().map(SessionLogEntry::Control));
        if let Some(store) = &self.store {
            store.append_session_entry_events(&entries)?;
        }
        self.entries.extend(entries);
        Ok(())
    }

    pub fn append_control(&mut self, control: ControlEntry) -> Result<()> {
        self.append(SessionLogEntry::Control(control))
    }

    /// Appends a control entry and returns its durable envelope when this session is store-backed.
    pub(crate) fn append_control_with_event(
        &mut self,
        control: ControlEntry,
    ) -> Result<Option<StoredEvent>> {
        let entry = SessionLogEntry::Control(control);
        let event = self
            .store
            .as_ref()
            .map(|store| store.append_session_entry_event(&entry))
            .transpose()?;
        self.entries.push(entry);
        Ok(event)
    }

    /// Returns a clone of the durable store for a blocking-I/O bridge.
    pub(crate) fn durable_store(&self) -> Option<JsonlSessionStore> {
        self.store.clone()
    }

    /// Updates the in-memory projection after a caller has synchronously persisted a control.
    ///
    /// The blocking append must have completed successfully before this method is called.
    pub(crate) fn record_durably_appended_control(&mut self, control: ControlEntry) {
        self.entries.push(SessionLogEntry::Control(control));
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
        self.entries
            .extend(controls.into_iter().map(SessionLogEntry::Control));
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
                SessionLogEntry::User(message)
                | SessionLogEntry::Assistant(message)
                | SessionLogEntry::ToolResult(message)
                    if message.id == provenance.message_id =>
                {
                    Some(message)
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("external provenance message does not exist"))?;
        provenance.validate_against_message(message)?;
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
        self.store
            .as_ref()
            .map(|store| store.append_event(event_type, event_class, payload))
            .transpose()
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
        if let Some(store) = self.store.as_ref() {
            store.append_events_and_session_entries(durable_events, &entries)?;
        }
        self.entries.extend(entries);
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
        recorder.reconcile_prepared_mutations(workspace_root)
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

    /// Selects the provider model used by subsequent runs without forking the durable session.
    ///
    /// The selection is append-only. Provider-native continuation material written before this
    /// boundary is intentionally excluded from future requests because it may be model-specific.
    pub fn select_model(&mut self, model_name: impl Into<String>) -> Result<()> {
        let model_name = model_name.into();
        if model_name.trim().is_empty() {
            bail!("session model selection must not be empty");
        }
        if model_name == self.model_name {
            return Ok(());
        }
        self.append_control(ControlEntry::SessionModelSelected {
            model_name: model_name.clone(),
        })?;
        self.model_name = model_name;
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
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        SessionContextProjection::from_durable_records(&self.entries, &records, None).map(Some)
    }

    /// Rebuilds a V2 fold preview from this session's durable stream without mutating it.
    ///
    /// # Errors
    ///
    /// Returns an error when this session has no durable store or the V2 stream cannot safely be
    /// planned.
    pub fn v2_compaction_preview(
        &self,
        requested_tail_message_count: usize,
    ) -> Result<Option<V2CompactionPreview>> {
        let store = self
            .store
            .as_ref()
            .context("v2 compaction preview requires a durable session store")?;
        store.v2_compaction_preview(requested_tail_message_count, None)
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
        let records = JsonlSessionStore::read_event_records(store.path())?;
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
        let mut latest_by_key: HashMap<(String, Option<String>), ProviderContinuationState> =
            HashMap::new();
        for entry in self.entries_after_latest_model_selection() {
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
        self.entries_after_latest_model_selection()
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

    fn entries_after_latest_model_selection(&self) -> &[SessionLogEntry] {
        let boundary = self.entries.iter().rposition(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionModelSelected { .. })
            )
        });
        boundary.map_or(&self.entries, |index| &self.entries[index + 1..])
    }

    pub fn latest_prefix_snapshot(&self) -> Option<PrefixSnapshot> {
        self.entries.iter().rev().find_map(|entry| match entry {
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

    /// Returns durable plan approvals reconstructed from append-only control entries.
    pub fn plan_approval_projection(&self) -> PlanApprovalProjection {
        PlanApprovalProjection::from_entries(&self.entries)
    }

    /// Returns durable plan artifact state reconstructed from append-only control entries.
    pub fn plan_artifact_projection(&self) -> PlanArtifactProjection {
        PlanArtifactProjection::from_entries(&self.entries)
    }

    /// Rebuilds plan artifact state directly from the durable v2 event stream.
    pub fn try_plan_artifact_projection_from_durable(
        &self,
    ) -> Result<Option<PlanArtifactProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = PlanArtifactProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_plan_artifact_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Rebuilds plan approval state directly from the durable v2 event stream.
    pub fn try_plan_approval_projection_from_durable(
        &self,
    ) -> Result<Option<PlanApprovalProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = PlanApprovalProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_plan_approval_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable task projection reconstructed from append-only control entries.
    pub fn task_state_projection(&self) -> TaskStateProjection {
        TaskStateProjection::from_entries(&self.entries)
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
        let records = JsonlSessionStore::read_event_records(store.path())?;
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
        let records = JsonlSessionStore::read_event_records(store.path())?;
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
        let records = JsonlSessionStore::read_event_records(store.path())?;
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

    /// Builds one provider request with extra transient messages and runtime-selected Context V1
    /// candidates.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    /// Context assembly failures are recorded as `ContextAssemblySkipped` and degrade to a request
    /// without Context V1.
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
    /// safe PrefixSnapshot and Context V1 materialization have been durably recorded.
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
        let session_projection = self.request_context_projection()?;
        let memory = self.memory_snapshot_for_request(workspace_root, memory_config)?;
        let projected_messages = session_projection.model_messages();
        let context_message = self.runtime_context_v0_message(
            &session_projection,
            &projected_messages,
            runtime_context,
        )?;
        let mut assembled = self.assemble_request_from_components(
            &memory,
            projected_messages,
            context_message,
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
    /// promotion and pre-send barrier before using this material for provider I/O.
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
        let context_message = self.build_runtime_context_v1_message(
            &session_projection,
            &projected_messages,
            runtime_context,
        )?;
        let mut request = self
            .assemble_request_from_components(
                &memory,
                projected_messages,
                context_message,
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
    /// `Started` barrier. Context V1 assembly errors are returned rather than downgraded because
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
        let context_message = self.build_runtime_context_v1_message(
            &candidate_projection,
            &projected_messages,
            runtime_context,
        )?;
        let mut request = self
            .assemble_request_from_components(
                &memory,
                projected_messages,
                context_message,
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
        context_message: Option<ModelMessage>,
        tools: Vec<ToolSpec>,
        target_max_tokens: Option<u32>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
        overlays: &[crate::TransientMessageOverlay],
    ) -> Result<AssembledRequest> {
        let mut safe_request_messages = memory.messages.clone();
        if let Some(context_message) = context_message {
            safe_request_messages.push(context_message);
        }
        safe_request_messages.extend(projected_messages);
        let mut exact_overlays = overlays.to_vec();
        for transient in transient_messages {
            let (safe_transient, exact_overlay) =
                crate::project_message_for_persistence(transient.clone())?;
            safe_request_messages.push(safe_transient);
            exact_overlays.push(exact_overlay);
        }

        let materialized_messages = serde_json::to_string(&safe_request_messages)
            .context("failed to serialize messages")?;
        let materialized_tools =
            serde_json::to_string(&tools).context("failed to serialize tool specs")?;
        let prefix_materialized = format!("{materialized_messages}\n{materialized_tools}");
        let digest = Sha256::digest(prefix_materialized.as_bytes());
        let mut prefix_snapshot = PrefixSnapshot {
            materialized_text: prefix_materialized,
            sha256: format!("{digest:x}"),
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            memory_fingerprint: "none".to_owned(),
            tool_schema_fingerprint: format!("{:x}", Sha256::digest(materialized_tools.as_bytes())),
            skill_index_fingerprint: "none".to_owned(),
        };
        apply_memory_report(&mut prefix_snapshot, &memory.report);
        let request_messages =
            crate::apply_exact_message_overlays(&safe_request_messages, &exact_overlays)?;
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
                deterministic_materialization: true,
                hosted_tools: Vec::new(),
            },
            prefix_snapshot,
        })
    }

    fn runtime_context_v0_message(
        &mut self,
        session_projection: &SessionContextProjection,
        projected_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
    ) -> Result<Option<ModelMessage>> {
        let runtime_candidate_count = runtime_context.items.len();
        let runtime_item_ids = runtime_context
            .items
            .iter()
            .take(12)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        match self.build_runtime_context_v1_message(
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
                Ok(None)
            }
        }
    }

    fn build_runtime_context_v1_message(
        &self,
        session_projection: &SessionContextProjection,
        projected_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
    ) -> Result<Option<ModelMessage>> {
        let mut snippets = BTreeMap::new();
        let mut items = Vec::new();

        if let Some((latest_user_index, query)) = latest_user_context_query(projected_messages) {
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

        if items.is_empty() {
            return Ok(None);
        }

        let packed = pack_context_items(
            items,
            ContextPackOptions::new(REQUEST_CONTEXT_V0_MAX_TOKENS),
        )?;
        render_runtime_context_v1_message(&packed, &snippets)
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
                    SessionLogEntry::User(message)
                    | SessionLogEntry::Assistant(message)
                    | SessionLogEntry::ToolResult(message)
                        if message.id == provenance.message_id =>
                    {
                        Some(message)
                    }
                    _ => None,
                });
                provenance.session_scope_id == session_scope_id
                    && message
                        .is_some_and(|message| provenance.validate_against_message(message).is_ok())
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
                                SessionLogEntry::Assistant(message)
                                | SessionLogEntry::ToolResult(message),
                            ) => message.id == descriptor.durable_entry_id,
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
