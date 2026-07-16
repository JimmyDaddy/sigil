use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum SessionTransitionKind {
    Switch,
    StartNew,
}

impl SessionTransitionKind {
    pub(in crate::runner) fn block_reason(
        self,
        foreground_run_active: bool,
        background_run_active: bool,
    ) -> Option<&'static str> {
        if foreground_run_active {
            return Some(match self {
                Self::Switch => "cannot switch sessions while the agent is running",
                Self::StartNew => "cannot start a new session while the agent is running",
            });
        }
        background_run_active.then_some(match self {
            Self::Switch => "cannot switch sessions while a background agent is running",
            Self::StartNew => "cannot start a new session while a background agent is running",
        })
    }

    fn trust_reason(self) -> &'static str {
        match self {
            Self::Switch => "trusted workspace carried into session",
            Self::StartNew => "trusted workspace carried into new session",
        }
    }

    fn message(
        self,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    ) -> WorkerMessage {
        match self {
            Self::Switch => WorkerMessage::SessionSwitched {
                session_log_path,
                provider_name,
                model_name,
                entries,
            },
            Self::StartNew => WorkerMessage::NewSessionStarted {
                session_log_path,
                provider_name,
                model_name,
                entries,
            },
        }
    }
}

pub(in crate::runner) fn transition_session(
    kind: SessionTransitionKind,
    session_log_path: PathBuf,
    root_config: &RootConfig,
    workspace_root: &Path,
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> std::result::Result<WorkerMessage, String> {
    if let Some(reason) = kind.block_reason(
        state.run.active.is_some(),
        state.agent.background_runs.has_any(),
    ) {
        return Err(reason.to_owned());
    }

    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        &session_log_path,
        state.session.current.as_ref(),
    )
    .map_err(|error| format!("{error:#}"))?;
    let same_logical_session = state
        .session
        .current
        .as_ref()
        .is_some_and(|current| current.session_scope_id() == session.session_scope_id());
    let empty_exact_prompts = ExactConversationPromptStore::new();
    let target_exact_prompts = if same_logical_session {
        &state.session.exact_prompts
    } else {
        &empty_exact_prompts
    };
    mark_stale_dispatching_conversation_queue_items(&mut session, target_exact_prompts, message_tx);

    if state
        .session
        .current
        .as_ref()
        .is_some_and(|session| session_workspace_is_trusted(session, workspace_root))
    {
        ensure_session_workspace_trust(&mut session, workspace_root, kind.trust_reason())?;
    }

    let pending_agent_result_continuations =
        pending_agent_result_continuations_from_session(Some(&session));
    let provider_name = session.provider_name().to_owned();
    let model_name = session.model_name().to_owned();
    let entries = session.entries().to_vec();

    state.compaction.preparation_tasks.abort_all();
    state.compaction.pending = None;
    state.compaction.idle_auto = IdleAutoCompactionState::default();
    state.session.pending_queued_pre_turn_preparation = None;
    state.session.last_queued_pre_turn_block = None;
    state.session.pending_agent_result_continuations = pending_agent_result_continuations;
    if !same_logical_session {
        state.session.exact_prompts.clear();
    }
    state.session.current = Some(session);
    state.session.log_path = session_log_path.clone();

    Ok(kind.message(session_log_path, provider_name, model_name, entries))
}
