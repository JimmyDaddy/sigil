use super::*;

pub(super) fn profile_index_description(index: &crate::ModelVisibleAgentIndex) -> String {
    if index.entries.is_empty() {
        return "No trusted model-invocable agent profiles are currently available.".to_owned();
    }
    let entries = index
        .entries
        .iter()
        .map(|entry| {
            format!(
                "- {}: {:?}; result_policy={}; {}",
                entry.profile_id.as_str(),
                entry.kind,
                entry.result_policy.as_str(),
                entry.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if index.hidden_count == 0 {
        format!("Available profile_id values:\n{entries}")
    } else {
        format!(
            "Available profile_id values:\n{entries}\n{} additional profiles hidden by index limit.",
            index.hidden_count
        )
    }
}

pub(super) fn agent_profile_system_prompt(profile: &ResolvedAgentProfile) -> Option<String> {
    let mut parts = Vec::new();
    if !profile.profile.description.trim().is_empty() {
        parts.push(format!(
            "Agent profile: {}\nDescription: {}",
            profile.profile.id.as_str(),
            profile.profile.description.trim()
        ));
    }
    if !profile.profile.instructions.trim().is_empty() {
        parts.push(format!(
            "Instructions:\n{}",
            profile.profile.instructions.trim()
        ));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub(super) fn simple_agent_preview(title: &str, summary: &str) -> ToolPreview {
    ToolPreview {
        title: title.to_owned(),
        summary: summary.to_owned(),
        body: summary.to_owned(),
        changed_files: Vec::new(),
        file_diffs: Vec::new(),
    }
}

pub(super) fn parse_tool_args(call: &ToolCall) -> Result<Value> {
    serde_json::from_str(&call.args_json)
        .with_context(|| format!("invalid tool args for {}", call.name))
}

pub(super) fn required_string(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing required string field {key}"))
}

pub(super) fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn thread_id_arg(args: &Value) -> Result<AgentThreadId> {
    AgentThreadId::new(required_string(args, "thread_id")?)
}

pub(super) fn parse_invocation_mode(value: &str) -> Result<AgentInvocationMode> {
    match value {
        "foreground" => Ok(AgentInvocationMode::Foreground),
        "join_before_final" => Ok(AgentInvocationMode::JoinBeforeFinal),
        "background" => Ok(AgentInvocationMode::Background),
        other => Err(anyhow!("unsupported agent invocation mode {other}")),
    }
}

pub(super) fn invocation_mode_label(mode: AgentInvocationMode) -> &'static str {
    match mode {
        AgentInvocationMode::Foreground => "foreground",
        AgentInvocationMode::Background => "background",
        AgentInvocationMode::JoinBeforeFinal => "join_before_final",
        AgentInvocationMode::Unknown => "unknown",
    }
}

pub(super) fn parent_session_ref(session: &Session) -> Result<SessionRef> {
    let Some(path) = session.store_path() else {
        return SessionRef::new_relative("current.jsonl");
    };
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("parent session path has no file name"))?;
    SessionRef::new_relative(PathBuf::from(file_name))
}

pub(super) fn agent_child_session_ref(thread_id: &AgentThreadId) -> Result<SessionRef> {
    SessionRef::new_relative(
        PathBuf::from("children")
            .join("agents")
            .join(format!("{}.jsonl", thread_id.as_str())),
    )
}

pub(super) fn build_agent_child_session(
    parent_session: &Session,
    child_ref: &SessionRef,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store = JsonlSessionStore::new(child_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        crate::attach_session_url_capability_store(&mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    crate::attach_session_url_capability_store(&mut session)?;
    Ok(session)
}

pub(super) fn chat_budget_scope_id(call_id: &str) -> Result<TaskId> {
    TaskId::new(format!("chat_{}", short_digest(&hash_text(call_id))))
}

pub(super) fn manual_agent_call_id(
    _session: &Session,
    profile_id: &AgentProfileId,
    _prompt: &str,
) -> String {
    format!(
        "manual_agent_{}_{}",
        profile_id.as_str(),
        uuid::Uuid::new_v4().simple()
    )
}

pub(super) fn usage_summary_from_stats(stats: &sigil_kernel::SessionStats) -> AgentUsageSummary {
    AgentUsageSummary {
        input_tokens: stats.prompt_tokens,
        output_tokens: stats.completion_tokens,
        total_tokens: stats.prompt_tokens + stats.completion_tokens,
        cached_tokens: Some(stats.cache_hit_tokens),
    }
}

pub(super) fn child_status_from_outcome(
    final_text: &str,
    outcome: &sigil_kernel::AgentRunOutcome,
) -> TaskChildSessionStatus {
    if outcome.terminal_reason == sigil_kernel::AgentRunTerminalReason::MaxTurns
        || !outcome.interrupted_tool_calls.is_empty()
    {
        TaskChildSessionStatus::Interrupted
    } else if outcome.approval_denials > 0
        || (!outcome.tool_errors.is_empty() && final_text.trim().is_empty())
    {
        TaskChildSessionStatus::Failed
    } else {
        TaskChildSessionStatus::Completed
    }
}

pub(super) fn bounded_summary(summary: &str, max_chars: usize) -> String {
    summary.chars().take(max_chars).collect()
}

pub(super) fn terminal_status_label(status: AgentThreadTerminalStatus) -> &'static str {
    match status {
        AgentThreadTerminalStatus::Completed => "completed",
        AgentThreadTerminalStatus::Failed => "failed",
        AgentThreadTerminalStatus::Cancelled => "cancelled",
        AgentThreadTerminalStatus::Interrupted => "interrupted",
        AgentThreadTerminalStatus::Unknown => "unknown",
    }
}

pub(super) fn thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Started => "started",
        AgentThreadStatus::Running => "running",
        AgentThreadStatus::Blocked => "blocked",
        AgentThreadStatus::Completed => "completed",
        AgentThreadStatus::Failed => "failed",
        AgentThreadStatus::Cancelled => "cancelled",
        AgentThreadStatus::Interrupted => "interrupted",
        AgentThreadStatus::Closed => "closed",
        AgentThreadStatus::Unavailable => "unavailable",
        AgentThreadStatus::Unknown => "unknown",
    }
}

pub(super) fn agent_route_id_for_call(
    thread_id: &AgentThreadId,
    call_id: &str,
) -> Result<AgentRouteId> {
    AgentRouteId::new(format!(
        "agent_route_{}",
        short_digest(&hash_text(&format!("{}:{}", thread_id.as_str(), call_id)))
    ))
}

pub(super) fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn short_digest(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

pub(super) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
