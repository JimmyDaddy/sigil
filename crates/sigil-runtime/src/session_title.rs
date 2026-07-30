use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sigil_kernel::{
    CompletionRequest, ModelMessage, Provider, ProviderChunk, SessionRef, UsageStats,
    safe_persistence_text,
};

use crate::{LocalSessionLifecycleService, current_unix_time_ms};

const TITLE_INPUT_MAX_BYTES: usize = 4 * 1024;
const TITLE_OUTPUT_MAX_BYTES: usize = 640;
const TITLE_PERSISTED_MAX_BYTES: usize = 100;
const TITLE_MAX_TOKENS: u32 = 64;
const TITLE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub(crate) struct GeneratedSessionTitle {
    pub(crate) title: String,
    pub(crate) usage: Option<UsageStats>,
}

pub(crate) struct SessionTitlePersistence {
    pub(crate) lifecycle: LocalSessionLifecycleService,
    pub(crate) session_ref: SessionRef,
    pub(crate) session_id: String,
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
}

/// Generates and durably projects one semantic title for an exact first-turn session.
///
/// The request is bounded, tool-free, and sent to the session's already selected provider/model.
/// Failure leaves the deterministic first-user-message title untouched.
pub async fn generate_and_persist_session_title(
    root_config: sigil_kernel::RootConfig,
    workspace_root: PathBuf,
    model_ref: sigil_kernel::ModelRef,
    session_log_path: PathBuf,
    session_id: String,
    prompt: String,
) -> Result<()> {
    let paths =
        crate::resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let session_ref = session_ref_for_title(&paths.session_log_dir, &session_log_path)?;
    let provider = crate::build_provider_for_model_ref_async(&root_config, &model_ref)
        .await
        .context("failed to build session title provider")?;
    let provider_name = provider.name().to_owned();
    let generated =
        generate_session_title(provider.as_ref(), &model_ref.model_id, &session_id, &prompt)
            .await?;
    let lifecycle = LocalSessionLifecycleService::new(
        paths.workspace_id,
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal);
    let persistence = SessionTitlePersistence {
        lifecycle,
        session_ref,
        session_id,
        provider_name,
        model_name: model_ref.model_id,
    };
    tokio::task::spawn_blocking(move || persist_generated_session_title(persistence, &generated))
        .await
        .context("session title persistence worker failed")??;
    Ok(())
}

pub(crate) async fn generate_session_title(
    provider: &dyn Provider,
    model_name: &str,
    session_id: &str,
    prompt: &str,
) -> Result<GeneratedSessionTitle> {
    let prompt = truncate_utf8(prompt.trim(), TITLE_INPUT_MAX_BYTES);
    if prompt.is_empty() {
        bail!("session title prompt is empty");
    }
    let request = CompletionRequest {
        provider_name: provider.name().to_owned(),
        model_name: model_name.to_owned(),
        messages: vec![
            ModelMessage::system(
                "Generate a concise semantic title for a coding-agent conversation. \
                 Use the same language as the user's request. Capture the concrete goal, \
                 component, or bug. Return only one plain-text title, without quotes, markdown, \
                 labels, explanation, or trailing punctuation. Keep it within 12 words.",
            ),
            ModelMessage::user(prompt),
        ],
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(TITLE_MAX_TOKENS),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: Some(format!("session-title:{session_id}")),
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let generated = tokio::time::timeout(TITLE_TIMEOUT, async {
        let mut stream = provider
            .stream(request)
            .await
            .context("session title provider stream failed to start")?;
        let mut output = String::new();
        let mut usage = None;
        while let Some(chunk) = stream.next().await {
            match chunk.context("session title provider stream failed")? {
                ProviderChunk::TextDelta(delta) => {
                    if output.len().saturating_add(delta.len()) > TITLE_OUTPUT_MAX_BYTES {
                        bail!("session title provider output exceeded byte limit");
                    }
                    output.push_str(&delta);
                }
                ProviderChunk::Usage(value) => usage = Some(value),
                ProviderChunk::ToolCallStart { .. }
                | ProviderChunk::ToolCallArgsDelta { .. }
                | ProviderChunk::ToolCallComplete(_) => {
                    bail!("session title provider unexpectedly requested a tool");
                }
                ProviderChunk::Done => break,
                _ => {}
            }
        }
        let title = clean_generated_title(&output)?;
        Ok(GeneratedSessionTitle { title, usage })
    })
    .await
    .context("session title provider request timed out")??;
    Ok(generated)
}

pub(crate) fn persist_generated_session_title(
    persistence: SessionTitlePersistence,
    generated: &GeneratedSessionTitle,
) -> Result<()> {
    let prompt_tokens = generated.usage.as_ref().map(|usage| usage.prompt_tokens);
    let completion_tokens = generated
        .usage
        .as_ref()
        .map(|usage| usage.completion_tokens);
    persistence.lifecycle.record_generated_title(
        &persistence.session_ref,
        &persistence.session_id,
        &generated.title,
        &persistence.provider_name,
        &persistence.model_name,
        prompt_tokens,
        completion_tokens,
        current_unix_time_ms(),
    )?;
    Ok(())
}

fn clean_generated_title(raw: &str) -> Result<String> {
    let mut cleaned = remove_tagged_block(raw, "think");
    cleaned = remove_tagged_block(&cleaned, "analysis");
    cleaned = cleaned
        .replace("<think>", "")
        .replace("</think>", "")
        .replace("<analysis>", "")
        .replace("</analysis>", "");
    let line = cleaned
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line
        .strip_prefix("Title:")
        .or_else(|| line.strip_prefix("title:"))
        .or_else(|| line.strip_prefix("标题："))
        .or_else(|| line.strip_prefix("标题:"))
        .unwrap_or(line)
        .trim();
    let line = line
        .trim_start_matches(['#', '*', '-', ' '])
        .trim_matches(['"', '\'', '`', '“', '”', '‘', '’'])
        .trim()
        .trim_end_matches(['.', '。'])
        .trim();
    let line = safe_persistence_text(line);
    let title = truncate_utf8(line.trim(), TITLE_PERSISTED_MAX_BYTES);
    if title.is_empty() {
        bail!("session title provider returned no usable title");
    }
    Ok(title)
}

fn remove_tagged_block(value: &str, tag: &str) -> String {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let mut output = value.to_owned();
    while let Some(start) = output.find(&opening) {
        let Some(relative_end) = output[start + opening.len()..].find(&closing) else {
            break;
        };
        let end = start + opening.len() + relative_end + closing.len();
        output.replace_range(start..end, "");
    }
    output
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

pub(crate) fn session_ref_for_title(
    session_dir: &Path,
    session_log_path: &Path,
) -> Result<SessionRef> {
    let file_name = session_log_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .context("session title source has no UTF-8 file name")?;
    let session_ref = SessionRef::new_relative(file_name)?;
    let canonical_session_dir = session_dir
        .canonicalize()
        .context("failed to canonicalize the session title directory")?;
    let canonical_session_log_path = session_log_path
        .canonicalize()
        .context("failed to canonicalize the session title source")?;
    if canonical_session_dir.join(session_ref.as_path()) != canonical_session_log_path {
        bail!("session title source is outside the configured session directory");
    }
    Ok(session_ref)
}
