use std::{
    collections::VecDeque,
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use clap::Parser;
use futures::{Stream, stream};
use sigil_kernel::{
    EventHandler, ModelMessage, ProviderChunk, RootConfig, RunEvent, ToolAccess, ToolCall,
    ToolCategory, ToolErrorKind, ToolPreview, ToolPreviewCapability, ToolResult, ToolResultMeta,
    ToolSpec, ToolSubject, UsageStats,
};
use sigil_runtime::doctor::{DoctorCheck, DoctorReport, DoctorStatus};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    BuildInfo, Cli, Commands, DEFAULT_HTTP_TOKEN_ENV, ServeOptions, ServeStartupPlan,
    StdoutEventHandler, build_serve_startup_plan, default_session_path, drain_provider_stream,
    render_cli_doctor_report, render_doctor_report, render_provider_chunk, render_run_event,
    render_serve_startup_plan, render_version, resolve_workspace_root, serve_command,
};

fn boxed_chunk_stream(
    chunks: Vec<Result<ProviderChunk>>,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    Box::pin(stream::iter(chunks))
}

#[test]
fn resolve_workspace_root_uses_config_parent() -> Result<()> {
    let config_path = std::env::temp_dir()
        .join("sigil-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-launch");
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
        .join("sigil-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-launch");

    let resolved = resolve_workspace_root(&config_path, &launch_cwd, ".");

    assert_eq!(resolved, launch_cwd);
}

#[test]
fn default_session_path_uses_configured_log_dir_and_jsonl_suffix() {
    let workspace_root = std::env::temp_dir().join("sigil-workspace");
    let session_dir = workspace_root.join("state/sessions");
    let session_path = default_session_path(&session_dir);

    assert!(session_path.starts_with(session_dir));
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
fn render_provider_chunk_formats_text_reasoning_usage_and_done() {
    let text = render_provider_chunk(ProviderChunk::TextDelta("hello".to_owned()));
    assert_eq!(text.stdout, "hello");
    assert!(!text.stop);

    let reasoning = render_provider_chunk(ProviderChunk::ReasoningSummaryDelta("plan".to_owned()));
    assert_eq!(reasoning.stderr, "[reasoning] plan");

    let usage = render_provider_chunk(ProviderChunk::Usage(UsageStats {
        prompt_tokens: 7,
        completion_tokens: 3,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: Some("fp-1".to_owned()),
    }));
    assert!(
        usage
            .stderr
            .contains("[usage] prompt=7 completion=3 fingerprint=fp-1")
    );

    let done = render_provider_chunk(ProviderChunk::Done);
    assert!(done.stop);
}

#[test]
fn render_run_event_formats_tool_events_usage_and_notice() {
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        args_json: "{\"path\":\"src/main.rs\"}".to_owned(),
    };
    let spec = ToolSpec {
        name: "write_file".to_owned(),
        description: "write".to_owned(),
        input_schema: Default::default(),
        category: ToolCategory::File,
        access: ToolAccess::Write,
        preview: ToolPreviewCapability::Required,
    };
    let approval = render_run_event(RunEvent::ToolApprovalRequested {
        call: call.clone(),
        spec,
        subjects: vec![ToolSubject::path("src/main.rs", "src/main.rs")],
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: vec![sigil_kernel::PathTrustZone::WorkspaceSource],
        confirmation: None,
        snapshot_required: false,
        preview: Some(ToolPreview {
            title: "Write".to_owned(),
            summary: "1 file changed".to_owned(),
            body: String::new(),
            changed_files: vec!["src/main.rs".to_owned()],
            file_diffs: Vec::new(),
        }),
    });
    assert!(
        approval
            .stderr
            .contains("[tool:approval] write_file (call-1) file write")
    );
    assert!(approval.stderr.contains("[tool:preview] 1 file changed"));

    let args = render_run_event(RunEvent::ToolCallArgsDelta {
        id: "call-1".to_owned(),
        delta: "{\"path\":\"src/main.rs\"}".to_owned(),
    });
    assert!(args.stderr.contains("[tool:args:call-1]"));

    let result = render_run_event(RunEvent::ToolResult(ToolResult::error(
        "call-1",
        "write_file",
        sigil_kernel::ToolErrorKind::PermissionDenied,
        "denied",
    )));
    assert!(
        result
            .stderr
            .contains("[tool:result] write_file error=true denied")
    );

    let usage = render_run_event(RunEvent::Usage(UsageStats {
        prompt_tokens: 9,
        completion_tokens: 4,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: Some("fp-2".to_owned()),
    }));
    assert!(
        usage
            .stderr
            .contains("[usage] prompt=9 completion=4 fingerprint=fp-2")
    );

    let notice = render_run_event(RunEvent::Notice("heads up".to_owned()));
    assert_eq!(notice.stderr, "[notice] heads up\n");
}

