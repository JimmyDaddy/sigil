use anyhow::{Context, Result};
use serde_json::json;
use sigil_kernel::{
    AgentArtifactRef, AgentFinalAnswerRef, AgentRunOutcome, AgentRunResult, AgentThreadId,
    AgentThreadResult, AgentThreadTerminalStatus, AgentUsageSummary, AssistantMessageKind,
    ModelMessage, Session, SessionRef,
};

use super::hash_text;

const AGENT_RESULT_SUMMARY_LIMIT: usize = 4_000;
const AGENT_RESULT_ARTIFACT_THRESHOLD: usize = AGENT_RESULT_SUMMARY_LIMIT;
const AGENT_RESULT_EXCERPT_LIMIT: usize = 1_200;
const AGENT_FINAL_REPORT_ARTIFACT_KIND: &str = "final_report";

#[derive(Debug, Clone)]
pub(crate) struct AgentResultMaterialization {
    pub(crate) final_text: String,
    pub(crate) final_answer_ref: Option<AgentFinalAnswerRef>,
    pub(crate) extra_artifacts: Vec<AgentArtifactRef>,
    pub(crate) original_summary_chars: Option<usize>,
}

impl AgentResultMaterialization {
    pub(crate) fn inline(
        final_text: impl Into<String>,
        final_answer_ref: Option<AgentFinalAnswerRef>,
    ) -> Self {
        Self {
            final_text: final_text.into(),
            final_answer_ref,
            extra_artifacts: Vec::new(),
            original_summary_chars: None,
        }
    }
}

struct BoundedAgentSummary {
    text: String,
    truncated: bool,
    original_chars: Option<usize>,
}

fn bounded_agent_summary(
    final_text: &str,
    original_summary_chars: Option<usize>,
) -> BoundedAgentSummary {
    let trimmed = final_text.trim();
    let original_chars = original_summary_chars.unwrap_or_else(|| trimmed.chars().count());
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
    materialized: &AgentResultMaterialization,
    outcome: &AgentRunOutcome,
    usage: Option<AgentUsageSummary>,
) -> AgentThreadResult {
    let summary = bounded_agent_summary(
        &materialized.final_text,
        materialized.original_summary_chars,
    );
    AgentThreadResult {
        thread_id,
        session_ref: session_ref.clone(),
        status,
        summary: summary.text,
        summary_truncated: summary.truncated,
        original_summary_chars: summary.original_chars,
        artifacts: agent_result_artifacts(
            &session_ref,
            &materialized.final_text,
            materialized.extra_artifacts.clone(),
        ),
        changed_paths: outcome.changed_files.clone(),
        risks: Vec::new(),
        followups: Vec::new(),
        usage,
        output_hash: hash_text(&materialized.final_text),
        final_answer_ref: materialized.final_answer_ref.clone(),
    }
}

pub(crate) async fn materialize_child_agent_final_answer(
    child_session: &mut Session,
    child_session_ref: &SessionRef,
    thread_id: &AgentThreadId,
    result: &AgentRunResult,
) -> Result<AgentResultMaterialization> {
    let original_chars = result.final_text.trim().chars().count();
    let can_write_artifact = child_session.store_path().is_some();
    if original_chars <= AGENT_RESULT_ARTIFACT_THRESHOLD || !can_write_artifact {
        return Ok(AgentResultMaterialization::inline(
            result.final_text.clone(),
            agent_final_answer_ref(child_session_ref, result),
        ));
    }

    let full_result_hash = hash_text(&result.final_text);
    let artifact_ref = AgentArtifactRef {
        kind: AGENT_FINAL_REPORT_ARTIFACT_KIND.to_owned(),
        path: final_report_artifact_ref_path(child_session_ref)
            .display()
            .to_string(),
        hash: Some(full_result_hash),
    };
    let artifact_path = final_report_artifact_store_path(child_session)
        .context("child session has no artifact store path")?;
    if let Some(parent) = artifact_path.parent() {
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to create agent result artifact directory {}",
                parent.display()
            )
        })?;
    }
    tokio::fs::write(&artifact_path, result.final_text.as_bytes())
        .await
        .with_context(|| {
            format!(
                "failed to write child agent final report artifact {}",
                artifact_path.display()
            )
        })?;

    let final_text =
        compact_child_agent_final_text(thread_id, original_chars, &artifact_ref, result);
    let final_answer_ref =
        append_compact_final_answer(child_session, child_session_ref, final_text.clone())?;
    Ok(AgentResultMaterialization {
        final_text,
        final_answer_ref: Some(final_answer_ref),
        extra_artifacts: vec![artifact_ref],
        original_summary_chars: Some(original_chars),
    })
}

fn agent_final_answer_ref(
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
    mut extra_artifacts: Vec<AgentArtifactRef>,
) -> Vec<AgentArtifactRef> {
    let mut artifacts = vec![AgentArtifactRef {
        kind: "child_session".to_owned(),
        path: child_session_ref.as_path().display().to_string(),
        hash: Some(hash_text(final_text)),
    }];
    artifacts.append(&mut extra_artifacts);
    artifacts
}

fn final_report_artifact_ref_path(child_session_ref: &SessionRef) -> std::path::PathBuf {
    child_session_ref.as_path().with_extension("final.md")
}

fn final_report_artifact_store_path(child_session: &Session) -> Option<std::path::PathBuf> {
    child_session
        .store_path()
        .map(|child_store_path| child_store_path.with_extension("final.md"))
}

fn compact_child_agent_final_text(
    thread_id: &AgentThreadId,
    original_chars: usize,
    artifact_ref: &AgentArtifactRef,
    result: &AgentRunResult,
) -> String {
    let excerpt = result
        .final_text
        .trim()
        .chars()
        .take(AGENT_RESULT_EXCERPT_LIMIT)
        .collect::<String>();
    json!({
        "summary": "child agent produced a long final report; read full_result_artifact for complete details",
        "thread_id": thread_id.as_str(),
        "original_chars": original_chars,
        "final_report_truncated_from_inline_result": true,
        "excerpt": excerpt,
        "full_result_artifact": artifact_ref,
    })
    .to_string()
}

fn append_compact_final_answer(
    child_session: &mut Session,
    child_session_ref: &SessionRef,
    final_text: String,
) -> Result<AgentFinalAnswerRef> {
    let assistant_message = ModelMessage::assistant_with_kind(
        Some(final_text.clone()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    let message_id = assistant_message.id.clone();
    child_session
        .append_assistant_message(assistant_message)
        .context("failed to append compact child agent final answer")?;
    Ok(AgentFinalAnswerRef {
        session_ref: child_session_ref.clone(),
        message_id,
        content_hash: hash_text(&final_text),
        char_count: final_text.chars().count(),
    })
}
