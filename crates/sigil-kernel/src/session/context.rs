use super::*;

pub(super) fn latest_user_context_query_from_projection(
    projection: &SessionContextProjection,
    projected_messages: &[ModelMessage],
) -> Option<(usize, String)> {
    projected_messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            let projected_origin = projection
                .retained_entries
                .iter()
                .find(|entry| entry.message.id == message.id)
                .map(|entry| entry.origin);
            let user_authored = projected_origin.map_or_else(
                || {
                    matches!(message.role, MessageRole::User)
                        && !is_runtime_context_v2_message(message)
                },
                SessionProjectionOrigin::is_user_authored,
            );
            if !user_authored {
                return None;
            }
            let content = message.content.as_deref()?.trim();
            (!content.is_empty()).then(|| (index, content.to_owned()))
        })
}

pub(super) fn session_archive_from_projected_messages_with_external(
    projection: &SessionContextProjection,
    projected_messages: &[ModelMessage],
    latest_user_index: usize,
    external_message_ids: &std::collections::BTreeSet<String>,
) -> SessionArchive {
    projected_messages
        .iter()
        .take(latest_user_index)
        .enumerate()
        .flat_map(|(index, message)| {
            let is_runtime_context_snapshot = projection.retained_entries.iter().any(|entry| {
                entry.message.id == message.id
                    && entry.origin == SessionProjectionOrigin::RuntimeContextSnapshotV2
            });
            if is_runtime_context_snapshot {
                return Vec::new();
            }
            session_archive_entries_from_message_with_external(
                index,
                message,
                external_message_ids.contains(&message.id),
            )
        })
        .fold(SessionArchive::new(), |archive, entry| {
            archive.with_entry(entry)
        })
}

pub(super) fn session_archive_entries_from_message_with_external(
    index: usize,
    message: &ModelMessage,
    external_untrusted: bool,
) -> Vec<SessionArchiveEntry> {
    if is_runtime_context_v2_message(message) {
        return Vec::new();
    }
    let Some(content) = message.content.as_deref().map(str::trim) else {
        return Vec::new();
    };
    if content.is_empty() || matches!(message.role, MessageRole::System) {
        return Vec::new();
    }
    let (role, source, trust_level, sensitivity) = if external_untrusted {
        (
            match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                MessageRole::System => return Vec::new(),
            },
            ContextSource::ExternalSource,
            ContextTrustLevel::ExternalUntrusted,
            ContextSensitivity::External,
        )
    } else {
        match message.role {
            MessageRole::System => return Vec::new(),
            MessageRole::User => (
                "user",
                ContextSource::UserMessage,
                ContextTrustLevel::UserProvided,
                ContextSensitivity::Public,
            ),
            MessageRole::Assistant => (
                "assistant",
                ContextSource::ToolObservation,
                ContextTrustLevel::ToolObservation,
                ContextSensitivity::Repository,
            ),
            MessageRole::Tool => (
                "tool",
                ContextSource::ToolObservation,
                ContextTrustLevel::ToolObservation,
                ContextSensitivity::Repository,
            ),
        }
    };
    let chunks = chunk_runtime_context_body(
        content,
        REQUEST_CONTEXT_V0_ENTRY_MAX_BYTES,
        REQUEST_CONTEXT_V0_ENTRY_OVERLAP_BYTES,
    );
    let chunk_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let body = if chunk_count == 1 {
                format!("{role}: {chunk}")
            } else {
                format!("{role} chunk {}/{}: {chunk}", chunk_index + 1, chunk_count)
            };
            let digest = Sha256::digest(body.as_bytes());
            let entry = SessionArchiveEntry::new(
                format!("message:{index}:{chunk_index}:{digest:x}"),
                source.clone(),
                body,
                trust_level,
                sensitivity,
            );
            if external_untrusted {
                entry.egress_decision("external_safe_persistence")
            } else {
                entry
            }
        })
        .collect()
}

pub(super) fn chunk_runtime_context_body(
    value: &str,
    max_bytes: usize,
    overlap_bytes: usize,
) -> Vec<String> {
    if value.len() <= max_bytes {
        return vec![value.to_owned()];
    }

    let max_bytes = max_bytes.max(1);
    let overlap_bytes = overlap_bytes.min(max_bytes.saturating_sub(1));
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < value.len() {
        let mut end = start.saturating_add(max_bytes).min(value.len());
        while end > start && !value.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        chunks.push(value[start..end].to_owned());
        if end == value.len() {
            break;
        }
        let mut next_start = end.saturating_sub(overlap_bytes);
        while next_start > start && !value.is_char_boundary(next_start) {
            next_start -= 1;
        }
        if next_start <= start {
            next_start = end;
        }
        start = next_start;
    }
    chunks
}

