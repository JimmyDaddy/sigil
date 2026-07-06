use sigil_kernel::{
    AgentArtifactRef, AgentFinalAnswerRef, AgentRunOutcome, AgentRunResult, AgentThreadId,
    AgentThreadResult, AgentThreadTerminalStatus, AgentUsageSummary, SessionRef,
};

use super::hash_text;

const AGENT_RESULT_SUMMARY_LIMIT: usize = 4_000;

struct BoundedAgentSummary {
    text: String,
    truncated: bool,
    original_chars: Option<usize>,
}

fn bounded_agent_summary(final_text: &str) -> BoundedAgentSummary {
    let trimmed = final_text.trim();
    let original_chars = trimmed.chars().count();
    let text = trimmed
        .chars()
        .take(AGENT_RESULT_SUMMARY_LIMIT)
        .collect::<String>();
    let rendered_chars = text.chars().count();
    let truncated = original_chars > rendered_chars;
    BoundedAgentSummary {
        text,
        truncated,
        original_chars: truncated.then_some(original_chars),
    }
}

pub(super) fn build_agent_thread_result(
    thread_id: AgentThreadId,
    session_ref: SessionRef,
    status: AgentThreadTerminalStatus,
    final_text: &str,
    outcome: &AgentRunOutcome,
    usage: Option<AgentUsageSummary>,
    final_answer_ref: Option<AgentFinalAnswerRef>,
) -> AgentThreadResult {
    let summary = bounded_agent_summary(final_text);
    AgentThreadResult {
        thread_id,
        session_ref: session_ref.clone(),
        status,
        summary: summary.text,
        summary_truncated: summary.truncated,
        original_summary_chars: summary.original_chars,
        artifacts: agent_result_artifacts(&session_ref, final_text),
        changed_paths: outcome.changed_files.clone(),
        risks: Vec::new(),
        followups: Vec::new(),
        usage,
        output_hash: hash_text(final_text),
        final_answer_ref,
    }
}

pub(crate) fn agent_final_answer_ref(
    session_ref: &SessionRef,
    result: &AgentRunResult,
) -> Option<AgentFinalAnswerRef> {
    let message_id = result.final_message_id.as_ref()?;
    Some(AgentFinalAnswerRef {
        session_ref: session_ref.clone(),
        message_id: message_id.clone(),
        content_hash: hash_text(&result.final_text),
        char_count: result.final_text.chars().count(),
    })
}

fn agent_result_artifacts(
    child_session_ref: &SessionRef,
    final_text: &str,
) -> Vec<AgentArtifactRef> {
    vec![AgentArtifactRef {
        kind: "child_session".to_owned(),
        path: child_session_ref.as_path().display().to_string(),
        hash: Some(hash_text(final_text)),
    }]
}