#[tokio::test]
async fn drain_provider_stream_and_stdout_event_handler_accept_supported_events() -> Result<()> {
    let mut provider_stream: std::pin::Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> =
        Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("hello".to_owned())),
            Ok(ProviderChunk::ReasoningDelta("think".to_owned())),
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 1,
                completion_tokens: 2,
                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: Some("fp".to_owned()),
            })),
            Ok(ProviderChunk::Done),
            Ok(ProviderChunk::TextDelta("ignored after done".to_owned())),
        ]));

    drain_provider_stream(&mut provider_stream).await?;

    let mut handler = StdoutEventHandler;
    handler.handle(RunEvent::TextDelta("hello".to_owned()))?;
    handler.handle(RunEvent::ReasoningDelta("think".to_owned()))?;
    handler.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    handler.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    handler.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approved: false,
        reason: Some("blocked".to_owned()),
    })?;
    handler.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "read_file",
        "ok",
        ToolResultMeta::default(),
    )))?;
    handler.handle(RunEvent::ContinuationState(
        sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "kind".to_owned(),
            message_id: None,
            opaque_blob: Default::default(),
        },
    ))?;
    handler.handle(RunEvent::AssistantMessage(
        sigil_kernel::ModelMessage::assistant(None, Vec::new()),
    ))?;
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
        Some(Commands::Fim {
            ref prompt,
            ref suffix,
            ref stop,
            ref model,
            max_tokens,
        }) if prompt == "prefix"
            && suffix == "tail"
            && stop == &vec!["<eof>".to_owned()]
            && model.as_deref() == Some("deepseek-test")
            && max_tokens == Some(64)
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
        Some(Commands::Prefix {
            ref prompt,
            ref assistant_prefix,
            ref stop,
            ref model,
        }) if prompt == "prompt"
            && assistant_prefix == "seed"
            && stop == &vec!["\n\n".to_owned()]
            && model.as_deref() == Some("deepseek-test")
    ));
    Ok(())
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
        Some(Commands::Run { ref prompt }) if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn cli_parses_doctor_command_with_explicit_config() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--config", "custom.toml", "doctor"])?;

    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("custom.toml"))
    );
    assert!(matches!(cli.command, Some(Commands::Doctor)));
    Ok(())
}

#[test]
fn cli_parses_serve_command_with_secure_defaults() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "serve"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Serve {
            host,
            port: 0,
            ref token_env,
            no_token: false,
        }) if host == IpAddr::V4(Ipv4Addr::LOCALHOST)
            && token_env == DEFAULT_HTTP_TOKEN_ENV
    ));
    Ok(())
}

#[test]
fn cli_parses_serve_command_overrides() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "serve",
        "--host",
        "0.0.0.0",
        "--port",
        "8765",
        "--token-env",
        "CUSTOM_SIGIL_HTTP_TOKEN",
        "--no-token",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Serve {
            host,
            port: 8765,
            ref token_env,
            no_token: true,
        }) if host == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            && token_env == "CUSTOM_SIGIL_HTTP_TOKEN"
    ));
    Ok(())
}

#[test]
fn cli_parses_version_flag_without_subcommand() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--version"])?;

    assert!(cli.show_version);
    assert!(cli.command.is_none());
    Ok(())
}

#[test]
fn cli_without_subcommand_defaults_to_tui() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil"])?;

    assert!(!cli.show_version);
    assert!(cli.command.is_none());
    Ok(())
}

#[test]
fn render_version_includes_build_metadata() {
    let rendered = render_version(BuildInfo {
        version: "1.2.3",
        git_hash: "abc123def456",
        target: "test-target",
        profile: "release",
    });

    assert!(rendered.contains("sigil 1.2.3"));
    assert!(rendered.contains("commit: abc123def456"));
    assert!(rendered.contains("target: test-target"));
    assert!(rendered.contains("profile: release"));
}