pub(super) fn truncate_runtime_context_body(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &value[..end])
}

pub(super) fn insert_task_memory_context_snippets(
    memory: &TaskMemoryV1,
    snippets: &mut BTreeMap<String, String>,
) {
    snippets.insert(
        format!("task-memory:{}:objective", memory.memory_id),
        truncate_runtime_context_body(&memory.objective, 160),
    );
    if let Some(plan) = &memory.active_plan {
        for (index, step) in plan.steps.iter().enumerate() {
            snippets.insert(
                format!("task-memory:{}:active-plan:{index}", memory.memory_id),
                truncate_runtime_context_body(&format!("{:?}: {}", step.status, step.title), 160),
            );
        }
    }
    for (index, decision) in memory.decisions.iter().enumerate() {
        snippets.insert(
            format!("task-memory:{}:decision:{index}", memory.memory_id),
            truncate_runtime_context_body(&decision.decision.text, 160),
        );
    }
    for (index, issue) in memory.unresolved_issues.iter().enumerate() {
        snippets.insert(
            format!("task-memory:{}:unresolved:{index}", memory.memory_id),
            truncate_runtime_context_body(&issue.text, 160),
        );
    }
    for (index, file) in memory.files_changed.iter().enumerate() {
        snippets.insert(
            format!("task-memory:{}:file:{index}", memory.memory_id),
            format!("changed file: {}", file.path.display()),
        );
    }
}

pub(super) fn render_runtime_context_v2_content(
    packed: &PackedContext,
    snippets: &BTreeMap<String, String>,
) -> Result<String> {
    let included = packed
        .stable_prefix
        .iter()
        .chain(packed.dynamic_suffix.iter())
        .map(|item| runtime_context_item_json(item, snippets))
        .collect::<Result<Vec<_>>>()?;
    let excluded = packed
        .excluded
        .iter()
        .map(|item| runtime_context_item_json(item, snippets))
        .collect::<Result<Vec<_>>>()?;
    let payload = serde_json::json!({
        "schema": RUNTIME_CONTEXT_V2_SCHEMA,
        "placement": RUNTIME_CONTEXT_V2_PLACEMENT,
        "selection_policy": RUNTIME_CONTEXT_V2_SELECTION_POLICY,
        "note": RUNTIME_CONTEXT_V2_NOTE,
        "state": "active",
        "budget": {
            "max_tokens": packed.max_tokens,
            "used_tokens": packed.used_tokens,
        },
        "included": included,
        "excluded": excluded,
    });
    let payload = serde_json::to_string_pretty(&payload)
        .context("failed to serialize runtime context v2 payload")?;
    Ok(format!("{RUNTIME_CONTEXT_V2_HEADING}\n{payload}"))
}

pub(super) fn render_runtime_context_v2_clear_content() -> Result<String> {
    let payload = serde_json::json!({
        "schema": RUNTIME_CONTEXT_V2_SCHEMA,
        "placement": RUNTIME_CONTEXT_V2_PLACEMENT,
        "selection_policy": RUNTIME_CONTEXT_V2_SELECTION_POLICY,
        "note": RUNTIME_CONTEXT_V2_NOTE,
        "state": "cleared",
        "budget": {
            "max_tokens": REQUEST_CONTEXT_V0_MAX_TOKENS,
            "used_tokens": 0,
        },
        "included": [],
        "excluded": [],
    });
    let payload = serde_json::to_string_pretty(&payload)
        .context("failed to serialize cleared runtime context v2 payload")?;
    Ok(format!("{RUNTIME_CONTEXT_V2_HEADING}\n{payload}"))
}

