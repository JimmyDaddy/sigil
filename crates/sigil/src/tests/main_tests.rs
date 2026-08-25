use std::{
    collections::VecDeque,
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow, bail};
use clap::{CommandFactory, Parser};
use futures::{Stream, stream};
use sigil_kernel::{
    ApprovalMode, EventHandler, JsonlSessionStore, ModelMessage, PermissionConfirmation,
    PermissionDecision, ProviderChunk, PublicRunEventKind, PublicTaskPhase, RootConfig, RunEvent,
    SessionConfig, StorageConfig, ToolAccess, ToolCall, ToolCategory, ToolErrorKind,
    ToolExecutionId, ToolPreview, ToolPreviewCapability, ToolProgressEvent, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, UsageStats, WorkspaceTrust, resolve_workspace_root,
    workspace_trust_from_entries,
};
use sigil_runtime::SessionCatalogProjectionError;
use sigil_runtime::application_run::{application_run_input, default_application_session_path};
use sigil_runtime::doctor::{DoctorCheck, DoctorReport, DoctorStatus};
use sigil_runtime::machine_protocol::MachineExitCode;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    BuildInfo, Cli, Commands, DEFAULT_HTTP_TOKEN_ENV, DoctorOutput, HTTP_SERVER_STATE_DIR,
    RunOutput, ServeOptions, ServeOwnerChannelWatcher, ServeStartupOutput, ServeStartupPlan,
    StdoutEventHandler, build_serve_startup_plan, build_session_catalog_service,
    cli_application_run_request, drain_provider_stream, interactive_tui_requested,
    load_serve_root_config, render_cli_doctor_report, render_doctor_report, render_provider_chunk,
    render_run_event, render_serve_startup_json, render_serve_startup_plan, render_update_apply,
    render_update_check, render_version, run_machine_command_with_cancellation,
    run_machine_command_with_writer, session_catalog_projection_error_code,
};

fn boxed_chunk_stream(
    chunks: Vec<Result<ProviderChunk>>,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    Box::pin(stream::iter(chunks))
}

fn test_approval_identity(call_id: &str) -> sigil_kernel::ApprovalRequestIdentityV2 {
    sigil_kernel::ApprovalRequestIdentityV2 {
        session_id: "session-cli-test".to_owned(),
        run_id: "run-cli-test".to_owned(),
        call_id: call_id.to_owned(),
        approval_request_id: format!("approval-{call_id}"),
        plan_hash: "plan-cli-test".to_owned(),
        policy_version: "policy-cli-test".to_owned(),
        execution_binding_hash: "binding-cli-test".to_owned(),
        expires_at_ms: u64::MAX,
    }
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
fn serve_session_catalog_service_uses_resolved_global_projection_path() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let paths = sigil_runtime::resolve_sigil_paths(
        &StorageConfig::default(),
        &SessionConfig::default(),
        workspace.path(),
    );

    let service = build_session_catalog_service(&paths);

    assert_eq!(service.database_path(), paths.session_catalog_db);
    assert_eq!(HTTP_SERVER_STATE_DIR, "http-server-v4");
    assert_eq!(
        paths.session_catalog_db.parent(),
        Some(paths.projections_root.as_path())
    );
    Ok(())
}

#[test]
fn session_catalog_warmup_errors_use_stable_path_free_codes() {
    let private_path = "/Users/private/workspace/session-catalog.sqlite3";
    let error = SessionCatalogProjectionError::UnsafePath {
        message: private_path.to_owned(),
    };

    let code = session_catalog_projection_error_code(&error);

    assert_eq!(code, "unsafe_path");
    assert!(!code.contains(private_path));
}

#[test]
fn serve_root_config_uses_setup_shell_for_an_absent_config() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("missing-sigil.toml");

    let config = load_serve_root_config(&config_path);

    assert_eq!(config.workspace.root, ".");
    assert!(config.agent.runtime_provider.is_empty());
    assert!(config.agent.connection.is_none());
    assert!(config.agent.model.is_empty());
    assert!(config.connections.is_empty());
    assert!(!config_path.exists());
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
    let session_path = default_application_session_path(&session_dir);

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
fn fresh_cli_session_projects_unknown_workspace_trust() -> Result<()> {
    let workspace = unique_temp_workspace("sigil-cli-workspace-trust")?;
    let store = JsonlSessionStore::new(workspace.join("session.jsonl"))?;

    let session = sigil_kernel::Session::load_from_store("deepseek", "deepseek-test", store)?;
    let trust = workspace_trust_from_entries(session.entries(), &workspace)?;

    assert_eq!(trust, WorkspaceTrust::Unknown);
    Ok(())
}

#[test]
fn run_input_with_repo_context_attaches_repository_candidates() -> Result<()> {
    let workspace = unique_temp_workspace("sigil-cli-context")?;
    fs::write(
        workspace.join("README.md"),
        "Sigil is a Rust coding agent with Desktop and TUI experiences.",
    )?;

    let input = application_run_input(&workspace, "summarize README.md".to_owned());

    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "repo-file:README.md"
            && matches!(item.source, sigil_kernel::ContextSource::RepositoryFile)
    }));
    fs::remove_dir_all(workspace)?;
    Ok(())
}

