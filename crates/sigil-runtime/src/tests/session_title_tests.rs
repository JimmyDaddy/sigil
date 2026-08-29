use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{CompletionRequest, Provider, ProviderCapabilities, ProviderChunk, UsageStats};

use crate::session_title::{generate_session_title, session_ref_for_title};

#[derive(Clone)]
struct TitleProvider {
    request: Arc<Mutex<Option<CompletionRequest>>>,
    chunks: Vec<ProviderChunk>,
}

#[async_trait]
impl Provider for TitleProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        crate::provider_capabilities_for_name("deepseek").expect("known provider")
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        *self.request.lock().expect("request lock") = Some(request);
        Ok(Box::pin(stream::iter(
            self.chunks.clone().into_iter().map(Ok),
        )))
    }
}

#[tokio::test]
async fn semantic_title_request_is_bounded_tool_free_and_cleans_reasoning() -> Result<()> {
    let request = Arc::new(Mutex::new(None));
    let provider = TitleProvider {
        request: Arc::clone(&request),
        chunks: vec![
            ProviderChunk::ReasoningDelta("hidden".to_owned()),
            ProviderChunk::TextDelta("<think>ignore this</think>\n标题：".to_owned()),
            ProviderChunk::TextDelta("修复桌面会话标题同步。".to_owned()),
            ProviderChunk::Usage(UsageStats {
                prompt_tokens: 123,
                completion_tokens: 9,
                ..UsageStats::default()
            }),
            ProviderChunk::Done,
        ],
    };

    let generated = generate_session_title(
        &provider,
        "deepseek-v4-flash",
        "session-test",
        "会话名称已经修改，但 Desktop 页面标题没有同步，请修复。",
    )
    .await?;

    assert_eq!(generated.title, "修复桌面会话标题同步");
    assert_eq!(
        generated.usage.as_ref().map(|usage| usage.prompt_tokens),
        Some(123)
    );
    let request = request
        .lock()
        .expect("request lock")
        .clone()
        .expect("captured request");
    assert!(request.tools.is_empty());
    assert!(request.hosted_tools.is_empty());
    assert_eq!(request.max_tokens, Some(64));
    assert_eq!(request.messages.len(), 2);
    assert!(!request.store);
    assert!(request.deterministic_materialization);
    Ok(())
}

#[tokio::test]
async fn semantic_title_rejects_tool_calls() {
    let provider = TitleProvider {
        request: Arc::new(Mutex::new(None)),
        chunks: vec![
            ProviderChunk::ToolCallStart {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
            },
            ProviderChunk::Done,
        ],
    };

    let error = generate_session_title(
        &provider,
        "deepseek-v4-flash",
        "session-test",
        "Inspect a bug",
    )
    .await
    .expect_err("tool request must fail");
    assert!(error.to_string().contains("unexpectedly requested a tool"));
}

#[test]
fn title_session_ref_accepts_a_canonical_source_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("session-title.jsonl");
    std::fs::write(&session_path, b"")?;

    let managed_root = temp.path().join("managed/session-log");
    let session_ref =
        session_ref_for_title(&session_dir, &managed_root, &session_path.canonicalize()?)?;

    assert_eq!(
        session_ref.as_path(),
        std::path::Path::new("session-title.jsonl")
    );
    Ok(())
}

#[test]
fn title_session_ref_rejects_a_source_outside_the_session_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&session_dir)?;
    let outside_path = temp.path().join("outside.jsonl");
    std::fs::write(&outside_path, b"")?;

    let error = session_ref_for_title(
        &session_dir,
        &temp.path().join("managed/session-log"),
        &outside_path,
    )
    .expect_err("an outside source must not become a session reference");

    assert!(
        error
            .to_string()
            .contains("outside the configured session directory")
    );
    Ok(())
}

#[test]
fn title_session_ref_maps_a_managed_session_source_to_its_logical_key() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_dir = temp.path().join("sessions");
    let managed_root = temp.path().join("managed/session-log");
    let managed_dir = managed_root.join("session-managed");
    std::fs::create_dir_all(&session_dir)?;
    std::fs::create_dir_all(&managed_dir)?;
    let managed_path = managed_dir.join("records.jsonl");
    std::fs::write(&managed_path, b"")?;

    let session_ref = session_ref_for_title(&session_dir, &managed_root, &managed_path)?;

    assert_eq!(
        session_ref.as_path(),
        std::path::Path::new("session-managed.jsonl")
    );
    Ok(())
}
