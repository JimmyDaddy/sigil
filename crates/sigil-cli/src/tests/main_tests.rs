use std::pin::Pin;

use anyhow::{Result, anyhow};
use clap::Parser;
use futures::{Stream, stream};
use sigil_kernel::{
    EventHandler, ModelMessage, ProviderChunk, ToolAccess, ToolCall, ToolCategory, ToolErrorKind,
    ToolPreview, ToolPreviewCapability, ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
    UsageStats,
};

use super::{
    Cli, Commands, StdoutEventHandler, default_session_path, drain_provider_stream,
    resolve_workspace_root,
};

fn boxed_chunk_stream(
    chunks: Vec<Result<ProviderChunk>>,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    Box::pin(stream::iter(chunks))
}

#[test]
fn resolve_workspace_root_uses_config_parent() -> Result<()> {
    let config_path = std::env::temp_dir()
        .join("sigil-cli-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-cli-launch");
    let resolved = resolve_workspace_root(&config_path, &launch_cwd, "workspace/project");

    assert_eq!(
        resolved,
        config_path
            .parent()
            .expect("config path should have a parent")
            .join("workspace/project")
    );
    Ok(())
}

#[test]
fn resolve_workspace_root_uses_launch_cwd_for_default_dot() {
    let config_path = std::env::temp_dir()
        .join("sigil-cli-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-cli-launch");

    let resolved = resolve_workspace_root(&config_path, &launch_cwd, ".");

    assert_eq!(resolved, launch_cwd);
}

#[test]
fn default_session_path_uses_configured_log_dir_and_jsonl_suffix() {
    let workspace_root = std::env::temp_dir().join("sigil-cli-workspace");
    let session_path = default_session_path(&workspace_root, ".sigil/sessions");

    assert!(session_path.starts_with(workspace_root.join(".sigil/sessions")));
    assert_eq!(
        session_path.extension().and_then(|ext| ext.to_str()),
        Some("jsonl")
    );
    assert!(
        session_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-"))
    );
}

#[test]
fn cli_parses_run_command_with_explicit_config() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--config", "custom.toml", "run", "hello"])?;

    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("custom.toml"))
    );
    assert!(matches!(
        cli.command,
        Commands::Run { ref prompt } if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn cli_parses_hidden_prefix_command_options() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "prefix",
        "prompt",
        "--assistant-prefix",
        "seed",
        "--stop",
        "\n\n",
        "--model",
        "deepseek-test",
    ])?;

    assert!(matches!(
        cli.command,
        Commands::Prefix {
            ref prompt,
            ref assistant_prefix,
            ref stop,
            ref model,
        } if prompt == "prompt"
            && assistant_prefix == "seed"
            && stop == &vec!["\n\n".to_owned()]
            && model.as_deref() == Some("deepseek-test")
    ));
    Ok(())
}

#[test]
fn cli_parses_hidden_fim_command_options() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "fim",
        "prefix",
        "--suffix",
        "tail",
        "--stop",
        "<eof>",
        "--model",
        "deepseek-test",
        "--max-tokens",
        "64",
    ])?;

    assert!(matches!(
        cli.command,
        Commands::Fim {
            ref prompt,
            ref suffix,
            ref stop,
            ref model,
            max_tokens,
        } if prompt == "prefix"
            && suffix == "tail"
            && stop == &vec!["<eof>".to_owned()]
            && model.as_deref() == Some("deepseek-test")
            && max_tokens == Some(64)
    ));
    Ok(())
}

#[tokio::test]
async fn drain_provider_stream_handles_visible_and_ignored_chunks() -> Result<()> {
    let mut stream = boxed_chunk_stream(vec![
        Ok(ProviderChunk::TextDelta("hello".to_owned())),
        Ok(ProviderChunk::ReasoningDelta("plan".to_owned())),
        Ok(ProviderChunk::ReasoningSummaryDelta("summary".to_owned())),
        Ok(ProviderChunk::ToolCallStart {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: "{}".to_owned(),
        }),
        Ok(ProviderChunk::Usage(UsageStats {
            prompt_tokens: 3,
            completion_tokens: 5,
            system_fingerprint: Some("fp-test".to_owned()),
            ..UsageStats::default()
        })),
        Ok(ProviderChunk::Done),
    ]);

    drain_provider_stream(&mut stream).await
}

#[tokio::test]
async fn drain_provider_stream_propagates_chunk_errors() {
    let mut stream = boxed_chunk_stream(vec![Err(anyhow!("stream failed"))]);

    let error = drain_provider_stream(&mut stream)
        .await
        .expect_err("stream errors must be propagated");

    assert!(error.to_string().contains("stream failed"));
}

#[test]
fn stdout_event_handler_accepts_all_visible_event_variants() -> Result<()> {
    let mut handler = StdoutEventHandler;
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
    };
    let spec = ToolSpec {
        name: "read_file".to_owned(),
        description: "Read file".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        preview: ToolPreviewCapability::Optional,
    };

    handler.handle(sigil_kernel::RunEvent::TextDelta("text".to_owned()))?;
    handler.handle(sigil_kernel::RunEvent::ReasoningDelta(
        "reasoning".to_owned(),
    ))?;
    handler.handle(sigil_kernel::RunEvent::ToolCallStarted(call.clone()))?;
    handler.handle(sigil_kernel::RunEvent::ToolCallArgsDelta {
        id: call.id.clone(),
        delta: "{}".to_owned(),
    })?;
    handler.handle(sigil_kernel::RunEvent::ToolCallCompleted(call.clone()))?;
    handler.handle(sigil_kernel::RunEvent::ToolApprovalRequested {
        call: call.clone(),
        spec,
        subjects: vec![ToolSubject::path("README.md", "README.md")],
        preview: Some(ToolPreview {
            title: "Preview".to_owned(),
            summary: "read README".to_owned(),
            body: String::new(),
            changed_files: vec!["README.md".to_owned()],
            file_diffs: Vec::new(),
        }),
    })?;
    handler.handle(sigil_kernel::RunEvent::ToolApprovalResolved {
        call_id: call.id.clone(),
        approved: false,
        reason: Some("denied by test".to_owned()),
    })?;
    handler.handle(sigil_kernel::RunEvent::ToolResult(ToolResult::error(
        call.id,
        call.name,
        ToolErrorKind::Internal,
        "failed",
    )))?;
    handler.handle(sigil_kernel::RunEvent::Usage(UsageStats {
        prompt_tokens: 1,
        completion_tokens: 2,
        system_fingerprint: Some("fp-test".to_owned()),
        ..UsageStats::default()
    }))?;
    handler.handle(sigil_kernel::RunEvent::Notice("notice".to_owned()))?;
    handler.handle(sigil_kernel::RunEvent::AssistantMessage(
        ModelMessage::assistant(Some("assistant".to_owned()), Vec::new()),
    ))?;
    handler.handle(sigil_kernel::RunEvent::ToolResult(ToolResult::ok(
        "call-ok",
        "read_file",
        "ok",
        ToolResultMeta::default(),
    )))?;

    Ok(())
}