fn unique_temp_workspace(prefix: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path)?;
    Ok(path)
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
        cache_usage: None,
        pricing_snapshot: None,
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
        network_effect: None,
        preview: ToolPreviewCapability::Required,
    };
    let approval = render_run_event(RunEvent::ToolApprovalRequested {
        approval_identity: test_approval_identity(&call.id),
        effects: std::collections::BTreeSet::from([sigil_kernel::ToolPermissionEffect::FileWrite]),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(
            sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
                code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
            },
        ),
        call: call.clone(),
        spec,
        subjects: vec![ToolSubject::path("src/main.rs", "src/main.rs")],
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: vec![sigil_kernel::PathTrustZone::WorkspaceSource],
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
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
    assert!(approval.stderr.contains("network=none risk=medium"));
    assert!(
        approval
            .stderr
            .contains("policy=local:ask network:allow source:allow final:ask")
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

    let progress = render_run_event(RunEvent::ToolProgress(ToolProgressEvent {
        execution_id: ToolExecutionId::new("execution-1").expect("valid tool execution id"),
        call_id: "call-1".to_owned(),
        tool_name: "terminal_start".to_owned(),
        sequence: 1,
        status: "running".to_owned(),
        message: Some("running workspace check".to_owned()),
        output_preview: Some("Compiling sigil".to_owned()),
        output_log_ref: None,
        total_bytes: Some(128),
        updated_at_ms: Some(10),
        details: serde_json::json!({"task_id": "terminal-1"}),
    }));
    assert!(
        progress
            .stderr
            .contains("[tool:progress] terminal_start (call-1) running")
    );
    assert!(
        progress
            .stderr
            .contains("[tool:progress:message] running workspace check")
    );
    assert!(
        progress
            .stderr
            .contains("[tool:progress:preview] Compiling sigil")
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
        cache_usage: None,
        pricing_snapshot: None,
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
                cache_usage: None,
                pricing_snapshot: None,
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
        approval_request_id: "approval-call-1".to_owned(),
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
fn cli_help_hides_provider_debug_commands() {
    let help = Cli::command().render_long_help().to_string();

    assert!(help.contains("run"));
    assert!(help.contains("resume"));
    assert!(help.contains("doctor"));
    assert!(help.contains("intent"));
    assert!(help.contains("mcp"));
    assert!(help.contains("serve"));
    assert!(!help.contains("prefix"));
    assert!(!help.contains("fim"));
    assert!(!help.contains("model-eval"));
}

#[test]
fn text_cli_ignores_structured_task_state_events() {
    let events = [
        PublicRunEventKind::TaskRoutingChanged {
            handoff_id: "handoff-1".to_owned(),
            status: "accepted".to_owned(),
            task_id: Some("task-1".to_owned()),
        },
        PublicRunEventKind::TaskPhaseChanged {
            task_id: Some("task-1".to_owned()),
            phase: PublicTaskPhase::Planning,
            status: "running".to_owned(),
        },
        PublicRunEventKind::TaskPlanUpdated {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            status: "accepted".to_owned(),
            steps: Vec::new(),
        },
        PublicRunEventKind::TaskBatchChanged {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            batch_id: "batch-1".to_owned(),
            active: 1,
            completed: 0,
            failed: 0,
        },
        PublicRunEventKind::TaskStepChanged {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            step_id: "step-1".to_owned(),
            attempt_id: Some("attempt-1".to_owned()),
            status: "running".to_owned(),
        },
        PublicRunEventKind::IntegrationLaneChanged {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            plan_id: "plan-1".to_owned(),
            lane_id: "lane-1".to_owned(),
            status: "pending".to_owned(),
            conflicts: Vec::new(),
        },
    ];

    for event in events {
        assert_eq!(
            super::render_public_run_event(event),
            super::RenderedOutput::default()
        );
    }
}

#[test]
fn text_cli_renders_route_recovery_without_private_binding_material() {
    let output = super::render_public_run_event(PublicRunEventKind::RouteRecoveryRequired {
        code: sigil_kernel::PublicRouteRecoveryCode::SessionRouteConfirmationRequired,
        actions: vec![sigil_kernel::PublicRouteRecoveryAction::ConfirmCurrentRoute],
        recovery_binding: "opaque-private-binding".to_owned(),
        retryable: true,
    });

    assert_eq!(
        output.stderr,
        "[recovery] the saved session route needs explicit confirmation\n"
    );
    assert!(!output.stderr.contains("opaque-private-binding"));
}

#[test]
fn text_cli_renders_partial_output_discard_without_content_or_attempt_identity() {
    let output =
        super::render_public_run_event(PublicRunEventKind::ProviderTurnPartialOutputDiscarded {
            output: sigil_kernel::PublicProviderTurnPartialOutputDiscardedViewV1 {
                text_discarded: true,
                reasoning_discarded: true,
                tool_request_discarded: false,
            },
        });

    assert_eq!(
        output.stderr,
        "[provider:recovery] discarded partial text, reasoning; replacement output will follow\n"
    );
}

#[test]
fn text_cli_renders_recoverable_run_states_without_downgrading_them_to_failed() {
    let cases = [
        (
            PublicRunEventKind::RunBlocked {
                reason: "waiting for workspace reconciliation".to_owned(),
            },
            "[run:blocked] waiting for workspace reconciliation\n",
        ),
        (
            PublicRunEventKind::RunPaused {
                reason: "provider retry budget is exhausted".to_owned(),
            },
            "[run:paused] provider retry budget is exhausted\n",
        ),
        (
            PublicRunEventKind::RunInterrupted {
                reason: "process stopped before the provider replied".to_owned(),
            },
            "[run:interrupted] process stopped before the provider replied\n",
        ),
    ];

    for (event, expected) in cases {
        assert_eq!(super::render_public_run_event(event).stderr, expected);
    }
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
        Some(Commands::Run {
            ref prompt,
            output: RunOutput::Text,
            ..
        }) if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn cli_requires_and_preserves_compound_model_route() -> Result<()> {
    assert!(
        Cli::try_parse_from(["sigil", "run", "hello", "--connection", "openai-personal"]).is_err()
    );
    assert!(Cli::try_parse_from(["sigil", "run", "hello", "--model", "gpt-4.1"]).is_err());
    let cli = Cli::try_parse_from([
        "sigil",
        "run",
        "hello",
        "--connection",
        "openai-personal",
        "--model",
        "gpt-4.1",
    ])?;
    assert!(matches!(
        cli.command,
        Some(Commands::Run {
            connection: Some(ref connection),
            model: Some(ref model),
            ..
        }) if connection == "openai-personal" && model == "gpt-4.1"
    ));

    let request = cli_application_run_request(
        Path::new("sigil.toml"),
        Path::new("."),
        "hello".to_owned(),
        Some("openai-personal"),
        Some("gpt-4.1"),
        None,
        None,
    )
    .expect("compound route should become one application request");
    assert_eq!(
        request
            .model_connection_id
            .as_ref()
            .map(sigil_kernel::ConnectionId::as_str),
        Some("openai-personal")
    );
    assert_eq!(request.model_name.as_deref(), Some("gpt-4.1"));
    Ok(())
}

#[test]
fn cli_parses_run_machine_output_modes() -> Result<()> {
    for (label, expected) in [("json", RunOutput::Json), ("jsonl", RunOutput::Jsonl)] {
        let cli = Cli::try_parse_from(["sigil", "run", "hello", "--output", label])?;
        assert!(matches!(
            cli.command,
            Some(Commands::Run { output, .. }) if output == expected
        ));
    }
    assert!(Cli::try_parse_from(["sigil", "run", "hello", "--output", "xml"]).is_err());
    Ok(())
}

#[test]
fn cli_parses_resume_command_with_explicit_config_and_session_id() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--config", "custom.toml", "resume", "session-123"])?;

    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("custom.toml"))
    );
    assert!(matches!(
        cli.command,
        Some(Commands::Resume { ref session }) if session.as_deref() == Some("session-123")
    ));
    Ok(())
}

#[test]
fn cli_parses_resume_command_without_selector_as_latest() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "resume"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Resume { ref session }) if session.is_none()
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
    assert!(matches!(
        cli.command,
        Some(Commands::Doctor {
            output: DoctorOutput::Text,
        })
    ));
    Ok(())
}