pub(super) fn summarize_runtime_context(packed: &PackedContext) -> PrefixRuntimeContextSummary {
    let mut included = packed
        .stable_prefix
        .iter()
        .chain(packed.dynamic_suffix.iter())
        .enumerate()
        .collect::<Vec<_>>();
    included.sort_by(|left, right| {
        right
            .1
            .token_cost
            .cmp(&left.1.token_cost)
            .then_with(|| left.1.id.cmp(&right.1.id))
            .then_with(|| left.0.cmp(&right.0))
    });
    let top_included = included
        .into_iter()
        .take(PREFIX_SNAPSHOT_CONTEXT_ITEM_LIMIT)
        .map(|(_, item)| PrefixRuntimeContextItemSummary {
            source: item.source.clone(),
            inclusion_reason: item.inclusion_reason.clone(),
            token_cost: item.token_cost,
        })
        .collect();

    let mut excluded_counts = BTreeMap::<ContextInclusionReason, usize>::new();
    for item in &packed.excluded {
        *excluded_counts
            .entry(item.inclusion_reason.clone())
            .or_default() += 1;
    }
    let excluded_by_reason = excluded_counts
        .into_iter()
        .map(|(reason, count)| PrefixRuntimeContextExclusionSummary { reason, count })
        .collect();

    PrefixRuntimeContextSummary {
        schema: RUNTIME_CONTEXT_V2_SCHEMA.to_owned(),
        max_tokens: packed.max_tokens,
        used_tokens: packed.used_tokens,
        included_count: packed
            .stable_prefix
            .len()
            .saturating_add(packed.dynamic_suffix.len()),
        excluded_count: packed.excluded.len(),
        top_included,
        excluded_by_reason,
    }
}

pub(super) fn runtime_context_item_json(
    item: &ContextItem,
    snippets: &BTreeMap<String, String>,
) -> Result<serde_json::Value> {
    let provenance = context_provenance_row_v1(item, None, None, None);
    Ok(serde_json::json!({
        "id": &item.id,
        "source": &item.source,
        "source_ref": &provenance.source_ref,
        "source_event_id": &item.source_event_id,
        "trust_level": &item.trust_level,
        "sensitivity": &item.sensitivity,
        "egress_decision": &item.egress_decision,
        "repo_revision": &provenance.repo_revision,
        "token_cost": item.token_cost,
        "score": item.score,
        "score_breakdown": &provenance.score_breakdown,
        "score_missing_reason": &provenance.score_missing_reason,
        "inclusion_reason": &item.inclusion_reason,
        "why_included": &provenance.why_included,
        "why_excluded": &provenance.why_excluded,
        "placement_missing_reason": &provenance.placement_missing_reason,
        "body_ref": &item.body_ref,
        "snippet": renderable_runtime_context_snippet(item, snippets)?,
    }))
}

pub(super) fn renderable_runtime_context_snippet<'a>(
    item: &ContextItem,
    snippets: &'a BTreeMap<String, String>,
) -> Result<Option<&'a str>> {
    if !item.inclusion_reason.is_included() {
        return Ok(None);
    }
    if matches!(
        item.sensitivity,
        ContextSensitivity::PotentialSecret
            | ContextSensitivity::Secret
            | ContextSensitivity::External
    ) && item.egress_decision.is_none()
    {
        return Ok(None);
    }
    let Some(snippet) = snippets.get(&item.id) else {
        return Ok(None);
    };
    validate_context_render_snippet(item, snippet, DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES)?;
    Ok(Some(snippet.as_str()))
}

pub(super) fn empty_runtime_context_v2_summary() -> PrefixRuntimeContextSummary {
    PrefixRuntimeContextSummary {
        schema: RUNTIME_CONTEXT_V2_SCHEMA.to_owned(),
        max_tokens: REQUEST_CONTEXT_V0_MAX_TOKENS,
        used_tokens: 0,
        included_count: 0,
        excluded_count: 0,
        top_included: Vec::new(),
        excluded_by_reason: Vec::new(),
    }
}

pub(super) fn is_runtime_context_v2_message(message: &ModelMessage) -> bool {
    message.id.starts_with("context:v2:")
}

pub(super) fn runtime_context_v2_state_from_message(
    message: &ModelMessage,
) -> Result<RuntimeContextSnapshotStateV2> {
    if !is_runtime_context_v2_message(message) || message.role != MessageRole::User {
        bail!("message is not a runtime context snapshot v2");
    }
    let content = message
        .content
        .as_deref()
        .context("runtime context snapshot v2 message is missing content")?;
    let payload = content
        .strip_prefix(RUNTIME_CONTEXT_V2_HEADING)
        .context("runtime context snapshot v2 message has an invalid heading")?
        .trim();
    let value: serde_json::Value = serde_json::from_str(payload)
        .context("runtime context snapshot v2 payload is not valid JSON")?;
    match value.get("state").and_then(serde_json::Value::as_str) {
        Some("active") => Ok(RuntimeContextSnapshotStateV2::Active),
        Some("cleared") => Ok(RuntimeContextSnapshotStateV2::Cleared),
        _ => bail!("runtime context snapshot v2 payload has an invalid state"),
    }
}