#[test]
fn build_info_current_uses_compile_time_metadata() {
    let info = BuildInfo::current();

    assert!(!info.version.is_empty());
    assert!(!info.git_hash.is_empty());
    assert!(!info.target.is_empty());
    assert!(!info.profile.is_empty());
}

#[test]
fn render_doctor_report_formats_checks_and_summary() {
    let report = DoctorReport {
        checks: vec![
            DoctorCheck {
                status: DoctorStatus::Ok,
                name: "config:load".to_owned(),
                message: "config parsed".to_owned(),
                remediation: None,
            },
            DoctorCheck {
                status: DoctorStatus::Warn,
                name: "terminal".to_owned(),
                message: "TERM is not set".to_owned(),
                remediation: Some("set TERM in the shell before launching the TUI".to_owned()),
            },
        ],
    };

    let rendered = render_doctor_report(&report);

    assert!(rendered.contains("Sigil doctor"));
    assert!(rendered.contains("[ok] config:load - config parsed"));
    assert!(rendered.contains("[warn] terminal - TERM is not set"));
    assert!(rendered.contains("fix: set TERM in the shell before launching the TUI"));
    assert!(rendered.contains("summary: warn"));
}

#[test]
fn serve_startup_plan_requires_token_by_default() {
    let error = build_serve_startup_plan(default_serve_options(), None)
        .expect_err("serve should require token by default");

    assert!(error.to_string().contains(DEFAULT_HTTP_TOKEN_ENV));
}

#[test]
fn serve_startup_plan_rejects_external_bind_without_token() {
    let options = ServeOptions {
        host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        no_token: true,
        ..default_serve_options()
    };

    let error = build_serve_startup_plan(options, None)
        .expect_err("external bind without token should be rejected");

    assert!(
        error
            .to_string()
            .contains("token auth is required for non-loopback bind addresses")
    );
}

#[test]
fn serve_startup_plan_renders_pending_routing_hint() -> Result<()> {
    let plan = build_serve_startup_plan(default_serve_options(), Some("secret-token"))?;
    let rendered = render_serve_startup_plan(&plan);

    assert_eq!(plan.bind_addr, SocketAddr::from(([127, 0, 0, 1], 0)));
    assert!(plan.token_required);
    assert_eq!(plan.token_env.as_deref(), Some(DEFAULT_HTTP_TOKEN_ENV));
    assert!(rendered.contains("Sigil HTTP/SSE adapter"));
    assert!(rendered.contains("bind: 127.0.0.1:0"));
    assert!(rendered.contains("auth: bearer token from SIGIL_HTTP_TOKEN"));
    assert!(rendered.contains("HTTP routing is not implemented yet"));
    serve_command(default_serve_options(), Some("secret-token"))?;
    Ok(())
}

#[test]
fn serve_startup_plan_renders_disabled_auth_and_token_env_fallback() -> Result<()> {
    let disabled = build_serve_startup_plan(
        ServeOptions {
            no_token: true,
            ..default_serve_options()
        },
        None,
    )?;
    let fallback = ServeStartupPlan {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        token_required: true,
        token_env: None,
    };

    assert!(!disabled.token_required);
    assert_eq!(disabled.token_env, None);
    assert!(render_serve_startup_plan(&disabled).contains("auth: disabled"));
    assert!(
        render_serve_startup_plan(&fallback).contains("auth: bearer token from SIGIL_HTTP_TOKEN")
    );
    Ok(())
}

#[test]
fn doctor_command_renders_report_for_missing_config() -> Result<()> {
    let workspace = create_test_workspace("doctor-command");

    super::doctor_command(&workspace.join("missing.toml"), &workspace)
}

#[test]
fn doctor_command_report_includes_appearance_warnings() -> Result<()> {
    let workspace = create_test_workspace("doctor-appearance");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, "https://example.com")?;
    let mut config = fs::read_to_string(&config_path)?;
    config.push_str(
        r##"
[appearance.colors]
surface_base = "#101010"
text_primary = "#101010"
"##,
    );
    fs::write(&config_path, config)?;

    let output = render_cli_doctor_report(&config_path, &workspace);

    assert!(output.contains("[warn] appearance:contrast:text-base"));
    assert!(output.contains("text_primary on surface_base"));
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
        operation: sigil_kernel::ToolOperation::Read,
        risk: sigil_kernel::PermissionRisk::Low,
        subject_zones: vec![sigil_kernel::PathTrustZone::WorkspaceSource],
        confirmation: None,
        snapshot_required: false,
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