#[test]
fn cli_parses_doctor_json_output() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "doctor", "--output", "json"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Doctor {
            output: DoctorOutput::Json,
        })
    ));
    Ok(())
}

#[test]
fn cli_parses_mcp_management_commands() -> Result<()> {
    let add = Cli::try_parse_from([
        "sigil",
        "--config",
        "custom.toml",
        "mcp",
        "add",
        "filesystem",
        "--inherit-env",
        "MCP_TOKEN",
        "--",
        "node",
        "server.js",
    ])?;
    assert!(matches!(
        add.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::Add {
                ref name,
                url: None,
                ref inherit_env,
                ref command,
                ..
            },
        }) if name == "filesystem"
            && inherit_env.iter().map(String::as_str).eq(["MCP_TOKEN"])
            && command.iter().map(String::as_str).eq(["node", "server.js"])
    ));

    let remote = Cli::try_parse_from([
        "sigil",
        "mcp",
        "add",
        "search",
        "--url",
        "https://mcp.example.com/mcp",
        "--bearer-token-env-var",
        "SEARCH_TOKEN",
    ])?;
    assert!(matches!(
        remote.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::Add {
                ref name,
                ref url,
                ref bearer_token_env_var,
                ref command,
                ..
            },
        }) if name == "search"
            && url.as_deref() == Some("https://mcp.example.com/mcp")
            && bearer_token_env_var.as_deref() == Some("SEARCH_TOKEN")
            && command.is_empty()
    ));

    let list = Cli::try_parse_from(["sigil", "mcp", "list", "--json"])?;
    assert!(matches!(
        list.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::List { json: true },
        })
    ));
    let remove = Cli::try_parse_from(["sigil", "mcp", "remove", "search"])?;
    assert!(matches!(
        remove.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::Remove { ref name },
        }) if name == "search"
    ));
    Ok(())
}

#[test]
fn cli_parses_explicit_tokenizer_install_command() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "tokenizer", "install", "deepseek-v4-flash"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Tokenizer {
            command: super::TokenizerCommand::Install { ref profile },
        }) if profile == "deepseek-v4-flash"
    ));
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
            startup_output: ServeStartupOutput::Text,
            shutdown_on_stdin_close: false,
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
        "--startup-output",
        "json",
        "--shutdown-on-stdin-close",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Serve {
            host,
            port: 8765,
            ref token_env,
            no_token: true,
            startup_output: ServeStartupOutput::Json,
            shutdown_on_stdin_close: true,
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
    assert!(interactive_tui_requested(&cli));
    Ok(())
}