pub(super) fn stable_runtime_context_v2_message_id(
    source_tail_message_id: Option<&str>,
    content: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(source_tail_message_id.unwrap_or("<empty-tail>").as_bytes());
    digest.update(b"\0");
    digest.update(content.as_bytes());
    format!("context:v2:{:x}", digest.finalize())
}

impl RuntimeContextSnapshotV2 {
    pub(super) fn new(
        state: RuntimeContextSnapshotStateV2,
        content: String,
        source_tail_message_id: Option<String>,
    ) -> Self {
        let canonical_content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        let message = ModelMessage {
            id: stable_runtime_context_v2_message_id(source_tail_message_id.as_deref(), &content),
            role: MessageRole::User,
            content: Some(content),
            tool_calls: Vec::new(),
            tool_call_id: None,
            assistant_kind: None,
            image_attachments: Vec::new(),
            tool_result_payload: None,
        };
        Self {
            schema_version: RUNTIME_CONTEXT_SNAPSHOT_V2_SCHEMA_VERSION,
            state,
            message,
            canonical_content_sha256,
            source_tail_message_id,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.schema_version != RUNTIME_CONTEXT_SNAPSHOT_V2_SCHEMA_VERSION {
            bail!(
                "unsupported runtime context snapshot v2 schema {}",
                self.schema_version
            );
        }
        if self.message.role != MessageRole::User
            || !self.message.tool_calls.is_empty()
            || self.message.tool_call_id.is_some()
            || self.message.assistant_kind.is_some()
            || !self.message.image_attachments.is_empty()
            || self.message.tool_result_payload.is_some()
        {
            bail!("runtime context snapshot v2 message has an invalid provider-visible shape");
        }
        let content = self
            .message
            .content
            .as_deref()
            .context("runtime context snapshot v2 message is missing content")?;
        if content.len() > MAX_RUNTIME_CONTEXT_SNAPSHOT_V2_BYTES {
            bail!(
                "runtime context snapshot v2 exceeds {MAX_RUNTIME_CONTEXT_SNAPSHOT_V2_BYTES} bytes"
            );
        }
        if !content.starts_with(RUNTIME_CONTEXT_V2_HEADING) {
            bail!("runtime context snapshot v2 message has an invalid heading");
        }
        let observed_hash = format!("{:x}", Sha256::digest(content.as_bytes()));
        if observed_hash != self.canonical_content_sha256 {
            bail!("runtime context snapshot v2 content hash does not match its message");
        }
        let expected_id =
            stable_runtime_context_v2_message_id(self.source_tail_message_id.as_deref(), content);
        if self.message.id != expected_id {
            bail!("runtime context snapshot v2 message id does not match its source tail");
        }
        let payload = content
            .strip_prefix(RUNTIME_CONTEXT_V2_HEADING)
            .context("runtime context snapshot v2 heading is malformed")?
            .trim();
        let value: serde_json::Value = serde_json::from_str(payload)
            .context("runtime context snapshot v2 payload is not valid JSON")?;
        let expected_state = match self.state {
            RuntimeContextSnapshotStateV2::Active => "active",
            RuntimeContextSnapshotStateV2::Cleared => "cleared",
        };
        if value.get("schema").and_then(serde_json::Value::as_str)
            != Some(RUNTIME_CONTEXT_V2_SCHEMA)
            || value.get("placement").and_then(serde_json::Value::as_str)
                != Some(RUNTIME_CONTEXT_V2_PLACEMENT)
            || value.get("state").and_then(serde_json::Value::as_str) != Some(expected_state)
        {
            bail!("runtime context snapshot v2 payload conflicts with its durable metadata");
        }
        Ok(())
    }

    pub(super) fn from_provider_message(
        message: ModelMessage,
        source_tail_message_id: Option<String>,
    ) -> Result<Self> {
        let state = runtime_context_v2_state_from_message(&message)?;
        let content = message
            .content
            .as_deref()
            .context("runtime context snapshot v2 message is missing content")?;
        let snapshot = Self {
            schema_version: RUNTIME_CONTEXT_SNAPSHOT_V2_SCHEMA_VERSION,
            state,
            canonical_content_sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
            source_tail_message_id,
            message,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}