#[tokio::test]
async fn prefix_command_streams_against_configured_provider() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"prefixed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = create_test_workspace("prefix-command");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, &server)?;

    super::prefix_command(
        &config_path,
        &workspace,
        "write code".to_owned(),
        "```rust\n".to_owned(),
        vec!["```".to_owned()],
        Some("deepseek-v4-flash".to_owned()),
    )
    .await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded prefix request");
    assert!(raw_request.contains("POST /chat/completions"));
    assert!(raw_request.contains("\"prefix\":true"));
    assert!(raw_request.contains("```rust"));
    assert!(raw_request.contains("\"user_id\":\"workspace-"));
    assert!(!raw_request.contains("local-user"));
    Ok(())
}

#[tokio::test]
async fn fim_command_streams_against_configured_provider() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"text\":\"middle\",\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"prompt_cache_hit_tokens\":2,\"prompt_cache_miss_tokens\":5},\"system_fingerprint\":\"fp-fim\"}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = create_test_workspace("fim-command");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, &server)?;

    super::fim_command(
        &config_path,
        "fn main() {\n".to_owned(),
        "\n}\n".to_owned(),
        vec!["STOP".to_owned()],
        Some("deepseek-v4-pro".to_owned()),
        Some(32),
    )
    .await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded fim request");
    assert!(raw_request.contains("POST /completions"));
    assert!(raw_request.contains("\"suffix\":\"\\n}\\n\""));
    assert!(raw_request.contains("\"max_tokens\":32"));
    Ok(())
}

#[tokio::test]
async fn run_command_creates_session_log_in_user_state() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello from agent\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = create_test_workspace("run-command");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, &server)?;

    super::run_command(&config_path, &workspace, "Say hi".to_owned()).await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded run request");
    assert!(raw_request.contains("POST /chat/completions"));
    assert!(raw_request.contains("\"Say hi\""));

    let root_config = RootConfig::load(&config_path)?;
    let paths =
        sigil_runtime::resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace);
    let session_dir = paths.session_log_dir;
    let entries = fs::read_dir(&session_dir)?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(
        entries.len(),
        1,
        "run_command should create one session log"
    );
    let session_path = entries[0].path();
    assert_eq!(
        session_path.extension().and_then(|ext| ext.to_str()),
        Some("jsonl")
    );
    let session_contents = fs::read_to_string(session_path)?;
    assert!(session_contents.contains("Say hi"));
    assert!(session_contents.contains("hello from agent"));
    Ok(())
}

fn create_test_workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("sigil-tests-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).expect("test workspace should create");
    path
}

fn default_serve_options() -> ServeOptions {
    ServeOptions {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned(),
        no_token: false,
    }
}

fn write_test_config(path: &std::path::Path, base_url: &str) -> Result<()> {
    let workspace = path
        .parent()
        .ok_or_else(|| anyhow!("test config path should have a parent"))?;
    let state_root = workspace.join("state");
    let cache_root = workspace.join("cache");
    let config = format!(
        r#"[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[providers.deepseek]
base_url = "{base_url}"
beta_base_url = "{base_url}"
anthropic_base_url = "{base_url}"
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
api_key = "test-key"
strict_tools_mode = "auto"
request_timeout_secs = 5
"#,
        state_root.display(),
        cache_root.display()
    );
    fs::write(path, config)?;
    Ok(())
}

async fn spawn_recording_server(
    requests: Arc<Mutex<VecDeque<String>>>,
    responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let requests = Arc::clone(&requests);
            let responses = Arc::clone(&responses);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let bytes = socket.read(&mut buffer).await.unwrap_or(0);
                requests
                    .lock()
                    .expect("requests poisoned")
                    .push_back(String::from_utf8_lossy(&buffer[..bytes]).to_string());
                let response = responses
                    .lock()
                    .expect("responses poisoned")
                    .pop_front()
                    .unwrap_or_else(|| http_response(500, "text/plain", "missing fixture"));
                let _ = socket.write_all(&response).await;
            });
        }
    });
    Ok(format!("http://{}", address))
}

fn http_response(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}