#[test]
fn only_interactive_tui_commands_suppress_terminal_tracing() -> Result<()> {
    let version = Cli::try_parse_from(["sigil", "--version"])?;
    let resume = Cli::try_parse_from(["sigil", "resume", "session-123"])?;
    let doctor = Cli::try_parse_from(["sigil", "doctor"])?;

    assert!(!interactive_tui_requested(&version));
    assert!(interactive_tui_requested(&resume));
    assert!(!interactive_tui_requested(&doctor));
    Ok(())
}

#[test]
fn render_version_includes_build_metadata() {
    let rendered = render_version(BuildInfo {
        version: "1.2.3",
        git_hash: "abc123def456",
        target: "test-target",
        profile: "release",
        distribution: "github-release",
    });

    assert!(rendered.contains("sigil 1.2.3"));
    assert!(rendered.contains("commit: abc123def456"));
    assert!(rendered.contains("target: test-target"));
    assert!(rendered.contains("profile: release"));
    assert!(rendered.contains("distribution: github-release"));
}

#[test]
fn build_info_current_uses_compile_time_metadata() {
    let info = BuildInfo::current();
    let update = info.update_metadata();

    assert!(!info.version.is_empty());
    assert!(!info.git_hash.is_empty());
    assert!(!info.target.is_empty());
    assert!(!info.profile.is_empty());
    assert!(!info.distribution.is_empty());
    assert_eq!(update.version, info.version);
    assert_eq!(update.distribution, info.distribution);
}

#[test]
fn build_info_projects_exactly_into_tui_support_metadata() {
    let support: sigil_runtime::support::SupportBuildInfo = BuildInfo {
        version: "1.2.3",
        git_hash: "abc123",
        target: "test-target",
        profile: "release",
        distribution: "github-release",
    }
    .into();

    assert_eq!(support.version, "1.2.3");
    assert_eq!(support.commit, "abc123");
    assert_eq!(support.target, "test-target");
    assert_eq!(support.profile, "release");
}

#[test]
fn cli_parses_update_check_and_explicit_apply() -> Result<()> {
    let check = Cli::try_parse_from([
        "sigil",
        "update",
        "check",
        "--channel",
        "beta",
        "--refresh",
        "--output",
        "json",
    ])?;
    assert!(matches!(
        check.command,
        Some(Commands::Update {
            command: super::UpdateCommand::Check {
                channel: super::UpdateChannelArg::Beta,
                refresh: true,
                output: super::UpdateOutput::Json,
            }
        })
    ));

    let apply = Cli::try_parse_from(["sigil", "update", "apply", "--yes"])?;
    assert!(matches!(
        apply.command,
        Some(Commands::Update {
            command: super::UpdateCommand::Apply { yes: true, .. }
        })
    ));
    Ok(())
}

#[test]
fn update_text_output_explains_managed_and_installed_results() {
    let check = sigil_updater::UpdateCheckOutcome {
        current_version: "1.0.0-beta.1".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        channel: sigil_updater::UpdateChannel::Current,
        install_source: sigil_updater::InstallSource::Npm,
        checked_at_unix_seconds: 1,
        cached: false,
        candidate: Some(sigil_updater::UpdateCandidate {
            version: "1.0.0-beta.2".to_owned(),
            tag_name: "v1.0.0-beta.2".to_owned(),
            prerelease: true,
            asset_name: None,
            security: sigil_updater::ReleaseSecurity {
                immutable: true,
                sha256: Some("a".repeat(64)),
                eligible_for_apply: true,
                blocking_reason: None,
            },
        }),
        managed_update_command: Some("npm install -g @sigil-ai/sigil@beta".to_owned()),
    };

    assert!(render_update_check(&check).contains("npm install -g @sigil-ai/sigil@beta"));
    assert!(
        render_update_apply(&sigil_updater::UpdateApplyOutcome::Installed {
            version: "1.0.0-beta.2".to_owned(),
        })
        .contains("Restart Sigil")
    );
}

#[test]
fn update_text_output_does_not_offer_in_place_apply_to_source_builds() {
    let check = sigil_updater::UpdateCheckOutcome {
        current_version: "1.0.0".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        channel: sigil_updater::UpdateChannel::Stable,
        install_source: sigil_updater::InstallSource::Source,
        checked_at_unix_seconds: 1,
        cached: false,
        candidate: Some(sigil_updater::UpdateCandidate {
            version: "1.0.1".to_owned(),
            tag_name: "v1.0.1".to_owned(),
            prerelease: false,
            asset_name: Some("sigil-1.0.1-aarch64-apple-darwin.tar.gz".to_owned()),
            security: sigil_updater::ReleaseSecurity {
                immutable: true,
                sha256: Some("a".repeat(64)),
                eligible_for_apply: true,
                blocking_reason: None,
            },
        }),
        managed_update_command: None,
    };

    let rendered = render_update_check(&check);
    assert!(!rendered.contains("sigil update apply --yes"));
    assert!(rendered.contains("install blocked"));
}

