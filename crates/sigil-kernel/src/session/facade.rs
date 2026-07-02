use super::*;

/// In-memory session state backed by an optional append-only JSONL store.
#[derive(Debug)]
pub struct Session {
    pub(super) provider_name: String,
    pub(super) model_name: String,
    pub(super) entries: Vec<SessionLogEntry>,
    pub(super) store: Option<JsonlSessionStore>,
    pub(super) stats: SessionStats,
}

impl Session {
    /// Creates a new in-memory session with the given provider and model identity.
    pub fn new(provider_name: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            entries: Vec::new(),
            store: None,
            stats: SessionStats::default(),
        }
    }

    /// Attaches a durable JSONL store to the session.
    pub fn with_store(mut self, store: JsonlSessionStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Rehydrates a session from a preloaded list of entries.
    pub fn from_entries(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        entries: Vec<SessionLogEntry>,
    ) -> Self {
        let stats = session_stats_from_entries(&entries);
        Self {
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            entries,
            store: None,
            stats,
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
        let (entries, provider_name, model_name) =
            store.load_entries_writer_reconciled(fallback_provider_name, fallback_model_name)?;
        Ok(Self::from_entries(provider_name, model_name, entries).with_store(store))
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

    pub fn append_control(&mut self, control: ControlEntry) -> Result<()> {
        self.append(SessionLogEntry::Control(control))
    }

    /// Appends a durable domain event that does not project into provider-visible chat history.
    ///
    /// In-memory sessions without a backing store cannot persist durable-only events, so they return
    /// `Ok(None)` instead of fabricating an in-memory fact that would disappear on resume.
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

    /// Returns the provider-visible message projection, including the latest compaction summary.
    pub fn messages(&self) -> Vec<ModelMessage> {
        self.projected_messages()
    }

    pub fn continuation_states(&self, provider_name: &str) -> Vec<ProviderContinuationState> {
        let mut latest_by_key: HashMap<(String, Option<String>), ProviderContinuationState> =
            HashMap::new();
        for entry in &self.entries {
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
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ResponseHandleTracked(handle))
                if handle.provider_name == provider_name =>
            {
                Some(handle.clone())
            }
            _ => None,
        })
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

    pub fn latest_compaction_record(&self) -> Option<CompactionRecord> {
        latest_compaction_record(&self.entries)
    }

    /// Returns durable plan approvals reconstructed from append-only control entries.
    pub fn plan_approval_projection(&self) -> PlanApprovalProjection {
        PlanApprovalProjection::from_entries(&self.entries)
    }

    /// Returns durable plan artifact state reconstructed from append-only control entries.
    pub fn plan_artifact_projection(&self) -> PlanArtifactProjection {
        PlanArtifactProjection::from_entries(&self.entries)
    }

    /// Rebuilds plan artifact state directly from the durable mixed-format event stream.
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

    /// Rebuilds plan approval state directly from the durable mixed-format event stream.
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

    /// Rebuilds resume job state directly from the durable mixed-format event stream.
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

    /// Rebuilds task state directly from the durable mixed-format event stream.
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

    /// Rebuilds the session list row for this session directly from the durable mixed-format
    /// event stream.
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

    /// Rebuilds agent thread state directly from the durable mixed-format event stream.
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

    /// Rebuilds the agent graph projection directly from the durable mixed-format event stream.
    ///
    /// This is a product-surface alias for `try_agent_thread_state_projection_from_durable`, which
    /// remains the lower-level domain name for the same projection state.
    pub fn try_agent_graph_projection_from_durable(
        &self,
    ) -> Result<Option<AgentThreadStateProjection>> {
        self.try_agent_thread_state_projection_from_durable()
    }

    /// Rebuilds the dispatch trace projection directly from the durable mixed-format event stream.
    ///
    /// Dispatch traces are a redacted materialized view over tool, agent, usage, readiness and
    /// egress events. This method keeps new state adoption on the durable replay path instead of
    /// requiring callers to project from in-memory legacy entries.
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

    /// Rebuilds agent profile trust decisions directly from the durable mixed-format event stream.
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

    /// Rebuilds agent profile policy decisions directly from the durable mixed-format event stream.
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

    /// Rebuilds skill state directly from the durable mixed-format event stream.
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

    /// Rebuilds plugin state directly from the durable mixed-format event stream.
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

    /// Rebuilds change set state directly from the durable mixed-format event stream.
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

    /// Rebuilds write-isolation state directly from the durable mixed-format event stream.
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

    /// Rebuilds verification state directly from the durable mixed-format event stream.
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

    /// Rebuilds terminal task state directly from the durable mixed-format event stream.
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

    /// Rebuilds conversation queue state directly from the durable mixed-format event stream.
    pub fn try_conversation_queue_projection_from_durable(
        &self,
    ) -> Result<Option<ConversationQueueProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = ConversationQueueProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_conversation_queue_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    pub fn agent_result_continuation_projection(&self) -> AgentResultContinuationProjection {
        AgentResultContinuationProjection::from_entries(&self.entries)
    }

    /// Rebuilds agent result continuation state directly from the durable mixed-format event stream.
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

    /// Builds one provider request with extra transient messages and runtime-selected Context V0
    /// candidates.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    /// Context assembly failures are recorded as `ContextAssemblySkipped` and degrade to a request
    /// without Context V0.
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
        let memory = self.memory_snapshot_for_request(workspace_root, memory_config)?;
        let projected_messages = self.projected_messages();
        let mut request_messages = memory.messages.clone();
        if let Some(context_message) =
            self.runtime_context_v0_message(&projected_messages, runtime_context)?
        {
            request_messages.push(context_message);
        }
        request_messages.extend(projected_messages);
        request_messages.extend(transient_messages.iter().cloned());

        let materialized_messages =
            serde_json::to_string(&request_messages).context("failed to serialize messages")?;
        let materialized_tools =
            serde_json::to_string(&tools).context("failed to serialize tool specs")?;
        let prefix_materialized = format!("{materialized_messages}\n{materialized_tools}");
        let digest = Sha256::digest(prefix_materialized.as_bytes());
        let mut snapshot = PrefixSnapshot {
            materialized_text: prefix_materialized,
            sha256: format!("{digest:x}"),
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            memory_fingerprint: "none".to_owned(),
            tool_schema_fingerprint: format!("{:x}", Sha256::digest(materialized_tools.as_bytes())),
            skill_index_fingerprint: "none".to_owned(),
        };
        apply_memory_report(&mut snapshot, &memory.report);
        self.append_control(ControlEntry::PrefixSnapshotCaptured(snapshot))?;
        Ok(CompletionRequest {
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            messages: request_messages,
            tools,
            temperature: None,
            max_tokens: None,
            reasoning_effort,
            previous_response_handle,
            continuation_states: self.continuation_states(&self.provider_name),
            traffic_partition_key,
            background: false,
            store: false,
            deterministic_materialization: true,
        })
    }

    fn runtime_context_v0_message(
        &mut self,
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
        match self.build_runtime_context_v0_message(projected_messages, runtime_context) {
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

    fn build_runtime_context_v0_message(
        &self,
        projected_messages: &[ModelMessage],
        runtime_context: RuntimeContextCandidates,
    ) -> Result<Option<ModelMessage>> {
        let mut snippets = BTreeMap::new();
        let mut items = Vec::new();

        if let Some((latest_user_index, query)) = latest_user_context_query(projected_messages) {
            let archive =
                session_archive_from_projected_messages(projected_messages, latest_user_index);
            let hits = archive.search_bm25(&query, REQUEST_CONTEXT_V0_SESSION_ARCHIVE_LIMIT);
            for hit in hits {
                snippets.insert(hit.item.id.clone(), hit.snippet);
                items.push(hit.item);
            }
        }

        if let Some(record) = self.latest_compaction_record()
            && let Some(task_memory) = record.task_memory.as_ref()
            && let Ok(task_items) = task_memory_context_items(task_memory)
        {
            insert_task_memory_context_snippets(task_memory, &mut snippets);
            items.extend(task_items);
        }

        snippets.extend(runtime_context.snippets);
        items.extend(runtime_context.items);

        if items.is_empty() {
            return Ok(None);
        }

        let packed = pack_context_items(
            items,
            ContextPackOptions::new(REQUEST_CONTEXT_V0_MAX_TOKENS),
        )?;
        render_runtime_context_v0_message(&packed, &snippets)
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

    /// Applies one stable compaction record and persists it in the append-only control log.
    ///
    /// # Errors
    ///
    /// Returns an error when compaction is disabled or the session does not yet have enough
    /// history to fold safely.
    pub fn compact_now(&mut self, config: &CompactionConfig) -> Result<CompactionRecord> {
        if !config.enabled {
            bail!("compaction is disabled");
        }

        let raw_messages = self.raw_messages();
        if raw_messages.len() < 2 {
            bail!("session does not have enough history to compact");
        }

        let compacted_message_count = compaction_boundary(&raw_messages, config.tail_messages);
        if compacted_message_count == 0 {
            bail!("session does not have enough stable history to compact");
        }

        let summary = summarize_messages(&raw_messages[..compacted_message_count]);
        let task_memory = self.default_compaction_task_memory(&summary, compacted_message_count)?;
        let record = CompactionRecord {
            summary,
            compacted_message_count,
            retained_tail_message_count: raw_messages.len().saturating_sub(compacted_message_count),
            task_memory,
        };
        self.append_control(ControlEntry::CompactionApplied(record.clone()))?;
        self.stats.last_prompt_tokens = 0;
        Ok(record)
    }

    /// Returns whether the current session has enough stable history to compact safely.
    pub fn can_compact(&self, config: &CompactionConfig) -> bool {
        if !config.enabled {
            return false;
        }

        let raw_messages = self.raw_messages();
        raw_messages.len() >= 2 && compaction_boundary(&raw_messages, config.tail_messages) > 0
    }

    /// Computes a deterministic manual compaction preview without mutating durable state.
    ///
    /// # Errors
    ///
    /// Returns an error when compaction is disabled. Returns `Ok(None)` when the current session
    /// does not yet have enough stable history to fold safely.
    pub fn compaction_preview(
        &self,
        config: &CompactionConfig,
    ) -> Result<Option<CompactionPreview>> {
        if !config.enabled {
            bail!("compaction is disabled");
        }

        let raw_messages = self.raw_messages();
        if raw_messages.len() < 2 {
            return Ok(None);
        }

        let compacted_message_count = compaction_boundary(&raw_messages, config.tail_messages);
        if compacted_message_count == 0 {
            return Ok(None);
        }

        let summary = summarize_messages(&raw_messages[..compacted_message_count]);
        let record = CompactionRecord {
            task_memory: self.default_compaction_task_memory(&summary, compacted_message_count)?,
            summary,
            compacted_message_count,
            retained_tail_message_count: raw_messages.len().saturating_sub(compacted_message_count),
        };
        Ok(Some(CompactionPreview {
            folded_messages: raw_messages[..compacted_message_count].to_vec(),
            projected_messages: projected_messages_with_record(&raw_messages, &record),
            record,
        }))
    }

    pub fn store_path(&self) -> Option<&Path> {
        self.store.as_ref().map(JsonlSessionStore::path)
    }

    /// Returns the next session-stream sequence for synthetic evidence tied to this session.
    ///
    /// Durable-only domain events do not appear in `Session::entries`, so callers that need
    /// stream ordering must read the mixed-format JSONL stream when a store is present.
    pub fn next_stream_sequence_hint(&self) -> Result<u64> {
        let Some(path) = self.store_path() else {
            return Ok((self.entries.len() as u64).saturating_add(1));
        };
        let records = JsonlSessionStore::read_event_records(path)?;
        Ok(next_stream_sequence(&records))
    }

    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    pub fn stats_mut(&mut self) -> &mut SessionStats {
        &mut self.stats
    }

    /// Rebuilds usage and cost statistics directly from the durable mixed-format event stream.
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

    fn default_compaction_task_memory(
        &self,
        summary: &str,
        compacted_message_count: usize,
    ) -> Result<Option<crate::TaskMemoryV1>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let Some(valid_for_snapshot) = latest_task_memory_workspace_snapshot_id(&records)? else {
            return Ok(None);
        };
        let digest = Sha256::digest(
            format!(
                "{}\n{}\n{}\n{}",
                store.path().display(),
                compacted_message_count,
                valid_for_snapshot,
                summary
            )
            .as_bytes(),
        );
        let memory = crate::extract_task_memory_from_stream_records(
            &records,
            crate::TaskMemoryExtractionInput {
                memory_id: format!("task-memory:{digest:x}"),
                valid_for_snapshot,
                branch_id: None,
                supersedes: latest_compaction_record(&self.entries)
                    .and_then(|record| record.task_memory)
                    .map(|memory| memory.memory_id),
                objective: None,
            },
        )?;
        if memory.source_event_ids.is_empty()
            && memory.files_changed.is_empty()
            && memory.commands_run.is_empty()
            && memory.verification_results.is_empty()
            && memory.failed_attempts.is_empty()
            && memory.unresolved_issues.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(memory))
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

    fn raw_messages(&self) -> Vec<ModelMessage> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::User(message)
                | SessionLogEntry::Assistant(message)
                | SessionLogEntry::ToolResult(message) => Some(message.clone()),
                SessionLogEntry::Control(_) => None,
            })
            .collect()
    }

    fn projected_messages(&self) -> Vec<ModelMessage> {
        let raw_messages = self.raw_messages();
        let Some(record) = latest_compaction_record(&self.entries) else {
            return repair_orphan_tool_results(&raw_messages);
        };
        if record.compacted_message_count == 0 || record.summary.trim().is_empty() {
            return repair_orphan_tool_results(&raw_messages);
        }
        repair_orphan_tool_results(&projected_messages_with_record(&raw_messages, &record))
    }
}
