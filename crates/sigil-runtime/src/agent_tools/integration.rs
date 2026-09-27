use super::*;

/// Applies or rejects the pending changeset review owned by one completed child agent.
///
/// The model supplies only the child thread and the semantic decision. The host resolves the
/// durable child result, checks that it still owns the pending review, and delegates the actual
/// parent mutation to the kernel write-isolation authority.
pub(super) fn integrate_agent_changes(
    _runtime: &mut AgentToolRuntime,
    session: &mut Session,
    call: &ToolCall,
    args: &Value,
    options: &AgentRunOptions,
) -> ToolResult {
    let raw_thread_id = match required_string(args, "thread_id") {
        Ok(value) => value,
        Err(error) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
        }
    };
    let thread_id = match AgentThreadId::new(raw_thread_id) {
        Ok(thread_id) => thread_id,
        Err(error) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
        }
    };
    let decision = match optional_string(args, "decision")
        .as_deref()
        .map(parse_merge_decision)
        .transpose()
    {
        Ok(Some(decision)) => decision,
        Ok(None) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                "decision is required",
            );
        }
        Err(error) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
        }
    };
    let decision = match decision {
        MergeDecision::Accepted | MergeDecision::Rejected => decision,
        _ => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                "decision must be accepted or rejected",
            );
        }
    };
    let reason = optional_string(args, "reason");
    let projection = session.agent_thread_state_projection();
    let Some(thread) = projection.threads.get(&thread_id) else {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::NotFound,
            format!("agent thread {} was not found", thread_id.as_str()),
        );
    };
    if !thread.status.is_terminal() {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::InvalidInput,
            format!("agent thread {} is not terminal", thread_id.as_str()),
        );
    }
    let Some(result) = thread.result.as_ref() else {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::InvalidInput,
            format!("agent thread {} has no durable result", thread_id.as_str()),
        );
    };

    let owner_agent_id = format!("agent:{}", thread_id.as_str());
    let write_projection = session.write_isolation_projection();
    let Some(review) = write_projection.merge_reviews.values().find(|review| {
        review.is_pending()
            && review.requested.as_ref().is_some_and(|requested| {
                write_projection
                    .isolated_changesets
                    .get(&requested.changeset_id)
                    .is_some_and(|isolated| isolated.owner_agent_id == owner_agent_id)
            })
    }) else {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::NotFound,
            format!(
                "agent thread {} has no pending changeset merge review",
                thread_id.as_str()
            ),
        );
    };
    let requested = review
        .requested
        .as_ref()
        .expect("pending merge review must have a request");
    let review_id = requested.review_id.clone();
    let changeset_id = requested.changeset_id.clone();
    let isolated_changeset = write_projection
        .isolated_changesets
        .get(&changeset_id)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "merge review {} is missing its durable isolated changeset binding",
                review_id.as_str()
            )
        });
    let isolated_changeset = match isolated_changeset {
        Ok(value) => value,
        Err(error) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
    };
    let durable_change_set = match session
        .changeset_projection()
        .changesets
        .get(&changeset_id)
        .and_then(|state| state.proposal.clone())
    {
        Some(change_set) => change_set,
        None => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                format!(
                    "merge review {} is missing its durable changeset proposal",
                    review_id.as_str()
                ),
            );
        }
    };

    let artifact_content = if decision == MergeDecision::Accepted
        && (isolated_changeset.source_isolation == WriteIsolationMode::Worktree
            || isolated_changeset
                .artifact_ref
                .as_deref()
                .is_some_and(|reference| !reference.starts_with("inline:")))
    {
        let Some(artifact_ref) = isolated_changeset.artifact_ref.as_deref() else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                "merge review is missing its durable artifact reference",
            );
        };
        let Some(recorder) = session.mutation_event_recorder() else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                "merge review requires a durable session store",
            );
        };
        match recorder
            .read_immutable_content_artifact(&artifact_ref.to_owned())
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|error| anyhow!("changeset artifact is not UTF-8: {error}"))
            }) {
            Ok(content) => content,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    format!("failed to read durable changeset artifact: {error:#}"),
                );
            }
        }
    } else if decision == MergeDecision::Accepted {
        let final_text = match read_agent_final_answer_text(session, result) {
            Ok(text) => text,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    format!("failed to read child changeset result: {error:#}"),
                );
            }
        };
        let proposal = match decode_changeset_only_child_output(&final_text) {
            Ok(proposal) => proposal,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    format!("child result is not a valid changeset proposal: {error:#}"),
                );
            }
        };
        if proposal.change_set != durable_change_set {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                "child changeset proposal differs from the durable merge-review proposal",
            );
        }
        proposal.artifact.content
    } else {
        String::new()
    };

    let outcome = match sigil_kernel::resolve_merge_review_parent_mutation(
        session,
        MergeReviewParentMutationRequest {
            review_id,
            decision,
            reason,
            change_set: durable_change_set,
            artifact_content,
            workspace_root: options.workspace_root.clone(),
            tool_call_id: call.id.clone(),
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
    };
    let payload = json!({
        "review_id": outcome.review_id.as_str(),
        "decision": outcome.decision,
        "changeset_id": changeset_id.as_str(),
        "batch_id": outcome.batch_id,
        "batch_status": outcome.batch_status,
        "committed_operations": outcome.committed_operations,
        "failed_operations": outcome.failed_operations,
        "parent_mutated": !outcome.committed_operations.is_empty(),
        "next_action": if outcome.decision == MergeDecision::Accepted {
            "inspect the mutation result and continue the root Task"
        } else {
            "continue the root Task without applying this child proposal"
        },
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize merge outcome: {error}")),
        ToolResultMeta {
            details: payload,
            ..ToolResultMeta::default()
        },
    )
}

fn parse_merge_decision(value: &str) -> Result<MergeDecision> {
    match value {
        "accepted" => Ok(MergeDecision::Accepted),
        "rejected" => Ok(MergeDecision::Rejected),
        other => anyhow::bail!("unsupported merge decision {other:?}"),
    }
}