#[test]
fn render_doctor_report_formats_checks_and_summary() {
    let report = DoctorReport {
        cutover: Default::default(),
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
    assert!(rendered.contains("cutover: epoch=legacy authority=legacy blockers=0"));
    assert!(rendered.contains("[ok] config:load - config parsed"));
    assert!(rendered.contains("[warn] terminal - TERM is not set"));
    assert!(rendered.contains("fix: set TERM in the shell before launching the TUI"));
    assert!(rendered.contains("summary: warn"));
}

#[test]
fn r71_headless_permission_fixture_matches_kernel_blockers() {
    let mut confirmation = PermissionDecision::new(
        ApprovalMode::Allow,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("notes.txt", "notes.txt")],
        false,
    );
    confirmation.confirmation = Some(PermissionConfirmation::TypePhrase {
        phrase: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
    });
    assert_eq!(
        confirmation.headless_blocker(),
        Some(sigil_kernel::HeadlessPermissionBlockerV1::ConfirmationRequired)
    );

    confirmation.confirmation = None;
    assert_eq!(confirmation.headless_blocker(), None);
    confirmation.mode = ApprovalMode::Ask;
    assert_eq!(
        confirmation.headless_blocker(),
        Some(sigil_kernel::HeadlessPermissionBlockerV1::ApprovalRequired)
    );
}

#[test]
fn serve_startup_plan_requires_token_by_default() {
    let error = build_serve_startup_plan(default_serve_options(), None)
        .expect_err("serve should require token by default");

    assert!(error.to_string().contains(DEFAULT_HTTP_TOKEN_ENV));
}

#[test]
fn serve_startup_plan_rejects_every_external_bind() {
    let options = ServeOptions {
        host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ..default_serve_options()
    };

    let error = build_serve_startup_plan(options, Some("secret-token"))
        .expect_err("V1 external bind should be rejected");

    assert!(
        error
            .to_string()
            .contains("only accepts loopback bind addresses")
    );
}

#[test]
fn serve_startup_plan_renders_listener_status_without_token_value() -> Result<()> {
    let plan = build_serve_startup_plan(default_serve_options(), Some("secret-token"))?;
    let rendered = render_serve_startup_plan(&plan);

    assert_eq!(plan.bind_addr, SocketAddr::from(([127, 0, 0, 1], 0)));
    assert!(plan.token_required);
    assert_eq!(plan.token_env.as_deref(), Some(DEFAULT_HTTP_TOKEN_ENV));
    assert!(rendered.contains("Sigil HTTP/SSE adapter"));
    assert!(rendered.contains("bind: 127.0.0.1:0"));
    assert!(rendered.contains("auth: bearer token from SIGIL_HTTP_TOKEN"));
    assert!(rendered.contains("status: listening; press Ctrl-C for graceful shutdown"));
    assert!(!rendered.contains("secret-token"));
    Ok(())
}

#[test]
fn serve_startup_json_is_one_line_secret_free_server_metadata() -> Result<()> {
    let info = sigil_http::HttpServerInfo::new(
        "workspace-1",
        SocketAddr::from(([127, 0, 0, 1], 43123)),
        true,
    );

    let rendered = render_serve_startup_json(&info)?;
    let decoded: sigil_http::HttpServerInfo = serde_json::from_str(rendered.trim_end())?;

    assert_eq!(rendered.lines().count(), 1);
    assert_eq!(decoded, info);
    assert_eq!(
        decoded.schema_version,
        sigil_http::HTTP_SERVER_INFO_SCHEMA_VERSION
    );
    assert_eq!(decoded.bind_addr, "127.0.0.1:43123");
    assert!(decoded.capabilities.durable_session_reopen);
    assert!(decoded.capabilities.bounded_transcript_replay);
    assert!(decoded.capabilities.support_diagnostics);
    assert!(!rendered.contains(DEFAULT_HTTP_TOKEN_ENV));
    assert!(!rendered.contains("secret-token"));
    assert!(!rendered.contains("session_log_path"));
    Ok(())
}

#[tokio::test]
async fn serve_owner_channel_reports_eof_and_reaps_its_reader() -> Result<()> {
    let mut watcher = ServeOwnerChannelWatcher::spawn(std::io::Cursor::new(b"owner".to_vec()))?;

    tokio::time::timeout(std::time::Duration::from_secs(1), watcher.wait()).await?;
    watcher.reap_if_finished()?;
    Ok(())
}

#[test]
fn serve_startup_plan_rejects_disabled_auth_and_renders_token_env_fallback() -> Result<()> {
    let disabled = build_serve_startup_plan(
        ServeOptions {
            no_token: true,
            ..default_serve_options()
        },
        None,
    )
    .expect_err("V1 should reject disabled bearer authentication");
    let fallback = ServeStartupPlan {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        token_required: true,
        token_env: None,
    };

    assert!(disabled.to_string().contains("requires bearer token"));
    assert!(
        render_serve_startup_plan(&fallback).contains("auth: bearer token from SIGIL_HTTP_TOKEN")
    );
    Ok(())
}

#[test]
fn doctor_command_renders_report_for_missing_config() -> Result<()> {
    let workspace = create_test_workspace("doctor-command");

    super::doctor_command(
        &workspace.join("missing.toml"),
        &workspace,
        DoctorOutput::Text,
    )
}

#[test]
fn doctor_command_report_includes_appearance_warnings() -> Result<()> {
    let workspace = create_test_workspace("doctor-appearance");
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        format!(
            r##"config_version = 2

[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
connection = "local"
model = "local-model"

[connections.local]
label = "Local"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = {{ source = "none" }}

[appearance.colors]
surface_base = "#101010"
text_primary = "#101010"
"##,
            workspace.join("state").display(),
            workspace.join("cache").display()
        ),
    )?;

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
        network_effect: None,
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
        approval_identity: test_approval_identity(&call.id),
        effects: std::collections::BTreeSet::from([sigil_kernel::ToolPermissionEffect::FileRead]),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(
            sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
                code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
            },
        ),
        call: call.clone(),
        spec,
        subjects: vec![ToolSubject::path("README.md", "README.md")],
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::Read,
        risk: sigil_kernel::PermissionRisk::Low,
        subject_zones: vec![sigil_kernel::PathTrustZone::WorkspaceSource],
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
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
        approval_request_id: format!("approval-{}", call.id),
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
#[ignore = "requires an HTTPS provider fixture under the current connection schema"]
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
#[ignore = "requires an HTTPS provider fixture under the current connection schema"]
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
    write_application_run_test_config(&config_path, &server)?;

    super::run_command(
        &config_path,
        &workspace,
        "Say hi".to_owned(),
        None,
        None,
        None,
        None,
    )
    .await?;

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
    let session_dir = paths.state_root.join("managed/session-log");
    let entries = fs::read_dir(&session_dir)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        1,
        "run_command should create one session log"
    );
    let session_path = entries[0].path().join("records.jsonl");
    assert_eq!(
        session_path.extension().and_then(|ext| ext.to_str()),
        Some("jsonl")
    );
    let session_contents = fs::read_to_string(session_path)?;
    assert!(session_contents.contains("Say hi"));
    assert!(session_contents.contains("hello from agent"));
    assert!(session_contents.contains("\"event_type\":\"session_entry_recorded\""));
    let provider_attempt = session_contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["event_type"] == "provider_physical_attempt_started")
        .expect("run should persist a provider physical-attempt start");
    let logical_run_id = provider_attempt["payload"]["logical_run_id"]
        .as_str()
        .expect("provider attempt should carry the application run id");
    assert!(uuid::Uuid::parse_str(logical_run_id).is_ok());
    assert!(!logical_run_id.starts_with("agent-run-"));
    Ok(())
}

#[tokio::test]
async fn run_json_emits_exactly_one_terminal_result_record() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"json answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), responses).await?;
    let workspace = create_test_workspace("run-json");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, &server)?;
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_writer(
        &config_path,
        &workspace,
        "Say hi".to_owned(),
        RunOutput::Json,
        &mut stdout,
    )
    .await;

    assert_eq!(exit, MachineExitCode::Success);
    let lines = String::from_utf8(stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["record_type"], "result");
    assert_eq!(lines[0]["protocol_version"], 1);
    assert_eq!(lines[0]["result"]["status"], "succeeded");
    assert_eq!(lines[0]["result"]["final_text"], "json answer");
    assert!(
        std::path::Path::new(
            lines[0]["result"]["session_log_path"]
                .as_str()
                .expect("result must expose the durable session path")
        )
        .exists()
    );
    Ok(())
}

#[tokio::test]
async fn run_jsonl_emits_ordered_events_then_one_terminal_result() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"jsonl answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), responses).await?;
    let workspace = create_test_workspace("run-jsonl");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, &server)?;
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_writer(
        &config_path,
        &workspace,
        "Say hi".to_owned(),
        RunOutput::Jsonl,
        &mut stdout,
    )
    .await;

    assert_eq!(exit, MachineExitCode::Success);
    let lines = String::from_utf8(stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert!(lines.len() >= 3);
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .all(|record| record["record_type"] == "event")
    );
    assert_eq!(
        lines.last().expect("terminal record")["record_type"],
        "result"
    );
    let sequences = lines[..lines.len() - 1]
        .iter()
        .map(|record| {
            record["event"]["sequence"]
                .as_u64()
                .expect("event sequence")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sequences,
        (1..=u64::try_from(sequences.len())?).collect::<Vec<_>>()
    );
    assert_eq!(
        lines.last().expect("terminal record")["result"]["final_text"],
        "jsonl answer"
    );
    Ok(())
}

#[tokio::test]
async fn run_json_classifies_missing_config_without_leaking_raw_source() -> Result<()> {
    let workspace = create_test_workspace("run-json-invalid-config");
    let missing = workspace.join("missing.toml");
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_writer(
        &missing,
        &workspace,
        "Say hi".to_owned(),
        RunOutput::Json,
        &mut stdout,
    )
    .await;

    assert_eq!(exit, MachineExitCode::InvalidInput);
    let record: serde_json::Value = serde_json::from_slice(&stdout)?;
    assert_eq!(record["record_type"], "error");
    // Missing config on first-run/machine flows is classified by the request layer (the boot
    // attach degrades to epoch-only for absent configs); the message never leaks raw paths.
    assert_eq!(record["error"]["code"], "model_route_not_configured");
    assert_eq!(record["error"]["message"], "model route is not configured");
    assert!(!String::from_utf8(stdout)?.contains("missing.toml"));
    Ok(())
}

#[tokio::test]
async fn run_json_cancellation_during_preparation_emits_error_and_exit_130() -> Result<()> {
    let workspace = create_test_workspace("run-json-cancelled");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, "http://127.0.0.1:9")?;
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_cancellation(
        &config_path,
        &workspace,
        "Wait".to_owned(),
        RunOutput::Json,
        &mut stdout,
        std::future::ready(Ok(())),
    )
    .await;

    assert_eq!(exit, MachineExitCode::Cancelled);
    let record: serde_json::Value = serde_json::from_slice(&stdout)?;
    assert_eq!(record["record_type"], "error");
    assert_eq!(record["error"]["code"], "cancelled");
    assert_eq!(
        record["error"]["message"],
        "application run was cancelled before startup completed"
    );
    // RFC-0071 R71.6: the authority anchors may exist after boot attach, but no session data
    // may have been written before the cancellation.
    let state = workspace.join("state");
    if state.exists() {
        assert!(
            !state.join("sessions").exists(),
            "cancelled before any durable session data"
        );
    }
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
        startup_output: ServeStartupOutput::Text,
        shutdown_on_stdin_close: false,
    }
}

fn write_test_config(path: &std::path::Path, base_url: &str) -> Result<()> {
    let workspace = path
        .parent()
        .ok_or_else(|| anyhow!("test config path should have a parent"))?;
    let state_root = workspace.join("state");
    let cache_root = workspace.join("cache");
    let config = format!(
        r#"config_version = 2

[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
connection = "deepseek-test"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 5

[connections.deepseek-test]
label = "DeepSeek test"
provider = "deepseek"
protocol = "deepseek"
base_url = "{base_url}"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}

[connections.deepseek-test.options]
beta_base_url = "{base_url}"
anthropic_base_url = "{base_url}"
fim_model = "deepseek-v4-pro"
strict_tools_mode = "auto"
"#,
        state_root.display(),
        cache_root.display()
    );
    fs::write(path, config)?;
    Ok(())
}

#[tokio::test]
async fn plan_decision_command_applies_typed_hash_bound_action_and_rejects_replay() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, "http://127.0.0.1:1")?;
    let session_path = workspace.join("session.jsonl");
    let draft = session_with_pending_plan_draft(&session_path)?;

    // A committed draft without a decision is projected as the bounded awaiting artifact.
    let pending =
        super::pending_plan_review_artifact(session_path.to_str().expect("session path"))?
            .expect("pending draft must be projected");
    assert_eq!(pending.plan_id, draft.plan_id);
    assert_eq!(pending.plan_hash, draft.plan_hash);
    assert_eq!(pending.summary, "Migrate the coordinator");
    assert_eq!(pending.step_count, 1);

    // Reject is hash-bound and returns a receipt without executing anything.
    let output = run_plan_decision(&config_path, &workspace, &session_path, &draft, "reject")?;
    assert_eq!(output["command"], "plan_decision");
    assert_eq!(output["action"], "reject");
    assert_eq!(output["plan_id"], draft.plan_id);
    assert!(output["task_id"].is_null());

    // The same decision cannot be applied twice against the durable facts.
    let result = run_plan_decision(&config_path, &workspace, &session_path, &draft, "save");
    assert!(
        result.is_err(),
        "a second decision must be rejected as a durable conflict"
    );

    // A rejected plan is no longer pending.
    let pending =
        super::pending_plan_review_artifact(session_path.to_str().expect("session path"))?;
    assert!(pending.is_none(), "rejected plan must not remain pending");
    Ok(())
}

#[tokio::test]
async fn plan_decision_run_creates_the_durable_task_prefix() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, "http://127.0.0.1:1")?;
    let session_path = workspace.join("session.jsonl");
    let draft = session_with_pending_plan_draft(&session_path)?;

    let output = run_plan_decision(&config_path, &workspace, &session_path, &draft, "run")?;
    assert_eq!(output["action"], "run");
    let task_id = output["task_id"]
        .as_str()
        .expect("Run decision must return the created task id");
    assert!(!task_id.is_empty());
    Ok(())
}

struct PendingPlanDraft {
    plan_id: String,
    plan_hash: String,
}

fn session_with_pending_plan_draft(session_path: &std::path::Path) -> Result<PendingPlanDraft> {
    use sigil_kernel::{
        ControlEntry, ConversationRoute, ConversationRouteDecisionId,
        ConversationRouteDecisionRecordedEntry, ConversationRouteReason, ConversationTurnRef,
        ModelMessage, PlanReviewAttemptEntry, PlanReviewAttemptId, PlanReviewAttemptStatus,
        SessionRef, submit_plan_draft_entry,
    };
    // Initialize the session through the production route-resume path so the durable stream
    // carries the required identity and route trust records.
    let config_path = session_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("sigil.toml");
    let root_config = sigil_kernel::RootConfig::load(&config_path)?;
    let (_, fallback_route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)?;
    let store = sigil_kernel::JsonlSessionStore::new(session_path)?;
    let mut session = sigil_runtime::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
        &root_config,
        &fallback_route,
        store,
        None,
        None,
        None,
    )?;
    let source = ConversationTurnRef::new(session.session_scope_id(), "message-1", "run-1")?;
    session.append_user_message(ModelMessage::user(
        "design the migration before touching anything",
    ))?;
    let review_id = sigil_kernel::plan_review_id_for_source(&source);
    let decision = ConversationRouteDecisionRecordedEntry {
        decision_id: ConversationRouteDecisionId::new(format!("decision-{}", review_id.as_str()))?,
        source_turn: source.clone(),
        route: ConversationRoute::PlanReview,
        reason_codes: vec![ConversationRouteReason::RouteReviewRequired],
        configured_policy: sigil_kernel::TaskRoutingPolicy::Auto,
        effective_capability: sigil_kernel::AutomaticRouteCapability::ReviewFirst,
        policy_snapshot_hash: format!("sha256:{}", "a".repeat(64)),
        route_contract_fingerprint: format!("sha256:{}", "b".repeat(64)),
        decided_at_ms: 1,
    };
    let decision_id = decision.decision_id.clone();
    session.append_control(ControlEntry::ConversationRouteDecisionRecorded(decision))?;
    let attempt_id = PlanReviewAttemptId::new(format!("attempt-{}", review_id.as_str()))?;
    let attempt = PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: sigil_kernel::PlanId::new("plan_draft_pending")?,
        source: sigil_kernel::PlanReviewSource::AutomaticConversationRoute,
        source_turn: source.clone(),
        route_decision_id: Some(decision_id.clone()),
        child_session_ref: SessionRef::new_relative("child.jsonl")?,
        finalizer_session_ref: None,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 1,
    };
    session.append_control(ControlEntry::PlanReviewAttempt(attempt.clone()))?;
    let draft_args = r#"{
        "schema_version": 2,
        "summary": "Migrate the coordinator",
        "steps": [{
            "step_id": "migrate_1",
            "title": "Migrate coordinator",
            "role": "executor",
            "mode": "write",
            "isolation": "sequential_workspace_write",
            "target_paths": ["src/coordinator.rs"]
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    let draft = submit_plan_draft_entry(
        draft_args,
        sigil_kernel::PlanId::new("plan_draft_pending")?,
        sigil_kernel::PlanSourceRef {
            source_turn: Some(source.clone()),
            route_decision_id: Some(decision_id.clone()),
            plan_review_id: Some(review_id.clone()),
            ..sigil_kernel::PlanSourceRef::default()
        },
        1,
        None,
    )?
    .expect("draft must be valid");
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    // RFC-0067: a DraftReady plan must carry its executable candidate and ready marker.
    let candidate = sigil_kernel::compile_executable_plan_candidate(
        &draft,
        &sigil_kernel::PlanCompileInputV1 {
            source_attempt_id: attempt.attempt_id.as_str().to_owned(),
            source_turn_id: source.message_id.clone(),
            task_config_contract_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-task-config-v1",
                "test",
            ),
            planner_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-planner-schema-v1",
                "v2",
            ),
            task_contract_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-task-contract-schema-v1",
                "v2",
            ),
            intent_schema_hash: None,
            max_plan_steps: 64,
            workspace_id: None,
            session_scope_id: Some(session.session_scope_id().to_owned()),
        },
    )
    .expect("fixture draft must compile");
    session.append_control(ControlEntry::ExecutablePlanCandidatePreparedV1(Box::new(
        candidate.clone(),
    )))?;
    session.append_control(ControlEntry::PlanReadyCommittedV1(
        sigil_kernel::PlanReadyCommittedV1Entry {
            plan_id: draft.plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            candidate_hash: candidate.candidate_hash.clone(),
            attempt_id: attempt.attempt_id.as_str().to_owned(),
            committed_at_ms: 2,
        },
    ))?;
    let mut ready = attempt;
    ready.status = PlanReviewAttemptStatus::DraftReady;
    ready.recorded_at_ms = 2;
    session.append_control(ControlEntry::PlanReviewAttempt(ready))?;
    Ok(PendingPlanDraft {
        plan_id: draft.plan_id.as_str().to_owned(),
        plan_hash: draft.plan_hash,
    })
}

fn run_plan_decision(
    config_path: &std::path::Path,
    workspace: &std::path::Path,
    session_path: &std::path::Path,
    draft: &PendingPlanDraft,
    action: &str,
) -> Result<serde_json::Value> {
    let action = match action {
        "run" => super::PlanDecisionAction::Run,
        "save" => super::PlanDecisionAction::Save,
        "revise" => super::PlanDecisionAction::Revise,
        "reject" => super::PlanDecisionAction::Reject,
        _ => bail!("unknown test action"),
    };
    let rendered = futures::executor::block_on(super::plan_decision_command(
        config_path,
        workspace,
        session_path,
        &draft.plan_id,
        &draft.plan_hash,
        action,
    ))?;
    Ok(serde_json::from_str(&rendered).expect("plan decision receipt should be JSON"))
}

fn write_application_run_test_config(path: &std::path::Path, base_url: &str) -> Result<()> {
    let workspace = path
        .parent()
        .ok_or_else(|| anyhow!("test config path should have a parent"))?;
    let state_root = workspace.join("state");
    let cache_root = workspace.join("cache");
    let config = format!(
        r#"config_version = 2

[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 5

[task]
routing_policy = "manual"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
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
